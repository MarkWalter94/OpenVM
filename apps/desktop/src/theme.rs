//! Visual system of the UI: dark and light palettes, type scale, spacing and
//! the few recurring elements (page headers, cards, setting rows, status
//! indicators, notices, buttons).
//!
//! Look: minimal and pixel-flavoured, terminal-like. Near-black (or
//! off-white) ground, crisp 1 px borders, square corners, no soft shadows
//! (dialogs and menus cast a hard, offset "pixel" shadow), monospace body
//! text (JetBrains Mono) and a pixel face (Departure Mono) for headings and
//! the small uppercase labels. Both faces are SIL OFL-1.1 and embedded from
//! `assets/fonts/` (ADR-0023); egui's default fonts stay behind them as
//! fallback for glyph coverage.
//!
//! Rules: a single accent colour; colour always accompanies text and is
//! never the only carrier of meaning (ROADMAP F2, accessibility); a visible
//! focus ring for keyboard navigation; no text fields except the VM name in
//! the creation wizard. Text drawn uppercase keeps its original wording as
//! accessible name.

use std::sync::Arc;

use egui::{
    Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Response,
    RichText, Sense, Shadow, Stroke, TextStyle, Theme, Ui, Vec2, WidgetInfo, WidgetType,
};

/// Corner radius of buttons, menus, notices: square, pixel look.
const WIDGET_RADIUS: u8 = 0;
/// Corner radius of cards and dialogs.
const CARD_RADIUS: u8 = 0;
/// Width of the label column in setting rows.
pub const LABEL_COLUMN: f32 = 220.0;
/// Maximum width of page content (Dashboard, Settings).
pub const PAGE_WIDTH: f32 = 920.0;

/// Departure Mono 1.500 (Helena Zhang, SIL OFL-1.1): pixel face for
/// headings and labels, drawn at multiples of 11 px for crisp pixels.
const PIXEL_FONT: &[u8] = include_bytes!("../assets/fonts/DepartureMono-Regular.otf");
/// JetBrains Mono 2.304 Regular (JetBrains, SIL OFL-1.1): body text.
const BODY_FONT: &[u8] = include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf");
const PIXEL_FONT_NAME: &str = "DepartureMono-Regular";
const BODY_FONT_NAME: &str = "JetBrainsMono-Regular";
/// Font family of the pixel face.
const PIXEL_FAMILY: &str = "pixel";
/// Size of the pixel face for small uppercase labels.
const LABEL_SIZE: f32 = 11.0;
/// Size of the pixel face for page titles.
const TITLE_SIZE: f32 = 22.0;

/// Colours of a theme.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    /// Window background.
    pub background: Color32,
    /// Background of the sidebar and of the status bar.
    pub sidebar_background: Color32,
    /// Card surface.
    pub surface: Color32,
    /// Control surface.
    pub control: Color32,
    /// Control surface on mouse hover.
    pub control_active: Color32,
    /// Background of the selected navigation item.
    pub nav_item_active: Color32,
    /// Borders and separators.
    pub border: Color32,
    /// Primary text.
    pub text: Color32,
    /// Secondary text.
    pub text_weak: Color32,
    /// Accent colour.
    pub accent: Color32,
    /// Text on top of the accent.
    pub on_accent: Color32,
    /// Positive outcome, VM running.
    pub positive: Color32,
    /// Warning, transient states.
    pub warning: Color32,
    /// Error, destructive actions.
    pub danger: Color32,
}

impl Palette {
    /// Dark theme (default): near-black ground, one cyan accent.
    pub const DARK: Self = Self {
        background: Color32::from_rgb(0x0c, 0x0c, 0x0c),
        sidebar_background: Color32::from_rgb(0x08, 0x08, 0x08),
        surface: Color32::from_rgb(0x11, 0x11, 0x11),
        control: Color32::from_rgb(0x1a, 0x1a, 0x1a),
        control_active: Color32::from_rgb(0x26, 0x26, 0x26),
        nav_item_active: Color32::from_rgb(0x17, 0x17, 0x17),
        border: Color32::from_rgb(0x2a, 0x2a, 0x2a),
        text: Color32::from_rgb(0xed, 0xed, 0xed),
        text_weak: Color32::from_rgb(0x8a, 0x8a, 0x8a),
        accent: Color32::from_rgb(0x5c, 0xe1, 0xff),
        on_accent: Color32::from_rgb(0x0a, 0x0a, 0x0a),
        positive: Color32::from_rgb(0x4a, 0xde, 0x80),
        warning: Color32::from_rgb(0xff, 0xc5, 0x3d),
        danger: Color32::from_rgb(0xff, 0x5f, 0x56),
    };

    /// Light theme: off-white ground, ink text, deep cyan accent.
    pub const LIGHT: Self = Self {
        background: Color32::from_rgb(0xfa, 0xfa, 0xf9),
        sidebar_background: Color32::from_rgb(0xf1, 0xf1, 0xef),
        surface: Color32::WHITE,
        control: Color32::from_rgb(0xf0, 0xf0, 0xee),
        control_active: Color32::from_rgb(0xe4, 0xe4, 0xe1),
        nav_item_active: Color32::from_rgb(0xe6, 0xe6, 0xe3),
        border: Color32::from_rgb(0xd6, 0xd6, 0xd2),
        text: Color32::from_rgb(0x11, 0x11, 0x11),
        text_weak: Color32::from_rgb(0x5c, 0x5c, 0x5c),
        accent: Color32::from_rgb(0x00, 0x6f, 0x9e),
        on_accent: Color32::WHITE,
        positive: Color32::from_rgb(0x1a, 0x7f, 0x37),
        warning: Color32::from_rgb(0x9a, 0x67, 0x00),
        danger: Color32::from_rgb(0xcf, 0x22, 0x2e),
    };

    /// Palette of the theme in use in `ui`.
    pub fn of(ui: &Ui) -> Self {
        if ui.visuals().dark_mode {
            Self::DARK
        } else {
            Self::LIGHT
        }
    }

    /// Colour of a tone.
    pub fn tone(&self, t: Tone) -> Color32 {
        match t {
            Tone::Neutral => self.text_weak,
            Tone::Accent => self.accent,
            Tone::Positive => self.positive,
            Tone::Warning => self.warning,
            Tone::Danger => self.danger,
        }
    }
}

/// Meaning of an indicator or of a notice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    /// No particular meaning (stopped, not started).
    Neutral,
    /// Information.
    Accent,
    /// Running, connected.
    Positive,
    /// Transient or to be completed.
    Warning,
    /// Error.
    Danger,
}

fn style(theme: Theme) -> egui::Style {
    let (p, mut v) = match theme {
        Theme::Dark => (Palette::DARK, egui::Visuals::dark()),
        Theme::Light => (Palette::LIGHT, egui::Visuals::light()),
    };
    let r = CornerRadius::same(WIDGET_RADIUS);
    let border = Stroke::new(1.0, p.border);

    v.panel_fill = p.background;
    v.window_fill = p.surface;
    v.window_stroke = border;
    v.window_corner_radius = CornerRadius::same(CARD_RADIUS);
    v.menu_corner_radius = r;
    // Hard, offset shadow without blur: a pixel drop shadow.
    let shadow = if theme == Theme::Dark {
        Color32::BLACK
    } else {
        p.border
    };
    v.window_shadow = Shadow {
        offset: [4, 4],
        blur: 0,
        spread: 0,
        color: shadow,
    };
    v.popup_shadow = Shadow {
        offset: [3, 3],
        blur: 0,
        spread: 0,
        color: shadow,
    };
    v.faint_bg_color = p.control;
    v.extreme_bg_color = p.sidebar_background;
    v.code_bg_color = p.control;
    v.override_text_color = None;
    v.weak_text_color = Some(p.text_weak);
    v.hyperlink_color = p.accent;
    v.warn_fg_color = p.warning;
    v.error_fg_color = p.danger;
    v.selection.bg_fill = p.accent.linear_multiply(0.30);
    v.selection.stroke = Stroke::new(1.0, p.text);
    v.slider_trailing_fill = true;
    v.handle_shape = egui::style::HandleShape::Rect { aspect_ratio: 0.6 };
    v.striped = false;
    v.indent_has_left_vline = false;
    v.text_cursor.stroke = Stroke::new(2.0, p.accent);

    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.surface;
    w.noninteractive.weak_bg_fill = p.surface;
    w.noninteractive.bg_stroke = border;
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    w.noninteractive.corner_radius = r;

    // Controls: flat fill and a crisp 1 px outline.
    w.inactive.bg_fill = p.control_active;
    w.inactive.weak_bg_fill = p.control;
    w.inactive.bg_stroke = border;
    w.inactive.fg_stroke = Stroke::new(1.0, p.text);
    w.inactive.corner_radius = r;

    w.hovered.bg_fill = p.control_active;
    w.hovered.weak_bg_fill = p.control_active;
    w.hovered.bg_stroke = Stroke::new(1.0, p.text_weak);
    w.hovered.fg_stroke = Stroke::new(1.5, p.text);
    w.hovered.corner_radius = r;
    w.hovered.expansion = 0.0;

    w.active.bg_fill = p.control_active;
    w.active.weak_bg_fill = p.control_active;
    // Also the keyboard focus style (egui uses `active` for focused
    // widgets): a clearly visible accent ring.
    w.active.bg_stroke = Stroke::new(2.0, p.accent);
    w.active.fg_stroke = Stroke::new(1.5, p.text);
    w.active.corner_radius = r;
    w.active.expansion = 0.0;

    w.open.bg_fill = p.control;
    w.open.weak_bg_fill = p.control;
    w.open.bg_stroke = Stroke::new(1.0, p.text_weak);
    w.open.fg_stroke = Stroke::new(1.0, p.text);
    w.open.corner_radius = r;

    let mut s = egui::Style {
        visuals: v,
        ..egui::Style::default()
    };
    // The body face is monospace in both families; headings use the pixel
    // face through `page_header` (see `pixel_font`).
    s.text_styles = [
        (TextStyle::Small, FontId::proportional(11.5)),
        (TextStyle::Body, FontId::proportional(13.0)),
        (TextStyle::Button, FontId::proportional(13.0)),
        (TextStyle::Monospace, FontId::monospace(13.0)),
        (TextStyle::Heading, FontId::proportional(20.0)),
    ]
    .into();
    let sp = &mut s.spacing;
    sp.item_spacing = Vec2::new(8.0, 8.0);
    sp.button_padding = Vec2::new(12.0, 5.0);
    sp.interact_size = Vec2::new(40.0, 28.0);
    sp.window_margin = Margin::same(16);
    sp.menu_margin = Margin::same(4);
    sp.slider_width = 240.0;
    sp.combo_width = 160.0;
    sp.icon_width = 14.0;
    sp.icon_width_inner = 8.0;
    sp.icon_spacing = 8.0;
    // Label text is not selectable: no Ctrl+C towards the host clipboard
    // (ROADMAP: no clipboard features).
    s.interaction.selectable_labels = false;
    s
}

/// Font definitions: JetBrains Mono first in both families, Departure Mono
/// in its own family; egui's default fonts follow as fallback (accents,
/// symbols, emoji).
fn fonts() -> FontDefinitions {
    let mut f = FontDefinitions::default();
    f.font_data.insert(
        PIXEL_FONT_NAME.to_owned(),
        Arc::new(FontData::from_static(PIXEL_FONT)),
    );
    f.font_data.insert(
        BODY_FONT_NAME.to_owned(),
        Arc::new(FontData::from_static(BODY_FONT)),
    );
    for family in [FontFamily::Proportional, FontFamily::Monospace] {
        f.families
            .entry(family)
            .or_default()
            .insert(0, BODY_FONT_NAME.to_owned());
    }
    let mut pixel = vec![PIXEL_FONT_NAME.to_owned()];
    pixel.extend(f.families[&FontFamily::Proportional].iter().cloned());
    f.families
        .insert(FontFamily::Name(PIXEL_FAMILY.into()), pixel);
    f
}

/// Installs the fonts and the styles of both themes in the context; the
/// active theme is then chosen with [`egui::Context::set_theme`].
pub fn install(ctx: &egui::Context) {
    ctx.set_fonts(fonts());
    ctx.set_style_of(Theme::Dark, style(Theme::Dark));
    ctx.set_style_of(Theme::Light, style(Theme::Light));
}

/// The pixel face at `size`; the body face until the fonts installed by
/// [`install`] are in use (they apply from the next frame).
pub fn pixel_font(ui: &Ui, size: f32) -> FontId {
    let family = FontFamily::Name(PIXEL_FAMILY.into());
    let loaded = ui
        .ctx()
        .fonts(|f| f.definitions().families.contains_key(&family));
    if loaded {
        FontId::new(size, family)
    } else {
        FontId::proportional(size)
    }
}

/// Page header: title and, below, a description line. `actions` draws the
/// controls right-aligned on the title row.
pub fn page_header(ui: &mut Ui, title: &str, description: &str, actions: impl FnOnce(&mut Ui)) {
    let p = Palette::of(ui);
    let font = pixel_font(ui, TITLE_SIZE);
    ui.horizontal(|ui| {
        ui.label(RichText::new(title).font(font).color(p.text));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), actions);
    });
    ui.label(RichText::new(description).color(p.text_weak));
    ui.add_space(20.0);
}

/// Text drawn in the pixel face at label size, uppercase, with `text` as
/// accessible name; `marker` draws a small accent square before it.
fn caps_label(ui: &mut Ui, text: &str, colour: Color32, marker: Option<Color32>) -> Response {
    let font = pixel_font(ui, LABEL_SIZE);
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_uppercase(), font, colour);
    let lead = if marker.is_some() { 12.0 } else { 0.0 };
    let (rect, resp) = ui.allocate_exact_size(
        Vec2::new(lead + galley.size().x, galley.size().y),
        Sense::hover(),
    );
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, text));
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        if let Some(m) = marker {
            let sq = egui::Rect::from_min_size(
                egui::pos2(rect.min.x, (rect.center().y - 3.0).round()),
                Vec2::splat(6.0),
            );
            painter.rect_filled(sq, 0.0, m);
        }
        painter.galley(egui::pos2(rect.min.x + lead, rect.min.y), galley, colour);
    }
    resp
}

/// Title of a content group, above a card: small uppercase pixel label with
/// an accent square.
pub fn group_title(ui: &mut Ui, text: &str) {
    let p = Palette::of(ui);
    ui.add_space(10.0);
    caps_label(ui, text, p.text_weak, Some(p.accent));
    ui.add_space(2.0);
}

/// Card: flat surface with a 1 px border and square corners taking the full
/// available width.
pub fn card<R>(ui: &mut Ui, content: impl FnOnce(&mut Ui) -> R) -> R {
    let p = Palette::of(ui);
    egui::Frame::new()
        .fill(p.surface)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(CARD_RADIUS))
        .inner_margin(Margin::symmetric(18, 14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            content(ui)
        })
        .inner
}

/// Separator line inside a card: dashed, like a terminal rule.
pub fn divider(ui: &mut Ui) {
    let p = Palette::of(ui);
    ui.add_space(4.0);
    let (r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), Sense::hover());
    if ui.is_rect_visible(r) {
        let painter = ui.painter();
        let mut x = r.min.x;
        while x < r.max.x {
            let end = (x + 4.0).min(r.max.x);
            painter.rect_filled(
                egui::Rect::from_min_max(egui::pos2(x, r.min.y), egui::pos2(end, r.max.y)),
                0.0,
                p.border,
            );
            x += 6.0;
        }
    }
    ui.add_space(4.0);
}

/// Setting row: label (with optional description) in the left column,
/// control on the right.
pub fn row<R>(
    ui: &mut Ui,
    label: &str,
    description: Option<&str>,
    control: impl FnOnce(&mut Ui) -> R,
) -> R {
    let p = Palette::of(ui);
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            Vec2::new(LABEL_COLUMN, 0.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.set_width(LABEL_COLUMN);
                ui.add_space(4.0);
                ui.label(RichText::new(label).color(p.text));
                if let Some(d) = description {
                    ui.add(egui::Label::new(RichText::new(d).small().color(p.text_weak)).wrap());
                }
            },
        );
        ui.vertical(control).inner
    })
    .inner
}

/// Coloured square followed by the status text: the text remains the
/// carrier of the meaning.
pub fn status(ui: &mut Ui, text: &str, tone: Tone) -> Response {
    let p = Palette::of(ui);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 7.0;
        dot(ui, p.tone(tone));
        ui.label(RichText::new(text).color(p.text))
    })
    .inner
}

/// Status indicator, centred on the text line: a square "pixel" with a
/// faint halo of the same colour.
pub fn dot(ui: &mut Ui, colour: Color32) {
    let h = ui.text_style_height(&TextStyle::Body);
    let (r, _) = ui.allocate_exact_size(Vec2::new(8.0, h), Sense::hover());
    if ui.is_rect_visible(r) {
        let c = egui::pos2(r.center().x.round(), r.center().y.round());
        let painter = ui.painter();
        painter.rect_filled(
            egui::Rect::from_center_size(c, Vec2::splat(8.0)),
            0.0,
            colour.linear_multiply(0.25),
        );
        painter.rect_filled(
            egui::Rect::from_center_size(c, Vec2::splat(4.0)),
            0.0,
            colour,
        );
    }
}

/// Pill label: small uppercase pixel text in a square tag of the tone, as
/// tall as the text whatever the row height.
pub fn pill(ui: &mut Ui, text: &str, tone: Tone) -> Response {
    let p = Palette::of(ui);
    let c = p.tone(tone);
    let (background, colour, outline) = if tone == Tone::Neutral {
        (p.control, p.text_weak, p.border)
    } else {
        (c.linear_multiply(0.10), c, c.linear_multiply(0.55))
    };
    let galley =
        ui.painter()
            .layout_no_wrap(text.to_uppercase(), pixel_font(ui, LABEL_SIZE), colour);
    let margin = Vec2::new(7.0, 3.0);
    let (rect, resp) = ui.allocate_exact_size(galley.size() + 2.0 * margin, Sense::hover());
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, text));
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        painter.rect(
            rect,
            0.0,
            background,
            Stroke::new(1.0, outline),
            egui::StrokeKind::Inside,
        );
        painter.galley(rect.min + margin, galley, colour);
    }
    resp
}

/// Inline notice: coloured bar on the left, faint tone fill and outline,
/// text.
pub fn notice(ui: &mut Ui, text: &str, tone: Tone) -> Response {
    let p = Palette::of(ui);
    let c = p.tone(tone);
    let r = egui::Frame::new()
        .fill(c.linear_multiply(0.07))
        .stroke(Stroke::new(1.0, c.linear_multiply(0.35)))
        .corner_radius(CornerRadius::same(WIDGET_RADIUS))
        .inner_margin(Margin {
            left: 14,
            right: 12,
            top: 7,
            bottom: 7,
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(egui::Label::new(RichText::new(text).color(p.text)).wrap())
        });
    let rect = r.response.rect;
    ui.painter().rect_filled(
        egui::Rect::from_min_max(rect.min, egui::pos2(rect.min.x + 3.0, rect.max.y)),
        0.0,
        c,
    );
    r.inner
}

/// Primary button (recommended action of the view): solid accent block.
pub fn primary(ui: &Ui, text: &str) -> egui::Button<'static> {
    let p = Palette::of(ui);
    egui::Button::new(RichText::new(text).color(p.on_accent))
        .fill(p.accent)
        .stroke(Stroke::new(1.0, p.accent))
}

/// Button for destructive actions: outline and text in the danger colour.
pub fn danger(ui: &Ui, text: &str) -> egui::Button<'static> {
    let p = Palette::of(ui);
    egui::Button::new(RichText::new(text).color(p.danger))
        .fill(Color32::TRANSPARENT)
        .stroke(Stroke::new(1.0, p.danger.linear_multiply(0.8)))
}

/// Button without background, for secondary actions inside rows.
pub fn subtle(text: &str) -> egui::Button<'static> {
    egui::Button::new(text).frame_when_inactive(false)
}

/// Gives a control an accessible name different from the visible text
/// (for example two "Browse…" buttons that choose different folders).
pub fn accessible_name(r: &Response, kind: WidgetType, name: &str) {
    let enabled = r.enabled();
    r.widget_info(|| WidgetInfo::labeled(kind, enabled, name));
}

/// Side navigation item: full width, left-aligned text, a square accent
/// marker on the selected item; accent ring when it has the keyboard focus
/// and keyboard navigation is in use (`focus_ring`).
pub fn nav_item(ui: &mut Ui, text: &str, selected: bool, focus_ring: bool) -> Response {
    let p = Palette::of(ui);
    let (rect, resp) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 32.0), Sense::click());
    resp.widget_info(|| WidgetInfo::selected(WidgetType::Button, true, selected, text));
    if ui.is_rect_visible(rect) {
        let background = if selected {
            p.nav_item_active
        } else if resp.hovered() {
            p.nav_item_active.linear_multiply(0.6)
        } else {
            Color32::TRANSPARENT
        };
        let painter = ui.painter();
        painter.rect_filled(rect, 0.0, background);
        if selected {
            painter.rect_filled(
                egui::Rect::from_min_size(rect.min, Vec2::new(2.0, rect.height())),
                0.0,
                p.accent,
            );
            painter.rect_filled(
                egui::Rect::from_center_size(
                    egui::pos2(rect.min.x + 14.0, rect.center().y.round()),
                    Vec2::splat(4.0),
                ),
                0.0,
                p.accent,
            );
        }
        if focus_ring && resp.has_focus() {
            painter.rect_stroke(
                rect,
                0.0,
                Stroke::new(2.0, p.accent),
                egui::StrokeKind::Inside,
            );
        }
        let colour = if selected || resp.hovered() {
            p.text
        } else {
            p.text_weak
        };
        painter.text(
            egui::pos2(rect.min.x + 26.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            text,
            FontId::proportional(13.0),
            colour,
        );
    }
    resp
}

/// Read-only value in fixed-width characters (paths).
pub fn path_value(ui: &mut Ui, text: &str) -> Response {
    let p = Palette::of(ui);
    ui.add(egui::Label::new(RichText::new(text).monospace().color(p.text)).wrap())
}
