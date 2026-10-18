// Real guest tests (ADR-0033, R6-06): the Ubuntu live system of the lab,
// booted under WHPX with its NIC on `-netdev stream` to this crate's stack
// (endpoint allowlist), or on QEMU's user network (NAT). The guest is
// driven through its serial console; probe scripts are pushed in base64.
//
// `#[ignore]`: they need the lab (`LIGHTVM_LAB`, default `E:\OpenVM\lab`):
// QEMU in `qemu/qemu-11.1.0`, the kernel and initrd extracted from the
// approved Ubuntu ISO in `integration-spike/ubuntu-boot/casper`, the ISO in
// `media/`. Scratch files go to `lab/net/`. Run with
// `cargo test -p guest-net --test real_guest -- --ignored --test-threads=1 --nocapture`.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use guest_net::{EndpointSpec, NetConfig, NetStack};

fn lab() -> PathBuf {
    std::env::var_os("LIGHTVM_LAB")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"E:\OpenVM\lab"))
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Host LAN address (the source address of a route towards the Internet).
fn host_lan_ip() -> Option<Ipv4Addr> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("192.0.2.1:9").ok()?;
    match s.local_addr().ok()?.ip() {
        std::net::IpAddr::V4(ip) if !ip.is_loopback() => Some(ip),
        _ => None,
    }
}

/// Line server (see `stack.rs`): `ECHO <conn#> <line>`, `SINK n`, `SEND n`.
fn line_server(bind: &str) -> (SocketAddr, Arc<AtomicU32>) {
    let l = TcpListener::bind(bind).unwrap();
    let addr = l.local_addr().unwrap();
    let count = Arc::new(AtomicU32::new(0));
    let c2 = count.clone();
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            let id = c2.fetch_add(1, Ordering::SeqCst) + 1;
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                let mut b = [0u8; 65536];
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
                            let mut left: usize = n.parse().unwrap_or(0);
                            let take = buf.len().min(left);
                            let mut sum: u64 = buf.drain(..take).map(u64::from).sum();
                            left -= take;
                            while left > 0 {
                                let n = match s.read(&mut b[..left.min(65536)]) {
                                    Ok(0) | Err(_) => return,
                                    Ok(n) => n,
                                };
                                sum += b[..n].iter().map(|&x| u64::from(x)).sum::<u64>();
                                left -= n;
                            }
                            let _ = s.write_all(format!("SUM {sum}\n").as_bytes());
                        } else if let Some(n) = line.strip_prefix("SEND ") {
                            let n: usize = n.parse().unwrap_or(0);
                            let data: Vec<u8> = (0..n).map(|i| (i % 251) as u8).collect();
                            let _ = s.write_all(&data);
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

/// Listener that must never see a connection.
fn decoy() -> (u16, Arc<AtomicU32>) {
    let l = TcpListener::bind("0.0.0.0:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let count = Arc::new(AtomicU32::new(0));
    let c2 = count.clone();
    std::thread::spawn(move || {
        for mut s in l.incoming().flatten() {
            c2.fetch_add(1, Ordering::SeqCst);
            let _ = s.write_all(b"HOST-SERVICE-REACHED\n");
        }
    });
    (port, count)
}

struct Guest {
    child: Child,
    serial: TcpStream,
    log: std::fs::File,
}

impl Drop for Guest {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Guest {
    /// Boots the live system with the given network arguments.
    fn boot(tag: &str, net_args: &[String]) -> Guest {
        let lab = lab();
        let q = lab.join(r"qemu\qemu-11.1.0\qemu-system-x86_64.exe");
        // The kernel and initrd extracted by the network spike, or by the
        // Tools real tests (`lab/tools-real`).
        let casper = [
            r"integration-spike\ubuntu-boot\casper",
            r"tools-real\ubuntu-boot\casper",
        ]
        .iter()
        .map(|p| lab.join(p))
        .find(|p| p.join("vmlinuz").is_file())
        .unwrap_or_else(|| lab.join(r"integration-spike\ubuntu-boot\casper"));
        let iso = lab.join(r"media\ubuntu-26.04.1-desktop-amd64.iso");
        for p in [&q, &casper.join("vmlinuz"), &casper.join("initrd"), &iso] {
            assert!(p.is_file(), "missing lab file {}", p.display());
        }
        let dir = lab.join("net").join("logs");
        std::fs::create_dir_all(&dir).unwrap();
        let serial_port = free_port();
        let mut args: Vec<String> = [
            "-no-user-config",
            "-nodefaults",
            "-accel",
            "whpx",
            "-machine",
            "q35",
            "-cpu",
            "Skylake-Client-v4",
            "-m",
            "2048",
            "-smp",
            "2",
            "-display",
            "none",
            "-device",
            "VGA",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
        args.extend([
            "-kernel".into(),
            casper.join("vmlinuz").display().to_string(),
            "-initrd".into(),
            casper.join("initrd").display().to_string(),
            "-append".into(),
            "boot=casper console=ttyS0,115200 systemd.unit=multi-user.target".into(),
            "-drive".into(),
            format!(
                "if=none,id=cd0,media=cdrom,readonly=on,file={}",
                iso.display()
            ),
            "-device".into(),
            "ide-cd,drive=cd0,bus=ide.0".into(),
            "-serial".into(),
            format!("tcp:127.0.0.1:{serial_port},server=on,wait=off"),
        ]);
        args.extend(net_args.iter().cloned());
        std::fs::write(dir.join(format!("{tag}.cmdline.txt")), args.join(" ")).unwrap();
        let child = Command::new(&q)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(std::fs::File::create(dir.join(format!("{tag}.stdout.txt"))).unwrap())
            .stderr(std::fs::File::create(dir.join(format!("{tag}.stderr.txt"))).unwrap())
            .spawn()
            .unwrap();
        // BelowNormal priority (shared host).
        let _ = Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                &format!(
                    "(Get-Process -Id {}).PriorityClass = 'BelowNormal'",
                    child.id()
                ),
            ])
            .status();
        let t = Instant::now();
        let serial = loop {
            if let Ok(s) = TcpStream::connect(("127.0.0.1", serial_port)) {
                break s;
            }
            assert!(t.elapsed() < Duration::from_secs(20), "serial port");
            std::thread::sleep(Duration::from_millis(200));
        };
        serial
            .set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        let log = std::fs::File::create(dir.join(format!("{tag}.serial.txt"))).unwrap();
        Guest { child, serial, log }
    }

    fn read_until(&mut self, pat: &str, timeout: Duration) -> Option<String> {
        let t = Instant::now();
        let mut acc = Vec::new();
        let mut b = [0u8; 65536];
        while t.elapsed() < timeout {
            match self.serial.read(&mut b) {
                Ok(0) => break,
                Ok(n) => {
                    let _ = self.log.write_all(&b[..n]);
                    acc.extend_from_slice(&b[..n]);
                    if String::from_utf8_lossy(&acc).contains(pat) {
                        return Some(String::from_utf8_lossy(&acc).into_owned());
                    }
                }
                Err(_) => {}
            }
        }
        None
    }

    fn login(&mut self) {
        let t = Instant::now();
        loop {
            self.serial.write_all(b"\n").unwrap();
            if self.read_until("login:", Duration::from_secs(10)).is_some() {
                break;
            }
            assert!(t.elapsed() < Duration::from_secs(600), "no login prompt");
        }
        self.serial.write_all(b"ubuntu\n").unwrap();
        assert!(
            self.read_until("ubuntu@ubuntu", Duration::from_secs(60))
                .is_some(),
            "no shell"
        );
        self.cmd("export TERM=dumb; stty -echo", Duration::from_secs(10));
    }

    /// Runs a shell command; returns its output up to the end marker.
    fn cmd(&mut self, c: &str, timeout: Duration) -> String {
        let line = format!("{c}; echo \"@@E\"\"ND@@ rc=$?\"\n");
        self.serial.write_all(line.as_bytes()).unwrap();
        let out = self.read_until("@@END@@ rc=", timeout).unwrap_or_else(|| {
            panic!("command timed out: {c}");
        });
        // Read the rest of the marker line.
        let mut tail = String::new();
        if let Some(t) = self.read_until("\n", Duration::from_millis(500)) {
            tail = t;
        }
        out + &tail
    }

    /// Writes a file in the guest through base64 chunks.
    fn push(&mut self, content: &str, dst: &str) {
        let b64 = base64(content.as_bytes());
        self.cmd("rm -f /tmp/push.b64", Duration::from_secs(10));
        for chunk in b64.as_bytes().chunks(900) {
            let c = std::str::from_utf8(chunk).unwrap();
            let r = self.cmd(
                &format!("echo -n {c} >> /tmp/push.b64"),
                Duration::from_secs(10),
            );
            assert!(r.contains("rc=0"), "{r}");
        }
        let r = self.cmd(
            &format!("base64 -d /tmp/push.b64 > {dst} && wc -c < {dst}"),
            Duration::from_secs(10),
        );
        assert!(r.contains(&content.len().to_string()), "{r}");
    }
}

fn base64(data: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in data.chunks(3) {
        let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= c.len() {
                out.push(A[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Guest probe: one line per check, `name<TAB>result`.
const PROBE: &str = r#"
import socket, struct, subprocess, sys, time
T = 3.0
def tcp(host, port, fam=socket.AF_INET, send=None):
    s = socket.socket(fam, socket.SOCK_STREAM); s.settimeout(T)
    try:
        s.connect((host, port)); r = "CONNECTED"
        if send is not None:
            s.sendall(send)
            try: r += " recv=" + s.recv(200).decode(errors="replace").strip()
            except Exception as e: r += " recv-err=" + type(e).__name__
    except socket.timeout: r = "TIMEOUT"
    except ConnectionRefusedError: r = "REFUSED"
    except Exception as e: r = "FAIL " + type(e).__name__
    finally: s.close()
    return r
def dnsq(server, name, qtype=1, fam=socket.AF_INET):
    q = struct.pack(">HHHHHH", 0x1234, 0x0100, 1, 0, 0, 0)
    for p in name.split("."): q += bytes([len(p)]) + p.encode()
    q += b"\x00" + struct.pack(">HH", qtype, 1)
    s = socket.socket(fam, socket.SOCK_DGRAM); s.settimeout(T)
    try:
        s.sendto(q, (server, 53)); d, _ = s.recvfrom(512)
        an = struct.unpack(">H", d[6:8])[0]
        ip = ".".join(str(x) for x in d[-4:]) if an else "-"
        return "ANSWER rcode=%d an=%d ip=%s" % (d[3] & 15, an, ip)
    except socket.timeout: return "TIMEOUT"
    except Exception as e: return "FAIL " + type(e).__name__
    finally: s.close()
def ping(host, v6=False):
    r = subprocess.run(["ping", "-c", "1", "-W", "2"] + (["-6"] if v6 else []) + [host], capture_output=True)
    return "REPLY" if r.returncode == 0 else "NOREPLY"
def multi(host, port, n):
    ss = [socket.create_connection((host, port), timeout=5) for _ in range(n)]
    ids = set(); ok = True
    for i in reversed(range(n)):
        ss[i].sendall(b"TOKEN %d\n" % i)
    for i, s in enumerate(ss):
        s.settimeout(5); line = b""
        while not line.endswith(b"\n"): line += s.recv(200)
        p = line.decode().split()
        ok = ok and p[0] == "ECHO" and p[2:] == ["TOKEN", str(i)]
        ids.add(p[1])
    for s in ss: s.close()
    return "OK" if ok and len(ids) == n else "BAD ids=%d" % len(ids)
def bulk(host, port, n):
    s = socket.create_connection((host, port), timeout=10); s.settimeout(30)
    data = bytes(i % 253 for i in range(n)); t = time.time()
    s.sendall(b"SINK %d\n" % n); s.sendall(data)
    line = b""
    while not line.endswith(b"\n"): line += s.recv(100)
    up = n / (time.time() - t) / 1048576
    ok1 = line.decode().strip() == "SUM %d" % sum(data)
    t = time.time(); s.sendall(b"SEND %d\n" % n); got = bytearray()
    while len(got) < n: got += s.recv(1 << 16)
    down = n / (time.time() - t) / 1048576
    ok2 = all(got[i] == i % 251 for i in range(0, n, 4099))
    s.close()
    return "%s up=%.1fMiB/s down=%.1fMiB/s" % ("OK" if ok1 and ok2 else "BAD", up, down)
mode = sys.argv[1]
args = sys.argv[2:]
checks = []
if mode == "allow":
    host_ip, decoy = args[0], int(args[1])
    checks = [
        ("dns sql.lan", lambda: dnsq("10.0.2.3", "sql.lan")),
        ("dns sql.lan AAAA", lambda: dnsq("10.0.2.3", "sql.lan", 28)),
        ("dns wcfhost (not allowed)", lambda: dnsq("10.0.2.3", "wcfhost")),
        ("dns example.com", lambda: dnsq("10.0.2.3", "example.com")),
        ("getaddrinfo sql.lan", lambda: ",".join(sorted({a[4][0] for a in socket.getaddrinfo("sql.lan", 1433, socket.AF_INET)}))),
        ("tcp by name sql.lan:1433", lambda: tcp("sql.lan", 1433, send=b"HELLO name\n")),
        ("tcp by vip 10.0.2.101:1433", lambda: tcp("10.0.2.101", 1433, send=b"HELLO vip\n")),
        ("multi 8 concurrent", lambda: multi("10.0.2.101", 1433, 8)),
        ("bulk 16MiB", lambda: bulk("10.0.2.101", 1433, 16 << 20)),
        ("tcp 10.0.2.102:8733 (not allowed)", lambda: tcp("10.0.2.102", 8733)),
        ("tcp 10.0.2.101:80 (other port)", lambda: tcp("10.0.2.101", 80)),
        ("tcp gateway 10.0.2.2:decoy", lambda: tcp("10.0.2.2", decoy)),
        ("tcp gateway 10.0.2.2:135", lambda: tcp("10.0.2.2", 135)),
        ("tcp host LAN ip:decoy", lambda: tcp(host_ip, decoy)),
        ("tcp host LAN ip:135", lambda: tcp(host_ip, 135)),
        ("tcp host LAN ip:445", lambda: tcp(host_ip, 445)),
        ("tcp Internet 1.1.1.1:443", lambda: tcp("1.1.1.1", 443)),
        ("tcp Internet 8.8.8.8:53", lambda: tcp("8.8.8.8", 53)),
        ("dns udp 8.8.8.8", lambda: dnsq("8.8.8.8", "example.com")),
        ("dns udp 1.1.1.1", lambda: dnsq("1.1.1.1", "example.com")),
        ("tcp IPv6 [2606:4700:4700::1111]:443", lambda: tcp("2606:4700:4700::1111", 443, socket.AF_INET6)),
        ("tcp IPv6 [fec0::2]:135", lambda: tcp("fec0::2", 135, socket.AF_INET6)),
        ("icmp 10.0.2.2", lambda: ping("10.0.2.2")),
        ("icmp 1.1.1.1", lambda: ping("1.1.1.1")),
    ]
elif mode == "second":
    checks = [
        ("dns wcfhost", lambda: dnsq("10.0.2.3", "wcfhost")),
        ("dns wcfhost search", lambda: dnsq("10.0.2.3", "wcfhost.lightvm.internal")),
        ("tcp by name wcfhost:8733", lambda: tcp("wcfhost", 8733, send=b"HELLO wcf\n")),
    ]
elif mode == "revoked":
    checks = [
        ("dns sql.lan", lambda: dnsq("10.0.2.3", "sql.lan")),
        ("tcp by vip 10.0.2.101:1433", lambda: tcp("10.0.2.101", 1433, send=b"HELLO late\n")),
    ]
elif mode == "filtered":
    import ssl
    def https(name, path="/"):
        ctx = ssl.create_default_context()
        try:
            with socket.create_connection((name, 443), timeout=10) as raw:
                with ctx.wrap_socket(raw, server_hostname=name) as s:
                    s.sendall(b"GET %s HTTP/1.1\r\nHost: %s\r\nConnection: close\r\n\r\n" % (path.encode(), name.encode()))
                    s.settimeout(10); d = s.recv(64)
                    return "TLS " + s.version() + " " + d.split(b"\r\n")[0].decode(errors="replace")
        except socket.timeout: return "TIMEOUT"
        except ConnectionRefusedError: return "REFUSED"
        except Exception as e: return "FAIL " + type(e).__name__
    web, host_ip, decoy = args[0], args[1], int(args[2])
    checks = [
        ("dns web", lambda: dnsq("10.0.2.3", web)),
        ("dns other name", lambda: dnsq("10.0.2.3", "www.iana.org")),
        ("https web", lambda: https(web)),
        ("http web", lambda: tcp(web, 80, send=b"HEAD / HTTP/1.0\r\nHost: " + web.encode() + b"\r\n\r\n")),
        ("tcp web:22 (other port)", lambda: tcp(web, 22)),
        ("dns rebind", lambda: dnsq("10.0.2.3", "rebind.example.net")),
        ("tcp rebind:443 (pinned to loopback)", lambda: tcp("rebind.example.net", 443, send=b"HELLO rebind\n")),
        ("tcp gateway 10.0.2.2:decoy", lambda: tcp("10.0.2.2", decoy)),
        ("tcp host LAN ip:decoy", lambda: tcp(host_ip, decoy)),
        ("tcp Internet 1.1.1.1:443", lambda: tcp("1.1.1.1", 443)),
        ("dns udp 8.8.8.8", lambda: dnsq("8.8.8.8", "example.com")),
        ("tcp IPv6 [2606:4700:4700::1111]:443", lambda: tcp("2606:4700:4700::1111", 443, socket.AF_INET6)),
        ("icmp 1.1.1.1", lambda: ping("1.1.1.1")),
    ]
elif mode == "persist":
    # One TLS connection kept open across a revocation: the first request
    # must work, the second (after /tmp/go appears) must not.
    import os, ssl
    name = args[0]
    ctx = ssl.create_default_context()
    s = ctx.wrap_socket(socket.create_connection((name, 443), timeout=10), server_hostname=name)
    s.settimeout(10)
    def req():
        try:
            s.sendall(b"HEAD / HTTP/1.1\r\nHost: %s\r\n\r\n" % name.encode())
            d = s.recv(4096)
            return d.split(b"\r\n")[0].decode(errors="replace") if d else "EOF"
        except Exception as e: return "FAIL " + type(e).__name__
    print("@@ persist first\t" + req(), flush=True)
    t = time.time()
    while not os.path.exists("/tmp/go") and time.time() - t < 120: time.sleep(0.2)
    print("@@ persist after revoke\t" + req(), flush=True)
elif mode == "nat":
    host_port, decoy = int(args[0]), int(args[1])
    checks = [
        ("tcp host 10.0.2.2:server", lambda: tcp("10.0.2.2", host_port, send=b"HELLO nat\n")),
        ("tcp host 10.0.2.2:decoy", lambda: tcp("10.0.2.2", decoy)),
        ("multi 4 concurrent", lambda: multi("10.0.2.2", host_port, 4)),
        ("bulk 16MiB", lambda: bulk("10.0.2.2", host_port, 16 << 20)),
        ("tcp Internet 1.1.1.1:443", lambda: tcp("1.1.1.1", 443)),
        ("dns udp 10.0.2.3", lambda: dnsq("10.0.2.3", "example.com")),
    ]
for name, f in checks:
    try: r = f()
    except Exception as e: r = "EXC " + type(e).__name__ + ": " + str(e)[:80]
    print("@@ %s\t%s" % (name, r), flush=True)
print("@@ done", flush=True)
"#;

fn results(out: &str) -> Vec<(String, String)> {
    out.lines()
        .filter_map(|l| l.find("@@ ").map(|i| &l[i + 3..]))
        .filter_map(|l| l.split_once('\t'))
        .map(|(a, b)| (a.trim().to_owned(), b.trim().to_owned()))
        .collect()
}

fn get<'a>(r: &'a [(String, String)], name: &str) -> &'a str {
    r.iter()
        .find(|(n, _)| n == name)
        .map(|(_, v)| v.as_str())
        .unwrap_or_else(|| panic!("no result for {name}: {r:?}"))
}

fn evidence(name: &str, text: &str) {
    let dir = lab().join("net").join("logs");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(name), text).unwrap();
}

fn wait_dhcp(g: &mut Guest) -> String {
    let t = Instant::now();
    loop {
        let r = g.cmd(
            "ip -color=never -4 -o addr show; ip -color=never -4 route",
            Duration::from_secs(20),
        );
        if r.contains("10.0.2.15/24") {
            return r;
        }
        assert!(t.elapsed() < Duration::from_secs(180), "no DHCP lease: {r}");
        std::thread::sleep(Duration::from_secs(3));
    }
}

/// Connects to QEMU's `-netdev stream` server socket.
fn connect_unix(path: &Path) -> socket2::Socket {
    let t = Instant::now();
    loop {
        let s = socket2::Socket::new(socket2::Domain::UNIX, socket2::Type::STREAM, None).unwrap();
        if s.connect(&socket2::SockAddr::unix(path).unwrap()).is_ok() {
            return s;
        }
        assert!(t.elapsed() < Duration::from_secs(20), "netdev socket");
        std::thread::sleep(Duration::from_millis(100));
    }
}
