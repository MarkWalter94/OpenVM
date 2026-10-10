//! Detection of the parent process (UI) death.
//!
//! Main mechanism, identical on Windows and Linux: **lifeline pipe** on stdin.
//! The UI starts the service with stdin connected to an anonymous pipe whose
//! only write end it owns; when the UI terminates (even through a crash or a
//! kill) the operating system closes that handle and the service reads EOF.
//! It does not depend on the PID (no reuse possible) and requires no `unsafe`.
//! The same channel accepts the text command `shutdown` for the controlled
//! shutdown requested by the UI.
//!
//! On Linux a `pidfd` on the parent is added (F0-07: `pidfd_open` + `poll`),
//! opened at startup after checking `getppid()`: it covers the case where the
//! write end of stdin was inherited by mistake by another process.
//!
//! On Windows, waiting on a parent handle (`OpenProcess` +
//! `WaitForSingleObject`) would require `unsafe` FFI in the crate; the lifeline
//! pipe offers the same semantics tied to the process lifetime. See
//! `docs/verification/F1-04-service-ipc-ci.md`.

use std::io::BufRead;

/// Event from the parent channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParentSignal {
    /// The parent requests the controlled shutdown.
    Shutdown,
    /// The parent terminated (EOF or error on stdin, or readable pidfd).
    Lost,
}

/// Starts the thread that reads stdin until EOF and calls `notify`.
pub fn watch_stdin(notify: impl Fn(ParentSignal) + Send + 'static) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("lightvm-parent-stdin".into())
        .spawn(move || {
            let stdin = std::io::stdin();
            let mut lines = stdin.lock();
            let mut buf = String::new();
            loop {
                buf.clear();
                match lines.read_line(&mut buf) {
                    Ok(0) | Err(_) => {
                        notify(ParentSignal::Lost);
                        return;
                    }
                    Ok(_) => {
                        if buf.trim() == "shutdown" {
                            notify(ParentSignal::Shutdown);
                        }
                        // The channel carries nothing else: other lines are ignored.
                        if buf.len() > 4096 {
                            buf = String::new();
                        }
                    }
                }
            }
        })
        .map(|_| ())
}

/// Linux: watches the parent with a pidfd.
#[cfg(target_os = "linux")]
pub fn watch_parent_pidfd(notify: impl Fn(ParentSignal) + Send + 'static) -> std::io::Result<()> {
    use rustix::event::{PollFd, PollFlags, poll};
    use rustix::process::{PidfdFlags, getppid, pidfd_open};

    let Some(ppid) = getppid() else {
        notify(ParentSignal::Lost);
        return Ok(());
    };
    let fd = pidfd_open(ppid, PidfdFlags::empty())?;
    // If the parent died between getppid and pidfd_open the process has been
    // reparented: the pidfd might refer to a reused PID.
    if getppid() != Some(ppid) {
        notify(ParentSignal::Lost);
        return Ok(());
    }
    std::thread::Builder::new()
        .name("lightvm-parent-pidfd".into())
        .spawn(move || {
            loop {
                let mut fds = [PollFd::new(&fd, PollFlags::IN)];
                match poll(&mut fds, None) {
                    Ok(n) if n > 0 => {
                        notify(ParentSignal::Lost);
                        return;
                    }
                    Ok(_) => {}
                    Err(rustix::io::Errno::INTR) => {}
                    Err(_) => return,
                }
            }
        })
        .map(|_| ())
}
