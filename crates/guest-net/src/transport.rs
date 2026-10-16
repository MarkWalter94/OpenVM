//! Frames between QEMU's `-netdev stream` and the stack.
//!
//! QEMU's stream back end sends every Ethernet frame as a 4-byte big-endian
//! length followed by the frame (the format of `-netdev socket` and
//! `-netdev stream`). [`attach`] runs two threads on one socket (AF_UNIX in
//! the run's private directory, or TCP in tests):
//!
//! - the reader blocks the stack's input when the stack is slow
//!   (backpressure towards QEMU);
//! - the writer drops a frame when its queue is full, as a NIC does (TCP
//!   retransmits), so the stack never waits on the guest.
//!
//! Both threads use the same handle in non-blocking mode and poll a stop
//! flag (on Windows a shutdown does not unblock an AF_UNIX `recv` in
//! progress and a duplicate handle breaks the stream, F0-04): they end
//! within a few milliseconds of [`Link::close`] or of the peer closing.

use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

use socket2::Socket;

/// Largest frame accepted from QEMU (a jumbo frame plus margin; e1000 and
/// rtl8139 send at most 1514 bytes).
pub const MAX_FRAME: usize = 65_536;
/// Frames queued towards the stack before the reader waits.
pub const IN_QUEUE: usize = 1024;
/// Frames queued towards QEMU before new ones are dropped.
pub const OUT_QUEUE: usize = 4096;

const POLL_MIN: Duration = Duration::from_micros(200);
const POLL_MAX: Duration = Duration::from_millis(4);

/// Frames sent by the stack towards the guest.
#[derive(Clone, Debug)]
pub struct FrameSink {
    tx: mpsc::SyncSender<Vec<u8>>,
}

impl FrameSink {
    /// Queues a frame; `false` if it was dropped (queue full or closed).
    pub fn send(&self, frame: Vec<u8>) -> bool {
        self.tx.try_send(frame).is_ok()
    }
}

/// Both directions of a link, as the stack uses them.
#[derive(Debug)]
pub struct Link {
    pub(crate) from_guest: tokio::sync::mpsc::Receiver<Vec<u8>>,
    pub(crate) to_guest: FrameSink,
    stop: Arc<AtomicBool>,
    socket: Option<Arc<Socket>>,
    threads: Vec<JoinHandle<()>>,
}

impl Link {
    /// Stops the threads and closes the socket (idempotent).
    pub fn close(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(s) = &self.socket {
            let _ = s.shutdown(Shutdown::Both);
        }
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }

    /// In-memory link for tests: the other end is a [`GuestEnd`].
    #[must_use]
    pub fn pair() -> (Link, GuestEnd) {
        let (in_tx, in_rx) = tokio::sync::mpsc::channel(IN_QUEUE);
        let (out_tx, out_rx) = mpsc::sync_channel(OUT_QUEUE);
        (
            Link {
                from_guest: in_rx,
                to_guest: FrameSink { tx: out_tx },
                stop: Arc::default(),
                socket: None,
                threads: Vec::new(),
            },
            GuestEnd {
                to_stack: in_tx,
                from_stack: out_rx,
            },
        )
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        self.close();
    }
}

/// Guest side of an in-memory [`Link`] (tests).
#[derive(Debug)]
pub struct GuestEnd {
    /// Frames to the stack.
    pub to_stack: tokio::sync::mpsc::Sender<Vec<u8>>,
    /// Frames from the stack.
    pub from_stack: mpsc::Receiver<Vec<u8>>,
}

/// Runs the link over a connected stream socket (QEMU `-netdev stream`).
pub fn attach(socket: Socket) -> io::Result<Link> {
    socket.set_nonblocking(true)?;
    let socket = Arc::new(socket);
    let stop = Arc::new(AtomicBool::new(false));
    let (in_tx, in_rx) = tokio::sync::mpsc::channel(IN_QUEUE);
    let (out_tx, out_rx) = mpsc::sync_channel::<Vec<u8>>(OUT_QUEUE);
    let reader = {
        let (s, stop) = (socket.clone(), stop.clone());
        std::thread::Builder::new()
            .name("guest-net-rx".into())
            .spawn(move || {
                let r = read_frames(&s, &stop, &in_tx);
                stop.store(true, Ordering::SeqCst);
                drop(r);
            })?
    };
    let writer = {
        let (s, stop) = (socket.clone(), stop.clone());
        std::thread::Builder::new()
            .name("guest-net-tx".into())
            .spawn(move || {
                let r = write_frames(&s, &stop, &out_rx);
                stop.store(true, Ordering::SeqCst);
                drop(r);
            })?
    };
    Ok(Link {
        from_guest: in_rx,
        to_guest: FrameSink { tx: out_tx },
        stop,
        socket: Some(socket),
        threads: vec![reader, writer],
    })
}

/// Reads exactly `buf.len()` bytes, polling `stop`. `Ok(false)` on EOF or
/// stop before the first byte.
fn read_full(s: &Socket, buf: &mut [u8], stop: &AtomicBool) -> io::Result<bool> {
    let mut got = 0;
    let mut idle = POLL_MIN;
    while got < buf.len() {
        if stop.load(Ordering::SeqCst) {
            return Ok(false);
        }
        match (&*s).read(&mut buf[got..]) {
            Ok(0) => {
                return if got == 0 {
                    Ok(false)
                } else {
                    Err(io::ErrorKind::UnexpectedEof.into())
                };
            }
            Ok(n) => {
                got += n;
                idle = POLL_MIN;
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(idle);
                idle = (idle * 2).min(POLL_MAX);
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(true)
}

fn read_frames(
    s: &Socket,
    stop: &AtomicBool,
    to_stack: &tokio::sync::mpsc::Sender<Vec<u8>>,
) -> io::Result<()> {
    let mut len = [0u8; 4];
    loop {
        if !read_full(s, &mut len, stop)? {
            return Ok(());
        }
        let n = u32::from_be_bytes(len) as usize;
        if n == 0 || n > MAX_FRAME {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "frame length out of range",
            ));
        }
        let mut frame = vec![0u8; n];
        if !read_full(s, &mut frame, stop)? {
            return Ok(());
        }
        if to_stack.blocking_send(frame).is_err() {
            return Ok(()); // stack gone
        }
    }
}

fn write_all(s: &Socket, mut buf: &[u8], stop: &AtomicBool) -> io::Result<()> {
    let mut idle = POLL_MIN;
    while !buf.is_empty() {
        if stop.load(Ordering::SeqCst) {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        match (&*s).write(buf) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(n) => {
                buf = &buf[n..];
                idle = POLL_MIN;
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

fn write_frames(
    s: &Socket,
    stop: &AtomicBool,
    from_stack: &mpsc::Receiver<Vec<u8>>,
) -> io::Result<()> {
    let mut out = Vec::with_capacity(2048);
    loop {
        if stop.load(Ordering::SeqCst) {
            return Ok(());
        }
        let frame = match from_stack.recv_timeout(Duration::from_millis(50)) {
            Ok(f) => f,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
        };
        out.clear();
        out.extend_from_slice(&u32::try_from(frame.len()).unwrap_or(0).to_be_bytes());
        out.extend_from_slice(&frame);
        // Coalesce what is already queued into one write.
        while out.len() < 256 * 1024 {
            match from_stack.try_recv() {
                Ok(f) => {
                    out.extend_from_slice(&u32::try_from(f.len()).unwrap_or(0).to_be_bytes());
                    out.extend_from_slice(&f);
                }
                Err(_) => break,
            }
        }
        write_all(s, &out, stop)?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{TcpListener, TcpStream};

    #[test]
    fn frames_round_trip_over_a_stream() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let mut peer = TcpStream::connect(addr).unwrap();
        let (s, _) = l.accept().unwrap();
        let mut link = attach(Socket::from(s)).unwrap();
        // QEMU → stack.
        for n in [60usize, 1514, 9000] {
            let f: Vec<u8> = (0..n).map(|i| i as u8).collect();
            peer.write_all(&u32::try_from(n).unwrap().to_be_bytes())
                .unwrap();
            peer.write_all(&f).unwrap();
            let got = link.from_guest.blocking_recv().unwrap();
            assert_eq!(got, f);
        }
        // Stack → QEMU.
        assert!(link.to_guest.send(vec![7u8; 100]));
        let mut len = [0u8; 4];
        peer.read_exact(&mut len).unwrap();
        assert_eq!(u32::from_be_bytes(len), 100);
        let mut b = vec![0u8; 100];
        peer.read_exact(&mut b).unwrap();
        assert_eq!(b, vec![7u8; 100]);
        // A bad length ends the reader; close ends everything.
        peer.write_all(&0u32.to_be_bytes()).unwrap();
        assert!(link.from_guest.blocking_recv().is_none());
        link.close();
    }

    #[test]
    fn close_stops_threads_with_a_silent_peer() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let _peer = TcpStream::connect(l.local_addr().unwrap()).unwrap();
        let (s, _) = l.accept().unwrap();
        let mut link = attach(Socket::from(s)).unwrap();
        let t = std::time::Instant::now();
        link.close();
        assert!(t.elapsed() < Duration::from_secs(1));
    }
}
