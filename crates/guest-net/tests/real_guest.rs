//! Real guest network qualification (ignored; requires the lab).
include!("common/real_guest.rs");
use guest_net::NetRecord;

#[test]
#[ignore = "needs the lab: QEMU, WHPX, the Ubuntu live kernel and ISO"]
fn endpoint_allowlist_with_a_real_linux_guest() {
    let (server, accepted) = line_server("127.0.0.1:0");
    let (decoy_port, decoy_hits) = decoy();
    let host_ip = host_lan_ip().map_or("192.168.101.134".to_owned(), |ip| ip.to_string());
    let sock_dir = lab().join("net").join("s");
    std::fs::create_dir_all(&sock_dir).unwrap();
    let sock = sock_dir.join("net.sock");
    let _ = std::fs::remove_file(&sock);
    let mut g = Guest::boot(
        "allowlist",
        &[
            "-netdev".into(),
            format!(
                "stream,id=n0,server=on,addr.type=unix,addr.path={}",
                sock.display()
            ),
            "-device".into(),
            "e1000,netdev=n0,mac=52:54:00:12:34:56".into(),
        ],
    );
    let link = guest_net::transport::attach(connect_unix(&sock)).unwrap();
    let eps = vec![
        EndpointSpec {
            name: "sql.lan".into(),
            virtual_ip: Ipv4Addr::new(10, 0, 2, 101),
            port: 1433,
            target: server,
        },
        EndpointSpec {
            name: "wcfhost".into(),
            virtual_ip: Ipv4Addr::new(10, 0, 2, 102),
            port: 8733,
            target: server,
        },
    ];
    let mut stack = NetStack::start(NetConfig::new(eps), link, ["sql.lan".to_owned()]).unwrap();
    g.login();
    let lease = wait_dhcp(&mut g);
    let mut ev = format!("== guest addresses and routes\n{lease}\n");
    g.push(PROBE, "/tmp/probe.py");
    // A hostile guest adds the default route the stack never offers.
    g.cmd(
        "sudo ip route add default via 10.0.2.2 2>&1; cat /etc/resolv.conf; resolvectl status 2>&1 | head -20",
        Duration::from_secs(20),
    );
    let out = g.cmd(
        &format!("python3 /tmp/probe.py allow {host_ip} {decoy_port}"),
        Duration::from_secs(400),
    );
    ev.push_str("== allow (sql.lan allowed)\n");
    let r = results(&out);
    for (n, v) in &r {
        ev.push_str(&format!("{n:<42} {v}\n"));
    }
    stack.set_allowed(["sql.lan".to_owned(), "wcfhost".to_owned()]);
    let out2 = g.cmd("python3 /tmp/probe.py second", Duration::from_secs(60));
    let r2 = results(&out2);
    ev.push_str("== second (sql.lan and wcfhost allowed)\n");
    for (n, v) in &r2 {
        ev.push_str(&format!("{n:<42} {v}\n"));
    }
    stack.set_allowed(Vec::<String>::new());
    std::thread::sleep(Duration::from_millis(200));
    let out3 = g.cmd("python3 /tmp/probe.py revoked", Duration::from_secs(60));
    let r3 = results(&out3);
    ev.push_str("== revoked (nothing allowed)\n");
    for (n, v) in &r3 {
        ev.push_str(&format!("{n:<42} {v}\n"));
    }
    let stats = stack.stats();
    stack.stop();
    let recs = stack.take_records();
    ev.push_str(&format!(
        "== stack\n{stats:?}\nhost line-server connections: {}\ndecoy connections: {}\n",
        accepted.load(Ordering::SeqCst),
        decoy_hits.load(Ordering::SeqCst)
    ));
    for rec in &recs {
        match rec {
            NetRecord::Connection(c) => ev.push_str(&format!(
                "conn {} {} -> {} guest:{} up={} down={} {}\n",
                c.endpoint,
                c.virtual_addr,
                c.target,
                c.guest_port,
                c.bytes_from_guest,
                c.bytes_to_guest,
                c.outcome.as_str()
            )),
            NetRecord::Denied(d) => {
                ev.push_str(&format!("denied {} {}\n", d.destination, d.reason.as_str()));
            }
        }
    }
    evidence("allowlist-real.txt", &ev);
    println!("{ev}");
    drop(g);

    assert!(lease.contains("10.0.2.15/24"));
    assert!(!lease.contains("default"), "no default route from DHCP");
    assert_eq!(get(&r, "dns sql.lan"), "ANSWER rcode=0 an=1 ip=10.0.2.101");
    assert_eq!(get(&r, "dns sql.lan AAAA"), "ANSWER rcode=0 an=0 ip=-");
    assert_eq!(
        get(&r, "dns wcfhost (not allowed)"),
        "ANSWER rcode=3 an=0 ip=-"
    );
    assert_eq!(get(&r, "dns example.com"), "ANSWER rcode=3 an=0 ip=-");
    assert!(get(&r, "tcp by name sql.lan:1433").contains("recv=ECHO"));
    assert!(get(&r, "tcp by vip 10.0.2.101:1433").contains("recv=ECHO"));
    assert_eq!(get(&r, "multi 8 concurrent"), "OK");
    assert!(get(&r, "bulk 16MiB").starts_with("OK"));
    for n in [
        "tcp 10.0.2.102:8733 (not allowed)",
        "tcp 10.0.2.101:80 (other port)",
        "tcp gateway 10.0.2.2:decoy",
        "tcp gateway 10.0.2.2:135",
    ] {
        assert_eq!(get(&r, n), "REFUSED", "{n}");
    }
    for n in [
        "tcp host LAN ip:decoy",
        "tcp host LAN ip:135",
        "tcp host LAN ip:445",
        "tcp Internet 1.1.1.1:443",
        "tcp Internet 8.8.8.8:53",
    ] {
        assert!(!get(&r, n).starts_with("CONNECTED"), "{n}");
    }
    for n in ["dns udp 8.8.8.8", "dns udp 1.1.1.1"] {
        assert_eq!(get(&r, n), "TIMEOUT", "{n}");
    }
    for n in [
        "tcp IPv6 [2606:4700:4700::1111]:443",
        "tcp IPv6 [fec0::2]:135",
    ] {
        assert!(!get(&r, n).starts_with("CONNECTED"), "{n}");
    }
    assert_eq!(get(&r, "icmp 1.1.1.1"), "NOREPLY");
    assert_eq!(get(&r2, "dns wcfhost"), "ANSWER rcode=0 an=1 ip=10.0.2.102");
    assert_eq!(
        get(&r2, "dns wcfhost search"),
        "ANSWER rcode=0 an=1 ip=10.0.2.102"
    );
    assert!(get(&r2, "tcp by name wcfhost:8733").contains("recv=ECHO"));
    assert_eq!(get(&r3, "dns sql.lan"), "ANSWER rcode=3 an=0 ip=-");
    assert_eq!(get(&r3, "tcp by vip 10.0.2.101:1433"), "REFUSED");
    assert_eq!(decoy_hits.load(Ordering::SeqCst), 0, "decoy reached");
    assert!(stats.connections >= 12, "{stats:?}");
}

#[test]
#[ignore = "needs the lab: QEMU, WHPX, the Ubuntu live kernel and ISO"]
fn nat_with_a_real_linux_guest() {
    // NAT reaches host services listening on every interface through the
    // gateway address (slirp maps 10.0.2.2 to the host's loopback).
    let (server, _) = line_server("127.0.0.1:0");
    let (decoy_port, decoy_hits) = decoy();
    let mut g = Guest::boot(
        "nat",
        &[
            "-netdev".into(),
            "user,id=n0".into(),
            "-device".into(),
            "e1000,netdev=n0,mac=52:54:00:12:34:56".into(),
        ],
    );
    g.login();
    let lease = wait_dhcp(&mut g);
    g.push(PROBE, "/tmp/probe.py");
    let out = g.cmd(
        &format!("python3 /tmp/probe.py nat {} {decoy_port}", server.port()),
        Duration::from_secs(300),
    );
    let r = results(&out);
    let mut ev = format!("== guest addresses and routes\n{lease}\n== nat\n");
    for (n, v) in &r {
        ev.push_str(&format!("{n:<42} {v}\n"));
    }
    evidence("nat-real.txt", &ev);
    println!("{ev}");
    drop(g);
    assert!(lease.contains("default via 10.0.2.2"));
    assert!(get(&r, "tcp host 10.0.2.2:server").contains("recv=ECHO"));
    assert!(get(&r, "tcp host 10.0.2.2:decoy").starts_with("CONNECTED"));
    assert_eq!(get(&r, "multi 4 concurrent"), "OK");
    assert!(get(&r, "bulk 16MiB").starts_with("OK"));
    assert!(decoy_hits.load(Ordering::SeqCst) >= 1);
    // The Internet row depends on the host's connectivity: recorded only.
}

#[test]
#[ignore = "needs the lab (QEMU, WHPX, Ubuntu live) and host Internet access"]
fn filtered_internet_with_a_real_linux_guest() {
    use std::net::ToSocketAddrs;
    const WEB: &str = "example.com";
    // As the supervisor does at run start (ADR-0037): every IPv4 answer
    // must pass the policy, the first one is pinned for the run.
    let answers: Vec<SocketAddr> = (WEB, 443)
        .to_socket_addrs()
        .unwrap()
        .filter(SocketAddr::is_ipv4)
        .collect();
    assert!(!answers.is_empty(), "no IPv4 answer for {WEB}");
    for a in &answers {
        guest_net::egress::validate_target(*a).unwrap();
    }
    let (loopback, rebind_hits) = line_server("127.0.0.1:0");
    let (decoy_port, decoy_hits) = decoy();
    let host_ip = host_lan_ip().map_or("192.168.101.134".to_owned(), |ip| ip.to_string());
    let sock_dir = lab().join("net").join("s");
    std::fs::create_dir_all(&sock_dir).unwrap();
    let sock = sock_dir.join("filtered.sock");
    let _ = std::fs::remove_file(&sock);
    let mut g = Guest::boot(
        "filtered",
        &[
            "-netdev".into(),
            format!(
                "stream,id=n0,server=on,addr.type=unix,addr.path={}",
                sock.display()
            ),
            "-device".into(),
            "e1000,netdev=n0,mac=52:54:00:12:34:56".into(),
        ],
    );
    let link = guest_net::transport::attach(connect_unix(&sock)).unwrap();
    let eps = vec![
        EndpointSpec {
            name: WEB.into(),
            virtual_ip: Ipv4Addr::new(10, 0, 2, 101),
            port: 443,
            target: answers[0],
        },
        // A name whose (rebound) answer points at this host: the
        // public-only check must refuse it although it is allowed.
        EndpointSpec {
            name: "rebind.example.net".into(),
            virtual_ip: Ipv4Addr::new(10, 0, 2, 102),
            port: 443,
            target: loopback,
        },
    ];
    let mut cfg = NetConfig::new(eps);
    cfg.public_endpoints = [WEB, "rebind.example.net"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    let allowed = [WEB.to_owned(), "rebind.example.net".to_owned()];
    let mut stack = NetStack::start(cfg, link, allowed).unwrap();
    g.login();
    let lease = wait_dhcp(&mut g);
    let mut ev = format!("== pinned {WEB} -> {}\n== guest\n{lease}\n", answers[0]);
    g.push(PROBE, "/tmp/probe.py");
    g.cmd(
        "sudo ip route add default via 10.0.2.2 2>&1",
        Duration::from_secs(20),
    );
    let out = g.cmd(
        &format!("python3 /tmp/probe.py filtered {WEB} {host_ip} {decoy_port}"),
        Duration::from_secs(300),
    );
    let r = results(&out);
    for (n, v) in &r {
        ev.push_str(&format!("{n:<42} {v}\n"));
    }
    // A persistent TLS connection, then revocation of every endpoint.
    g.cmd(
        &format!("rm -f /tmp/go; (python3 /tmp/probe.py persist {WEB} >/tmp/p.out 2>&1 &)"),
        Duration::from_secs(10),
    );
    let t = Instant::now();
    while !g
        .cmd("cat /tmp/p.out", Duration::from_secs(10))
        .contains("persist first")
    {
        assert!(
            t.elapsed() < Duration::from_secs(60),
            "persistent connection"
        );
        std::thread::sleep(Duration::from_secs(1));
    }
    stack.set_allowed(Vec::<String>::new());
    std::thread::sleep(Duration::from_millis(300));
    g.cmd("touch /tmp/go", Duration::from_secs(10));
    let t = Instant::now();
    let persist = loop {
        let o = g.cmd("cat /tmp/p.out", Duration::from_secs(10));
        if o.contains("persist after revoke") {
            break results(&o);
        }
        assert!(t.elapsed() < Duration::from_secs(60), "revoked connection");
        std::thread::sleep(Duration::from_secs(1));
    };
    for (n, v) in &persist {
        ev.push_str(&format!("{n:<42} {v}\n"));
    }
    let stats = stack.stats();
    stack.stop();
    ev.push_str(&format!(
        "== stack\n{stats:?}\nloopback (rebind) connections: {}\ndecoy connections: {}\n",
        rebind_hits.load(Ordering::SeqCst),
        decoy_hits.load(Ordering::SeqCst)
    ));
    for rec in stack.take_records() {
        if let NetRecord::Connection(c) = rec {
            ev.push_str(&format!(
                "conn {} -> {} up={} down={} {}\n",
                c.endpoint,
                c.target,
                c.bytes_from_guest,
                c.bytes_to_guest,
                c.outcome.as_str()
            ));
        }
    }
    evidence("filtered-real.txt", &ev);
    println!("{ev}");
    drop(g);

    assert!(!lease.contains("default"), "no default route from DHCP");
    assert_eq!(get(&r, "dns web"), "ANSWER rcode=0 an=1 ip=10.0.2.101");
    assert_eq!(get(&r, "dns other name"), "ANSWER rcode=3 an=0 ip=-");
    assert!(get(&r, "https web").starts_with("TLS "), "{r:?}");
    assert!(get(&r, "https web").contains(" HTTP/1.1 "), "{r:?}");
    for n in [
        "http web",
        "tcp web:22 (other port)",
        "tcp gateway 10.0.2.2:decoy",
    ] {
        assert_eq!(get(&r, n), "REFUSED", "{n}");
    }
    assert_eq!(get(&r, "dns rebind"), "ANSWER rcode=0 an=1 ip=10.0.2.102");
    // The stack may accept the guest's handshake before its host connect
    // fails; what matters is that no byte of the loopback service comes back
    // (and, below, that it never accepted a connection).
    assert!(
        !get(&r, "tcp rebind:443 (pinned to loopback)").contains("ECHO"),
        "{r:?}"
    );
    for n in [
        "tcp host LAN ip:decoy",
        "tcp Internet 1.1.1.1:443",
        "tcp IPv6 [2606:4700:4700::1111]:443",
    ] {
        assert!(!get(&r, n).starts_with("CONNECTED"), "{n}");
    }
    assert_eq!(get(&r, "dns udp 8.8.8.8"), "TIMEOUT");
    assert_eq!(get(&r, "icmp 1.1.1.1"), "NOREPLY");
    assert!(
        get(&persist, "persist first").starts_with("HTTP/1.1"),
        "{persist:?}"
    );
    assert!(
        !get(&persist, "persist after revoke").starts_with("HTTP/"),
        "{persist:?}"
    );
    assert_eq!(rebind_hits.load(Ordering::SeqCst), 0, "loopback reached");
    assert_eq!(decoy_hits.load(Ordering::SeqCst), 0, "decoy reached");
}
