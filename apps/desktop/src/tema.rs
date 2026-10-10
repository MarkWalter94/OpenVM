//! Sistema visivo della UI: palette scura e chiara, scala tipografica,
//! spaziature e i pochi elementi ricorrenti (intestazioni di pagina, schede,
//! righe di impostazione, indicatori di stato, avvisi, pulsanti).
//!
//! Regole: un solo colore d'accento; il colore accompagna sempre un testo e
//! non è mai l'unico portatore di significato (ROADMAP F2, accessibilità);
//! nessun campo di testo.

use egui::{
    Color32, CornerRadius, FontId, Margin, Response, RichText, Sense, Shadow, Stroke, TextStyle,
    Theme, Ui, Vec2, WidgetInfo, WidgetType,
};

/// Raggio degli angoli di pulsanti e menu.
const RAGGIO_WIDGET: u8 = 6;
/// Raggio degli angoli delle schede.
const RAGGIO_SCHEDA: u8 = 10;
/// Larghezza della colonna delle etichette nelle righe di impostazione.
pub const COLONNA_ETICHETTE: f32 = 220.0;
/// Larghezza massima del contenuto delle pagine (Dashboard, Settings).
pub const LARGHEZZA_PAGINA: f32 = 920.0;

/// Colori di un tema.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    /// Fondo della finestra.
    pub fondo: Color32,
    /// Fondo della barra laterale e della barra di stato.
    pub fondo_laterale: Color32,
    /// Superficie delle schede.
    pub superficie: Color32,
    /// Superficie dei controlli.
    pub controllo: Color32,
    /// Superficie dei controlli al passaggio del mouse.
    pub controllo_attivo: Color32,
    /// Fondo della voce di navigazione selezionata.
    pub voce_attiva: Color32,
    /// Bordi e separatori.
    pub bordo: Color32,
    /// Testo principale.
    pub testo: Color32,
    /// Testo secondario.
    pub testo_debole: Color32,
    /// Colore d'accento.
    pub accento: Color32,
    /// Testo sopra l'accento.
    pub su_accento: Color32,
    /// Esito positivo, VM in esecuzione.
    pub positivo: Color32,
    /// Attenzione, stati transitori.
    pub attenzione: Color32,
    /// Errore, azioni distruttive.
    pub pericolo: Color32,
}

impl Palette {
    /// Tema scuro (predefinito).
    pub const SCURA: Self = Self {
        fondo: Color32::from_rgb(0x13, 0x15, 0x18),
        fondo_laterale: Color32::from_rgb(0x0e, 0x10, 0x12),
        superficie: Color32::from_rgb(0x1a, 0x1d, 0x21),
        controllo: Color32::from_rgb(0x24, 0x28, 0x2e),
        controllo_attivo: Color32::from_rgb(0x2d, 0x32, 0x39),
        voce_attiva: Color32::from_rgb(0x22, 0x26, 0x2c),
        bordo: Color32::from_rgb(0x2a, 0x2e, 0x35),
        testo: Color32::from_rgb(0xe6, 0xe8, 0xeb),
        testo_debole: Color32::from_rgb(0x8d, 0x94, 0x9e),
        accento: Color32::from_rgb(0x5b, 0x8d, 0xef),
        su_accento: Color32::WHITE,
        positivo: Color32::from_rgb(0x3f, 0xb9, 0x80),
        attenzione: Color32::from_rgb(0xe0, 0xa8, 0x48),
        pericolo: Color32::from_rgb(0xec, 0x5d, 0x55),
    };

    /// Tema chiaro.
    pub const CHIARA: Self = Self {
        fondo: Color32::from_rgb(0xf6, 0xf7, 0xf9),
        fondo_laterale: Color32::from_rgb(0xec, 0xee, 0xf1),
        superficie: Color32::WHITE,
        controllo: Color32::from_rgb(0xee, 0xf0, 0xf3),
        controllo_attivo: Color32::from_rgb(0xe2, 0xe6, 0xea),
        voce_attiva: Color32::WHITE,
        bordo: Color32::from_rgb(0xdd, 0xe1, 0xe6),
        testo: Color32::from_rgb(0x1f, 0x23, 0x28),
        testo_debole: Color32::from_rgb(0x5f, 0x67, 0x71),
        accento: Color32::from_rgb(0x2f, 0x6f, 0xde),
        su_accento: Color32::WHITE,
        positivo: Color32::from_rgb(0x1a, 0x7f, 0x52),
        attenzione: Color32::from_rgb(0xa1, 0x66, 0x07),
        pericolo: Color32::from_rgb(0xc9, 0x2a, 0x2a),
    };

    /// Palette del tema in uso in `ui`.
    pub fn di(ui: &Ui) -> Self {
        if ui.visuals().dark_mode {
            Self::SCURA
        } else {
            Self::CHIARA
        }
    }

    /// Colore di un tono.
    pub fn tono(&self, t: Tono) -> Color32 {
        match t {
            Tono::Neutro => self.testo_debole,
            Tono::Accento => self.accento,
            Tono::Positivo => self.positivo,
            Tono::Attenzione => self.attenzione,
            Tono::Pericolo => self.pericolo,
        }
    }
}

/// Significato di un indicatore o di un avviso.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tono {
    /// Nessun significato particolare (fermo, non avviato).
    Neutro,
    /// Informazione.
    Accento,
    /// In esecuzione, connesso.
    Positivo,
    /// Transitorio o da completare.
    Attenzione,
    /// Errore.
    Pericolo,
}

fn stile(tema: Theme) -> egui::Style {
    let (p, mut v) = match tema {
        Theme::Dark => (Palette::SCURA, egui::Visuals::dark()),
        Theme::Light => (Palette::CHIARA, egui::Visuals::light()),
    };
    let r = CornerRadius::same(RAGGIO_WIDGET);
    let bordo = Stroke::new(1.0, p.bordo);

    v.panel_fill = p.fondo;
    v.window_fill = p.superficie;
    v.window_stroke = bordo;
    v.window_corner_radius = CornerRadius::same(RAGGIO_SCHEDA);
    v.menu_corner_radius = CornerRadius::same(8);
    let ombra = Color32::from_black_alpha(if tema == Theme::Dark { 90 } else { 28 });
    v.window_shadow = Shadow {
        offset: [0, 6],
        blur: 18,
        spread: 0,
        color: ombra,
    };
    v.popup_shadow = Shadow {
        offset: [0, 4],
        blur: 12,
        spread: 0,
        color: ombra,
    };
    v.faint_bg_color = p.controllo;
    v.extreme_bg_color = p.fondo_laterale;
    v.code_bg_color = p.controllo;
    v.override_text_color = None;
    v.weak_text_color = Some(p.testo_debole);
    v.hyperlink_color = p.accento;
    v.warn_fg_color = p.attenzione;
    v.error_fg_color = p.pericolo;
    v.selection.bg_fill = p.accento.linear_multiply(0.35);
    v.selection.stroke = Stroke::new(1.0, p.testo);
    v.slider_trailing_fill = true;
    v.striped = false;
    v.indent_has_left_vline = false;

    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.superficie;
    w.noninteractive.weak_bg_fill = p.superficie;
    w.noninteractive.bg_stroke = bordo;
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.testo);
    w.noninteractive.corner_radius = r;

    w.inactive.bg_fill = p.controllo_attivo;
    w.inactive.weak_bg_fill = p.controllo;
    w.inactive.bg_stroke = Stroke::NONE;
    w.inactive.fg_stroke = Stroke::new(1.0, p.testo);
    w.inactive.corner_radius = r;

    w.hovered.bg_fill = p.controllo_attivo;
    w.hovered.weak_bg_fill = p.controllo_attivo;
    w.hovered.bg_stroke = Stroke::new(1.0, p.bordo);
    w.hovered.fg_stroke = Stroke::new(1.5, p.testo);
    w.hovered.corner_radius = r;
    w.hovered.expansion = 0.0;

    w.active.bg_fill = p.controllo_attivo;
    w.active.weak_bg_fill = p.controllo_attivo;
    w.active.bg_stroke = Stroke::new(1.0, p.accento);
    w.active.fg_stroke = Stroke::new(1.5, p.testo);
    w.active.corner_radius = r;
    w.active.expansion = 0.0;

    w.open.bg_fill = p.controllo;
    w.open.weak_bg_fill = p.controllo;
    w.open.bg_stroke = Stroke::new(1.0, p.bordo);
    w.open.fg_stroke = Stroke::new(1.0, p.testo);
    w.open.corner_radius = r;

    let mut s = egui::Style {
        visuals: v,
        ..egui::Style::default()
    };
    s.text_styles = [
        (TextStyle::Small, FontId::proportional(12.0)),
        (TextStyle::Body, FontId::proportional(14.0)),
        (TextStyle::Button, FontId::proportional(14.0)),
        (TextStyle::Monospace, FontId::monospace(13.0)),
        (TextStyle::Heading, FontId::proportional(22.0)),
    ]
    .into();
    let sp = &mut s.spacing;
    sp.item_spacing = Vec2::new(8.0, 8.0);
    sp.button_padding = Vec2::new(12.0, 5.0);
    sp.interact_size = Vec2::new(40.0, 28.0);
    sp.window_margin = Margin::same(14);
    sp.menu_margin = Margin::same(6);
    sp.slider_width = 240.0;
    sp.combo_width = 160.0;
    sp.icon_spacing = 8.0;
    // Nessuna selezione del testo delle etichette: niente Ctrl+C verso gli
    // appunti dell'host (ROADMAP: nessuna funzione degli appunti).
    s.interaction.selectable_labels = false;
    s
}

/// Installa gli stili dei due temi nel contesto; il tema attivo si sceglie
/// poi con [`egui::Context::set_theme`].
pub fn installa(ctx: &egui::Context) {
    ctx.set_style_of(Theme::Dark, stile(Theme::Dark));
    ctx.set_style_of(Theme::Light, stile(Theme::Light));
}

/// Intestazione di pagina: titolo e, sotto, una riga di descrizione. `azioni`
/// disegna i controlli allineati a destra sulla riga del titolo.
pub fn intestazione(ui: &mut Ui, titolo: &str, descrizione: &str, azioni: impl FnOnce(&mut Ui)) {
    let p = Palette::di(ui);
    ui.horizontal(|ui| {
        ui.label(RichText::new(titolo).heading().color(p.testo));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), azioni);
    });
    ui.label(RichText::new(descrizione).color(p.testo_debole));
    ui.add_space(18.0);
}

/// Titolo di un gruppo di contenuti, sopra una scheda.
pub fn titolo_gruppo(ui: &mut Ui, testo: &str) {
    let p = Palette::di(ui);
    ui.add_space(6.0);
    ui.label(RichText::new(testo).size(15.0).color(p.testo));
    ui.add_space(2.0);
}

/// Scheda: superficie con bordo e angoli arrotondati che occupa tutta la
/// larghezza disponibile.
pub fn scheda<R>(ui: &mut Ui, contenuto: impl FnOnce(&mut Ui) -> R) -> R {
    let p = Palette::di(ui);
    egui::Frame::new()
        .fill(p.superficie)
        .stroke(Stroke::new(1.0, p.bordo))
        .corner_radius(CornerRadius::same(RAGGIO_SCHEDA))
        .inner_margin(Margin::symmetric(18, 14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            contenuto(ui)
        })
        .inner
}

/// Linea di separazione interna a una scheda.
pub fn divisore(ui: &mut Ui) {
    let p = Palette::di(ui);
    ui.add_space(4.0);
    let (r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(r, 0.0, p.bordo);
    ui.add_space(4.0);
}

/// Riga di impostazione: etichetta (con descrizione facoltativa) nella colonna
/// di sinistra, controllo a destra.
pub fn riga<R>(
    ui: &mut Ui,
    etichetta: &str,
    descrizione: Option<&str>,
    controllo: impl FnOnce(&mut Ui) -> R,
) -> R {
    let p = Palette::di(ui);
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            Vec2::new(COLONNA_ETICHETTE, 0.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.set_width(COLONNA_ETICHETTE);
                ui.add_space(4.0);
                ui.label(RichText::new(etichetta).color(p.testo));
                if let Some(d) = descrizione {
                    ui.add(egui::Label::new(RichText::new(d).small().color(p.testo_debole)).wrap());
                }
            },
        );
        ui.vertical(controllo).inner
    })
    .inner
}

/// Pallino colorato seguito dal testo dello stato: il testo resta il
/// portatore del significato.
pub fn stato(ui: &mut Ui, testo: &str, tono: Tono) -> Response {
    let p = Palette::di(ui);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 7.0;
        pallino(ui, p.tono(tono));
        ui.label(RichText::new(testo).color(p.testo))
    })
    .inner
}

/// Pallino di stato, centrato sulla riga di testo.
pub fn pallino(ui: &mut Ui, colore: Color32) {
    let h = ui.text_style_height(&TextStyle::Body);
    let (r, _) = ui.allocate_exact_size(Vec2::new(8.0, h), Sense::hover());
    ui.painter().circle_filled(r.center(), 4.0, colore);
}

/// Etichetta a pastiglia: testo piccolo su fondo tenue del tono, alta
/// quanto il testo qualunque sia l'altezza della riga.
pub fn pastiglia(ui: &mut Ui, testo: &str, tono: Tono) -> Response {
    let p = Palette::di(ui);
    let c = p.tono(tono);
    let (fondo, colore) = if tono == Tono::Neutro {
        (p.controllo, p.testo_debole)
    } else {
        (c.linear_multiply(0.14), c)
    };
    let galley = egui::WidgetText::from(RichText::new(testo).small()).into_galley(
        ui,
        Some(egui::TextWrapMode::Extend),
        f32::INFINITY,
        TextStyle::Small,
    );
    let margine = Vec2::new(9.0, 3.0);
    let (rect, resp) = ui.allocate_exact_size(galley.size() + 2.0 * margine, Sense::hover());
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, testo));
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        painter.rect_filled(rect, CornerRadius::same(255), fondo);
        painter.galley(rect.min + margine, galley, colore);
    }
    resp
}

/// Avviso in linea: barra colorata a sinistra e testo.
pub fn avviso(ui: &mut Ui, testo: &str, tono: Tono) -> Response {
    let p = Palette::di(ui);
    let c = p.tono(tono);
    let r = egui::Frame::new()
        .fill(c.linear_multiply(0.10))
        .corner_radius(CornerRadius::same(RAGGIO_WIDGET))
        .inner_margin(Margin {
            left: 14,
            right: 12,
            top: 7,
            bottom: 7,
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(egui::Label::new(RichText::new(testo).color(p.testo)).wrap())
        });
    let rect = r.response.rect;
    ui.painter().rect_filled(
        egui::Rect::from_min_max(rect.min, egui::pos2(rect.min.x + 3.0, rect.max.y)),
        CornerRadius {
            nw: RAGGIO_WIDGET,
            sw: RAGGIO_WIDGET,
            ne: 0,
            se: 0,
        },
        c,
    );
    r.inner
}

/// Pulsante principale (azione raccomandata della vista).
pub fn primario(ui: &Ui, testo: &str) -> egui::Button<'static> {
    let p = Palette::di(ui);
    egui::Button::new(RichText::new(testo).color(p.su_accento)).fill(p.accento)
}

/// Pulsante per azioni distruttive: contorno e testo nel colore di pericolo.
pub fn pericolo(ui: &Ui, testo: &str) -> egui::Button<'static> {
    let p = Palette::di(ui);
    egui::Button::new(RichText::new(testo).color(p.pericolo))
        .fill(Color32::TRANSPARENT)
        .stroke(Stroke::new(1.0, p.pericolo.linear_multiply(0.7)))
}

/// Pulsante senza fondo, per azioni secondarie dentro le righe.
pub fn discreto(testo: &str) -> egui::Button<'static> {
    egui::Button::new(testo).frame_when_inactive(false)
}

/// Assegna a un controllo un nome accessibile diverso dal testo visibile
/// (per esempio due "Sfoglia…" che scelgono cartelle diverse).
pub fn nome_accessibile(r: &Response, tipo: WidgetType, nome: &str) {
    let abilitato = r.enabled();
    r.widget_info(|| WidgetInfo::labeled(tipo, abilitato, nome));
}

/// Voce della navigazione laterale: tutta la larghezza, testo allineato a
/// sinistra, barretta d'accento sulla voce selezionata.
pub fn voce_navigazione(ui: &mut Ui, testo: &str, selezionata: bool) -> Response {
    let p = Palette::di(ui);
    let (rect, resp) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 34.0), Sense::click());
    resp.widget_info(|| WidgetInfo::selected(WidgetType::Button, true, selezionata, testo));
    if ui.is_rect_visible(rect) {
        let fondo = if selezionata {
            p.voce_attiva
        } else if resp.hovered() {
            p.voce_attiva.linear_multiply(0.6)
        } else {
            Color32::TRANSPARENT
        };
        let painter = ui.painter();
        painter.rect_filled(rect, CornerRadius::same(RAGGIO_WIDGET), fondo);
        if selezionata {
            let barra = egui::Rect::from_center_size(
                egui::pos2(rect.min.x + 1.5, rect.center().y),
                Vec2::new(3.0, 16.0),
            );
            painter.rect_filled(barra, CornerRadius::same(2), p.accento);
        }
        let colore = if selezionata || resp.hovered() {
            p.testo
        } else {
            p.testo_debole
        };
        painter.text(
            egui::pos2(rect.min.x + 14.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            testo,
            FontId::proportional(14.0),
            colore,
        );
    }
    resp
}

/// Valore in sola lettura in caratteri a larghezza fissa (percorsi).
pub fn percorso(ui: &mut Ui, testo: &str) -> Response {
    let p = Palette::di(ui);
    ui.add(egui::Label::new(RichText::new(testo).monospace().color(p.testo)).wrap())
}
