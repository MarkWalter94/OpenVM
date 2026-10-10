//! Visual system of the UI: dark and light palettes, type scale, spacing and
//! the few recurring elements (page headers, cards, setting rows, status
//! indicators, notices, buttons).
//!
//! Rules: a single accent colour; colour always accompanies text and is
//! never the only carrier of meaning (ROADMAP F2, accessibility); a visible
//! focus ring for keyboard navigation; no text fields except the VM name in
//! the creation wizard.

use egui::{
    Color32, CornerRadius, FontId, Margin, Response, RichText, Sense, Shadow, Stroke, TextStyle,
    Theme, Ui, Vec2, WidgetInfo, WidgetType,
};

/// Corner radius of buttons and menus.
const WIDGET_RADIUS: u8 = 6;
/// Corner radius of cards.
const CARD_RADIUS: u8 = 10;
/// Width of the label column in setting rows.
pub const LABEL_COLUMN: f32 = 220.0;
/// Maximum width of page content (Dashboard, Settings).
pub const PAGE_WIDTH: f32 = 920.0;

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
    /// Dark theme (default).
    pub const DARK: Self = Self {
        background: Color32::from_rgb(0x13, 0x15, 0x18),
        sidebar_background: Color32::from_rgb(0x0e, 0x10, 0x12),
        surface: Color32::from_rgb(0x1a, 0x1d, 0x21),
        control: Color32::from_rgb(0x24, 0x28, 0x2e),
        control_active: Color32::from_rgb(0x2d, 0x32, 0x39),
        nav_item_active: Color32::from_rgb(0x22, 0x26, 0x2c),
        border: Color32::from_rgb(0x2a, 0x2e, 0x35),
        text: Color32::from_rgb(0xe6, 0xe8, 0xeb),
        text_weak: Color32::from_rgb(0x8d, 0x94, 0x9e),
        accent: Color32::from_rgb(0x5b, 0x8d, 0xef),
        on_accent: Color32::WHITE,
        positive: Color32::from_rgb(0x3f, 0xb9, 0x80),
        warning: Color32::from_rgb(0xe0, 0xa8, 0x48),
        danger: Color32::from_rgb(0xec, 0x5d, 0x55),
    };

    /// Light theme.
    pub const LIGHT: Self = Self {
        background: Color32::from_rgb(0xf6, 0xf7, 0xf9),
        sidebar_background: Color32::from_rgb(0xec, 0xee, 0xf1),
        surface: Color32::WHITE,
        control: Color32::from_rgb(0xee, 0xf0, 0xf3),
        control_active: Color32::from_rgb(0xe2, 0xe6, 0xea),
        nav_item_active: Color32::WHITE,
        border: Color32::from_rgb(0xdd, 0xe1, 0xe6),
        text: Color32::from_rgb(0x1f, 0x23, 0x28),
        text_weak: Color32::from_rgb(0x5f, 0x67, 0x71),
        accent: Color32::from_rgb(0x2f, 0x6f, 0xde),
        on_accent: Color32::WHITE,
        positive: Color32::from_rgb(0x1a, 0x7f, 0x52),
        warning: Color32::from_rgb(0xa1, 0x66, 0x07),
        danger: Color32::from_rgb(0xc9, 0x2a, 0x2a),
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
    v.menu_corner_radius = CornerRadius::same(8);
    let shadow = Color32::from_black_alpha(if theme == Theme::Dark { 90 } else { 28 });
    v.window_shadow = Shadow {
        offset: [0, 6],
        blur: 18,
        spread: 0,
        color: shadow,
    };
    v.popup_shadow = Shadow {
        offset: [0, 4],
        blur: 12,
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
    v.selection.bg_fill = p.accent.linear_multiply(0.35);
    v.selection.stroke = Stroke::new(1.0, p.text);
    v.slider_trailing_fill = true;
    v.striped = false;
    v.indent_has_left_vline = false;

    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.surface;
    w.noninteractive.weak_bg_fill = p.surface;
    w.noninteractive.bg_stroke = border;
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    w.noninteractive.corner_radius = r;

    w.inactive.bg_fill = p.control_active;
    w.inactive.weak_bg_fill = p.control;
    w.inactive.bg_stroke = Stroke::NONE;
    w.inactive.fg_stroke = Stroke::new(1.0, p.text);
    w.inactive.corner_radius = r;

    w.hovered.bg_fill = p.control_active;
    w.hovered.weak_bg_fill = p.control_active;
    w.hovered.bg_stroke = Stroke::new(1.0, p.border);
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
    w.open.bg_stroke = Stroke::new(1.0, p.border);
    w.open.fg_stroke = Stroke::new(1.0, p.text);
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
    // Label text is not selectable: no Ctrl+C towards the host clipboard
    // (ROADMAP: no clipboard features).
    s.interaction.selectable_labels = false;
    s
}

/// Installs the styles of both themes in the context; the active theme is
/// then chosen with [`egui::Context::set_theme`].
pub fn install(ctx: &egui::Context) {
    ctx.set_style_of(Theme::Dark, style(Theme::Dark));
    ctx.set_style_of(Theme::Light, style(Theme::Light));
}

/// Page header: title and, below, a description line. `actions` draws the
/// controls right-aligned on the title row.
pub fn page_header(ui: &mut Ui, title: &str, description: &str, actions: impl FnOnce(&mut Ui)) {
    let p = Palette::of(ui);
    ui.horizontal(|ui| {
        ui.label(RichText::new(title).heading().color(p.text));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), actions);
    });
    ui.label(RichText::new(description).color(p.text_weak));
    ui.add_space(18.0);
}

/// Title of a content group, above a card.
pub fn group_title(ui: &mut Ui, text: &str) {
    let p = Palette::of(ui);
    ui.add_space(6.0);
    ui.label(RichText::new(text).size(15.0).color(p.text));
    ui.add_space(2.0);
}

/// Card: surface with border and rounded corners taking the full available
/// width.
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

/// Separator line inside a card.
pub fn divider(ui: &mut Ui) {
    let p = Palette::of(ui);
    ui.add_space(4.0);
    let (r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(r, 0.0, p.border);
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

/// Coloured dot followed by the status text: the text remains the carrier
/// of the meaning.
pub fn status(ui: &mut Ui, text: &str, tone: Tone) -> Response {
    let p = Palette::of(ui);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 7.0;
        dot(ui, p.tone(tone));
        ui.label(RichText::new(text).color(p.text))
    })
    .inner
}

/// Status dot, centred on the text line.
pub fn dot(ui: &mut Ui, colour: Color32) {
    let h = ui.text_style_height(&TextStyle::Body);
    let (r, _) = ui.allocate_exact_size(Vec2::new(8.0, h), Sense::hover());
    ui.painter().circle_filled(r.center(), 4.0, colour);
}

/// Pill label: small text on a faint background of the tone, as tall as the
/// text whatever the row height.
pub fn pill(ui: &mut Ui, text: &str, tone: Tone) -> Response {
    let p = Palette::of(ui);
    let c = p.tone(tone);
    let (background, colour) = if tone == Tone::Neutral {
        (p.control, p.text_weak)
    } else {
        (c.linear_multiply(0.14), c)
    };
    let galley = egui::WidgetText::from(RichText::new(text).small()).into_galley(
        ui,
        Some(egui::TextWrapMode::Extend),
        f32::INFINITY,
        TextStyle::Small,
    );
    let margin = Vec2::new(9.0, 3.0);
    let (rect, resp) = ui.allocate_exact_size(galley.size() + 2.0 * margin, Sense::hover());
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, text));
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        painter.rect_filled(rect, CornerRadius::same(255), background);
        painter.galley(rect.min + margin, galley, colour);
    }
    resp
}

/// Inline notice: coloured bar on the left and text.
pub fn notice(ui: &mut Ui, text: &str, tone: Tone) -> Response {
    let p = Palette::of(ui);
    let c = p.tone(tone);
    let r = egui::Frame::new()
        .fill(c.linear_multiply(0.10))
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
        CornerRadius {
            nw: WIDGET_RADIUS,
            sw: WIDGET_RADIUS,
            ne: 0,
            se: 0,
        },
        c,
    );
    r.inner
}

/// Primary button (recommended action of the view).
pub fn primary(ui: &Ui, text: &str) -> egui::Button<'static> {
    let p = Palette::of(ui);
    egui::Button::new(RichText::new(text).color(p.on_accent)).fill(p.accent)
}

/// Button for destructive actions: outline and text in the danger colour.
pub fn danger(ui: &Ui, text: &str) -> egui::Button<'static> {
    let p = Palette::of(ui);
    egui::Button::new(RichText::new(text).color(p.danger))
        .fill(Color32::TRANSPARENT)
        .stroke(Stroke::new(1.0, p.danger.linear_multiply(0.7)))
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

/// Side navigation item: full width, left-aligned text, accent bar on the
/// selected item; accent ring when it has the keyboard focus and keyboard
/// navigation is in use (`focus_ring`).
pub fn nav_item(ui: &mut Ui, text: &str, selected: bool, focus_ring: bool) -> Response {
    let p = Palette::of(ui);
    let (rect, resp) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 34.0), Sense::click());
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
        painter.rect_filled(rect, CornerRadius::same(WIDGET_RADIUS), background);
        if focus_ring && resp.has_focus() {
            painter.rect_stroke(
                rect,
                CornerRadius::same(WIDGET_RADIUS),
                Stroke::new(2.0, p.accent),
                egui::StrokeKind::Inside,
            );
        }
        if selected {
            let bar = egui::Rect::from_center_size(
                egui::pos2(rect.min.x + 1.5, rect.center().y),
                Vec2::new(3.0, 16.0),
            );
            painter.rect_filled(bar, CornerRadius::same(2), p.accent);
        }
        let colour = if selected || resp.hovered() {
            p.text
        } else {
            p.text_weak
        };
        painter.text(
            egui::pos2(rect.min.x + 14.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            text,
            FontId::proportional(14.0),
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
