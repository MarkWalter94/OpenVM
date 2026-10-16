//! A run-scoped file-channel endpoint owned by the supervisor. The listener
//! binds only loopback; it must be exposed to the guest solely through the
//! supervisor network's internal endpoint, never through unrestricted NAT.

use std::io;
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::files::{FileRecord, FileService};
use crate::policy::{Gate, Secret};
use crate::protocol::{self, FileRequest, Hello};

const AUTH_TIMEOUT: Duration = Duration::from_secs(3);
const IO_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Welcome {
    pub version: u32,
}

#[derive(Debug, Default)]
struct Shared {
    epoch: AtomicU64,
    stopped: AtomicBool,
    connection: Mutex<Option<TcpStream>>,
    records: Mutex<Vec<FileRecord>>,
}

/// Independent close handle: shutdown interrupts a guest blocked in a read
/// or write without waiting for the supervisor's normal request queue.
#[derive(Clone, Debug)]
pub struct Stop {
    shared: Arc<Shared>,
    gate: Gate,
}

impl Stop {
    /// Closes the active connection without destroying the run's listener.
    pub fn suspend(&self) {
        self.shared.epoch.fetch_add(1, Ordering::SeqCst);
        self.gate.revoke();
        if let Some(s) = self
            .shared
            .connection
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            let _ = s.shutdown(Shutdown::Both);
        }
    }
    pub fn close(&self) {
        self.shared.stopped.store(true, Ordering::SeqCst);
        if let Some(s) = self
            .shared
            .connection
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            let _ = s.shutdown(Shutdown::Both);
        }
        self.gate.revoke();
    }
}

#[derive(Debug)]
pub struct FileServer {
    address: SocketAddr,
    stop: Stop,
    worker: Option<JoinHandle<()>>,
}

impl FileServer {
    pub fn start(
        vm: String,
        run: String,
        secret: Secret,
        gate: Gate,
        mut files: FileService,
    ) -> io::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let shared = Arc::new(Shared::default());
        let stop = Stop {
            shared: shared.clone(),
            gate: gate.clone(),
        };
        let worker = std::thread::Builder::new()
            .name("lightvm-tools-files".into())
            .spawn(move || {
                while !shared.stopped.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, peer)) if peer.ip().is_loopback() => {
                            let epoch = shared.epoch.load(Ordering::SeqCst);
                            if shared.stopped.load(Ordering::SeqCst) {
                                break;
                            }
                            let cloned = match stream.try_clone() {
                                Ok(s) => s,
                                Err(_) => continue,
                            };
                            *shared.connection.lock().unwrap_or_else(|e| e.into_inner()) =
                                Some(cloned);
                            // close() can race with registration. Check again
                            // after publishing the interruptible socket.
                            if !shared.stopped.load(Ordering::SeqCst) {
                                let _ = serve(
                                    stream,
                                    (&vm, &run),
                                    &secret,
                                    &gate,
                                    &mut files,
                                    &shared,
                                    epoch,
                                );
                            }
                            files.cancel_all();
                            shared
                                .connection
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .take();
                            flush_records(&shared, &mut files);
                        }
                        Ok(_) => {}
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(10))
                        }
                        Err(_) => break,
                    }
                    files.reap();
                    flush_records(&shared, &mut files);
                }
                files.cancel_all();
                flush_records(&shared, &mut files);
            })?;
        Ok(Self {
            address,
            stop,
            worker: Some(worker),
        })
    }

    pub fn address(&self) -> SocketAddr {
        self.address
    }
    pub fn stop_handle(&self) -> Stop {
        self.stop.clone()
    }
    pub fn take_records(&self) -> Vec<FileRecord> {
        std::mem::take(
            &mut *self
                .stop
                .shared
                .records
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
        )
    }
    pub fn stop(&mut self) {
        self.stop.close();
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}
impl Drop for FileServer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn flush_records(shared: &Shared, files: &mut FileService) {
    let mut records = shared.records.lock().unwrap_or_else(|e| e.into_inner());
    for record in files.take_records() {
        if records.len() >= crate::files::MAX_RECORDS {
            records.remove(0);
        }
        records.push(record);
    }
}

fn serve(
    mut stream: TcpStream,
    (vm, run): (&str, &str),
    secret: &Secret,
    gate: &Gate,
    files: &mut FileService,
    shared: &Shared,
    epoch: u64,
) -> io::Result<()> {
    stream.set_nonblocking(true)?;
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(AUTH_TIMEOUT))?;
    stream.set_write_timeout(Some(AUTH_TIMEOUT))?;
    let cancelled =
        || shared.stopped.load(Ordering::SeqCst) || shared.epoch.load(Ordering::SeqCst) != epoch;
    let mut auth = crate::wire_io::Wire::new(&mut stream, Instant::now() + AUTH_TIMEOUT, cancelled);
    let hello = protocol::read::<_, Hello>(&mut auth)?
        .ok_or_else(|| protocol::invalid("hello required"))?;
    if !secret.authenticate(&hello, vm, run) {
        return Err(io::Error::from(io::ErrorKind::PermissionDenied));
    }
    drop(hello);
    protocol::write(
        &mut auth,
        &Welcome {
            version: protocol::VERSION,
        },
    )?;
    let permit = gate.permit();
    while !shared.stopped.load(Ordering::SeqCst) {
        let mut wire =
            crate::wire_io::Wire::new(&mut stream, Instant::now() + IO_TIMEOUT, cancelled);
        let Some(request) = protocol::read::<_, FileRequest>(&mut wire)? else {
            break;
        };
        if shared.stopped.load(Ordering::SeqCst) {
            break;
        }
        let reply = files.dispatch(&permit, request);
        protocol::write(&mut wire, &reply)?;
        flush_records(shared, files);
    }
    Ok(())
}

/// Guest-side synchronous file channel. A failed or partial frame poisons
/// the connection; callers must reconnect rather than replay writes.
#[derive(Debug)]
pub struct FileClient {
    stream: Option<TcpStream>,
}

impl FileClient {
    pub fn connect(address: SocketAddr, hello: &Hello) -> io::Result<Self> {
        let mut stream = TcpStream::connect_timeout(&address, AUTH_TIMEOUT)?;
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(IO_TIMEOUT))?;
        stream.set_write_timeout(Some(IO_TIMEOUT))?;
        protocol::write(&mut stream, hello)?;
        let welcome = protocol::read::<_, Welcome>(&mut stream)?
            .ok_or_else(|| protocol::invalid("welcome required"))?;
        if welcome.version != protocol::VERSION {
            return Err(protocol::invalid("unsupported tools version"));
        }
        Ok(Self {
            stream: Some(stream),
        })
    }

    pub fn call(&mut self, request: &FileRequest) -> io::Result<protocol::FileReply> {
        let stream = self
            .stream
            .as_mut()
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotConnected))?;
        let result = protocol::write(stream, request)
            .and_then(|()| protocol::read(stream))
            .and_then(|r| r.ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof)));
        if result.is_err() {
            self.stream.take();
        }
        result
    }
}
