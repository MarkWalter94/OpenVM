//! The stack: smoltcp interface, per-connection TCP proxy, DHCP and DNS
//! answers, limits and records. One thread per stack with its own
//! single-threaded tokio runtime (host connections are tasks on it).

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::socket::tcp;
use smoltcp::wire::{EthernetAddress, HardwareAddress, IpAddress, IpCidr, IpListenEndpoint};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{Notify, mpsc};

use crate::frame::{self, Class, Dropped};
use crate::transport::{FrameSink, Link};
use crate::{
    ConnOutcome, ConnRecord, DNS_IP, DeniedAttempt, DenyReason, GATEWAY_IP, GATEWAY_MAC,
    MAX_DENIED_RECORDS, MAX_PENDING_RECORDS, NetConfig, NetRecord, NetStats, PREFIX_LEN, dhcp, dns,
};

/// Receive buffer of a proxied guest connection (guest → host).
const SOCKET_RX: usize = 128 * 1024;
/// Send buffer of a proxied guest connection (host → guest).
const SOCKET_TX: usize = 128 * 1024;
/// Largest chunk moved in one step.
const CHUNK: usize = 32 * 1024;
/// Chunks queued per direction between the stack and a host connection.
const HOST_QUEUE: usize = 4;
/// A guest connection that stops acknowledging for this long is aborted.
const GUEST_TIMEOUT: Duration = Duration::from_secs(120);
/// Longest sleep of the loop (safety net; it is woken by frames, host data
/// and commands).
const MAX_SLEEP: Duration = Duration::from_millis(50);
/// Frames processed before the loop services the connections.
const FRAME_BATCH: usize = 256;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Error starting a stack.
#[derive(Debug)]
pub enum StackError {
    /// The configuration is not valid.
    Config(String),
    /// The thread or its runtime could not be created.
    Io(std::io::Error),
}

impl std::fmt::Display for StackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Config(m) => write!(f, "network configuration: {m}"),
            Self::Io(e) => write!(f, "network stack: {e}"),
        }
    }
}

impl std::error::Error for StackError {}

#[derive(Debug)]
enum Cmd {
    SetAllowed(BTreeSet<String>),
    Stop,
}

#[derive(Debug, Default)]
struct Shared {
    stats: Mutex<NetStats>,
    records: Mutex<VecDeque<NetRecord>>,
    allowed: Mutex<BTreeSet<String>>,
    running: std::sync::atomic::AtomicBool,
}

impl Shared {
    fn push(&self, r: NetRecord) {
        let mut q = lock(&self.records);
        if q.len() >= MAX_PENDING_RECORDS {
            q.pop_front();
            lock(&self.stats).records_dropped += 1;
        }
        q.push_back(r);
    }
}

/// A running stack. Dropping it stops it.
#[derive(Debug)]
pub struct NetStack {
    cmd: mpsc::UnboundedSender<Cmd>,
    shared: Arc<Shared>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl NetStack {
    /// Starts a stack on `link` with `allowed` endpoint names allowed
    /// (compared without regard to case; unknown names are ignored).
    pub fn start(
        config: NetConfig,
        link: Link,
        allowed: impl IntoIterator<Item = String>,
    ) -> Result<Self, StackError> {
        config.validate().map_err(StackError::Config)?;
        let allowed: BTreeSet<String> = allowed.into_iter().map(|n| dns::normalize(&n)).collect();
        let shared = Arc::new(Shared::default());
        *lock(&shared.allowed) = allowed.clone();
        shared
            .running
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let sh = shared.clone();
        let thread = std::thread::Builder::new()
            .name("guest-net".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build();
                if let Ok(rt) = rt {
                    let mut link = link;
                    let from_guest = std::mem::replace(&mut link.from_guest, mpsc::channel(1).1);
                    let stack = Stack::new(config, link.to_guest.clone(), allowed, sh.clone());
                    rt.block_on(stack.run(from_guest, cmd_rx));
                    link.close();
                }
                sh.running.store(false, std::sync::atomic::Ordering::SeqCst);
            })
            .map_err(StackError::Io)?;
        Ok(Self {
            cmd: cmd_tx,
            shared,
            thread: Some(thread),
        })
    }

    /// Replaces the allowed endpoint names. Connections to endpoints no
    /// longer allowed are reset at once ([`ConnOutcome::Revoked`]); their
    /// names stop resolving.
    pub fn set_allowed(&self, names: impl IntoIterator<Item = String>) {
        let set: BTreeSet<String> = names.into_iter().map(|n| dns::normalize(&n)).collect();
        *lock(&self.shared.allowed) = set.clone();
        let _ = self.cmd.send(Cmd::SetAllowed(set));
    }

    /// Endpoint names allowed now (normalized).
    #[must_use]
    pub fn allowed(&self) -> BTreeSet<String> {
        lock(&self.shared.allowed).clone()
    }

    /// Counters.
    #[must_use]
    pub fn stats(&self) -> NetStats {
        *lock(&self.shared.stats)
    }

    /// Records produced since the last call.
    #[must_use]
    pub fn take_records(&self) -> Vec<NetRecord> {
        lock(&self.shared.records).drain(..).collect()
    }

    /// `false` once the stack ended (stopped, or QEMU closed the link).
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.shared
            .running
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Stops the stack: every connection is reset ([`ConnOutcome::Stopped`])
    /// and the link closed. Waits for the thread (it ends within
    /// milliseconds). Records stay available to [`Self::take_records`].
    pub fn stop(&mut self) {
        let _ = self.cmd.send(Cmd::Stop);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for NetStack {
    fn drop(&mut self) {
        self.stop();
    }
}

// ----- smoltcp device -------------------------------------------------------

#[derive(Debug, Default)]
struct QueueDevice {
    rx: VecDeque<Vec<u8>>,
    tx: Vec<Vec<u8>>,
}

struct Rx(Vec<u8>);
struct Tx<'a>(&'a mut Vec<Vec<u8>>);

impl RxToken for Rx {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&[u8]) -> R,
    {
        f(&self.0)
    }
}

impl TxToken for Tx<'_> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut b = vec![0u8; len];
        let r = f(&mut b);
        self.0.push(b);
        r
    }
}

impl Device for QueueDevice {
    type RxToken<'a> = Rx;
    type TxToken<'a> = Tx<'a>;

    fn receive(
        &mut self,
        _: smoltcp::time::Instant,
    ) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let f = self.rx.pop_front()?;
        Some((Rx(f), Tx(&mut self.tx)))
    }

    fn transmit(&mut self, _: smoltcp::time::Instant) -> Option<Self::TxToken<'_>> {
        Some(Tx(&mut self.tx))
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut c = DeviceCapabilities::default();
        c.medium = Medium::Ethernet;
        c.max_transmission_unit = 1514;
        c
    }
}

// ----- connections ----------------------------------------------------------

#[derive(Debug)]
enum HostMsg {
    Connected,
    ConnectFailed,
    Data(Vec<u8>),
    Eof,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HostState {
    NotStarted,
    Connecting,
    Connected,
    Done,
}

#[derive(Debug)]
struct Conn {
    handle: SocketHandle,
    ep: usize,
    guest_port: u16,
    opened_at: SystemTime,
    host: HostState,
    to_host: Option<mpsc::Sender<Vec<u8>>>,
    from_host: Option<mpsc::Receiver<HostMsg>>,
    task: Option<tokio::task::AbortHandle>,
    pending: Option<(Vec<u8>, usize)>,
    host_eof: bool,
    close_sent: bool,
    bytes_from_guest: u64,
    bytes_to_guest: u64,
    outcome: Option<ConnOutcome>,
    /// Counted in `open_connections`.
    counted: bool,
}

/// Token bucket shared by every connection, both directions.
#[derive(Debug)]
struct Bucket {
    rate: u64,
    tokens: u64,
    last: Instant,
}

impl Bucket {
    fn new(rate: u64) -> Self {
        Self {
            rate,
            tokens: rate.min(Self::burst(rate)),
            last: Instant::now(),
        }
    }

    fn burst(rate: u64) -> u64 {
        rate.max(64 * 1024)
    }

    fn refill(&mut self) {
        let now = Instant::now();
        let dt = now.duration_since(self.last);
        let add = u64::try_from(dt.as_micros())
            .unwrap_or(u64::MAX)
            .saturating_mul(self.rate)
            / 1_000_000;
        if add > 0 {
            self.tokens = (self.tokens + add).min(Self::burst(self.rate));
            self.last = now;
        }
    }

    /// Time until `n` tokens are available.
    fn wait_for(&self, n: u64) -> Duration {
        if self.tokens >= n || self.rate == 0 {
            return Duration::ZERO;
        }
        Duration::from_micros((n - self.tokens).saturating_mul(1_000_000) / self.rate)
    }
}

struct Stack {
    cfg: NetConfig,
    iface: Interface,
    dev: QueueDevice,
    sockets: SocketSet<'static>,
    owned: BTreeSet<Ipv4Addr>,
    by_vaddr: HashMap<SocketAddrV4, usize>,
    names: Vec<String>,
    allowed: BTreeSet<String>,
    conns: Vec<Conn>,
    shared: Arc<Shared>,
    sink: FrameSink,
    t0: Instant,
    bucket: Option<Bucket>,
    notify: Arc<Notify>,
    stats: NetStats,
    denied_records: usize,
}

impl Stack {
    fn new(
        cfg: NetConfig,
        sink: FrameSink,
        allowed: BTreeSet<String>,
        shared: Arc<Shared>,
    ) -> Self {
        let mut dev = QueueDevice::default();
        let t0 = Instant::now();
        let mut config = Config::new(HardwareAddress::Ethernet(EthernetAddress(GATEWAY_MAC)));
        config.random_seed = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x5eed);
        let mut iface = Interface::new(config, &mut dev, smoltcp::time::Instant::from_micros(0));
        iface.update_ip_addrs(|a| {
            let _ = a.push(IpCidr::new(IpAddress::Ipv4(GATEWAY_IP), PREFIX_LEN));
        });
        // The frame filter decides what reaches smoltcp; "any IP" lets it
        // terminate connections addressed to the virtual addresses.
        iface.set_any_ip(true);
        let owned = cfg.endpoints.iter().map(|e| e.virtual_ip).collect();
        let by_vaddr = cfg
            .endpoints
            .iter()
            .enumerate()
            .map(|(i, e)| (e.virtual_addr(), i))
            .collect();
        let names = cfg
            .endpoints
            .iter()
            .map(|e| dns::normalize(&e.name))
            .collect();
        let bucket = cfg.max_bytes_per_second.map(Bucket::new);
        Self {
            cfg,
            iface,
            dev,
            sockets: SocketSet::new(Vec::new()),
            owned,
            by_vaddr,
            names,
            allowed,
            conns: Vec::new(),
            shared,
            sink,
            t0,
            bucket,
            notify: Arc::new(Notify::new()),
            stats: NetStats::default(),
            denied_records: 0,
        }
    }

    fn now(&self) -> smoltcp::time::Instant {
        smoltcp::time::Instant::from_micros(
            i64::try_from(self.t0.elapsed().as_micros()).unwrap_or(i64::MAX),
        )
    }

    async fn run(
        mut self,
        mut from_guest: mpsc::Receiver<Vec<u8>>,
        mut cmd: mpsc::UnboundedReceiver<Cmd>,
    ) {
        let notify = self.notify.clone();
        loop {
            let delay = self.next_delay();
            tokio::select! {
                biased;
                c = cmd.recv() => match c {
                    Some(Cmd::SetAllowed(set)) => self.apply_allowed(set),
                    Some(Cmd::Stop) | None => break,
                },
                f = from_guest.recv() => match f {
                    Some(f) => {
                        self.on_frame(f);
                        for _ in 0..FRAME_BATCH {
                            match from_guest.try_recv() {
                                Ok(f) => self.on_frame(f),
                                Err(_) => break,
                            }
                        }
                    }
                    None => break,
                },
                () = notify.notified() => {}
                () = tokio::time::sleep(delay) => {}
            }
            self.pump();
        }
        self.shutdown();
    }

    fn next_delay(&mut self) -> Duration {
        let now = self.now();
        let mut d = self
            .iface
            .poll_delay(now, &self.sockets)
            .map_or(MAX_SLEEP, |d| Duration::from_micros(d.total_micros()));
        if let Some(b) = &self.bucket
            && b.tokens == 0
        {
            d = d.min(b.wait_for(16 * 1024).max(Duration::from_millis(1)));
        }
        d.min(MAX_SLEEP)
    }

    fn poll(&mut self) {
        let now = self.now();
        self.iface.poll(now, &mut self.dev, &mut self.sockets);
        for f in self.dev.tx.drain(..) {
            self.stats.frames_out += 1;
            let _ = self.sink.send(f);
        }
    }

    fn send_frame(&mut self, f: Vec<u8>) {
        self.stats.frames_out += 1;
        let _ = self.sink.send(f);
    }

    fn deny(&mut self, destination: SocketAddrV4, reason: DenyReason) {
        self.stats.refused += 1;
        if self.denied_records < MAX_DENIED_RECORDS {
            self.denied_records += 1;
            self.shared.push(NetRecord::Denied(DeniedAttempt {
                at: SystemTime::now(),
                destination,
                reason,
            }));
        }
    }

    fn on_frame(&mut self, f: Vec<u8>) {
        self.stats.frames_in += 1;
        match frame::classify(&f, &self.owned) {
            Class::Stack => self.feed(f),
            Class::TcpSyn { dst, src_port } => {
                self.on_syn(dst, src_port);
                self.feed(f);
            }
            Class::Dhcp { payload } => self.on_dhcp(&payload),
            Class::Dns {
                payload,
                src_mac,
                src,
            } => self.on_dns(&payload, src_mac, src),
            Class::Drop(Dropped::Udp) => self.stats.udp_dropped += 1,
            Class::Drop(Dropped::Ipv6) => self.stats.ipv6_dropped += 1,
            Class::Drop(Dropped::TcpOutside(dst)) => self.deny(dst, DenyReason::NotAnEndpoint),
            Class::Drop(Dropped::Other) => self.stats.other_dropped += 1,
        }
    }

    fn feed(&mut self, f: Vec<u8>) {
        self.dev.rx.push_back(f);
        self.poll();
    }

    fn live_connections(&self) -> usize {
        self.conns.iter().filter(|c| c.outcome.is_none()).count()
    }

    /// A SYN to one of the stack's addresses: creates the listener that
    /// will take it if the endpoint is allowed and under the limit;
    /// otherwise smoltcp resets it.
    fn on_syn(&mut self, dst: SocketAddrV4, src_port: u16) {
        let Some(&ep) = self.by_vaddr.get(&dst) else {
            self.deny(dst, DenyReason::NotAnEndpoint);
            return;
        };
        // A retransmitted SYN of a connection already being set up.
        if self
            .conns
            .iter()
            .any(|c| c.ep == ep && c.guest_port == src_port && c.outcome.is_none())
        {
            return;
        }
        if !self.allowed.contains(&self.names[ep]) {
            self.deny(dst, DenyReason::NotAllowed);
            return;
        }
        if self.live_connections() >= self.cfg.max_connections {
            self.deny(dst, DenyReason::ConnectionLimit);
            return;
        }
        let mut s = tcp::Socket::new(
            tcp::SocketBuffer::new(vec![0u8; SOCKET_RX]),
            tcp::SocketBuffer::new(vec![0u8; SOCKET_TX]),
        );
        s.set_nagle_enabled(false);
        s.set_timeout(Some(smoltcp::time::Duration::from_secs(
            GUEST_TIMEOUT.as_secs(),
        )));
        if s.listen(IpListenEndpoint {
            addr: Some(IpAddress::Ipv4(*dst.ip())),
            port: dst.port(),
        })
        .is_err()
        {
            self.stats.other_dropped += 1;
            return;
        }
        let handle = self.sockets.add(s);
        self.conns.push(Conn {
            handle,
            ep,
            guest_port: src_port,
            opened_at: SystemTime::now(),
            host: HostState::NotStarted,
            to_host: None,
            from_host: None,
            task: None,
            pending: None,
            host_eof: false,
            close_sent: false,
            bytes_from_guest: 0,
            bytes_to_guest: 0,
            outcome: None,
            counted: false,
        });
    }

    fn on_dhcp(&mut self, payload: &[u8]) {
        let Some(m) = dhcp::parse(payload) else {
            self.stats.udp_dropped += 1;
            return;
        };
        let Some(kind) = dhcp::reply_type(&m) else {
            return;
        };
        let reply = dhcp::build_reply(&m, kind);
        let f = frame::udp_frame(
            [0xff; 6],
            SocketAddrV4::new(GATEWAY_IP, 67),
            SocketAddrV4::new(Ipv4Addr::BROADCAST, 68),
            &reply,
        );
        self.stats.dhcp_replies += 1;
        self.send_frame(f);
    }

    fn on_dns(&mut self, payload: &[u8], src_mac: [u8; 6], src: SocketAddrV4) {
        let Some(q) = dns::parse_query(payload) else {
            self.stats.udp_dropped += 1;
            return;
        };
        let resolve = |name: &str| {
            self.names
                .iter()
                .position(|n| n == name)
                .filter(|_| self.allowed.contains(name))
                .map(|i| self.cfg.endpoints[i].virtual_ip)
        };
        let answer = dns::decide(&q, &resolve);
        match answer {
            dns::Answer::Address(_) => self.stats.dns_answered += 1,
            _ => self.stats.dns_refused += 1,
        }
        let reply = dns::build_response(payload, &q, answer);
        let f = frame::udp_frame(src_mac, SocketAddrV4::new(DNS_IP, 53), src, &reply);
        self.send_frame(f);
    }

    fn apply_allowed(&mut self, set: BTreeSet<String>) {
        self.allowed = set;
        for i in 0..self.conns.len() {
            let ep = self.conns[i].ep;
            if self.conns[i].outcome.is_none() && !self.allowed.contains(&self.names[ep]) {
                self.end(i, ConnOutcome::Revoked);
            }
        }
        self.poll();
    }

    /// Resets the guest connection and ends the host one.
    fn end(&mut self, i: usize, outcome: ConnOutcome) {
        let c = &mut self.conns[i];
        if c.outcome.is_none() {
            c.outcome = Some(outcome);
        }
        if let Some(t) = c.task.take() {
            t.abort();
        }
        c.to_host = None;
        c.from_host = None;
        c.host = HostState::Done;
        self.sockets.get_mut::<tcp::Socket<'_>>(c.handle).abort();
    }

    fn start_host(&mut self, i: usize) {
        let target = self.cfg.endpoints[self.conns[i].ep].target;
        let timeout = self.cfg.connect_timeout;
        let (to_host_tx, mut to_host_rx) = mpsc::channel::<Vec<u8>>(HOST_QUEUE);
        let (msg_tx, msg_rx) = mpsc::channel::<HostMsg>(HOST_QUEUE);
        let notify = self.notify.clone();
        let task = tokio::spawn(async move {
            let stream =
                match tokio::time::timeout(timeout, tokio::net::TcpStream::connect(target)).await {
                    Ok(Ok(s)) => s,
                    _ => {
                        let _ = msg_tx.send(HostMsg::ConnectFailed).await;
                        notify.notify_one();
                        return;
                    }
                };
            let _ = stream.set_nodelay(true);
            let (mut r, mut w) = stream.into_split();
            if msg_tx.send(HostMsg::Connected).await.is_err() {
                return;
            }
            notify.notify_one();
            let (wtx, wn) = (msg_tx.clone(), notify.clone());
            let writer = async move {
                while let Some(buf) = to_host_rx.recv().await {
                    if w.write_all(&buf).await.is_err() {
                        let _ = wtx.send(HostMsg::Error).await;
                        wn.notify_one();
                        return;
                    }
                    // Room in the queue: the stack may read more from the guest.
                    wn.notify_one();
                }
                let _ = w.shutdown().await;
            };
            let reader = async move {
                let mut buf = vec![0u8; CHUNK];
                loop {
                    let msg = match r.read(&mut buf).await {
                        Ok(0) => HostMsg::Eof,
                        Ok(n) => HostMsg::Data(buf[..n].to_vec()),
                        Err(_) => HostMsg::Error,
                    };
                    let last = !matches!(msg, HostMsg::Data(_));
                    if msg_tx.send(msg).await.is_err() {
                        return;
                    }
                    notify.notify_one();
                    if last {
                        return;
                    }
                }
            };
            tokio::join!(writer, reader);
        });
        let c = &mut self.conns[i];
        c.task = Some(task.abort_handle());
        c.to_host = Some(to_host_tx);
        c.from_host = Some(msg_rx);
        c.host = HostState::Connecting;
    }

    fn budget(&mut self) -> usize {
        match &mut self.bucket {
            Some(b) => {
                b.refill();
                usize::try_from(b.tokens).unwrap_or(usize::MAX)
            }
            None => usize::MAX,
        }
    }

    fn spend(&mut self, n: usize) {
        if let Some(b) = &mut self.bucket {
            b.tokens = b.tokens.saturating_sub(n as u64);
        }
    }

    /// Moves data between the guest sockets and the host connections,
    /// starts host connections, closes and removes finished ones.
    fn pump(&mut self) {
        self.poll();
        for i in 0..self.conns.len() {
            self.service(i);
        }
        self.poll();
        // Finished connections: socket closed (or in TIME-WAIT) on the
        // guest side, or never left LISTEN (an invalid SYN).
        let mut i = 0;
        while i < self.conns.len() {
            let c = &self.conns[i];
            let state = self.sockets.get::<tcp::Socket<'_>>(c.handle).state();
            let finished = match state {
                tcp::State::Listen => c.host == HostState::NotStarted,
                tcp::State::Closed | tcp::State::TimeWait => true,
                _ => false,
            };
            if finished {
                let c = self.conns.swap_remove(i);
                self.finish(c, state);
            } else {
                i += 1;
            }
        }
        self.publish();
    }

    fn finish(&mut self, mut c: Conn, state: tcp::State) {
        if let Some(t) = c.task.take() {
            t.abort();
        }
        self.sockets.remove(c.handle);
        if c.counted {
            self.stats.open_connections = self.stats.open_connections.saturating_sub(1);
        }
        if c.host == HostState::NotStarted && c.outcome.is_none() {
            return; // listener that never took its SYN
        }
        let outcome = c.outcome.unwrap_or(match state {
            tcp::State::TimeWait => ConnOutcome::Closed,
            _ if c.close_sent => ConnOutcome::Closed,
            _ => ConnOutcome::ResetByGuest,
        });
        let e = &self.cfg.endpoints[c.ep];
        self.shared.push(NetRecord::Connection(ConnRecord {
            endpoint: e.name.clone(),
            virtual_addr: e.virtual_addr(),
            target: e.target,
            guest_port: c.guest_port,
            opened_at: c.opened_at,
            closed_at: SystemTime::now(),
            bytes_from_guest: c.bytes_from_guest,
            bytes_to_guest: c.bytes_to_guest,
            outcome,
        }));
    }

    fn service(&mut self, i: usize) {
        let handle = self.conns[i].handle;
        if self.conns[i].outcome.is_some() {
            return;
        }
        let state = self.sockets.get::<tcp::Socket<'_>>(handle).state();
        if self.conns[i].host == HostState::NotStarted {
            if matches!(state, tcp::State::Listen | tcp::State::Closed) {
                return;
            }
            self.stats.connections += 1;
            self.stats.open_connections += 1;
            self.conns[i].counted = true;
            self.start_host(i);
        }
        // Host → guest.
        loop {
            if let Some((buf, off)) = self.conns[i].pending.take() {
                let budget = self.budget();
                let s = self.sockets.get_mut::<tcp::Socket<'_>>(handle);
                let room = buf.len() - off;
                let n = if s.can_send() {
                    s.send_slice(&buf[off..off + room.min(budget)]).unwrap_or(0)
                } else {
                    0
                };
                self.spend(n);
                self.conns[i].bytes_to_guest += n as u64;
                self.stats.bytes_to_guest += n as u64;
                if off + n < buf.len() {
                    self.conns[i].pending = Some((buf, off + n));
                    break;
                }
            }
            let Some(rx) = self.conns[i].from_host.as_mut() else {
                break;
            };
            match rx.try_recv() {
                Ok(HostMsg::Connected) => self.conns[i].host = HostState::Connected,
                Ok(HostMsg::ConnectFailed) => {
                    self.end(i, ConnOutcome::TargetUnreachable);
                    return;
                }
                Ok(HostMsg::Error) => {
                    self.end(i, ConnOutcome::TargetError);
                    return;
                }
                Ok(HostMsg::Data(v)) => self.conns[i].pending = Some((v, 0)),
                Ok(HostMsg::Eof) | Err(mpsc::error::TryRecvError::Disconnected) => {
                    self.conns[i].host_eof = true;
                    self.conns[i].from_host = None;
                    break;
                }
                Err(mpsc::error::TryRecvError::Empty) => break,
            }
        }
        let c = &mut self.conns[i];
        if c.host_eof && c.pending.is_none() && !c.close_sent {
            c.close_sent = true;
            self.sockets.get_mut::<tcp::Socket<'_>>(handle).close();
        }
        // Guest → host.
        if self.conns[i].host != HostState::Connected {
            return;
        }
        loop {
            let budget = self.budget();
            let c = &mut self.conns[i];
            let Some(tx) = c.to_host.as_ref() else {
                break;
            };
            if tx.capacity() == 0 || budget == 0 {
                break;
            }
            let s = self.sockets.get_mut::<tcp::Socket<'_>>(handle);
            if !s.can_recv() {
                if !s.may_recv() {
                    // FIN from the guest: half-close towards the host.
                    c.to_host = None;
                }
                break;
            }
            let chunk = s
                .recv(|b| {
                    let n = b.len().min(CHUNK).min(budget);
                    (n, b[..n].to_vec())
                })
                .unwrap_or_default();
            if chunk.is_empty() {
                break;
            }
            let n = chunk.len();
            let _ = tx.try_send(chunk);
            c.bytes_from_guest += n as u64;
            self.stats.bytes_from_guest += n as u64;
            self.spend(n);
        }
    }

    fn publish(&self) {
        let mut s = lock(&self.shared.stats);
        *s = NetStats {
            records_dropped: s.records_dropped,
            ..self.stats
        };
    }

    fn shutdown(&mut self) {
        for i in 0..self.conns.len() {
            if self.conns[i].outcome.is_none() {
                self.end(i, ConnOutcome::Stopped);
            }
        }
        self.poll();
        for c in std::mem::take(&mut self.conns) {
            let state = self.sockets.get::<tcp::Socket<'_>>(c.handle).state();
            self.finish(c, state);
        }
        self.publish();
    }
}
