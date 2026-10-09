//! Sessione su thread dedicato: consegna gli aggiornamenti tramite callback e
//! mantiene una copia condivisa del framebuffer per la UI.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::transport::SocketControl;
use crate::{ChannelKind, Connection, Framebuffer, Rect, ServerInit, Transport};

/// Configurazione della sessione.
#[derive(Clone, Debug)]
pub struct SessionConfig {
    /// Trasporto.
    pub transport: Transport,
    /// Massimo numero di FramebufferUpdateRequest al secondo (>= 1).
    pub max_updates_per_second: u32,
}

/// Aggiornamento consegnato alla UI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateInfo {
    /// Numero progressivo dell'aggiornamento.
    pub seq: u64,
    /// Rettangoli modificati (già copiati nel framebuffer condiviso).
    pub rects: Vec<Rect>,
    /// Ridimensionamento avvenuto.
    pub resized: bool,
    /// Larghezza corrente.
    pub width: u16,
    /// Altezza corrente.
    pub height: u16,
}

/// Eventi della sessione.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionEvent {
    /// Handshake completato.
    Connected {
        /// ServerInit.
        init: ServerInit,
        /// Canale.
        channel: ChannelKind,
        /// Sicurezza negoziata.
        security: String,
    },
    /// Nuovo aggiornamento.
    Update(UpdateInfo),
    /// Fine della sessione, con l'errore se non è stata chiusa localmente.
    Disconnected {
        /// Errore, se presente.
        error: Option<String>,
    },
}

/// Sessione RFB su un thread dedicato.
pub struct Session {
    fb: Arc<Mutex<Framebuffer>>,
    control: Arc<Mutex<Option<SocketControl>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session").finish_non_exhaustive()
    }
}

impl Session {
    /// Avvia la sessione. `on_event` è chiamata sul thread della sessione.
    pub fn spawn<F>(config: SessionConfig, mut on_event: F) -> Self
    where
        F: FnMut(SessionEvent) + Send + 'static,
    {
        let fb = Arc::new(Mutex::new(Framebuffer::new(0, 0)));
        let control: Arc<Mutex<Option<SocketControl>>> = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let (fb2, control2, stop2) = (fb.clone(), control.clone(), stop.clone());
        let thread = std::thread::Builder::new()
            .name("rfb-session".into())
            .spawn(move || {
                let res = run(&config, &fb2, &control2, &stop2, &mut on_event);
                let error = match res {
                    Ok(()) => None,
                    Err(_) if stop2.load(Ordering::SeqCst) => None,
                    Err(e) => Some(e.to_string()),
                };
                on_event(SessionEvent::Disconnected { error });
            })
            .ok();
        Self {
            fb,
            control,
            stop,
            thread,
        }
    }

    /// Framebuffer condiviso, aggiornato prima di ogni [`SessionEvent::Update`].
    pub fn framebuffer(&self) -> Arc<Mutex<Framebuffer>> {
        self.fb.clone()
    }

    /// Chiude la connessione e attende la fine del thread (al massimo circa
    /// 2 s; oltre, il thread viene lasciato terminare da solo).
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Ok(c) = self.control.lock()
            && let Some(c) = c.as_ref()
        {
            // Su Windows `shutdown` non sblocca una `recv` AF_UNIX in corso (F0-04):
            // si provoca una risposta del server, poi si chiude.
            c.wake();
            c.shutdown();
        }
        if let Some(t) = self.thread.take() {
            let deadline = Instant::now() + Duration::from_secs(2);
            while !t.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            if t.is_finished() {
                let _ = t.join();
            }
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run<F: FnMut(SessionEvent)>(
    config: &SessionConfig,
    fb: &Mutex<Framebuffer>,
    control: &Mutex<Option<SocketControl>>,
    stop_flag: &Arc<AtomicBool>,
    on_event: &mut F,
) -> Result<(), crate::Error> {
    let mut conn = Connection::connect(&config.transport)?;
    if let Ok(mut c) = control.lock() {
        *c = conn.control();
    }
    if stop_flag.load(Ordering::SeqCst) {
        return Ok(());
    }
    {
        let mut shared = fb.lock().map_err(|_| crate::Error::Closed)?;
        *shared = conn.framebuffer().clone();
    }
    on_event(SessionEvent::Connected {
        init: conn.server_init().clone(),
        channel: conn.channel(),
        security: conn.security().to_owned(),
    });
    let min_interval = Duration::from_secs(1) / config.max_updates_per_second.max(1);
    let mut seq = 0u64;
    loop {
        let requested = Instant::now();
        conn.request_update()?;
        let u = conn.read_update()?;
        if stop_flag.load(Ordering::SeqCst) {
            return Ok(());
        }
        {
            let mut shared = fb.lock().map_err(|_| crate::Error::Closed)?;
            if u.resized || shared.width() != u.width || shared.height() != u.height {
                *shared = conn.framebuffer().clone();
            } else {
                for r in &u.rects {
                    shared.copy_from(conn.framebuffer(), *r);
                }
            }
        }
        seq += 1;
        on_event(SessionEvent::Update(UpdateInfo {
            seq,
            rects: u.rects,
            resized: u.resized,
            width: u.width,
            height: u.height,
        }));
        let elapsed = requested.elapsed();
        if elapsed < min_interval {
            std::thread::sleep(min_interval - elapsed);
        }
    }
}
