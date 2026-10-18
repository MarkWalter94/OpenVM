use std::collections::BTreeMap;
use std::io::Cursor;
use std::path::PathBuf;

use lightvm_tools::files::{FileService, Share};
use lightvm_tools::policy::{Feature, Gate, Grant, Secret};
use lightvm_tools::protocol::{self, FileReply, FileRequest, Hello, RelativePath};
use sha2::{Digest, Sha256};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "lightvm-tools-{}",
            Secret::generate().unwrap().export()
        ));
        std::fs::create_dir(&p).unwrap();
        // macOS /var is a symlink. Only the test fixture resolves it; the
        // service requires an explicitly resolved, non-link root.
        Self(std::fs::canonicalize(p).unwrap())
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn grant(write: bool) -> Grant {
    Grant {
        features: [Feature::ShareRead, Feature::ShareWrite].into(),
        shares: [("builds".into(), write)].into(),
        ..Grant::default()
    }
}
fn path(s: &str) -> RelativePath {
    RelativePath::new(vec![s.into()]).unwrap()
}
fn service(t: &Temp, write: bool) -> FileService {
    FileService::new(BTreeMap::from([(
        "builds".into(),
        Share::open(&t.0, write).unwrap(),
    )]))
    .unwrap()
}
fn begin(
    s: &mut FileService,
    p: &lightvm_tools::policy::Permit,
    name: &str,
    data: &[u8],
) -> String {
    let r = s.dispatch(
        p,
        FileRequest::Begin {
            replace: false,
            share: "builds".into(),
            path: path(name),
            size: data.len() as u64,
            sha256: protocol::encode(&Sha256::digest(data)),
        },
    );
    match r {
        FileReply::Transfer { transfer } => transfer,
        r => panic!("{r:?}"),
    }
}
fn denied(r: FileReply) {
    assert!(matches!(r, FileReply::Error { .. }), "{r:?}");
}

#[test]
fn wire_limits_truncation_unknown_fields_and_secret_redaction() {
    let mut buf = Vec::new();
    let h = Hello {
        version: protocol::VERSION,
        vm: "vm".into(),
        run: "run".into(),
        credential: "secret".into(),
    };
    protocol::write(&mut buf, &h).unwrap();
    assert!(!format!("{h:?}").contains("secret"));
    assert_eq!(
        protocol::read::<_, Hello>(&mut Cursor::new(&buf))
            .unwrap()
            .unwrap()
            .vm,
        "vm"
    );
    for b in [
        vec![0, 0, 0, 0],
        ((protocol::MAX_FRAME + 1) as u32).to_be_bytes().to_vec(),
        vec![0, 0],
        vec![0, 0, 0, 9, b'{'],
    ] {
        assert!(protocol::read::<_, Hello>(&mut Cursor::new(b)).is_err());
    }
    assert!(
        protocol::read::<_, Hello>(&mut Cursor::new(Vec::new()))
            .unwrap()
            .is_none()
    );
    assert!(protocol::write(&mut Vec::new(), &"x".repeat(protocol::MAX_FRAME)).is_err());
    assert!(
        serde_json::from_str::<Hello>(
            r#"{"version":1,"vm":"v","run":"r","credential":"s","extra":1}"#
        )
        .is_err()
    );
}

#[test]
fn run_identity_and_permission_generation_cannot_be_reused() {
    let secret = Secret::generate().unwrap();
    let mut h = Hello {
        version: 1,
        vm: "a".into(),
        run: "first".into(),
        credential: secret.export(),
    };
    assert!(secret.authenticate(&h, "a", "first"));
    assert!(!secret.authenticate(&h, "b", "first"));
    assert!(!secret.authenticate(&h, "a", "second"));
    h.credential.replace_range(..2, "00");
    // An unrelated credential is rejected irrespective of the run identity.
    h.credential = Secret::generate().unwrap().export();
    assert!(!secret.authenticate(&h, "a", "first"));
    let gate = Gate::new(grant(true));
    let old = gate.permit();
    gate.replace(grant(true));
    assert!(
        old.with(Feature::ShareRead, Some(("builds", false)), || Ok(()))
            .is_err()
    );
    let new = gate.permit();
    gate.revoke();
    assert!(
        new.with(Feature::ShareRead, Some(("builds", false)), || Ok(()))
            .is_err()
    );
}

#[test]
fn expiry_and_revocation_do_not_wait_for_an_inflight_operation() {
    use std::sync::mpsc;
    use std::time::{Duration, Instant};
    let gate = Gate::new(Grant {
        expires: Some(Instant::now() - Duration::from_secs(1)),
        ..grant(true)
    });
    assert!(
        gate.permit()
            .with(Feature::ShareRead, Some(("builds", false)), || Ok(()))
            .is_err()
    );
    gate.replace(grant(true));
    let p = gate.permit();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        p.with(Feature::ShareWrite, Some(("builds", true)), || {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            assert!(p.revoked());
            Ok(())
        })
    });
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let start = Instant::now();
    gate.revoke();
    assert!(start.elapsed() < Duration::from_millis(100));
    release_tx.send(()).unwrap();
    worker.join().unwrap().unwrap();
    assert!(
        gate.permit()
            .with(Feature::ShareRead, Some(("builds", false)), || Ok(()))
            .is_err()
    );
}

#[test]
fn cancellation_does_not_refund_byte_budget_and_file_count_is_bounded() {
    use lightvm_tools::files::Limits;
    let t = Temp::new();
    let shares = BTreeMap::from([("builds".into(), Share::open(&t.0, true).unwrap())]);
    let mut s = FileService::with_limits(
        shares,
        Limits {
            file_bytes: 8,
            total_bytes: 5,
            files: 2,
        },
    )
    .unwrap();
    let gate = Gate::new(grant(true));
    let p = gate.permit();
    let id = begin(&mut s, &p, "first", b"abcd");
    assert!(matches!(
        s.dispatch(
            &p,
            FileRequest::Write {
                transfer: id.clone(),
                offset: 0,
                data: protocol::encode(b"abcd")
            }
        ),
        FileReply::Ok
    ));
    assert!(matches!(
        s.dispatch(&p, FileRequest::Cancel { transfer: id }),
        FileReply::Ok
    ));
    denied(s.dispatch(
        &p,
        FileRequest::Begin {
            replace: false,
            share: "builds".into(),
            path: path("too-large"),
            size: 2,
            sha256: protocol::encode(&Sha256::digest(b"xx")),
        },
    ));
    let id = begin(&mut s, &p, "last", b"x");
    assert!(matches!(
        s.dispatch(&p, FileRequest::Cancel { transfer: id }),
        FileReply::Ok
    ));
    denied(s.dispatch(
        &p,
        FileRequest::Begin {
            replace: false,
            share: "builds".into(),
            path: path("too-many"),
            size: 0,
            sha256: protocol::encode(&Sha256::digest([])),
        },
    ));
    assert_eq!(std::fs::read_dir(&t.0).unwrap().count(), 0);
}

#[test]
fn debug_formatting_never_includes_file_payloads() {
    let marker = protocol::encode(b"private contents");
    let request = FileRequest::Write {
        transfer: "id".into(),
        offset: 0,
        data: marker.clone(),
    };
    let reply = FileReply::Data {
        offset: 0,
        data: marker.clone(),
        eof: true,
    };
    assert!(!format!("{request:?}").contains(&marker));
    assert!(!format!("{reply:?}").contains(&marker));
}

#[test]
fn path_rules_also_apply_to_deserialized_input() {
    for name in [
        "..", ".", "", "/etc", "a/b", "a\\b", "a:stream", "CON", "COM1.txt", "nul.txt", "a.", "a ",
        "x\0y", "<script>",
    ] {
        assert!(RelativePath::new(vec![name.into()]).is_err(), "{name:?}");
    }
    let t = Temp::new();
    let mut s = service(&t, true);
    let g = Gate::new(grant(true));
    let malicious: RelativePath = serde_json::from_str(r#"["..","outside"]"#).unwrap();
    denied(s.dispatch(
        &g.permit(),
        FileRequest::Read {
            share: "builds".into(),
            path: malicious,
            offset: 0,
            length: 1,
        },
    ));
}

#[test]
fn streaming_transfer_hash_length_order_and_create_only_publication() {
    let t = Temp::new();
    let mut s = service(&t, true);
    let g = Gate::new(grant(true));
    let p = g.permit();
    let data = vec![0xa7; protocol::MAX_CHUNK * 3 + 11];
    let id = begin(&mut s, &p, "build.zip", &data);
    assert!(!t.0.join("build.zip").exists());
    for (i, chunk) in data.chunks(protocol::MAX_CHUNK).enumerate() {
        assert!(matches!(
            s.dispatch(
                &p,
                FileRequest::Write {
                    transfer: id.clone(),
                    offset: (i * protocol::MAX_CHUNK) as u64,
                    data: protocol::encode(chunk)
                }
            ),
            FileReply::Ok
        ));
    }
    assert!(matches!(
        s.dispatch(&p, FileRequest::Commit { transfer: id }),
        FileReply::Ok
    ));
    assert_eq!(std::fs::read(t.0.join("build.zip")).unwrap(), data);
    let id = begin(&mut s, &p, "build.zip", b"");
    denied(s.dispatch(&p, FileRequest::Commit { transfer: id }));
    assert_eq!(std::fs::read(t.0.join("build.zip")).unwrap(), data);
    let id = begin(&mut s, &p, "wrong.zip", b"abcd");
    assert!(matches!(
        s.dispatch(
            &p,
            FileRequest::Write {
                transfer: id.clone(),
                offset: 0,
                data: protocol::encode(b"xxxx")
            }
        ),
        FileReply::Ok
    ));
    denied(s.dispatch(&p, FileRequest::Commit { transfer: id }));
    assert!(!t.0.join("wrong.zip").exists());
    let id = begin(&mut s, &p, "order.zip", b"abcd");
    denied(s.dispatch(
        &p,
        FileRequest::Write {
            transfer: id,
            offset: 1,
            data: protocol::encode(b"abcd"),
        },
    ));
    assert!(!t.0.join("order.zip").exists());
    assert!(std::fs::read_dir(&t.0).unwrap().all(|e| {
        !e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".lightvm-transfer-")
    }));
}

#[test]
fn readonly_unknown_share_revoke_and_cancel_never_publish_partial_files() {
    let t = Temp::new();
    let mut s = service(&t, true);
    let gate = Gate::new(grant(false));
    let readonly = gate.permit();
    denied(s.dispatch(
        &readonly,
        FileRequest::Begin {
            replace: false,
            share: "builds".into(),
            path: path("denied"),
            size: 0,
            sha256: protocol::encode(&Sha256::digest([])),
        },
    ));
    denied(s.dispatch(
        &readonly,
        FileRequest::List {
            share: "arbitrary-host-folder".into(),
            path: RelativePath::new(vec![]).unwrap(),
        },
    ));
    gate.replace(grant(true));
    let p = gate.permit();
    let id = begin(&mut s, &p, "revoked", b"abcd");
    gate.revoke();
    denied(s.dispatch(
        &p,
        FileRequest::Write {
            transfer: id,
            offset: 0,
            data: protocol::encode(b"abcd"),
        },
    ));
    assert_eq!(std::fs::read_dir(&t.0).unwrap().count(), 0);
    gate.replace(grant(true));
    let p = gate.permit();
    let _ = begin(&mut s, &p, "cancelled", b"abcd");
    s.cancel_all();
    assert_eq!(std::fs::read_dir(&t.0).unwrap().count(), 0);
    let mut readonly_service = service(&t, false);
    denied(readonly_service.dispatch(
        &p,
        FileRequest::Begin {
            replace: false,
            share: "builds".into(),
            path: path("denied"),
            size: 0,
            sha256: protocol::encode(&Sha256::digest([])),
        },
    ));
}

#[cfg(unix)]
#[test]
fn symlink_roots_intermediate_components_and_files_are_refused() {
    use std::os::unix::fs::symlink;
    let t = Temp::new();
    let outside = Temp::new();
    std::fs::write(outside.0.join("secret"), b"secret").unwrap();
    symlink(&outside.0, t.0.join("escape")).unwrap();
    symlink(outside.0.join("secret"), t.0.join("secret")).unwrap();
    assert!(Share::open(&t.0.join("escape"), false).is_err());
    let mut s = service(&t, true);
    let gate = Gate::new(grant(true));
    let p = gate.permit();
    for parts in [
        vec!["escape".into(), "secret".into()],
        vec!["secret".into()],
    ] {
        denied(s.dispatch(
            &p,
            FileRequest::Read {
                share: "builds".into(),
                path: RelativePath::new(parts).unwrap(),
                offset: 0,
                length: 1024,
            },
        ));
    }
    let list = s.dispatch(
        &p,
        FileRequest::List {
            share: "builds".into(),
            path: RelativePath::new(vec![]).unwrap(),
        },
    );
    assert!(matches!(list, FileReply::Entries { entries } if entries.is_empty()));
}

#[cfg(unix)]
#[test]
fn replacing_a_share_path_cannot_redirect_an_open_share() {
    use std::os::unix::fs::symlink;
    let parent = Temp::new();
    let outside = Temp::new();
    let root = parent.0.join("root");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("file"), b"approved").unwrap();
    std::fs::write(outside.0.join("file"), b"secret").unwrap();
    let share = Share::open(&root, false).unwrap();
    std::fs::rename(&root, parent.0.join("moved")).unwrap();
    symlink(&outside.0, &root).unwrap();
    let mut s = FileService::new(BTreeMap::from([("builds".into(), share)])).unwrap();
    let gate = Gate::new(grant(false));
    let r = s.dispatch(
        &gate.permit(),
        FileRequest::Read {
            share: "builds".into(),
            path: path("file"),
            offset: 0,
            length: 100,
        },
    );
    assert!(matches!(r, FileReply::Data { data, .. } if data == protocol::encode(b"approved")));
}

#[test]
fn guest_cannot_address_host_staging_names() {
    let t = Temp::new();
    let journal_dir = Temp::new();
    let mut s = service(&t, true)
        .with_journal(lightvm_tools::journal::Journal::open(&journal_dir.0).unwrap())
        .unwrap();
    let gate = Gate::new(grant(true));
    let p = gate.permit();
    let data = b"unverified";
    let id = begin(&mut s, &p, "final.bin", data);
    assert!(matches!(
        s.dispatch(
            &p,
            FileRequest::Write {
                transfer: id.clone(),
                offset: 0,
                data: protocol::encode(data)
            }
        ),
        FileReply::Ok
    ));
    let staging = format!(".lightvm-transfer-{id}");
    assert!(t.0.join(&staging).is_file());
    for name in [staging.clone(), staging.to_ascii_uppercase()] {
        let share = || "builds".to_string();
        for request in [
            FileRequest::Remove {
                share: share(),
                path: path(&name),
            },
            FileRequest::Mkdir {
                share: share(),
                path: path(&name),
            },
            FileRequest::Rename {
                share: share(),
                from: path(&name),
                to: path("final.bin"),
                replace: true,
            },
            FileRequest::Rename {
                share: share(),
                from: path("final.bin"),
                to: path(&name),
                replace: true,
            },
            FileRequest::Read {
                share: share(),
                path: path(&name),
                offset: 0,
                length: 16,
            },
            FileRequest::List {
                share: share(),
                path: path(&name),
            },
            FileRequest::Begin {
                replace: false,
                share: share(),
                path: RelativePath::new(vec![name.clone(), "inner".into()]).unwrap(),
                size: 0,
                sha256: protocol::encode(&Sha256::digest([])),
            },
        ] {
            denied(s.dispatch(&p, request));
        }
    }
    assert!(t.0.join(&staging).is_file());
    assert!(!t.0.join("final.bin").exists());
    assert!(matches!(
        s.dispatch(&p, FileRequest::Cancel { transfer: id }),
        FileReply::Ok
    ));
    // Cleanup removed the staging file and its journal record.
    assert_eq!(std::fs::read_dir(&t.0).unwrap().count(), 0);
    assert_eq!(std::fs::read_dir(&journal_dir.0).unwrap().count(), 0);
}

#[test]
fn large_listings_are_bounded_to_one_frame() {
    let t = Temp::new();
    let long = "x".repeat(246);
    for i in 0..protocol::MAX_ENTRIES {
        std::fs::write(t.0.join(format!("{i:04}{long}")), b"").unwrap();
    }
    let mut s = service(&t, false);
    let gate = Gate::new(grant(false));
    let reply = s.dispatch(
        &gate.permit(),
        FileRequest::List {
            share: "builds".into(),
            path: RelativePath::new(vec![]).unwrap(),
        },
    );
    let FileReply::Entries { entries } = &reply else {
        panic!("{reply:?}");
    };
    assert!(!entries.is_empty() && entries.len() < protocol::MAX_ENTRIES);
    // The sorted head is kept.
    assert!(
        entries
            .iter()
            .enumerate()
            .all(|(i, e)| e.name == format!("{i:04}{long}"))
    );
    let mut frame = Vec::new();
    protocol::write(&mut frame, &reply).unwrap();
    assert!(frame.len() - 4 <= protocol::MAX_FRAME);
}
