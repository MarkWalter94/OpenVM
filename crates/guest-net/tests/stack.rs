//! The stack against a guest made of a second smoltcp interface: DHCP, DNS,
//! per-connection TCP proxy to real host sockets, refusals, revocation,
//! limits and records (ADR-0033).

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use guest_net::transport::{GuestEnd, Link};
use guest_net::{
    ConnOutcome, DNS_IP, DenyReason, EndpointSpec, GATEWAY_IP, GUEST_IP, GUEST_MAC, NetConfig,
    NetRecord, NetStack,
};
use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::socket::{dhcpv4, tcp, udp};
use smoltcp::wire::{EthernetAddress, HardwareAddress, IpAddress, IpCidr, IpEndpoint, Ipv4Cidr};

const VIP_SQL: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 101);
const VIP_WCF: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 102);
const T: Duration = Duration::from_secs(10);

// ----- guest ------------------------------------------------------------------

struct GuestDev {
    end: GuestEnd,
}

struct Rx(Vec<u8>);
struct Tx<'a>(&'a tokio::sync::mpsc::Sender<Vec<u8>>);

impl RxToken for Rx {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        f(&self.0)
    }
}

impl TxToken for Tx<'_> {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        let mut b = vec![0u8; len];
        let r = f(&mut b);
        let _ = self.0.blocking_send(b);
        r
    }
}

impl Device for GuestDev {
    type RxToken<'a> = Rx;
    type TxToken<'a> = Tx<'a>;
    fn receive(&mut self, _: smoltcp::time::Instant) -> Option<(Rx, Tx<'_>)> {
        let f = self.end.from_stack.try_recv().ok()?;
        Some((Rx(f), Tx(&self.end.to_stack)))
    }
    fn transmit(&mut self, _: smoltcp::time::Instant) -> Option<Tx<'_>> {
        Some(Tx(&self.end.to_stack))
    }
    fn capabilities(&self) -> DeviceCapabilities {
        let mut c = DeviceCapabilities::default();
        c.medium = Medium::Ethernet;
        c.max_transmission_unit = 1514;
        c
    }
}

struct Guest {
    iface: Interface,
    dev: GuestDev,
    sockets: SocketSet<'static>,
    t0: Instant,
    next_port: u16,
}

impl Guest {
    fn new(end: GuestEnd) -> Self {
        let mut dev = GuestDev { end };
        let config = Config::new(HardwareAddress::Ethernet(EthernetAddress(GUEST_MAC)));
        let iface = Interface::new(config, &mut dev, smoltcp::time::Instant::from_micros(0));
        Self {
            iface,
            dev,
            sockets: SocketSet::new(Vec::new()),
            t0: Instant::now(),
            next_port: 40000,
        }
    }

    /// Static configuration as after DHCP, with a default route via the
    /// gateway (a hostile guest adds one: the stack never offers it).
    fn configure(&mut self) {
        self.iface.update_ip_addrs(|a| {
            a.push(IpCidr::new(IpAddress::Ipv4(GUEST_IP), 24)).unwrap();
        });
        self.iface
            .routes_mut()
            .add_default_ipv4_route(GATEWAY_IP)
            .unwrap();
    }

    fn poll(&mut self) {
        let now = smoltcp::time::Instant::from_micros(self.t0.elapsed().as_micros() as i64);
        self.iface.poll(now, &mut self.dev, &mut self.sockets);
    }

    fn until(&mut self, what: &str, mut cond: impl FnMut(&mut Self) -> bool) {
        let t = Instant::now();
        loop {
            self.poll();
            if cond(self) {
                return;
            }
            assert!(t.elapsed() < T, "timeout waiting for {what}");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Polls for `d` without a condition.
    fn idle(&mut self, d: Duration) {
        let t = Instant::now();
        while t.elapsed() < d {
            self.poll();
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn connect(&mut self, ip: Ipv4Addr, port: u16) -> SocketHandle {
        let mut s = tcp::Socket::new(
            tcp::SocketBuffer::new(vec![0; 256 * 1024]),
            tcp::SocketBuffer::new(vec![0; 256 * 1024]),
        );
        self.next_port += 1;
        s.connect(
            self.iface.context(),
            IpEndpoint::new(IpAddress::Ipv4(ip), port),
            self.next_port,
        )
        .unwrap();
        self.sockets.add(s)
    }

    fn tcp(&mut self, h: SocketHandle) -> &mut tcp::Socket<'static> {
        self.sockets.get_mut::<tcp::Socket<'static>>(h)
    }

    fn established(&mut self, h: SocketHandle) {
        self.until("established", |g| {
            g.tcp(h).state() == tcp::State::Established
        });
    }

    /// Waits for the connection to be refused (RST: back to CLOSED).
    fn refused(&mut self, h: SocketHandle) {
        self.until("refusal", |g| g.tcp(h).state() == tcp::State::Closed);
    }

    fn send_all(&mut self, h: SocketHandle, data: &[u8]) {
        let mut off = 0;
        self.until("send", |g| {
            if g.tcp(h).can_send() {
                off += g.tcp(h).send_slice(&data[off..]).unwrap();
            }
            off == data.len()
        });
    }

    fn recv_exact(&mut self, h: SocketHandle, n: usize) -> Vec<u8> {
        let mut out = Vec::new();
        self.until("receive", |g| {
            let s = g.tcp(h);
            if s.can_recv() {
                s.recv(|b| {
                    let k = b.len().min(n - out.len());
                    out.extend_from_slice(&b[..k]);
                    (k, ())
                })
                .unwrap();
            }
            out.len() == n
        });
        out
    }

    fn dns(&mut self, name: &str, qtype: u16) -> Option<(u8, Vec<Ipv4Addr>)> {
        let mut s = udp::Socket::new(
            udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 4], vec![0; 2048]),
            udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 4], vec![0; 2048]),
        );
        self.next_port += 1;
        s.bind(self.next_port).unwrap();
        let h = self.sockets.add(s);
        let mut q = vec![0xab, 0xcd, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0];
        for l in name.split('.') {
            q.push(l.len() as u8);
            q.extend_from_slice(l.as_bytes());
        }
        q.push(0);
        q.extend_from_slice(&qtype.to_be_bytes());
        q.extend_from_slice(&1u16.to_be_bytes());
        self.sockets
            .get_mut::<udp::Socket<'static>>(h)
            .send_slice(&q, IpEndpoint::new(IpAddress::Ipv4(DNS_IP), 53))
            .unwrap();
        let mut reply = None;
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(2) && reply.is_none() {
            self.poll();
            if let Ok((d, _)) = self.sockets.get_mut::<udp::Socket<'static>>(h).recv() {
                reply = Some(d.to_vec());
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        self.sockets.remove(h);
        let r = reply?;
        assert_eq!(&r[..2], &[0xab, 0xcd]);
        let rcode = r[3] & 0x0f;
        let an = u16::from_be_bytes([r[6], r[7]]);
        let addrs = (0..an)
            .map(|_| {
                let a = &r[r.len() - 4..];
                Ipv4Addr::new(a[0], a[1], a[2], a[3])
            })
            .collect();
        Some((rcode, addrs))
    }
}

// ----- host servers ---------------------------------------------------------

/// Line server: for each connection, replies `ECHO <conn#> <line>` to each
/// line; `SINK n` reads n bytes and answers their sum; `SEND n` sends n
/// bytes of a pattern.
fn line_server() -> (SocketAddr, Arc<AtomicU32>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let count = Arc::new(AtomicU32::new(0));
    let c2 = count.clone();
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            let id = c2.fetch_add(1, Ordering::SeqCst) + 1;
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                let mut b = [0u8; 4096];
                loop {
                    let n = match s.read(&mut b) {
                        Ok(0) | Err(_) => return,
                        Ok(n) => n,
                    };
                    buf.extend_from_slice(&b[..n]);
                    while let Some(p) = buf.iter().position(|&c| c == b'\n') {
                        let line: Vec<u8> = buf.drain(..=p).collect();
                        let line = String::from_utf8_lossy(&line[..line.len() - 1]).into_owned();
                        if let Some(n) = line.strip_prefix("SINK ") {
                            let mut left: usize = n.parse().unwrap();
                            let mut sum: u64 = buf.iter().map(|&x| u64::from(x)).sum();
                            left -= buf.len().min(left);
                            buf.clear();
                            while left > 0 {
                                let n = s.read(&mut b[..left.min(4096)]).unwrap();
                                assert!(n > 0);
                                sum += b[..n].iter().map(|&x| u64::from(x)).sum::<u64>();
                                left -= n;
                            }
                            let _ = s.write_all(format!("SUM {sum}\n").as_bytes());
                        } else if let Some(n) = line.strip_prefix("SEND ") {
                            let n: usize = n.parse().unwrap();
                            let data: Vec<u8> = (0..n).map(|i| (i % 251) as u8).collect();
                            let _ = s.write_all(&data);
                        } else if line == "BYE" {
                            return;
                        } else {
                            let _ = s.write_all(format!("ECHO {id} {line}\n").as_bytes());
                        }
                    }
                }
            });
        }
    });
    (addr, count)
}

fn endpoints(sql: SocketAddr, wcf: SocketAddr) -> Vec<EndpointSpec> {
    vec![
        EndpointSpec {
            name: "sql.lan".into(),
            virtual_ip: VIP_SQL,
            port: 1433,
            target: sql,
        },
        EndpointSpec {
            name: "WcfHost".into(),
            virtual_ip: VIP_WCF,
            port: 8733,
            target: wcf,
        },
    ]
}

fn start(config: NetConfig, allowed: &[&str]) -> (NetStack, Guest) {
    let (link, end) = Link::pair();
    let stack = NetStack::start(config, link, allowed.iter().map(|s| (*s).to_owned())).unwrap();
    (stack, Guest::new(end))
}

fn wait_records(stack: &NetStack, n: usize) -> Vec<NetRecord> {
    let mut v = Vec::new();
    let t = Instant::now();
    while v.len() < n && t.elapsed() < T {
        v.extend(stack.take_records());
        std::thread::sleep(Duration::from_millis(5));
    }
    v
}

fn connections(v: &[NetRecord]) -> Vec<&guest_net::ConnRecord> {
    v.iter()
        .filter_map(|r| match r {
            NetRecord::Connection(c) => Some(c),
            NetRecord::Denied(_) => None,
        })
        .collect()
}

// ----- tests ------------------------------------------------------------------

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
