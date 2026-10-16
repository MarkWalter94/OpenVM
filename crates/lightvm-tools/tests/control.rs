use lightvm_tools::{
    control::{Command, ControlServer, Reply, Request, Response},
    policy::{Feature, Gate, Grant, Secret},
    protocol::{self, Hello},
};
use std::{
    net::TcpStream,
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

fn setup() -> (Arc<ControlServer>, Gate, Hello) {
    let secret = Secret::generate().unwrap();
    let hello = Hello {
        version: 1,
        vm: "vm".into(),
        run: "run".into(),
        credential: secret.export(),
    };
    let gate = Gate::new(Grant {
        features: [
            Feature::Clipboard,
            Feature::UiInspect,
            Feature::FilesTransfer,
        ]
        .into(),
        ..Grant::default()
    });
    let server =
        ControlServer::start(hello.vm.clone(), hello.run.clone(), secret, gate.clone()).unwrap();
    (Arc::new(server), gate, hello)
}
fn connect(server: &ControlServer, hello: &Hello) -> TcpStream {
    let mut stream = TcpStream::connect(server.address()).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    protocol::write(&mut stream, hello).unwrap();
    let _: lightvm_tools::server::Welcome = protocol::read(&mut stream).unwrap().unwrap();
    let until = Instant::now() + Duration::from_secs(2);
    while !server.connected() {
        assert!(Instant::now() < until);
        std::thread::yield_now();
    }
    stream
}

#[test]
fn clipboard_images_require_clipboard_permission_not_file_permission() {
    use lightvm_tools::protocol::{FileReply, FileRequest, RelativePath};
    let (server, gate, hello) = setup();
    let mut stream = connect(&server, &hello);
    let request = FileRequest::Begin {
        replace: false,
        share: "clipboard-read".into(),
        path: RelativePath::new(vec!["image".into()]).unwrap(),
        size: 0,
        sha256: "0".repeat(64),
    };
    gate.replace(Grant {
        features: [Feature::FilesTransfer].into(),
        ..Default::default()
    });
    assert!(
        server
            .call(Command::ClipboardImage {
                request: request.clone()
            })
            .is_err()
    );
    gate.replace(Grant {
        features: [Feature::Clipboard].into(),
        ..Default::default()
    });
    let guest = std::thread::spawn(move || {
        let request: Request = protocol::read(&mut stream).unwrap().unwrap();
        assert!(matches!(request.command, Command::ClipboardImage { .. }));
        protocol::write(
            &mut stream,
            &Response {
                id: request.id,
                reply: Reply::ClipboardImage {
                    reply: FileReply::Transfer {
                        transfer: "ab".repeat(32),
                    },
                },
            },
        )
        .unwrap();
    });
    assert!(matches!(
        server.call(Command::ClipboardImage { request }).unwrap(),
        Reply::ClipboardImage { .. }
    ));
    guest.join().unwrap();
}

#[test]
fn internal_flush_needs_a_live_run_but_grants_no_file_or_clipboard_access() {
    let (server, gate, hello) = setup();
    let mut stream = connect(&server, &hello);
    gate.replace(Grant::default());
    assert!(server.call(Command::ClipboardRead).is_err());
    let guest = std::thread::spawn(move || {
        let request: Request = protocol::read(&mut stream).unwrap().unwrap();
        assert!(matches!(request.command, Command::Flush));
        protocol::write(
            &mut stream,
            &Response {
                id: request.id,
                reply: Reply::Ok,
            },
        )
        .unwrap();
    });
    assert!(matches!(server.call(Command::Flush).unwrap(), Reply::Ok));
    guest.join().unwrap();
    gate.revoke();
    assert!(server.call(Command::Flush).is_err());
}

#[test]
fn flush_rejects_an_unrelated_guest_reply() {
    let (server, _, hello) = setup();
    let mut stream = connect(&server, &hello);
    let guest = std::thread::spawn(move || {
        let request: Request = protocol::read(&mut stream).unwrap().unwrap();
        protocol::write(
            &mut stream,
            &Response {
                id: request.id,
                reply: Reply::Clipboard {
                    text: "wrong reply".into(),
                },
            },
        )
        .unwrap();
    });
    assert!(server.call(Command::Flush).is_err());
    guest.join().unwrap();
}

#[test]
fn an_idle_control_connection_can_reconnect_after_scope_change() {
    let (server, gate, hello) = setup();
    let _old = connect(&server, &hello);
    server.close_handle().disconnect();
    gate.replace(Grant {
        features: [Feature::Clipboard].into(),
        ..Default::default()
    });
    let mut stream = connect(&server, &hello);
    let guest = std::thread::spawn(move || {
        let request: Request = protocol::read(&mut stream).unwrap().unwrap();
        protocol::write(
            &mut stream,
            &Response {
                id: request.id,
                reply: Reply::Clipboard {
                    text: "new scope".into(),
                },
            },
        )
        .unwrap();
    });
    assert!(
        matches!(server.call(Command::ClipboardRead).unwrap(), Reply::Clipboard { text } if text == "new scope")
    );
    guest.join().unwrap();
}
#[test]
fn clipboard_roundtrip_reply_type_validation_and_no_payload_debug() {
    let (server, _, hello) = setup();
    let mut stream = connect(&server, &hello);
    let guest = std::thread::spawn(move || {
        let req: Request = protocol::read(&mut stream).unwrap().unwrap();
        assert!(matches!(req.command, Command::ClipboardRead));
        protocol::write(
            &mut stream,
            &Response {
                id: req.id,
                reply: Reply::Clipboard {
                    text: "secret text".into(),
                },
            },
        )
        .unwrap();
        let req: Request = protocol::read(&mut stream).unwrap().unwrap();
        protocol::write(
            &mut stream,
            &Response {
                id: req.id,
                reply: Reply::Ok,
            },
        )
        .unwrap();
    });
    let reply = server.call(Command::ClipboardRead).unwrap();
    assert!(matches!(&reply, Reply::Clipboard { text } if text == "secret text"));
    assert!(!format!("{reply:?}").contains("secret text"));
    assert!(
        !format!(
            "{:?}",
            Command::ClipboardWrite {
                text: "secret text".into()
            }
        )
        .contains("secret text")
    );
    assert!(server.call(Command::ClipboardRead).is_err());
    guest.join().unwrap();
}
#[test]
fn revoke_interrupts_waiting_guest_rpc_without_waiting_for_deadline() {
    let (server, gate, hello) = setup();
    let mut stream = connect(&server, &hello);
    let worker = server.clone();
    let call = std::thread::spawn(move || worker.call(Command::ClipboardRead));
    let _: Request = protocol::read(&mut stream).unwrap().unwrap();
    let start = Instant::now();
    gate.revoke();
    server.close_handle().disconnect();
    assert!(call.join().unwrap().is_err());
    assert!(start.elapsed() < Duration::from_secs(1));
    assert!(server.call(Command::ClipboardRead).is_err());
}
#[test]
fn queued_calls_are_not_replayed_after_disconnect() {
    let (server, gate, hello) = setup();
    let mut stream = connect(&server, &hello);
    let (done, recv) = mpsc::channel();
    let mut joins = Vec::new();
    for _ in 0..4 {
        let server = server.clone();
        let done = done.clone();
        joins.push(std::thread::spawn(move || {
            done.send(server.call(Command::ClipboardRead).is_err())
                .unwrap();
        }));
    }
    let _: Request = protocol::read(&mut stream).unwrap().unwrap();
    gate.revoke();
    server.close_handle().disconnect();
    for _ in 0..4 {
        assert!(recv.recv_timeout(Duration::from_secs(2)).unwrap());
    }
    for join in joins {
        join.join().unwrap();
    }
}
#[test]
fn guest_file_requests_cannot_name_host_shares_or_delete_guest_files() {
    use lightvm_tools::protocol::{FileRequest as F, RelativePath as P};
    let path = P::new(vec!["file.txt".into()]).unwrap();
    for command in [
        Command::File {
            request: F::Read {
                share: "host".into(),
                path: path.clone(),
                offset: 0,
                length: 16,
            },
        },
        Command::File {
            request: F::Read {
                share: "outbox".into(),
                path: path.clone(),
                offset: 0,
                length: 65536,
            },
        },
        Command::File {
            request: F::Remove {
                share: "inbox".into(),
                path,
            },
        },
    ] {
        assert!(command.validate().is_err());
    }
}

/// Executes the production Python guest against the production host RPC over
/// real TCP. HOME is isolated; no clipboard or accessibility API is used.
#[cfg(unix)]
#[test]
fn production_python_guest_streams_verified_inbox_and_outbox() {
    use lightvm_tools::protocol::{FileReply as R, FileRequest as F, RelativePath as P};
    use sha2::{Digest, Sha256};
    use std::{
        io::Write,
        process::{Command as Process, Stdio},
    };
    struct Guest {
        child: std::process::Child,
        root: std::path::PathBuf,
    }
    impl Drop for Guest {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
    let (server, _, hello) = setup();
    let root = std::env::temp_dir().join(format!(
        "lightvm-python-{}",
        Secret::generate().unwrap().export()
    ));
    std::fs::create_dir(&root).unwrap();
    let script =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/tools/TOOLS.PY");
    let mut child = Process::new("python3").args(["-B", "-c", "import importlib.machinery, json, sys; m=importlib.machinery.SourceFileLoader('guest',sys.argv[1]).load_module(); m.control_loop(json.load(sys.stdin))"])
        .arg(script).env("HOME", &root).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().expect("Python 3 required for the production guest integration test");
    let config = serde_json::json!({"host":"127.0.0.1", "control_port":server.address().port(), "vm":hello.vm, "run":hello.run, "control_credential":hello.credential});
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(&config).unwrap())
        .unwrap();
    let guest = Guest { child, root };
    let until = Instant::now() + Duration::from_secs(5);
    while !server.connected() {
        assert!(Instant::now() < until, "production guest did not connect");
        std::thread::sleep(Duration::from_millis(10));
    }
    let file = |request| match server.call(Command::File { request }).unwrap() {
        Reply::File { reply } => reply,
        r => panic!("{r:?}"),
    };
    let data = vec![0x57; 2 * 1024 * 1024 + 17];
    let path = P::new(vec!["verified.bin".into()]).unwrap();
    let token = match file(F::Begin {
        replace: false,
        share: "inbox".into(),
        path: path.clone(),
        size: data.len() as u64,
        sha256: protocol::encode(&Sha256::digest(&data)),
    }) {
        R::Transfer { transfer } => transfer,
        r => panic!("{r:?}"),
    };
    for (i, bytes) in data.chunks(16384).enumerate() {
        assert_eq!(
            file(F::Write {
                transfer: token.clone(),
                offset: (i * 16384) as u64,
                data: protocol::encode(bytes)
            }),
            R::Ok
        );
    }
    assert_eq!(file(F::Commit { transfer: token }), R::Ok);
    let docs = guest.root.join("Documents/LightVM Tools");
    assert_eq!(
        std::fs::read(docs.join("inbox/verified.bin")).unwrap(),
        data
    );
    std::fs::write(docs.join("outbox/verified.bin"), &data).unwrap();
    let mut result = Vec::new();
    while result.len() < data.len() {
        match file(F::Read {
            share: "outbox".into(),
            path: path.clone(),
            offset: result.len() as u64,
            length: 16384,
        }) {
            R::Data { offset, data, .. } => {
                assert_eq!(offset, result.len() as u64);
                result.extend(protocol::decode(&data, 16384).unwrap());
            }
            r => panic!("{r:?}"),
        }
    }
    assert_eq!(result, data);
}
