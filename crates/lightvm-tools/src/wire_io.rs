//! Cancellable framed TCP I/O. Windows shutdown on a cloned socket does not
//! reliably interrupt a blocking read; explicit polling bounds teardown time.
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

pub(crate) struct Wire<'a, F> {
    stream: &'a mut TcpStream,
    cancelled: F,
    deadline: Instant,
}
impl<'a, F: Fn() -> bool> Wire<'a, F> {
    pub(crate) fn new(stream: &'a mut TcpStream, deadline: Instant, cancelled: F) -> Self {
        Self {
            stream,
            deadline,
            cancelled,
        }
    }
    fn retry(
        &mut self,
        mut operation: impl FnMut(&mut TcpStream) -> io::Result<usize>,
    ) -> io::Result<usize> {
        loop {
            if (self.cancelled)() {
                return Err(io::ErrorKind::ConnectionAborted.into());
            }
            if Instant::now() >= self.deadline {
                return Err(io::ErrorKind::TimedOut.into());
            }
            match operation(self.stream) {
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(1))
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => (),
                result => return result,
            }
        }
    }
}
impl<F: Fn() -> bool> Read for Wire<'_, F> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.retry(|stream| stream.read(bytes))
    }
}
impl<F: Fn() -> bool> Write for Wire<'_, F> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.retry(|stream| stream.write(bytes))
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
