//! F1-14: librerie Linux caricate con `dlopen` e trasformazione del panico di
//! `xkbcommon-dl` (D-17) in un errore utilizzabile.
//!
//! winit, glutin e wgpu non collegano X11, Wayland, xkbcommon, EGL/GL e Vulkan
//! al binario: li aprono a runtime. Se una libreria manca, `xkbcommon-dl` va in
//! panico (`Library libxkbcommon-x11.so could not be loaded.`) invece di
//! restituire un errore.
//!
//! Due difese, senza codice `unsafe`:
//!
//! 1. **controllo preventivo** prima di creare la finestra: cerca i nomi delle
//!    librerie nella cache del linker dinamico (`/etc/ld.so.cache`), nelle
//!    directory di `LD_LIBRARY_PATH` e nelle directory predefinite, cioè negli
//!    stessi posti in cui guarda `dlopen`;
//! 2. **`catch_unwind`** attorno all'avvio di eframe in `main.rs`: un panico
//!    durante la creazione dell'event loop diventa un messaggio che elenca le
//!    librerie mancanti e i pacchetti da installare.
//!
//! La logica di analisi è pura e si prova su ogni piattaforma; la lettura del
//! file system è usata solo su Linux.

/// Sessione grafica rilevata dalle variabili d'ambiente.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sessione {
    /// `WAYLAND_DISPLAY` impostata (winit preferisce Wayland).
    Wayland,
    /// Solo `DISPLAY` impostata.
    X11,
    /// Nessuna sessione grafica.
    Nessuna,
}

impl Sessione {
    /// Rileva la sessione come fa winit (Wayland prima di X11).
    pub fn rileva(wayland_display: Option<&str>, display: Option<&str>) -> Self {
        let impostata = |v: Option<&str>| v.is_some_and(|s| !s.is_empty());
        if impostata(wayland_display) {
            Self::Wayland
        } else if impostata(display) {
            Self::X11
        } else {
            Self::Nessuna
        }
    }
}

/// Libreria caricata a runtime, con il pacchetto Debian/Ubuntu che la fornisce.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Libreria {
    /// Nome del file aperto con `dlopen`.
    pub file: &'static str,
    /// Pacchetto Debian/Ubuntu.
    pub pacchetto: &'static str,
    /// A che cosa serve.
    pub uso: &'static str,
}

const XKB: Libreria = Libreria {
    file: "libxkbcommon.so.0",
    pacchetto: "libxkbcommon0",
    uso: "tastiera (xkbcommon)",
};
const XKB_X11: Libreria = Libreria {
    file: "libxkbcommon-x11.so.0",
    pacchetto: "libxkbcommon-x11-0",
    uso: "tastiera su X11",
};
const X11: Libreria = Libreria {
    file: "libX11.so.6",
    pacchetto: "libx11-6",
    uso: "finestra X11",
};
const X11_XCB: Libreria = Libreria {
    file: "libX11-xcb.so.1",
    pacchetto: "libx11-xcb1",
    uso: "finestra X11 (xcb)",
};
const XCURSOR: Libreria = Libreria {
    file: "libXcursor.so.1",
    pacchetto: "libxcursor1",
    uso: "cursori X11",
};
const XI: Libreria = Libreria {
    file: "libXi.so.6",
    pacchetto: "libxi6",
    uso: "input X11",
};
const WAYLAND: Libreria = Libreria {
    file: "libwayland-client.so.0",
    pacchetto: "libwayland-client0",
    uso: "finestra Wayland",
};
const EGL: Libreria = Libreria {
    file: "libEGL.so.1",
    pacchetto: "libegl1",
    uso: "OpenGL/EGL (glow, wgpu GL)",
};
const GL: Libreria = Libreria {
    file: "libGL.so.1",
    pacchetto: "libgl1",
    uso: "OpenGL/GLX (glow su X11)",
};
const VULKAN: Libreria = Libreria {
    file: "libvulkan.so.1",
    pacchetto: "libvulkan1",
    uso: "Vulkan (wgpu)",
};

/// Librerie indispensabili per la sessione, raggruppate: ogni gruppo è
/// soddisfatto se **almeno una** delle librerie è presente.
pub fn gruppi_richiesti(sessione: Sessione) -> Vec<Vec<Libreria>> {
    let mut g = vec![vec![XKB]];
    match sessione {
        Sessione::Wayland => g.push(vec![WAYLAND]),
        Sessione::X11 => {
            for l in [XKB_X11, X11, X11_XCB, XCURSOR, XI] {
                g.push(vec![l]);
            }
        }
        Sessione::Nessuna => {}
    }
    // Almeno un'API grafica: EGL o GL per glow, Vulkan (o EGL) per wgpu.
    g.push(vec![EGL, GL, VULKAN]);
    g
}

/// Gruppi non soddisfatti, dato un predicato di presenza.
pub fn mancanti(sessione: Sessione, presente: impl Fn(&str) -> bool) -> Vec<Vec<Libreria>> {
    gruppi_richiesti(sessione)
        .into_iter()
        .filter(|g| !g.iter().any(|l| presente(l.file)))
        .collect()
}

/// Messaggio d'errore per l'utente.
pub fn messaggio(sessione: Sessione, mancanti: &[Vec<Libreria>], causa: Option<&str>) -> String {
    let mut s = String::from(
        "LightVM non può aprire la finestra: mancano librerie di sistema caricate a runtime.\n",
    );
    s.push_str(&format!("Sessione grafica rilevata: {sessione:?}.\n"));
    if mancanti.is_empty() {
        s.push_str(
            "Il controllo preventivo non ha trovato librerie mancanti; verificare comunque:\n",
        );
        for g in gruppi_richiesti(sessione) {
            s.push_str(&riga_gruppo(&g));
        }
    } else {
        s.push_str("Librerie mancanti:\n");
        for g in mancanti {
            s.push_str(&riga_gruppo(g));
        }
        let pacchetti: Vec<&str> = mancanti
            .iter()
            .filter_map(|g| g.first().map(|l| l.pacchetto))
            .collect();
        s.push_str(&format!(
            "Su Debian/Ubuntu: sudo apt install {}\n",
            pacchetti.join(" ")
        ));
    }
    if sessione == Sessione::Nessuna {
        s.push_str("Nessuna sessione grafica: né WAYLAND_DISPLAY né DISPLAY sono impostate.\n");
    }
    if let Some(c) = causa {
        s.push_str(&format!("Dettaglio tecnico: {c}\n"));
    }
    s
}

fn riga_gruppo(g: &[Libreria]) -> String {
    let alternative: Vec<String> = g
        .iter()
        .map(|l| format!("{} (pacchetto {}, {})", l.file, l.pacchetto, l.uso))
        .collect();
    format!("  - {}\n", alternative.join(" oppure "))
}

/// Il messaggio di panico riguarda una libreria caricata a runtime.
pub fn panico_da_libreria(msg: &str) -> bool {
    msg.contains("could not be loaded")
        || msg.contains("xkbcommon")
        || msg.contains("cannot open shared object")
        || msg.contains("Failed to open")
}

/// Directory in cui `dlopen` cerca oltre alla cache (ld.so(8)).
#[cfg(target_os = "linux")]
const DIRECTORY_PREDEFINITE: &[&str] = &[
    "/lib",
    "/usr/lib",
    "/lib64",
    "/usr/lib64",
    "/lib/x86_64-linux-gnu",
    "/usr/lib/x86_64-linux-gnu",
    "/usr/local/lib",
];

/// Presenza di una libreria sul sistema corrente (solo Linux).
#[cfg(target_os = "linux")]
pub fn presente_nel_sistema(cache: &[u8], file: &str) -> bool {
    use std::path::Path;
    // La cache di ld.so contiene i nomi come stringhe terminate da NUL.
    let mut ago = file.as_bytes().to_vec();
    ago.push(0);
    let in_cache = cache.windows(ago.len()).any(|w| w == ago.as_slice());
    if in_cache {
        return true;
    }
    let ld = std::env::var("LD_LIBRARY_PATH").unwrap_or_default();
    ld.split(':')
        .filter(|d| !d.is_empty())
        .chain(DIRECTORY_PREDEFINITE.iter().copied())
        .any(|d| Path::new(d).join(file).exists())
}

/// Controllo preventivo prima di creare la finestra (solo Linux).
/// `Err` con il messaggio per l'utente se manca qualcosa di indispensabile.
#[cfg(target_os = "linux")]
pub fn controllo_preventivo() -> Result<Sessione, String> {
    let sessione = Sessione::rileva(
        std::env::var("WAYLAND_DISPLAY").ok().as_deref(),
        std::env::var("DISPLAY").ok().as_deref(),
    );
    let cache = std::fs::read("/etc/ld.so.cache").unwrap_or_default();
    let m = mancanti(sessione, |f| presente_nel_sistema(&cache, f));
    if m.is_empty() && sessione != Sessione::Nessuna {
        Ok(sessione)
    } else {
        Err(messaggio(sessione, &m, None))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sessione_wayland_prevale_su_x11() {
        assert_eq!(
            Sessione::rileva(Some("wayland-0"), Some(":0")),
            Sessione::Wayland
        );
        assert_eq!(Sessione::rileva(Some(""), Some(":0")), Sessione::X11);
        assert_eq!(Sessione::rileva(None, None), Sessione::Nessuna);
    }

    #[test]
    fn x11_senza_xkbcommon_x11_elenca_il_pacchetto() {
        let presenti = [
            "libxkbcommon.so.0",
            "libX11.so.6",
            "libX11-xcb.so.1",
            "libXcursor.so.1",
            "libXi.so.6",
            "libvulkan.so.1",
        ];
        let m = mancanti(Sessione::X11, |f| presenti.contains(&f));
        assert_eq!(m.len(), 1);
        assert_eq!(m[0][0].file, "libxkbcommon-x11.so.0");
        let msg = messaggio(Sessione::X11, &m, Some("panico"));
        assert!(msg.contains("libxkbcommon-x11-0"));
        assert!(msg.contains("sudo apt install libxkbcommon-x11-0"));
        assert!(msg.contains("Dettaglio tecnico: panico"));
    }

    #[test]
    fn senza_alcuna_libreria_elenca_quelle_richieste_dalla_roadmap() {
        let m = mancanti(Sessione::Wayland, |_| false);
        let msg = messaggio(Sessione::Wayland, &m, None);
        for p in [
            "libxkbcommon0",
            "libwayland-client0",
            "libegl1",
            "libgl1",
            "libvulkan1",
        ] {
            assert!(msg.contains(p), "manca {p} nel messaggio");
        }
    }

    #[test]
    fn un_api_grafica_basta() {
        let presenti = ["libxkbcommon.so.0", "libwayland-client.so.0", "libEGL.so.1"];
        assert!(mancanti(Sessione::Wayland, |f| presenti.contains(&f)).is_empty());
    }

    #[test]
    fn riconosce_il_panico_di_xkbcommon_dl() {
        assert!(panico_da_libreria(
            "Library libxkbcommon-x11.so could not be loaded."
        ));
        assert!(!panico_da_libreria("index out of bounds"));
    }
}
