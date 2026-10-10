//! `lightvm-desktop`: interfaccia desktop nativa di LightVM (egui/eframe).
//!
//! Stato: spike di F0, passo 1 (ROADMAP sezione 15). Serve a verificare la build
//! minima reale di eframe, i backend di rendering e il percorso "console come
//! texture" (ROADMAP 6.0 e 6.4). Il framebuffer mostrato è un pattern generato in
//! memoria: non è una console reale e non va usato nel percorso di prodotto
//! (criterio di uscita F2: nessuna schermata con dati simulati).
//!
//! F0-04 aggiunge la console reale (`console`): con `--vnc-socket`/`--qmp-socket`
//! il pannello Console mostra il framebuffer ricevuto dal client RFB in sola
//! lettura e inoltra tastiera e mouse soltanto via QMP.
//!
//! F1-05 (backlog F1-13, F1-14, F1-16, F1-19): blocco del testo degli appunti
//! host (`appunti`), diagnostica delle librerie Linux caricate a runtime
//! (`librerie`), selezione automatica del backend con barra di stato
//! (`render`), elenco VM e istantanee egui_kittest.
//!
//! La UI è separata dal ciclo eframe (`main.rs`) per poterla pilotare headless
//! con `egui_kittest` attraverso l'albero AccessKit.

use std::time::{Duration, Instant};

pub mod appunti;
pub mod console;
pub mod impostazioni;
pub mod impostazioni_vm;
pub mod keymap;
pub mod librerie;
pub mod render;
pub mod servizio;
pub mod tema;

use console::{ConsoleConfig, LiveConsole};
use impostazioni::{AzioneSupervisor, Impostazioni, StatoSupervisorUi};
use render::InfoRendering;
use servizio::{Comando, ConfigServizio, Servizio, StatoServizio};

/// Selettore di cartelle: riceve il titolo, restituisce la cartella scelta.
pub type Selettore = Box<dyn FnMut(&str) -> Option<std::path::PathBuf>>;

/// Selettore nativo (solo Windows; altrove nessun selettore).
pub fn selettore_nativo() -> Option<Selettore> {
    #[cfg(windows)]
    {
        Some(Box::new(|titolo: &str| {
            rfd::FileDialog::new().set_title(titolo).pick_folder()
        }))
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Riga dell'elenco delle VM mostrato nella Dashboard.
///
/// In F1 non esiste ancora un catalogo: nel percorso di prodotto l'elenco è
/// vuoto e la Dashboard lo dichiara. Le righe arrivano dal catalogo in F2; i
/// test possono impostarle con [`LightVmApp::set_elenco_vm`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VmRiga {
    /// Nome della VM.
    pub nome: String,
    /// Sistema operativo dichiarato.
    pub os: String,
    /// Stato leggibile (testo, non solo colore).
    pub stato: String,
    /// La VM è in esecuzione.
    pub attiva: bool,
}

/// Larghezza del framebuffer simulato, in texel (riferimento ROADMAP 13: 1280×720).
pub const FB_WIDTH: usize = 1280;
/// Altezza del framebuffer simulato, in texel.
pub const FB_HEIGHT: usize = 720;
/// Intervallo minimo tra due aggiornamenti del framebuffer: al massimo 10 al secondo.
pub const FB_MIN_INTERVAL: Duration = Duration::from_millis(100);
/// Anticipo tollerato del risveglio oltre a quello introdotto da egui.
const FB_WAKE_TOLERANCE: Duration = Duration::from_millis(3);

/// Voci della navigazione laterale.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    /// Panoramica (in F0 solo segnaposto).
    Dashboard,
    /// Console con il framebuffer simulato.
    Console,
    /// Impostazioni (in F0 solo il tema).
    Settings,
}

impl Section {
    /// Tutte le voci, nell'ordine del pannello laterale.
    pub const ALL: [Self; 3] = [Self::Dashboard, Self::Console, Self::Settings];

    /// Etichetta visibile e accessibile (nome del nodo AccessKit).
    pub fn label(self) -> &'static str {
        match self {
            Self::Dashboard => "Dashboard",
            Self::Console => "Console",
            Self::Settings => "Settings",
        }
    }
}

/// Tema dell'interfaccia: scuro predefinito (ROADMAP F2), chiaro selezionabile.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiTheme {
    /// Tema scuro (predefinito).
    #[default]
    Dark,
    /// Tema chiaro.
    Light,
}

impl From<UiTheme> for egui::Theme {
    fn from(t: UiTheme) -> Self {
        match t {
            UiTheme::Dark => Self::Dark,
            UiTheme::Light => Self::Light,
        }
    }
}

/// Esito di un riavvio in background: `None` se era solo un arresto.
type EsitoRiavvio = Option<Result<(Servizio, ConfigServizio), String>>;

/// Stato della UI dello spike.
pub struct LightVmApp {
    section: Section,
    theme: UiTheme,
    applied_theme: Option<UiTheme>,
    animate: bool,
    texture: Option<egui::TextureHandle>,
    pixels: Vec<egui::Color32>,
    phase: u32,
    last_fb_update: Option<Instant>,
    fb_updates: u64,
    frames: u64,
    live: Option<LiveConsole>,
    elenco_vm: Vec<VmRiga>,
    servizio: Option<Servizio>,
    errore_servizio: Option<String>,
    impostazioni: Impostazioni,
    percorso_impostazioni: Option<std::path::PathBuf>,
    errore_impostazioni: Option<String>,
    eseguibile_servizio: Option<std::path::PathBuf>,
    /// Catalogo e cartella QEMU con cui è stato avviato il supervisor.
    servizio_applicato: Option<(std::path::PathBuf, Option<std::path::PathBuf>)>,
    selettore: Option<Selettore>,
    /// Configurazione del supervisor data da riga di comando (`--catalogo`):
    /// ha la precedenza sulla pagina Settings anche nei riavvii.
    config_cli: Option<ConfigServizio>,
    /// Riavvio o arresto del supervisor in corso fuori dal thread della UI.
    riavvio: Option<std::sync::mpsc::Receiver<EsitoRiavvio>>,
    modifica_vm: Option<impostazioni_vm::ModificaVm>,
    appunti_scartati: u64,
    /// Backend di rendering effettivo, impostato dal ciclo eframe (F1-16).
    pub rendering: InfoRendering,
}

impl Default for LightVmApp {
    fn default() -> Self {
        Self::new(true)
    }
}

impl LightVmApp {
    /// Crea la UI. Con `animate == false` il framebuffer resta statico e non
    /// vengono richiesti repaint periodici (utile per misurare l'idle reale).
    pub fn new(animate: bool) -> Self {
        Self {
            section: Section::Console,
            theme: UiTheme::Dark,
            applied_theme: None,
            animate,
            texture: None,
            pixels: vec![egui::Color32::BLACK; FB_WIDTH * FB_HEIGHT],
            phase: 0,
            last_fb_update: None,
            fb_updates: 0,
            frames: 0,
            live: None,
            elenco_vm: Vec::new(),
            servizio: None,
            errore_servizio: None,
            impostazioni: Impostazioni::default(),
            percorso_impostazioni: None,
            errore_impostazioni: None,
            eseguibile_servizio: None,
            servizio_applicato: None,
            selettore: None,
            config_cli: None,
            riavvio: None,
            modifica_vm: None,
            appunti_scartati: 0,
            rendering: InfoRendering::default(),
        }
    }

    /// Crea la UI con una console già costruita (per esempio
    /// [`LiveConsole::sintetica`] nei test).
    pub fn with_live_console(console: LiveConsole) -> Self {
        let mut app = Self::new(false);
        app.live = Some(console);
        app
    }

    /// Sostituisce l'elenco delle VM mostrato nella Dashboard.
    pub fn set_elenco_vm(&mut self, righe: Vec<VmRiga>) {
        self.elenco_vm = righe;
    }

    /// Collega il supervisor avviato dalla UI (F1-11) con la configurazione
    /// usata. La Dashboard mostra allora le VM del catalogo del servizio.
    pub fn set_servizio(&mut self, servizio: Servizio, cfg: &ConfigServizio) {
        self.servizio = Some(servizio);
        self.servizio_applicato = Some((cfg.catalogo.clone(), cfg.qemu_dir.clone()));
        self.errore_servizio = None;
        self.section = Section::Dashboard;
    }

    /// Eseguibile del supervisor (`--servizio-exe`); senza, quello accanto
    /// all'eseguibile della UI.
    pub fn set_eseguibile_servizio(&mut self, p: Option<std::path::PathBuf>) {
        self.eseguibile_servizio = p;
    }

    /// Selettore di cartelle della pagina Settings (nativo nell'app, finto
    /// nei test).
    pub fn set_selettore(&mut self, s: Option<Selettore>) {
        self.selettore = s;
    }

    /// Configurazione del supervisor data da riga di comando: resta in uso
    /// anche quando il supervisor viene riavviato da Settings.
    pub fn set_config_riga_di_comando(&mut self, cfg: ConfigServizio) {
        self.config_cli = Some(cfg);
    }

    /// Configurazione del supervisor: quella della riga di comando se
    /// presente, altrimenti dalle impostazioni correnti.
    pub fn config_servizio(&self) -> Result<ConfigServizio, String> {
        if let Some(c) = &self.config_cli {
            return Ok(ConfigServizio {
                politica: self.impostazioni.politica_perdita_ui,
                ..c.clone()
            });
        }
        let eseguibile = self
            .eseguibile_servizio
            .clone()
            .or_else(ConfigServizio::eseguibile_accanto)
            .ok_or("eseguibile del supervisor non trovato")?;
        let catalogo = self
            .impostazioni
            .catalogo_effettivo()
            .ok_or("cartella del catalogo non determinabile: sceglierla in Settings")?;
        Ok(ConfigServizio {
            eseguibile,
            catalogo,
            qemu_dir: self.impostazioni.qemu_dir.clone(),
            runtime_dir: None,
            audit_log: None,
            politica: self.impostazioni.politica_perdita_ui,
        })
    }

    /// (Ri)avvia il supervisor con le impostazioni correnti; un supervisor già
    /// attivo è prima arrestato in modo controllato.
    pub fn avvia_servizio(&mut self, ctx: &egui::Context) {
        let _ = self.arresta_servizio();
        let cfg = match self.config_servizio() {
            Ok(c) => c,
            Err(e) => {
                self.errore_servizio = Some(e);
                return;
            }
        };
        let ctx = ctx.clone();
        match Servizio::avvia(&cfg, std::sync::Arc::new(move || ctx.request_repaint())) {
            Ok(s) => {
                let section = self.section;
                self.set_servizio(s, &cfg);
                self.section = section;
            }
            Err(e) => {
                eprintln!("servizio: {e}");
                self.errore_servizio = Some(e.to_string());
            }
        }
    }

    /// Riavvio (o solo arresto) del supervisor da Settings, su un thread a
    /// parte: l'arresto ordinato delle VM può durare decine di secondi e la
    /// finestra deve restare reattiva. L'esito arriva in [`Self::ui`].
    fn riavvia_in_background(&mut self, ctx: &egui::Context, riavvia: bool) {
        if self.riavvio.is_some() {
            return;
        }
        let cfg = if riavvia {
            match self.config_servizio() {
                Ok(c) => Some(c),
                Err(e) => {
                    self.errore_servizio = Some(e);
                    return;
                }
            }
        } else {
            None
        };
        let vecchio = self.servizio.take();
        self.modifica_vm = None;
        self.errore_servizio = None;
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = ctx.clone();
        let avviato = std::thread::Builder::new()
            .name("lightvm-riavvio-servizio".into())
            .spawn(move || {
                if let Some(mut s) = vecchio {
                    let _ = s.arresta(servizio::ATTESA_ARRESTO);
                }
                let esito = cfg.map(|cfg| {
                    let c = ctx.clone();
                    Servizio::avvia(&cfg, std::sync::Arc::new(move || c.request_repaint()))
                        .map(|s| (s, cfg))
                        .map_err(|e| e.to_string())
                });
                let _ = tx.send(esito);
                ctx.request_repaint();
            });
        match avviato {
            Ok(_) => self.riavvio = Some(rx),
            Err(e) => self.errore_servizio = Some(format!("riavvio non avviato: {e}")),
        }
    }

    /// Raccoglie l'esito di un riavvio in background, se arrivato.
    fn raccogli_riavvio(&mut self) {
        let Some(rx) = &self.riavvio else {
            return;
        };
        let esito = match rx.try_recv() {
            Ok(e) => e,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Some(Err("riavvio del supervisor interrotto".into()))
            }
        };
        self.riavvio = None;
        match esito {
            Some(Ok((s, cfg))) => {
                let section = self.section;
                self.set_servizio(s, &cfg);
                self.section = section;
            }
            Some(Err(e)) => {
                eprintln!("servizio: {e}");
                self.errore_servizio = Some(e);
            }
            None => {}
        }
    }

    /// Riavvio o arresto del supervisor in corso.
    pub fn riavvio_in_corso(&self) -> bool {
        self.riavvio.is_some()
    }

    fn stato_supervisor_ui(&mut self) -> StatoSupervisorUi {
        let attuale = match &self.config_cli {
            Some(c) => Some((c.catalogo.clone(), c.qemu_dir.clone())),
            None => self
                .impostazioni
                .catalogo_effettivo()
                .map(|c| (c, self.impostazioni.qemu_dir.clone())),
        };
        let selettore = self.selettore.is_some();
        let riga_di_comando = self
            .config_cli
            .as_ref()
            .map(|c| (c.catalogo.clone(), c.qemu_dir.clone()));
        if self.riavvio.is_some() {
            return StatoSupervisorUi {
                descrizione: "riavvio o arresto in corso…".into(),
                tono: tema::Tono::Attenzione,
                attivo: false,
                in_corso: true,
                vm_accese: 0,
                da_applicare: false,
                selettore,
                riga_di_comando,
            };
        }
        match &mut self.servizio {
            Some(s) => {
                let vivo = s.vivo();
                let st = s.stato();
                let (testo, tono) = stato_connessione(vivo, &st);
                StatoSupervisorUi {
                    descrizione: format!("{testo} (PID {})", s.pid()),
                    tono,
                    attivo: vivo,
                    in_corso: false,
                    vm_accese: st.vms.iter().filter(|v| v.state.has_process()).count(),
                    da_applicare: attuale != self.servizio_applicato,
                    selettore,
                    riga_di_comando,
                }
            }
            None => StatoSupervisorUi {
                descrizione: self
                    .errore_servizio
                    .as_ref()
                    .map_or_else(|| "non avviato".into(), |e| format!("non avviato: {e}")),
                tono: if self.errore_servizio.is_some() {
                    tema::Tono::Pericolo
                } else {
                    tema::Tono::Neutro
                },
                attivo: false,
                in_corso: false,
                vm_accese: 0,
                da_applicare: false,
                selettore,
                riga_di_comando,
            },
        }
    }

    fn esegui_azione_supervisor(&mut self, a: AzioneSupervisor, ctx: &egui::Context) {
        let mut scegli = |titolo: &str| self.selettore.as_mut().and_then(|f| f(titolo));
        match a {
            AzioneSupervisor::ScegliCatalogo => {
                if let Some(p) = scegli("Cartella del catalogo delle VM") {
                    self.impostazioni.catalogo = Some(p);
                    self.salva_impostazioni();
                }
            }
            AzioneSupervisor::ScegliQemu => {
                if let Some(p) = scegli("Cartella della distribuzione QEMU") {
                    self.impostazioni.qemu_dir = Some(p);
                    self.salva_impostazioni();
                }
            }
            AzioneSupervisor::CatalogoPredefinito => {
                self.impostazioni.catalogo = None;
                self.salva_impostazioni();
            }
            AzioneSupervisor::RimuoviQemu => {
                self.impostazioni.qemu_dir = None;
                self.salva_impostazioni();
            }
            AzioneSupervisor::Riavvia => self.riavvia_in_background(ctx, true),
            AzioneSupervisor::Arresta => self.riavvia_in_background(ctx, false),
        }
    }

    /// Registra un avvio del supervisor non riuscito, mostrato nella Dashboard.
    pub fn set_errore_servizio(&mut self, errore: String) {
        self.errore_servizio = Some(errore);
    }

    /// Impostazioni lette all'avvio, file su cui salvarle ed eventuale
    /// problema di lettura da mostrare in Settings.
    pub fn set_impostazioni(
        &mut self,
        impostazioni: Impostazioni,
        percorso: Option<std::path::PathBuf>,
        errore: Option<String>,
    ) {
        self.theme = impostazioni.tema;
        self.impostazioni = impostazioni;
        self.percorso_impostazioni = percorso;
        self.errore_impostazioni = errore;
    }

    /// Impostazioni correnti.
    pub fn impostazioni(&self) -> &Impostazioni {
        &self.impostazioni
    }

    fn salva_impostazioni(&mut self) {
        if let Some(p) = &self.percorso_impostazioni {
            self.errore_impostazioni = self
                .impostazioni
                .salva(p)
                .err()
                .map(|e| format!("impostazioni non salvate: {e}"));
        }
    }

    /// Applica una nuova politica alla perdita della UI: la salva su file e,
    /// se il supervisor è collegato, la invia subito.
    fn cambia_politica(&mut self) {
        self.salva_impostazioni();
        if let Some(s) = &self.servizio {
            s.invia(Comando::ImpostaPolitica(
                self.impostazioni.politica_perdita_ui,
            ));
        }
    }

    /// Apre una VM per nome con le modifiche indicate (riga di comando).
    /// Richiede il supervisor; il risultato appare nella Dashboard.
    pub fn apri_vm(&mut self, nome: String, modifiche: impostazioni_vm::ModificheVm, crea: bool) {
        self.section = Section::Dashboard;
        match &self.servizio {
            Some(s) => {
                s.invia(Comando::ApriVm {
                    nome,
                    modifiche,
                    crea,
                });
            }
            None => {
                // La causa del mancato avvio, se nota, resta visibile.
                let causa = self
                    .errore_servizio
                    .take()
                    .unwrap_or_else(|| "supervisor non avviato".into());
                let e = format!("impossibile aprire la VM \"{nome}\": {causa}");
                eprintln!("servizio: {e}");
                self.errore_servizio = Some(e);
            }
        }
    }

    /// Modifica delle impostazioni di una VM aperta nel pannello, se presente.
    pub fn modifica_vm(&self) -> Option<&impostazioni_vm::ModificaVm> {
        self.modifica_vm.as_ref()
    }

    /// Supervisor collegato, se presente.
    pub fn servizio(&self) -> Option<&Servizio> {
        self.servizio.as_ref()
    }

    /// Arresto controllato del supervisor collegato, se presente.
    /// Restituisce il codice di uscita e se è stato necessario forzare.
    pub fn arresta_servizio(&mut self) -> Option<(Option<i32>, bool)> {
        self.modifica_vm = None;
        let mut s = self.servizio.take()?;
        Some(s.arresta(servizio::ATTESA_ARRESTO))
    }

    /// Imposta la sezione visibile.
    pub fn set_section(&mut self, section: Section) {
        self.section = section;
    }

    /// Eventi `Paste` (appunti host) scartati finora (F1-13).
    pub fn appunti_scartati(&self) -> u64 {
        self.appunti_scartati
    }

    /// Registra gli eventi `Paste` scartati da `raw_input_hook`.
    pub fn conta_appunti_scartati(&mut self, n: u32) {
        self.appunti_scartati += u64::from(n);
    }

    /// Crea la UI con la console reale (RFB in sola lettura + input QMP).
    pub fn with_console(config: ConsoleConfig) -> Self {
        let mut app = Self::new(false);
        app.live = Some(LiveConsole::new(config));
        app
    }

    /// Console reale, se configurata.
    pub fn live_console(&self) -> Option<&LiveConsole> {
        self.live.as_ref()
    }

    /// Voce selezionata.
    pub fn section(&self) -> Section {
        self.section
    }

    /// Tema corrente.
    pub fn theme(&self) -> UiTheme {
        self.theme
    }

    /// Imposta il tema (applicato al frame successivo).
    pub fn set_theme(&mut self, theme: UiTheme) {
        self.theme = theme;
    }

    /// Numero di passaggi UI eseguiti (frame disegnati dal ciclo eframe).
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Numero di aggiornamenti del framebuffer simulato.
    pub fn fb_updates(&self) -> u64 {
        self.fb_updates
    }

    /// Disegna l'intera interfaccia nella `Ui` radice.
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        self.frames += 1;
        let ctx = ui.ctx().clone();
        // F1-13: prima di qualsiasi widget nessun `Paste` resta nell'input.
        let filtro = appunti::filtra_contesto(&ctx);
        self.conta_appunti_scartati(filtro.scartati);
        if self.applied_theme.is_none() {
            tema::installa(&ctx);
        }
        if self.applied_theme != Some(self.theme) {
            ctx.set_theme(egui::Theme::from(self.theme));
            self.applied_theme = Some(self.theme);
            // Questo frame usa ancora lo stile precedente: se ne chiede subito
            // un altro con il nuovo.
            ctx.request_repaint();
        }
        self.raccogli_riavvio();
        let p = tema::Palette::di(ui);
        let (supervisor, tono_supervisor) = self.sintesi_supervisor();

        egui::Panel::left("lightvm_nav")
            .resizable(false)
            .exact_size(208.0)
            .frame(
                egui::Frame::new()
                    .fill(p.fondo_laterale)
                    .inner_margin(egui::Margin {
                        left: 12,
                        right: 12,
                        top: 20,
                        bottom: 14,
                    }),
            )
            .show(ui, |ui| {
                marchio(ui);
                ui.add_space(24.0);
                ui.spacing_mut().item_spacing.y = 2.0;
                for s in Section::ALL {
                    if tema::voce_navigazione(ui, s.label(), self.section == s).clicked() {
                        self.section = s;
                    }
                }
                ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                    ui.horizontal(|ui| {
                        ui.add_space(6.0);
                        ui.spacing_mut().item_spacing.x = 7.0;
                        tema::pallino(ui, p.tono(tono_supervisor));
                        ui.label(
                            egui::RichText::new(supervisor)
                                .small()
                                .color(p.testo_debole),
                        );
                    });
                });
            });

        egui::Panel::bottom("lightvm_stato")
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(p.fondo_laterale)
                    .inner_margin(egui::Margin::symmetric(14, 4)),
            )
            .show(ui, |ui| barra_di_stato_ui(ui, &self.rendering));

        if self.section != Section::Console
            && let Some(live) = &mut self.live
        {
            live.on_hidden();
        }

        let margini = egui::Margin {
            left: 32,
            right: 32,
            top: 26,
            bottom: 16,
        };
        egui::CentralPanel::no_frame()
            .frame(egui::Frame::new().fill(p.fondo).inner_margin(margini))
            .show(ui, |ui| match self.section {
                Section::Dashboard => pagina(ui, "lightvm_pagina_dashboard", |ui| {
                    self.dashboard_ui(ui);
                }),
                Section::Console => self.console_ui(ui, &ctx),
                Section::Settings => pagina(ui, "lightvm_pagina_settings", |ui| {
                    self.settings_ui(ui, &ctx);
                }),
            });
    }

    /// Stato del supervisor in poche parole, per la barra laterale: stesso
    /// testo e tono della Dashboard e di Settings.
    fn sintesi_supervisor(&mut self) -> (String, tema::Tono) {
        if self.riavvio.is_some() {
            return ("Supervisor in riavvio".into(), tema::Tono::Attenzione);
        }
        let Some(s) = &mut self.servizio else {
            let tono = if self.errore_servizio.is_some() {
                tema::Tono::Pericolo
            } else {
                tema::Tono::Neutro
            };
            return ("Supervisor non avviato".into(), tono);
        };
        let vivo = s.vivo();
        let (testo, tono) = stato_connessione(vivo, &s.stato());
        (format!("Supervisor {testo}"), tono)
    }

    fn dashboard_ui(&mut self, ui: &mut egui::Ui) {
        let Some(srv) = &mut self.servizio else {
            tema::intestazione(ui, "Dashboard", DESCRIZIONE_DASHBOARD, |_| {});
            if self.riavvio.is_some() {
                tema::avviso(
                    ui,
                    "Riavvio del supervisor in corso: le VM accese vengono arrestate in modo ordinato.",
                    tema::Tono::Attenzione,
                );
                ui.add_space(14.0);
            }
            if let Some(e) = &self.errore_servizio {
                tema::avviso(ui, &format!("Supervisor: {e}"), tema::Tono::Pericolo);
                ui.add_space(14.0);
            }
            elenco_vm_ui(ui, &self.elenco_vm);
            return;
        };
        let vivo = srv.vivo();
        let stato = srv.stato();
        match servizio_ui(ui, &stato, srv.pid(), vivo) {
            Some(Comando::ApriImpostazioni(id)) => {
                self.modifica_vm = stato.vms.iter().find(|v| v.vm_id == id).map(|v| {
                    impostazioni_vm::ModificaVm::nuova(v.vm_id, v.config_revision, v.config.clone())
                });
            }
            Some(c) => {
                srv.invia(c);
            }
            None => {}
        }
        let Some(m) = &mut self.modifica_vm else {
            return;
        };
        ui.add_space(14.0);
        let attuale = stato.vms.iter().find(|v| v.vm_id == m.vm_id);
        let spenta = vivo
            && stato.connesso
            && attuale.is_some_and(|v| v.state == lightvm_proto::VmState::Stopped);
        let max_vcpu = std::thread::available_parallelism()
            .map_or(1, |n| u32::try_from(n.get()).unwrap_or(u32::MAX));
        match impostazioni_vm::impostazioni_vm_ui(ui, m, spenta, max_vcpu) {
            Some(impostazioni_vm::EsitoModifica::Salva) => {
                srv.invia(Comando::AggiornaVm {
                    vm_id: m.vm_id,
                    revisione: m.revisione,
                    config: Box::new(m.config.clone()),
                });
                self.modifica_vm = None;
            }
            Some(impostazioni_vm::EsitoModifica::Annulla) => self.modifica_vm = None,
            None => {}
        }
    }

    fn settings_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        tema::intestazione(
            ui,
            "Settings",
            "Preferenze di LightVM, salvate nel profilo utente.",
            |_| {},
        );
        if let Some(e) = &self.errore_impostazioni {
            tema::avviso(ui, e, tema::Tono::Attenzione);
            ui.add_space(10.0);
        }

        tema::titolo_gruppo(ui, "Aspetto");
        tema::scheda(ui, |ui| {
            tema::riga(ui, "Tema", None, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    ui.selectable_value(&mut self.theme, UiTheme::Dark, "Scuro");
                    ui.selectable_value(&mut self.theme, UiTheme::Light, "Chiaro");
                });
            });
        });
        if self.theme != self.impostazioni.tema {
            self.impostazioni.tema = self.theme;
            self.salva_impostazioni();
        }

        ui.add_space(14.0);
        tema::titolo_gruppo(ui, "Supervisor");
        let stato = self.stato_supervisor_ui();
        let (azione, cambiato) = tema::scheda(ui, |ui| {
            impostazioni::supervisor_ui(ui, &mut self.impostazioni, &stato)
        });
        if cambiato {
            self.salva_impostazioni();
        }
        if let Some(a) = azione {
            self.esegui_azione_supervisor(a, ctx);
        }

        ui.add_space(14.0);
        tema::titolo_gruppo(ui, "Se LightVM si chiude in modo anomalo");
        if tema::scheda(ui, |ui| {
            impostazioni::politica_ui(ui, &mut self.impostazioni.politica_perdita_ui)
        }) {
            self.cambia_politica();
        }

        ui.add_space(14.0);
        tema::titolo_gruppo(ui, "Rendering");
        let attivo = self.rendering.backend();
        if tema::scheda(ui, |ui| {
            impostazioni::rendering_ui(ui, &mut self.impostazioni.rendering, &attivo)
        }) {
            self.salva_impostazioni();
        }

        ui.add_space(14.0);
        tema::titolo_gruppo(ui, "Diagnostica");
        tema::scheda(ui, |ui| {
            tema::riga(
                ui,
                "Console simulata",
                Some("Usata senza console reale: genera un'immagine di prova. Non salvata."),
                |ui| ui.checkbox(&mut self.animate, "Aggiorna il framebuffer simulato"),
            );
        });
    }

    fn console_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        if let Some(live) = &mut self.live {
            live.ui(ui, ctx);
            return;
        }
        tema::intestazione(
            ui,
            "Console",
            &format!(
                "Framebuffer simulato {FB_WIDTH}×{FB_HEIGHT}, al massimo 10 aggiornamenti/s. Non è una console reale."
            ),
            |_| {},
        );

        let now = Instant::now();
        // Scadenze su una griglia fissa di 100 ms. egui anticipa ogni
        // `request_repaint_after` della durata prevista di un frame
        // (`predicted_dt`, circa 16 ms) e il timer del sistema può aggiungere
        // altro anticipo: senza questa tolleranza ogni risveglio anticipato
        // produrrebbe una raffica di frame vuoti fino alla scadenza.
        let predicted =
            Duration::try_from_secs_f32(ctx.input(|i| i.predicted_dt)).unwrap_or(Duration::ZERO);
        let tolerance = (predicted + FB_WAKE_TOLERANCE).min(FB_MIN_INTERVAL / 2);
        let due = match self.last_fb_update {
            None => true,
            Some(t) => self.animate && now + tolerance >= t + FB_MIN_INTERVAL,
        };
        if due {
            self.render_pattern();
            let image = egui::ColorImage::new([FB_WIDTH, FB_HEIGHT], self.pixels.clone());
            match &mut self.texture {
                Some(tex) => tex.set(image, egui::TextureOptions::LINEAR),
                None => {
                    self.texture =
                        Some(ctx.load_texture("console-fb", image, egui::TextureOptions::LINEAR));
                }
            }
            // Avanza sulla griglia; se il ritardo supera un intervallo, riparte da `now`.
            let next = match self.last_fb_update {
                Some(t) if now < t + 2 * FB_MIN_INTERVAL => t + FB_MIN_INTERVAL,
                _ => now,
            };
            self.last_fb_update = Some(next);
            self.fb_updates += 1;
        }
        if self.animate {
            // Repaint solo al prossimo aggiornamento del framebuffer, nessun polling continuo.
            let target = self.last_fb_update.map_or(now, |t| t + FB_MIN_INTERVAL);
            ctx.request_repaint_after(target.saturating_duration_since(now));
        }

        if let Some(tex) = &self.texture {
            let avail = ui.available_size();
            let aspect = FB_WIDTH as f32 / FB_HEIGHT as f32;
            let mut size = egui::vec2(avail.x, avail.x / aspect);
            if size.y > avail.y {
                size = egui::vec2(avail.y * aspect, avail.y);
            }
            ui.add(egui::Image::new((tex.id(), size)).alt_text("Framebuffer console simulato"));
        }
    }

    /// Genera un gradiente animato con griglia nel buffer riusato (costo lineare nei
    /// pixel). La copia verso `ColorImage` alloca un nuovo buffer per aggiornamento:
    /// accettabile nello spike, da rivedere con il client display reale.
    fn render_pattern(&mut self) {
        self.phase = (self.phase + 8) % (2 * FB_WIDTH as u32);
        let p = self.phase;
        for (y, row) in self
            .pixels
            .as_chunks_mut::<FB_WIDTH>()
            .0
            .iter_mut()
            .enumerate()
        {
            let yy = y as u32;
            for (x, px) in row.iter_mut().enumerate() {
                let xx = x as u32;
                let grid = xx.is_multiple_of(64) || yy.is_multiple_of(64);
                let r = (((xx + p) % FB_WIDTH as u32) * 255 / FB_WIDTH as u32) as u8;
                let g = (((yy + p / 2) % FB_HEIGHT as u32) * 255 / FB_HEIGHT as u32) as u8;
                let b = ((xx ^ yy).wrapping_add(p) & 0xff) as u8;
                *px = if grid {
                    egui::Color32::from_gray(230)
                } else {
                    egui::Color32::from_rgb(r, g, b)
                };
            }
        }
    }
}

impl std::fmt::Debug for LightVmApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LightVmApp")
            .field("section", &self.section)
            .field("theme", &self.theme)
            .field("animate", &self.animate)
            .field("frames", &self.frames)
            .field("fb_updates", &self.fb_updates)
            .field("rendering", &self.rendering)
            .field("appunti_scartati", &self.appunti_scartati)
            .finish_non_exhaustive()
    }
}

/// Descrizione sotto il titolo della Dashboard.
const DESCRIZIONE_DASHBOARD: &str = "Macchine virtuali del catalogo locale.";

/// Contenuto di una pagina: scorrimento verticale e larghezza massima, per
/// righe leggibili anche su schermi larghi.
fn pagina(ui: &mut egui::Ui, id: &str, contenuto: impl FnOnce(&mut egui::Ui)) {
    egui::ScrollArea::vertical()
        .id_salt(id)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            // `set_max_width` può anche allargare: mai oltre lo spazio visibile.
            ui.set_max_width(ui.available_width().min(tema::LARGHEZZA_PAGINA));
            contenuto(ui);
            ui.add_space(12.0);
        });
}

/// Marchio nella barra laterale: un riquadro d'accento con uno schermo
/// stilizzato e il nome del prodotto.
fn marchio(ui: &mut egui::Ui) {
    let p = tema::Palette::di(ui);
    ui.horizontal(|ui| {
        ui.add_space(6.0);
        let (r, _) = ui.allocate_exact_size(egui::vec2(22.0, 22.0), egui::Sense::hover());
        let painter = ui.painter();
        painter.rect_filled(r, egui::CornerRadius::same(6), p.accento);
        let schermo = egui::Rect::from_center_size(r.center(), egui::vec2(12.0, 9.0));
        painter.rect_stroke(
            schermo,
            egui::CornerRadius::same(2),
            egui::Stroke::new(1.5, p.su_accento),
            egui::StrokeKind::Middle,
        );
        ui.add_space(4.0);
        ui.label(egui::RichText::new("LightVM").size(17.0).color(p.testo));
    });
}

/// Stato vuoto di un elenco: messaggio e, sotto, un suggerimento.
fn vuoto(ui: &mut egui::Ui, messaggio: &str, suggerimento: &str) {
    let p = tema::Palette::di(ui);
    ui.vertical_centered(|ui| {
        ui.add_space(14.0);
        ui.label(egui::RichText::new(messaggio).color(p.testo));
        ui.label(
            egui::RichText::new(suggerimento)
                .small()
                .color(p.testo_debole),
        );
        ui.add_space(14.0);
    });
}

/// Testo e tono dello stato del supervisor, uguali in tutte le pagine.
pub fn stato_connessione(vivo: bool, stato: &StatoServizio) -> (&'static str, tema::Tono) {
    if !vivo {
        ("terminato", tema::Tono::Pericolo)
    } else if stato.connesso {
        ("connesso", tema::Tono::Positivo)
    } else if stato.connessione == servizio::CONNESSIONE_IN_CORSO {
        ("connessione in corso", tema::Tono::Attenzione)
    } else {
        ("non connesso", tema::Tono::Pericolo)
    }
}

/// Testo e tono dello stato di una VM.
pub fn stato_vm(s: lightvm_proto::VmState) -> (&'static str, tema::Tono) {
    use lightvm_proto::VmState;
    match s {
        VmState::Stopped => ("Spenta", tema::Tono::Neutro),
        VmState::Starting => ("In avvio", tema::Tono::Attenzione),
        VmState::Running => ("In esecuzione", tema::Tono::Positivo),
        VmState::Stopping => ("In arresto", tema::Tono::Attenzione),
        VmState::Failed => ("Errore", tema::Tono::Pericolo),
        VmState::Recovering => ("In ripristino", tema::Tono::Attenzione),
    }
}

fn quantita_mib(mib: u64) -> String {
    if mib >= 1024 && mib.is_multiple_of(1024) {
        format!("{} GiB", mib / 1024)
    } else {
        format!("{mib} MiB")
    }
}

/// Riassunto della configurazione mostrato sotto il nome della VM.
fn riassunto_vm(c: &lightvm_proto::VmConfig) -> String {
    let os = match c.guest_os {
        lightvm_proto::GuestOs::UbuntuLtsX86_64 => "Ubuntu LTS",
        lightvm_proto::GuestOs::OtherLinuxX86_64 => "Linux",
    };
    format!(
        "{os} · {} vCPU · {} RAM · disco {} GiB",
        c.vcpus,
        quantita_mib(c.memory_mib),
        c.disk_gib
    )
}

/// Riga di una VM: nome e riassunto a sinistra, stato e azioni a destra.
fn riga_vm(
    ui: &mut egui::Ui,
    nome: &str,
    dettaglio: &str,
    stato: (&str, tema::Tono),
    azioni: impl FnOnce(&mut egui::Ui),
) {
    let p = tema::Palette::di(ui);
    ui.horizontal(|ui| {
        ui.set_min_height(44.0);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.label(egui::RichText::new(nome).size(15.0).color(p.testo));
            ui.label(egui::RichText::new(dettaglio).small().color(p.testo_debole));
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            azioni(ui);
            ui.add_space(6.0);
            tema::pastiglia(ui, stato.0, stato.1);
        });
    });
}

/// Elenco "Macchine virtuali" senza supervisor: nome, sistema e stato, oppure
/// lo stato vuoto dichiarato.
pub fn elenco_vm_ui(ui: &mut egui::Ui, righe: &[VmRiga]) {
    tema::titolo_gruppo(ui, "Macchine virtuali");
    tema::scheda(ui, |ui| {
        if righe.is_empty() {
            vuoto(
                ui,
                "Nessuna VM registrata.",
                "Il catalogo e la creazione guidata arrivano in F2.",
            );
            return;
        }
        for (i, r) in righe.iter().enumerate() {
            if i > 0 {
                tema::divisore(ui);
            }
            let tono = if r.attiva {
                tema::Tono::Positivo
            } else {
                tema::Tono::Neutro
            };
            riga_vm(ui, &r.nome, &r.os, (&r.stato, tono), |_| {});
        }
    });
}

/// Dashboard con il supervisor (F1-11): stato della connessione, VM del
/// catalogo con avvio, arresto e impostazioni, Emergency Stop. Restituisce
/// l'azione scelta; non esegue nulla da sé, così si può provare senza processi.
pub fn servizio_ui(
    ui: &mut egui::Ui,
    stato: &StatoServizio,
    pid: u32,
    vivo: bool,
) -> Option<Comando> {
    let p = tema::Palette::di(ui);
    let mut scelta = None;
    let attivo = vivo && stato.connesso;
    tema::intestazione(ui, "Dashboard", DESCRIZIONE_DASHBOARD, |ui| {
        let stop = tema::pericolo(ui, "Emergency Stop");
        if ui
            .add_enabled(attivo, stop)
            .on_hover_text("Termina subito tutte le VM e revoca le sessioni agente")
            .clicked()
        {
            scelta = Some(Comando::EmergencyStop);
        }
        if ui
            .add_enabled(attivo, egui::Button::new("Aggiorna"))
            .clicked()
        {
            scelta = Some(Comando::Aggiorna);
        }
    });

    let (connessione, tono) = stato_connessione(vivo, stato);
    tema::scheda(ui, |ui| {
        let didascalia = |ui: &mut egui::Ui, t: &str| {
            ui.label(egui::RichText::new(t).small().color(p.testo_debole));
        };
        egui::Grid::new("lightvm_sintesi_supervisor")
            .num_columns(4)
            .min_col_width(150.0)
            .spacing([28.0, 4.0])
            .show(ui, |ui| {
                didascalia(ui, &format!("Supervisor · PID {pid}"));
                didascalia(ui, "Backend");
                didascalia(ui, "Accelerazione");
                didascalia(ui, "Se la UI si chiude");
                ui.end_row();
                tema::stato(ui, connessione, tono).on_hover_text(&stato.connessione);
                ui.label(stato.backend.as_deref().unwrap_or("—"));
                ui.label(stato.acceleratore.as_deref().unwrap_or("non rilevata"));
                ui.label(
                    stato
                        .politica
                        .map_or_else(|| "—".into(), impostazioni::descrivi),
                );
                ui.end_row();
            });
    });
    if vivo && !stato.connesso && tono == tema::Tono::Pericolo {
        ui.add_space(8.0);
        tema::avviso(ui, &stato.connessione, tema::Tono::Pericolo);
    }
    if let Some(e) = &stato.ultimo_esito {
        ui.add_space(6.0);
        ui.label(
            egui::RichText::new(format!("Ultima azione: {e}"))
                .small()
                .color(p.testo_debole),
        );
    }

    ui.add_space(18.0);
    tema::titolo_gruppo(ui, "Macchine virtuali");
    tema::scheda(ui, |ui| {
        if stato.vms.is_empty() {
            vuoto(
                ui,
                "Nessuna VM nel catalogo del supervisor.",
                "Per crearne una: lightvm-desktop --apri-vm <nome> --crea",
            );
            return;
        }
        for (i, v) in stato.vms.iter().enumerate() {
            if i > 0 {
                tema::divisore(ui);
            }
            ui.push_id(v.vm_id, |ui| {
                riga_vm(
                    ui,
                    v.name.as_str(),
                    &riassunto_vm(&v.config),
                    stato_vm(v.state),
                    |ui| {
                        use lightvm_proto::VmState;
                        match v.state {
                            VmState::Stopped => {
                                if ui
                                    .add_enabled(attivo, tema::primario(ui, "Avvia"))
                                    .clicked()
                                {
                                    scelta = Some(Comando::Avvia(v.vm_id));
                                }
                            }
                            VmState::Failed => {
                                ui.add_enabled(false, egui::Button::new("Avvia"))
                                    .on_disabled_hover_text(
                                        "VM in errore: il supervisor la riporta a Spenta",
                                    );
                            }
                            s => {
                                if ui
                                    .add_enabled(
                                        attivo && s == VmState::Running,
                                        egui::Button::new("Arresta"),
                                    )
                                    .clicked()
                                {
                                    scelta = Some(Comando::Arresta(v.vm_id));
                                }
                            }
                        }
                        if ui
                            .add_enabled(attivo, tema::discreto("Modifica"))
                            .on_hover_text("Risorse e limiti della VM (modificabili a VM spenta)")
                            .clicked()
                        {
                            scelta = Some(Comando::ApriImpostazioni(v.vm_id));
                        }
                    },
                );
            });
        }
    });
    scelta
}

/// Barra di stato: backend di rendering effettivo e modalità degradata
/// (F1-16). Il testo dice tutto anche senza colore.
pub fn barra_di_stato_ui(ui: &mut egui::Ui, info: &InfoRendering) {
    let p = tema::Palette::di(ui);
    let debole = |t: String| egui::RichText::new(t).small().color(p.testo_debole);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 14.0;
        ui.label(debole(info.etichetta()))
            .on_hover_text(info.spiegazione());
        if info.degradato() {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                tema::pallino(ui, p.attenzione);
                let testo = format!(
                    "Modalità degradata (software): console a {} agg./s",
                    render::FPS_CONSOLE_DEGRADATA
                );
                ui.label(egui::RichText::new(testo).small().color(p.attenzione))
                    .on_hover_text(info.spiegazione());
            });
        }
        if !info.falliti.is_empty() {
            let nomi: Vec<&str> = info.falliti.iter().map(|(t, _)| t.nome()).collect();
            ui.label(debole(format!("non disponibili: {}", nomi.join(", "))))
                .on_hover_text(info.spiegazione());
        }
    });
}
