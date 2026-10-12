//! Windows guests (ADR-0027) through the real IPC: firmware allowed per
//! guest, hardware profile fixed after creation, guest OS detected at the
//! import of an installation ISO (synthetic images of `guest-detect`),
//! baselines of a legacy BIOS VM without UEFI variables. Real core and
//! SQLite catalog; fake VM backend and image tool (no QEMU).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use guest_detect::testing::{UdfOpts, build_iso, build_udf, build_wim, wim_xml};
use lightvm_proto::ipc::{BaselineSummary, Request, ResponseBody, ServerMessage};
use lightvm_proto::{
    ErrorCode, Firmware, GuestArch, GuestOs, Label, MediaKind, MediaRecord, OperationId,
    OperationStatus, ReplyError, VmAction, VmConfig, VmId, VmRecord, VmRequest,
};
use lightvm_service::adapters::audit::FileAuditSink;
use lightvm_service::adapters::catalog::{ManagedMediaInspector, SqliteStore};
use lightvm_service::client::Client;
use lightvm_service::credential::Credential;
use lightvm_service::engine::{CoreEngine, core_config};
use lightvm_service::fakes::{
    Calls, FakeHost, FakeImageTool, FakeVmBackend, fake_qcow2, host_capacity, sample_config,
    sample_guest_config,
};
use lightvm_service::test_support::temp_dir;
use lightvm_service::{ServiceConfig, ServiceHandle, start};

const T: Duration = Duration::from_secs(20);

struct Svc {
    handle: ServiceHandle,
    ui: Credential,
    calls: Calls,
    dir: PathBuf,
}

fn short_dir(prefix: &str) -> PathBuf {
    let mut b = [0u8; 4];
    getrandom::fill(&mut b).unwrap();
    let d = std::env::temp_dir().join(format!(
        "{prefix}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3]
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

async fn start_svc(name: &str) -> Svc {
    let dir = temp_dir(name);
    let store = SqliteStore::open(&dir).unwrap();
    let audit = FileAuditSink::open(&dir.join("audit").join("audit.jsonl")).unwrap();
    let calls = Calls::default();
    let backend = FakeVmBackend {
        calls: calls.clone(),
        image_tool: Some(Arc::new(FakeImageTool {
            convert_delay: Duration::ZERO,
            calls: calls.clone(),
            disk_full: Arc::default(),
        })),
        ..FakeVmBackend::default()
    };
    let inspector = ManagedMediaInspector::new(store.root());
    let engine = CoreEngine::new(
        store,
        audit,
        backend,
        inspector,
        core_config(host_capacity()),
    )
    .unwrap();
    let host = FakeHost {
        calls: calls.clone(),
    };
    let mut cfg = ServiceConfig::new(&dir);
    cfg.runtime_dir = Some(dir.join("run"));
    cfg.display_dir = Some(short_dir("lvd"));
    let (handle, ui) = start(cfg, Box::new(engine), Box::new(host))
        .await
        .expect("start");
    Svc {
        handle,
        ui,
        calls,
        dir,
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

fn error(m: &ServerMessage) -> Option<(ErrorCode, String, Option<String>)> {
    match m {
        ServerMessage::Error {
            error: ReplyError::Operational(p),
            ..
        } => Some((
            p.code,
            p.message.clone(),
            p.details.as_ref().and_then(|d| d.get("field")).cloned(),
        )),
        _ => None,
    }
}

async fn create(c: &mut Client, config: VmConfig) -> ServerMessage {
    req(c, Request::CreateVm { config }).await
}

fn created(m: &ServerMessage) -> VmRecord {
    match m {
        ServerMessage::Response {
            result: ResponseBody::VmCreated { vm },
            ..
        } => vm.clone(),
        other => panic!("{other:?}"),
    }
}

async fn submit(c: &mut Client, vm_id: VmId, action: VmAction) -> OperationStatus {
    match req(
        c,
        Request::SubmitVm {
            request: VmRequest {
                operation_id: OperationId::new_v7(),
                vm_id,
                expected_run_id: None,
                action,
            },
        },
    )
    .await
    {
        ServerMessage::Response {
            result:
                ResponseBody::Operation {
                    operation: Some(op),
                    ..
                },
            ..
        } => op.status,
        other => panic!("{other:?}"),
    }
}

async fn import(c: &mut Client, path: &Path, kind: MediaKind) -> MediaRecord {
    match req(
        c,
        Request::ImportMedia {
            source_path: path.to_str().unwrap().into(),
            media_kind: kind,
            declared_origin: "synthetic test image".into(),
        },
    )
    .await
    {
        ServerMessage::Response {
            result: ResponseBody::MediaImported { media },
            ..
        } => media,
        other => panic!("{other:?}"),
    }
}

async fn list_media(c: &mut Client) -> Vec<MediaRecord> {
    match req(c, Request::ListMedia).await {
        ServerMessage::Response {
            result: ResponseBody::MediaList { media },
            ..
        } => media,
        other => panic!("{other:?}"),
    }
}

fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, bytes).unwrap();
    p
}

/// Windows XP Professional (x86) layout: `I386/` and `WIN51`, `WIN51IP`.
fn xp_image() -> Vec<u8> {
    build_iso(
        &[
            ("I386/TXTSETUP.SIF", b"[Version]".as_slice()),
            ("WIN51", b"".as_slice()),
            ("WIN51IP", b"".as_slice()),
        ],
        true,
    )
}

/// Windows 7 Professional x64: `sources/install.wim` on UDF only.
fn win7_image() -> Vec<u8> {
    let wim = build_wim(&wim_xml(
        6,
        1,
        7601,
        9,
        "Professional",
        "Windows 7 Professional",
    ));
    build_udf(&[("sources/install.wim", &wim)], UdfOpts::default())
}

#[tokio::test(flavor = "multi_thread")]
async fn firmware_and_profile_rules_at_create_and_update() {
    let svc = start_svc("win-rules").await;
    let mut c = connect(&svc).await;

    // Windows 7 boots with legacy BIOS only.
    let w7 = created(
        &create(
            &mut c,
            sample_guest_config("w7", GuestOs::Windows7, Firmware::Bios),
        )
        .await,
    );
    assert_eq!(
        (w7.config.guest_os, w7.config.firmware),
        (GuestOs::Windows7, Firmware::Bios)
    );
    let m = create(
        &mut c,
        sample_guest_config("w7-uefi", GuestOs::Windows7, Firmware::Uefi),
    )
    .await;
    let (code, msg, field) = error(&m).expect("refused");
    assert_eq!(code, ErrorCode::InvalidArgument);
    assert_eq!(field.as_deref(), Some("firmware"));
    assert!(
        msg.contains("Windows 7") && msg.contains("legacy BIOS"),
        "{msg}"
    );

    // Windows 10 with either firmware.
    for (name, fw) in [("w10-uefi", Firmware::Uefi), ("w10-bios", Firmware::Bios)] {
        let vm = created(&create(&mut c, sample_guest_config(name, GuestOs::Windows10, fw)).await);
        assert_eq!(vm.config.firmware, fw);
    }

    // Linux keeps UEFI only.
    let mut lin = sample_config("linux-bios");
    lin.firmware = Firmware::Bios;
    let (code, _, field) = error(&create(&mut c, lin).await).expect("refused");
    assert_eq!(
        (code, field.as_deref()),
        (ErrorCode::InvalidArgument, Some("firmware"))
    );

    // Per-guest minimum memory.
    let mut small = sample_guest_config("w10-small", GuestOs::Windows10, Firmware::Uefi);
    small.memory_mib = 1024;
    let (code, msg, _) = error(&create(&mut c, small).await).expect("refused");
    assert_eq!(code, ErrorCode::InvalidArgument);
    assert!(msg.contains("2048"), "{msg}");

    // The hardware profile is fixed after creation.
    let update = |config: VmConfig| Request::UpdateVm {
        vm_id: w7.id,
        expected_revision: w7.config_revision,
        config,
    };
    let mut n = w7.config.clone();
    n.guest_os = GuestOs::Windows10;
    let (code, _, field) = error(&req(&mut c, update(n)).await).expect("refused");
    assert_eq!(
        (code, field.as_deref()),
        (ErrorCode::InvalidArgument, Some("guest_os"))
    );
    let mut n = w7.config.clone();
    n.guest_os = GuestOs::Windows10;
    n.firmware = Firmware::Uefi;
    let (_, _, field) = error(&req(&mut c, update(n)).await).expect("refused");
    assert_eq!(field.as_deref(), Some("firmware"));
    // Resources can still change.
    let mut n = w7.config.clone();
    n.memory_mib = 3072;
    match req(&mut c, update(n)).await {
        ServerMessage::Response {
            result: ResponseBody::VmUpdated { vm },
            ..
        } => assert_eq!(vm.config.memory_mib, 3072),
        other => panic!("{other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn guest_is_detected_at_import_and_lazily_for_older_media() {
    let svc = start_svc("win-detect").await;
    let mut c = connect(&svc).await;
    let src = temp_dir("win-detect-src");

    let xp = import(
        &mut c,
        &write(&src, "xp.iso", &xp_image()),
        MediaKind::InstallIso,
    )
    .await;
    let g = xp.detected_guest.clone().expect("XP detected");
    assert_eq!(g.os, GuestOs::WindowsXp);
    assert_eq!(g.arch, Some(GuestArch::X86));
    assert_eq!(
        g.edition.as_ref().map(Label::as_str),
        Some("Windows XP Professional")
    );

    let w7 = import(
        &mut c,
        &write(&src, "win7.iso", &win7_image()),
        MediaKind::InstallIso,
    )
    .await;
    let g = w7.detected_guest.clone().expect("Windows 7 detected");
    assert_eq!(
        (g.os, g.arch, g.build),
        (GuestOs::Windows7, Some(GuestArch::X86_64), Some(7601))
    );

    // A data ISO is never examined, even with Windows content.
    let data = import(
        &mut c,
        &write(&src, "data.iso", &xp_image()),
        MediaKind::DataIso,
    )
    .await;
    assert_eq!(data.detected_guest, None);
    // An image that is not recognized imports normally, without a guest.
    let mut plain = build_iso(&[("README.TXT", b"hello".as_slice())], true);
    plain.resize(plain.len().max(48 * 1024), 0);
    let other = import(
        &mut c,
        &write(&src, "plain.iso", &plain),
        MediaKind::InstallIso,
    )
    .await;
    assert_eq!(other.detected_guest, None);

    // Listed with the detected guest.
    let listed = list_media(&mut c).await;
    let find = |id| listed.iter().find(|m| m.id == id).unwrap().clone();
    assert_eq!(find(xp.id), xp);
    assert_eq!(find(w7.id), w7);
    assert_eq!(find(data.id).detected_guest, None);

    // Media imported before the detection existed (schema 6 rows read as
    // not examined): examined once at the next listing and persisted.
    let conn = vm_storage::migrate::open_connection(&svc.dir.join(vm_storage::DB_FILE)).unwrap();
    conn.execute_batch("UPDATE media SET detected_guest = NULL, guest_examined = 0")
        .unwrap();
    let listed = list_media(&mut c).await;
    let find = |id| listed.iter().find(|m| m.id == id).unwrap().clone();
    assert_eq!(find(xp.id).detected_guest, xp.detected_guest);
    assert_eq!(find(w7.id).detected_guest, w7.detected_guest);
    assert_eq!(find(data.id).detected_guest, None);
    let examined: i64 = conn
        .query_row(
            "SELECT count(*) FROM media WHERE guest_examined = 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(examined, 3, "install media examined, the data ISO not");
}

async fn baselines(c: &mut Client, vm_id: VmId) -> Vec<BaselineSummary> {
    match req(c, Request::ListBaselines { vm_id }).await {
        ServerMessage::Response {
            result: ResponseBody::Baselines { baselines, .. },
            ..
        } => baselines,
        other => panic!("{other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn legacy_bios_baseline_has_no_firmware_state() {
    let svc = start_svc("win-baseline").await;
    let mut c = connect(&svc).await;
    let vm = created(
        &create(
            &mut c,
            sample_guest_config("w7-base", GuestOs::Windows7, Firmware::Bios),
        )
        .await,
    );
    // Standalone disk as the backend writes it at the first start; no UEFI
    // variables for SeaBIOS.
    let vm_dir = svc.dir.join("vms").join(vm.id.to_string());
    std::fs::create_dir_all(&vm_dir).unwrap();
    std::fs::write(
        vm_dir.join("primary.qcow2"),
        fake_qcow2(None, b"windows 7 installed"),
    )
    .unwrap();

    let b = match req(
        &mut c,
        Request::CreateBaseline {
            vm_id: vm.id,
            label: Label::parse("clean install").unwrap(),
            allow_unclean: false,
        },
    )
    .await
    {
        ServerMessage::Response {
            result: ResponseBody::BaselineCreated { baseline },
            ..
        } => baseline,
        other => panic!("{other:?}"),
    };
    assert_eq!(b.firmware_sha256, None, "SeaBIOS has no NVRAM");
    assert!(b.clean_shutdown);
    assert!(
        !vm_dir.join("firmware").exists(),
        "no UEFI variables created for a legacy BIOS VM"
    );
    assert_eq!(baselines(&mut c, vm.id).await, vec![b.clone()]);

    // Restore and start on the overlay, without variables.
    assert!(matches!(
        req(
            &mut c,
            Request::RestoreBaseline {
                vm_id: vm.id,
                baseline_id: b.baseline_id,
            },
        )
        .await,
        ServerMessage::Response {
            result: ResponseBody::BaselineRestored { .. },
            ..
        }
    ));
    assert_eq!(
        submit(&mut c, vm.id, VmAction::Start).await,
        OperationStatus::Succeeded
    );
    let calls = svc.calls.lock().unwrap().clone();
    assert!(
        calls
            .iter()
            .any(|c| c.starts_with("vm_start:w7-base:") && c.ends_with(":no_vars:overlay")),
        "{calls:?}"
    );
    assert_eq!(
        submit(&mut c, vm.id, VmAction::Stop).await,
        OperationStatus::Succeeded
    );
    // A second baseline from the overlay: still without firmware state.
    let b2 = match req(
        &mut c,
        Request::CreateBaseline {
            vm_id: vm.id,
            label: Label::parse("after restore").unwrap(),
            allow_unclean: true,
        },
    )
    .await
    {
        ServerMessage::Response {
            result: ResponseBody::BaselineCreated { baseline },
            ..
        } => baseline,
        other => panic!("{other:?}"),
    };
    assert_eq!(b2.firmware_sha256, None);
    assert_eq!(b2.derived_from, Some(b.baseline_id));
    assert!(!vm_dir.join("firmware").exists());
}
