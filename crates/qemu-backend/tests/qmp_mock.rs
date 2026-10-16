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

/// Simulated QEMU run state for the vCPU fault tests, following the QEMU
/// 11.1 rules: `vm_stop(<fault_state>)` emits `STOP`; `cont` is refused
/// while a reset is needed; a reset from a stopped state moves to
/// `prelaunch`, except from `paused`, which stays paused. The WHPX source of
/// 11.1.0 faults to `paused`, other accelerators to `internal-error`. `faults` faults are injected, the first one right after the
/// handshake and each further one when `cont` resumes the guest. Every
/// command name is recorded in `log`.
fn qemu_with_faults(
    path: &ManagedPath,
    faults: u32,
    log: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
) -> std::thread::JoinHandle<()> {
    qemu_faulting_to(path, faults, "internal-error", log)
}

fn qemu_faulting_to(
    path: &ManagedPath,
    faults: u32,
    fault_state: &'static str,
    log: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
) -> std::thread::JoinHandle<()> {
    serve(path, move |mut c| {
        let mut left = faults;
        let mut status = "running";
        let fault = |c: &mut Conn, status: &mut &str, left: &mut u32| {
            if *left > 0 {
                *left -= 1;
                *status = fault_state;
                c.send(json!({"event": "STOP", "timestamp": {"seconds": 1, "microseconds": 0}}));
            }
        };
        fault(&mut c, &mut status, &mut left);
        while let Some(v) = c.recv() {
            let cmd = v["execute"].as_str().unwrap_or_default().to_owned();
            log.lock().unwrap().push(cmd.clone());
            let needs_reset = matches!(status, "internal-error" | "guest-panicked" | "shutdown");
            match cmd.as_str() {
                "query-status" => c.send(json!({
                    "return": {"status": status, "running": status == "running"},
                    "id": v["id"]
                })),
                "system_reset" => {
                    if !matches!(status, "running" | "paused") {
                        status = "prelaunch";
                    }
                    c.send(json!({"return": {}, "id": v["id"]}));
                    c.send(json!({"event": "RESET", "data": {"guest": false, "reason": "host-qmp-system-reset"},
                        "timestamp": {"seconds": 2, "microseconds": 0}}));
                }
                "cont" if needs_reset => c.send(json!({
                    "error": {"class": "GenericError", "desc": "Resetting the Virtual Machine is required"},
                    "id": v["id"]
                })),
                "cont" => {
                    status = "running";
                    c.send(json!({"return": {}, "id": v["id"]}));
                    fault(&mut c, &mut status, &mut left);
                }
                _ => c.send(json!({"return": {}, "id": v["id"]})),
            }
        }
    })
}

fn count(log: &std::sync::Mutex<Vec<String>>, cmd: &str) -> usize {
    log.lock().unwrap().iter().filter(|c| *c == cmd).count()
}

/// Waits (at most 5 s) for `f`.
async fn until(mut f: impl FnMut() -> bool) {
    let t0 = std::time::Instant::now();
    while !f() {
        assert!(
            t0.elapsed() < Duration::from_secs(5),
            "condition not reached"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[test]
fn internal_error_is_reset_and_resumed() {
    use qemu_backend::ProcessState;
    use qemu_backend::vcpu_fault::{
        VcpuFaultEvent, VcpuFaultPolicy, VcpuFaults, watch_vcpu_faults,
    };
    let path = socket_path("vcpu-reset");
    let log = std::sync::Arc::default();
    let server = qemu_with_faults(&path, 2, std::sync::Arc::clone(&log));
    rt().block_on(async {
        let c = std::sync::Arc::new(
            QmpClient::connect(&path, QmpConfig::default())
                .await
                .unwrap(),
        );
        let (tx, rx) = tokio::sync::watch::channel(ProcessState::Running);
        let faults = std::sync::Arc::new(VcpuFaults::new(VcpuFaultPolicy::default()));
        let w = tokio::spawn(watch_vcpu_faults(c.clone(), rx, faults.clone()));
        let mut events = Vec::new();
        until(|| {
            events.extend(faults.take_events());
            events.len() == 2
        })
        .await;
        let reason = lightvm_proto::ipc::VcpuHaltReason::InternalError;
        assert_eq!(
            events,
            vec![
                VcpuFaultEvent::AutoReset {
                    reason,
                    resets_in_window: 1
                },
                VcpuFaultEvent::AutoReset {
                    reason,
                    resets_in_window: 2
                },
            ]
        );
        assert_eq!(count(&log, "cont"), 2);
        assert_eq!(c.query_status().await.unwrap().status, RunState::Running);
        assert_eq!(faults.halt(), None);
        assert_eq!(count(&log, "system_reset"), 2);
        // `cont` only after the reset happened (never refused).
        let l = log.lock().unwrap().clone();
        let first_reset = l.iter().position(|x| x == "system_reset").unwrap();
        let first_cont = l.iter().position(|x| x == "cont").unwrap();
        assert!(first_reset < first_cont, "{l:?}");
        drop(l);
        // The watcher ends with the process.
        tx.send_replace(ProcessState::Exited {
            code: Some(0),
            cause: qemu_backend::ExitCause::QmpQuit,
        });
        tokio::time::timeout(Duration::from_secs(3), w)
            .await
            .unwrap()
            .unwrap();
        c.close(Duration::from_secs(2)).await;
    });
    server.join().unwrap();
}

#[test]
fn whpx_pause_after_an_unrecoverable_exit_is_reset_and_resumed() {
    use qemu_backend::ProcessState;
    use qemu_backend::vcpu_fault::{
        VcpuFaultEvent, VcpuFaultPolicy, VcpuFaults, watch_vcpu_faults,
    };
    let path = socket_path("vcpu-paused");
    let log = std::sync::Arc::default();
    let server = qemu_faulting_to(&path, 1, "paused", std::sync::Arc::clone(&log));
    rt().block_on(async {
        let c = std::sync::Arc::new(
            QmpClient::connect(&path, QmpConfig::default())
                .await
                .unwrap(),
        );
        let (tx, rx) = tokio::sync::watch::channel(ProcessState::Running);
        let faults = std::sync::Arc::new(VcpuFaults::new(VcpuFaultPolicy::default()));
        let w = tokio::spawn(watch_vcpu_faults(c.clone(), rx, faults.clone()));
        let mut events = Vec::new();
        until(|| {
            events.extend(faults.take_events());
            !events.is_empty()
        })
        .await;
        assert_eq!(
            events,
            vec![VcpuFaultEvent::AutoReset {
                reason: lightvm_proto::ipc::VcpuHaltReason::InternalError,
                resets_in_window: 1
            }]
        );
        assert_eq!(count(&log, "system_reset"), 1);
        assert_eq!(count(&log, "cont"), 1);
        assert_eq!(c.query_status().await.unwrap().status, RunState::Running);
        tx.send_replace(ProcessState::Exited {
            code: Some(0),
            cause: qemu_backend::ExitCause::QmpQuit,
        });
        tokio::time::timeout(Duration::from_secs(3), w)
            .await
            .unwrap()
            .unwrap();
        c.close(Duration::from_secs(2)).await;
    });
    server.join().unwrap();
}

#[test]
fn repeated_internal_error_is_left_halted_and_a_manual_reset_resumes() {
    use lightvm_proto::ipc::{VcpuHalt, VcpuHaltReason};
    use qemu_backend::ProcessState;
    use qemu_backend::vcpu_fault::{
        VcpuFaultEvent, VcpuFaultPolicy, VcpuFaults, reset_and_resume, watch_vcpu_faults,
    };
    let path = socket_path("vcpu-halt");
    let log = std::sync::Arc::default();
    // Three faults, two automatic resets allowed: the third is left halted.
    let server = qemu_with_faults(&path, 3, std::sync::Arc::clone(&log));
    rt().block_on(async {
        let c = std::sync::Arc::new(
            QmpClient::connect(&path, QmpConfig::default())
                .await
                .unwrap(),
        );
        let (_tx, rx) = tokio::sync::watch::channel(ProcessState::Running);
        let faults = std::sync::Arc::new(VcpuFaults::new(VcpuFaultPolicy {
            max_resets: 2,
            window: Duration::from_secs(120),
            reset_guest_panicked: false,
        }));
        let w = tokio::spawn(watch_vcpu_faults(c.clone(), rx, faults.clone()));
        until(|| faults.halt().is_some()).await;
        let halt = VcpuHalt {
            reason: VcpuHaltReason::InternalError,
            automatic_resets: 2,
        };
        assert_eq!(faults.halt(), Some(halt));
        let reset = VcpuFaultEvent::AutoReset {
            reason: VcpuHaltReason::InternalError,
            resets_in_window: 1,
        };
        let events = faults.take_events();
        assert_eq!(events.len(), 3, "{events:?}");
        assert_eq!(events[0], reset);
        assert_eq!(events[2], VcpuFaultEvent::Halted(halt));
        // Left stopped: no third reset.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(count(&log, "system_reset"), 2);
        assert_eq!(
            c.query_status().await.unwrap().status,
            RunState::InternalError
        );
        // The user's reset: reset, then `cont` once the reset happened.
        assert!(reset_and_resume(&c).await.unwrap());
        faults.cleared();
        assert_eq!(c.query_status().await.unwrap().status, RunState::Running);
        assert_eq!(count(&log, "system_reset"), 3);
        assert_eq!(count(&log, "cont"), 3);
        assert_eq!(faults.halt(), None);
        // A reset of a running guest does not send `cont`.
        assert!(!reset_and_resume(&c).await.unwrap());
        assert_eq!(count(&log, "cont"), 3);
        w.abort();
        c.close(Duration::from_secs(2)).await;
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
