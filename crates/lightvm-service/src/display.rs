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
//! - after the check the proxy connects to QEMU's `vnc.sock` and forwards
//!   bytes both ways; it never parses or injects RFB (the RFB client stays
//!   read-only: input goes through IPC and QMP);
//! - **one viewer per run**: a new authenticated connection closes the
//!   previous one ("last wins"), so a stale console never blocks a reconnect;
//!   at most [`MAX_PENDING_HANDSHAKES`] unauthenticated connections at once;
//! - **one valid ticket per run**: issuing a new ticket for a VM revokes the
//!   previous one;
//! - **revocation**: when the run ends (the supervisor revokes on every VM
//!   state other than `running`), on Emergency Stop, when the UI connection
//!   that obtained the ticket is revoked or closed, and at service shutdown:
//!   the token becomes invalid, the listener and its directory are removed
//!   and live connections are closed (half-close towards both ends: QEMU
//!   drops its client at once; a client that keeps its end open no longer
//!   receives anything).
//!
//! Threads: one accept thread per proxy and two threads per live connection,
//! with blocking reads on a single shared handle (F0-04: on Windows a read
//! timeout or a shutdown on a duplicate handle breaks AF_UNIX streams).

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
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
/// Length of the token line: 64 hexadecimal digits and `\n`.
const TOKEN_LINE: usize = 65;

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
}

impl std::fmt::Debug for IssuedTicket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IssuedTicket")
            .field("socket_path", &self.socket_path)
            .field("token", &"<redacted>")
            .finish()
    }
}

/// Live forwarded connection (client and QEMU ends).
struct Live {
    client: Arc<Socket>,
    upstream: Arc<Socket>,
}

impl Live {
    fn close(&self) {
        let _ = self.client.shutdown(Shutdown::Write);
        let _ = self.upstream.shutdown(Shutdown::Write);
    }
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

/// Registry of the display proxies (one per VM with a ticket).
pub struct DisplayRegistry {
    root: PathBuf,
    prepared: OnceLock<Result<(), String>>,
    proxies: Mutex<HashMap<VmId, Proxy>>,
}

impl std::fmt::Debug for DisplayRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DisplayRegistry")
            .field("root", &self.root)
            .field("proxies", &lock(&self.proxies).len())
            .finish()
    }
}

impl DisplayRegistry {
    /// Registry with the display root (created with the user-only ACL at the
    /// first ticket). The root must be short: AF_UNIX paths are limited to
    /// 107 bytes and the proxy adds 21 characters.
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            prepared: OnceLock::new(),
            proxies: Mutex::new(HashMap::new()),
        }
    }

    /// Display root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn prepare_root(&self) -> io::Result<()> {
        self.prepared
            .get_or_init(|| restrict_dir(&self.root).map_err(|e| e.to_string()))
            .clone()
            .map_err(io::Error::other)
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
        let mut name = [0u8; 6];
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
        if let Some(p) = previous {
            p.inner.shut();
        }
        Ok(IssuedTicket {
            socket_path,
            token: hex,
        })
    }

    /// Revokes the ticket of a VM (run ended or VM no longer running).
    pub fn revoke_vm(&self, vm_id: VmId) -> bool {
        let p = lock(&self.proxies).remove(&vm_id);
        p.map(|p| p.inner.shut()).is_some()
    }

    /// Revokes the ticket of a VM only if it belongs to another run.
    pub fn revoke_other_runs(&self, vm_id: VmId, current: Option<RunId>) {
        let stale = lock(&self.proxies)
            .get(&vm_id)
            .is_some_and(|p| Some(p.inner.run_id) != current);
        if stale {
            self.revoke_vm(vm_id);
        }
    }

    /// Revokes the tickets obtained by a UI connection (revoked or closed);
    /// returns the VMs involved (their pressed keys are to be released).
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
        gone.into_iter()
            .map(|p| {
                p.inner.shut();
                (p.inner.vm_id, p.inner.run_id)
            })
            .collect()
    }

    /// Revokes every ticket (Emergency Stop, shutdown).
    pub fn revoke_all(&self) -> usize {
        let all: Vec<Proxy> = lock(&self.proxies).drain().map(|(_, p)| p).collect();
        let n = all.len();
        for p in all {
            p.inner.shut();
        }
        n
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
        let spawned = std::thread::Builder::new()
            .name("lightvm-display-conn".into())
            .spawn(move || serve(sock, pc));
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
    let client = Arc::new(sock);
    {
        let mut live = lock(&p.live);
        if p.closed.load(Ordering::SeqCst) {
            drop(live);
            let _ = client.shutdown(Shutdown::Write);
            let _ = upstream.shutdown(Shutdown::Write);
            return;
        }
        // One viewer per run: the previous connection is closed.
        if let Some(prev) = live.replace(Live {
            client: client.clone(),
            upstream: upstream.clone(),
        }) {
            prev.close();
        }
    }
    let (c2, u2) = (client.clone(), upstream.clone());
    let down = std::thread::Builder::new()
        .name("lightvm-display-down".into())
        .spawn(move || pump(&u2, &c2));
    pump(&client, &upstream);
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

/// Copies `from` → `to` until EOF or error, then half-closes `to` (the peer
/// sees EOF and closes in turn, which ends the opposite copy).
fn pump(from: &Socket, to: &Socket) {
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = match (&*from).read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        if (&*to).write_all(&buf[..n]).is_err() {
            break;
        }
    }
    let _ = to.shutdown(Shutdown::Write);
}
