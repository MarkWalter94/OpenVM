//! Fault injection is compiled only into this crate's test executable.
#![allow(dead_code)] // reusable real-guest fixture covers other network modes
include!("../tests/common/real_guest.rs");
use super::Cmd;
use std::net::ToSocketAddrs;

#[test]
#[ignore = "real Windows/WHPX, approved Ubuntu live lab and Internet access required"]
fn real_filtered_guest_loses_egress_on_isolated_gateway_panic() {
    let answers = ("example.com", 443)
        .to_socket_addrs()
        .unwrap()
        .filter(SocketAddr::is_ipv4)
        .collect::<Vec<_>>();
    assert!(!answers.is_empty());
    for target in &answers {
        crate::egress::validate_target(*target).unwrap();
    }
    let directory = lab().join("net/s");
    std::fs::create_dir_all(&directory).unwrap();
    let tag = format!("filtered-panic-{}", std::process::id());
    let socket = directory.join(format!("{tag}.sock"));
    assert!(!socket.exists(), "use a fresh lab socket");
    let mut guest = Guest::boot(
        &tag,
        &[
            "-netdev".into(),
            format!(
                "stream,id=n0,server=on,addr.type=unix,addr.path={}",
                socket.display()
            ),
            "-device".into(),
            "e1000,netdev=n0,mac=52:54:00:12:34:56".into(),
        ],
    );
    let config = NetConfig {
        public_endpoints: ["example.com".into()].into_iter().collect(),
        ..NetConfig::new(vec![EndpointSpec {
            name: "example.com".into(),
            virtual_ip: Ipv4Addr::new(10, 0, 2, 101),
            port: 443,
            target: answers[0],
        }])
    };
    let mut stack = NetStack::start(
        config,
        guest_net::transport::attach(connect_unix(&socket)).unwrap(),
        ["example.com".into()],
    )
    .unwrap();
    guest.login();
    let lease = wait_dhcp(&mut guest);
    guest.push(PROBE, "/tmp/probe.py");
    guest.cmd(
        "rm -f /tmp/go; (python3 /tmp/probe.py persist example.com >/tmp/p.out 2>&1 &)",
        Duration::from_secs(10),
    );
    let started = Instant::now();
    let before = loop {
        let text = guest.cmd("cat /tmp/p.out", Duration::from_secs(10));
        if text.contains("persist first") {
            break text;
        }
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "persistent HTTPS not ready"
        );
    };
    assert!(
        get(&results(&before), "persist first").contains("200"),
        "{before}"
    );
    assert!(stack.stats().open_connections > 0);
    let crashed = Instant::now();
    stack.cmd.send(Cmd::Panic).unwrap();
    while stack.is_running() {
        assert!(
            crashed.elapsed() < Duration::from_secs(3),
            "gateway still marked alive"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    let closed_ms = crashed.elapsed().as_millis();
    stack.stop();
    assert_eq!(stack.stats().open_connections, 0);
    assert!(
        guest.child.try_wait().unwrap().is_none(),
        "guest must survive isolated gateway failure"
    );
    guest.cmd("touch /tmp/go", Duration::from_secs(10));
    let started = Instant::now();
    let after = loop {
        let text = guest.cmd("cat /tmp/p.out", Duration::from_secs(10));
        if text.contains("persist after revoke") {
            break text;
        }
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "old TLS did not close"
        );
    };
    let value = get(&results(&after), "persist after revoke").to_owned();
    assert!(
        value == "EOF" || value.starts_with("FAIL "),
        "old TLS survived: {value}"
    );
    let fresh = guest.cmd("python3 -c \"import socket; s=socket.socket(); s.settimeout(2); print('NEW_CONNECTION_RESULT',s.connect_ex(('10.0.2.101',443)))\"",Duration::from_secs(10));
    assert!(fresh.contains("NEW_CONNECTION_RESULT "));
    assert!(
        !fresh.contains("NEW_CONNECTION_RESULT 0"),
        "new connection succeeded after gateway panic"
    );
    let report = format!(
        "gateway_closed_ms={closed_ms}\nqemu_survived=true\nopen_connections={}\n== lease\n{lease}\n== before\n{before}\n== after\n{after}\n== fresh connection\n{fresh}\n",
        stack.stats().open_connections
    );
    evidence(&format!("{tag}.txt"), &report);
    println!("{report}");
}
