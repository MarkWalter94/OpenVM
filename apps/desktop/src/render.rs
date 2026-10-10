//! F1-16: selezione automatica del backend di rendering con indicazione
//! visibile (ADR-0014).
//!
//! Ordine predefinito (`--renderer auto`): **glow** → **wgpu su adattatore
//! hardware** (DX12 su Windows) → **wgpu su rasterizzatore software** (WARP su
//! Windows, llvmpipe su Linux). Un tentativo fallisce quando eframe restituisce
//! un errore prima di creare l'app (contesto GL, superficie o adattatore non
//! disponibili); il ciclo in `main.rs` passa allora al tentativo successivo
//! riusando lo stesso event loop (`NativeOptions::run_and_return`).
//!
//! Il backend software è una **modalità degradata dichiarata**: CPU elevata e
//! frequenza della console ridotta (`FPS_CONSOLE_DEGRADATA`).
//!
//! Questo modulo contiene solo la logica pura (piano dei tentativi, testi
//! dell'indicatore); la creazione delle finestre è in `main.rs`.

/// Massimo di aggiornamenti al secondo della console in modalità degradata.
pub const FPS_CONSOLE_DEGRADATA: u32 = 5;

/// Un tentativo di inizializzazione del rendering.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tentativo {
    /// OpenGL tramite glow/glutin, accelerazione hardware preferita.
    Glow,
    /// wgpu su adattatore hardware (DX12 su Windows; Vulkan/GL altrove).
    WgpuHardware,
    /// wgpu su adattatore di tipo CPU (WARP su Windows): modalità degradata.
    WgpuSoftware,
}

impl Tentativo {
    /// Ordine della selezione automatica.
    pub const AUTO: [Self; 3] = [Self::Glow, Self::WgpuHardware, Self::WgpuSoftware];

    /// Nome usato sulla riga di comando.
    pub fn nome(self) -> &'static str {
        match self {
            Self::Glow => "glow",
            Self::WgpuHardware => "wgpu",
            Self::WgpuSoftware => "warp",
        }
    }

    /// Etichetta breve per la barra di stato.
    pub fn etichetta(self) -> &'static str {
        match self {
            Self::Glow => "glow (OpenGL)",
            Self::WgpuHardware if cfg!(windows) => "wgpu DX12",
            Self::WgpuHardware => "wgpu (hardware)",
            Self::WgpuSoftware if cfg!(windows) => "wgpu WARP (software)",
            Self::WgpuSoftware => "wgpu software",
        }
    }

    /// Il tentativo usa un rasterizzatore software.
    pub fn degradato(self) -> bool {
        matches!(self, Self::WgpuSoftware)
    }

    /// Il backend è compilato in questa build.
    pub fn compilato(self) -> bool {
        match self {
            Self::Glow => cfg!(feature = "glow"),
            Self::WgpuHardware | Self::WgpuSoftware => cfg!(feature = "wgpu"),
        }
    }
}

/// Piano dei tentativi scelto da riga di comando.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Piano {
    /// Tentativi nell'ordine.
    pub tentativi: Vec<Tentativo>,
    /// `true` se l'utente ha forzato un solo backend (nessun ripiego).
    pub forzato: bool,
}

impl Piano {
    /// Interpreta `--renderer`: `auto` oppure un elenco separato da virgole
    /// di `glow`, `wgpu`, `warp` (per esempio `glow,warp`). Un solo nome
    /// forza quel backend senza ripiego. `wgpu_cpu` (`--wgpu-adapter cpu`,
    /// compatibilità con F0) sostituisce `wgpu` con `warp`.
    pub fn da_argomenti(renderer: Option<&str>, wgpu_cpu: bool) -> Result<Self, String> {
        let spec = renderer.unwrap_or("auto");
        let (mut tentativi, forzato) = if spec == "auto" {
            (Tentativo::AUTO.to_vec(), false)
        } else {
            let mut v = Vec::new();
            for parte in spec.split(',') {
                let t = match parte.trim() {
                    "glow" => Tentativo::Glow,
                    "wgpu" => Tentativo::WgpuHardware,
                    "warp" | "software" => Tentativo::WgpuSoftware,
                    altro => return Err(format!("renderer sconosciuto: {altro}")),
                };
                if v.contains(&t) {
                    return Err(format!("renderer ripetuto: {}", t.nome()));
                }
                v.push(t);
            }
            if v.is_empty() {
                return Err("--renderer vuoto".into());
            }
            // Un solo nome forza il backend; un elenco è un ordine di ripiego.
            let forzato = v.len() == 1;
            (v, forzato)
        };
        if wgpu_cpu {
            if renderer.is_none() {
                // F0: `--wgpu-adapter cpu` da solo forza WARP.
                tentativi = vec![Tentativo::WgpuSoftware];
                return Ok(Self {
                    tentativi,
                    forzato: true,
                });
            }
            for t in &mut tentativi {
                if *t == Tentativo::WgpuHardware {
                    *t = Tentativo::WgpuSoftware;
                }
            }
            tentativi.dedup();
        }
        let non_compilati: Vec<_> = tentativi
            .iter()
            .filter(|t| !t.compilato())
            .map(|t| t.nome())
            .collect();
        if spec != "auto" && !non_compilati.is_empty() {
            return Err(format!(
                "renderer non inclusi in questa build (feature cargo): {}",
                non_compilati.join(", ")
            ));
        }
        tentativi.retain(|t| t.compilato());
        if tentativi.is_empty() {
            return Err("nessun backend di rendering compilato".into());
        }
        Ok(Self { tentativi, forzato })
    }
}

/// Backend effettivo e storia dei tentativi, mostrati nella barra di stato.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InfoRendering {
    /// Tentativo riuscito (`None` se non determinato, per esempio nei test).
    pub attivo: Option<Tentativo>,
    /// Descrizione dell'adattatore o del contesto GL.
    pub dettaglio: String,
    /// L'adattatore effettivo è di tipo CPU (anche se scelto come hardware).
    pub adattatore_cpu: bool,
    /// Tentativi falliti prima di quello attivo, con il motivo.
    pub falliti: Vec<(Tentativo, String)>,
    /// Scelta forzata da riga di comando.
    pub forzato: bool,
}

impl Default for InfoRendering {
    fn default() -> Self {
        Self {
            attivo: None,
            dettaglio: "non determinato".into(),
            adattatore_cpu: false,
            falliti: Vec::new(),
            forzato: false,
        }
    }
}

impl InfoRendering {
    /// Modalità degradata: rasterizzatore software.
    pub fn degradato(&self) -> bool {
        self.adattatore_cpu || self.attivo.is_some_and(Tentativo::degradato)
    }

    /// Testo principale dell'indicatore.
    pub fn etichetta(&self) -> String {
        format!("Rendering: {}", self.backend())
    }

    /// Backend in uso e come è stato scelto, senza prefisso.
    pub fn backend(&self) -> String {
        let base = self.attivo.map_or("non determinato", Tentativo::etichetta);
        let origine = match (self.attivo, self.forzato) {
            (None, _) => "",
            (Some(_), true) => " · forzato",
            (Some(_), false) if self.falliti.is_empty() => " · automatico",
            (Some(_), false) => " · ripiego automatico",
        };
        format!("{base}{origine}")
    }

    /// Spiegazione completa (suggerimento al passaggio del mouse e log).
    pub fn spiegazione(&self) -> String {
        let mut s = self.dettaglio.clone();
        for (t, motivo) in &self.falliti {
            s.push_str(&format!("\ntentativo {} fallito: {motivo}", t.nome()));
        }
        if self.degradato() {
            s.push_str(&format!(
                "\nmodalità degradata: rendering software, CPU elevata, console limitata a {FPS_CONSOLE_DEGRADATA} aggiornamenti/s"
            ));
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(all(feature = "glow", feature = "wgpu"))]
    #[test]
    fn auto_segue_l_ordine_glow_wgpu_warp() {
        let p = Piano::da_argomenti(None, false).unwrap();
        assert_eq!(p.tentativi, Tentativo::AUTO.to_vec());
        assert!(!p.forzato);
        let p = Piano::da_argomenti(Some("glow,warp"), false).unwrap();
        assert_eq!(p.tentativi, vec![Tentativo::Glow, Tentativo::WgpuSoftware]);
        assert!(
            !p.forzato,
            "un elenco è un ordine di ripiego, non una forzatura"
        );
        assert!(Piano::da_argomenti(Some("glow"), false).unwrap().forzato);
    }

    #[cfg(feature = "wgpu")]
    #[test]
    fn wgpu_adapter_cpu_forza_warp() {
        let p = Piano::da_argomenti(None, true).unwrap();
        assert_eq!(p.tentativi, vec![Tentativo::WgpuSoftware]);
        assert!(p.forzato);
        let p = Piano::da_argomenti(Some("wgpu"), true).unwrap();
        assert_eq!(p.tentativi, vec![Tentativo::WgpuSoftware]);
    }

    #[test]
    fn nomi_non_validi_rifiutati() {
        assert!(Piano::da_argomenti(Some("vulkan"), false).is_err());
        assert!(Piano::da_argomenti(Some("glow,glow"), false).is_err());
        assert!(Piano::da_argomenti(Some(""), false).is_err());
    }

    #[test]
    fn indicatore_segnala_ripiego_e_degrado() {
        let info = InfoRendering {
            attivo: Some(Tentativo::WgpuSoftware),
            dettaglio: "adapter WARP".into(),
            adattatore_cpu: true,
            falliti: vec![(Tentativo::Glow, "opengl 2.0+".into())],
            forzato: false,
        };
        assert!(info.degradato());
        assert!(info.etichetta().contains("ripiego automatico"));
        assert!(info.spiegazione().contains("tentativo glow fallito"));
        assert!(info.spiegazione().contains("modalità degradata"));
        assert!(!InfoRendering::default().degradato());
    }
}
