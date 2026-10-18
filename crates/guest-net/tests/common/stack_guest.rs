// The stack against a guest made of a second smoltcp interface: DHCP, DNS,
// per-connection TCP proxy to real host sockets, refusals, revocation,
// limits and records (ADR-0033).

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use guest_net::transport::{GuestEnd, Link};
use guest_net::{
    DNS_IP, EndpointSpec, GATEWAY_IP, GUEST_IP, GUEST_MAC, NetConfig, NetRecord, NetStack,
};
use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::socket::{tcp, udp};
use smoltcp::wire::{EthernetAddress, HardwareAddress, IpAddress, IpCidr, IpEndpoint};

const VIP_SQL: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 101);
const VIP_WCF: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 102);
const T: Duration = Duration::from_secs(10);

// ----- guest ------------------------------------------------------------------

struct GuestDev {
    end: GuestEnd,
    drop_plain_ack: bool,
}

struct Rx(Vec<u8>);
struct Tx<'a>(&'a tokio::sync::mpsc::Sender<Vec<u8>>, bool);

impl RxToken for Rx {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        f(&self.0)
    }
}

impl TxToken for Tx<'_> {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        let mut b = vec![0u8; len];
        let r = f(&mut b);
        if !self.1 || !is_plain_ack(&b) {
            let _ = self.0.blocking_send(b);
        }
        r
    }
}

impl Device for GuestDev {
    type RxToken<'a> = Rx;
    type TxToken<'a> = Tx<'a>;
    fn receive(&mut self, _: smoltcp::time::Instant) -> Option<(Rx, Tx<'_>)> {
        let f = self.end.from_stack.try_recv().ok()?;
        Some((Rx(f), Tx(&self.end.to_stack, self.drop_plain_ack)))
    }
    fn transmit(&mut self, _: smoltcp::time::Instant) -> Option<Tx<'_>> {
        Some(Tx(&self.end.to_stack, self.drop_plain_ack))
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
        let mut dev = GuestDev {
            end,
            drop_plain_ack: false,
        };
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

fn is_plain_ack(frame: &[u8]) -> bool {
    use smoltcp::wire::{EthernetFrame, EthernetProtocol, IpProtocol, Ipv4Packet, TcpPacket};
    let Ok(ethernet) = EthernetFrame::new_checked(frame) else {
        return false;
    };
    if ethernet.ethertype() != EthernetProtocol::Ipv4 {
        return false;
    }
    let Ok(ip) = Ipv4Packet::new_checked(ethernet.payload()) else {
        return false;
    };
    if ip.next_header() != IpProtocol::Tcp {
        return false;
    }
    let Ok(tcp) = TcpPacket::new_checked(ip.payload()) else {
        return false;
    };
    tcp.ack() && !tcp.syn() && !tcp.rst() && !tcp.fin() && tcp.payload().is_empty()
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
