//! Round-trip serde e validazioni dei tipi pubblici.

use std::collections::BTreeSet;

use lightvm_proto::*;
use proptest::prelude::*;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::json;

fn rt<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(v: &T) {
    let s = serde_json::to_string(v).expect("serializzazione");
    let back: T = serde_json::from_str(&s).expect("deserializzazione");
    assert_eq!(&back, v, "round-trip di {s}");
}

fn ts(ms: i64) -> UtcTimestamp {
    UtcTimestamp::from_unix_millis(ms).unwrap()
}

fn vm(n: u8) -> VmId {
    VmId::from_unix_millis(1_760_000_000_000, [n; 10])
}

fn sample_config() -> VmConfig {
    VmConfig {
        name: VmName::parse("ubuntu-lts 24.04").unwrap(),
        guest_os: GuestOs::UbuntuLtsX86_64,
        vcpus: 2,
        memory_mib: 4096,
        disk_gib: 40,
        network: NetworkProfile::Offline,
        install_media: Some(MediaId::from_unix_millis(1, [1; 10])),
        data_media: None,
        firmware: Firmware::Uefi,
    }
}

#[test]
fn ids_roundtrip_and_reject_non_v7() {
    let id = vm(7);
    let s = id.to_string();
    assert_eq!(s.len(), 36);
    assert_eq!(s.parse::<VmId>().unwrap(), id);
    assert_eq!(serde_json::to_value(id).unwrap(), json!(s));
    assert_eq!(VmId::from_bytes(id.to_bytes()).unwrap(), id);
    // v4 e nil rifiutati; forme non canoniche rifiutate.
    assert_eq!(
        "550e8400-e29b-41d4-a716-446655440000".parse::<VmId>(),
        Err(IdParseError::NotVersion7)
    );
    assert!("00000000-0000-0000-0000-000000000000".parse::<RunId>().is_err());
    assert!(format!("{{{s}}}").parse::<VmId>().is_err());
    assert!(s.replace('-', "").parse::<VmId>().is_err());
    assert!(serde_json::from_value::<VmId>(json!("../etc/passwd")).is_err());
    // Gli ID sono ordinati per istante di creazione.
    assert!(VmId::from_unix_millis(1, [0xff; 10]) < VmId::from_unix_millis(2, [0; 10]));
}

#[test]
fn timestamp_roundtrip_is_millisecond_utc() {
    let t = ts(1_760_004_000_123);
    assert_eq!(t.to_string(), "2025-10-09T10:00:00.123Z");
    rt(&t);
    let parsed: UtcTimestamp = "2025-10-09T12:00:00.123456+02:00".parse().unwrap();
    assert_eq!(parsed, t, "offset convertito in UTC, troncato al ms");
    assert!("2025-10-09 10:00:00".parse::<UtcTimestamp>().is_err());
    assert_eq!(
        t.checked_add_millis(877).unwrap().as_unix_millis(),
        1_760_004_001_000
    );
}

#[test]
fn models_roundtrip() {
    let cfg = sample_config();
    rt(&cfg);
    rt(&VmRecord {
        id: vm(1),
        config: cfg.clone(),
        config_revision: 3,
        persisted_state: VmState::Stopped,
        created_at: ts(0),
    });
    let media = MediaRecord {
        id: MediaId::from_unix_millis(5, [5; 10]),
        kind: MediaKind::DataIso,
        sha256: Sha256Digest::from_bytes([0xab; 32]),
        size_bytes: 1 << 30,
        declared_origin: Label::parse("ISO prodotta dall'utente").unwrap(),
        approved_at: ts(10),
    };
    rt(&media);
    rt(&MediaEntry {
        record: media,
        managed_path: ManagedRelPath::parse("media/0192-abc.iso").unwrap(),
        file_identity: Some(FileIdentity {
            volume: 7,
            file_id: [3; 16],
        }),
    });
    rt(&BaselineRecord {
        id: BaselineId::from_unix_millis(9, [9; 10]),
        vm_id: vm(1),
        config_revision: 3,
        origin: BaselineOrigin::CleanShutdown,
        disk_sha256: Sha256Digest::from_bytes([1; 32]),
        firmware_sha256: Sha256Digest::from_bytes([2; 32]),
        protected: true,
        label: None,
        created_at: ts(20),
    });
    rt(&RunRecord {
        id: RunId::from_unix_millis(30, [0; 10]),
        vm_id: vm(1),
        config_revision: 3,
        baseline_id: None,
        started_at: ts(30),
        ended_at: Some(ts(40)),
        end_reason: Some(RunEndReason::EmergencyStop),
    });
    rt(&OperationRecord {
        id: OperationId::from_unix_millis(31, [0; 10]),
        vm_id: vm(1),
        run_id: None,
        session_id: Some(SessionId::from_unix_millis(1, [1; 10])),
        kind: OperationKind::Start,
        status: OperationStatus::Discarded,
        error: Some(ErrorCode::PermissionDenied),
        requested_at: ts(31),
        updated_at: ts(32),
    });
    rt(&Screenshot {
        vm_id: vm(1),
        run_id: RunId::from_unix_millis(30, [0; 10]),
        geometry: DisplayGeometry::new(1280, 800).unwrap(),
        format: ImageFormat::Png,
        sha256: Sha256Digest::from_bytes([4; 32]),
        byte_len: 33_223,
        captured_at: ts(50),
    });
    rt(&ResourceLimits::MVP_DEFAULT);
    rt(&AuditEvent {
        seq: 1,
        at: ts(1),
        actor: Actor::User,
        action: AuditAction::VmCreate,
        outcome: AuditOutcome::Succeeded,
        vm_id: Some(vm(1)),
        run_id: None,
        operation_id: None,
        session_id: None,
        capability: None,
        error: None,
        termination: None,
    });
}

#[test]
fn config_rejects_unknown_fields_and_non_offline_network() {
    let mut v = serde_json::to_value(sample_config()).unwrap();
    v["network"] = json!("nat");
    assert!(serde_json::from_value::<VmConfig>(v.clone()).is_err());
    v["network"] = json!("offline");
    v["qemu_args"] = json!("-drive file=/etc/shadow");
    assert!(
        serde_json::from_value::<VmConfig>(v).is_err(),
        "campi sconosciuti rifiutati"
    );
}

#[test]
fn session_types_roundtrip() {
    let sid = SessionId::from_unix_millis(1, [1; 10]);
    let grant = Grant {
        id: GrantId::from_unix_millis(1, [2; 10]),
        session_id: sid,
        vm_allowlist: BTreeSet::from([vm(1)]),
        capabilities: BTreeSet::from([Capability::VmScreenshot, Capability::VmInput]),
        expires_at: ts(60_000),
        quotas: Quotas::UNBOUNDED,
        policy_revision: PolicyRevision(4),
    };
    rt(&grant);
    let v = serde_json::to_value(&grant).unwrap();
    assert_eq!(v["capabilities"], json!(["vm.screenshot", "vm.input"]));
    rt(&Lease::new(sid, ts(0), 10_000).unwrap());
    for cause in [TerminationCause::LeaseExpired, TerminationCause::EmergencyStop] {
        rt(&cause);
    }
    let outcomes: Vec<_> = [
        TestOutcome::Passed,
        TestOutcome::Failed,
        TestOutcome::Inconclusive,
        TestOutcome::Cancelled,
        TestOutcome::TimedOut,
        TestOutcome::InfrastructureError,
    ]
    .iter()
    .map(|o| serde_json::to_value(o).unwrap())
    .collect();
    assert_eq!(
        outcomes,
        [
            json!("passed"),
            json!("failed"),
            json!("inconclusive"),
            json!("cancelled"),
            json!("timed_out"),
            json!("infrastructure_error")
        ]
    );
    let caps: Vec<_> = Capability::ALL.iter().map(|c| c.as_str()).collect();
    assert_eq!(
        caps,
        [
            "vm.list",
            "vm.status",
            "vm.start",
            "vm.stop",
            "vm.reset",
            "vm.screenshot",
            "vm.input",
            "vm.snapshot",
            "vm.restore",
            "vm.logs"
        ]
    );
}

#[test]
fn lease_limit_is_ten_seconds() {
    let sid = SessionId::from_unix_millis(1, [1; 10]);
    assert!(Lease::new(sid, ts(0), 10_001).is_err());
    assert!(Lease::new(sid, ts(0), 0).is_err());
    assert!(Lease::new(sid, ts(0), -5).is_err());
    let l = Lease::new(sid, ts(0), 10_000).unwrap();
    assert!(l.is_valid_at(ts(9_999)));
    assert!(!l.is_valid_at(ts(10_000)));
    // Un lease deserializzato con durata oltre il limite non è mai valido.
    let forged = Lease {
        session_id: sid,
        renewed_at: ts(0),
        expires_at: ts(60_000),
    };
    assert!(!forged.is_valid_at(ts(1)));
}

#[test]
fn session_token_is_redacted() {
    let t = SessionToken::from_bytes([0x5a; 32]);
    assert_eq!(format!("{t:?}"), "SessionToken(<redatto>)");
    rt(&t);
    assert!(t.ct_eq(&SessionToken::from_bytes([0x5a; 32])));
    assert!(!t.ct_eq(&SessionToken::from_bytes([0x5b; 32])));
    let caller = Caller::Agent {
        session_id: SessionId::from_unix_millis(1, [1; 10]),
        token: Some(t),
    };
    assert!(!format!("{caller:?}").contains("5a5a"));
    assert!(!format!("{caller:?}").contains("90, 90"));
}

#[test]
fn request_and_input_roundtrip() {
    let geometry = DisplayGeometry::new(1280, 800).unwrap();
    let run = RunId::from_unix_millis(30, [0; 10]);
    let req = VmRequest {
        operation_id: OperationId::from_unix_millis(40, [0; 10]),
        vm_id: vm(1),
        expected_run_id: Some(run),
        action: VmAction::Input(InputBatch {
            vm_id: vm(1),
            run_id: run,
            geometry,
            events: vec![
                InputEvent::PointerMove(PointerMove::from_pixels(1279, 0, geometry).unwrap()),
                InputEvent::PointerButton(PointerButton {
                    button: MouseButton::Left,
                    down: true,
                }),
                InputEvent::Key(KeyAction {
                    qcode: QCode::parse("ret").unwrap(),
                    down: true,
                }),
            ],
        }),
    };
    rt(&req);
    let v = serde_json::to_value(&req).unwrap();
    assert_eq!(v["action"]["type"], json!("input"));
    rt(&VmRequest {
        action: VmAction::Start,
        expected_run_id: None,
        ..req
    });
}

#[test]
fn pointer_scaling_matches_qemu() {
    let g = DisplayGeometry::new(1280, 800).unwrap();
    assert_eq!(
        PointerMove::from_pixels(0, 0, g).unwrap(),
        PointerMove { x: 0, y: 0 }
    );
    assert_eq!(
        PointerMove::from_pixels(1279, 799, g).unwrap(),
        PointerMove { x: 32767, y: 32767 }
    );
    assert_eq!(
        u32::from(PointerMove::from_pixels(640, 400, g).unwrap().x),
        640 * 32767 / 1279
    );
    assert!(PointerMove::from_pixels(1280, 0, g).is_err());
    assert!(PointerMove::new(32768, 0).is_err());
    let bad: PointerMove = serde_json::from_value(json!({"x": 40000, "y": 1})).unwrap();
    assert!(
        bad.validate().is_err(),
        "intervallo verificato dopo la deserializzazione"
    );
    assert!(DisplayGeometry::new(0, 10).is_err());
    assert!(DisplayGeometry::new(8193, 10).is_err());
}

#[test]
fn text_validation_rejects_host_paths() {
    for bad in [
        "",
        "../x",
        "a/../b",
        "/abs",
        "C:/x",
        "C:x",
        "\\\\server\\share",
        "\\\\?\\C:\\x",
        "\\\\.\\PhysicalDrive0",
        "a\\b",
        "file.iso:stream",
        "CON",
        "nul.iso",
        "com1",
        "LPT9.txt",
        ".hidden",
        "trail.",
        "a//b",
        "a/b/c/d/e",
        "spazio nel nome",
    ] {
        assert!(ManagedRelPath::parse(bad).is_err(), "accettato: {bad:?}");
    }
    for good in ["media/x.iso", "vms/0192/disk.qcow2", "com10.iso", "console"] {
        assert!(ManagedRelPath::parse(good).is_ok(), "rifiutato: {good:?}");
    }
    let long = "x".repeat(65);
    for bad in [
        "",
        " x",
        "x ",
        ".",
        "..",
        "-rf",
        "a/b",
        "a\\b",
        "C:",
        "x\u{0}",
        long.as_str(),
    ] {
        assert!(VmName::parse(bad).is_err(), "nome accettato: {bad:?}");
    }
    assert!(VmName::parse("Ubuntu 24.04_test-1").is_ok());
    assert!(Label::parse("riga\nnuova").is_err());
    assert!(Label::parse("\u{1b}[31mrosso").is_err());
    assert!(QCode::parse("ret").is_ok());
    assert!(QCode::parse("Ret").is_err());
    assert!(QCode::parse("a b").is_err());
    assert!(Sha256Digest::parse_hex(&"A".repeat(64)).is_err());
    assert!(Sha256Digest::parse_hex(&"a".repeat(63)).is_err());
}

#[test]
fn vm_state_table_is_frozen() {
    use VmState::*;
    let expected: [(VmState, &[VmState]); 6] = [
        (Stopped, &[Starting]),
        (Starting, &[Running, Stopping, Failed, Recovering]),
        (Running, &[Stopping, Failed, Recovering]),
        (Stopping, &[Stopped, Failed, Recovering]),
        (Failed, &[Recovering]),
        (Recovering, &[Stopped, Failed]),
    ];
    for (from, to) in expected {
        assert_eq!(from.allowed_transitions(), to, "{from}");
    }
    let names: Vec<_> = VmState::ALL
        .iter()
        .map(|s| serde_json::to_value(s).unwrap())
        .collect();
    assert_eq!(
        names,
        [
            json!("stopped"),
            json!("starting"),
            json!("running"),
            json!("stopping"),
            json!("failed"),
            json!("recovering")
        ]
    );
}

fn any_state() -> impl Strategy<Value = VmState> {
    prop::sample::select(VmState::ALL.to_vec())
}

fn quotas_from(a: [u32; 4], s: u64) -> Quotas {
    Quotas {
        max_operations: a[0],
        max_input_events: a[1],
        max_screenshots: a[2],
        max_screenshot_bytes: s,
        max_queued_operations: a[3],
    }
}

proptest! {
    #[test]
    fn prop_ids_roundtrip(ms in 0u64..(1u64 << 48), rand in any::<[u8; 10]>()) {
        let id = OperationId::from_unix_millis(ms, rand);
        prop_assert_eq!(id.to_string().parse::<OperationId>().unwrap(), id);
        let json = serde_json::to_string(&id).unwrap();
        prop_assert_eq!(serde_json::from_str::<OperationId>(&json).unwrap(), id);
    }

    #[test]
    fn prop_timestamp_roundtrip(ms in -62_000_000_000_000i64..250_000_000_000_000i64) {
        let t = UtcTimestamp::from_unix_millis(ms).unwrap();
        let s = serde_json::to_string(&t).unwrap();
        let back: UtcTimestamp = serde_json::from_str(&s).unwrap();
        prop_assert_eq!(back.as_unix_millis(), ms);
    }

    #[test]
    fn prop_sha256_hex_roundtrip(bytes in any::<[u8; 32]>()) {
        let d = Sha256Digest::from_bytes(bytes);
        prop_assert_eq!(Sha256Digest::parse_hex(&d.to_hex()).unwrap(), d);
    }

    /// Nessuna stringa accettata come percorso gestito contiene separatori o componenti pericolosi.
    #[test]
    fn prop_managed_path_never_escapes(s in "[a-zA-Z0-9./\\\\:_ -]{0,40}") {
        if let Ok(p) = ManagedRelPath::parse(s.clone()) {
            let p = p.as_str();
            prop_assert!(!p.starts_with('/'));
            prop_assert!(!p.contains('\\') && !p.contains(':'));
            prop_assert!(p.split('/').all(|c| !c.is_empty() && c != ".." && c != "."));
        }
    }

    #[test]
    fn prop_pointer_from_pixels_in_range(w in 1u32..=8192, h in 1u32..=8192, px in any::<u32>(), py in any::<u32>()) {
        let g = DisplayGeometry::new(w, h).unwrap();
        let p = PointerMove::from_pixels(px % w, py % h, g).unwrap();
        prop_assert!(p.x <= ABS_AXIS_MAX && p.y <= ABS_AXIS_MAX);
    }

    /// Le transizioni non sono riflessive; `stopped` si raggiunge solo da `stopping`
    /// o `recovering`; `running` solo da `starting`.
    #[test]
    fn prop_transition_table_invariants(a in any_state(), b in any_state()) {
        if a.can_transition_to(b) {
            prop_assert_ne!(a, b);
            if b == VmState::Stopped {
                prop_assert!(matches!(a, VmState::Stopping | VmState::Recovering));
            }
            if b == VmState::Running {
                prop_assert_eq!(a, VmState::Starting);
            }
        }
    }

    #[test]
    fn prop_quotas_intersection_is_lower_bound(a in any::<[u32; 4]>(), b in any::<[u32; 4]>(), sa in any::<u64>(), sb in any::<u64>()) {
        let qa = quotas_from(a, sa);
        let qb = quotas_from(b, sb);
        let i = qa.intersect(&qb);
        prop_assert!(i.is_within(&qa) && i.is_within(&qb));
        prop_assert_eq!(i, qb.intersect(&qa));
    }
}
