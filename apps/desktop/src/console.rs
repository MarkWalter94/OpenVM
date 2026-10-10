//! Console of a running VM (ROADMAP 6.4, ADR-0013): framebuffer from the
//! read-only RFB client (`rfb-client`) connected to the supervisor's per-run
//! display socket with the ticket of `open_console`, drawn as a texture; keyboard
//! and mouse go **only** through the supervisor as `SubmitVm` `Input` batches
//! tied to the run and to the current display geometry.
//!
//! The UI opens neither QMP nor QEMU sockets: the display socket path and the
//! token come from the ticket, the input from the authorised IPC path. The
//! console exposes no text field.
//!
//! Key chords handled by LightVM and **not** forwarded to the guest (the
//! modifiers pressed before the chord reach the guest and are released
//! normally):
//! - `Ctrl+Alt+Enter`: full screen on/off ([`FULLSCREEN_CHORD`]);
//! - `Ctrl+Alt+Esc`: leave the console: exits full screen and releases the
//!   keyboard focus ([`LEAVE_CHORD`]).

use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use lightvm_proto::ipc::ConsoleTicket;
use lightvm_proto::{
    DisplayGeometry, InputBatch, InputEvent, KeyAction, MAX_INPUT_EVENTS_PER_REQUEST, MouseButton,
    PointerButton, PointerMove, QCode, RunId, VmId,
};
use rfb_client::{Framebuffer, Rect, Session, SessionConfig, SessionEvent, Transport, UnixImpl};

use crate::keymap::{self, KeyboardLayout};
use crate::theme::{self, Tone};

/// Description of the full screen chord, shown in the UI.
pub const FULLSCREEN_CHORD: &str = "Ctrl+Alt+Enter";
/// Description of the leave chord, shown in the UI.
pub const LEAVE_CHORD: &str = "Ctrl+Alt+Esc";

/// Maximum console updates per second in normal mode.
pub const DEFAULT_MAX_FPS: u32 = 30;

/// VM and run a console belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsoleTarget {
    /// VM.
    pub vm_id: VmId,
    /// VM name, for titles and accessible names.
    pub vm_name: String,
    /// Run the display ticket is valid for.
    pub run_id: RunId,
}

/// LightVM chords recognised in the input of a frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Chords {
    /// `Ctrl+Alt+Enter`.
    pub toggle_fullscreen: bool,
    /// `Ctrl+Alt+Esc`.
    pub leave: bool,
}

fn is_chord(m: egui::Modifiers) -> bool {
    m.ctrl && m.alt && !m.shift
}

/// Removes the LightVM chords from the events (press and release of the
/// final key), so that neither the guest nor egui sees them.
pub fn take_chords(events: &mut Vec<egui::Event>, swallow: &mut Vec<egui::Key>) -> Chords {
    let mut c = Chords::default();
    events.retain(|e| {
        let egui::Event::Key {
            key,
            physical_key,
            pressed,
            modifiers,
            ..
        } = e
        else {
            return true;
        };
        let k = physical_key.unwrap_or(*key);
        if !matches!(k, egui::Key::Enter | egui::Key::Escape) {
            return true;
        }
        if *pressed && is_chord(*modifiers) {
            if k == egui::Key::Enter {
                c.toggle_fullscreen = true;
            } else {
                c.leave = true;
            }
            if !swallow.contains(&k) {
                swallow.push(k);
            }
            return false;
        }
        if !*pressed && let Some(i) = swallow.iter().position(|s| *s == k) {
            swallow.remove(i);
            return false;
        }
        // Autorepeat of a swallowed key.
        !(*pressed && swallow.contains(&k))
    });
    c
}

/// Largest rectangle with the guest aspect ratio inside `avail`, centred.
pub fn fit(avail: egui::Rect, geom: (u32, u32)) -> egui::Rect {
    let (gw, gh) = (geom.0.max(1) as f32, geom.1.max(1) as f32);
    let aspect = gw / gh;
    let mut size = egui::vec2(avail.width(), avail.width() / aspect);
    if size.y > avail.height() {
        size = egui::vec2(avail.height() * aspect, avail.height());
    }
    egui::Rect::from_center_size(avail.center(), size.max(egui::Vec2::ZERO))
}

/// Guest pixel under a screen position, `None` outside the image.
pub fn to_framebuffer(rect: egui::Rect, geom: (u32, u32), p: egui::Pos2) -> Option<(u32, u32)> {
    if !rect.contains(p) || geom.0 == 0 || geom.1 == 0 || rect.width() <= 0.0 {
        return None;
    }
    let x = ((p.x - rect.min.x) / rect.width() * geom.0 as f32).floor() as u32;
    let y = ((p.y - rect.min.y) / rect.height() * geom.1 as f32).floor() as u32;
    Some((x.min(geom.0 - 1), y.min(geom.1 - 1)))
}

/// State of the forwarded keys and buttons, for releasing them when focus is lost.
#[derive(Debug, Default)]
pub struct InputState {
    pressed_keys: Vec<&'static str>,
    pressed_buttons: Vec<MouseButton>,
    last_abs: Option<PointerMove>,
    /// Pointer over the console image: only then does the wheel go to the
    /// guest.
    pointer_inside: bool,
}

fn egui_button(b: egui::PointerButton) -> Option<MouseButton> {
    Some(match b {
        egui::PointerButton::Primary => MouseButton::Left,
        egui::PointerButton::Secondary => MouseButton::Right,
        egui::PointerButton::Middle => MouseButton::Middle,
        _ => return None,
    })
}

fn key(q: &'static str, down: bool) -> Option<InputEvent> {
    QCode::parse(q)
        .ok()
        .map(|qcode| InputEvent::Key(KeyAction { qcode, down }))
}

fn btn(button: MouseButton, down: bool) -> InputEvent {
    InputEvent::PointerButton(PointerButton { button, down })
}

impl InputState {
    /// Keys currently pressed towards the guest.
    pub fn pressed_keys(&self) -> &[&'static str] {
        &self.pressed_keys
    }

    /// Translates the egui events of a frame into guest input events.
    /// - `keyboard`: the console has keyboard focus;
    /// - `to_fb`: converts a screen position into guest pixels (`None`
    ///   outside the image);
    /// - `geom`: current guest geometry.
    pub fn translate(
        &mut self,
        events: &[egui::Event],
        keyboard: bool,
        to_fb: &dyn Fn(egui::Pos2) -> Option<(u32, u32)>,
        geom: DisplayGeometry,
    ) -> Vec<InputEvent> {
        let mut out = Vec::new();
        for ev in events {
            match ev {
                egui::Event::Key {
                    physical_key,
                    pressed,
                    repeat,
                    ..
                } if keyboard => {
                    let Some(q) = physical_key.and_then(keymap::qcode) else {
                        continue;
                    };
                    if *pressed {
                        // Autorepeat is produced by the guest while the key stays pressed.
                        if *repeat || self.pressed_keys.contains(&q) {
                            continue;
                        }
                        self.pressed_keys.push(q);
                        out.extend(key(q, true));
                    } else if let Some(i) = self.pressed_keys.iter().position(|k| *k == q) {
                        self.pressed_keys.remove(i);
                        out.extend(key(q, false));
                    }
                }
                // egui-winit turns Ctrl+C/X/V into dedicated events without
                // `Key`: `crate::clipboard` replaces them earlier with the
                // physical keys; this branch remains as a second defence. The
                // content of `Paste` (host clipboard) is discarded.
                egui::Event::Copy if keyboard => tap(&mut out, "c"),
                egui::Event::Cut if keyboard => tap(&mut out, "x"),
                egui::Event::Paste(_) if keyboard => tap(&mut out, "v"),
                egui::Event::PointerMoved(pos) => {
                    let fb = to_fb(*pos);
                    self.pointer_inside = fb.is_some();
                    if let Some((x, y)) = fb {
                        self.push_abs(&mut out, x, y, geom);
                    }
                }
                egui::Event::PointerGone => self.pointer_inside = false,
                egui::Event::PointerButton {
                    pos,
                    button,
                    pressed,
                    ..
                } => {
                    let Some(b) = egui_button(*button) else {
                        continue;
                    };
                    if *pressed {
                        if let Some((x, y)) = to_fb(*pos) {
                            self.push_abs(&mut out, x, y, geom);
                            if !self.pressed_buttons.contains(&b) {
                                self.pressed_buttons.push(b);
                                out.push(btn(b, true));
                            }
                        }
                    } else if let Some(i) = self.pressed_buttons.iter().position(|p| *p == b) {
                        self.pressed_buttons.remove(i);
                        out.push(btn(b, false));
                    }
                }
                egui::Event::MouseWheel { delta, .. } if self.pointer_inside && delta.y != 0.0 => {
                    let b = if delta.y > 0.0 {
                        MouseButton::WheelUp
                    } else {
                        MouseButton::WheelDown
                    };
                    let n = (delta.y.abs().round() as u32).clamp(1, 5);
                    for _ in 0..n {
                        out.push(btn(b, true));
                        out.push(btn(b, false));
                    }
                }
                _ => {}
            }
        }
        out
    }

    fn push_abs(&mut self, out: &mut Vec<InputEvent>, x: u32, y: u32, geom: DisplayGeometry) {
        let Ok(a) = PointerMove::from_pixels(x, y, geom) else {
            return;
        };
        if self.last_abs != Some(a) {
            self.last_abs = Some(a);
            out.push(InputEvent::PointerMove(a));
        }
    }

    /// Releases all pressed keys and buttons (focus loss, disconnection,
    /// close).
    pub fn release_all(&mut self) -> Vec<InputEvent> {
        let mut out: Vec<InputEvent> = self
            .pressed_keys
            .drain(..)
            .rev()
            .filter_map(|q| key(q, false))
            .collect();
        out.extend(self.pressed_buttons.drain(..).map(|b| btn(b, false)));
        self.last_abs = None;
        out
    }
}

fn tap(out: &mut Vec<InputEvent>, q: &'static str) {
    out.extend(key(q, true));
    out.extend(key(q, false));
}

/// State of the display connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Link {
    /// Connecting with the ticket.
    Connecting,
    /// Connected; negotiated security.
    Connected(String),
    /// Session ended, with the reason if it was not closed locally.
    Disconnected(Option<String>),
}

impl Link {
    /// Readable text and tone.
    pub fn describe(&self) -> (String, Tone) {
        match self {
            Self::Connecting => ("Connecting to the display…".into(), Tone::Warning),
            Self::Connected(_) => ("Connected".into(), Tone::Positive),
            Self::Disconnected(Some(e)) => (format!("Disconnected: {e}"), Tone::Danger),
            Self::Disconnected(None) => ("Disconnected".into(), Tone::Danger),
        }
    }
}

/// Console counters for the smoke test.
#[derive(Clone, Debug, Default)]
pub struct ConsoleStats {
    /// RFB updates received by the UI.
    pub rfb_updates: u64,
    /// `set_partial`/`set` calls on the texture.
    pub texture_uploads: u64,
    /// Pixels uploaded to the texture.
    pub uploaded_pixels: u64,
    /// Input events queued towards the supervisor.
    pub input_events: u64,
    /// Resizes.
    pub resizes: u64,
}

/// Request of the console page to the app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsoleAction {
    /// Full screen on or off.
    ToggleFullscreen,
    /// Leave full screen (if on).
    ExitFullscreen,
    /// Save a PNG of the current framebuffer.
    Screenshot,
    /// Declared guest keyboard layout changed.
    SetLayout(KeyboardLayout),
}

/// Console connected to the supervisor's display socket.
pub struct LiveConsole {
    target: ConsoleTarget,
    transport: Option<Transport>,
    max_fps: u32,
    session: Option<Session>,
    events: Option<Receiver<SessionEvent>>,
    fb: Option<Arc<Mutex<Framebuffer>>>,
    texture: Option<egui::TextureHandle>,
    geometry: (u16, u16),
    link: Link,
    input: InputState,
    had_focus: bool,
    outgoing: Vec<InputBatch>,
    /// Frame to upload (tests only: no connection).
    synthetic: Option<egui::ColorImage>,
    /// Copy of the synthetic frame, for screenshots in tests.
    synthetic_copy: Option<egui::ColorImage>,
    /// Rectangle of the image in the last frame.
    image_rect: Option<egui::Rect>,
    disconnected_at: Option<Instant>,
    /// Counters.
    pub stats: ConsoleStats,
}

impl std::fmt::Debug for LiveConsole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // No transport: it contains the display ticket.
        f.debug_struct("LiveConsole")
            .field("target", &self.target)
            .field("geometry", &self.geometry)
            .field("link", &self.link)
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

impl LiveConsole {
    fn empty(target: ConsoleTarget, max_fps: u32) -> Self {
        Self {
            target,
            transport: None,
            max_fps: max_fps.max(1),
            session: None,
            events: None,
            fb: None,
            texture: None,
            geometry: (0, 0),
            link: Link::Connecting,
            input: InputState::default(),
            had_focus: false,
            outgoing: Vec::new(),
            synthetic: None,
            synthetic_copy: None,
            image_rect: None,
            disconnected_at: None,
            stats: ConsoleStats::default(),
        }
    }

    /// Console for a ticket of `open_console`. The connection starts at the
    /// first frame; the token stays only in the transport.
    pub fn from_ticket(name: &str, ticket: ConsoleTicket, max_fps: u32) -> Self {
        let mut c = Self::empty(
            ConsoleTarget {
                vm_id: ticket.vm_id,
                vm_name: name.to_owned(),
                run_id: ticket.run_id,
            },
            max_fps,
        );
        if let Some(g) = ticket.geometry {
            c.geometry = (
                u16::try_from(g.width).unwrap_or(u16::MAX),
                u16::try_from(g.height).unwrap_or(u16::MAX),
            );
        }
        c.transport = Some(Transport::Ticket {
            path: ticket.socket_path.into(),
            imp: UnixImpl::Socket2,
            token: ticket.token,
        });
        c
    }

    /// Console without a connection with a fixed frame, for headless tests
    /// (snapshots, key forwarding, coordinates). The input batches are
    /// queued as in the real console. Not reachable from the command line.
    #[doc(hidden)]
    pub fn synthetic(target: ConsoleTarget, frame: egui::ColorImage) -> Self {
        let mut c = Self::empty(target, DEFAULT_MAX_FPS);
        c.link = Link::Connected("test frame, no display".into());
        c.geometry = (
            u16::try_from(frame.size[0]).unwrap_or(u16::MAX),
            u16::try_from(frame.size[1]).unwrap_or(u16::MAX),
        );
        c.synthetic_copy = Some(frame.clone());
        c.synthetic = Some(frame);
        c
    }

    /// VM and run of the console.
    pub fn target(&self) -> &ConsoleTarget {
        &self.target
    }

    /// Current guest geometry.
    pub fn geometry(&self) -> (u16, u16) {
        self.geometry
    }

    /// State of the display connection.
    pub fn link(&self) -> &Link {
        &self.link
    }

    /// Instant the display session ended, if it did.
    pub fn disconnected_at(&self) -> Option<Instant> {
        self.disconnected_at
    }

    /// Rectangle of the image drawn in the last frame.
    pub fn image_rect(&self) -> Option<egui::Rect> {
        self.image_rect
    }

    /// The console image had keyboard focus in the last frame.
    pub fn has_focus(&self) -> bool {
        self.had_focus
    }

    /// Input batches to send to the supervisor (drained).
    pub fn take_input(&mut self) -> Vec<InputBatch> {
        std::mem::take(&mut self.outgoing)
    }

    fn display_geometry(&self) -> Option<DisplayGeometry> {
        DisplayGeometry::new(u32::from(self.geometry.0), u32::from(self.geometry.1)).ok()
    }

    fn queue(&mut self, events: Vec<InputEvent>) {
        if events.is_empty() {
            return;
        }
        let Some(geometry) = self.display_geometry() else {
            return;
        };
        self.stats.input_events += events.len() as u64;
        for chunk in events.chunks(MAX_INPUT_EVENTS_PER_REQUEST) {
            self.outgoing.push(InputBatch {
                vm_id: self.target.vm_id,
                run_id: self.target.run_id,
                geometry,
                events: chunk.to_vec(),
            });
        }
    }

    /// Releases all keys pressed towards the guest.
    pub fn release_all(&mut self) {
        let ev = self.input.release_all();
        self.queue(ev);
    }

    /// The page is not visible: releases the keys if it had focus.
    pub fn on_hidden(&mut self) {
        if self.had_focus {
            self.release_all();
            self.had_focus = false;
        }
    }

    /// Closes the display session; returns the last batches (key releases)
    /// to send.
    pub fn close(&mut self) -> Vec<InputBatch> {
        self.release_all();
        if let Some(mut s) = self.session.take() {
            s.stop();
        }
        self.take_input()
    }

    /// PNG of the current framebuffer.
    pub fn screenshot_png(&self) -> Option<Result<Vec<u8>, String>> {
        let img = if let Some(f) = &self.synthetic_copy {
            let mut data = Vec::with_capacity(f.pixels.len() * 3);
            for p in &f.pixels {
                data.extend_from_slice(&[p.r(), p.g(), p.b()]);
            }
            f0_console::image::RgbImage {
                width: u32::try_from(f.size[0]).ok()?,
                height: u32::try_from(f.size[1]).ok()?,
                data,
            }
        } else {
            let fb = self.fb.as_ref()?.lock().ok()?;
            if fb.width() == 0 || fb.height() == 0 {
                return None;
            }
            f0_console::image::RgbImage {
                width: u32::from(fb.width()),
                height: u32::from(fb.height()),
                data: fb.to_rgb(),
            }
        };
        Some(f0_console::image::encode_png(&img))
    }

    fn start(&mut self, ctx: &egui::Context) {
        let Some(transport) = self.transport.clone() else {
            return;
        };
        let (tx, rx) = channel();
        let ctx2 = ctx.clone();
        let session = Session::spawn(
            SessionConfig {
                transport,
                max_updates_per_second: self.max_fps,
            },
            move |ev| {
                let _ = tx.send(ev);
                ctx2.request_repaint();
            },
        );
        self.fb = Some(session.framebuffer());
        self.session = Some(session);
        self.events = Some(rx);
        self.link = Link::Connecting;
    }

    fn pump_updates(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.events else { return };
        let mut dirty: Vec<Rect> = Vec::new();
        let mut full = false;
        let mut ended = false;
        while let Ok(ev) = rx.try_recv() {
            match ev {
                SessionEvent::Connected { init, security, .. } => {
                    self.geometry = (init.width, init.height);
                    full = true;
                    self.link = Link::Connected(security);
                }
                SessionEvent::Update(u) => {
                    self.stats.rfb_updates += 1;
                    if u.resized || (u.width, u.height) != self.geometry {
                        self.stats.resizes += u64::from(u.resized);
                        self.geometry = (u.width, u.height);
                        full = true;
                    }
                    dirty.extend(u.rects);
                }
                SessionEvent::Disconnected { error } => {
                    self.link = Link::Disconnected(error);
                    ended = true;
                }
            }
        }
        if ended {
            self.disconnected_at = Some(Instant::now());
            self.release_all();
        }
        if !full && dirty.is_empty() {
            return;
        }
        let Some(fb) = &self.fb else { return };
        let Ok(fb) = fb.lock() else { return };
        if fb.width() == 0 || fb.height() == 0 {
            return;
        }
        let size = [usize::from(fb.width()), usize::from(fb.height())];
        let need_full = full || self.texture.as_ref().is_none_or(|t| t.size() != size);
        if need_full {
            let img =
                egui::ColorImage::from_rgba_unmultiplied(size, &fb.region_rgba(fb.full_rect()));
            match &mut self.texture {
                Some(t) => t.set(img, egui::TextureOptions::LINEAR),
                None => {
                    self.texture =
                        Some(ctx.load_texture("console-rfb", img, egui::TextureOptions::LINEAR));
                }
            }
            self.stats.texture_uploads += 1;
            self.stats.uploaded_pixels += (size[0] * size[1]) as u64;
            return;
        }
        // Many small rectangles: a single upload of the rectangle containing them.
        if dirty.len() > 32 {
            let u = dirty.iter().skip(1).fold(dirty[0], |a, r| a.union(r));
            dirty = vec![u];
        }
        if let Some(tex) = &mut self.texture {
            for r in dirty {
                let r = fb.clip(r);
                if r.w == 0 || r.h == 0 {
                    continue;
                }
                let img = egui::ColorImage::from_rgba_unmultiplied(
                    [usize::from(r.w), usize::from(r.h)],
                    &fb.region_rgba(r),
                );
                tex.set_partial(
                    [usize::from(r.x), usize::from(r.y)],
                    img,
                    egui::TextureOptions::LINEAR,
                );
                self.stats.texture_uploads += 1;
                self.stats.uploaded_pixels += r.area();
            }
        }
    }

    /// Draws the console: toolbar (state, geometry, layout, screenshot, full
    /// screen) and the framebuffer. `chords` are the LightVM chords already
    /// removed from the input of this frame by the app.
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        layout: KeyboardLayout,
        fullscreen: bool,
        chords: Chords,
        input_error: Option<&str>,
    ) -> Option<ConsoleAction> {
        let ctx = ui.ctx().clone();
        if let Some(frame) = self.synthetic.take() {
            self.texture =
                Some(ctx.load_texture("console-synthetic", frame, egui::TextureOptions::LINEAR));
            self.stats.texture_uploads += 1;
        } else if self.session.is_none() && self.transport.is_some() && self.events.is_none() {
            self.start(&ctx);
        }
        self.pump_updates(&ctx);

        let mut action = None;
        if chords.toggle_fullscreen {
            action = Some(ConsoleAction::ToggleFullscreen);
        }
        self.toolbar(ui, layout, fullscreen, &mut action);
        if let Some(e) = input_error {
            theme::notice(ui, e, Tone::Warning);
            ui.add_space(4.0);
        }
        ui.add_space(if fullscreen { 2.0 } else { 6.0 });

        let p = theme::Palette::of(ui);
        let avail = ui.available_rect_before_wrap();
        let Some(tex) = &self.texture else {
            self.image_rect = None;
            let (text, _) = self.link.describe();
            ui.vertical_centered(|ui| {
                ui.add_space(24.0);
                ui.label(egui::RichText::new("Waiting for the first screen update…").color(p.text));
                ui.label(egui::RichText::new(text).small().color(p.text_weak));
            });
            return action;
        };
        let geom = (u32::from(self.geometry.0), u32::from(self.geometry.1));
        let rect = fit(avail, geom);
        // The whole area is allocated so the layout stays stable; the image
        // is centred inside it.
        ui.allocate_rect(avail, egui::Sense::hover());
        let name = format!("Console of {}", self.target.vm_name);
        let resp = ui.put(
            rect,
            egui::Image::new((tex.id(), rect.size()))
                .sense(egui::Sense::click_and_drag())
                .alt_text(&name),
        );
        if resp.clicked() || resp.drag_started() {
            resp.request_focus();
        }
        // With focus, Tab, arrows and Esc go to the guest and not to egui navigation.
        ui.memory_mut(|m| {
            m.set_focus_lock_filter(
                resp.id,
                egui::EventFilter {
                    tab: true,
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    escape: true,
                },
            );
        });
        if chords.leave && resp.has_focus() {
            resp.surrender_focus();
        }
        let window_focused = ctx.input(|i| i.focused);
        let focused = resp.has_focus() && window_focused && !chords.leave;
        if self.had_focus && !focused {
            self.release_all();
        }
        self.had_focus = focused;
        self.image_rect = Some(rect);

        let connected = matches!(self.link, Link::Connected(_));
        if connected && let Some(g) = self.display_geometry() {
            let to_fb = move |p: egui::Pos2| to_framebuffer(rect, geom, p);
            let events = ctx.input(|i| i.events.clone());
            let out = self.input.translate(&events, focused, &to_fb, g);
            self.queue(out);
        }
        let border = if focused {
            egui::Stroke::new(2.0, p.accent)
        } else {
            egui::Stroke::new(1.0, p.border)
        };
        ui.painter()
            .rect_stroke(rect, 2.0, border, egui::StrokeKind::Outside);
        action
    }

    fn toolbar(
        &mut self,
        ui: &mut egui::Ui,
        layout: KeyboardLayout,
        fullscreen: bool,
        action: &mut Option<ConsoleAction>,
    ) {
        let p = theme::Palette::of(ui);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 16.0;
            if fullscreen {
                ui.label(egui::RichText::new(&self.target.vm_name).color(p.text));
            }
            let (text, tone) = self.link.describe();
            theme::status(ui, &text, tone).on_hover_text(match &self.link {
                Link::Connected(s) => format!("Per-run display ticket, security: {s}"),
                _ => "Per-run display ticket issued by the supervisor".into(),
            });
            if self.geometry.0 > 0 {
                ui.label(
                    egui::RichText::new(format!("{}×{}", self.geometry.0, self.geometry.1))
                        .color(p.text_weak),
                )
                .on_hover_text("Guest display resolution");
            }
            let mut chosen = layout;
            let r = egui::ComboBox::from_id_salt("lightvm_console_layout")
                .width(70.0)
                .selected_text(layout.label())
                .show_ui(ui, |ui| {
                    for l in KeyboardLayout::ALL {
                        ui.selectable_value(&mut chosen, l, l.description());
                    }
                });
            theme::accessible_name(
                &r.response,
                egui::WidgetType::ComboBox,
                "Guest keyboard layout",
            );
            r.response.on_hover_text(
                "Layout configured in the guest: keys are sent by physical position",
            );
            if chosen != layout {
                *action = Some(ConsoleAction::SetLayout(chosen));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let (text, hover) = if fullscreen {
                    ("Exit full screen", LEAVE_CHORD)
                } else {
                    ("Full screen", FULLSCREEN_CHORD)
                };
                if ui.button(text).on_hover_text(hover).clicked() {
                    *action = Some(if fullscreen {
                        ConsoleAction::ExitFullscreen
                    } else {
                        ConsoleAction::ToggleFullscreen
                    });
                }
                let can_shoot = self.texture.is_some();
                if ui
                    .add_enabled(can_shoot, egui::Button::new("Screenshot"))
                    .on_hover_text("Save the current screen as a PNG file")
                    .clicked()
                {
                    *action = Some(ConsoleAction::Screenshot);
                }
            });
        });
    }
}

impl Drop for LiveConsole {
    fn drop(&mut self) {
        if let Some(mut s) = self.session.take() {
            s.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(k: egui::Key, pressed: bool) -> egui::Event {
        egui::Event::Key {
            key: k,
            physical_key: Some(k),
            pressed,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }

    fn g(w: u32, h: u32) -> DisplayGeometry {
        DisplayGeometry::new(w, h).unwrap()
    }

    fn qk(q: &str, down: bool) -> InputEvent {
        InputEvent::Key(KeyAction {
            qcode: QCode::parse(q).unwrap(),
            down,
        })
    }

    #[test]
    fn keys_only_with_focus_and_release_on_loss() {
        let mut st = InputState::default();
        let none = |_p: egui::Pos2| None;
        let ev = [k(egui::Key::ControlLeft, true), k(egui::Key::T, true)];
        assert!(st.translate(&ev, false, &none, g(1280, 800)).is_empty());
        let out = st.translate(&ev, true, &none, g(1280, 800));
        assert_eq!(out, vec![qk("ctrl", true), qk("t", true)]);
        let rel = st.release_all();
        assert_eq!(rel, vec![qk("t", false), qk("ctrl", false)]);
        assert!(st.pressed_keys().is_empty());
    }

    #[test]
    fn pointer_scaled_and_buttons() {
        let mut st = InputState::default();
        let to_fb = |p: egui::Pos2| Some((p.x as u32, p.y as u32));
        let ev = [
            egui::Event::PointerMoved(egui::pos2(1279.0, 0.0)),
            egui::Event::PointerButton {
                pos: egui::pos2(1279.0, 0.0),
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
            egui::Event::PointerButton {
                pos: egui::pos2(1279.0, 0.0),
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            },
        ];
        let out = st.translate(&ev, false, &to_fb, g(1280, 800));
        assert_eq!(
            out,
            vec![
                InputEvent::PointerMove(PointerMove { x: 32767, y: 0 }),
                btn(MouseButton::Left, true),
                btn(MouseButton::Left, false),
            ]
        );
    }

    #[test]
    fn wheel_only_with_pointer_over_the_console() {
        let mut st = InputState::default();
        let inside = |p: egui::Pos2| (p.x < 100.0).then_some((p.x as u32, p.y as u32));
        let wheel = egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: egui::vec2(0.0, 1.0),
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::NONE,
        };
        let ev = [
            egui::Event::PointerMoved(egui::pos2(10.0, 10.0)),
            wheel.clone(),
        ];
        let out = st.translate(&ev, false, &inside, g(100, 100));
        assert!(out.contains(&btn(MouseButton::WheelUp, true)));
        let ev = [
            egui::Event::PointerMoved(egui::pos2(500.0, 10.0)),
            wheel.clone(),
        ];
        assert!(st.translate(&ev, false, &inside, g(100, 100)).is_empty());
        let ev = [egui::Event::PointerGone, wheel];
        assert!(st.translate(&ev, false, &inside, g(100, 100)).is_empty());
    }

    #[test]
    fn paste_does_not_forward_the_text() {
        let mut st = InputState::default();
        let none = |_p: egui::Pos2| None;
        let out = st.translate(&[egui::Event::Paste("secret".into())], true, &none, g(1, 1));
        assert_eq!(out, vec![qk("v", true), qk("v", false)]);
    }

    #[test]
    fn fit_keeps_aspect_and_centres() {
        let avail = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(800.0, 300.0));
        let r = fit(avail, (1280, 800));
        assert!((r.width() / r.height() - 1.6).abs() < 1e-3);
        assert_eq!(r.height(), 300.0);
        assert_eq!(r.center(), avail.center());
        let tall = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 900.0));
        let r = fit(tall, (1280, 800));
        assert_eq!(r.width(), 400.0);
        assert_eq!(r.height(), 250.0);
    }

    #[test]
    fn mapping_covers_corners_and_rejects_outside() {
        let rect = egui::Rect::from_min_size(egui::pos2(100.0, 50.0), egui::vec2(640.0, 400.0));
        let geom = (1280, 800);
        assert_eq!(to_framebuffer(rect, geom, rect.min), Some((0, 0)));
        assert_eq!(
            to_framebuffer(rect, geom, rect.max - egui::vec2(0.01, 0.01)),
            Some((1279, 799))
        );
        assert_eq!(to_framebuffer(rect, geom, rect.center()), Some((640, 400)));
        assert_eq!(to_framebuffer(rect, geom, egui::pos2(99.0, 60.0)), None);
    }

    #[test]
    fn chords_are_swallowed_with_their_release() {
        let ca = egui::Modifiers {
            ctrl: true,
            alt: true,
            command: true,
            ..Default::default()
        };
        let mut swallow = Vec::new();
        let mut ev = vec![
            egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: Some(egui::Key::Enter),
                pressed: true,
                repeat: false,
                modifiers: ca,
            },
            k(egui::Key::A, true),
        ];
        let c = take_chords(&mut ev, &mut swallow);
        assert!(c.toggle_fullscreen && !c.leave);
        assert_eq!(ev.len(), 1);
        let mut ev = vec![k(egui::Key::Enter, false)];
        assert_eq!(take_chords(&mut ev, &mut swallow), Chords::default());
        assert!(ev.is_empty(), "release of the chord key not forwarded");
        // Plain Enter is not a chord.
        let mut ev = vec![k(egui::Key::Enter, true), k(egui::Key::Enter, false)];
        assert_eq!(take_chords(&mut ev, &mut swallow), Chords::default());
        assert_eq!(ev.len(), 2);
    }
}
