//! Integration coverage of the production guest network.
include!("common/stack_guest.rs");

use guest_net::{ConnOutcome, DenyReason};
use smoltcp::socket::dhcpv4;
use smoltcp::wire::Ipv4Cidr;
use std::collections::BTreeSet;
use std::net::SocketAddrV4;

#[test]
fn dhcp_gives_the_fixed_lease_without_a_router() {
    let (sql, _) = line_server();
    let (stack, mut g) = start(NetConfig::new(endpoints(sql, sql)), &["sql.lan"]);
    let h = g.sockets.add(dhcpv4::Socket::new());
    let mut lease = None;
    g.until("DHCP lease", |g| {
        if let Some(dhcpv4::Event::Configured(c)) =
            g.sockets.get_mut::<dhcpv4::Socket<'static>>(h).poll()
        {
            lease = Some((c.address, c.router, c.dns_servers.to_vec()));
        }
        lease.is_some()
    });
    let (address, router, dns) = lease.unwrap();
    assert_eq!(address, Ipv4Cidr::new(GUEST_IP, 24));
    assert_eq!(router, None, "no default route is offered");
    assert_eq!(dns, vec![DNS_IP]);
    assert!(stack.stats().dhcp_replies >= 2, "offer and ack");
}

#[test]
fn dns_resolves_only_allowed_endpoint_names() {
    let (sql, _) = line_server();
    let (stack, mut g) = start(NetConfig::new(endpoints(sql, sql)), &["sql.lan"]);
    g.configure();
    assert_eq!(g.dns("sql.lan", 1), Some((0, vec![VIP_SQL])));
    assert_eq!(g.dns("SQL.LAN", 1), Some((0, vec![VIP_SQL])));
    assert_eq!(
        g.dns("sql.lan.lightvm.internal", 1),
        Some((0, vec![VIP_SQL]))
    );
    assert_eq!(g.dns("sql.lan", 28), Some((0, vec![])), "AAAA: no data");
    // Configured but not allowed, and unknown names: NXDOMAIN.
    assert_eq!(g.dns("wcfhost", 1), Some((3, vec![])));
    assert_eq!(g.dns("example.com", 1), Some((3, vec![])));
    stack.set_allowed(["sql.lan".to_owned(), "WCFHOST".to_owned()]);
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(g.dns("wcfhost", 1), Some((0, vec![VIP_WCF])));
    assert_eq!(
        g.dns("wcfhost.lightvm.internal", 1),
        Some((0, vec![VIP_WCF]))
    );
    let s = stack.stats();
    assert_eq!(s.dns_answered, 5);
    assert_eq!(s.dns_refused, 3);
}

#[test]
fn concurrent_connections_each_get_their_own_host_connection() {
    let (sql, accepted) = line_server();
    let (stack, mut g) = start(NetConfig::new(endpoints(sql, sql)), &["sql.lan"]);
    g.configure();
    let hs: Vec<SocketHandle> = (0..6).map(|_| g.connect(VIP_SQL, 1433)).collect();
    for &h in &hs {
        g.established(h);
    }
    // Send in reverse order; each reply carries the host connection number
    // and must come back on the socket that sent the line.
    let mut ids = BTreeSet::new();
    for (i, &h) in hs.iter().enumerate().rev() {
        g.send_all(h, format!("HELLO {i}\n").as_bytes());
        let mut line = Vec::new();
        g.until("reply", |g| {
            let s = g.tcp(h);
            if s.can_recv() {
                s.recv(|b| {
                    line.extend_from_slice(b);
                    (b.len(), ())
                })
                .unwrap();
            }
            line.ends_with(b"\n")
        });
        let line = String::from_utf8(line).unwrap();
        let parts: Vec<&str> = line.trim().split(' ').collect();
        assert_eq!(parts[0], "ECHO");
        assert_eq!(parts[2..], ["HELLO", &i.to_string()]);
        ids.insert(parts[1].to_owned());
    }
    assert_eq!(ids.len(), 6, "six distinct host connections");
    assert_eq!(accepted.load(Ordering::SeqCst), 6);
    // Bulk both ways on one connection.
    let h = hs[0];
    let data: Vec<u8> = (0..1_000_000u32).map(|i| (i % 253) as u8).collect();
    let sum: u64 = data.iter().map(|&x| u64::from(x)).sum();
    g.send_all(h, b"SINK 1000000\n");
    g.send_all(h, &data);
    let r = g.recv_exact(h, format!("SUM {sum}\n").len());
    assert_eq!(String::from_utf8(r).unwrap(), format!("SUM {sum}\n"));
    g.send_all(h, b"SEND 700000\n");
    let r = g.recv_exact(h, 700_000);
    assert!(r.iter().enumerate().all(|(i, &b)| b == (i % 251) as u8));
    // Close everything from the guest side.
    for &h in &hs {
        g.tcp(h).close();
    }
    g.until("closed", |g| {
        hs.iter()
            .all(|&h| matches!(g.tcp(h).state(), tcp::State::Closed | tcp::State::TimeWait))
    });
    let recs = wait_records(&stack, 6);
    let conns = connections(&recs);
    assert_eq!(conns.len(), 6);
    assert!(conns.iter().all(|c| c.endpoint == "sql.lan"
        && c.virtual_addr == SocketAddrV4::new(VIP_SQL, 1433)
        && c.target == sql
        && c.outcome == ConnOutcome::Closed));
    let big = conns
        .iter()
        .find(|c| c.bytes_from_guest > 1_000_000)
        .unwrap();
    assert!(big.bytes_to_guest >= 700_000);
    let s = stack.stats();
    assert_eq!(s.connections, 6);
    assert_eq!(s.open_connections, 0);
}

#[test]
fn everything_else_is_refused_or_dropped() {
    let (sql, accepted) = line_server();
    let (stack, mut g) = start(NetConfig::new(endpoints(sql, sql)), &["sql.lan"]);
    g.configure();
    // Configured endpoint, not allowed: RST.
    let h = g.connect(VIP_WCF, 8733);
    g.refused(h);
    // Allowed address, other port: RST.
    let h = g.connect(VIP_SQL, 80);
    g.refused(h);
    // The gateway and the DNS server have no TCP service.
    let h = g.connect(GATEWAY_IP, 135);
    g.refused(h);
    let h = g.connect(DNS_IP, 53);
    g.refused(h);
    // Addresses outside the guest network (via the route the guest added)
    // and inside it but not an endpoint: no answer at all.
    let lan = g.connect(Ipv4Addr::new(192, 168, 100, 254), 80);
    let net = g.connect(Ipv4Addr::new(1, 1, 1, 1), 443);
    let other = g.connect(Ipv4Addr::new(10, 0, 2, 77), 1433);
    g.idle(Duration::from_millis(500));
    for h in [lan, net, other] {
        assert_eq!(g.tcp(h).state(), tcp::State::SynSent, "no SYN-ACK");
    }
    // UDP elsewhere than the DNS server.
    let mut s = udp::Socket::new(
        udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 4], vec![0; 2048]),
        udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 4], vec![0; 2048]),
    );
    s.bind(5353).unwrap();
    let u = g.sockets.add(s);
    for dst in [
        Ipv4Addr::new(8, 8, 8, 8),
        Ipv4Addr::new(192, 168, 100, 4),
        GATEWAY_IP,
    ] {
        g.sockets
            .get_mut::<udp::Socket<'static>>(u)
            .send_slice(b"x", IpEndpoint::new(IpAddress::Ipv4(dst), 53))
            .unwrap();
    }
    g.idle(Duration::from_millis(200));
    // IPv6 frame.
    let mut v6 = vec![0u8; 80];
    v6[0..6].copy_from_slice(&guest_net::GATEWAY_MAC);
    v6[6..12].copy_from_slice(&GUEST_MAC);
    v6[12..14].copy_from_slice(&[0x86, 0xdd]);
    g.dev.end.to_stack.blocking_send(v6).unwrap();
    g.idle(Duration::from_millis(100));
    let s = stack.stats();
    assert_eq!(s.connections, 0);
    assert_eq!(accepted.load(Ordering::SeqCst), 0, "the host saw nothing");
    assert!(s.udp_dropped >= 3, "{s:?}");
    assert_eq!(s.ipv6_dropped, 1);
    // 10.0.2.77 is on-link and never answers ARP: no SYN leaves the guest.
    assert!(s.refused >= 6, "{s:?}");
    let recs = stack.take_records();
    let denied: Vec<(SocketAddrV4, DenyReason)> = recs
        .iter()
        .filter_map(|r| match r {
            NetRecord::Denied(d) => Some((d.destination, d.reason)),
            NetRecord::Connection(_) => None,
        })
        .collect();
    assert!(denied.contains(&(SocketAddrV4::new(VIP_WCF, 8733), DenyReason::NotAllowed)));
    assert!(denied.contains(&(SocketAddrV4::new(VIP_SQL, 80), DenyReason::NotAnEndpoint)));
    assert!(denied.contains(&(
        SocketAddrV4::new(Ipv4Addr::new(1, 1, 1, 1), 443),
        DenyReason::NotAnEndpoint
    )));
}

#[test]
fn revocation_resets_live_connections_and_unresolves_names() {
    let (sql, _) = line_server();
    let (stack, mut g) = start(NetConfig::new(endpoints(sql, sql)), &["sql.lan"]);
    g.configure();
    let h = g.connect(VIP_SQL, 1433);
    g.established(h);
    g.send_all(h, b"PING\n");
    let r = g.recv_exact(h, "ECHO 1 PING\n".len());
    assert_eq!(r, b"ECHO 1 PING\n");
    stack.set_allowed(Vec::<String>::new());
    g.refused(h);
    let recs = wait_records(&stack, 1);
    let c = connections(&recs);
    assert_eq!(c.len(), 1);
    assert_eq!(c[0].outcome, ConnOutcome::Revoked);
    assert_eq!(c[0].bytes_from_guest, 5);
    assert_eq!(c[0].bytes_to_guest, 12);
    assert_eq!(g.dns("sql.lan", 1), Some((3, vec![])));
    let h = g.connect(VIP_SQL, 1433);
    g.refused(h);
    assert!(stack.allowed().is_empty());
}

#[test]
fn connection_limit_and_unreachable_target() {
    let (sql, _) = line_server();
    // An endpoint whose host port is closed.
    let closed = {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap()
    };
    let mut cfg = NetConfig::new(endpoints(sql, closed));
    cfg.max_connections = 2;
    cfg.connect_timeout = Duration::from_secs(3);
    let (stack, mut g) = start(cfg, &["sql.lan", "wcfhost"]);
    g.configure();
    let a = g.connect(VIP_SQL, 1433);
    let b = g.connect(VIP_SQL, 1433);
    g.established(a);
    g.established(b);
    let c = g.connect(VIP_SQL, 1433);
    g.refused(c);
    g.tcp(a).close();
    g.tcp(b).close();
    g.until("closed", |g| {
        [a, b]
            .iter()
            .all(|&h| matches!(g.tcp(h).state(), tcp::State::Closed | tcp::State::TimeWait))
    });
    let _ = wait_records(&stack, 3);
    // The closed host port: the guest connection is reset.
    let d = g.connect(VIP_WCF, 8733);
    g.refused(d);
    let recs = wait_records(&stack, 1);
    let conns = connections(&recs);
    assert_eq!(conns.len(), 1, "{recs:?}");
    assert_eq!(conns[0].outcome, ConnOutcome::TargetUnreachable);
    assert_eq!(conns[0].target, closed);
}

#[test]
fn public_only_endpoint_never_reaches_a_host_target() {
    let (sql, accepted) = line_server();
    let mut cfg = NetConfig::new(endpoints(sql, sql));
    // Filtered Internet: a pinned answer that points at loopback (DNS
    // rebinding, a host route) is refused before any host connection.
    cfg.public_endpoints = BTreeSet::from(["wcfhost".to_owned()]);
    cfg.connect_timeout = Duration::from_secs(3);
    let (stack, mut g) = start(cfg, &["sql.lan", "wcfhost"]);
    g.configure();
    let refused = g.connect(VIP_WCF, 8733);
    g.refused(refused);
    let recs = wait_records(&stack, 1);
    let conns = connections(&recs);
    assert_eq!(conns.len(), 1, "{recs:?}");
    assert_eq!(conns[0].outcome, ConnOutcome::TargetUnreachable);
    assert_eq!(accepted.load(Ordering::SeqCst), 0);
    // An ordinary endpoint of the same stack keeps working.
    let ok = g.connect(VIP_SQL, 1433);
    g.established(ok);
    assert_eq!(accepted.load(Ordering::SeqCst), 1);
}

#[test]
fn stop_resets_connections_and_ends_the_stack() {
    let (sql, _) = line_server();
    let (mut stack, mut g) = start(NetConfig::new(endpoints(sql, sql)), &["sql.lan"]);
    g.configure();
    let h = g.connect(VIP_SQL, 1433);
    g.established(h);
    assert!(stack.is_running());
    stack.stop();
    assert!(!stack.is_running());
    let recs = stack.take_records();
    let c = connections(&recs);
    assert_eq!(c.len(), 1);
    assert_eq!(c[0].outcome, ConnOutcome::Stopped);
    g.refused(h);
}

#[test]
fn reset_before_handshake_ack_releases_host_connection_slots() {
    let (server, accepted) = line_server();
    let mut config = NetConfig::new(endpoints(server, server));
    config.max_connections = 2;
    let (stack, mut guest) = start(config, &["sql.lan"]);
    guest.configure();
    // Our TCP peer receives SYN-ACK but withholds its final ACK, so the
    // production socket stays SynReceived while its host connection opens.
    guest.dev.drop_plain_ack = true;
    for attempt in 0..6 {
        let handle = guest.connect(VIP_SQL, 1433);
        guest.established(handle);
        guest.until("host connection accepted", |_| {
            accepted.load(Ordering::SeqCst) > attempt
        });
        assert_eq!(stack.stats().open_connections, 1);
        guest.tcp(handle).abort();
        guest.until("reset handshake frees its slot", |_| {
            stack.stats().open_connections == 0
        });
        guest.sockets.remove(handle);
    }
    let records = wait_records(&stack, 6);
    assert_eq!(connections(&records).len(), 6);
    assert!(
        connections(&records)
            .iter()
            .all(|record| record.outcome == ConnOutcome::ResetByGuest)
    );
    // Once the final ACK is allowed again, ordinary traffic still works.
    guest.dev.drop_plain_ack = false;
    let handle = guest.connect(VIP_SQL, 1433);
    guest.established(handle);
    guest.send_all(handle, b"after-reset\n");
    assert!(
        String::from_utf8(guest.recv_exact(handle, 19))
            .unwrap()
            .contains("after-reset")
    );
}

#[test]
fn bandwidth_limit_slows_transfers() {
    let (sql, _) = line_server();
    let mut cfg = NetConfig::new(endpoints(sql, sql));
    cfg.max_bytes_per_second = Some(256 * 1024);
    let (_stack, mut g) = start(cfg, &["sql.lan"]);
    g.configure();
    let h = g.connect(VIP_SQL, 1433);
    g.established(h);
    let t = Instant::now();
    g.send_all(h, b"SEND 786432\n");
    let _ = g.recv_exact(h, 786_432);
    let e = t.elapsed();
    // 768 KiB at 256 KiB/s after a 256 KiB burst: at least about 2 s.
    assert!(e >= Duration::from_millis(1700), "{e:?}");
}

#[test]
fn invalid_configurations_are_refused() {
    let a: SocketAddr = "127.0.0.1:1".parse().unwrap();
    let mut eps = endpoints(a, a);
    eps[1].name = "SQL.lan".into();
    assert!(NetConfig::new(eps).validate().is_err(), "duplicate name");
    let mut eps = endpoints(a, a);
    eps[1].virtual_ip = VIP_SQL;
    eps[1].port = 1433;
    assert!(NetConfig::new(eps).validate().is_err(), "duplicate address");
    for ip in [
        GUEST_IP,
        GATEWAY_IP,
        DNS_IP,
        Ipv4Addr::new(10, 0, 2, 255),
        Ipv4Addr::new(10, 0, 3, 5),
    ] {
        let mut eps = endpoints(a, a);
        eps[0].virtual_ip = ip;
        assert!(NetConfig::new(eps).validate().is_err(), "{ip}");
    }
    let mut eps = endpoints(a, a);
    eps[0].name = "*.lan".into();
    assert!(NetConfig::new(eps).validate().is_err());
    let mut eps = endpoints(a, a);
    eps[0].port = 0;
    assert!(NetConfig::new(eps).validate().is_err());
    let (link, _end) = Link::pair();
    let mut eps = endpoints(a, a);
    eps[0].port = 0;
    assert!(NetStack::start(NetConfig::new(eps), link, []).is_err());
}
