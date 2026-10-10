//! Per-run display proxy with ticket (ADR-0013, ROADMAP 6.4).
//!
//! QEMU serves its display on `vnc.sock` in the run's private directory,
//! known only to the supervisor. For the console the supervisor issues a
//! **display ticket** per run and VM: 32 random bytes (the generator of the
//! IPC credentials), distinct from the QMP channel and from the IPC
//! credential, and opens a proxy listener for that run:
//!
//! - the listener is an AF_UNIX socket `d.sock` in a fresh per-proxy
//!   directory under the display root, whose ACL grants access to the
//!   current user only (Windows: protected DACL with one ACE for the user's
//!   SID, inherited by the per-proxy directories; Unix: mode `0700`);
//! - a client connects, writes the token as 64 lowercase hexadecimal digits
//!   followed by `\n` within [`HANDSHAKE_TIMEOUT`], then speaks RFB; the
//!   token is compared in constant time; anything else closes the connection
//!   without a reply and without contacting QEMU;
//! - after the check the proxy connects to QEMU's `vnc.sock`; server →
//!   client bytes are forwarded as they are, client → server bytes are
//!   **parsed and filtered** ([`ClientFilter`]): the handshake must be
//!   exactly the one of the read-only RFB client (version `RFB 003.008`,
//!   security type None, ClientInit), then only SetPixelFormat (0),
//!   SetEncodings (2, at most [`MAX_ENCODINGS`]) and
//!   FramebufferUpdateRequest (3) are forwarded, whole. KeyEvent (4),
//!   PointerEvent (5), ClientCutText (6) or any other message close the
//!   connection without forwarding it: a ticket holder cannot inject input
//!   or clipboard into the guest bypassing the core (input goes through IPC,
//!   the core's checks and audit, and QMP);
//! - **one viewer per run**: a new authenticated connection closes the
//!   previous one ("last wins"), so a stale console never blocks a reconnect;
//!   at most [`MAX_PENDING_HANDSHAKES`] unauthenticated connections at once;
//! - **one valid ticket per run**: issuing a new ticket for a VM revokes the
//!   previous one;
//! - **revocation**: when the run ends or is replaced (the supervisor
//!   compares state and run of every VM at each publication), on Emergency Stop, when the UI connection
//!   that obtained the ticket is revoked or closed, and at service shutdown:
//!   the token becomes invalid, the listener and its directory are removed
//!   and live connections are closed (`shutdown(Both)` on the client socket
//!   and on the QEMU socket, on the same handles the pumps read from, so
//!   both connection threads end even if the client keeps its end open);
//!   the revoked runs are returned to the caller for the audit
//!   (`console_revoke`).
//!
//! Threads: one accept thread per proxy and two threads per live connection,
//! each end used through a single shared handle (F0-04: on Windows a read
//! timeout or a shutdown on a duplicate handle breaks AF_UNIX streams, and a
//! shutdown does not unblock a `recv` in progress). Once authenticated, both
//! sockets are non-blocking and the pumps poll a per-connection stop flag
//! between reads (pauses of at most 4 ms when idle), so a revocation ends
//! both threads even with a client that never sends nor reads.
//! [`DisplayRegistry::connection_threads`] counts the live connection
//! threads (diagnostics and tests).
//!
//! The display root is (re)created with its ACL whenever it is missing (a
//! failure is not cached); at service start
//! [`DisplayRegistry::remove_stale`] removes per-proxy directories left by
//! a previous service.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use lightvm_proto::{RunId, VmId};
use socket2::{Domain, SockAddr, Socket, Type};

use crate::credential::Credential;

/// Time allowed to send the token after connecting.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
/// Unauthenticated connections accepted at the same time per proxy.
pub const MAX_PENDING_HANDSHAKES: usize = 4;
/// Name of the listener socket in the per-proxy directory.
pub const SOCKET_NAME: &str = "d.sock";
/// Encodings accepted in one SetEncodings message (the RFB client sends 5).
pub const MAX_ENCODINGS: usize = 64;
/// Length of the token line: 64 hexadecimal digits and `\n`.
const TOKEN_LINE: usize = 65;
/// Length of the name of a per-proxy directory (6 random bytes in hex).
const PROXY_DIR_NAME_LEN: usize = 12;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Ticket issued for a run: socket to connect to and token (secret).
#[derive(Clone)]
pub struct IssuedTicket {
    /// Listener socket of the proxy.
    pub socket_path: PathBuf,
    /// Token in hexadecimal (64 characters).
    pub token: String,
    /// Run of the previous ticket of the VM, revoked by this issue.
    pub replaced: Option<RunId>,
}

impl std::fmt::Debug for IssuedTicket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IssuedTicket")
            .field("socket_path", &self.socket_path)
            .field("token", &"<redacted>")
            .field("replaced", &self.replaced)
            .finish()
    }
}

/// Live forwarded connection (client and QEMU ends).
struct Live {
    client: Arc<Socket>,
    upstream: Arc<Socket>,
    /// Raised on revocation or replacement: both pumps stop.
    stop: Arc<AtomicBool>,
}

impl Live {
    /// Stops both pumps and closes both ends (`shutdown(Both)` on the
    /// handles the pumps use). The pumps work on non-blocking sockets and
    /// check `stop` between reads and writes: on Windows a shutdown does not
    /// unblock a `recv` already in progress on an AF_UNIX socket (F0-04), so
    /// a blocking read on a client that keeps its end open and never sends
    /// would keep its thread forever.
    fn close(&self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.client.shutdown(Shutdown::Both);
        let _ = self.upstream.shutdown(Shutdown::Both);
    }
}

/// Longest pause of a pump waiting for data on its non-blocking socket.
const POLL_MAX: Duration = Duration::from_millis(4);

/// Read on a non-blocking socket that waits for data (short pauses), EOF,
/// an error or `stop` (reported as EOF).
fn read_nb(s: &Socket, buf: &mut [u8], stop: &AtomicBool) -> io::Result<usize> {
    let mut idle = Duration::from_micros(250);
    loop {
        if stop.load(Ordering::SeqCst) {
            return Ok(0);
        }
        match (&*s).read(buf) {
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(idle);
                idle = (idle * 2).min(POLL_MAX);
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            r => return r,
        }
    }
}

/// `write_all` on a non-blocking socket; gives up on `stop` (a peer that
/// stops reading never holds the thread after a revocation).
fn write_all_nb(s: &Socket, mut data: &[u8], stop: &AtomicBool) -> io::Result<()> {
    let mut idle = Duration::from_micros(250);
    while !data.is_empty() {
        if stop.load(Ordering::SeqCst) {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        match (&*s).write(data) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(n) => {
                data = &data[n..];
                idle = Duration::from_micros(250);
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(idle);
                idle = (idle * 2).min(POLL_MAX);
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

struct ProxyInner {
    vm_id: VmId,
    run_id: RunId,
    token: Credential,
    upstream: PathBuf,
    socket_path: PathBuf,
    dir: PathBuf,
    closed: AtomicBool,
    pending: AtomicUsize,
    live: Mutex<Option<Live>>,
    /// Live connection threads of the whole registry.
    threads: Arc<AtomicUsize>,
}

impl ProxyInner {
    /// Revocation: the token is no longer accepted, the live connection is
    /// closed, the accept thread is woken up and the directory removed.
    fn shut(&self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Some(l) = lock(&self.live).take() {
            l.close();
        }
        // Wakes the blocking `accept`, which sees `closed` and exits.
        if let Ok(s) = Socket::new(Domain::UNIX, Type::STREAM, None)
            && let Ok(addr) = SockAddr::unix(&self.socket_path)
        {
            let _ = s.connect(&addr);
        }
    }
}

struct Proxy {
    owner: u64,
    inner: Arc<ProxyInner>,
}

impl Proxy {
    fn shut(self) -> (VmId, RunId) {
        self.inner.shut();
        (self.inner.vm_id, self.inner.run_id)
    }
}

/// Registry of the display proxies (one per VM with a ticket).
pub struct DisplayRegistry {
    root: PathBuf,
    /// The root has been created with the user-only ACL. Never caches a
    /// failure: the next ticket tries again, and a root removed meanwhile
    /// is recreated.
    prepared: Mutex<bool>,
    proxies: Mutex<HashMap<VmId, Proxy>>,
    threads: Arc<AtomicUsize>,
}

impl std::fmt::Debug for DisplayRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DisplayRegistry")
            .field("root", &self.root)
            .field("proxies", &lock(&self.proxies).len())
            .finish()
    }
}

/// Counts a live connection thread for its whole life.
struct ThreadSlot(Arc<AtomicUsize>);

impl ThreadSlot {
    fn new(counter: &Arc<AtomicUsize>) -> Self {
        counter.fetch_add(1, Ordering::SeqCst);
        Self(counter.clone())
    }
}

impl Drop for ThreadSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl DisplayRegistry {
    /// Registry with the display root (created with the user-only ACL at the
    /// first ticket). The root must be short: AF_UNIX paths are limited to
    /// 107 bytes and the proxy adds 21 characters.
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            prepared: Mutex::new(false),
            proxies: Mutex::new(HashMap::new()),
            threads: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Display root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Live connection threads (authenticated or in handshake).
    pub fn connection_threads(&self) -> usize {
        self.threads.load(Ordering::SeqCst)
    }

    /// Removes the per-proxy directories left under the root by a previous
    /// service (crash, forced termination): only directories whose name has
    /// the per-proxy form (12 hexadecimal digits); links and other entries
    /// are left alone. To be called at startup, before the first ticket,
    /// while holding the catalog's instance lock. Returns the directories
    /// removed.
    pub fn remove_stale(&self) -> usize {
        let Ok(rd) = std::fs::read_dir(&self.root) else {
            return 0;
        };
        let live: Vec<PathBuf> = lock(&self.proxies)
            .values()
            .map(|p| p.inner.dir.clone())
            .collect();
        let mut n = 0;
        for e in rd.filter_map(Result::ok) {
            let name = e.file_name();
            let Some(name) = name.to_str() else { continue };
            let per_proxy = name.len() == PROXY_DIR_NAME_LEN
                && name
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
            let path = e.path();
            if !per_proxy || live.contains(&path) {
                continue;
            }
            match std::fs::symlink_metadata(&path) {
                Ok(m) if m.is_dir() && !crate::adapters::catalog::is_link(&m) => {
                    match std::fs::remove_dir_all(&path) {
                        Ok(()) => n += 1,
                        Err(err) => crate::diag!(
                            "lightvm-service: stale display directory not removed: {err}"
                        ),
                    }
                }
                _ => {}
            }
        }
        n
    }

    fn prepare_root(&self) -> io::Result<()> {
        let mut prepared = lock(&self.prepared);
        let present = std::fs::symlink_metadata(&self.root)
            .is_ok_and(|m| m.is_dir() && !crate::adapters::catalog::is_link(&m));
        if *prepared && present {
            return Ok(());
        }
        *prepared = false;
        restrict_dir(&self.root)?;
        *prepared = true;
        Ok(())
    }

    /// Issues a new ticket for the run, revoking any previous ticket of the
    /// VM, and starts the proxy towards `upstream` (QEMU's `vnc.sock`).
    pub fn issue(
        &self,
        vm_id: VmId,
        run_id: RunId,
        owner: u64,
        upstream: &Path,
    ) -> io::Result<IssuedTicket> {
        self.prepare_root()?;
        let token = Credential::generate()?;
        let mut name = [0u8; PROXY_DIR_NAME_LEN / 2];
        getrandom::fill(&mut name).map_err(io::Error::other)?;
        let dir = self.root.join(crate::credential::hex_encode(&name));
        std::fs::create_dir(&dir)?;
        let socket_path = dir.join(SOCKET_NAME);
        let listener = match bind(&socket_path) {
            Ok(l) => l,
            Err(e) => {
                let _ = std::fs::remove_dir_all(&dir);
                return Err(e);
            }
        };
        let hex = token.to_hex();
        let inner = Arc::new(ProxyInner {
            vm_id,
            run_id,
            token,
            upstream: upstream.to_path_buf(),
            socket_path: socket_path.clone(),
            dir,
            closed: AtomicBool::new(false),
            pending: AtomicUsize::new(0),
            live: Mutex::new(None),
            threads: self.threads.clone(),
        });
        let acc = inner.clone();
        if let Err(e) = std::thread::Builder::new()
            .name("lightvm-display-accept".into())
            .spawn(move || accept_loop(listener, acc))
        {
            let _ = std::fs::remove_dir_all(&inner.dir);
            return Err(e);
        }
        let previous = lock(&self.proxies).insert(vm_id, Proxy { owner, inner });
        let replaced = previous.map(|p| p.shut().1);
        Ok(IssuedTicket {
            socket_path,
            token: hex,
            replaced,
        })
    }

    /// Revokes the ticket of a VM (run ended or VM no longer running);
    /// returns the revoked run.
    pub fn revoke_vm(&self, vm_id: VmId) -> Option<(VmId, RunId)> {
        let p = lock(&self.proxies).remove(&vm_id);
        p.map(Proxy::shut)
    }

    /// Revokes the ticket of a VM only if it belongs to another run than
    /// `current` (`None`: the VM has no run with a process).
    pub fn revoke_other_runs(&self, vm_id: VmId, current: Option<RunId>) -> Option<(VmId, RunId)> {
        let stale = {
            let mut m = lock(&self.proxies);
            let stale = m
                .get(&vm_id)
                .is_some_and(|p| Some(p.inner.run_id) != current);
            if stale { m.remove(&vm_id) } else { None }
        };
        stale.map(Proxy::shut)
    }

    /// Revokes the tickets obtained by a UI connection (revoked or closed);
    /// returns the runs involved (their pressed keys are to be released).
    pub fn revoke_owner(&self, owner: u64) -> Vec<(VmId, RunId)> {
        let gone: Vec<Proxy> = {
            let mut m = lock(&self.proxies);
            let ids: Vec<VmId> = m
                .iter()
                .filter(|(_, p)| p.owner == owner)
                .map(|(id, _)| *id)
                .collect();
            ids.iter().filter_map(|id| m.remove(id)).collect()
        };
        gone.into_iter().map(Proxy::shut).collect()
    }

    /// Revokes every ticket (Emergency Stop, shutdown); returns the runs.
    pub fn revoke_all(&self) -> Vec<(VmId, RunId)> {
        let all: Vec<Proxy> = lock(&self.proxies).drain().map(|(_, p)| p).collect();
        all.into_iter().map(Proxy::shut).collect()
    }

    /// VMs with a valid ticket and their run (diagnostics and tests).
    pub fn active(&self) -> Vec<(VmId, RunId)> {
        lock(&self.proxies)
            .values()
            .map(|p| (p.inner.vm_id, p.inner.run_id))
            .collect()
    }
}

impl Drop for DisplayRegistry {
    fn drop(&mut self) {
        self.revoke_all();
    }
}

/// Prefix of the fallback private roots in the temporary directory
/// (`lightvm-p-<pid>`, used when the catalog path is too long for the
/// AF_UNIX sockets).
pub const FALLBACK_ROOT_PREFIX: &str = "lightvm-p-";

/// Removes the fallback private roots (`<dir>/lightvm-p-<pid>`) left by
/// previous services whose process is no longer running (sockets and
/// display directories of a dead service). The current process and any PID
/// that may still be running are left alone; links are never followed.
/// Returns the directories removed.
pub fn remove_stale_fallback_roots(dir: &Path) -> usize {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return 0;
    };
    let me = std::process::id();
    let mut n = 0;
    for e in rd.filter_map(Result::ok) {
        let name = e.file_name();
        let Some(pid) = name
            .to_str()
            .and_then(|s| s.strip_prefix(FALLBACK_ROOT_PREFIX))
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        if pid == me || qemu_backend::prepare::pid_may_be_running(pid) {
            continue;
        }
        let path = e.path();
        let is_dir = std::fs::symlink_metadata(&path)
            .is_ok_and(|m| m.is_dir() && !crate::adapters::catalog::is_link(&m));
        if is_dir && std::fs::remove_dir_all(&path).is_ok() {
            n += 1;
        }
    }
    n
}

fn bind(path: &Path) -> io::Result<Socket> {
    let s = Socket::new(Domain::UNIX, Type::STREAM, None)?;
    s.bind(&SockAddr::unix(path)?)?;
    s.listen(8)?;
    Ok(s)
}

/// Directory accessible to the current user only.
fn restrict_dir(dir: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        let managed = qemu_backend::ManagedPath::verify(dir.to_path_buf())
            .map_err(|e| io::Error::other(e.to_string()))?;
        qemu_backend::prepare::restrict_to_current_user(&managed)
            .map(|_| ())
            .map_err(|e| io::Error::other(e.to_string()))
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = dir;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "private display directory not supported on this system",
        ))
    }
}

fn accept_loop(listener: Socket, p: Arc<ProxyInner>) {
    loop {
        let conn = listener.accept();
        if p.closed.load(Ordering::SeqCst) {
            break;
        }
        let Ok((sock, _)) = conn else {
            std::thread::sleep(Duration::from_millis(20));
            continue;
        };
        if p.pending.fetch_add(1, Ordering::SeqCst) >= MAX_PENDING_HANDSHAKES {
            p.pending.fetch_sub(1, Ordering::SeqCst);
            drop(sock);
            continue;
        }
        let pc = p.clone();
        let slot = ThreadSlot::new(&p.threads);
        let spawned = std::thread::Builder::new()
            .name("lightvm-display-conn".into())
            .spawn(move || {
                let _slot = slot;
                serve(sock, pc);
            });
        if spawned.is_err() {
            p.pending.fetch_sub(1, Ordering::SeqCst);
        }
    }
    drop(listener);
    let _ = std::fs::remove_file(&p.socket_path);
    let _ = std::fs::remove_dir(&p.dir);
}

/// Reads the token line within the deadline. A read timeout fires only on
/// a connection that is then closed (F0-04).
fn read_token_line(s: &Socket, deadline: Instant) -> io::Result<[u8; TOKEN_LINE]> {
    let mut line = [0u8; TOKEN_LINE];
    let mut got = 0;
    while got < TOKEN_LINE {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        s.set_read_timeout(Some(left))?;
        match (&*s).read(&mut line[got..]) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    s.set_read_timeout(None)?;
    Ok(line)
}

fn token_ok(p: &ProxyInner, line: &[u8; TOKEN_LINE]) -> bool {
    if line[TOKEN_LINE - 1] != b'\n' {
        return false;
    }
    let Ok(text) = std::str::from_utf8(&line[..TOKEN_LINE - 1]) else {
        return false;
    };
    // Lowercase only: the canonical form issued by the service.
    if text.bytes().any(|b| b.is_ascii_uppercase()) {
        return false;
    }
    Credential::from_hex(text).is_some_and(|c| c.ct_eq(&p.token))
}

fn serve(sock: Socket, p: Arc<ProxyInner>) {
    let authenticated = read_token_line(&sock, Instant::now() + HANDSHAKE_TIMEOUT)
        .map(|line| token_ok(&p, &line))
        .unwrap_or(false);
    p.pending.fetch_sub(1, Ordering::SeqCst);
    if !authenticated || p.closed.load(Ordering::SeqCst) {
        let _ = sock.shutdown(Shutdown::Write);
        return;
    }
    let upstream = match Socket::new(Domain::UNIX, Type::STREAM, None).and_then(|u| {
        SockAddr::unix(&p.upstream)
            .and_then(|a| u.connect(&a))
            .map(|()| u)
    }) {
        Ok(u) => Arc::new(u),
        Err(e) => {
            crate::diag!("lightvm-service: display of the run not reachable: {e}");
            let _ = sock.shutdown(Shutdown::Write);
            return;
        }
    };
    // Non-blocking from here on: the pumps poll `stop` (see `Live::close`).
    if let Err(e) = sock
        .set_nonblocking(true)
        .and_then(|()| upstream.set_nonblocking(true))
    {
        crate::diag!("lightvm-service: display connection not set up: {e}");
        let _ = sock.shutdown(Shutdown::Both);
        let _ = upstream.shutdown(Shutdown::Both);
        return;
    }
    let client = Arc::new(sock);
    let stop = Arc::new(AtomicBool::new(false));
    {
        let mut live = lock(&p.live);
        if p.closed.load(Ordering::SeqCst) {
            drop(live);
            let _ = client.shutdown(Shutdown::Both);
            let _ = upstream.shutdown(Shutdown::Both);
            return;
        }
        // One viewer per run: the previous connection is closed.
        if let Some(prev) = live.replace(Live {
            client: client.clone(),
            upstream: upstream.clone(),
            stop: stop.clone(),
        }) {
            prev.close();
        }
    }
    let (c2, u2, s2) = (client.clone(), upstream.clone(), stop.clone());
    let slot = ThreadSlot::new(&p.threads);
    let down = std::thread::Builder::new()
        .name("lightvm-display-down".into())
        .spawn(move || {
            let _slot = slot;
            pump(&u2, &c2, &s2);
        });
    if let Err(reason) = pump_filtered(&client, &upstream, &stop) {
        crate::diag!("lightvm-service: display connection closed: {reason}");
        let _ = client.shutdown(Shutdown::Both);
        let _ = upstream.shutdown(Shutdown::Both);
    }
    // The client went away (or was refused): the copy towards it stops too.
    stop.store(true, Ordering::SeqCst);
    if let Ok(t) = down {
        let _ = t.join();
    }
    let mut live = lock(&p.live);
    if live
        .as_ref()
        .is_some_and(|l| Arc::ptr_eq(&l.client, &client))
    {
        *live = None;
    }
}

/// Copies QEMU → client until EOF, error or `stop`, then half-closes the
/// client (it sees EOF).
fn pump(from: &Socket, to: &Socket, stop: &AtomicBool) {
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = match read_nb(from, &mut buf, stop) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        if write_all_nb(to, &buf[..n], stop).is_err() {
            break;
        }
    }
    let _ = to.shutdown(Shutdown::Write);
}

/// Client → QEMU copy through the [`ClientFilter`]: only whole, allowed
/// messages are written upstream. `Err` = message refused (nothing of it
/// was forwarded): the caller closes both ends.
fn pump_filtered(from: &Socket, to: &Socket, stop: &AtomicBool) -> Result<(), &'static str> {
    let mut filter = ClientFilter::default();
    let mut buf = vec![0u8; 16 * 1024];
    let mut out = Vec::with_capacity(16 * 1024);
    let result = loop {
        let n = match read_nb(from, &mut buf, stop) {
            Ok(0) | Err(_) => break Ok(()),
            Ok(n) => n,
        };
        out.clear();
        if let Err(reason) = filter.feed(&buf[..n], &mut out) {
            break Err(reason);
        }
        if !out.is_empty() && write_all_nb(to, &out, stop).is_err() {
            break Ok(());
        }
    };
    let _ = to.shutdown(Shutdown::Write);
    result
}

/// Version string of the only RFB client allowed (read-only, 3.8).
const RFB_VERSION: &[u8; 12] = b"RFB 003.008\n";
/// Security type None (private Unix socket).
const SEC_NONE: u8 = 1;

/// Phase of the client → server RFB stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Phase {
    /// ProtocolVersion (12 bytes).
    #[default]
    Version,
    /// Security type chosen by the client (1 byte, None only).
    Security,
    /// ClientInit (1 byte: shared flag 0 or 1).
    ClientInit,
    /// Normal client messages.
    Messages,
}

/// Parser of the client → server RFB stream (RFC 6143, 3.8 only).
///
/// It accepts exactly what the read-only RFB client (`rfb-client`) sends
/// and nothing else: version `RFB 003.008`, security type None, ClientInit,
/// then SetPixelFormat (0, 20 bytes), SetEncodings (2, 4 + 4·n bytes with
/// n ≤ [`MAX_ENCODINGS`]) and FramebufferUpdateRequest (3, 10 bytes,
/// incremental flag 0 or 1). Bytes are released only as whole messages; any
/// other message type (KeyEvent 4, PointerEvent 5, ClientCutText 6,
/// extensions) or malformed handshake is an error and nothing of it is
/// released.
#[derive(Debug, Default)]
pub(crate) struct ClientFilter {
    phase: Phase,
    buf: Vec<u8>,
}

impl ClientFilter {
    /// Adds `data`; appends to `out` the whole messages that may be
    /// forwarded.
    pub(crate) fn feed(&mut self, data: &[u8], out: &mut Vec<u8>) -> Result<(), &'static str> {
        self.buf.extend_from_slice(data);
        while let Some(len) = self.next_message_len()? {
            out.extend(self.buf.drain(..len));
            self.phase = match self.phase {
                Phase::Version => Phase::Security,
                Phase::Security => Phase::ClientInit,
                Phase::ClientInit | Phase::Messages => Phase::Messages,
            };
        }
        Ok(())
    }

    /// Length of the next whole message at the start of the buffer, `None`
    /// if more bytes are needed, `Err` if it is not allowed.
    fn next_message_len(&self) -> Result<Option<usize>, &'static str> {
        let b = &self.buf;
        let Some(&first) = b.first() else {
            return Ok(None);
        };
        let have = |n: usize| if b.len() >= n { Some(n) } else { None };
        match self.phase {
            Phase::Version => {
                let n = b.len().min(RFB_VERSION.len());
                if b[..n] != RFB_VERSION[..n] {
                    return Err("RFB version other than 3.8");
                }
                Ok(have(RFB_VERSION.len()))
            }
            Phase::Security if first == SEC_NONE => Ok(Some(1)),
            Phase::Security => Err("security type other than None"),
            Phase::ClientInit if first <= 1 => Ok(Some(1)),
            Phase::ClientInit => Err("invalid ClientInit"),
            Phase::Messages => match first {
                // SetPixelFormat: type, 3 padding, 16 pixel format.
                0 => Ok(have(20)),
                // SetEncodings: type, padding, u16 count, count × i32.
                2 => {
                    if b.len() < 4 {
                        return Ok(None);
                    }
                    let count = usize::from(u16::from_be_bytes([b[2], b[3]]));
                    if count > MAX_ENCODINGS {
                        return Err("too many encodings");
                    }
                    Ok(have(4 + 4 * count))
                }
                // FramebufferUpdateRequest: type, incremental, x, y, w, h.
                3 => {
                    if b.len() >= 2 && b[1] > 1 {
                        return Err("invalid FramebufferUpdateRequest");
                    }
                    Ok(have(10))
                }
                4 => Err("KeyEvent refused: input goes through the service"),
                5 => Err("PointerEvent refused: input goes through the service"),
                6 => Err("ClientCutText refused"),
                _ => Err("client message type not allowed"),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HANDSHAKE: &[u8] = b"RFB 003.008\n\x01\x01";

    fn fbur() -> Vec<u8> {
        vec![3, 1, 0, 0, 0, 0, 0, 16, 0, 16]
    }

    fn feed_all(chunks: &[&[u8]]) -> (Vec<u8>, Result<(), &'static str>) {
        let mut f = ClientFilter::default();
        let mut out = Vec::new();
        for c in chunks {
            if let Err(e) = f.feed(c, &mut out) {
                return (out, Err(e));
            }
        }
        (out, Ok(()))
    }

    #[test]
    fn filter_forwards_the_read_only_client_stream_whole() {
        let mut set_pf = vec![0u8; 20];
        set_pf[4] = 32;
        let set_enc = [
            &[2u8, 0, 0, 2][..],
            &[0, 0, 0, 0x10],
            &[0xff, 0xff, 0xff, 0x21],
        ]
        .concat();
        let stream = [HANDSHAKE, &set_pf, &set_enc, &fbur()].concat();
        // Whole, then one byte at a time: same output, never a partial message.
        assert_eq!(feed_all(&[&stream]), (stream.clone(), Ok(())));
        let boundaries = [0usize, 12, 13, 14, 34, 46, 56];
        let mut f = ClientFilter::default();
        let mut out = Vec::new();
        for (i, b) in stream.iter().enumerate() {
            f.feed(std::slice::from_ref(b), &mut out).unwrap();
            assert!(boundaries.contains(&out.len()), "partial message released");
            if boundaries.contains(&(i + 1)) {
                assert_eq!(out.len(), i + 1, "released at the message boundary");
            }
        }
        assert_eq!(out, stream);
    }

    #[test]
    fn filter_refuses_input_clipboard_and_bad_handshakes() {
        let key = [4u8, 1, 0, 0, 0, 0, 0, 0x61];
        let pointer = [5u8, 1, 0, 10, 0, 10];
        let cut = [6u8, 0, 0, 0, 0, 0, 0, 1, b'x'];
        for (bad, why) in [
            (&key[..], "KeyEvent"),
            (&pointer[..], "PointerEvent"),
            (&cut[..], "ClientCutText"),
            (&[255u8, 0][..], "QEMU extension"),
            (&[150u8][..], "unknown"),
            (
                &[3u8, 7, 0, 0, 0, 0, 0, 1, 0, 1][..],
                "bad incremental flag",
            ),
            (&[2u8, 0, 0xff, 0xff][..], "encoding count"),
        ] {
            let (out, r) = feed_all(&[HANDSHAKE, &fbur(), bad]);
            assert!(r.is_err(), "{why}");
            // Only what came before is released, nothing of the refused message.
            assert_eq!(out, [HANDSHAKE, &fbur()].concat(), "{why}");
        }
        // Key event split over two reads: nothing of it is released.
        let (out, r) = feed_all(&[HANDSHAKE, &key[..1]]);
        assert!(r.is_err());
        assert_eq!(out, HANDSHAKE);
        for bad in [
            &b"RFB 003.003\n"[..],
            b"RFB 003.007\n",
            b"GET / HTTP/1.1\r\n",
            b"RFB 003.008\n\x02",
            b"RFB 003.008\n\x01\x05",
        ] {
            let (out, r) = feed_all(&[bad]);
            assert!(r.is_err(), "{bad:?}");
            assert!(out.len() <= 13, "{bad:?}: {out:?}");
        }
        // Input right after the handshake bytes, in the same read.
        let (out, r) = feed_all(&[&[HANDSHAKE, &key[..]].concat()]);
        assert!(r.is_err());
        assert_eq!(out, HANDSHAKE);
    }

    // ----- proxy with a fake upstream -----------------------------------------

    fn short_dir(prefix: &str) -> PathBuf {
        let mut b = [0u8; 4];
        getrandom::fill(&mut b).unwrap();
        let d = std::env::temp_dir().join(format!("{prefix}{}", crate::credential::hex_encode(&b)));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Minimal RFB 3.8 server (None security, 16×16 framebuffer) that records
    /// every byte received from the proxy and answers each
    /// FramebufferUpdateRequest with an empty update.
    struct FakeRfb {
        path: PathBuf,
        received: Arc<Mutex<Vec<u8>>>,
    }

    fn fake_rfb() -> FakeRfb {
        let path = short_dir("lvu").join("v.sock");
        let l = bind(&path).unwrap();
        let received = Arc::new(Mutex::new(Vec::new()));
        let rec = received.clone();
        std::thread::spawn(move || {
            while let Ok((s, _)) = l.accept() {
                let rec = rec.clone();
                std::thread::spawn(move || {
                    let mut init = Vec::new();
                    init.extend_from_slice(RFB_VERSION);
                    let _ = (&s).write_all(&init);
                    let mut buf = [0u8; 4096];
                    let mut seen = 0usize;
                    let mut stage = 0;
                    loop {
                        let n = match (&s).read(&mut buf) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => n,
                        };
                        let mut r = lock(&rec);
                        r.extend_from_slice(&buf[..n]);
                        let total = r.len();
                        drop(r);
                        if stage == 0 && total >= 12 {
                            stage = 1;
                            let _ = (&s).write_all(&[1, SEC_NONE]);
                        }
                        if stage == 1 && total >= 13 {
                            stage = 2;
                            let _ = (&s).write_all(&[0, 0, 0, 0]);
                        }
                        if stage == 2 && total >= 14 {
                            stage = 3;
                            let mut si = vec![0, 16, 0, 16];
                            si.extend_from_slice(&[32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0]);
                            si.extend_from_slice(&[0, 0, 0]);
                            si.extend_from_slice(&[0, 0, 0, 4]);
                            si.extend_from_slice(b"fake");
                            let _ = (&s).write_all(&si);
                            seen = 14;
                        }
                        if stage == 3 {
                            // Count the FBURs among the whole messages received.
                            let r = lock(&rec).clone();
                            while seen < r.len() {
                                let len = match r[seen] {
                                    0 => 20,
                                    2 if r.len() >= seen + 4 => {
                                        4 + 4 * usize::from(u16::from_be_bytes([
                                            r[seen + 2],
                                            r[seen + 3],
                                        ]))
                                    }
                                    3 => 10,
                                    _ => break,
                                };
                                if r.len() < seen + len {
                                    break;
                                }
                                if r[seen] == 3 {
                                    let _ = (&s).write_all(&[0, 0, 0, 0]);
                                }
                                seen += len;
                            }
                        }
                    }
                    let _ = s.shutdown(Shutdown::Write);
                });
            }
        });
        FakeRfb { path, received }
    }

    fn vm_run() -> (VmId, RunId) {
        (VmId::new_v7(), RunId::new_v7())
    }

    fn connect(t: &IssuedTicket) -> Socket {
        let s = Socket::new(Domain::UNIX, Type::STREAM, None).unwrap();
        s.connect(&SockAddr::unix(&t.socket_path).unwrap()).unwrap();
        (&s).write_all(format!("{}\n", t.token).as_bytes()).unwrap();
        s
    }

    fn read_n(s: &Socket, n: usize) -> Vec<u8> {
        let mut out = vec![0u8; n];
        let mut got = 0;
        while got < n {
            match (&*s).read(&mut out[got..]) {
                Ok(0) | Err(_) => break,
                Ok(k) => got += k,
            }
        }
        out.truncate(got);
        out
    }

    fn eof_within(s: &Socket, d: Duration) -> bool {
        let t0 = Instant::now();
        let mut b = [0u8; 256];
        while t0.elapsed() < d {
            match (&*s).read(&mut b) {
                Ok(0) | Err(_) => return true,
                Ok(_) => {}
            }
        }
        false
    }

    fn wait_until(d: Duration, f: impl Fn() -> bool) -> bool {
        let t0 = Instant::now();
        while t0.elapsed() < d {
            if f() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        f()
    }

    #[test]
    fn key_event_closes_the_connection_and_is_never_forwarded() {
        let up = fake_rfb();
        let reg = DisplayRegistry::new(short_dir("lvr"));
        let (vm, run) = vm_run();
        let t = reg.issue(vm, run, 1, &up.path).unwrap();
        let s = connect(&t);
        assert_eq!(read_n(&s, 12), RFB_VERSION);
        (&s).write_all(RFB_VERSION).unwrap();
        assert_eq!(read_n(&s, 2), [1, SEC_NONE]);
        (&s).write_all(&[SEC_NONE]).unwrap();
        assert_eq!(read_n(&s, 4), [0, 0, 0, 0]);
        (&s).write_all(&[1]).unwrap();
        assert_eq!(read_n(&s, 28).len(), 28, "ServerInit");
        (&s).write_all(&fbur()).unwrap();
        assert_eq!(read_n(&s, 4), [0, 0, 0, 0], "empty update");
        // KeyEvent 'a' down: the proxy closes, QEMU never sees it.
        (&s).write_all(&[4, 1, 0, 0, 0, 0, 0, 0x61]).unwrap();
        assert!(eof_within(&s, Duration::from_secs(5)));
        let got = lock(&up.received).clone();
        assert_eq!(got, [HANDSHAKE, &fbur()].concat());
        assert!(wait_until(Duration::from_secs(5), || reg
            .connection_threads()
            == 0));
    }

    #[test]
    fn rfb_client_session_works_through_the_filter() {
        let up = fake_rfb();
        let reg = DisplayRegistry::new(short_dir("lvr"));
        let (vm, run) = vm_run();
        let t = reg.issue(vm, run, 1, &up.path).unwrap();
        let transport = rfb_client::Transport::Ticket {
            path: t.socket_path.clone(),
            imp: rfb_client::UnixImpl::Socket2,
            token: t.token.clone(),
        };
        let mut c = rfb_client::Connection::connect(&transport).unwrap();
        assert_eq!(c.server_init().width, 16);
        c.request_update().unwrap();
        c.read_update().unwrap();
        c.request_update().unwrap();
        c.read_update().unwrap();
        // Handshake, SetPixelFormat, SetEncodings and two requests forwarded.
        let got = lock(&up.received).clone();
        assert!(got.starts_with(HANDSHAKE));
        assert_eq!(got[14], 0, "SetPixelFormat");
        assert_eq!(got[34], 2, "SetEncodings");
        assert_eq!(got.iter().rev().nth(9), Some(&3), "last is a FBUR");
        drop(c);
        reg.revoke_all();
    }

    #[test]
    fn revocation_ends_both_threads_of_an_idle_client() {
        let up = fake_rfb();
        let reg = DisplayRegistry::new(short_dir("lvr"));
        let (vm, run) = vm_run();
        let t = reg.issue(vm, run, 7, &up.path).unwrap();
        // The client authenticates, reads the greeting and then stays idle
        // with its end open.
        let s = connect(&t);
        assert_eq!(read_n(&s, 12), RFB_VERSION);
        assert!(wait_until(Duration::from_secs(5), || reg
            .connection_threads()
            == 2));
        assert_eq!(reg.revoke_owner(7), vec![(vm, run)]);
        assert!(
            wait_until(Duration::from_secs(5), || reg.connection_threads() == 0),
            "threads left: {}",
            reg.connection_threads()
        );
        assert!(eof_within(&s, Duration::from_secs(5)));

        // Replacement by a second viewer ends the first one's threads too.
        let t = reg.issue(vm, run, 7, &up.path).unwrap();
        let a = connect(&t);
        assert_eq!(read_n(&a, 12), RFB_VERSION);
        assert!(wait_until(Duration::from_secs(5), || reg
            .connection_threads()
            == 2));
        let b = connect(&t);
        assert_eq!(read_n(&b, 12), RFB_VERSION);
        assert!(eof_within(&a, Duration::from_secs(5)));
        assert!(wait_until(Duration::from_secs(5), || reg
            .connection_threads()
            == 2));
        assert_eq!(reg.revoke_all(), vec![(vm, run)]);
        assert!(wait_until(Duration::from_secs(5), || reg
            .connection_threads()
            == 0));
        drop(b);
    }

    #[test]
    fn root_is_recreated_and_stale_directories_are_removed() {
        let base = short_dir("lvs");
        let root = base.join("d");
        let reg = DisplayRegistry::new(root.clone());
        let (vm, run) = vm_run();
        let up = base.join("none.sock");
        let t = reg.issue(vm, run, 1, &up).unwrap();
        assert!(t.socket_path.exists());
        assert_eq!(reg.revoke_vm(vm), Some((vm, run)));
        // Root removed behind the registry's back: recreated at the next ticket.
        assert!(wait_until(Duration::from_secs(5), || {
            std::fs::remove_dir_all(&root).is_ok()
        }));
        let t = reg.issue(vm, run, 1, &up).unwrap();
        assert!(t.socket_path.exists());
        reg.revoke_all();
        // Leftovers of a previous service: removed, other entries kept.
        std::fs::create_dir_all(root.join("0123456789ab").join("x")).unwrap();
        std::fs::create_dir_all(root.join("keep-me")).unwrap();
        std::fs::write(root.join("abcdefabcdef"), b"file").unwrap();
        let fresh = DisplayRegistry::new(root.clone());
        assert!(fresh.remove_stale() >= 1);
        assert!(!root.join("0123456789ab").exists());
        assert!(root.join("keep-me").exists());
        assert!(root.join("abcdefabcdef").exists());
        // A root that cannot be created is not cached as failed.
        let blocked = base.join("blocked");
        std::fs::write(&blocked, b"not a directory").unwrap();
        let reg = DisplayRegistry::new(blocked.clone());
        assert!(reg.issue(vm, run, 1, &up).is_err());
        std::fs::remove_file(&blocked).unwrap();
        let t = reg.issue(vm, run, 1, &up).unwrap();
        assert!(t.socket_path.exists());
        reg.revoke_all();
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn dead_fallback_roots_are_removed() {
        let base = short_dir("lvf");
        let mine = base.join(format!("{FALLBACK_ROOT_PREFIX}{}", std::process::id()));
        // PID that no process can have.
        let dead = base.join(format!("{FALLBACK_ROOT_PREFIX}{}", 0xFFFF_FFF0u32));
        let other = base.join("lightvm-p-x");
        for d in [&mine, &dead, &other] {
            std::fs::create_dir_all(d.join("d")).unwrap();
        }
        let n = remove_stale_fallback_roots(&base);
        assert!(mine.exists() && other.exists());
        if cfg!(windows) {
            assert_eq!(n, 1);
            assert!(!dead.exists());
        }
        let _ = std::fs::remove_dir_all(&base);
    }
}
