//! VMware import through the real IPC (ADR-0030): preview, import of a
//! snapshot chain converted by the real `qemu-img` (content identical to the
//! source, source files unchanged), progress events, refusals (locked VM,
//! name taken, storage budget), cancellation by the user and by the
//! Emergency Stop, shutdown during a conversion, with nothing left behind.
//! Real core and SQLite catalog; fake VM backend whose image tool runs the
//! approved `qemu-img` for the VMware operations. Needs `LIGHTVM_QEMU_DIR`
//! (skipped with a message otherwise). The VMware VM folders are written by
//! the tests: no real VMware VM is used.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use lightvm_proto::ipc::{
    ImportWarningKind, NotImportedItem, Request, ResponseBody, ServerMessage, ServiceEvent,
    VmwareController, VmwareImportPhase, VmwareImportPreview,
};
use lightvm_proto::{
    AuditAction, AuditEvent, AuditOutcome, DiskController, DisplayAdapter, ErrorCode, GuestOs,
    ReplyError, VmName, VmRecord,
};
use lightvm_service::adapters::audit::FileAuditSink;
use lightvm_service::adapters::backend::QemuImageTool;
use lightvm_service::adapters::catalog::{ManagedMediaInspector, SqliteStore};
use lightvm_service::client::Client;
use lightvm_service::credential::Credential;
use lightvm_service::engine::{CoreEngine, core_config};
use lightvm_service::fakes::{Calls, FakeHost, FakeImageTool, FakeVmBackend, host_capacity};
use lightvm_service::test_support::temp_dir;
use lightvm_service::{ServiceConfig, ServiceHandle, start};

const T: Duration = Duration::from_secs(120);

fn qemu_dir() -> Option<PathBuf> {
    let d = PathBuf::from(std::env::var_os("LIGHTVM_QEMU_DIR")?);
    d.join(exe("qemu-img")).is_file().then_some(d)
}

fn exe(n: &str) -> String {
    if cfg!(windows) {
        format!("{n}.exe")
    } else {
        n.to_owned()
    }
}

/// Path as the system resolves it (no `\\?\`), as the import requires.
fn resolved(p: &Path) -> PathBuf {
    let c = std::fs::canonicalize(p).unwrap();
    let s = c.to_string_lossy().into_owned();
    PathBuf::from(s.strip_prefix(r"\\?\").unwrap_or(&s))
}

struct Svc {
    handle: ServiceHandle,
    ui: Credential,
    dir: PathBuf,
}

struct Opts {
    delay: Duration,
    free_bytes: Option<u64>,
}

async fn start_svc(name: &str, q: &Path, o: Opts) -> Svc {
    let dir = temp_dir(name);
    let store = SqliteStore::open(&dir).unwrap();
    let audit = FileAuditSink::open(&dir.join("audit").join("audit.jsonl")).unwrap();
    let calls = Calls::default();
    let logs = dir.join("img-logs");
    std::fs::create_dir_all(&logs).unwrap();
    let tool = QemuImageTool::new(
        qemu_backend::ManagedPath::verify(resolved(&q.join(exe("qemu-img")))).unwrap(),
        qemu_backend::ManagedPath::verify(resolved(&logs)).unwrap(),
    );
    let backend = FakeVmBackend {
        calls: calls.clone(),
        image_tool: Some(Arc::new(FakeImageTool {
            calls: calls.clone(),
            vmdk_tool: Some(Arc::new(tool)),
            vmdk_delay: o.delay,
            free_bytes: o.free_bytes,
            ..FakeImageTool::default()
        })),
        ..FakeVmBackend::default()
    };
    let inspector = ManagedMediaInspector::new(store.root());
    let engine =
        CoreEngine::new(store, audit, backend, inspector, core_config(host_capacity())).unwrap();
    let mut cfg = ServiceConfig::new(&dir);
    cfg.runtime_dir = Some(dir.join("run"));
    let (handle, ui) = start(cfg, Box::new(engine), Box::new(FakeHost { calls }))
        .await
        .expect("start");
    Svc { handle, ui, dir }
}

fn plenty() -> Opts {
    Opts {
        delay: Duration::ZERO,
        free_bytes: Some(1 << 40),
    }
}

async fn connect(svc: &Svc) -> Client {
    let mut c = tokio::time::timeout(T, Client::connect(svc.handle.endpoint()))
        .await
        .unwrap()
        .unwrap();
    match c.hello(&svc.ui).await.unwrap() {
        ServerMessage::HelloOk { .. } => c,
        other => panic!("{other:?}"),
    }
}

async fn req(c: &mut Client, r: Request) -> ServerMessage {
    tokio::time::timeout(T, c.request(r))
        .await
        .expect("timeout")
        .unwrap()
}

fn error(m: &ServerMessage) -> Option<lightvm_proto::PublicError> {
    match m {
        ServerMessage::Error {
            error: ReplyError::Operational(p),
            ..
        } => Some(p.clone()),
        _ => None,
    }
}

fn read_audit(dir: &Path) -> Vec<AuditEvent> {
    std::fs::read_to_string(dir.join("audit").join("audit.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

fn imports(dir: &Path, o: AuditOutcome) -> usize {
    read_audit(dir)
        .iter()
        .filter(|e| e.action == AuditAction::VmImport && e.outcome == o)
        .count()
}

/// Nothing of an import left in the catalog: `tmp/` empty, no VM folder.
fn nothing_left(dir: &Path) {
    let tmp: Vec<_> = std::fs::read_dir(dir.join("tmp"))
        .map(|r| r.flatten().map(|e| e.file_name()).collect())
        .unwrap_or_default();
    assert!(tmp.is_empty(), "tmp/ not empty: {tmp:?}");
    let vms: Vec<_> = std::fs::read_dir(dir.join("vms"))
        .map(|r| r.flatten().map(|e| e.file_name()).collect())
        .unwrap_or_default();
    assert!(vms.is_empty(), "VM folders left: {vms:?}");
}

/// A VMware VM folder: a 64 MiB base with data and two snapshots with
/// data (made with `qemu-img` and `qemu-io`), and its `.vmx`.
struct VmwareVm {
    root: PathBuf,
    vmx: PathBuf,
    folder: PathBuf,
}

impl VmwareVm {
    fn new(q: &Path, name: &str, vmx_extra: &str) -> Self {
        let root = temp_dir(name);
        let folder = resolved(&root).join("Win7 VM");
        std::fs::create_dir_all(&folder).unwrap();
        let run = |prog: &str, args: &[&str]| {
            let o = std::process::Command::new(q.join(exe(prog)))
                .current_dir(&folder)
                .args(args)
                .output()
                .unwrap();
            assert!(o.status.success(), "{args:?}: {}", String::from_utf8_lossy(&o.stderr));
        };
        run("qemu-img", &["create", "-q", "-f", "vmdk", "Win7.vmdk", "64M"]);
        run("qemu-io", &["-f", "vmdk", "-c", "write -P 0x11 0 2M", "Win7.vmdk"]);
        run("qemu-img", &["create", "-q", "-f", "vmdk", "-b", "Win7.vmdk", "-F", "vmdk", "Win7-000001.vmdk"]);
        run("qemu-io", &["-f", "vmdk", "-c", "write -P 0x22 1M 2M", "Win7-000001.vmdk"]);
        run("qemu-img", &["create", "-q", "-f", "vmdk", "-b", "Win7-000001.vmdk", "-F", "vmdk", "Win7-000002.vmdk"]);
        run("qemu-io", &["-f", "vmdk", "-c", "write -P 0x33 0 512k", "Win7-000002.vmdk"]);
        let vmx = folder.join("Win7.vmx");
        std::fs::write(
            &vmx,
            format!(
                ".encoding = \"UTF-8\"\nconfig.version = \"8\"\ndisplayName = \"Win7 VS2010\"\n\
                 guestOS = \"windows7-64\"\nnumvcpus = \"4\"\ncpuid.coresPerSocket = \"2\"\nmemsize = \"3072\"\n\
                 cleanShutdown = \"TRUE\"\nscsi0.present = \"TRUE\"\nscsi0.virtualDev = \"pvscsi\"\n\
                 scsi0:0.present = \"TRUE\"\nscsi0:0.fileName = \"Win7-000002.vmdk\"\n\
                 ethernet0.present = \"TRUE\"\nsound.present = \"TRUE\"\nusb.present = \"TRUE\"\n{vmx_extra}"
            ),
        )
        .unwrap();
        Self { root, vmx, folder }
    }

    fn vmx_str(&self) -> String {
        self.vmx.to_string_lossy().into_owned()
    }

    fn hashes(&self) -> Vec<(String, Vec<u8>)> {
        use sha2::Digest as _;
        let mut v: Vec<_> = std::fs::read_dir(&self.folder)
            .unwrap()
            .flatten()
            .map(|e| {
                (
                    e.file_name().to_string_lossy().into_owned(),
                    sha2::Sha256::digest(std::fs::read(e.path()).unwrap()).to_vec(),
                )
            })
            .collect();
        v.sort();
        v
    }
}

impl Drop for VmwareVm {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

async fn preview(c: &mut Client, vmx: &str) -> Result<VmwareImportPreview, lightvm_proto::PublicError> {
    let m = req(
        c,
        Request::PreviewVmwareImport {
            vmx_path: vmx.into(),
        },
    )
    .await;
    match m {
        ServerMessage::Response {
            result: ResponseBody::VmwareImportPreview { preview },
            ..
        } => Ok(*preview),
        other => Err(error(&other).unwrap_or_else(|| panic!("{other:?}"))),
    }
}

fn import_req(vmx: &str, name: &str, display: DisplayAdapter) -> Request {
    Request::ImportVmware {
        vmx_path: vmx.into(),
        name: VmName::parse(name).unwrap(),
        display,
    }
}

fn imported(m: &ServerMessage) -> VmRecord {
    match m {
        ServerMessage::Response {
            result: ResponseBody::VmwareImported { vm },
            ..
        } => vm.clone(),
        other => panic!("{other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn preview_and_import_of_a_snapshot_chain() {
    let Some(q) = qemu_dir() else {
        eprintln!("LIGHTVM_QEMU_DIR not set: skipped");
        return;
    };
    let vm = VmwareVm::new(&q, "vmw-src", "");
    let before = vm.hashes();
    let svc = start_svc("vmw-ok", &q, plenty()).await;
    let mut c = connect(&svc).await;
    assert!(matches!(req(&mut c, Request::Subscribe).await, ServerMessage::Response { .. }));
    let p = preview(&mut c, &vm.vmx_str()).await.expect("preview");
    assert_eq!(p.vmx_file, "Win7.vmx");
    assert_eq!(p.display_name.as_deref(), Some("Win7 VS2010"));
    assert_eq!(p.config.name.as_str(), "Win7 VS2010");
    assert_eq!(p.config.guest_os, GuestOs::Windows7);
    assert_eq!((p.config.vcpus, p.config.memory_mib, p.config.disk_gib), (4, 3072, 1));
    let hw = p.config.imported.expect("imported hardware");
    assert_eq!(hw.disk_controller, DiskController::Pvscsi);
    assert_eq!(hw.cores_per_socket, Some(2));
    assert_eq!(p.disk.vmware_controller, VmwareController::Pvscsi);
    assert_eq!(p.disk.snapshot_levels, 2);
    assert_eq!(p.disk.virtual_size_bytes, 64 << 20);
    assert!(p.vmware_svga_offered);
    let items: Vec<_> = p.not_imported.iter().map(|n| n.item).collect();
    for want in [NotImportedItem::Network, NotImportedItem::Sound, NotImportedItem::Usb, NotImportedItem::Snapshots] {
        assert!(items.contains(&want), "{want:?}: {items:?}");
    }
    // The preview never writes into the catalog.
    nothing_left(&svc.dir);

    let r = req(&mut c, import_req(&vm.vmx_str(), "Win7 imported", DisplayAdapter::VmwareSvga)).await;
    let rec = imported(&r);
    let hw = rec.config.imported.unwrap();
    assert_eq!(hw.display, DisplayAdapter::VmwareSvga);
    // Listed, stopped, with its converted disk.
    match req(&mut c, Request::VmStatus { vm_id: rec.id }).await {
        ServerMessage::Response {
            result: ResponseBody::Vm { vm: s },
            ..
        } => {
            assert_eq!(s.state, lightvm_proto::VmState::Stopped);
            assert_eq!(s.config.imported, rec.config.imported);
        }
        other => panic!("{other:?}"),
    }
    let disk = svc.dir.join("vms").join(rec.id.to_string()).join("primary.qcow2");
    assert!(disk.is_file());
    let o = std::process::Command::new(q.join(exe("qemu-img")))
        .args(["compare", "-q", "-f", "vmdk", "-F", "qcow2"])
        .arg(vm.folder.join("Win7-000002.vmdk"))
        .arg(&disk)
        .output()
        .unwrap();
    assert!(o.status.success(), "converted content differs: {}", String::from_utf8_lossy(&o.stderr));
    // The VMware files are untouched.
    assert_eq!(vm.hashes(), before);
    // Nothing left in tmp/.
    assert_eq!(std::fs::read_dir(svc.dir.join("tmp")).unwrap().count(), 0);
    assert_eq!(imports(&svc.dir, AuditOutcome::Succeeded), 1);
    // Progress events: conversion reached 100 %, then finishing.
    let mut phases = Vec::new();
    while let Ok(Ok(Ok(ev))) =
        tokio::time::timeout(Duration::from_millis(500), c.next_event()).await
    {
        if let ServiceEvent::VmwareImportProgress { phase, percent } = ev {
            phases.push((phase, percent));
        }
    }
    assert!(phases.contains(&(VmwareImportPhase::Checking, 0)), "{phases:?}");
    assert!(phases.contains(&(VmwareImportPhase::Converting, 100)), "{phases:?}");
    assert!(phases.contains(&(VmwareImportPhase::Finishing, 100)), "{phases:?}");
    // The same name again: refused before any conversion.
    let r = req(&mut c, import_req(&vm.vmx_str(), "WIN7 IMPORTED", DisplayAdapter::Standard)).await;
    assert_eq!(error(&r).unwrap().code, ErrorCode::StateConflict);
    svc.handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn refusals_leave_nothing_and_are_audited() {
    let Some(q) = qemu_dir() else {
        eprintln!("LIGHTVM_QEMU_DIR not set: skipped");
        return;
    };
    // Locked VM (open in VMware).
    let locked = VmwareVm::new(&q, "vmw-lck", "");
    std::fs::create_dir(locked.folder.join("Win7.vmx.lck")).unwrap();
    // LSI Logic Windows VM: imported with the preparation warning.
    let lsi = VmwareVm::new(&q, "vmw-lsi", "scsi0.virtualDev = \"lsilogic\"\n");
    let svc = start_svc(
        "vmw-ref",
        &q,
        Opts {
            delay: Duration::ZERO,
            free_bytes: Some(1 << 20),
        },
    )
    .await;
    let mut c = connect(&svc).await;
    let e = preview(&mut c, &locked.vmx_str()).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::StateConflict);
    let r = req(&mut c, import_req(&locked.vmx_str(), "locked", DisplayAdapter::Standard)).await;
    let e = error(&r).unwrap();
    assert_eq!(e.code, ErrorCode::StateConflict);
    assert_eq!(e.details.as_ref().and_then(|d| d.get("reason")).map(String::as_str), Some("locked"));
    assert!(!e.message.contains(&*locked.folder.to_string_lossy()), "no folder in messages");
    let p = preview(&mut c, &lsi.vmx_str()).await.unwrap();
    assert!(p.warnings.iter().any(|w| w.kind == ImportWarningKind::StorageDriverPreparation));
    assert!(p.config.imported.unwrap().storage_driver_preparation);
    // 1 MiB free: the storage budget refuses the conversion.
    let r = req(&mut c, import_req(&lsi.vmx_str(), "lsi", DisplayAdapter::Standard)).await;
    assert_eq!(error(&r).unwrap().code, ErrorCode::ResourceLimit);
    // A path that is not a .vmx, a relative path.
    for bad in ["relative.vmx", "C:/x/disk.vmdk"] {
        let r = req(&mut c, import_req(bad, "bad", DisplayAdapter::Standard)).await;
        assert_eq!(error(&r).unwrap().code, ErrorCode::InvalidArgument, "{bad}");
    }
    nothing_left(&svc.dir);
    assert!(imports(&svc.dir, AuditOutcome::Denied) >= 3, "{:?}", read_audit(&svc.dir));
    assert_eq!(imports(&svc.dir, AuditOutcome::Succeeded), 0);
    svc.handle.shutdown().await;
}

/// Waits for the conversion phase of the import in progress.
async fn wait_converting(c: &mut Client) {
    loop {
        let ev = tokio::time::timeout(T, c.next_event()).await.expect("event");
        if let Ok(Ok(ServiceEvent::VmwareImportProgress {
            phase: VmwareImportPhase::Converting,
            ..
        })) = ev
        {
            return;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn cancellation_by_the_user_and_by_the_emergency_stop() {
    let Some(q) = qemu_dir() else {
        eprintln!("LIGHTVM_QEMU_DIR not set: skipped");
        return;
    };
    let vm = VmwareVm::new(&q, "vmw-cnl", "");
    let svc = start_svc(
        "vmw-cnl",
        &q,
        Opts {
            delay: Duration::from_secs(30),
            free_bytes: Some(1 << 40),
        },
    )
    .await;
    // By the user.
    let mut watcher = connect(&svc).await;
    req(&mut watcher, Request::Subscribe).await;
    let mut c = connect(&svc).await;
    let vmx = vm.vmx_str();
    let task = tokio::spawn(async move {
        let r = req(&mut c, import_req(&vmx, "cancelled", DisplayAdapter::Standard)).await;
        (c, r)
    });
    wait_converting(&mut watcher).await;
    // Only one import at a time.
    let r = req(&mut watcher, import_req(&vm.vmx_str(), "second", DisplayAdapter::Standard)).await;
    assert_eq!(error(&r).unwrap().code, ErrorCode::VmBusy);
    match req(&mut watcher, Request::CancelVmwareImport).await {
        ServerMessage::Response {
            result: ResponseBody::VmwareImportCancel { cancelled },
            ..
        } => assert!(cancelled),
        other => panic!("{other:?}"),
    }
    let (_c, r) = tokio::time::timeout(T, task).await.unwrap().unwrap();
    assert_eq!(error(&r).unwrap().code, ErrorCode::StateConflict, "{r:?}");
    nothing_left(&svc.dir);
    assert_eq!(imports(&svc.dir, AuditOutcome::Failed), 1);
    // Nothing to cancel now.
    match req(&mut watcher, Request::CancelVmwareImport).await {
        ServerMessage::Response {
            result: ResponseBody::VmwareImportCancel { cancelled },
            ..
        } => assert!(!cancelled),
        other => panic!("{other:?}"),
    }
    // By the Emergency Stop.
    let mut c = connect(&svc).await;
    let vmx = vm.vmx_str();
    let task = tokio::spawn(async move {
        let r = req(&mut c, import_req(&vmx, "stopped", DisplayAdapter::Standard)).await;
        (c, r)
    });
    wait_converting(&mut watcher).await;
    req(&mut watcher, Request::EmergencyStop).await;
    let (_c, r) = tokio::time::timeout(T, task).await.unwrap().unwrap();
    let e = error(&r).unwrap();
    assert!(e.message.contains("Emergency Stop"), "{e:?}");
    nothing_left(&svc.dir);
    // The service shutdown during a conversion rolls it back too.
    let mut c = connect(&svc).await;
    let vmx = vm.vmx_str();
    let task = tokio::spawn(async move {
        let _ = c.request(import_req(&vmx, "shutdown", DisplayAdapter::Standard)).await;
    });
    wait_converting(&mut watcher).await;
    let dir = svc.dir.clone();
    svc.handle.shutdown().await;
    let _ = tokio::time::timeout(T, task).await;
    nothing_left(&dir);
    // The catalog reopens clean (no open intent).
    let store = SqliteStore::open(&dir).unwrap();
    drop(store);
    nothing_left(&dir);
}
