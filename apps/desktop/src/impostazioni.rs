//! Impostazioni della UI, modificate solo dalla pagina Settings e salvate
//! dall'app in un file JSON (l'utente non lo modifica a mano): supervisor
//! (catalogo, distribuzione QEMU, avvio automatico), sorte delle VM se la UI
//! termina senza arresto controllato (ADR-0015 parte B), backend di rendering
//! preferito (ADR-0014; vale dal prossimo avvio).
//!
//! Il file vive nel profilo utente (`%LOCALAPPDATA%\LightVM\impostazioni-ui.json`
//! su Windows, `$XDG_CONFIG_HOME/lightvm/impostazioni-ui.json` o
//! `~/.config/lightvm/…` altrove) oppure dove indica `--impostazioni`. Un file
//! assente vale come impostazioni predefinite; un file illeggibile o non valido
//! è segnalato e sostituito dai valori predefiniti, senza bloccare l'avvio.

use std::path::{Path, PathBuf};

use egui::WidgetType;
use lightvm_proto::ipc::ParentLossPolicy;
use serde::{Deserialize, Serialize};

use crate::tema::{self, Tono};

/// Nome del file nel profilo utente.
pub const FILE: &str = "impostazioni-ui.json";

/// Attese proposte per lo spegnimento ordinato, in secondi (nessun campo di
/// testo nella UI).
pub const ATTESE_SPEGNIMENTO_S: [u32; 6] = [5, 10, 30, 60, 120, 300];

/// "Lascia le VM accese" selezionabile dalla UI: solo quando esisterà il
/// ricollegamento di una nuova UI al supervisor (ADR-0015 parte B, criterio 4).
pub const KEEP_VMS_DISPONIBILE: bool = false;

/// Attesa predefinita quando si sceglie lo spegnimento ordinato.
pub const ATTESA_PREDEFINITA_S: u32 = 10;

/// Backend di rendering preferito. Il backend scelto è provato per primo; gli
/// altri restano come ripiego, così una scelta non disponibile non impedisce
/// di aprire la finestra. WARP scelto esplicitamente è l'unico tentativo.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreferenzaRendering {
    /// glow → wgpu (hardware) → WARP (predefinito: glow sta nell'obiettivo di
    /// memoria, `F1-05`).
    #[default]
    Automatico,
    /// glow per primo (come automatico).
    Glow,
    /// wgpu su hardware per primo, poi glow e WARP. Memoria più alta (D-12).
    Wgpu,
    /// Solo rasterizzatore software (modalità degradata).
    Warp,
}

impl PreferenzaRendering {
    /// Tutte le scelte, nell'ordine mostrato.
    pub const TUTTE: [Self; 4] = [Self::Automatico, Self::Glow, Self::Wgpu, Self::Warp];

    /// Valore equivalente di `--renderer`.
    pub fn come_argomento(self) -> &'static str {
        match self {
            Self::Automatico => "auto",
            Self::Glow => "glow,wgpu,warp",
            Self::Wgpu => "wgpu,glow,warp",
            Self::Warp => "warp",
        }
    }

    /// Etichetta mostrata in Settings.
    pub fn etichetta(self) -> &'static str {
        match self {
            Self::Automatico => "Automatico: glow, poi wgpu, poi WARP (predefinito)",
            Self::Glow => "glow (OpenGL) per primo",
            Self::Wgpu => "wgpu (DX12/Vulkan) per primo: usa più memoria",
            Self::Warp => "Solo software (WARP): lento, per sessioni senza GPU",
        }
    }
}

/// Impostazioni persistenti della UI.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Impostazioni {
    /// Sorte delle VM alla perdita della UI (predefinita: terminarle).
    pub politica_perdita_ui: ParentLossPolicy,
    /// Backend di rendering preferito (vale dal prossimo avvio).
    pub rendering: PreferenzaRendering,
    /// Avvia il supervisor all'apertura di LightVM.
    pub avvio_supervisor: bool,
    /// Cartella del catalogo; `None` = predefinita nel profilo utente.
    pub catalogo: Option<PathBuf>,
    /// Distribuzione QEMU approvata; `None` = nessuna (avvio delle VM
    /// rifiutato dal supervisor con un messaggio chiaro).
    pub qemu_dir: Option<PathBuf>,
    /// Tema dell'interfaccia.
    pub tema: crate::UiTheme,
}

impl Default for Impostazioni {
    fn default() -> Self {
        Self {
            politica_perdita_ui: ParentLossPolicy::default(),
            rendering: PreferenzaRendering::default(),
            avvio_supervisor: true,
            catalogo: None,
            qemu_dir: None,
            tema: crate::UiTheme::Dark,
        }
    }
}

/// Catalogo predefinito nel profilo utente.
pub fn catalogo_predefinito() -> Option<PathBuf> {
    percorso_predefinito().and_then(|f| f.parent().map(|d| d.join("catalogo")))
}

impl Impostazioni {
    /// Catalogo effettivo (scelto o predefinito).
    pub fn catalogo_effettivo(&self) -> Option<PathBuf> {
        self.catalogo.clone().or_else(catalogo_predefinito)
    }
}

/// Azioni richieste dalla sezione Supervisor; le esegue l'app (il selettore
/// di cartelle è nativo e non è disponibile nei test headless).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AzioneSupervisor {
    /// Scegliere la cartella del catalogo.
    ScegliCatalogo,
    /// Tornare al catalogo predefinito.
    CatalogoPredefinito,
    /// Scegliere la cartella della distribuzione QEMU.
    ScegliQemu,
    /// Nessuna distribuzione QEMU.
    RimuoviQemu,
    /// Applicare le impostazioni riavviando il supervisor.
    Riavvia,
    /// Arrestare il supervisor.
    Arresta,
}

/// Stato del supervisor mostrato nella sezione.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatoSupervisorUi {
    /// Testo dello stato (connesso, non avviato, errore).
    pub descrizione: String,
    /// Tono dell'indicatore, uguale a quello della barra laterale.
    pub tono: Tono,
    /// Il supervisor è in esecuzione.
    pub attivo: bool,
    /// Riavvio o arresto in corso: azioni disabilitate.
    pub in_corso: bool,
    /// VM accese che un riavvio arresterebbe.
    pub vm_accese: usize,
    /// Le impostazioni del supervisor differiscono da quelle in uso.
    pub da_applicare: bool,
    /// Il selettore di cartelle è disponibile su questa piattaforma.
    pub selettore: bool,
    /// Catalogo e QEMU dati da riga di comando (`--catalogo`), in uso al
    /// posto di quelli scelti qui.
    pub riga_di_comando: Option<(PathBuf, Option<PathBuf>)>,
}

fn mostra_percorso(p: Option<&Path>, vuoto: &str) -> String {
    p.map_or_else(|| vuoto.to_owned(), |p| p.display().to_string())
}

/// Sezione Supervisor della pagina Settings: percorsi mostrati come testo non
/// modificabile, scelti con il selettore di cartelle nativo; avvio automatico;
/// riavvio per applicare. Restituisce l'azione richiesta e, separatamente, se
/// un valore modificabile qui (avvio automatico) è cambiato.
pub fn supervisor_ui(
    ui: &mut egui::Ui,
    imp: &mut Impostazioni,
    stato: &StatoSupervisorUi,
) -> (Option<AzioneSupervisor>, bool) {
    let mut azione = None;
    tema::riga(ui, "Stato", None, |ui| {
        tema::stato(ui, &stato.descrizione, stato.tono)
    });
    if let Some((catalogo, qemu)) = &stato.riga_di_comando {
        ui.add_space(4.0);
        tema::avviso(
            ui,
            &format!(
                "In uso da riga di comando: catalogo {}, QEMU {}. Le scelte qui sotto valgono negli avvii senza --catalogo.",
                catalogo.display(),
                mostra_percorso(qemu.as_deref(), "nessuna")
            ),
            Tono::Accento,
        );
    }
    tema::divisore(ui);
    tema::riga(
        ui,
        "Catalogo delle VM",
        Some("Configurazioni e dischi delle macchine virtuali."),
        |ui| {
            tema::percorso(
                ui,
                &mostra_percorso(imp.catalogo_effettivo().as_deref(), "non determinabile"),
            );
            ui.horizontal(|ui| {
                let r = ui.add_enabled(stato.selettore, egui::Button::new("Sfoglia…"));
                tema::nome_accessibile(&r, WidgetType::Button, "Scegli la cartella del catalogo");
                if r.clicked() {
                    azione = Some(AzioneSupervisor::ScegliCatalogo);
                }
                let r = ui.add_enabled(imp.catalogo.is_some(), tema::discreto("Predefinito"));
                tema::nome_accessibile(&r, WidgetType::Button, "Torna al catalogo predefinito");
                if r.clicked() {
                    azione = Some(AzioneSupervisor::CatalogoPredefinito);
                }
            });
        },
    );
    tema::divisore(ui);
    tema::riga(
        ui,
        "Distribuzione QEMU",
        Some("qemu-system-x86_64 e qemu-img approvati; il supervisor ne verifica gli hash."),
        |ui| {
            match imp.qemu_dir.as_deref() {
                Some(d) => {
                    tema::percorso(ui, &d.display().to_string());
                }
                None => {
                    tema::stato(
                        ui,
                        "non impostata: le VM non si possono avviare",
                        Tono::Attenzione,
                    );
                }
            }
            ui.horizontal(|ui| {
                let r = ui.add_enabled(stato.selettore, egui::Button::new("Sfoglia…"));
                tema::nome_accessibile(&r, WidgetType::Button, "Scegli la cartella di QEMU");
                if r.clicked() {
                    azione = Some(AzioneSupervisor::ScegliQemu);
                }
                let r = ui.add_enabled(imp.qemu_dir.is_some(), tema::discreto("Nessuna"));
                tema::nome_accessibile(&r, WidgetType::Button, "Nessuna distribuzione QEMU");
                if r.clicked() {
                    azione = Some(AzioneSupervisor::RimuoviQemu);
                }
            });
        },
    );
    tema::divisore(ui);
    let cambiato = tema::riga(ui, "Avvio", None, |ui| {
        ui.checkbox(
            &mut imp.avvio_supervisor,
            "Avvia il supervisor all'apertura di LightVM",
        )
        .changed()
    });
    if !stato.selettore {
        ui.add_space(4.0);
        tema::avviso(
            ui,
            "Selettore di cartelle non disponibile su questa piattaforma: usare --catalogo e --qemu-dir.",
            Tono::Neutro,
        );
    }
    if stato.da_applicare && stato.attivo {
        ui.add_space(4.0);
        tema::avviso(
            ui,
            "Modifiche non ancora applicate: riavviare il supervisor.",
            Tono::Attenzione,
        );
    }
    if stato.attivo && stato.vm_accese > 0 {
        ui.add_space(4.0);
        let testo = if stato.vm_accese == 1 {
            "Riavvio o arresto spengono la VM accesa (arresto controllato).".to_owned()
        } else {
            format!(
                "Riavvio o arresto spengono le {} VM accese (arresto controllato).",
                stato.vm_accese
            )
        };
        tema::avviso(ui, &testo, Tono::Attenzione);
    }
    tema::divisore(ui);
    ui.horizontal(|ui| {
        let testo = if stato.attivo {
            "Applica e riavvia il supervisor"
        } else {
            "Avvia il supervisor"
        };
        let avvia = tema::primario(ui, testo);
        if ui.add_enabled(!stato.in_corso, avvia).clicked() {
            azione = Some(AzioneSupervisor::Riavvia);
        }
        if ui
            .add_enabled(
                stato.attivo && !stato.in_corso,
                egui::Button::new("Arresta il supervisor"),
            )
            .clicked()
        {
            azione = Some(AzioneSupervisor::Arresta);
        }
    });
    (azione, cambiato)
}

/// Percorso predefinito nel profilo utente, se determinabile.
pub fn percorso_predefinito() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("LightVM").join(FILE))
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .map(|d| d.join("lightvm").join(FILE))
    }
}

impl Impostazioni {
    /// Legge il file. Assente → predefinite senza errore; illeggibile o non
    /// valido → predefinite con la descrizione del problema.
    pub fn carica(percorso: &Path) -> (Self, Option<String>) {
        let testo = match std::fs::read_to_string(percorso) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Self::default(), None),
            Err(e) => {
                return (
                    Self::default(),
                    Some(format!("impostazioni non lette: {e}")),
                );
            }
        };
        match serde_json::from_str::<Self>(&testo) {
            Ok(i) if i.politica_perdita_ui == ParentLossPolicy::KeepVms
                && !KEEP_VMS_DISPONIBILE => (
                Impostazioni {
                    politica_perdita_ui: ParentLossPolicy::default(),
                    ..i
                },
                Some("\"lascia le VM accese\" non è disponibile senza ricollegamento: uso \"termina subito le VM\"".into()),
            ),
            Ok(i) => match i.politica_perdita_ui.validate() {
                Ok(_) => (i, None),
                Err(e) => (
                    Self::default(),
                    Some(format!("impostazioni non valide: {}", e.message)),
                ),
            },
            Err(e) => (
                Self::default(),
                Some(format!("impostazioni non valide: {e}")),
            ),
        }
    }

    /// Scrive il file in modo atomico (file temporaneo accanto, poi rinomina).
    pub fn salva(&self, percorso: &Path) -> std::io::Result<()> {
        if let Some(dir) = percorso.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let testo = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        let tmp = percorso.with_extension("json.tmp");
        std::fs::write(&tmp, testo)?;
        std::fs::rename(&tmp, percorso)
    }
}

/// Descrizione leggibile della politica.
pub fn descrivi(p: ParentLossPolicy) -> String {
    match p {
        ParentLossPolicy::KillVms => "termina subito le VM".into(),
        ParentLossPolicy::ShutdownVms { powerdown_s } => {
            format!("spegnimento ordinato (attesa {powerdown_s} s), poi terminazione")
        }
        ParentLossPolicy::KeepVms => "lascia le VM accese".into(),
    }
}

/// Contenuto della scheda sulla politica alla perdita della UI. Restituisce
/// `true` se l'utente ha cambiato la scelta. Solo pulsanti di opzione e un
/// menu: nessun campo di testo.
pub fn politica_ui(ui: &mut egui::Ui, p: &mut ParentLossPolicy) -> bool {
    let prima = *p;
    let colori = tema::Palette::di(ui);
    ui.label(
        egui::RichText::new(
            "Crash o terminazione forzata della finestra: cosa fare delle VM accese.",
        )
        .color(colori.testo_debole),
    );
    ui.add_space(4.0);
    let attesa = match *p {
        ParentLossPolicy::ShutdownVms { powerdown_s } => powerdown_s,
        _ => ATTESA_PREDEFINITA_S,
    };
    if ui
        .radio(
            *p == ParentLossPolicy::KillVms,
            "Termina subito le VM (predefinito)",
        )
        .clicked()
    {
        *p = ParentLossPolicy::KillVms;
    }
    ui.horizontal(|ui| {
        let scelta = matches!(*p, ParentLossPolicy::ShutdownVms { .. });
        if ui
            .radio(scelta, "Spegnimento ordinato, poi terminazione")
            .clicked()
        {
            *p = ParentLossPolicy::ShutdownVms {
                powerdown_s: attesa,
            };
        }
        ui.add_enabled_ui(scelta, |ui| {
            let r = egui::ComboBox::from_id_salt("lightvm_attesa_spegnimento")
                .width(110.0)
                .selected_text(format!("attesa {attesa} s"))
                .show_ui(ui, |ui| {
                    for s in ATTESE_SPEGNIMENTO_S {
                        if ui.selectable_label(attesa == s, format!("{s} s")).clicked() {
                            *p = ParentLossPolicy::ShutdownVms { powerdown_s: s };
                        }
                    }
                });
            tema::nome_accessibile(
                &r.response,
                WidgetType::ComboBox,
                "Attesa dello spegnimento",
            );
        });
    });
    // ROADMAP 6.1 e ADR-0015 B4: lasciare le VM accese è ammesso solo con il
    // ricollegamento di una nuova UI al supervisor, non ancora disponibile.
    if ui
        .add_enabled(
            KEEP_VMS_DISPONIBILE,
            egui::RadioButton::new(
                *p == ParentLossPolicy::KeepVms,
                "Lascia le VM accese (richiede il ricollegamento della UI, non ancora disponibile)",
            ),
        )
        .on_disabled_hover_text(
            "Senza ricollegamento nessuna UI potrebbe più comandare quelle VM (ADR-0015 parte B).",
        )
        .clicked()
    {
        *p = ParentLossPolicy::KeepVms;
    }
    *p != prima
}

/// Contenuto della scheda del backend di rendering. Restituisce `true` se la
/// scelta cambia. `attivo` è il backend in uso in questa esecuzione.
pub fn rendering_ui(ui: &mut egui::Ui, p: &mut PreferenzaRendering, attivo: &str) -> bool {
    let prima = *p;
    let colori = tema::Palette::di(ui);
    tema::riga(ui, "In uso ora", None, |ui| {
        ui.add_space(4.0);
        ui.label(egui::RichText::new(attivo).color(colori.testo));
    });
    tema::divisore(ui);
    tema::riga(
        ui,
        "Backend preferito",
        Some("Vale dal prossimo avvio; --renderer da riga di comando ha la precedenza."),
        |ui| {
            for scelta in PreferenzaRendering::TUTTE {
                if ui.radio(*p == scelta, scelta.etichetta()).clicked() {
                    *p = scelta;
                }
            }
        },
    );
    *p != prima
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(nome: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "lightvm-impostazioni-{nome}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        d.join(FILE)
    }

    #[test]
    fn assente_vale_come_predefinito() {
        let (i, err) = Impostazioni::carica(&tmp("assente"));
        assert_eq!(i.politica_perdita_ui, ParentLossPolicy::KillVms);
        assert!(err.is_none());
    }

    #[test]
    fn salva_e_rilegge() {
        let p = tmp("salva");
        let i = Impostazioni {
            politica_perdita_ui: ParentLossPolicy::ShutdownVms { powerdown_s: 30 },
            rendering: PreferenzaRendering::Wgpu,
            avvio_supervisor: false,
            catalogo: Some(PathBuf::from(r"D:\vm\catalogo")),
            qemu_dir: Some(PathBuf::from(r"D:\qemu")),
            tema: crate::UiTheme::Light,
        };
        i.salva(&p).unwrap();
        assert_eq!(Impostazioni::carica(&p), (i, None));
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn campi_assenti_valgono_come_predefiniti() {
        let p = tmp("parziale");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, r#"{"rendering":"wgpu"}"#).unwrap();
        let (i, err) = Impostazioni::carica(&p);
        assert!(err.is_none());
        assert_eq!(i.rendering, PreferenzaRendering::Wgpu);
        assert_eq!(i.politica_perdita_ui, ParentLossPolicy::KillVms);
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn keep_vms_dal_file_torna_al_predefinito_finche_manca_il_ricollegamento() {
        let p = tmp("keep");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(
            &p,
            r#"{"politica_perdita_ui":{"kind":"keep_vms"},"rendering":"glow"}"#,
        )
        .unwrap();
        let (i, err) = Impostazioni::carica(&p);
        assert_eq!(i.politica_perdita_ui, ParentLossPolicy::KillVms);
        assert_eq!(i.rendering, PreferenzaRendering::Glow);
        assert!(err.is_some());
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn ogni_preferenza_produce_un_piano_valido() {
        for pref in PreferenzaRendering::TUTTE {
            assert!(
                crate::render::Piano::da_argomenti(Some(pref.come_argomento()), false).is_ok(),
                "{pref:?}"
            );
        }
    }

    #[test]
    fn file_non_valido_torna_al_predefinito_con_errore() {
        for (nome, testo) in [
            ("json", "non json"),
            (
                "limite",
                r#"{"politica_perdita_ui":{"kind":"shutdown_vms","powerdown_s":0}}"#,
            ),
        ] {
            let p = tmp(nome);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, testo).unwrap();
            let (i, err) = Impostazioni::carica(&p);
            assert_eq!(i, Impostazioni::default(), "{nome}");
            assert!(err.is_some(), "{nome}");
            let _ = std::fs::remove_dir_all(p.parent().unwrap());
        }
    }
}
