//! Rilevamento della morte del processo genitore (UI).
//!
//! Meccanismo principale, identico su Windows e Linux: **pipe di vita** sullo
//! stdin. La UI avvia il servizio con stdin collegato a una pipe anonima di cui
//! possiede l'unica estremità di scrittura; quando la UI termina (anche per
//! crash o kill) il sistema operativo chiude quell'handle e il servizio legge
//! EOF. Non dipende dal PID (nessun riuso possibile) e non richiede `unsafe`.
//! Lo stesso canale accetta il comando testuale `shutdown` per l'arresto
//! controllato richiesto dalla UI.
//!
//! Su Linux si aggiunge un `pidfd` sul genitore (F0-07: `pidfd_open` + `poll`),
//! aperto all'avvio dopo aver verificato `getppid()`: copre il caso in cui
//! l'estremità di scrittura dello stdin sia stata ereditata per errore da un
//! altro processo.
//!
//! Su Windows l'attesa su un handle del genitore (`OpenProcess` +
//! `WaitForSingleObject`) richiederebbe FFI `unsafe` nel crate; la pipe di vita
//! offre la stessa semantica legata alla vita del processo. Vedi
//! `docs/verification/F1-04-service-ipc-ci.md`.

use std::io::BufRead;

/// Evento dal canale del genitore.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParentSignal {
    /// Il genitore chiede l'arresto controllato.
    Shutdown,
    /// Il genitore è terminato (EOF o errore sullo stdin, o pidfd leggibile).
    Lost,
}

/// Avvia il thread che legge lo stdin fino a EOF e chiama `notify`.
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
                        // Il canale non trasporta altro: righe diverse ignorate.
                        if buf.len() > 4096 {
                            buf = String::new();
                        }
                    }
                }
            }
        })
        .map(|_| ())
}

/// Linux: sorveglia il genitore con un pidfd.
#[cfg(target_os = "linux")]
pub fn watch_parent_pidfd(notify: impl Fn(ParentSignal) + Send + 'static) -> std::io::Result<()> {
    use rustix::event::{PollFd, PollFlags, poll};
    use rustix::process::{PidfdFlags, getppid, pidfd_open};

    let Some(ppid) = getppid() else {
        notify(ParentSignal::Lost);
        return Ok(());
    };
    let fd = pidfd_open(ppid, PidfdFlags::empty())?;
    // Se il genitore è morto tra getppid e pidfd_open il processo è stato
    // riassegnato: il pidfd potrebbe riferirsi a un PID riusato.
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
