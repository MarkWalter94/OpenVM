//! Tests with real QEMU and WHPX (F1 exit criterion, item F1-12 of
//! `docs/RELEASE_PLAN.md`). Ignored by default: they need the local
//! lab and are enabled with the `LIGHTVM_QEMU_DIR` environment variable
//! (folder of portable QEMU 11.1.0 with `share/edk2-*`).
//!
//! ```text
//! LIGHTVM_QEMU_DIR=E:/OpenVM/lab/qemu/qemu-11.1.0 \
//!   cargo test -p qemu-backend --test real_qemu -- --ignored --nocapture --test-threads=1
//! ```
//!
//! Optional variables: `LIGHTVM_LAB_DIR` (default `<workspace>/lab`),
//! `LIGHTVM_ISO` (default `lab/media/ubuntu-26.04.1-desktop-amd64.iso`),
//! `LIGHTVM_F1_CYCLES` (default 20).
//!
//! No test uses TCG. The times recorded in `lab/logs/f1-03/` come
//! from a shared host and are not release measurements.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use lightvm_proto::{
    DisplayGeometry, ErrorCode, InputEvent, KeyAction, MediaId, PointerMove, QCode, RunId,
    UtcTimestamp, VmState,
};
use qemu_backend::cmdline::{
    Accelerator, CpuModel, DeviceProfile, DiskFormat, DiskSpec, FirmwareKind, MachineFamily,
    VmLaunchSpec,
};
use qemu_backend::probe::{self, AcceleratorStatus, HostCapabilities, ProbeConfig};
use qemu_backend::qmp::{QmpClient, QmpConfig};
use qemu_backend::run::{ExitCause, ProcessState, StopPhase, check_no_nic};
use qemu_backend::{
    BackendError, ExitCodeClass, IdentityStatus, ManagedPath, ManagedPaths, ProcessIdentity,
    QemuRun, RunConfig, ShutdownPolicy, prepare,
};

/// Real tests must not overlap (shared host, measured times).
static SERIAL: Mutex<()> = Mutex::new(());

const ISO_NAME: &str = "ubuntu-26.04.1-desktop-amd64.iso";

struct Lab {
    qemu_dir: PathBuf,
    iso: PathBuf,
    /// Working root of the session (`lab/vms/f1-03/<session>`).
    work: PathBuf,
    /// Log directory (`lab/logs/f1-03`).
    logs: PathBuf,
    session: String,
}

fn lab() -> Option<Lab> {
    let qemu_dir = PathBuf::from(std::env::var_os("LIGHTVM_QEMU_DIR")?);
    let lab_dir = std::env::var_os("LIGHTVM_LAB_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../lab"));
    let lab_dir = std::fs::canonicalize(&lab_dir).expect("lab folder");
    let lab_dir = strip_verbatim(&lab_dir);
    let iso = std::env::var_os("LIGHTVM_ISO")
        .map(PathBuf::from)
        .unwrap_or_else(|| lab_dir.join("media").join(ISO_NAME));
    let session = format!("s{}", UtcTimestamp::now().as_unix_millis());
    let work = lab_dir.join("vms").join("f1-03").join(&session);
    let logs = lab_dir.join("logs").join("f1-03");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::create_dir_all(&logs).unwrap();
    Some(Lab {
        qemu_dir,
        iso,
        work,
        logs,
        session,
    })
}

/// `canonicalize` on Windows returns `\\?\E:\...`, (rightly) rejected
/// by `ManagedPath`.
fn strip_verbatim(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    PathBuf::from(s.strip_prefix(r"\\?\").unwrap_or(&s).to_string())
}

macro_rules! require_lab {
    () => {
        match lab() {
            Some(l) => l,
            None => {
                eprintln!("LIGHTVM_QEMU_DIR not set: real test skipped");
                return;
            }
        }
    };
}

struct Log {
    file: std::fs::File,
}

impl Log {
    fn new(lab: &Lab, name: &str) -> Self {
        let path = lab.logs.join(format!("{}-{name}.log", lab.session));
        eprintln!("log: {}", path.display());
        Self {
            file: std::fs::File::create(path).unwrap(),
        }
    }
    fn line(&mut self, s: impl AsRef<str>) {
        let s = s.as_ref();
        eprintln!("{s}");
        let _ = writeln!(self.file, "{s}");
    }
}

/// Lab paths for the test runs.
struct LabPaths {
    qemu_dir: PathBuf,
    work: PathBuf,
    iso_id: MediaId,
    iso: PathBuf,
    base: PathBuf,
    /// Optional data medium (read-only ISO on the second CD-ROM).
    data: Option<(MediaId, PathBuf)>,
}

impl LabPaths {
    fn run_dir_path(&self, run: &RunId) -> PathBuf {
        self.work.join("runs").join(run.to_string())
    }
    fn private_dir_path(&self, run: &RunId) -> PathBuf {
        self.work.join("p").join(run.to_string())
    }
    fn qemu_img(&self) -> PathBuf {
        self.qemu_dir.join("qemu-img.exe")
    }
    /// Prepares the run directories, the copy of the UEFI variables and
    /// the qcow2 overlay created with `qemu-img` on the shared base.
    fn prepare_run(&self, run: &RunId) -> Duration {
        let t0 = Instant::now();
        let mp = |p: PathBuf| ManagedPath::verify(p).unwrap();
        let rd = self.run_dir_path(run);
        let dirs = prepare::prepare_run_dirs(&mp(rd.clone()), &mp(self.private_dir_path(run)))
            .expect("run directories");
        assert_eq!(dirs.private_acl_entries, 1);
        prepare::copy_firmware_vars(
            &mp(self.qemu_dir.join("share").join("edk2-i386-vars.fd")),
            &mp(rd.join("vars.fd")),
        )
        .unwrap();
        prepare::create_overlay(
            &mp(self.qemu_img()),
            &mp(self.base.clone()),
            DiskFormat::Qcow2,
            &mp(rd.join("overlay.qcow2")),
            prepare::QEMU_IMG_TIMEOUT,
        )
        .expect("qemu-img create overlay");
        t0.elapsed()
    }
}

impl ManagedPaths for LabPaths {
    fn qemu_system(&self) -> qemu_backend::error::Result<ManagedPath> {
        ManagedPath::verify(self.qemu_dir.join("qemu-system-x86_64.exe"))
    }
    fn firmware_code(&self) -> qemu_backend::error::Result<ManagedPath> {
        ManagedPath::verify(self.qemu_dir.join("share").join("edk2-x86_64-code.fd"))
    }
    fn firmware_vars(&self, run: &RunId) -> qemu_backend::error::Result<Option<ManagedPath>> {
        ManagedPath::verify(self.run_dir_path(run).join("vars.fd")).map(Some)
    }
    fn disk(&self, run: &RunId) -> qemu_backend::error::Result<Option<ManagedPath>> {
        ManagedPath::verify(self.run_dir_path(run).join("overlay.qcow2")).map(Some)
    }
    fn protected_readonly(&self, _run: &RunId) -> qemu_backend::error::Result<Vec<ManagedPath>> {
        Ok(vec![ManagedPath::verify(&self.base)?])
    }
    fn media(&self, media: &MediaId) -> qemu_backend::error::Result<ManagedPath> {
        if *media == self.iso_id {
            ManagedPath::verify(&self.iso)
        } else if let Some((id, path)) = &self.data
            && id == media
        {
            ManagedPath::verify(path)
        } else {
            Err(BackendError::invalid("media", "medium not registered"))
        }
    }
    fn run_dir(&self, run: &RunId) -> qemu_backend::error::Result<ManagedPath> {
        ManagedPath::verify(self.run_dir_path(run))
    }
    fn private_dir(&self, run: &RunId) -> qemu_backend::error::Result<ManagedPath> {
        ManagedPath::verify(self.private_dir_path(run))
    }
}

fn lab_paths(lab: &Lab, log: &mut Log) -> LabPaths {
    let base = lab.work.join("base.qcow2");
    let p = LabPaths {
        qemu_dir: lab.qemu_dir.clone(),
        work: lab.work.clone(),
        iso_id: MediaId::new_v7(),
        iso: lab.iso.clone(),
        base: base.clone(),
        data: None,
    };
    // Creation of the main disk (F1 criterion "disk creation").
    let t0 = Instant::now();
    let base_mp = ManagedPath::verify(&base).unwrap();
    prepare::create_disk(
        &ManagedPath::verify(p.qemu_img()).unwrap(),
        &base_mp,
        DiskFormat::Qcow2,
        1024,
        prepare::QEMU_IMG_TIMEOUT,
    )
    .expect("qemu-img create base");
    log.line(format!(
        "1024 MiB qcow2 disk created with prepare::create_disk in {:?}: {} ({} bytes)",
        t0.elapsed(),
        base.display(),
        std::fs::metadata(&base).unwrap().len()
    ));
    let again = prepare::create_disk(
        &ManagedPath::verify(p.qemu_img()).unwrap(),
        &base_mp,
        DiskFormat::Qcow2,
        1024,
        prepare::QEMU_IMG_TIMEOUT,
    );
    assert!(
        again.is_err(),
        "the backend does not overwrite an existing disk"
    );
    let res = prepare::host_resources(&ManagedPath::verify(&lab.work).unwrap()).unwrap();
    log.line(format!(
        "measured resources: total memory {} MiB, available {} MiB, free space {} MiB",
        res.total_memory_bytes >> 20,
        res.available_memory_bytes >> 20,
        res.free_disk_bytes >> 20
    ));
    std::fs::create_dir_all(lab.work.join("p")).unwrap();
    p
}

fn probe_caps(lab: &Lab, paths: &LabPaths, log: &mut Log) -> HostCapabilities {
    let cfg = ProbeConfig::default();
    let t0 = Instant::now();
    let mut caps = probe::probe_host(
        &paths.qemu_system().unwrap(),
        &ManagedPath::verify(paths.qemu_img()).unwrap(),
        &cfg,
    )
    .expect("probe_host");
    log.line(format!(
        "probe_host: {:?}; QEMU {} sha256 {}; qemu-img sha256 {}; accelerators {:?}; recommended machine {:?}; diagnostics {:?}",
        t0.elapsed(),
        caps.version,
        caps.qemu_system_sha256,
        caps.qemu_img_sha256,
        caps.accelerators,
        caps.recommended_machine(MachineFamily::Q35).map(|m| m.as_str().to_owned()),
        caps.diagnostics()
    ));
    let probe_dir = ManagedPath::verify(lab.work.join("p").join("probe")).unwrap();
    std::fs::create_dir_all(probe_dir.as_path()).unwrap();
    let machine = caps
        .recommended_machine(MachineFamily::Q35)
        .unwrap()
        .clone();
    let cpu = caps.cpu_model(CpuModel::VERIFIED_DEFAULT).unwrap();
    let rt = rt();
    let t0 = Instant::now();
    let status = rt.block_on(probe::probe_accelerator(
        &caps,
        Accelerator::Whpx,
        &machine,
        FirmwareKind::Uefi,
        &cpu,
        &probe_dir,
        &cfg,
    ));
    log.line(format!(
        "probe_accelerator whpx: {:?} in {:?}",
        status,
        t0.elapsed()
    ));
    assert!(status.is_available(), "WHPX not available: {status}");
    caps.accelerator = status;
    caps
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn spec(caps: &HostCapabilities, paths: &LabPaths, run_id: RunId) -> VmLaunchSpec {
    VmLaunchSpec {
        run_id,
        machine: caps
            .recommended_machine(MachineFamily::Q35)
            .unwrap()
            .clone(),
        profile: DeviceProfile::LINUX,
        accelerator: Accelerator::Whpx,
        cpu: caps.cpu_model(CpuModel::VERIFIED_DEFAULT).unwrap(),
        vcpus: 2,
        cores_per_socket: None,
        memory_mib: 2048,
        disk: Some(DiskSpec {
            format: DiskFormat::Qcow2,
        }),
        install_media: Some(paths.iso_id),
        data_media: None,
        limits: qemu_backend::JobLimits::default(),
        network: qemu_backend::GuestNetwork::None,
    }
}

/// PIDs of the existing `qemu-system-x86_64.exe` processes (all users and
/// all owners: the host is shared, used only for comparison).
fn qemu_pids() -> Vec<u32> {
    let out = Command::new("tasklist")
        .args([
            "/FI",
            "IMAGENAME eq qemu-system-x86_64.exe",
            "/FO",
            "CSV",
            "/NH",
        ])
        .stdin(Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    out.lines()
        .filter_map(|l| l.split("\",\"").nth(1).and_then(|p| p.parse().ok()))
        .collect()
}

/// No running process with the recorded identity (PID + creation
/// time). `Exited` denotes a process object already exited but still
/// referenced by a third-party handle (for example antivirus or host
/// monitoring tools): it is not an orphan. Waits up to 2 s for it to disappear
/// and returns the final state.
fn assert_no_orphan(id: &ProcessIdentity) -> IdentityStatus {
    let t0 = Instant::now();
    let st = loop {
        let st = id.status().expect("identity status");
        if st != IdentityStatus::Exited || t0.elapsed() > Duration::from_secs(2) {
            break st;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(
        !st.is_alive(),
        "orphan QEMU process still running: {id:?} {st:?}"
    );
    if qemu_pids().contains(&id.pid) {
        // A qemu with the same PID exists but it is not ours (different creation
        // time): allowed only if the identity says `Reused`.
        assert_eq!(st, IdentityStatus::Reused, "PID {} still ours", id.pid);
    }
    st
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn stats(v: &[f64]) -> (f64, f64, f64) {
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    let med = if s.len() % 2 == 1 {
        s[s.len() / 2]
    } else {
        (s[s.len() / 2 - 1] + s[s.len() / 2]) / 2.0
    };
    (s[0], med, s[s.len() - 1])
}

/// Probe of the approved binaries, rejected hash and missing accelerator simulated
/// with `-accel kvm` (not compiled into the Windows build: same path as a
/// binary without WHPX).
#[test]
#[ignore = "needs real QEMU: set LIGHTVM_QEMU_DIR"]
fn real_probe_hash_and_missing_accelerator() {
    let _g = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let lab = require_lab!();
    let mut log = Log::new(&lab, "probe");
    let paths = lab_paths(&lab, &mut log);
    let caps = probe_caps(&lab, &paths, &mut log);

    // 1. Unapproved hash: the binary is not executed.
    let cfg = ProbeConfig {
        approved_qemu_system_sha256: vec!["00".repeat(32)],
        ..ProbeConfig::default()
    };
    let e = probe::probe_host(
        &paths.qemu_system().unwrap(),
        &ManagedPath::verify(paths.qemu_img()).unwrap(),
        &cfg,
    )
    .unwrap_err();
    let p = e.to_public();
    log.line(format!(
        "unapproved hash: {e} -> {} / {}",
        p.code, p.message
    ));
    assert!(matches!(e, BackendError::UnapprovedBinary { .. }));
    assert_eq!(p.code, ErrorCode::Unsupported);

    // 2. Missing accelerator: real probe with `-accel kvm`.
    let probe_dir = ManagedPath::verify(lab.work.join("p").join("probe")).unwrap();
    let machine = caps
        .recommended_machine(MachineFamily::Q35)
        .unwrap()
        .clone();
    let cpu = caps.cpu_model("qemu64").unwrap();
    let before = qemu_pids();
    let t0 = Instant::now();
    let status = rt().block_on(probe::probe_accelerator(
        &caps,
        Accelerator::Kvm,
        &machine,
        FirmwareKind::Uefi,
        &cpu,
        &probe_dir,
        &ProbeConfig::default(),
    ));
    log.line(format!(
        "probe_accelerator kvm: {:?} in {:?}",
        status,
        t0.elapsed()
    ));
    assert!(matches!(status, AcceleratorStatus::Unavailable { .. }));
    let leftover: Vec<_> = qemu_pids()
        .into_iter()
        .filter(|p| !before.contains(p))
        .collect();
    assert!(
        leftover.is_empty(),
        "new QEMU processes after the probe: {leftover:?}"
    );

    // 3. Launch refused before the spawn with a negative probe outcome.
    let mut caps_kvm = caps.clone();
    caps_kvm.accelerator = status;
    let run_id = RunId::new_v7();
    paths.prepare_run(&run_id);
    let mut s = spec(&caps, &paths, run_id);
    s.accelerator = Accelerator::Kvm;
    let e = rt()
        .block_on(QemuRun::start(&caps_kvm, &s, &paths, RunConfig::default()))
        .unwrap_err();
    let p = e.to_public();
    log.line(format!(
        "launch with missing accelerator: {} / {}",
        p.code, p.message
    ));
    assert_eq!(p.code, ErrorCode::Unsupported);
    assert!(
        p.message.contains("TCG") && p.message.contains("KVM"),
        "{}",
        p.message
    );
    assert!(!paths.private_dir_path(&run_id).join("qmp.sock").exists());

    // 4. WHPX specification but a probe that verified another accelerator.
    let mut caps_mismatch = caps.clone();
    caps_mismatch.accelerator = AcceleratorStatus::Available {
        accelerator: Accelerator::Kvm,
        machine: machine.as_str().to_owned(),
        cpu: "qemu64".into(),
    };
    let s = spec(&caps, &paths, run_id);
    let e = rt()
        .block_on(QemuRun::start(
            &caps_mismatch,
            &s,
            &paths,
            RunConfig::default(),
        ))
        .unwrap_err();
    log.line(format!(
        "accelerator different from the probe: {}",
        e.to_public().message
    ));
    assert_eq!(e.code(), ErrorCode::Unsupported);
}

/// Invalid paths and resources rejected before the spawn.
#[test]
#[ignore = "needs real QEMU: set LIGHTVM_QEMU_DIR"]
fn real_rejection_of_paths_and_resources() {
    let _g = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let lab = require_lab!();
    let mut log = Log::new(&lab, "rejections");
    let paths = lab_paths(&lab, &mut log);
    let caps = probe_caps(&lab, &paths, &mut log);
    let before = qemu_pids();
    let rt = rt();

    let mut checks: Vec<(&str, BackendError)> = Vec::new();
    // Paths.
    for (name, p) in [
        ("comma", lab.work.join("a,file.driver=nbd").join("x.iso")),
        ("relative", PathBuf::from(r"lab\media\x.iso")),
        ("unc", PathBuf::from(r"\\server\share\x.iso")),
        ("device", PathBuf::from(r"\\.\PhysicalDrive0")),
        ("traversal", lab.work.join("..").join("x.iso")),
        ("protocol", PathBuf::from("nbd://127.0.0.1/x")),
    ] {
        checks.push((name, ManagedPath::verify(p).unwrap_err()));
    }
    // Medium with an invalid path returned by the registry.
    let bad_iso = LabPaths {
        iso: lab.work.join("a,b.iso"),
        qemu_dir: paths.qemu_dir.clone(),
        work: paths.work.clone(),
        iso_id: paths.iso_id,
        base: paths.base.clone(),
        data: None,
    };
    let run_id = RunId::new_v7();
    paths.prepare_run(&run_id);
    let e = rt
        .block_on(QemuRun::start(
            &caps,
            &spec(&caps, &bad_iso, run_id),
            &bad_iso,
            RunConfig::default(),
        ))
        .unwrap_err();
    checks.push(("medium with comma", e));
    // Unregistered medium.
    let mut s = spec(&caps, &paths, run_id);
    s.install_media = Some(MediaId::new_v7());
    checks.push((
        "unregistered medium",
        rt.block_on(QemuRun::start(&caps, &s, &paths, RunConfig::default()))
            .unwrap_err(),
    ));
    // Resources.
    for (name, vcpus, mem) in [
        ("vcpu 0", 0, 2048),
        ("vcpu 65", 65, 2048),
        ("ram 64 MiB", 2, 64),
        ("excessive ram", 2, u32::MAX),
    ] {
        let mut s = spec(&caps, &paths, run_id);
        s.vcpus = vcpus;
        s.memory_mib = mem;
        checks.push((
            name,
            rt.block_on(QemuRun::start(&caps, &s, &paths, RunConfig::default()))
                .unwrap_err(),
        ));
    }
    checks.push(("cpu host", CpuModel::nominal("host").unwrap_err()));
    checks.push((
        "cpu with flags",
        CpuModel::nominal("qemu64,+vmx").unwrap_err(),
    ));
    checks.push((
        "q35 alias machine",
        qemu_backend::MachineType::versioned_q35("q35").unwrap_err(),
    ));
    checks.push(("missing cpu", caps.cpu_model("Fantasia-v9").unwrap_err()));
    checks.push((
        "nbd protocol",
        qemu_backend::validate::check_block_protocol("nbd").unwrap_err(),
    ));
    checks.push((
        "https protocol",
        qemu_backend::validate::check_block_protocol("https").unwrap_err(),
    ));

    for (name, e) in &checks {
        let p = e.to_public();
        log.line(format!("rejection [{name}]: {} / {}", p.code, p.message));
        assert!(
            matches!(
                p.code,
                ErrorCode::InvalidArgument | ErrorCode::ResourceLimit | ErrorCode::Unsupported
            ),
            "{name}: {p:?}"
        );
    }
    assert!(!paths.private_dir_path(&run_id).join("qmp.sock").exists());
    let new_pids: Vec<_> = qemu_pids()
        .into_iter()
        .filter(|p| !before.contains(p))
        .collect();
    assert!(
        new_pids.is_empty(),
        "QEMU started by a rejected request: {new_pids:?}"
    );
    log.line(format!(
        "rejections verified: {}, no QEMU process created",
        checks.len()
    ));
}

/// The NIC detector works on real QEMU: with an explicit NIC (without a
/// network backend, VM paused) `info network` and `query-pci` report it.
/// The process is started directly (outside the API, which cannot produce a
/// NIC) and terminated at the end.
#[test]
#[ignore = "needs real QEMU: set LIGHTVM_QEMU_DIR"]
fn real_nic_detector() {
    let _g = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let lab = require_lab!();
    let mut log = Log::new(&lab, "nic");
    let dir = lab.work.join("p").join("nic");
    std::fs::create_dir_all(&dir).unwrap();
    let sock = ManagedPath::verify(dir.join("qmp.sock")).unwrap();
    let _ = std::fs::remove_file(sock.as_path());

    struct Kill(std::process::Child);
    impl Drop for Kill {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let child = Command::new(lab.qemu_dir.join("qemu-system-x86_64.exe"))
        .args([
            "-no-user-config",
            "-nodefaults",
            "-machine",
            "type=pc-q35-11.1",
            "-accel",
            "whpx",
            "-cpu",
            "qemu64",
            "-m",
            "size=256M",
            "-display",
            "none",
            "-device",
            "virtio-net-pci",
            "-qmp",
        ])
        .arg(format!("unix:{},server=on,wait=off", sock.qemu_value()))
        .arg("-S")
        .env_clear()
        .env("SystemRoot", std::env::var_os("SystemRoot").unwrap())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut child = Kill(child);
    let rt = rt();
    let (info, pci) = rt.block_on(async {
        let t0 = Instant::now();
        let qmp = loop {
            match QmpClient::connect(&sock, QmpConfig::default()).await {
                Ok(q) => break q,
                Err(e) if t0.elapsed() < Duration::from_secs(20) => {
                    let _ = e;
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Err(e) => panic!("QMP: {e}"),
            }
        };
        let info = qmp.network_diagnostics().await.unwrap();
        let pci = qmp.pci_network_devices().await.unwrap();
        let _ = qmp.quit().await;
        (info, pci)
    });
    let code = child.0.wait().ok().and_then(|s| s.code());
    log.line(format!("info network with NIC: {:?}", info.trim()));
    log.line(format!("query-pci network class: {pci:?}; exit {code:?}"));
    let e = check_no_nic(&info, &pci).unwrap_err();
    log.line(format!("NIC check: {e} -> {}", e.to_public().code));
    assert!(!pci.is_empty());
    assert!(!info.trim().is_empty());
    let _ = std::fs::remove_file(sock.as_path());
}

/// F1 exit criterion: at least 20 real start/stop cycles with WHPX, overlay
/// disk created with `qemu-img`, no orphans, no unexpected NIC.
#[test]
#[ignore = "needs real QEMU: set LIGHTVM_QEMU_DIR"]
fn real_start_stop_cycles() {
    let _g = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let lab = require_lab!();
    let cycles: usize = std::env::var("LIGHTVM_F1_CYCLES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20);
    let mut log = Log::new(&lab, "cycles");
    log.line("Shared host: the following times are not release measurements.");
    let paths = lab_paths(&lab, &mut log);
    let caps = probe_caps(&lab, &paths, &mut log);
    let csv_path = lab.logs.join(format!("{}-cycles.csv", lab.session));
    let mut csv = std::fs::File::create(&csv_path).unwrap();
    writeln!(
        csv,
        "cycle,run_id,pid,overlay_ms,start_ms,stop_ms,close_ms,phase,code,class,cause,active_after,job_closed,handle_released,identity_after,nic_ok,reconciled_state,notes"
    )
    .unwrap();
    let rt = rt();
    let mut start_ms = Vec::new();
    let mut stop_ms = Vec::new();
    let mut identities = Vec::new();
    let mut phases: BTreeMap<String, usize> = BTreeMap::new();

    for i in 1..=cycles {
        let run_id = RunId::new_v7();
        let overlay = paths.prepare_run(&run_id);
        if i == 1 {
            // Orphan socket of a previous run: the backend must remove it.
            std::fs::write(paths.private_dir_path(&run_id).join("qmp.sock"), b"orphan").unwrap();
        }
        let s = spec(&caps, &paths, run_id);
        let t0 = Instant::now();
        let run = rt
            .block_on(QemuRun::start(&caps, &s, &paths, RunConfig::default()))
            .unwrap_or_else(|e| panic!("cycle {i}: launch failed: {e}"));
        let t_start = t0.elapsed();
        let id = run.identity();
        identities.push(id);
        assert_eq!(id.run_id, run_id);
        assert_eq!(run.state(), ProcessState::Running);
        assert_eq!(run.accelerator().enabled, "whpx");
        assert_eq!(
            run.active_processes().unwrap(),
            1,
            "a single process in the job"
        );
        let nic_ok = rt.block_on(run.verify_no_nic()).is_ok();
        assert!(nic_ok, "cycle {i}: unexpected NIC");
        let rec = rt.block_on(run.reconcile(Duration::from_secs(5))).unwrap();
        assert_eq!(rec.observed.state, VmState::Running, "{rec:?}");

        if i == 1 {
            // Real capture and input on the first run: repeated screenshots
            // until a non-black image (firmware or boot loader), at most 30 s.
            let t_shot = Instant::now();
            let (shot, img) = loop {
                std::thread::sleep(Duration::from_secs(1));
                let (shot, img) = rt.block_on(run.screenshot_rgb()).unwrap();
                if img.non_black_percent() > 0.5 || t_shot.elapsed() > Duration::from_secs(30) {
                    break (shot, img);
                }
            };
            log.line(format!(
                "screendump after {:?}: {}x{}, PNG {} bytes, sha256 {}, non-black {:.2}%",
                t_shot.elapsed(),
                shot.geometry.width,
                shot.geometry.height,
                shot.png.len(),
                shot.sha256,
                img.non_black_percent()
            ));
            let png_path = lab.logs.join(format!("{}-screendump.png", lab.session));
            std::fs::write(&png_path, &shot.png).unwrap();
            log.line(format!("PNG saved: {}", png_path.display()));
            let leftovers: Vec<_> = std::fs::read_dir(paths.private_dir_path(&run_id))
                .unwrap()
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.ends_with(".ppm"))
                .collect();
            assert!(leftovers.is_empty(), "screendump files left: {leftovers:?}");
            let g = shot.geometry;
            let shift_down = InputEvent::Key(KeyAction {
                qcode: QCode::parse("shift").unwrap(),
                down: true,
            });
            let mv = InputEvent::PointerMove(
                PointerMove::from_pixels(g.width / 2, g.height / 2, g).unwrap(),
            );
            rt.block_on(run.send_input(run_id, g, &[mv, shift_down]))
                .unwrap();
            assert!(run.has_pressed_input());
            let released = rt.block_on(run.release_all_input(run_id)).unwrap();
            log.line(format!(
                "input: move + shift pressed, released {released} keys"
            ));
            assert_eq!(released, 1);
            assert!(!run.has_pressed_input());
            // Stale geometry and superseded run_id rejected.
            let stale = DisplayGeometry::new(g.width + 1, g.height).unwrap();
            assert!(rt.block_on(run.send_input(run_id, stale, &[])).is_err());
            assert!(
                rt.block_on(run.send_input(RunId::new_v7(), g, &[]))
                    .is_err()
            );
            let power = InputEvent::Key(KeyAction {
                qcode: QCode::parse("power").unwrap(),
                down: true,
            });
            assert!(rt.block_on(run.send_input(run_id, g, &[power])).is_err());
            // Screenshot size limit.
            let e = rt.block_on(run.screenshot_limited(16)).unwrap_err();
            assert_eq!(e.code(), ErrorCode::ResourceLimit);
        } else {
            std::thread::sleep(Duration::from_millis(1500));
        }

        // Shutdown: alternating between full escalation (powerdown → quit), quit
        // only, and Emergency Stop in the last cycle.
        let t1 = Instant::now();
        let report = if i == cycles {
            rt.block_on(run.emergency_stop(Duration::from_secs(10)))
                .unwrap()
        } else {
            let policy = if i % 2 == 0 {
                ShutdownPolicy {
                    powerdown_timeout: Duration::from_secs(2),
                    quit_timeout: Duration::from_secs(5),
                    kill_timeout: Duration::from_secs(10),
                }
            } else {
                ShutdownPolicy {
                    powerdown_timeout: Duration::ZERO,
                    quit_timeout: Duration::from_secs(5),
                    kill_timeout: Duration::from_secs(10),
                }
            };
            rt.block_on(run.shutdown(policy)).unwrap()
        };
        let t_stop = t1.elapsed();
        assert_eq!(
            report.active_after, 0,
            "cycle {i}: processes in the job after the exit"
        );
        match report.phase {
            StopPhase::Quit => {
                assert_eq!(report.cause, ExitCause::QmpQuit);
                assert_eq!(report.class, ExitCodeClass::Clean);
            }
            StopPhase::Kill => {
                assert_eq!(report.cause, ExitCause::EmergencyStop);
                assert_eq!(report.class, ExitCodeClass::EmergencyStop);
            }
            StopPhase::Powerdown => assert_eq!(report.cause, ExitCause::GuestShutdown),
            StopPhase::AlreadyExited => panic!("cycle {i}: exit before the shutdown"),
        }
        let rec = rt.block_on(run.reconcile(Duration::from_secs(1))).unwrap();
        assert_eq!(rec.observed.state, VmState::Stopped, "{rec:?}");
        let t2 = Instant::now();
        let closed = rt.block_on(run.close()).unwrap();
        let t_close = t2.elapsed();
        assert!(closed.job_closed, "cycle {i}: Job Object not closed");
        assert!(
            closed.process_handle_released,
            "cycle {i}: process handle retained"
        );
        assert!(!closed.forced);
        assert_eq!(closed.active_at_close, 0);
        assert!(!closed.qmp_socket_left, "cycle {i}: QMP socket left");
        let after = assert_no_orphan(&id);
        *phases.entry(format!("{:?}", report.phase)).or_default() += 1;
        start_ms.push(ms(t_start));
        stop_ms.push(ms(t_stop));
        let line = format!(
            "{i},{run_id},{},{:.1},{:.1},{:.1},{:.1},{:?},{:?},{:?},{:?},{},{},{},{:?},{},{:?},{}",
            id.pid,
            ms(overlay),
            ms(t_start),
            ms(t_stop),
            ms(t_close),
            report.phase,
            report.code,
            report.class,
            report.cause,
            report.active_after,
            closed.job_closed,
            closed.process_handle_released,
            after,
            nic_ok,
            rec.observed.state,
            report.notes.join(" / ").replace(',', ";")
        );
        writeln!(csv, "{line}").unwrap();
        log.line(format!("cycle {line}"));
    }

    // Final check on all identities of the session.
    for id in &identities {
        assert_no_orphan(id);
    }
    let (a0, a1, a2) = stats(&start_ms);
    let (b0, b1, b2) = stats(&stop_ms);
    log.line(format!(
        "SUMMARY {cycles} cycles: start ms min {a0:.1} median {a1:.1} max {a2:.1}; \
         stop ms min {b0:.1} median {b1:.1} max {b2:.1}; phases {phases:?}; \
         no orphans across {} identities; csv {}",
        identities.len(),
        csv_path.display()
    ));
    assert!(cycles >= 20 || std::env::var("LIGHTVM_F1_CYCLES").is_ok());
}

/// F1-17 (D-1, ADR-0011 criterion 2): boot of the live ISO with a nominal CPU
/// model richer than `qemu64` (`LIGHTVM_F1_CPU`, default
/// `Skylake-Client-v4`), 2 vCPUs, 4096 MiB. One screendump every 10 s for at
/// most `LIGHTVM_F1_BOOT_S` seconds (default 240), saved in
/// `lab/logs/f1-03/`. The test only verifies that the process stays alive and that
/// the images are not black; reaching the desktop is judged from the images
/// and recorded in the verification file.
#[test]
#[ignore = "needs real QEMU: set LIGHTVM_QEMU_DIR"]
fn real_live_boot_nominal_cpu() {
    let _g = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let lab = require_lab!();
    let cpu_name = std::env::var("LIGHTVM_F1_CPU").unwrap_or_else(|_| "Skylake-Client-v4".into());
    let boot_s: u64 = std::env::var("LIGHTVM_F1_BOOT_S")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(240);
    let mut log = Log::new(&lab, "boot-cpu");
    let paths = lab_paths(&lab, &mut log);
    let caps = probe_caps(&lab, &paths, &mut log);
    let cpu = caps
        .cpu_model(&cpu_name)
        .unwrap_or_else(|e| panic!("model {cpu_name}: {e}"));
    let run_id = RunId::new_v7();
    paths.prepare_run(&run_id);
    let mut s = spec(&caps, &paths, run_id);
    s.cpu = cpu;
    s.memory_mib = 4096;
    let rt = rt();
    let t0 = Instant::now();
    let run = rt
        .block_on(QemuRun::start(&caps, &s, &paths, RunConfig::default()))
        .unwrap_or_else(|e| panic!("launch with {cpu_name}: {e}"));
    log.line(format!(
        "launch with -cpu {cpu_name}: {:?}, pid {}",
        t0.elapsed(),
        run.identity().pid
    ));
    let mut n = 0u32;
    while t0.elapsed() < Duration::from_secs(boot_s) {
        std::thread::sleep(Duration::from_secs(10));
        assert_eq!(
            run.state(),
            ProcessState::Running,
            "QEMU exited during boot with {cpu_name}"
        );
        let (shot, img) = rt.block_on(run.screenshot_rgb()).unwrap();
        n += 1;
        let png = lab.logs.join(format!(
            "{}-boot-{cpu_name}-{:03}s.png",
            lab.session,
            t0.elapsed().as_secs()
        ));
        std::fs::write(&png, &shot.png).unwrap();
        log.line(format!(
            "t={:>4}s {}x{} non-black {:.2}% sha256 {} -> {}",
            t0.elapsed().as_secs(),
            shot.geometry.width,
            shot.geometry.height,
            img.non_black_percent(),
            shot.sha256,
            png.display()
        ));
    }
    let report = rt
        .block_on(run.emergency_stop(Duration::from_secs(10)))
        .unwrap();
    assert_eq!(report.active_after, 0);
    let closed = rt.block_on(run.close()).unwrap();
    assert!(closed.job_closed);
    log.line(format!("{n} screendumps; shutdown {:?}", report.phase));
}

/// ADR-0017: Job Object limits applied to real QEMU. (1) CPU cap and
/// job memory = RAM + 1024 MiB: read back from the job, VM started, peak
/// recorded. (2) Job memory lower than the guest RAM (512 MiB versus
/// 2048 MiB): QEMU must not keep running beyond the limit.
#[test]
#[ignore = "needs real QEMU: set LIGHTVM_QEMU_DIR"]
fn real_job_cpu_and_memory_limits() {
    let _g = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let lab = require_lab!();
    let mut log = Log::new(&lab, "job-limits");
    let paths = lab_paths(&lab, &mut log);
    let caps = probe_caps(&lab, &paths, &mut log);
    let rt = rt();

    // (1) Limits compatible with the VM.
    let run_id = RunId::new_v7();
    paths.prepare_run(&run_id);
    let mut s = spec(&caps, &paths, run_id);
    s.limits = qemu_backend::JobLimits {
        cpu_rate: Some(300),
        job_memory_bytes: Some((2048 + 1024) << 20),
    };
    let run = rt
        .block_on(QemuRun::start(&caps, &s, &paths, RunConfig::default()))
        .expect("launch with limits");
    std::thread::sleep(Duration::from_secs(5));
    assert_eq!(run.state(), ProcessState::Running);
    let r = run.limit_report().unwrap();
    log.line(format!("applied limits: {r:?}"));
    assert_eq!(r.cpu_rate, Some(300));
    assert_eq!(r.job_memory_bytes, Some((2048 + 1024) << 20));
    assert!(r.peak_job_memory_bytes > 0);
    assert!(!r.memory_limit_reached());
    let id = run.identity();
    let report = rt
        .block_on(run.emergency_stop(Duration::from_secs(10)))
        .unwrap();
    assert_eq!(report.active_after, 0);
    let _ = rt.block_on(run.close()).unwrap();
    assert_no_orphan(&id);

    // (2) Job memory below the guest RAM.
    let run_id = RunId::new_v7();
    paths.prepare_run(&run_id);
    let mut s = spec(&caps, &paths, run_id);
    s.limits = qemu_backend::JobLimits {
        cpu_rate: None,
        job_memory_bytes: Some(512 << 20),
    };
    let t0 = Instant::now();
    match rt.block_on(QemuRun::start(&caps, &s, &paths, RunConfig::default())) {
        Err(e) => log.line(format!(
            "job memory 512 MiB: launch refused after {:?}: {e}",
            t0.elapsed()
        )),
        Ok(run) => {
            let id = run.identity();
            let mut exited = None;
            while t0.elapsed() < Duration::from_secs(60) {
                if let ProcessState::Exited { .. } = run.state() {
                    exited = Some(t0.elapsed());
                    break;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            let r = run.limit_report().unwrap();
            log.line(format!(
                "job memory 512 MiB: state {:?}, exit after {exited:?}, report {r:?}, limit reached {}",
                run.state(),
                r.memory_limit_reached()
            ));
            if exited.is_none() {
                let _ = rt.block_on(run.emergency_stop(Duration::from_secs(10)));
                let _ = rt.block_on(run.close());
                assert_no_orphan(&id);
                panic!("QEMU still running beyond the job memory limit");
            }
            assert!(r.peak_job_memory_bytes <= 512 << 20);
            let _ = rt.block_on(run.close());
            assert_no_orphan(&id);
        }
    }
}

/// Qcode (US layout) and Shift for a character of the command typed in the guest.
fn qcode_us(c: char) -> (&'static str, bool) {
    const LETTERS: [&str; 26] = [
        "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p", "q", "r",
        "s", "t", "u", "v", "w", "x", "y", "z",
    ];
    const DIGITS: [&str; 10] = ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"];
    match c {
        'a'..='z' => (LETTERS[(c as u8 - b'a') as usize], false),
        '0'..='9' => (DIGITS[(c as u8 - b'0') as usize], false),
        ' ' => ("spc", false),
        '-' => ("minus", false),
        '/' => ("slash", false),
        '.' => ("dot", false),
        '&' => ("7", true),
        '\n' => ("ret", false),
        _ => panic!("unexpected character: {c:?}"),
    }
}

fn key(qcode: &str, down: bool) -> InputEvent {
    InputEvent::Key(KeyAction {
        qcode: QCode::parse(qcode).unwrap(),
        down,
    })
}

/// P-5, Linux part (docs/STATUS.md): in the live Ubuntu guest, with the data ISO
/// `lab/media/p5-dati.iso` (script `crates/lightvm-service/scripts/
/// p5-guest-test.sh` and `lightvm-peer-check` for Linux) on the second CD-ROM,
/// opens a terminal and runs the test; saves the screenshots in
/// `lab/logs/f1-03/` for reading the outcome. `LIGHTVM_P5_ISO` overrides
/// the path of the data ISO.
#[test]
#[ignore = "needs real QEMU: set LIGHTVM_QEMU_DIR"]
fn real_p5_second_user_linux_in_vm() {
    let _g = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let lab = require_lab!();
    let data_iso = std::env::var_os("LIGHTVM_P5_ISO")
        .map(PathBuf::from)
        .unwrap_or_else(|| lab.iso.with_file_name("p5-dati.iso"));
    assert!(
        data_iso.is_file(),
        "data ISO missing: {}",
        data_iso.display()
    );
    let mut log = Log::new(&lab, "p5");
    let mut paths = lab_paths(&lab, &mut log);
    let data_id = MediaId::new_v7();
    paths.data = Some((data_id, data_iso));
    let caps = probe_caps(&lab, &paths, &mut log);
    let run_id = RunId::new_v7();
    paths.prepare_run(&run_id);
    let mut s = spec(&caps, &paths, run_id);
    s.cpu = caps.cpu_model("Skylake-Client-v4").unwrap();
    s.memory_mib = 4096;
    s.data_media = Some(data_id);
    let rt = rt();
    let run = rt
        .block_on(QemuRun::start(&caps, &s, &paths, RunConfig::default()))
        .expect("launch");
    let t0 = Instant::now();
    let save = |name: &str, log: &mut Log| {
        let (shot, img) = rt.block_on(run.screenshot_rgb()).unwrap();
        let png = lab.logs.join(format!("{}-p5-{name}.png", lab.session));
        std::fs::write(&png, &shot.png).unwrap();
        log.line(format!(
            "t={:>4}s {name}: non-black {:.2}% -> {}",
            t0.elapsed().as_secs(),
            img.non_black_percent(),
            png.display()
        ));
        shot.geometry
    };
    // Live desktop: about 100 s with Skylake-Client-v4 (F1-03).
    std::thread::sleep(Duration::from_secs(130));
    let g = save("desktop", &mut log);
    // Terminal: Ctrl+Alt+T.
    let combo = [
        key("ctrl", true),
        key("alt", true),
        key("t", true),
        key("t", false),
        key("alt", false),
        key("ctrl", false),
    ];
    rt.block_on(run.send_input(run_id, g, &combo)).unwrap();
    std::thread::sleep(Duration::from_secs(8));
    save("terminal", &mut log);
    let command = "sudo mount -o ro /dev/sr1 /mnt && bash /mnt/prova.sh\n";
    for c in command.chars() {
        let (q, shift) = qcode_us(c);
        let mut ev = Vec::new();
        if shift {
            ev.push(key("shift", true));
        }
        ev.push(key(q, true));
        ev.push(key(q, false));
        if shift {
            ev.push(key("shift", false));
        }
        rt.block_on(run.send_input(run_id, g, &ev)).unwrap();
        std::thread::sleep(Duration::from_millis(40));
    }
    std::thread::sleep(Duration::from_secs(20));
    save("outcome", &mut log);
    let id = run.identity();
    let _ = rt.block_on(run.emergency_stop(Duration::from_secs(10)));
    let _ = rt.block_on(run.close());
    assert_no_orphan(&id);
}

/// Paths with disk and UEFI variables persistent across runs (F1-17):
/// same lab paths for everything else.
struct FixedDisk<'a> {
    base: &'a LabPaths,
    disk: PathBuf,
    vars: PathBuf,
}

impl ManagedPaths for FixedDisk<'_> {
    fn qemu_system(&self) -> qemu_backend::error::Result<ManagedPath> {
        self.base.qemu_system()
    }
    fn firmware_code(&self) -> qemu_backend::error::Result<ManagedPath> {
        self.base.firmware_code()
    }
    fn firmware_vars(&self, _run: &RunId) -> qemu_backend::error::Result<Option<ManagedPath>> {
        ManagedPath::verify(&self.vars).map(Some)
    }
    fn disk(&self, _run: &RunId) -> qemu_backend::error::Result<Option<ManagedPath>> {
        ManagedPath::verify(&self.disk).map(Some)
    }
    fn protected_readonly(&self, _run: &RunId) -> qemu_backend::error::Result<Vec<ManagedPath>> {
        Ok(Vec::new())
    }
    fn media(&self, media: &MediaId) -> qemu_backend::error::Result<ManagedPath> {
        self.base.media(media)
    }
    fn run_dir(&self, run: &RunId) -> qemu_backend::error::Result<ManagedPath> {
        self.base.run_dir(run)
    }
    fn private_dir(&self, run: &RunId) -> qemu_backend::error::Result<ManagedPath> {
        self.base.private_dir(run)
    }
}

/// F1-17 disk (32 GiB) and UEFI variables in `lab/vms/f1-17/`, created if missing.
fn f1_17_disk(lab: &Lab, paths: &LabPaths) -> (PathBuf, PathBuf) {
    let dir = lab.work.parent().unwrap().parent().unwrap().join("f1-17");
    std::fs::create_dir_all(&dir).unwrap();
    let disk = dir.join("disco.qcow2");
    let vars = dir.join("vars.fd");
    if !disk.exists() {
        prepare::create_disk(
            &ManagedPath::verify(paths.qemu_img()).unwrap(),
            &ManagedPath::verify(&disk).unwrap(),
            DiskFormat::Qcow2,
            32 * 1024,
            prepare::QEMU_IMG_TIMEOUT,
        )
        .unwrap();
    }
    if !vars.exists() {
        std::fs::copy(
            paths.qemu_dir.join("share").join("edk2-i386-vars.fd"),
            &vars,
        )
        .unwrap();
    }
    (disk, vars)
}

/// F1-17, part 1: automatic installation of Ubuntu on disk (autoinstall from
/// a `CIDATA` volume, `lab/media/f1-17-cidata.iso`, on the second CD-ROM). The
/// guest powers off by itself at the end of the installation (`shutdown: poweroff`).
/// Screenshot every 2 minutes; maximum wait `LIGHTVM_F1_17_MIN` minutes (45).
#[test]
#[ignore = "needs real QEMU: set LIGHTVM_QEMU_DIR"]
fn real_f1_17_install_to_disk() {
    let _g = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let lab = require_lab!();
    let minutes: u64 = std::env::var("LIGHTVM_F1_17_MIN")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(45);
    let mut log = Log::new(&lab, "f1-17-install");
    let mut paths = lab_paths(&lab, &mut log);
    let cidata = lab.iso.with_file_name("f1-17-cidata.iso");
    assert!(
        cidata.is_file(),
        "CIDATA volume missing: {}",
        cidata.display()
    );
    let cid = MediaId::new_v7();
    paths.data = Some((cid, cidata));
    let caps = probe_caps(&lab, &paths, &mut log);
    let (disk, vars) = f1_17_disk(&lab, &paths);
    let fixed = FixedDisk {
        base: &paths,
        disk,
        vars,
    };
    let run_id = RunId::new_v7();
    paths.prepare_run(&run_id);
    let mut s = spec(&caps, &paths, run_id);
    s.cpu = caps.cpu_model("Skylake-Client-v4").unwrap();
    s.vcpus = 4;
    s.memory_mib = 6144;
    s.data_media = Some(cid);
    let rt = rt();
    let run = rt
        .block_on(QemuRun::start(&caps, &s, &fixed, RunConfig::default()))
        .expect("installation launch");
    let t0 = Instant::now();
    let mut n = 0u32;
    let mut clicked = false;
    let outcome = loop {
        if let ProcessState::Exited { cause, code, .. } = run.state() {
            break Some((cause, code));
        }
        // The desktop installer shows the autoinstall summary and
        // waits for "Install" (bottom right) about 2 minutes after startup.
        if !clicked && t0.elapsed() >= Duration::from_secs(140) {
            clicked = true;
            let (shot, _) = rt.block_on(run.screenshot_rgb()).unwrap();
            let g = shot.geometry;
            let png = lab
                .logs
                .join(format!("{}-f1-17-inst-summary.png", lab.session));
            std::fs::write(&png, &shot.png).unwrap();
            let x = g.width * 1035 / 1280;
            let y = g.height * 716 / 800;
            let click = [
                InputEvent::PointerMove(PointerMove::from_pixels(x, y, g).unwrap()),
                InputEvent::PointerButton(lightvm_proto::PointerButton {
                    button: lightvm_proto::MouseButton::Left,
                    down: true,
                }),
                InputEvent::PointerButton(lightvm_proto::PointerButton {
                    button: lightvm_proto::MouseButton::Left,
                    down: false,
                }),
            ];
            rt.block_on(run.send_input(run_id, g, &click)).unwrap();
            log.line(format!(
                "click on Install ({x},{y}) at t={}s",
                t0.elapsed().as_secs()
            ));
        }
        if t0.elapsed() > Duration::from_secs(minutes * 60) {
            break None;
        }
        std::thread::sleep(Duration::from_secs(10));
        if t0.elapsed().as_secs() / 120 >= u64::from(n) {
            n += 1;
            if let Ok((shot, img)) = rt.block_on(run.screenshot_rgb()) {
                let png = lab.logs.join(format!(
                    "{}-f1-17-inst-{:03}min.png",
                    lab.session,
                    t0.elapsed().as_secs() / 60
                ));
                std::fs::write(&png, &shot.png).unwrap();
                log.line(format!(
                    "t={:>5}s non-black {:.2}% -> {}",
                    t0.elapsed().as_secs(),
                    img.non_black_percent(),
                    png.display()
                ));
            }
        }
    };
    log.line(format!("outcome after {:?}: {outcome:?}", t0.elapsed()));
    let id = run.identity();
    if outcome.is_none() {
        let _ = rt.block_on(run.emergency_stop(Duration::from_secs(10)));
    }
    let _ = rt.block_on(run.close());
    assert_no_orphan(&id);
    assert!(
        outcome.is_some(),
        "installation not completed in {minutes} minutes"
    );
}

/// F1-17, part 2: boot of Ubuntu from the installed disk (no ISO), then
/// ordinary ACPI shutdown (`system_powerdown`): a real guest must power off
/// by itself, without `quit` or termination.
#[test]
#[ignore = "needs real QEMU: set LIGHTVM_QEMU_DIR"]
fn real_f1_17_boot_from_disk_and_acpi_shutdown() {
    let _g = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let lab = require_lab!();
    let mut log = Log::new(&lab, "f1-17-boot");
    let paths = lab_paths(&lab, &mut log);
    let caps = probe_caps(&lab, &paths, &mut log);
    let (disk, vars) = f1_17_disk(&lab, &paths);
    let fixed = FixedDisk {
        base: &paths,
        disk,
        vars,
    };
    let run_id = RunId::new_v7();
    paths.prepare_run(&run_id);
    let mut s = spec(&caps, &paths, run_id);
    s.cpu = caps.cpu_model("Skylake-Client-v4").unwrap();
    s.memory_mib = 4096;
    s.install_media = None;
    let rt = rt();
    let run = rt
        .block_on(QemuRun::start(&caps, &s, &fixed, RunConfig::default()))
        .expect("launch from disk");
    let t0 = Instant::now();
    for i in 1..=8 {
        std::thread::sleep(Duration::from_secs(15));
        let (shot, img) = rt.block_on(run.screenshot_rgb()).unwrap();
        let png = lab.logs.join(format!(
            "{}-f1-17-boot-{:03}s.png",
            lab.session,
            t0.elapsed().as_secs()
        ));
        std::fs::write(&png, &shot.png).unwrap();
        log.line(format!(
            "boot {i}: t={}s non-black {:.2}% -> {}",
            t0.elapsed().as_secs(),
            img.non_black_percent(),
            png.display()
        ));
    }
    let t1 = Instant::now();
    let report = rt
        .block_on(run.shutdown(ShutdownPolicy {
            powerdown_timeout: Duration::from_secs(90),
            quit_timeout: Duration::from_secs(10),
            kill_timeout: Duration::from_secs(10),
        }))
        .unwrap();
    log.line(format!(
        "ACPI shutdown: phase {:?}, cause {:?}, code {:?}, duration {:?}",
        report.phase,
        report.cause,
        report.code,
        t1.elapsed()
    ));
    let id = run.identity();
    let _ = rt.block_on(run.close());
    assert_no_orphan(&id);
    assert_eq!(
        report.phase,
        StopPhase::Powerdown,
        "the guest did not power off with ACPI"
    );
}

/// Total CPU time (seconds) of a process, read from the operating system.
fn cpu_seconds(pid: u32) -> f64 {
    let out = Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &format!("(Get-Process -Id {pid}).TotalProcessorTime.TotalSeconds"),
        ])
        .stdin(Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().replace(',', "."))
        .unwrap_or_default();
    out.parse().unwrap_or(f64::NAN)
}

/// ADR-0017 / F1-10: effect of the Job Object CPU cap on a real guest.
/// Boot of Ubuntu from the F1-17 disk (2 vCPUs) for 60 s without a cap and with the
/// minimum cap (5 % of the vCPUs = 0.1 core): the boot uses on average about 0.2
/// cores (first run: a 25 % cap was never reached). The
/// QEMU CPU time with the cap must stay close to the granted share and
/// well below the one without a cap.
#[test]
#[ignore = "needs real QEMU and the F1-17 disk: set LIGHTVM_QEMU_DIR"]
fn real_cpu_cap_effect_during_boot() {
    let _g = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let lab = require_lab!();
    let mut log = Log::new(&lab, "cpu-cap");
    let paths = lab_paths(&lab, &mut log);
    let caps = probe_caps(&lab, &paths, &mut log);
    let (disk, vars) = f1_17_disk(&lab, &paths);
    let fixed = FixedDisk {
        base: &paths,
        disk,
        vars,
    };
    let logical = u32::try_from(
        std::thread::available_parallelism()
            .map(std::num::NonZero::get)
            .unwrap_or(1),
    )
    .unwrap();
    let rt = rt();
    let mut measure = |cpu_rate: Option<u32>| -> f64 {
        let run_id = RunId::new_v7();
        paths.prepare_run(&run_id);
        let mut s = spec(&caps, &paths, run_id);
        s.cpu = caps.cpu_model("Skylake-Client-v4").unwrap();
        s.vcpus = 2;
        s.memory_mib = 4096;
        s.install_media = None;
        s.limits = qemu_backend::JobLimits {
            cpu_rate,
            job_memory_bytes: None,
        };
        let run = rt
            .block_on(QemuRun::start(&caps, &s, &fixed, RunConfig::default()))
            .expect("launch from disk");
        let pid = run.identity().pid;
        let c0 = cpu_seconds(pid);
        std::thread::sleep(Duration::from_secs(60));
        let c1 = cpu_seconds(pid);
        let id = run.identity();
        let _ = rt.block_on(run.emergency_stop(Duration::from_secs(10)));
        let _ = rt.block_on(run.close());
        assert_no_orphan(&id);
        let used = c1 - c0;
        log.line(format!(
            "cap {cpu_rate:?}: {used:.1} s of CPU in 60 s ({:.2} average cores)",
            used / 60.0
        ));
        used
    };
    let uncapped = measure(None);
    // 5 % of 2 vCPUs = 0.1 core: machine share in hundredths of a point.
    let rate = (2 * 5 * 100 / logical).max(1);
    let capped = measure(Some(rate));
    let share_cores = f64::from(rate) / 10_000.0 * f64::from(logical);
    log.line(format!(
        "granted share {share_cores:.2} cores; uncapped {uncapped:.1} s, capped {capped:.1} s"
    ));
    assert!(
        capped <= share_cores * 60.0 * 1.25,
        "capped CPU time beyond the share: {capped:.1} s"
    );
    assert!(
        uncapped > capped * 1.5,
        "the cap does not reduce consumption: without {uncapped:.1} s, with {capped:.1} s"
    );
}

/// Paths of a Windows hardware profile run: empty disk created for the
/// run, UEFI variables only for the UEFI profile, SeaBIOS image of the lab
/// distribution.
struct ProfilePaths<'a> {
    base: &'a LabPaths,
    disk: PathBuf,
    vars: Option<PathBuf>,
}

impl ManagedPaths for ProfilePaths<'_> {
    fn qemu_system(&self) -> qemu_backend::error::Result<ManagedPath> {
        self.base.qemu_system()
    }
    fn firmware_code(&self) -> qemu_backend::error::Result<ManagedPath> {
        self.base.firmware_code()
    }
    fn firmware_vars(&self, _run: &RunId) -> qemu_backend::error::Result<Option<ManagedPath>> {
        self.vars.as_ref().map(ManagedPath::verify).transpose()
    }
    fn seabios(&self) -> qemu_backend::error::Result<ManagedPath> {
        ManagedPath::verify(self.base.qemu_dir.join("share").join("bios-256k.bin"))
    }
    fn disk(&self, _run: &RunId) -> qemu_backend::error::Result<Option<ManagedPath>> {
        ManagedPath::verify(&self.disk).map(Some)
    }
    fn protected_readonly(&self, _run: &RunId) -> qemu_backend::error::Result<Vec<ManagedPath>> {
        Ok(Vec::new())
    }
    fn media(&self, media: &MediaId) -> qemu_backend::error::Result<ManagedPath> {
        self.base.media(media)
    }
    fn run_dir(&self, run: &RunId) -> qemu_backend::error::Result<ManagedPath> {
        self.base.run_dir(run)
    }
    fn private_dir(&self, run: &RunId) -> qemu_backend::error::Result<ManagedPath> {
        self.base.private_dir(run)
    }
}

/// ADR-0027: the Windows hardware profiles (SeaBIOS on q35 with xHCI and
/// UHCI, SeaBIOS on i440fx with PIIX IDE, UEFI on q35 with AHCI) start
/// under WHPX with an empty disk and the lab ISO on the install CD: the
/// accelerator probe of each machine family / firmware combination
/// passes, the VM reaches `running`, the no-NIC check passes, the display
/// is not black after a few seconds, and the process leaves no orphan.
#[test]
#[ignore = "needs real QEMU: set LIGHTVM_QEMU_DIR"]
fn real_windows_hardware_profiles() {
    let _g = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let lab = require_lab!();
    let mut log = Log::new(&lab, "profiles");
    let paths = lab_paths(&lab, &mut log);
    let mut caps = probe_caps(&lab, &paths, &mut log);
    log.line(format!(
        "SeaBIOS: {:?}; i440fx machines: {:?}",
        caps.seabios,
        caps.recommended_machine(MachineFamily::I440fx)
            .map(|m| m.as_str().to_owned())
    ));
    assert!(caps.verified_seabios().is_ok(), "{:?}", caps.seabios);
    let probe_dir = ManagedPath::verify(lab.work.join("p").join("probe")).unwrap();
    let cpu = caps.cpu_model("Skylake-Client-v4").unwrap();
    let rt = rt();
    for (name, profile) in [
        ("win10-bios-q35-xhci", DeviceProfile::WINDOWS_BIOS),
        ("win7-bios-q35-uhci", DeviceProfile::WINDOWS_VISTA_7),
        ("xp-bios-i440fx", DeviceProfile::WINDOWS_XP),
        ("win10-uefi-q35-ahci", DeviceProfile::WINDOWS_UEFI),
    ] {
        let machine = caps
            .recommended_machine(profile.machine_family())
            .unwrap()
            .clone();
        let t0 = Instant::now();
        let status = rt.block_on(probe::probe_accelerator_for(
            &mut caps,
            Accelerator::Whpx,
            &profile,
            &machine,
            &cpu,
            &probe_dir,
            &ProbeConfig::default(),
        ));
        log.line(format!(
            "{name}: probe {:?} ({:?}) -> {status} in {:?}",
            profile.probe_target(),
            machine.as_str(),
            t0.elapsed()
        ));
        assert!(status.is_available(), "{name}: {status}");
        assert!(caps.accelerator_for(profile.probe_target()).is_available());

        let run_id = RunId::new_v7();
        let mp = |p: PathBuf| ManagedPath::verify(p).unwrap();
        let rd = paths.run_dir_path(&run_id);
        prepare::prepare_run_dirs(&mp(rd.clone()), &mp(paths.private_dir_path(&run_id)))
            .expect("run directories");
        let disk = rd.join("disk.qcow2");
        prepare::create_disk(
            &mp(paths.qemu_img()),
            &mp(disk.clone()),
            DiskFormat::Qcow2,
            1024,
            prepare::QEMU_IMG_TIMEOUT,
        )
        .expect("qemu-img create");
        let vars = profile.firmware().has_vars().then(|| {
            let v = rd.join("vars.fd");
            prepare::copy_firmware_vars(
                &mp(paths.qemu_dir.join("share").join("edk2-i386-vars.fd")),
                &mp(v.clone()),
            )
            .unwrap();
            v
        });
        let pp = ProfilePaths {
            base: &paths,
            disk,
            vars,
        };
        let s = VmLaunchSpec {
            run_id,
            machine: machine.clone(),
            profile,
            accelerator: Accelerator::Whpx,
            cpu: cpu.clone(),
            vcpus: 2,
            cores_per_socket: None,
            memory_mib: 2048,
            disk: Some(DiskSpec {
                format: DiskFormat::Qcow2,
            }),
            install_media: Some(paths.iso_id),
            data_media: None,
            limits: qemu_backend::JobLimits::default(),
            network: qemu_backend::GuestNetwork::None,
        };
        let t0 = Instant::now();
        let run = rt
            .block_on(QemuRun::start(&caps, &s, &pp, RunConfig::default()))
            .unwrap_or_else(|e| panic!("{name}: start failed: {e}"));
        log.line(format!(
            "{name}: started in {:?}, args {:?}",
            t0.elapsed(),
            run.command().args
        ));
        assert_eq!(run.state(), ProcessState::Running, "{name}");
        assert_eq!(run.accelerator().enabled, "whpx");
        let t_run = Instant::now();
        let mut non_black = 0.0;
        while t_run.elapsed() < Duration::from_secs(8) {
            std::thread::sleep(Duration::from_secs(1));
            let (_, img) = rt.block_on(run.screenshot_rgb()).unwrap();
            non_black = img.non_black_percent();
            assert_eq!(run.state(), ProcessState::Running, "{name}: exited early");
        }
        rt.block_on(run.verify_no_nic())
            .unwrap_or_else(|e| panic!("{name}: NIC check: {e}"));
        let rec = rt.block_on(run.reconcile(Duration::from_secs(5))).unwrap();
        log.line(format!(
            "{name}: after {:?} state {:?}, non-black {non_black:.2}%, no NIC",
            t_run.elapsed(),
            rec.observed.state
        ));
        assert_eq!(rec.observed.state, VmState::Running, "{name}: {rec:?}");
        assert!(non_black > 0.0, "{name}: display still black");
        let id = run.identity();
        let _ = rt.block_on(run.emergency_stop(Duration::from_secs(10)));
        let _ = rt.block_on(run.close());
        assert_no_orphan(&id);
    }
    // Probe cache: one entry per non-reference combination.
    log.line(format!(
        "accelerator cache: reference {}; others {:?}",
        caps.accelerator,
        caps.profile_accelerators.keys().collect::<Vec<_>>()
    ));
    assert_eq!(caps.profile_accelerators.len(), 2);
}
