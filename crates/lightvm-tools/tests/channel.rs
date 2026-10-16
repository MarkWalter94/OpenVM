use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use lightvm_tools::files::{FileService, Share};
use lightvm_tools::policy::{Feature, Gate, Grant, Secret};
use lightvm_tools::protocol::{self, FileReply, FileRequest, Hello, RelativePath};
use lightvm_tools::server::{FileClient, FileServer};
use lightvm_tools::transfer;
use sha2::{Digest, Sha256};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "lightvm-tools-channel-{}",
            Secret::generate().unwrap().export()
        ));
        std::fs::create_dir(&p).unwrap();
        Self(std::fs::canonicalize(p).unwrap())
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn path(name: &str) -> RelativePath {
    RelativePath::new(vec![name.into()]).unwrap()
}
fn setup(root: &Temp) -> (FileServer, Hello, Gate) {
    let secret = Secret::generate().unwrap();
    let hello = Hello {
        version: 1,
        vm: "vm-a".into(),
        run: "run-1".into(),
        credential: secret.export(),
    };
    let gate = Gate::new(Grant {
        features: [Feature::ShareRead, Feature::ShareWrite].into(),
        shares: [("builds".into(), true)].into(),
        ..Grant::default()
    });
    let files = FileService::new(BTreeMap::from([(
        "builds".into(),
        Share::open(&root.0, true).unwrap(),
    )]))
    .unwrap();
    (
        FileServer::start(
            hello.vm.clone(),
            hello.run.clone(),
            secret,
            gate.clone(),
            files,
        )
        .unwrap(),
        hello,
        gate,
    )
}

#[test]
fn idle_scope_change_releases_the_old_connection_and_accepts_a_new_one() {
    let root = Temp::new();
    let (mut server, hello, gate) = setup(&root);
    let mut old = FileClient::connect(server.address(), &hello).unwrap();
    let start = Instant::now();
    server.stop_handle().suspend();
    gate.replace(Grant {
        features: [Feature::ShareRead].into(),
        shares: [("builds".into(), false)].into(),
        ..Default::default()
    });
    let mut new = FileClient::connect(server.address(), &hello).unwrap();
    assert!(start.elapsed() < Duration::from_secs(1));
    let list = || FileRequest::List {
        share: "builds".into(),
        path: RelativePath::new(vec![]).unwrap(),
    };
    assert!(old.call(&list()).is_err());
    assert!(matches!(
        new.call(&list()).unwrap(),
        FileReply::Entries { .. }
    ));
    server.stop();
}

#[test]
fn real_tcp_upload_download_over_fifty_mb_and_cancel_cleanup() {
    let root = Temp::new();
    let guest = Temp::new();
    let (mut server, hello, _) = setup(&root);
    let mut client = FileClient::connect(server.address(), &hello).unwrap();
    // Exceeds the Windows WebDAV redirector's default 50 MB limit. The
    // native file channel is independently streaming and has no such cap.
    let size = 52 * 1024 * 1024 + 17;
    let source = guest.0.join("source.zip");
    let mut file = File::create(&source).unwrap();
    let chunk = vec![0xa7; protocol::MAX_CHUNK];
    for _ in 0..size / chunk.len() {
        file.write_all(&chunk).unwrap();
    }
    file.write_all(&chunk[..size % chunk.len()]).unwrap();
    drop(file);
    let mut hash = Sha256::new();
    let mut source_file = File::open(&source).unwrap();
    let mut buf = vec![0; protocol::MAX_CHUNK];
    loop {
        let n = source_file.read(&mut buf).unwrap();
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    let digest = protocol::encode(&hash.finalize());
    let mut progress = 0;
    transfer::upload(
        &mut client,
        &mut source_file,
        "builds",
        path("build.zip"),
        &AtomicBool::new(false),
        |done, total| {
            assert_eq!(total, size as u64);
            assert!(done >= progress);
            progress = done;
        },
    )
    .unwrap();
    assert_eq!(progress, size as u64);
    let destination = guest.0.join("received.zip");
    transfer::download(
        &mut client,
        "builds",
        path("build.zip"),
        &destination,
        &transfer::Manifest {
            size: size as u64,
            sha256: digest,
        },
        &AtomicBool::new(false),
        |_, _| {},
    )
    .unwrap();
    assert_eq!(std::fs::metadata(&destination).unwrap().len(), size as u64);
    let cancel = AtomicBool::new(false);
    let r = transfer::upload(
        &mut client,
        &mut source_file,
        "builds",
        path("cancelled.zip"),
        &cancel,
        |done, _| {
            if done > 0 {
                cancel.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        },
    );
    assert_eq!(r.unwrap_err().kind(), std::io::ErrorKind::Interrupted);
    assert!(!root.0.join("cancelled.zip").exists());
    assert_eq!(std::fs::read_dir(&root.0).unwrap().count(), 1);
    server.stop();
    assert!(
        server
            .take_records()
            .iter()
            .any(|r| r.outcome == "completed" && r.bytes == size as u64)
    );
}

#[test]
fn wrong_run_rejected_and_stop_interrupts_half_written_handshake() {
    let root = Temp::new();
    let (mut server, mut hello, _) = setup(&root);
    hello.run = "previous-run".into();
    assert!(FileClient::connect(server.address(), &hello).is_err());
    let mut blocked = TcpStream::connect(server.address()).unwrap();
    blocked.write_all(&[0, 1]).unwrap();
    std::thread::sleep(Duration::from_millis(30));
    let start = Instant::now();
    server.stop();
    assert!(start.elapsed() < Duration::from_secs(1));
}

#[test]
fn disconnect_and_revoke_cancel_staging_files() {
    let root = Temp::new();
    let (mut server, hello, gate) = setup(&root);
    let mut client = FileClient::connect(server.address(), &hello).unwrap();
    let reply = client
        .call(&FileRequest::Begin {
            replace: false,
            share: "builds".into(),
            path: path("unfinished"),
            size: 1,
            sha256: protocol::encode(&Sha256::digest(b"x")),
        })
        .unwrap();
    let FileReply::Transfer { transfer } = reply else {
        panic!("{reply:?}")
    };
    gate.revoke();
    assert!(matches!(
        client
            .call(&FileRequest::Write {
                transfer,
                offset: 0,
                data: "78".into()
            })
            .unwrap(),
        FileReply::Error { .. }
    ));
    drop(client);
    server.stop();
    assert_eq!(std::fs::read_dir(&root.0).unwrap().count(), 0);
    let connection = TcpStream::connect(server.address());
    assert!(connection.is_err());
}

#[test]
fn a_truncated_frame_is_not_treated_as_a_new_message() {
    let root = Temp::new();
    let (mut server, hello, _) = setup(&root);
    let mut socket = TcpStream::connect(server.address()).unwrap();
    protocol::write(&mut socket, &hello).unwrap();
    let _: lightvm_tools::server::Welcome = protocol::read(&mut socket).unwrap().unwrap();
    socket.write_all(&[0, 0, 0, 50, b'{']).unwrap();
    socket.shutdown(Shutdown::Write).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    let mut b = [0; 1];
    assert!(matches!(socket.read(&mut b), Ok(0) | Err(_)));
    server.stop();
    assert_eq!(std::fs::read_dir(&root.0).unwrap().count(), 0);
}
