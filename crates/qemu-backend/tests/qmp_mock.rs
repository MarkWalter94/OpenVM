//! Synthetic tests of the QMP protocol against a simulated server on a Unix socket
//! (allowed for errors and protocol by the F1 exit criterion; they do not replace
//! the tests with real QEMU in `real_qemu.rs`).

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::time::Duration;

use qemu_backend::ManagedPath;
use qemu_backend::qmp::{ConnectionState, EventKind, QmpClient, QmpConfig, QmpError, RunState};
use serde_json::{Value, json};
use socket2::{Domain, SockAddr, Socket, Type};

fn socket_path(name: &str) -> ManagedPath {
    let dir = std::env::temp_dir().join("lightvm-qmp-mock");
    std::fs::create_dir_all(&dir).unwrap();
    let p: PathBuf = dir.join(format!("{name}-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&p);
    ManagedPath::verify(p).unwrap()
}

struct Conn {
    r: BufReader<Socket>,
    w: Socket,
}

impl Conn {
    fn send(&mut self, v: Value) {
        let mut s = v.to_string();
        s.push_str("\r\n");
        self.w.write_all(s.as_bytes()).unwrap();
    }
    fn recv(&mut self) -> Option<Value> {
        let mut line = String::new();
        if self.r.read_line(&mut line).ok()? == 0 {
            return None;
        }
        serde_json::from_str(&line).ok()
    }
}

fn serve(path: &ManagedPath, f: impl FnOnce(Conn) + Send + 'static) -> std::thread::JoinHandle<()> {
    let l = Socket::new(Domain::UNIX, Type::STREAM, None).unwrap();
    l.bind(&SockAddr::unix(path.as_path()).unwrap()).unwrap();
    l.listen(1).unwrap();
    std::thread::spawn(move || {
        let (s, _) = l.accept().unwrap();
        let w = s.try_clone().unwrap();
        let mut c = Conn {
            r: BufReader::new(s),
            w,
        };
        c.send(
            json!({"QMP": {"version": {"qemu": {"micro": 0, "minor": 1, "major": 11},
                "package": "mock"}, "capabilities": ["oob"]}}),
        );
        let neg = c.recv().unwrap();
        assert_eq!(neg["execute"], "qmp_capabilities");
        assert_eq!(neg["arguments"]["enable"], json!(["oob"]));
        c.send(json!({"return": {}, "id": neg["id"]}));
        f(c);
    })
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
}

#[test]
fn out_of_order_responses_events_and_errors() {
    let path = socket_path("order");
    let server = serve(&path, |mut c| {
        let a = c.recv().unwrap();
        let b = c.recv().unwrap();
        assert!(a["id"].is_u64() && b["id"].is_u64() && a["id"] != b["id"]);
        // event before the responses, responses in reverse order
        c.send(json!({"timestamp": {"seconds": 1, "microseconds": 2}, "event": "POWERDOWN"}));
        let (status, accel) = if a["execute"] == "query-status" {
            (a, b)
        } else {
            (b, a)
        };
        c.send(
            json!({"return": {"enabled": "whpx", "present": ["tcg", "whpx"]}, "id": accel["id"]}),
        );
        c.send(json!({"return": {"status": "running", "running": true}, "id": status["id"]}));
        let r = c.recv().unwrap();
        assert_eq!(r["execute"], "system_reset");
        c.send(json!({"error": {"class": "GenericError", "desc": "no"}, "id": r["id"]}));
        // response without id and unknown id: discarded
        c.send(json!({"return": {}}));
        c.send(json!({"return": {}, "id": 999_999}));
        let q = c.recv().unwrap();
        assert_eq!(q["execute"], "quit");
        c.send(
            json!({"event": "SHUTDOWN", "data": {"guest": false, "reason": "host-qmp-quit"},
                      "timestamp": {"seconds": 3, "microseconds": 0}}),
        );
        c.send(json!({"return": {}, "id": q["id"]}));
    });
    rt().block_on(async {
        let c = QmpClient::connect(&path, QmpConfig::default())
            .await
            .unwrap();
        assert!(c.oob_enabled());
        assert_eq!(c.greeting().version.major, 11);
        let mut ev = c.subscribe_events();
        let (s, a) = tokio::join!(c.query_status(), c.query_accelerators());
        assert_eq!(s.unwrap().status, RunState::Running);
        assert_eq!(a.unwrap().enabled, "whpx");
        let e = ev.recv().await.unwrap();
        assert_eq!(e.kind(), EventKind::Powerdown);
        match c.system_reset().await {
            Err(QmpError::Command { class, command, .. }) => {
                assert_eq!(class, "GenericError");
                assert_eq!(command, "system_reset");
            }
            other => panic!("{other:?}"),
        }
        c.quit().await.unwrap();
        let e = ev.recv().await.unwrap();
        assert!(matches!(e.kind(), EventKind::Shutdown { guest: false, .. }));
    });
    server.join().unwrap();
}

#[test]
fn per_command_timeout_and_disconnection() {
    let path = socket_path("timeout");
    let server = serve(&path, |mut c| {
        // does not answer the first command
        let _ignored = c.recv().unwrap();
        // answers the second
        let b = c.recv().unwrap();
        c.send(json!({"return": {"qemu": {"major": 11, "minor": 1, "micro": 0}, "package": "p"}, "id": b["id"]}));
        // third command: closes the connection without answering
        let _ = c.recv();
        drop(c);
    });
    rt().block_on(async {
        let cfg = QmpConfig {
            command_timeout: Duration::from_millis(300),
            ..QmpConfig::default()
        };
        let c = QmpClient::connect(&path, cfg).await.unwrap();
        match c.query_status().await {
            Err(QmpError::Timeout { command }) => assert_eq!(command, "query-status"),
            other => panic!("{other:?}"),
        }
        assert_eq!(c.query_version().await.unwrap().qemu.minor, 1);
        assert!(matches!(
            c.query_name().await,
            Err(QmpError::Disconnected) | Err(QmpError::Timeout { .. })
        ));
        let mut st = c.subscribe_state();
        let _ = tokio::time::timeout(
            Duration::from_secs(2),
            st.wait_for(|s| matches!(s, ConnectionState::Closed { .. })),
        )
        .await
        .unwrap();
        assert!(matches!(
            c.query_status().await,
            Err(QmpError::Disconnected)
        ));
    });
    server.join().unwrap();
}

#[test]
fn invalid_greeting_rejected() {
    let path = socket_path("greeting");
    let l = Socket::new(Domain::UNIX, Type::STREAM, None).unwrap();
    l.bind(&SockAddr::unix(path.as_path()).unwrap()).unwrap();
    l.listen(1).unwrap();
    let server = std::thread::spawn(move || {
        let (mut s, _) = l.accept().unwrap();
        s.write_all(b"{\"hello\": 1}\n").unwrap();
        std::thread::sleep(Duration::from_millis(200));
    });
    rt().block_on(async {
        assert!(matches!(
            QmpClient::connect(&path, QmpConfig::default()).await,
            Err(QmpError::Protocol(_))
        ));
    });
    server.join().unwrap();
}

#[test]
fn local_close() {
    let path = socket_path("close");
    let server = serve(&path, |mut c| {
        // answers any command until the client closes
        while let Some(v) = c.recv() {
            c.send(json!({"return": {"status": "running", "running": true}, "id": v["id"]}));
        }
    });
    rt().block_on(async {
        let c = QmpClient::connect(&path, QmpConfig::default())
            .await
            .unwrap();
        c.query_status().await.unwrap();
        c.close(Duration::from_secs(2)).await;
        assert!(matches!(c.query_status().await, Err(QmpError::Closed)));
        assert!(matches!(c.state(), ConnectionState::Closed { .. }));
    });
    server.join().unwrap();
}
