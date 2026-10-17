//! Explicit Windows-host lab: production bundle validation, TPM lifecycle and
//! QemuRun, disposable Windows PE. No Windows installation or setup bypass.
#![cfg(windows)]
use guest_net::{NetConfig, NetStack};
use lightvm_proto::{Firmware, GuestOs, MediaId, RunId, ToolsConfig, VmId};
use lightvm_service::{
    adapters::backend::{QemuSetup, probe},
    tools::{Policy, ToolsRun},
};
use qemu_backend::cmdline::{Accelerator, CpuModel, DiskFormat, DiskSpec, GuestNetwork};
use qemu_backend::{DeviceProfile, ManagedPath, ManagedPaths, VmLaunchSpec};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

struct Paths {
    base: PathBuf,
    bundle: PathBuf,
    iso: PathBuf,
    id: MediaId,
    restored: Option<(PathBuf, Option<PathBuf>, PathBuf)>,
}
impl Paths {
    fn path(&self, p: &str) -> qemu_backend::error::Result<ManagedPath> {
        ManagedPath::verify(self.base.join(p))
    }
}
impl ManagedPaths for Paths {
    fn qemu_system(&self) -> qemu_backend::error::Result<ManagedPath> {
        ManagedPath::verify(self.bundle.join("qemu/qemu-system-x86_64.exe"))
    }
    fn firmware_code(&self) -> qemu_backend::error::Result<ManagedPath> {
        ManagedPath::verify(self.bundle.join("firmware/lightvm-x64-code.fd"))
    }
    fn firmware_vars(&self, _: &RunId) -> qemu_backend::error::Result<Option<ManagedPath>> {
        if let Some((_, vars, _)) = &self.restored {
            vars.as_ref().map(ManagedPath::verify).transpose()
        } else {
            self.path("vars.fd").map(Some)
        }
    }
    fn disk(&self, _: &RunId) -> qemu_backend::error::Result<Option<ManagedPath>> {
        if let Some((disk, _, _)) = &self.restored {
            ManagedPath::verify(disk).map(Some)
        } else {
            self.path("disk.qcow2").map(Some)
        }
    }
    fn protected_readonly(&self, _: &RunId) -> qemu_backend::error::Result<Vec<ManagedPath>> {
        Ok(vec![])
    }
    fn media(&self, id: &MediaId) -> qemu_backend::error::Result<ManagedPath> {
        assert_eq!(*id, self.id);
        ManagedPath::verify(&self.iso)
    }
    fn run_dir(&self, _: &RunId) -> qemu_backend::error::Result<ManagedPath> {
        self.path("run")
    }
    fn private_dir(&self, _: &RunId) -> qemu_backend::error::Result<ManagedPath> {
        self.path("p")
    }
    fn tools_iso(&self, _: &RunId) -> qemu_backend::error::Result<Option<ManagedPath>> {
        self.path("p/tools.iso").map(Some)
    }
    fn swtpm(&self) -> qemu_backend::error::Result<ManagedPath> {
        ManagedPath::verify(self.bundle.join("swtpm/swtpm.exe"))
    }
    fn tpm_state_dir(&self, _: &RunId) -> qemu_backend::error::Result<Option<ManagedPath>> {
        if let Some((_, _, tpm)) = &self.restored {
            ManagedPath::verify(tpm).map(Some)
        } else {
            self.path("tpm").map(Some)
        }
    }
}

#[test]
#[ignore = "requires an explicitly approved development Windows 11 bundle and test ISO on E:"]
fn windows_pe_reports_tpm2_and_secure_boot_on_production_run() {
    qualify_pe(false, false);
}

#[test]
#[ignore = "real TPM checkpoint qualification requires the explicit lab bundle and ISO on E:"]
fn windows_pe_tpm_state_follows_snapshot_and_baseline_restore() {
    qualify_pe(true, false);
}

#[test]
#[ignore = "requires an explicitly approved development Windows 11 bundle and test ISO on E:"]
fn windows_pe_boots_again_after_a_warm_reset_with_the_tpm() {
    qualify_pe(false, true);
}

fn qualify_pe(checkpoints: bool, warm_reset: bool) {
    let env = |name: &str| PathBuf::from(std::env::var_os(name).expect(name));
    let lab = env("LIGHTVM_LAB");
    let bundle = env("LIGHTVM_WIN11_BUNDLE");
    let detected = guest_detect::detect_path(&env("LIGHTVM_WINPE_ISO"))
        .unwrap()
        .unwrap();
    assert_eq!(detected.os, GuestOs::Windows11X64);
    assert_eq!(detected.arch, Some(lightvm_proto::GuestArch::X86_64));
    let root = lab.join("w11").join(&RunId::new_v7().to_string()[24..]);
    let mut cat = vm_storage::Catalog::open(&root).unwrap();
    let vm = cat
        .with_tx(|tx| {
            tx.insert_vm(&vm_storage::NewVm {
                name: "native TPM checkpoint".into(),
                guest_os: "windows11_x64".into(),
                config_version: 1,
                config: serde_json::json!({"guest_os":"windows11_x64"}),
            })
        })
        .unwrap();
    let base = root.join("vms").join(vm.to_string());
    for p in ["run", "p", "tpm", "journal", "probe"] {
        std::fs::create_dir_all(base.join(p)).unwrap();
    }
    let mut setup = QemuSetup::new(
        lab.join("qemu/qemu-11.1.0"),
        base.clone(),
        base.join("probe"),
    );
    setup.win11_dir = Some(bundle.clone());
    // Lab-only approval of a manifest built from pinned, verified sources.
    setup
        .probe
        .approved_win11_bundle_sha256
        .push(qemu_backend::probe::sha256_file(&bundle.join("SHA256SUMS")).unwrap());
    let (_host, backend, _) = probe(setup).unwrap();
    let (mut caps, _bundle_guard) = backend.windows11_capabilities().unwrap();
    let profile = DeviceProfile::for_guest(GuestOs::Windows11X64, Firmware::Uefi).unwrap();
    let machine = caps
        .recommended_machine(profile.machine_family())
        .unwrap()
        .clone();
    let cpu = CpuModel::nominal("Skylake-Client-v4").unwrap();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let private = ManagedPath::verify(base.join("probe")).unwrap();
    let status = rt.block_on(qemu_backend::probe::probe_accelerator_for(
        &mut caps,
        Accelerator::Whpx,
        &profile,
        &machine,
        &cpu,
        &private,
        &Default::default(),
    ));
    assert!(status.is_available(), "{status:?}");
    assert!(
        std::process::Command::new(caps.qemu_img.as_path())
            .args(["create", "-f", "qcow2"])
            .arg(base.join("disk.qcow2"))
            .arg("64G")
            .status()
            .unwrap()
            .success()
    );
    std::fs::copy(
        bundle.join("firmware/lightvm-x64-vars-win.fd"),
        base.join("vars.fd"),
    )
    .unwrap();
    let mut paths = Paths {
        base: base.clone(),
        bundle,
        iso: env("LIGHTVM_WINPE_ISO"),
        id: MediaId::new_v7(),
        restored: None,
    };
    for dir in ["run", "p", "tpm", "journal"] {
        qemu_backend::prepare::restrict_to_current_user(&paths.path(dir).unwrap()).unwrap();
    }
    let standalone = vm_storage::CurrentStateFiles {
        disk: vm_storage::ManagedPath::parse(&format!("vms/{vm}/disk.qcow2")).unwrap(),
        vars: Some(vm_storage::ManagedPath::parse(&format!("vms/{vm}/vars.fd")).unwrap()),
    };
    let mut baseline = None;
    let mut snapshot = None;
    for cycle in 0..if checkpoints { 5 } else { 2 } {
        let run = RunId::new_v7();
        let config = ToolsConfig {
            enabled: true,
            files_transfer: true,
            ..Default::default()
        };
        let fixture = env("LIGHTVM_WINPE_FIXTURE");
        let probe_cmd = String::from_utf8(b"@echo off\r\nwpeutil InitializeNetwork\r\nreg add \"HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Shell Folders\" /v Personal /t REG_SZ /d X:\\LightVMGuestDocs /f\r\nreg add \"HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\User Shell Folders\" /v Personal /t REG_EXPAND_SZ /d X:\\LightVMGuestDocs /f\r\nmkdir \"X:\\LightVMGuestDocs\\LightVM Tools\\outbox\"\r\nsc start tbs\r\n\"%~dp0TPMTEST.EXE\" >\"X:\\LightVMGuestDocs\\LightVM Tools\\outbox\\tpm.log\" 2>&1\r\nstart \"\" /b \"%~dp0TOOLS.EXE\"\r\nexit /b 0\r\n".to_vec()).unwrap();
        let probe_cmd = if checkpoints && (cycle == 1 || cycle == 3) {
            probe_cmd.replace("TPMTEST.EXE\" >", "TPMTEST.EXE\" --mutate >")
        } else {
            probe_cmd
        };
        let mut tools = ToolsRun::start(
            vm,
            run,
            &config,
            &base.join("p"),
            &base.join("journal"),
            &[
                (
                    "TOOLS.EXE",
                    std::fs::read(fixture.join("TOOLS64.EXE")).unwrap(),
                ),
                (
                    "TPMTEST.EXE",
                    std::fs::read(fixture.join("TPMTEST.EXE")).unwrap(),
                ),
                (
                    "AUTOUNATTEND.XML",
                    include_bytes!("common/winpe-unattend.xml").to_vec(),
                ),
                ("PROBE.CMD", probe_cmd.into_bytes()),
            ],
        )
        .unwrap();
        tools.apply(Policy::manual(&config));
        let spec = VmLaunchSpec {
            run_id: run,
            machine: machine.clone(),
            profile,
            accelerator: Accelerator::Whpx,
            cpu: cpu.clone(),
            vcpus: 2,
            cores_per_socket: None,
            memory_mib: 4096,
            disk: Some(DiskSpec {
                format: DiskFormat::Qcow2,
            }),
            install_media: Some(paths.id),
            data_media: None,
            limits: Default::default(),
            network: GuestNetwork::Supervisor,
        };
        rt.block_on(async {
            let launch = || async {
                let guest = qemu_backend::QemuRun::start(&caps, &spec, &paths, Default::default())
                    .await
                    .unwrap();
                let socket =
                    socket2::Socket::new(socket2::Domain::UNIX, socket2::Type::STREAM, None)
                        .unwrap();
                socket
                    .connect(&socket2::SockAddr::unix(guest.net_socket().unwrap()).unwrap())
                    .unwrap();
                let mut net = NetConfig::new(tools.endpoints().into());
                net.advertise_dns = false;
                let stack = NetStack::start(
                    net,
                    guest_net::transport::attach(socket).unwrap(),
                    ToolsRun::allowed_endpoints(),
                )
                .unwrap();
                (guest, stack)
            };
            let (mut guest, mut stack) = launch().await;
            let mut boot = tokio::spawn(guest.boot_prompt_keys());
            let start = Instant::now();
            while !tools.control.connected() {
                assert!(
                    start.elapsed() < Duration::from_secs(180),
                    "guest Tools timeout: {}",
                    base.display()
                );
                if let Ok(shot) = guest.screenshot().await {
                    std::fs::write(base.join("setup.png"), shot.png).unwrap();
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            let read_tpm_log = || {
                let reply = tools
                    .control
                    .call(lightvm_tools::control::Command::File {
                        request: lightvm_tools::protocol::FileRequest::Read {
                            share: "outbox".into(),
                            path: lightvm_tools::protocol::RelativePath::new(vec![
                                "tpm.log".into(),
                            ])
                            .unwrap(),
                            offset: 0,
                            length: 16384,
                        },
                    })
                    .unwrap();
                match reply {
                    lightvm_proto::ToolReply::File {
                        reply: lightvm_tools::protocol::FileReply::Data { data, .. },
                    } => String::from_utf8(lightvm_tools::protocol::decode(&data, 16384).unwrap())
                        .unwrap(),
                    other => panic!("{other:?}"),
                }
            };
            let text = read_tpm_log();
            std::fs::write(base.join(format!("tpm-{cycle}.log")), &text).unwrap();
            println!("{}\n{text}", base.display());
            if warm_reset && cycle == 0 {
                // A reset of this profile ends QEMU (`-action
                // reboot=shutdown`); like the service, the test starts the
                // same run again from cold, and the same probe from the same
                // medium must see the TPM again.
                guest.reset().await.unwrap();
                let exit = guest.wait_exit(Duration::from_secs(20)).await;
                assert!(
                    matches!(exit, Some((_, qemu_backend::ExitCause::Reboot))),
                    "{exit:?}"
                );
                stack.stop();
                boot.abort();
                let closed = guest.close().await.unwrap();
                assert_eq!(closed.active_at_close, 0);
                tools.control.close_handle().disconnect();
                (guest, stack) = launch().await;
                boot = tokio::spawn(guest.boot_prompt_keys());
                let start = Instant::now();
                let mut disconnected = false;
                loop {
                    assert!(
                        start.elapsed() < Duration::from_secs(240),
                        "no reconnect after warm reset: {}",
                        base.display()
                    );
                    for event in guest.take_vcpu_fault_events() {
                        println!("vCPU event: {event:?}");
                    }
                    if let Ok(shot) = guest.screenshot().await {
                        std::fs::write(base.join("reset.png"), shot.png).unwrap();
                    }
                    disconnected |= !tools.control.connected();
                    if disconnected && tools.control.connected() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
                let after = read_tpm_log();
                std::fs::write(base.join("tpm-after-reset.log"), &after).unwrap();
                println!("after warm reset:\n{after}");
                assert!(after.contains("tpm_version=2"), "{after}");
                assert!(after.contains("tbs_status=0"), "{after}");
                assert!(after.contains("secure_boot=1"), "{after}");
            }
            let helper = guest.tpm_identity().unwrap();
            if cycle == 0 || checkpoints {
                let stopped = guest
                    .shutdown(qemu_backend::run::ShutdownPolicy {
                        powerdown_timeout: Duration::ZERO,
                        quit_timeout: Duration::from_secs(5),
                        kill_timeout: Duration::from_secs(10),
                    })
                    .await
                    .unwrap();
                assert_eq!(stopped.active_after, 0);
            } else {
                assert!(helper.status().unwrap().is_alive());
                // Only the helper identity owned by this disposable run is killed.
                assert!(
                    std::process::Command::new("taskkill.exe")
                        .args(["/F", "/PID", &helper.pid.to_string()])
                        .status()
                        .unwrap()
                        .success()
                );
                assert!(
                    guest.wait_exit(Duration::from_secs(5)).await.is_some(),
                    "TPM failure must stop QEMU"
                );
            }
            stack.stop();
            boot.abort();
            assert!(text.contains("secure_boot=1"), "{text}");
            assert!(text.contains("setup_mode=0"), "{text}");
            assert!(text.contains("tpm_version=2"), "{text}");
            assert!(text.contains("tbs_status=0"), "{text}");
            assert!(
                text.contains(if cycle == 0 {
                    "persistent_nv=created"
                } else if checkpoints && (cycle == 1 || cycle == 3) {
                    "persistent_nv=mutated"
                } else {
                    "persistent_nv=matched"
                }),
                "{text}"
            );
            let closed = guest.close().await.unwrap();
            assert_eq!(closed.active_at_close, 0);
            assert!(
                closed.job_closed && closed.process_handle_released,
                "{closed:?}"
            );
            assert!(!helper.status().unwrap().is_alive());
        });
        if checkpoints {
            let image = |args: &[&str], path: &std::path::Path| {
                assert!(
                    std::process::Command::new(caps.qemu_img.as_path())
                        .args(args)
                        .arg(path)
                        .status()
                        .unwrap()
                        .success()
                );
            };
            if cycle == 0 {
                let req = vm_storage::TakeSnapshot {
                    vm_id: vm,
                    name: "TPM checkpoint".into(),
                    description: String::new(),
                    running: false,
                    clean_shutdown: false,
                    config: serde_json::json!({"guest_os":"windows11_x64"}),
                    config_revision: 1,
                    disk: standalone.disk.clone(),
                    vars: standalone.vars.clone(),
                    thumbnail: None,
                };
                let op = cat.begin_take_snapshot(req).unwrap();
                image(&["snapshot", "-c", op.tag()], op.disk());
                assert_eq!(cat.settle_snapshot(&op).unwrap(), vm_storage::Settled::Done);
                snapshot = Some(op.snapshot_id());
                let job = cat
                    .begin_create_baseline(vm_storage::CreateBaseline {
                        vm_id: vm,
                        label: "TPM baseline".into(),
                        clean_shutdown: false,
                        protected: false,
                        standalone: standalone.clone(),
                    })
                    .unwrap();
                let done = job.run(|src, dst| {
                    assert!(
                        std::process::Command::new(caps.qemu_img.as_path())
                            .args(["convert", "-f", "qcow2", "-O", "qcow2"])
                            .arg(src)
                            .arg(dst)
                            .status()
                            .unwrap()
                            .success()
                    );
                    Ok(64 << 30)
                });
                baseline = Some(cat.finish_create_baseline(done).unwrap().baseline_id);
            }
            if cycle == 1 {
                let record = cat.read(|tx| tx.vm(&vm)).unwrap();
                let op = cat
                    .begin_revert_snapshot(vm_storage::RevertSnapshot {
                        vm_id: vm,
                        snapshot_id: snapshot.unwrap(),
                        disk: standalone.disk.clone(),
                        vars: standalone.vars.clone(),
                        config: record.config,
                        config_version: 1,
                        expected_revision: record.revision,
                    })
                    .unwrap();
                image(&["snapshot", "-a", op.tag()], op.disk());
                assert_eq!(cat.settle_snapshot(&op).unwrap(), vm_storage::Settled::Done);
            }
            if cycle == 3 {
                let op = cat
                    .begin_delete_snapshot(&vm, &snapshot.unwrap(), &standalone.disk)
                    .unwrap();
                image(&["snapshot", "-d", op.tag()], op.disk());
                assert_eq!(cat.settle_snapshot(&op).unwrap(), vm_storage::Settled::Done);
                let job = cat
                    .begin_restore(vm_storage::RestoreBaseline {
                        vm_id: vm,
                        baseline_id: baseline.unwrap(),
                        standalone: standalone.clone(),
                    })
                    .unwrap();
                let done = job.run(|backing, size, dst| {
                    assert!(
                        std::process::Command::new(caps.qemu_img.as_path())
                            .args(["create", "-u", "-f", "qcow2", "-F", "qcow2", "-b", backing])
                            .arg(dst)
                            .arg(size.to_string())
                            .status()
                            .unwrap()
                            .success()
                    );
                    Ok(())
                });
                let current = cat.finish_restore(done).unwrap();
                let (disk, vars) = cat
                    .read(|tx| {
                        Ok((
                            tx.disk(&current.overlay_disk_id)?.managed_path,
                            current
                                .vars_fw_id
                                .map(|id| tx.firmware(&id).map(|f| f.managed_path))
                                .transpose()?,
                        ))
                    })
                    .unwrap();
                let tpm = cat.current_tpm_dir(&vm).unwrap();
                paths.restored = Some((
                    cat.layout().resolve(&disk).unwrap(),
                    vars.map(|v| cat.layout().resolve(&v).unwrap()),
                    cat.layout().resolve(&tpm).unwrap(),
                ));
            }
        }
    }
    if checkpoints {
        std::fs::write(base.join("checkpoint-results.txt"),"real_swtpm_nv_mutated=passed\nsnapshot_restores_original_nv=passed\nbaseline_restores_original_nv=passed\n").unwrap();
    }
    std::fs::write(base.join("results.txt"),"guest=Windows PE x64/WHPX\ninstalled_windows=false\nsecure_boot=passed\ntpm2=passed\nnv_persistence_cold_restart=passed\ntpm_failure_stops_vm=passed\njob_and_handles_closed=passed\n").unwrap();
}

#[test]
#[ignore = "requires explicitly approved Windows 11 development bundle and WHPX"]
fn service_backend_starts_windows11_twice_and_keeps_other_profiles_working() {
    use vm_core::ports::VmBackend;
    let env = |key: &str| PathBuf::from(std::env::var_os(key).expect(key));
    let lab = env("LIGHTVM_LAB");
    let bundle = env("LIGHTVM_WIN11_BUNDLE");
    let base = lab
        .join("win11-real")
        .join(format!("backend-{}", RunId::new_v7()));
    std::fs::create_dir_all(&base).unwrap();
    let mut setup = QemuSetup::new(lab.join("qemu/qemu-11.1.0"), base.clone(), base.join("p"));
    setup.win11_dir = Some(bundle.clone());
    setup
        .probe
        .approved_win11_bundle_sha256
        .push(qemu_backend::probe::sha256_file(&bundle.join("SHA256SUMS")).unwrap());
    let (_host, mut backend, _) = probe(setup).unwrap();
    let mut config = lightvm_service::fakes::sample_guest_config(
        "tpm-backend",
        GuestOs::Windows11X64,
        Firmware::Uefi,
    );
    config.install_media = None;
    config.data_media = None;
    config.vcpus = 2;
    config.memory_mib = 4096;
    config.disk_gib = 64;
    let vm = lightvm_proto::VmRecord {
        id: VmId::new_v7(),
        config,
        config_revision: 1,
        persisted_state: lightvm_proto::VmState::Stopped,
        created_at: lightvm_proto::UtcTimestamp::now(),
    };
    for cycle in 0..2 {
        let run = RunId::new_v7();
        backend.start(&vm, run, &[]).unwrap();
        std::thread::sleep(Duration::from_millis(500));
        if cycle == 0 {
            // R6-05 §15: a reset ends QEMU and the same run starts again from
            // cold; the core never sees an exit and the run keeps working.
            std::thread::sleep(Duration::from_secs(3));
            backend.reset(vm.id, run).unwrap();
            let t0 = Instant::now();
            while t0.elapsed() < Duration::from_secs(15) {
                let obs = lightvm_service::engine::BackendPort::drain_observations(&mut backend);
                assert!(obs.is_empty(), "the reset was reported as an exit: {obs:?}");
                std::thread::sleep(Duration::from_millis(250));
            }
            backend
                .screenshot(vm.id, run, 16 << 20)
                .expect("the same run answers after the cold reboot");
        }
        backend.force_stop(vm.id, run).unwrap();
    }
    assert!(
        base.join("vms")
            .join(vm.id.to_string())
            .join("tpm/tpm2-00.permall")
            .is_file()
    );
    // A new validation rejects any accidental cache writes in the bundle.
    drop(backend.windows11_capabilities().unwrap());
    let mut linux = vm.clone();
    linux.id = VmId::new_v7();
    linux.config.guest_os = GuestOs::UbuntuLtsX86_64;
    let run = RunId::new_v7();
    backend.start(&linux, run, &[]).unwrap();
    backend.force_stop(linux.id, run).unwrap();
    std::fs::write(base.join("results.txt"),"windows11_backend_start_restart=passed\nreset_is_a_cold_reboot_in_the_same_run=passed\nper_vm_tpm_state=passed\nother_profiles=passed\nbundle_still_immutable=passed\n").unwrap();
    println!("backend evidence: {}", base.display());
}

#[test]
#[ignore = "requires an explicitly approved development Windows 11 bundle and test ISO on E:"]
fn windows11_installs_and_reboots_with_tpm_and_secure_boot() {
    qualify_install(false);
}

/// R6-05 §16: after the qualification above, BitLocker with a TPM
/// protector on C:; the guest reboots (a cold reboot of this profile) once
/// encryption started and Windows must start again from the volume, i.e.
/// the emulated TPM released the key with no recovery prompt.
#[test]
#[ignore = "requires an explicitly approved development Windows 11 bundle and test ISO on E:"]
fn windows11_bitlocker_unlocks_from_the_tpm_after_a_cold_reboot() {
    qualify_install(true);
}

fn qualify_install(bitlocker: bool) {
    let env = |name: &str| PathBuf::from(std::env::var_os(name).expect(name));
    let lab = env("LIGHTVM_LAB");
    let bundle = env("LIGHTVM_WIN11_BUNDLE");
    let detected = guest_detect::detect_path(&env("LIGHTVM_WINPE_ISO"))
        .unwrap()
        .unwrap();
    assert_eq!(detected.os, GuestOs::Windows11X64);
    assert_eq!(detected.arch, Some(lightvm_proto::GuestArch::X86_64));
    let resume = std::env::var_os("LIGHTVM_WIN11_RESUME").map(PathBuf::from);
    let base = resume.clone().unwrap_or_else(|| {
        lab.join("win11-real")
            .join(format!("installed-{}", RunId::new_v7()))
    });
    if resume.is_some() {
        assert!(
            base.canonicalize()
                .unwrap()
                .starts_with(lab.join("win11-real").canonicalize().unwrap())
        );
        assert!(
            base.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("installed-")
        );
        assert!(base.join("disk.qcow2").is_file());
    }
    for p in ["run", "p", "tpm", "journal", "probe"] {
        std::fs::create_dir_all(base.join(p)).unwrap();
    }
    let mut setup = QemuSetup::new(
        lab.join("qemu/qemu-11.1.0"),
        base.clone(),
        base.join("probe"),
    );
    setup.win11_dir = Some(bundle.clone());
    // Lab-only approval of a manifest built from pinned, verified sources.
    setup
        .probe
        .approved_win11_bundle_sha256
        .push(qemu_backend::probe::sha256_file(&bundle.join("SHA256SUMS")).unwrap());
    let (_host, backend, _) = probe(setup).unwrap();
    let (mut caps, _bundle_guard) = backend.windows11_capabilities().unwrap();
    let profile = DeviceProfile::for_guest(GuestOs::Windows11X64, Firmware::Uefi).unwrap();
    let machine = caps
        .recommended_machine(profile.machine_family())
        .unwrap()
        .clone();
    let cpu = CpuModel::nominal("Skylake-Client-v4").unwrap();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let private = ManagedPath::verify(base.join("probe")).unwrap();
    let status = rt.block_on(qemu_backend::probe::probe_accelerator_for(
        &mut caps,
        Accelerator::Whpx,
        &profile,
        &machine,
        &cpu,
        &private,
        &Default::default(),
    ));
    assert!(status.is_available(), "{status:?}");
    if resume.is_none() {
        assert!(
            std::process::Command::new(caps.qemu_img.as_path())
                .args(["create", "-f", "qcow2"])
                .arg(base.join("disk.qcow2"))
                .arg("64G")
                .status()
                .unwrap()
                .success()
        );
        std::fs::copy(
            bundle.join("firmware/lightvm-x64-vars-win.fd"),
            base.join("vars.fd"),
        )
        .unwrap();
    }
    let paths = Paths {
        base: base.clone(),
        bundle,
        iso: env("LIGHTVM_WINPE_ISO"),
        id: MediaId::new_v7(),
        restored: None,
    };
    for dir in ["run", "p", "tpm", "journal"] {
        qemu_backend::prepare::restrict_to_current_user(&paths.path(dir).unwrap()).unwrap();
    }
    let vm = VmId::new_v7();
    for cycle in 0..1 {
        let run = RunId::new_v7();
        let config = ToolsConfig {
            enabled: true,
            files_transfer: true,
            ..Default::default()
        };
        let fixture = env("LIGHTVM_WINPE_FIXTURE");
        let mut files = vec![
            (
                "TOOLS.EXE",
                std::fs::read(fixture.join("TOOLS64.EXE")).unwrap(),
            ),
            (
                "TPMTEST.EXE",
                std::fs::read(fixture.join("TPMTEST.EXE")).unwrap(),
            ),
            (
                "AUTOUNATTEND.XML",
                include_bytes!("common/win11-install.xml").to_vec(),
            ),
            ("LAB.CMD", include_bytes!("common/win11-lab.cmd").to_vec()),
        ];
        if bitlocker {
            // Their presence on the CD turns on the BitLocker step of LAB.CMD.
            files.push((
                "BLENABLE.PS1",
                include_bytes!("common/win11-blenable.ps1").to_vec(),
            ));
            files.push((
                "BLSTATE.PS1",
                include_bytes!("common/win11-blstate.ps1").to_vec(),
            ));
        }
        let mut tools = ToolsRun::start(
            vm,
            run,
            &config,
            &base.join("p"),
            &base.join("journal"),
            &files,
        )
        .unwrap();
        tools.apply(Policy::manual(&config));
        let spec = VmLaunchSpec {
            run_id: run,
            machine: machine.clone(),
            profile,
            accelerator: Accelerator::Whpx,
            cpu: cpu.clone(),
            vcpus: 4,
            cores_per_socket: None,
            memory_mib: 6144,
            disk: Some(DiskSpec {
                format: DiskFormat::Qcow2,
            }),
            install_media: Some(paths.id),
            data_media: None,
            limits: Default::default(),
            network: GuestNetwork::Supervisor,
        };
        rt.block_on(async {
            // What the service does (`cold_reboot`): QEMU ends on every
            // reset of this profile and the same run starts again from cold.
            // `detached`: without the installation ISO (BitLocker refuses
            // while bootable media is attached).
            let detached_spec = VmLaunchSpec {
                install_media: None,
                ..spec.clone()
            };
            let (caps, spec, detached_spec, paths, tools) =
                (&caps, &spec, &detached_spec, &paths, &tools);
            let launch = move |detached: bool| async move {
                let s = if detached { detached_spec } else { spec };
                let guest = qemu_backend::QemuRun::start(caps, s, paths, Default::default())
                    .await
                    .unwrap();
                let socket =
                    socket2::Socket::new(socket2::Domain::UNIX, socket2::Type::STREAM, None)
                        .unwrap();
                socket
                    .connect(&socket2::SockAddr::unix(guest.net_socket().unwrap()).unwrap())
                    .unwrap();
                let mut net = NetConfig::new(tools.endpoints().into());
                net.advertise_dns = false;
                let stack = NetStack::start(
                    net,
                    guest_net::transport::attach(socket).unwrap(),
                    ToolsRun::allowed_endpoints(),
                )
                .unwrap();
                (guest, stack)
            };
            let rebooted = |g: &qemu_backend::QemuRun| {
                matches!(
                    g.state(),
                    qemu_backend::ProcessState::Exited {
                        cause: qemu_backend::ExitCause::Reboot,
                        ..
                    }
                )
            };
            let mut cold_reboots = 0u32;
            let (mut guest, mut stack) = launch(false).await;
            let boot = if resume.is_none() {
                Some(tokio::spawn(guest.boot_prompt_keys()))
            } else {
                None
            };
            let start = Instant::now();
            while !tools.control.connected() {
                assert!(
                    start.elapsed() < Duration::from_secs(2400),
                    "guest Tools timeout: {}",
                    base.display()
                );
                if rebooted(&guest) {
                    stack.stop();
                    let closed = guest.close().await.unwrap();
                    assert_eq!(closed.active_at_close, 0);
                    tools.control.close_handle().disconnect();
                    (guest, stack) = launch(false).await;
                    cold_reboots += 1;
                    println!("cold reboot {cold_reboots} at {:?}", start.elapsed());
                    continue;
                }
                for event in guest.take_vcpu_fault_events() {
                    println!("vCPU event: {event:?}");
                }
                if let Some(halt) = guest.vcpu_halt() {
                    panic!("guest halted: {halt:?}");
                }
                if let Ok(shot) = guest.screenshot().await {
                    std::fs::write(base.join("setup.png"), shot.png).unwrap();
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            let reply = tools
                .control
                .call(lightvm_tools::control::Command::File {
                    request: lightvm_tools::protocol::FileRequest::Read {
                        share: "outbox".into(),
                        path: lightvm_tools::protocol::RelativePath::new(vec!["tpm.log".into()])
                            .unwrap(),
                        offset: 0,
                        length: 16384,
                    },
                })
                .unwrap();
            let text = match reply {
                lightvm_proto::ToolReply::File {
                    reply: lightvm_tools::protocol::FileReply::Data { data, .. },
                } => String::from_utf8(lightvm_tools::protocol::decode(&data, 16384).unwrap())
                    .unwrap(),
                other => panic!("{other:?}"),
            };
            std::fs::write(base.join(format!("tpm-{cycle}.log")), &text).unwrap();
            println!("{}\n{text}", base.display());
            assert!(text.contains("secure_boot=1"), "{text}");
            assert!(text.contains("tpm_version=2"), "{text}");
            // Windows provisions its owner authorization; the PE-only NV test
            // is not required to be writable under the installed OS.
            let read_log = || match tools
                .control
                .call(lightvm_tools::control::Command::File {
                    request: lightvm_tools::protocol::FileRequest::Read {
                        share: "outbox".into(),
                        path: lightvm_tools::protocol::RelativePath::new(vec![
                            "installed.log".into(),
                        ])
                        .unwrap(),
                        offset: 0,
                        length: 16384,
                    },
                })
                .unwrap()
            {
                lightvm_proto::ToolReply::File {
                    reply: lightvm_tools::protocol::FileReply::Data { data, .. },
                } => {
                    String::from_utf8_lossy(&lightvm_tools::protocol::decode(&data, 16384).unwrap())
                        .into_owned()
                }
                other => panic!("{other:?}"),
            };
            let first = read_log();
            std::fs::write(base.join("installed-before.log"), &first).unwrap();
            println!("Installed guest connected: {}\n{first}", base.display());
            let wait = Instant::now();
            let mut disconnected = false;
            loop {
                assert!(
                    wait.elapsed() < Duration::from_secs(600),
                    "warm reboot timeout"
                );
                if rebooted(&guest) {
                    stack.stop();
                    let closed = guest.close().await.unwrap();
                    assert_eq!(closed.active_at_close, 0);
                    tools.control.close_handle().disconnect();
                    (guest, stack) = launch(false).await;
                    cold_reboots += 1;
                    println!("guest reboot: cold reboot {cold_reboots}");
                    continue;
                }
                for event in guest.take_vcpu_fault_events() {
                    println!("vCPU event: {event:?}");
                }
                if let Some(halt) = guest.vcpu_halt() {
                    panic!("guest halted: {halt:?}");
                }
                if let Ok(shot) = guest.screenshot().await {
                    std::fs::write(base.join("installed.png"), shot.png).unwrap();
                }
                if !tools.control.connected() {
                    disconnected = true;
                }
                if disconnected && tools.control.connected() {
                    break;
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            let second = read_log();
            std::fs::write(base.join("installed-after.log"), &second).unwrap();
            assert_ne!(
                first, second,
                "boot timestamp must change after guest shutdown /r"
            );
            assert!(second.contains("Windows 11"), "{second}");
            assert!(cold_reboots >= 1, "the guest's own reboot was not seen");
            std::fs::write(
                base.join("cold-reboots.txt"),
                format!("cold_reboots={cold_reboots}\n"),
            )
            .unwrap();
            if bitlocker {
                let read_out = |name: &str| -> Option<String> {
                    match tools
                        .control
                        .call(lightvm_tools::control::Command::File {
                            request: lightvm_tools::protocol::FileRequest::Read {
                                share: "outbox".into(),
                                path: lightvm_tools::protocol::RelativePath::new(vec![name.into()])
                                    .ok()?,
                                offset: 0,
                                length: 16384,
                            },
                        })
                        .ok()?
                    {
                        lightvm_proto::ToolReply::File {
                            reply: lightvm_tools::protocol::FileReply::Data { data, .. },
                        } => Some(
                            String::from_utf8_lossy(
                                &lightvm_tools::protocol::decode(&data, 16384).ok()?,
                            )
                            .into_owned(),
                        ),
                        _ => None,
                    }
                };
                // From here every (guest-requested) reboot starts without the
                // installation ISO: LAB.CMD reboots once while it is attached.
                let wait = Instant::now();
                let reboots_before = cold_reboots;
                let mut before = None;
                let after = loop {
                    assert!(
                        wait.elapsed() < Duration::from_secs(1200),
                        "BitLocker timeout: {}",
                        base.display()
                    );
                    if rebooted(&guest) {
                        stack.stop();
                        let closed = guest.close().await.unwrap();
                        assert_eq!(closed.active_at_close, 0);
                        tools.control.close_handle().disconnect();
                        (guest, stack) = launch(true).await;
                        cold_reboots += 1;
                        println!("BitLocker reboot: cold reboot {cold_reboots}");
                        continue;
                    }
                    if let Some(halt) = guest.vcpu_halt() {
                        panic!("guest halted: {halt:?}");
                    }
                    if let Ok(shot) = guest.screenshot().await {
                        std::fs::write(base.join("bitlocker.png"), shot.png).unwrap();
                    }
                    if tools.control.connected() {
                        if before.is_none() {
                            before = read_out("bitlocker-before.log")
                                .filter(|t| t.contains("VolumeStatus"));
                            if let Some(t) = &before {
                                println!("BitLocker before reboot:\n{t}");
                            }
                        }
                        if read_out("bitlocker-done.log").is_some() {
                            break read_out("bitlocker-after.log").unwrap_or_default();
                        }
                    }
                    tokio::time::sleep(Duration::from_secs(3)).await;
                };
                let before = before.expect("BitLocker state read before the reboot");
                for (name, text) in [
                    ("bitlocker-before.log", &before),
                    ("bitlocker-after.log", &after),
                ] {
                    std::fs::write(base.join(name), text).unwrap();
                }
                if let Some(e) = read_out("bitlocker-enable.log") {
                    std::fs::write(base.join("bitlocker-enable.log"), e).unwrap();
                }
                println!("BitLocker after reboot:\n{after}");
                let field = |text: &str, name: &str| {
                    text.lines()
                        .find(|l| l.starts_with(name))
                        .and_then(|l| l.split_once(':'))
                        .map(|(_, v)| v.trim().to_owned())
                        .unwrap_or_default()
                };
                // Encryption started with a TPM protector before the reboot ...
                assert!(
                    matches!(
                        field(&before, "VolumeStatus").as_str(),
                        "EncryptionInProgress" | "FullyEncrypted"
                    ),
                    "{before}"
                );
                assert!(field(&before, "KeyProtector").contains("Tpm"), "{before}");
                // ... and Windows started again from the volume: the TPM
                // released its key with no recovery prompt.
                assert!(
                    cold_reboots >= reboots_before + 2,
                    "detach and BitLocker reboots expected"
                );
                assert_eq!(field(&after, "LockStatus"), "Unlocked", "{after}");
                assert!(field(&after, "KeyProtector").contains("Tpm"), "{after}");
                assert_ne!(field(&after, "VolumeStatus"), "FullyDecrypted", "{after}");
                std::fs::write(
                    base.join("bitlocker-results.txt"),
                    "bitlocker_tpm_protector=passed\nunlocked_from_tpm_after_cold_reboot=passed\n",
                )
                .unwrap();
            }
            let stopped = guest
                .shutdown(qemu_backend::run::ShutdownPolicy {
                    powerdown_timeout: Duration::from_secs(60),
                    quit_timeout: Duration::from_secs(5),
                    kill_timeout: Duration::from_secs(10),
                })
                .await
                .unwrap();
            assert_eq!(stopped.active_after, 0);
            stack.stop();
            if let Some(boot) = boot {
                boot.abort();
            }
            let closed = guest.close().await.unwrap();
            assert_eq!(closed.active_at_close, 0);
            assert!(
                closed.job_closed && closed.process_handle_released,
                "{closed:?}"
            );
        });
    }
    std::fs::write(base.join("results.txt"), "guest=installed Windows 11 Pro x64/WHPX\nmode=audit\noobe=not_qualified\nsetup_no_requirements_bypass=passed\ntpm2=passed\nsecure_boot=passed\nguest_warm_reboot=passed\n").unwrap();
}
