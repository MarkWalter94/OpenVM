//! ADR-0032 required evidence (R6-03): user snapshots on real QCOW2 disks
//! with the product's own disk operations, the data written and read back
//! with `qemu-io`:
//!
//! * VM off: `qemu-img snapshot -c/-a/-d` through
//!   [`qemu_backend::image::internal_snapshot`], the exact command the
//!   service runs on a worker when the VM is stopped;
//! * VM running: QMP `blockdev-snapshot-internal-sync` and
//!   `blockdev-snapshot-delete-internal-sync` through
//!   [`qemu_backend::QemuRun::snapshot_disk`] and
//!   [`qemu_backend::QemuRun::delete_disk_snapshot`] on a real WHPX run
//!   (the disk is crash-consistent, like a power cut at that instant; the
//!   revert itself needs the VM off, so it is done after the stop, as the
//!   product enforces).
//!
//! Every test proves that an apply (`-a`) restores the data the disk held
//! when the snapshot was taken, whatever was written after it.
//!
//! Ignored by default: they need the local lab and are enabled with the
//! `LIGHTVM_QEMU_DIR` environment variable (folder of portable QEMU 11.1.0
//! with `share/edk2-*` and `qemu-io.exe`).
//!
//! ```text
//! LIGHTVM_QEMU_DIR=E:/OpenVM/lab/qemu/qemu-11.1.0 \
//!   cargo test -p qemu-backend --test real_snapshots -- --ignored --nocapture --test-threads=1
//! ```
//!
//! Optional variable: `LIGHTVM_LAB_DIR` (default `<workspace>/lab`).

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use lightvm_proto::{RunId, UtcTimestamp};
use qemu_backend::cmdline::{
    Accelerator, CpuModel, DeviceProfile, DiskFormat, DiskSpec, FirmwareKind, MachineFamily,
    VmLaunchSpec,
};
use qemu_backend::image::{self, ImageCommand, SnapshotAction};
use qemu_backend::probe::{self, ProbeConfig};
use qemu_backend::run::ProcessState;
use qemu_backend::{ManagedPath, ManagedPaths, QemuRun, RunConfig, prepare};

/// Real tests must not overlap (shared host).
static SERIAL: Mutex<()> = Mutex::new(());

struct Lab {
    qemu_dir: PathBuf,
    /// Working root of the session (`lab/vms/r6-03/<session>`).
    work: PathBuf,
    /// Log directory (`lab/logs/r6-03`).
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
    let session = format!("s{}", UtcTimestamp::now().as_unix_millis());
    let work = lab_dir.join("vms").join("r6-03").join(&session);
    let logs = lab_dir.join("logs").join("r6-03");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::create_dir_all(&logs).unwrap();
    Some(Lab {
        qemu_dir,
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

/// `qemu-io -f qcow2 -c "<words…>" <disk>`: runs the command and returns
/// its output. The command is one `-c` string, as the lab fixtures use it.
fn qemu_io(lab: &Lab, disk: &Path, command: &[&str]) -> String {
    let joined = command.join(" ");
    let o = Command::new(lab.qemu_dir.join("qemu-io.exe"))
        .args(["-f", "qcow2", "-c", &joined])
        .arg(disk)
        .stdin(Stdio::null())
        .output()
        .expect("qemu-io");
    assert!(
        o.status.success(),
        "qemu-io {joined:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// Fills the first 8 bytes of the disk with `state` (a readable marker, one
/// hex value per written state: `0x41` = STATE-A, `0x42` = STATE-B, …).
fn write_state(lab: &Lab, disk: &Path, state: &str) {
    qemu_io(lab, disk, &["write", "-P", state, "0", "8"]);
}

/// The first 8 bytes of the disk must hold `state`, else the test fails:
/// `qemu-io read -P` verifies the pattern and reports a mismatch on stderr.
fn assert_state(lab: &Lab, disk: &Path, state: &str) {
    qemu_io(lab, disk, &["read", "-P", state, "0", "8"]);
}

fn not_cancelled() -> impl Fn() -> bool {
    || false
}

/// A 32 MiB scratch disk in `dir`, with the verified `qemu-img` and the
/// private directory of the image command outputs.
fn scratch_disk(lab: &Lab, dir: &Path) -> (ManagedPath, ManagedPath, ManagedPath, PathBuf) {
    let log_dir = dir.join("img-logs");
    std::fs::create_dir_all(&log_dir).unwrap();
    let disk = dir.join("disk.qcow2");
    let qemu_img = ManagedPath::verify(lab.qemu_dir.join("qemu-img.exe")).unwrap();
    let log_dir_mp = ManagedPath::verify(&log_dir).unwrap();
    let disk_mp = ManagedPath::verify(&disk).unwrap();
    prepare::create_disk(
        &qemu_img,
        &disk_mp,
        DiskFormat::Qcow2,
        32,
        prepare::QEMU_IMG_TIMEOUT,
    )
    .expect("qemu-img create scratch disk");
    (qemu_img, log_dir_mp, disk_mp, disk)
}

/// The `qemu-img` command on the scratch disk of the test.
fn image_cmd<'a>(qemu_img: &'a ManagedPath, log_dir: &'a ManagedPath) -> ImageCommand<'a> {
    ImageCommand {
        qemu_img,
        log_dir,
        timeout: image::SHORT_IMAGE_TIMEOUT,
    }
}

/// VM off, the product's exact path: `qemu-img snapshot -c/-a/-d` on the
/// disk through [`qemu_backend::image::internal_snapshot`]. The apply
/// restores the data of the moment of the take, whatever was written after.
#[test]
#[ignore = "needs real QEMU: set LIGHTVM_QEMU_DIR"]
fn real_snapshot_off_qemu_img() {
    let _g = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let lab = require_lab!();
    let mut log = Log::new(&lab, "snapshot-off");
    let dir = lab.work.join("off");
    std::fs::create_dir_all(&dir).unwrap();
    let (qemu_img, log_dir_mp, disk_mp, disk) = scratch_disk(&lab, &dir);
    let cmd = image_cmd(&qemu_img, &log_dir_mp);

    // STATE-A, then the first snapshot.
    write_state(&lab, &disk, "0x41");
    let t0 = Instant::now();
    image::internal_snapshot(
        &cmd,
        SnapshotAction::Create,
        &disk_mp,
        "0001",
        &not_cancelled(),
    )
    .expect("snapshot -c 0001");
    log.line(format!("snapshot -c 0001 in {:?}", t0.elapsed()));
    // STATE-B: written after the take, must be discarded by the apply.
    write_state(&lab, &disk, "0x42");
    image::internal_snapshot(
        &cmd,
        SnapshotAction::Apply,
        &disk_mp,
        "0001",
        &not_cancelled(),
    )
    .expect("snapshot -a 0001");
    log.line("after -a 0001 (STATE-B written after): the first 8 bytes must be STATE-A");
    assert_state(&lab, &disk, "0x41");

    // A second snapshot of the current state, then STATE-C, then the
    // deletion of the first: the apply of the second still restores
    // STATE-A (the deleted one is gone, the other keeps its state).
    image::internal_snapshot(
        &cmd,
        SnapshotAction::Create,
        &disk_mp,
        "0002",
        &not_cancelled(),
    )
    .expect("snapshot -c 0002");
    write_state(&lab, &disk, "0x43");
    image::internal_snapshot(
        &cmd,
        SnapshotAction::Delete,
        &disk_mp,
        "0001",
        &not_cancelled(),
    )
    .expect("snapshot -d 0001");
    image::internal_snapshot(
        &cmd,
        SnapshotAction::Apply,
        &disk_mp,
        "0002",
        &not_cancelled(),
    )
    .expect("snapshot -a 0002");
    log.line(
        "after -d 0001 and -a 0002 (STATE-C written after): the first 8 bytes must be STATE-A",
    );
    assert_state(&lab, &disk, "0x41");

    // Deleting a tag that is not there is a failure, not a change.
    let e = image::internal_snapshot(
        &cmd,
        SnapshotAction::Delete,
        &disk_mp,
        "0003",
        &not_cancelled(),
    );
    assert!(e.is_err(), "deleting a missing tag must fail");
    log.line(format!("deleting a missing tag: {e:?}"));
    log.line("nothing changed: the first 8 bytes are still STATE-A");
    assert_state(&lab, &disk, "0x41");
}

/// Lab paths for the QMP run: a fixed scratch disk and a copy of the UEFI
/// variables, everything else from the lab.
struct FixedDisk {
    qemu_dir: PathBuf,
    disk: PathBuf,
    vars: PathBuf,
    run: PathBuf,
    p: PathBuf,
}

impl ManagedPaths for FixedDisk {
    fn qemu_system(&self) -> qemu_backend::error::Result<ManagedPath> {
        ManagedPath::verify(self.qemu_dir.join("qemu-system-x86_64.exe"))
    }
    fn firmware_code(&self) -> qemu_backend::error::Result<ManagedPath> {
        ManagedPath::verify(self.qemu_dir.join("share").join("edk2-x86_64-code.fd"))
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
    fn media(&self, _media: &lightvm_proto::MediaId) -> qemu_backend::error::Result<ManagedPath> {
        Err(qemu_backend::BackendError::invalid(
            "media",
            "no media in this test",
        ))
    }
    fn run_dir(&self, _run: &RunId) -> qemu_backend::error::Result<ManagedPath> {
        ManagedPath::verify(&self.run)
    }
    fn private_dir(&self, _run: &RunId) -> qemu_backend::error::Result<ManagedPath> {
        ManagedPath::verify(&self.p)
    }
}

/// VM running: the take and the delete go through QMP on a real WHPX run
/// (the disk is open by QEMU, `qemu-img` could not touch it). The revert
/// needs the VM off, so after the stop the apply is done with `qemu-img`
/// and restores the data the disk held while the guest ran.
#[test]
#[ignore = "needs real QEMU: set LIGHTVM_QEMU_DIR"]
fn real_snapshot_running_qmp() {
    let _g = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let lab = require_lab!();
    let mut log = Log::new(&lab, "snapshot-qmp");
    let dir = lab.work.join("qmp");
    std::fs::create_dir_all(&dir).unwrap();
    let (qemu_img, log_dir_mp, disk_mp, disk) = scratch_disk(&lab, &dir);
    let cmd = image_cmd(&qemu_img, &log_dir_mp);

    // STATE-X on the disk before the VM starts.
    write_state(&lab, &disk, "0x58");
    let vars = dir.join("vars.fd");
    std::fs::copy(lab.qemu_dir.join("share").join("edk2-i386-vars.fd"), &vars).unwrap();
    let run = dir.join("run");
    let p = dir.join("p");
    std::fs::create_dir_all(&run).unwrap();
    std::fs::create_dir_all(&p).unwrap();
    let paths = FixedDisk {
        qemu_dir: lab.qemu_dir.clone(),
        disk: disk.clone(),
        vars,
        run,
        p,
    };

    let qemu_system = ManagedPath::verify(lab.qemu_dir.join("qemu-system-x86_64.exe")).unwrap();
    let mut caps =
        probe::probe_host(&qemu_system, &qemu_img, &ProbeConfig::default()).expect("probe_host");
    log.line(format!("QEMU {}", caps.version));
    let probe_dir = ManagedPath::verify(dir.join("probe")).unwrap();
    std::fs::create_dir_all(probe_dir.as_path()).unwrap();
    let machine = caps
        .recommended_machine(MachineFamily::Q35)
        .unwrap()
        .clone();
    let cpu = caps.cpu_model(CpuModel::VERIFIED_DEFAULT).unwrap();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let status = rt.block_on(probe::probe_accelerator(
        &caps,
        Accelerator::Whpx,
        &machine,
        FirmwareKind::Uefi,
        &cpu,
        &probe_dir,
        &ProbeConfig::default(),
    ));
    assert!(status.is_available(), "WHPX not available: {status}");
    caps.accelerator = status;

    let run_id = RunId::new_v7();
    let spec = VmLaunchSpec {
        run_id,
        machine,
        profile: DeviceProfile::LINUX,
        accelerator: Accelerator::Whpx,
        cpu,
        vcpus: 2,
        cores_per_socket: None,
        memory_mib: 1024,
        disk: Some(DiskSpec {
            format: DiskFormat::Qcow2,
        }),
        install_media: None,
        data_media: None,
        limits: qemu_backend::JobLimits::default(),
    };
    let t0 = Instant::now();
    let run = rt
        .block_on(QemuRun::start(&caps, &spec, &paths, RunConfig::default()))
        .expect("launch");
    assert_eq!(run.state(), ProcessState::Running);
    log.line(format!("VM running (WHPX) in {:?}", t0.elapsed()));

    // The take while it runs: QMP `blockdev-snapshot-internal-sync`.
    let t0 = Instant::now();
    rt.block_on(run.snapshot_disk("aaaa1111"))
        .expect("snapshot_disk");
    log.line(format!(
        "snapshot_disk(aaaa1111) while running in {:?}",
        t0.elapsed()
    ));
    // A second take and its deletion, both over QMP.
    rt.block_on(run.snapshot_disk("bbbb2222"))
        .expect("snapshot_disk 2");
    let t0 = Instant::now();
    rt.block_on(run.delete_disk_snapshot("bbbb2222"))
        .expect("delete_disk_snapshot");
    log.line(format!(
        "delete_disk_snapshot(bbbb2222) in {:?}",
        t0.elapsed()
    ));
    // An invalid tag is refused before it reaches QMP.
    assert!(
        rt.block_on(run.snapshot_disk("BAD NAME")).is_err(),
        "invalid tag accepted"
    );

    // The revert needs the VM off: orderly escalation, then the apply with
    // `qemu-img` (the product's path with the VM stopped).
    let report = rt
        .block_on(run.emergency_stop(Duration::from_secs(10)))
        .unwrap();
    assert_eq!(report.active_after, 0);
    let closed = rt.block_on(run.close()).unwrap();
    assert!(closed.job_closed);
    log.line(format!("VM stopped: phase {:?}", report.phase));
    // STATE-Y written after the take: must be discarded by the apply.
    write_state(&lab, &disk, "0x59");
    image::internal_snapshot(
        &cmd,
        SnapshotAction::Apply,
        &disk_mp,
        "aaaa1111",
        &not_cancelled(),
    )
    .expect("snapshot -a aaaa1111");
    log.line("after -a aaaa1111 (STATE-Y written after): the first 8 bytes must be STATE-X");
    assert_state(&lab, &disk, "0x58");
}
