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
pub mod keymap;
pub mod librerie;
pub mod render;
pub mod servizio;

use console::{ConsoleConfig, LiveConsole};
use impostazioni::Impostazioni;
use render::InfoRendering;
use servizio::{Comando, Servizio, StatoServizio};

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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiTheme {
    /// Tema scuro (predefinito).
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

    /// Collega il supervisor avviato dalla UI (F1-11). La Dashboard mostra
    /// allora le VM del catalogo del servizio al posto dell'elenco locale.
    pub fn set_servizio(&mut self, servizio: Servizio) {
        self.servizio = Some(servizio);
        self.errore_servizio = None;
        self.section = Section::Dashboard;
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
        self.impostazioni = impostazioni;
        self.percorso_impostazioni = percorso;
        self.errore_impostazioni = errore;
    }

    /// Impostazioni correnti.
    pub fn impostazioni(&self) -> Impostazioni {
        self.impostazioni
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

    /// Supervisor collegato, se presente.
    pub fn servizio(&self) -> Option<&Servizio> {
        self.servizio.as_ref()
    }

    /// Arresto controllato del supervisor collegato, se presente.
    /// Restituisce il codice di uscita e se è stato necessario forzare.
    pub fn arresta_servizio(&mut self) -> Option<(Option<i32>, bool)> {
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
        if self.applied_theme != Some(self.theme) {
            ctx.set_theme(egui::Theme::from(self.theme));
            self.applied_theme = Some(self.theme);
        }

        egui::Panel::left("lightvm_nav")
            .resizable(false)
            .exact_size(170.0)
            .show(ui, |ui| {
                ui.add_space(8.0);
                ui.heading("LightVM");
                ui.separator();
                for s in Section::ALL {
                    ui.selectable_value(&mut self.section, s, s.label());
                }
            });

        egui::Panel::bottom("lightvm_stato")
            .resizable(false)
            .show(ui, |ui| barra_di_stato_ui(ui, &self.rendering));

        if self.section != Section::Console
            && let Some(live) = &mut self.live
        {
            live.on_hidden();
        }

        egui::CentralPanel::default_margins().show(ui, |ui| match self.section {
            Section::Dashboard => {
                ui.heading("Dashboard");
                if let Some(srv) = &mut self.servizio {
                    let vivo = srv.vivo();
                    let stato = srv.stato();
                    if let Some(c) = servizio_ui(ui, &stato, srv.pid(), vivo) {
                        srv.invia(c);
                    }
                } else {
                    if let Some(e) = &self.errore_servizio {
                        ui.colored_label(ui.visuals().error_fg_color, format!("Supervisor: {e}"));
                        ui.add_space(6.0);
                    }
                    elenco_vm_ui(ui, &self.elenco_vm);
                }
            }
            Section::Console => self.console_ui(ui, &ctx),
            Section::Settings => {
                ui.heading("Settings");
                ui.horizontal(|ui| {
                    ui.label("Tema:");
                    ui.selectable_value(&mut self.theme, UiTheme::Dark, "Scuro");
                    ui.selectable_value(&mut self.theme, UiTheme::Light, "Chiaro");
                });
                ui.checkbox(&mut self.animate, "Aggiorna il framebuffer simulato");
                ui.separator();
                if impostazioni::politica_ui(ui, &mut self.impostazioni.politica_perdita_ui) {
                    self.cambia_politica();
                }
                ui.separator();
                let attivo = self.rendering.etichetta();
                if impostazioni::rendering_ui(ui, &mut self.impostazioni.rendering, &attivo) {
                    self.salva_impostazioni();
                }
                if let Some(e) = &self.errore_impostazioni {
                    ui.colored_label(ui.visuals().warn_fg_color, e);
                }
            }
        });
    }

    fn console_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        if let Some(live) = &mut self.live {
            live.ui(ui, ctx);
            return;
        }
        ui.heading("Console");
        ui.label(format!(
            "Framebuffer simulato {FB_WIDTH}×{FB_HEIGHT}, al massimo 10 aggiornamenti/s. Non è una console reale."
        ));

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

/// Pannello "Macchine virtuali": tabella con nome, sistema e stato, oppure
/// lo stato vuoto dichiarato.
pub fn elenco_vm_ui(ui: &mut egui::Ui, righe: &[VmRiga]) {
    ui.label(egui::RichText::new("Macchine virtuali").strong());
    if righe.is_empty() {
        ui.label("Nessuna VM registrata. Il catalogo e la creazione guidata arrivano in F2.");
        return;
    }
    egui::Grid::new("lightvm_elenco_vm")
        .num_columns(3)
        .striped(true)
        .spacing([24.0, 6.0])
        .show(ui, |ui| {
            ui.label(egui::RichText::new("Nome").strong());
            ui.label(egui::RichText::new("Sistema").strong());
            ui.label(egui::RichText::new("Stato").strong());
            ui.end_row();
            for r in righe {
                ui.label(&r.nome);
                ui.label(&r.os);
                // Stato distinguibile anche senza colore: simbolo e testo.
                let (simbolo, colore) = if r.attiva {
                    ("▶", egui::Color32::from_rgb(60, 180, 90))
                } else {
                    ("■", ui.visuals().weak_text_color())
                };
                ui.horizontal(|ui| {
                    ui.colored_label(colore, simbolo);
                    ui.label(&r.stato);
                });
                ui.end_row();
            }
        });
}

/// Pannello del supervisor (F1-11): stato della connessione, VM del catalogo
/// con avvio e arresto, Emergency Stop. Restituisce l'azione scelta; non
/// esegue nulla da sé, così si può provare senza processi.
pub fn servizio_ui(
    ui: &mut egui::Ui,
    stato: &StatoServizio,
    pid: u32,
    vivo: bool,
) -> Option<Comando> {
    let mut scelta = None;
    ui.label(egui::RichText::new("Supervisor").strong());
    ui.horizontal_wrapped(|ui| {
        let testo = if vivo {
            format!("lightvm-service (PID {pid}): {}", stato.connessione)
        } else {
            format!("lightvm-service (PID {pid}) terminato")
        };
        ui.label(testo);
        if let Some(b) = &stato.backend {
            ui.separator();
            ui.label(format!("backend: {b}"));
        }
        ui.separator();
        ui.label(format!(
            "acceleratore: {}",
            stato.acceleratore.as_deref().unwrap_or("non rilevato")
        ));
    });
    let attivo = vivo && stato.connesso;
    ui.horizontal(|ui| {
        if ui
            .add_enabled(attivo, egui::Button::new("Aggiorna"))
            .clicked()
        {
            scelta = Some(Comando::Aggiorna);
        }
        let stop = egui::Button::new(
            egui::RichText::new("⏹ Emergency Stop").color(ui.visuals().error_fg_color),
        );
        if ui
            .add_enabled(attivo, stop)
            .on_hover_text("Termina subito tutte le VM e revoca le sessioni agente")
            .clicked()
        {
            scelta = Some(Comando::EmergencyStop);
        }
    });
    if let Some(p) = stato.politica {
        ui.label(format!(
            "Alla chiusura anomala della UI: {}",
            impostazioni::descrivi(p)
        ));
    }
    if let Some(e) = &stato.ultimo_esito {
        ui.label(format!("Ultima azione: {e}"));
    }
    ui.add_space(8.0);
    ui.label(egui::RichText::new("Macchine virtuali").strong());
    if stato.vms.is_empty() {
        ui.label("Nessuna VM nel catalogo del supervisor.");
        return scelta;
    }
    egui::Grid::new("lightvm_vm_servizio")
        .num_columns(3)
        .striped(true)
        .spacing([24.0, 6.0])
        .show(ui, |ui| {
            ui.label(egui::RichText::new("Nome").strong());
            ui.label(egui::RichText::new("Stato").strong());
            ui.label(egui::RichText::new("Azioni").strong());
            ui.end_row();
            for v in &stato.vms {
                ui.label(v.name.as_str());
                ui.label(v.state.as_str());
                ui.horizontal(|ui| {
                    let ferma = v.state == lightvm_proto::VmState::Stopped;
                    let attiva = v.state == lightvm_proto::VmState::Running;
                    if ui
                        .add_enabled(attivo && ferma, egui::Button::new("Avvia"))
                        .clicked()
                    {
                        scelta = Some(Comando::Avvia(v.vm_id));
                    }
                    if ui
                        .add_enabled(attivo && attiva, egui::Button::new("Arresta"))
                        .clicked()
                    {
                        scelta = Some(Comando::Arresta(v.vm_id));
                    }
                });
                ui.end_row();
            }
        });
    scelta
}

/// Barra di stato: backend di rendering effettivo e modalità degradata
/// (F1-16). Il testo dice tutto anche senza colore.
pub fn barra_di_stato_ui(ui: &mut egui::Ui, info: &InfoRendering) {
    ui.horizontal_wrapped(|ui| {
        ui.label(info.etichetta()).on_hover_text(info.spiegazione());
        if info.degradato() {
            ui.separator();
            let testo = format!(
                "⚠ Modalità degradata (software): console a {} agg./s",
                render::FPS_CONSOLE_DEGRADATA
            );
            ui.colored_label(ui.visuals().warn_fg_color, testo)
                .on_hover_text(info.spiegazione());
        }
        if !info.falliti.is_empty() {
            ui.separator();
            let nomi: Vec<&str> = info.falliti.iter().map(|(t, _)| t.nome()).collect();
            ui.label(format!("non disponibili: {}", nomi.join(", ")))
                .on_hover_text(info.spiegazione());
        }
    });
}
