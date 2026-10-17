//! Installed Linux Tools, application transfer, reboot and offline checkpoints.
use super::*;
use lightvm_proto::{
    DisplayAdapter, Label, NetworkProfile, ToolCall, ToolFileReply, ToolFileRequest,
    ToolRelativePath, VmName,
};
use sha2::{Digest, Sha256};

#[path = "desktop_linux.rs"]
mod desktop_linux;

struct ExternalService(std::process::Child);

impl Drop for ExternalService {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

impl ExternalService {
    fn start(executable: &Path, root: &Path) -> (Self, String, Credential) {
        use std::io::BufRead;
        use std::process::{Command, Stdio};
        let mut command = Command::new(executable);
        command
            .args(["--catalog"])
            .arg(root.join("catalog"))
            .arg("--runtime-dir")
            .arg(root.join("ipc"))
            .arg("--audit-log")
            .arg(root.join("audit.jsonl"))
            .arg("--qemu-dir")
            .arg(qemu_dir())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(std::fs::File::create(root.join("packaged-service.stderr.log")).unwrap());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        let mut service = Self(command.spawn().expect("packaged service start"));
        let stdout = service.0.stdout.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut line = String::new();
            let result = std::io::BufReader::new(stdout).read_line(&mut line);
            let _ = tx.send(result.map(|_| line));
        });
        let line = rx
            .recv_timeout(Duration::from_secs(90))
            .expect("packaged service ready timeout")
            .unwrap();
        let ready: serde_json::Value =
            serde_json::from_str(&line).expect("packaged service ready JSON");
        assert_eq!(ready["event"], "ready");
        assert_eq!(ready["pid"], service.0.id());
        let endpoint = ready["endpoint"].as_str().unwrap().to_owned();
        let credential =
            Credential::from_hex(ready["credential"].as_str().unwrap()).expect("ready credential");
        // The ready credential stays in memory, never in the evidence/logs.
        std::fs::write(root.join("packaged-service.json"), serde_json::to_vec_pretty(&serde_json::json!({
            "pid":service.0.id(), "binary_sha256":lightvm_tools::protocol::encode(&hash_file(executable))
        })).unwrap()).unwrap();
        (service, endpoint, credential)
    }

    async fn shutdown(&mut self) {
        use std::io::Write;
        self.0
            .stdin
            .as_mut()
            .unwrap()
            .write_all(b"shutdown\n")
            .unwrap();
        self.0.stdin.as_mut().unwrap().flush().unwrap();
        let started = Instant::now();
        loop {
            if let Some(status) = self.0.try_wait().unwrap() {
                assert!(status.success(), "packaged service shutdown failed");
                return;
            }
            assert!(
                started.elapsed() < Duration::from_secs(30),
                "packaged service shutdown timeout"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

fn hash_file(path: &Path) -> sha2::digest::Output<Sha256> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).unwrap();
    let mut hash = Sha256::new();
    let mut buffer = [0; 128 * 1024];
    loop {
        let count = file.read(&mut buffer).unwrap();
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    hash.finalize()
}

async fn call(
    ui: &mut Client,
    vm: VmId,
    run: RunId,
    request: ToolFileRequest,
) -> Option<ToolFileReply> {
    match req(
        ui,
        Request::GuestTools {
            vm_id: vm,
            run_id: run,
            call: ToolCall::File { request },
        },
    )
    .await
    {
        ServerMessage::Response {
            result:
                ResponseBody::GuestTools {
                    reply: lightvm_proto::ToolReply::File { reply },
                },
            ..
        } => Some(reply),
        _ => None,
    }
}

fn path(name: &str) -> ToolRelativePath {
    ToolRelativePath::new(vec![name.into()]).unwrap()
}

async fn read(ui: &mut Client, vm: VmId, run: RunId, name: &str) -> Option<Vec<u8>> {
    match call(
        ui,
        vm,
        run,
        ToolFileRequest::Read {
            share: "outbox".into(),
            path: path(name),
            offset: 0,
            length: 16384,
        },
    )
    .await
    {
        Some(ToolFileReply::Data { data, .. }) => {
            Some(lightvm_tools::protocol::decode(&data, 16384).unwrap())
        }
        _ => None,
    }
}

async fn put(ui: &mut Client, vm: VmId, run: RunId, name: &str, bytes: &[u8]) {
    assert!(bytes.len() <= 16384);
    let reply = call(
        ui,
        vm,
        run,
        ToolFileRequest::Begin {
            replace: false,
            share: "inbox".into(),
            path: path(name),
            size: bytes.len() as u64,
            sha256: lightvm_tools::protocol::encode(&Sha256::digest(bytes)),
        },
    )
    .await;
    let transfer = match reply {
        Some(ToolFileReply::Transfer { transfer }) => transfer,
        _ => panic!("Tools Begin failed"),
    };
    assert!(matches!(
        call(
            ui,
            vm,
            run,
            ToolFileRequest::Write {
                transfer: transfer.clone(),
                offset: 0,
                data: lightvm_tools::protocol::encode(bytes)
            }
        )
        .await,
        Some(ToolFileReply::Ok)
    ));
    assert!(matches!(
        call(ui, vm, run, ToolFileRequest::Commit { transfer }).await,
        Some(ToolFileReply::Ok)
    ));
}

async fn command(ui: &mut Client, vm: VmId, run: RunId, action: &str, state: &str) -> String {
    let nonce = RunId::new_v7().to_string();
    put(
        ui,
        vm,
        run,
        "command.json",
        &serde_json::to_vec(&serde_json::json!({"nonce":nonce,"action":action,"state":state}))
            .unwrap(),
    )
    .await;
    nonce
}

async fn json_until(
    ui: &mut Client,
    vm: VmId,
    run: RunId,
    name: &str,
    field: &str,
    value: &str,
    equal: bool,
) -> serde_json::Value {
    let started = Instant::now();
    loop {
        if let Some(bytes) = read(ui, vm, run, name).await {
            let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            if (json[field].as_str() == Some(value)) == equal {
                assert!(json.get("error").is_none(), "guest error: {json}");
                return json;
            }
        }
        assert!(
            started.elapsed() < Duration::from_secs(300),
            "waiting for {name}: {field}"
        );
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

async fn boot(
    ui: &mut Client,
    vm: VmId,
    previous_boot: &str,
    evidence: &Path,
    tag: &str,
) -> (RunId, String) {
    ok(
        ui,
        Request::SubmitVm {
            request: VmRequest {
                operation_id: OperationId::new_v7(),
                vm_id: vm,
                expected_run_id: None,
                action: VmAction::Start,
            },
        },
    )
    .await;
    let run = wait_state(ui, vm, VmState::Running, T).await.unwrap();
    json_until(ui, vm, run, "ready.json", "boot_id", previous_boot, false).await;
    // Snapshot restore also restores old outbox files. Require a fresh
    // nonce response rather than mistaking a restored ready file for boot.
    let nonce = command(ui, vm, run, "status", "").await;
    let ready = json_until(ui, vm, run, "result.json", "nonce", &nonce, true).await;
    assert_ne!(ready["boot_id"], previous_boot);
    assert_eq!(ready["tools_installed"], true);
    assert_eq!(ready["desktop_autostart"], true);
    assert!(
        ready["os_release"]
            .as_str()
            .unwrap()
            .contains("Ubuntu 24.04")
    );
    std::fs::write(
        evidence.join(format!("{tag}-ready.json")),
        serde_json::to_vec_pretty(&ready).unwrap(),
    )
    .unwrap();
    println!(
        "{tag}: installed Tools connected, boot {}",
        ready["boot_id"]
    );
    (run, ready["boot_id"].as_str().unwrap().to_owned())
}

async fn poweroff(ui: &mut Client, vm: VmId, run: RunId) {
    command(ui, vm, run, "poweroff", "").await;
    wait_state(ui, vm, VmState::Stopped, T).await;
}

async fn inspect(ui: &mut Client, vm: VmId, run: RunId, state: &str, sha: &str) {
    let nonce = command(ui, vm, run, "inspect", "").await;
    let result = json_until(ui, vm, run, "result.json", "nonce", &nonce, true).await;
    assert_eq!(result["state"], state);
    assert_eq!(result["sha256"], sha);
}

#[test]
#[ignore = "real WHPX, official Ubuntu disk and prepared NoCloud fixture required"]
fn installed_linux_tools_build_restart_snapshot_and_baseline() {
    qualify(false);
}

#[test]
#[ignore = "real installed guest: two guest-requested WHPX reboots retaining run, Tools and checkpoint contents"]
fn installed_linux_guest_warm_reboot() {
    qualify(true);
}

fn qualify(warm_reboot: bool) {
    lightvm_service::tools::ToolsRun::load_binaries()
        .expect("stage packaged Tools beside the test executable on E:");
    let fixture = PathBuf::from(
        std::env::var_os("LIGHTVM_INSTALLED_LINUX_FIXTURE")
            .expect("LIGHTVM_INSTALLED_LINUX_FIXTURE"),
    );
    let lab = PathBuf::from(std::env::var_os("LIGHTVM_LAB").expect("LIGHTVM_LAB"));
    let root = lab
        .join("installed-linux")
        .join(&RunId::new_v7().to_string()[24..]);
    std::fs::create_dir_all(&root).unwrap();
    let store = SqliteStore::open(&root.join("catalog")).unwrap();
    let mut setup = QemuSetup::new(qemu_dir(), store.root(), root.join("p"));
    setup.cpu_model = "Skylake-Client-v4".into();
    let (host, backend, capacity) = probe(setup).unwrap();
    assert!(capacity.accelerator_available);
    assert!(
        capacity.free_storage_gib >= capacity.reserved_storage_gib + 10,
        "installed-Linux qualification needs 10 GiB above the production {} GiB reserve; catalog volume has {} GiB free",
        capacity.reserved_storage_gib,
        capacity.free_storage_gib
    );
    let source_sha = hash_file(&fixture.join("ubuntu.vmdk"));
    let engine = CoreEngine::new(
        store,
        FileAuditSink::open(&root.join("audit.jsonl")).unwrap(),
        backend,
        ManagedMediaInspector::new(root.join("catalog")),
        core_config(capacity),
    )
    .unwrap();
    println!("Installed Linux evidence: {}", root.display());
    let external_executable = std::env::var_os("LIGHTVM_INSTALLED_SERVICE_EXE");
    let mut engine = Some(engine);
    let mut host = Some(host);
    if external_executable.is_some() {
        // QemuHost owns a runtime: release it outside the async context,
        // together with the preparatory engine's catalog lock.
        drop(engine.take());
        drop(host.take());
    }
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let mut service = ServiceConfig::new(root.join("catalog"));
            service.runtime_dir = Some(root.join("ipc"));
            service.display_dir = Some(root.join("d"));
            let mut in_process = None;
            let mut external = None;
            let (endpoint, credential) = if let Some(executable) = external_executable {
                let (child, endpoint, credential) = ExternalService::start(Path::new(&executable), &root);
                external = Some(child);
                (endpoint, credential)
            } else {
                let (handle, credential) = start(service, Box::new(engine.take().unwrap()), Box::new(host.take().unwrap())).await.unwrap();
                let endpoint = handle.endpoint().to_owned();
                in_process = Some(handle);
                (endpoint, credential)
            };
            let mut ui = Client::connect(&endpoint).await.unwrap();
            ui.hello(&credential).await.unwrap();
            let seed = import(&mut ui, &fixture.join("seed.iso"), MediaKind::DataIso).await;
            let mut vm = match ok(
                &mut ui,
                Request::ImportVmware {
                    vmx_path: fixture.join("ubuntu.vmx").to_str().unwrap().into(),
                    name: VmName::parse("Installed Linux Tools").unwrap(),
                    display: DisplayAdapter::Standard,
                },
            )
            .await
            {
                ResponseBody::VmwareImported { vm } => vm,
                _ => panic!("import failed"),
            };
            vm.config.network = NetworkProfile::Offline;
            vm.config.data_media = Some(seed.id);
            vm.config.tools.enabled = true;
            vm.config.tools.files_transfer = true;
            ok(
                &mut ui,
                Request::UpdateVm {
                    vm_id: vm.id,
                    expected_revision: vm.config_revision,
                    config: vm.config,
                },
            )
            .await;
            let id = vm.id;
            let (run, boot_a) = boot(&mut ui, id, "", &root, "first").await;
            let expected_output = format!("Installed Linux application {}", RunId::new_v7());
            let app_a = format!("print('{expected_output}')\n");
            put(&mut ui, id, run, "application.py", app_a.as_bytes()).await;
            let nonce = command(&mut ui, id, run, "build", "A").await;
            let a = json_until(&mut ui, id, run, "result.json", "nonce", &nonce, true).await;
            assert_eq!(a["state"], "A");
            assert_eq!(a["output"], expected_output + "\n");
            let artifact = read(&mut ui, id, run, "application.pyz").await.unwrap();
            let hash_a = lightvm_tools::protocol::encode(&Sha256::digest(&artifact));
            assert_eq!(a["sha256"], hash_a);
            std::fs::write(root.join("application-A.pyz"), artifact).unwrap();
            let log = read(&mut ui, id, run, "build.log").await.unwrap();
            assert!(String::from_utf8_lossy(&log).contains("compileall=passed"));
            std::fs::write(root.join("build-A.log"), log).unwrap();
            poweroff(&mut ui, id, run).await;
            let snapshot = match ok(
                &mut ui,
                Request::TakeSnapshot {
                    vm_id: id,
                    name: Label::parse("installed-A").unwrap(),
                    description: "Tools/application checkpoint".into(),
                },
            )
            .await
            {
                ResponseBody::Snapshot { snapshot } => {
                    assert!(snapshot.clean_shutdown);
                    snapshot.snapshot_id
                }
                _ => panic!("snapshot failed"),
            };
            let baseline = match ok(
                &mut ui,
                Request::CreateBaseline {
                    vm_id: id,
                    label: Label::parse("installed-A").unwrap(),
                    allow_unclean: false,
                },
            )
            .await
            {
                ResponseBody::BaselineCreated { baseline } => {
                    assert!(baseline.clean_shutdown);
                    baseline.baseline_id
                }
                _ => panic!("baseline failed"),
            };
            let (run, boot_b) = boot(&mut ui, id, &boot_a, &root, "second").await;
            inspect(&mut ui, id, run, "A", &hash_a).await;
            put(&mut ui, id, run, "application.py", b"print('mutated B')\n").await;
            let nonce = command(&mut ui, id, run, "build", "B").await;
            let b = json_until(&mut ui, id, run, "result.json", "nonce", &nonce, true).await;
            assert_eq!(b["state"], "B");
            assert_ne!(b["sha256"], hash_a);
            std::fs::write(
                root.join("mutation-B.json"),
                serde_json::to_vec_pretty(&b).unwrap(),
            )
            .unwrap();
            poweroff(&mut ui, id, run).await;
            ok(
                &mut ui,
                Request::RevertSnapshot {
                    vm_id: id,
                    snapshot_id: snapshot,
                },
            )
            .await;
            let (run, boot_snapshot) = boot(&mut ui, id, &boot_b, &root, "snapshot-restored").await;
            inspect(&mut ui, id, run, "A", &hash_a).await;
            poweroff(&mut ui, id, run).await;
            // Baseline restore must protect snapshots stored in the primary
            // disk. Explicitly delete only this test's verified snapshot.
            match req(
                &mut ui,
                Request::RestoreBaseline {
                    vm_id: id,
                    baseline_id: baseline,
                },
            )
            .await
            {
                ServerMessage::Error {
                    error: ReplyError::Operational(error),
                    ..
                } => {
                    assert_eq!(reason(&error), Some("snapshots_exist"));
                }
                _ => panic!("baseline restore did not protect existing snapshots"),
            }
            ok(
                &mut ui,
                Request::DeleteSnapshot {
                    vm_id: id,
                    snapshot_id: snapshot,
                },
            )
            .await;
            ok(
                &mut ui,
                Request::RestoreBaseline {
                    vm_id: id,
                    baseline_id: baseline,
                },
            )
            .await;
            let (run, boot_baseline) =
                boot(&mut ui, id, &boot_snapshot, &root, "baseline-restored").await;
            inspect(&mut ui, id, run, "A", &hash_a).await;
            if warm_reboot {
                let mut previous_boot = boot_baseline;
                for index in 1..=2 {
                    command(&mut ui, id, run, "reboot", "").await;
                    let rebooted = json_until(
                        &mut ui, id, run, "ready.json", "boot_id", &previous_boot, false,
                    ).await;
                    assert_eq!(rebooted["tools_installed"], true);
                    match ok(&mut ui, Request::VmStatus { vm_id: id }).await {
                        ResponseBody::Vm { vm } => {
                            assert_eq!(vm.state, VmState::Running);
                            assert_eq!(vm.run_id, Some(run), "guest reboot changed the run");
                            assert!(vm.vcpu_halt.is_none(), "guest reboot halted the vCPUs");
                        }
                        _ => panic!("VM status unavailable after guest reboot"),
                    }
                    std::fs::write(
                        root.join(format!("guest-reboot-{index}-ready.json")),
                        serde_json::to_vec_pretty(&rebooted).unwrap(),
                    ).unwrap();
                    inspect(&mut ui, id, run, "A", &hash_a).await;
                    previous_boot = rebooted["boot_id"].as_str().unwrap().to_owned();
                    println!("guest reboot {index}: same run {run}, Tools and artifact verified, boot {previous_boot}");
                }
            }
            poweroff(&mut ui, id, run).await;
            if let Some(handle) = in_process { handle.shutdown().await; }
            if let Some(mut service) = external { service.shutdown().await; }
        });
    assert_eq!(hash_file(&fixture.join("ubuntu.vmdk")), source_sha);
    std::fs::write(root.join("results.txt"), format!("installed_linux_tools=passed\nsource_unchanged=passed\napplication_upload_build_log_checksum=passed\nclean_shutdown=passed\ncold_restart=passed\nsnapshot_restore=passed\nbaseline_preserves_existing_snapshots=passed\nguest_requested_reboot={}\nbaseline_restore=passed\n", if warm_reboot {"two_passed_same_run_tools_and_artifact"} else {"not_run"})).unwrap();
}
