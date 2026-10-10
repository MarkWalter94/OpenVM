//! Real console of the F0-04 spike: framebuffer from the read-only RFB client
//! (`rfb-client`) and input to QEMU **only** through QMP (`input-send-event`).
//!
//! The UI builds no QEMU configuration: it receives the socket paths from the
//! command line and exposes no text fields.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use f0_console::qmp::{Button, InputEvent, Qmp, scale_abs};
use rfb_client::{Framebuffer, Rect, Session, SessionConfig, SessionEvent, Transport};

use crate::keymap::{self, KeyboardLayout};
use crate::theme;

/// Parameters of the real console (from the command line).
#[derive(Clone, Debug)]
pub struct ConsoleConfig {
    /// RFB transport.
    pub vnc: Transport,
    /// QMP Unix socket.
    pub qmp_socket: PathBuf,
    /// Directory where screenshots are saved (the temporary PPM goes to `tmp_dir`).
    pub capture_dir: PathBuf,
    /// Private directory for the temporary PPM of `screendump`.
    pub tmp_dir: PathBuf,
    /// Declared layout of the guest.
    pub layout: KeyboardLayout,
    /// Maximum RFB updates per second.
    pub max_fps: u32,
}

/// Commands to the QMP thread.
#[derive(Debug)]
pub enum QmpCmd {
    /// Input events to send in a single `input-send-event`.
    Input(Vec<InputEvent>),
    /// Screenshot via `screendump`.
    Screenshot,
}

/// State of the forwarded keys and buttons, for releasing them when focus is lost.
#[derive(Debug, Default)]
pub struct InputState {
    pressed_keys: Vec<&'static str>,
    pressed_buttons: Vec<Button>,
    last_abs: Option<(u32, u32)>,
    /// Pointer over the console image: only then does the wheel go to the
    /// guest.
    pointer_inside: bool,
}

fn egui_button(b: egui::PointerButton) -> Option<Button> {
    Some(match b {
        egui::PointerButton::Primary => Button::Left,
        egui::PointerButton::Secondary => Button::Right,
        egui::PointerButton::Middle => Button::Middle,
        _ => return None,
    })
}

impl InputState {
    /// Keys currently pressed towards the guest.
    pub fn pressed_keys(&self) -> &[&'static str] {
        &self.pressed_keys
    }

    /// Translates the egui events of a frame into QMP events.
    /// - `keyboard`: the console has keyboard focus;
    /// - `to_fb`: converts a screen position into guest pixels (`None`
    ///   outside the image);
    /// - `geom`: current guest geometry, to scale the axes.
    pub fn translate(
        &mut self,
        events: &[egui::Event],
        keyboard: bool,
        to_fb: &dyn Fn(egui::Pos2) -> Option<(u32, u32)>,
        geom: (u32, u32),
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
                        out.push(InputEvent::Key {
                            qcode: q,
                            down: true,
                        });
                    } else if let Some(i) = self.pressed_keys.iter().position(|k| *k == q) {
                        self.pressed_keys.remove(i);
                        out.push(InputEvent::Key {
                            qcode: q,
                            down: false,
                        });
                    }
                }
                // egui-winit turns Ctrl+C/X/V into dedicated events without
                // `Key`: the key press is reconstructed (Ctrl is already
                // pressed). The content of `Paste` (host clipboard) is
                // discarded. Normally `Paste` does not get here:
                // `crate::clipboard` replaces it earlier with the physical
                // keys; this branch remains as a second defence.
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
                                out.push(InputEvent::Btn {
                                    button: b,
                                    down: true,
                                });
                            }
                        }
                    } else if let Some(i) = self.pressed_buttons.iter().position(|p| *p == b) {
                        self.pressed_buttons.remove(i);
                        out.push(InputEvent::Btn {
                            button: b,
                            down: false,
                        });
                    }
                }
                egui::Event::MouseWheel { delta, .. } if self.pointer_inside && delta.y != 0.0 => {
                    let b = if delta.y > 0.0 {
                        Button::WheelUp
                    } else {
                        Button::WheelDown
                    };
                    let n = (delta.y.abs().round() as u32).clamp(1, 5);
                    for _ in 0..n {
                        out.push(InputEvent::Btn {
                            button: b,
                            down: true,
                        });
                        out.push(InputEvent::Btn {
                            button: b,
                            down: false,
                        });
                    }
                }
                _ => {}
            }
        }
        out
    }

    fn push_abs(&mut self, out: &mut Vec<InputEvent>, x: u32, y: u32, geom: (u32, u32)) {
        let a = (scale_abs(x, geom.0), scale_abs(y, geom.1));
        if self.last_abs != Some(a) {
            self.last_abs = Some(a);
            out.push(InputEvent::Abs { x: a.0, y: a.1 });
        }
    }

    /// Releases all pressed keys and buttons (focus loss, disconnection,
    /// close).
    pub fn release_all(&mut self) -> Vec<InputEvent> {
        let mut out: Vec<InputEvent> = self
            .pressed_keys
            .drain(..)
            .rev()
            .map(|qcode| InputEvent::Key { qcode, down: false })
            .collect();
        out.extend(
            self.pressed_buttons
                .drain(..)
                .map(|button| InputEvent::Btn {
                    button,
                    down: false,
                }),
        );
        out
    }
}

fn tap(out: &mut Vec<InputEvent>, q: &'static str) {
    out.push(InputEvent::Key {
        qcode: q,
        down: true,
    });
    out.push(InputEvent::Key {
        qcode: q,
        down: false,
    });
}

/// Visible state of the connection.
#[derive(Clone, Debug, Default)]
pub struct LinkStatus {
    /// Text of the RFB state.
    pub rfb: String,
    /// Text of the QMP state.
    pub qmp: String,
    /// Outcome of the last screenshot.
    pub screenshot: String,
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
    /// Input events sent to the QMP thread.
    pub input_events: u64,
    /// Resizes.
    pub resizes: u64,
}

/// Console connected to QEMU.
pub struct LiveConsole {
    config: ConsoleConfig,
    session: Option<Session>,
    events: Option<Receiver<SessionEvent>>,
    fb: Option<Arc<Mutex<Framebuffer>>>,
    qmp_tx: Option<Sender<QmpCmd>>,
    status: Arc<Mutex<LinkStatus>>,
    texture: Option<egui::TextureHandle>,
    geometry: (u16, u16),
    connected: bool,
    input: InputState,
    had_focus: bool,
    /// Synthetic frame to upload (tests only: no connection to QEMU).
    synthetic: Option<egui::ColorImage>,
    /// Counters.
    pub stats: ConsoleStats,
}

impl std::fmt::Debug for LiveConsole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveConsole")
            .field("geometry", &self.geometry)
            .field("connected", &self.connected)
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

impl LiveConsole {
    /// Creates the console (the connection starts at the first frame).
    pub fn new(config: ConsoleConfig) -> Self {
        Self {
            config,
            session: None,
            events: None,
            fb: None,
            qmp_tx: None,
            status: Arc::new(Mutex::new(LinkStatus {
                rfb: "not started".into(),
                qmp: "not started".into(),
                screenshot: String::new(),
            })),
            texture: None,
            geometry: (0, 0),
            connected: false,
            input: InputState::default(),
            had_focus: false,
            synthetic: None,
            stats: ConsoleStats::default(),
        }
    }

    /// Console without QEMU with a fixed frame, for headless tests
    /// (snapshots and key forwarding). It opens no socket or thread: the
    /// events the UI would send via QMP arrive on the returned `Receiver`. It
    /// is not reachable from the command line (ADR-0022, criterion 5).
    #[doc(hidden)]
    pub fn synthetic(layout: KeyboardLayout, frame: egui::ColorImage) -> (Self, Receiver<QmpCmd>) {
        let dir = std::env::temp_dir().join("lightvm-synthetic-console");
        let mut c = Self::new(ConsoleConfig {
            vnc: Transport::Unix {
                path: dir.join("no-socket"),
                imp: rfb_client::UnixImpl::Socket2,
            },
            qmp_socket: dir.join("no-socket"),
            capture_dir: dir.clone(),
            tmp_dir: dir,
            layout,
            max_fps: 30,
        });
        let (tx, rx) = channel();
        c.qmp_tx = Some(tx);
        c.connected = true;
        c.geometry = (
            u16::try_from(frame.size[0]).unwrap_or(u16::MAX),
            u16::try_from(frame.size[1]).unwrap_or(u16::MAX),
        );
        if let Ok(mut s) = c.status.lock() {
            s.rfb = "synthetic frame (test, no QEMU)".into();
            s.qmp = "test channel".into();
        }
        c.synthetic = Some(frame);
        (c, rx)
    }

    /// Declared layout.
    pub fn layout(&self) -> KeyboardLayout {
        self.config.layout
    }

    /// Current guest geometry.
    pub fn geometry(&self) -> (u16, u16) {
        self.geometry
    }

    /// Copy of the shared framebuffer (for the smoke test).
    pub fn framebuffer_snapshot(&self) -> Option<Framebuffer> {
        self.fb
            .as_ref()
            .and_then(|f| f.lock().ok().map(|g| g.clone()))
    }

    fn start(&mut self, ctx: &egui::Context) {
        let (tx, rx) = channel();
        let ctx2 = ctx.clone();
        let session = Session::spawn(
            SessionConfig {
                transport: self.config.vnc.clone(),
                max_updates_per_second: self.config.max_fps,
            },
            move |ev| {
                let _ = tx.send(ev);
                ctx2.request_repaint();
            },
        );
        self.fb = Some(session.framebuffer());
        self.session = Some(session);
        self.events = Some(rx);
        if let Ok(mut s) = self.status.lock() {
            s.rfb = "connecting".into();
        }

        let (qtx, qrx) = channel::<QmpCmd>();
        let status = self.status.clone();
        let cfg = self.config.clone();
        let ctx3 = ctx.clone();
        let _ = std::thread::Builder::new()
            .name("qmp-input".into())
            .spawn(move || qmp_worker(&cfg, &qrx, &status, &ctx3));
        self.qmp_tx = Some(qtx);
    }

    fn send(&mut self, events: Vec<InputEvent>) {
        if events.is_empty() {
            return;
        }
        self.stats.input_events += events.len() as u64;
        if let Some(tx) = &self.qmp_tx {
            let _ = tx.send(QmpCmd::Input(events));
        }
    }

    /// The panel is not visible: releases the keys if it had focus.
    pub fn on_hidden(&mut self) {
        if self.had_focus {
            self.release_all();
            self.had_focus = false;
        }
    }

    /// Requests a screenshot via QMP.
    pub fn request_screenshot(&mut self) {
        if let Some(tx) = &self.qmp_tx {
            let _ = tx.send(QmpCmd::Screenshot);
        }
    }

    /// Releases all keys pressed towards the guest.
    pub fn release_all(&mut self) {
        let ev = self.input.release_all();
        self.send(ev);
    }

    fn pump_updates(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.events else { return };
        let mut dirty: Vec<Rect> = Vec::new();
        let mut full = false;
        while let Ok(ev) = rx.try_recv() {
            match ev {
                SessionEvent::Connected { init, security, .. } => {
                    self.connected = true;
                    self.geometry = (init.width, init.height);
                    full = true;
                    if let Ok(mut s) = self.status.lock() {
                        s.rfb = format!("connected ({security}), desktop \"{}\"", init.name);
                    }
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
                    self.connected = false;
                    if let Ok(mut s) = self.status.lock() {
                        s.rfb = match error {
                            Some(e) => format!("disconnected: {e}"),
                            None => "disconnected".into(),
                        };
                    }
                }
            }
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

    /// Draws the Console panel.
    pub fn ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        if let Some(frame) = self.synthetic.take() {
            self.texture =
                Some(ctx.load_texture("console-synthetic", frame, egui::TextureOptions::LINEAR));
            self.stats.texture_uploads += 1;
        } else if self.session.is_none() && self.texture.is_none() {
            self.start(ctx);
        }
        self.pump_updates(ctx);
        let status = self.status.lock().map(|s| s.clone()).unwrap_or_default();

        let p = theme::Palette::of(ui);
        let mut screenshot = false;
        theme::page_header(
            ui,
            "Console",
            "Screen of the guest: click on the image to send keyboard and mouse.",
            |ui| screenshot = ui.button("Screenshot").clicked(),
        );
        if screenshot {
            self.request_screenshot();
        }
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 18.0;
            let tone = if self.connected {
                theme::Tone::Positive
            } else {
                theme::Tone::Danger
            };
            theme::status(ui, &format!("RFB: {}", status.rfb), tone);
            let weak = |t: String| egui::RichText::new(t).color(p.text_weak);
            ui.label(weak(format!(
                "Geometry: {}×{}",
                self.geometry.0, self.geometry.1
            )));
            ui.label(weak(format!("QMP input: {}", status.qmp)));
            ui.label(weak(format!(
                "Guest layout: {}",
                self.config.layout.label()
            )));
        });
        if !status.screenshot.is_empty() {
            ui.label(
                egui::RichText::new(&status.screenshot)
                    .small()
                    .color(p.text_weak),
            );
        }
        ui.add_space(8.0);

        let Some(tex) = &self.texture else {
            ui.label("Waiting for the first framebuffer update…");
            return;
        };
        let avail = ui.available_size();
        let (gw, gh) = (
            f32::from(self.geometry.0.max(1)),
            f32::from(self.geometry.1.max(1)),
        );
        let aspect = gw / gh;
        let mut size = egui::vec2(avail.x, avail.x / aspect);
        if size.y > avail.y {
            size = egui::vec2(avail.y * aspect, avail.y);
        }
        let resp = ui.add(
            egui::Image::new((tex.id(), size))
                .sense(egui::Sense::click_and_drag())
                .alt_text("Guest console framebuffer"),
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
        let window_focused = ctx.input(|i| i.focused);
        let focused = resp.has_focus() && window_focused;
        if self.had_focus && !focused {
            self.release_all();
        }
        self.had_focus = focused;

        let rect = resp.rect;
        let geom = (u32::from(self.geometry.0), u32::from(self.geometry.1));
        let to_fb = move |p: egui::Pos2| -> Option<(u32, u32)> {
            if !rect.contains(p) || geom.0 == 0 || geom.1 == 0 {
                return None;
            }
            let x = ((p.x - rect.min.x) / rect.width() * geom.0 as f32).floor() as u32;
            let y = ((p.y - rect.min.y) / rect.height() * geom.1 as f32).floor() as u32;
            Some((x.min(geom.0 - 1), y.min(geom.1 - 1)))
        };
        let events = ctx.input(|i| i.events.clone());
        let out = self.input.translate(&events, focused, &to_fb, geom);
        self.send(out);
        let border = if focused {
            egui::Stroke::new(2.0, p.accent)
        } else {
            egui::Stroke::new(1.0, p.border)
        };
        ui.painter()
            .rect_stroke(rect, 2.0, border, egui::StrokeKind::Outside);
    }
}

impl Drop for LiveConsole {
    fn drop(&mut self) {
        self.release_all();
        if let Some(mut s) = self.session.take() {
            s.stop();
        }
    }
}

fn set_status(status: &Mutex<LinkStatus>, f: impl FnOnce(&mut LinkStatus)) {
    if let Ok(mut s) = status.lock() {
        f(&mut s);
    }
}

fn qmp_worker(
    cfg: &ConsoleConfig,
    rx: &Receiver<QmpCmd>,
    status: &Mutex<LinkStatus>,
    ctx: &egui::Context,
) {
    let mut q = match Qmp::connect(&cfg.qmp_socket) {
        Ok(q) => {
            set_status(status, |s| s.qmp = "connected".into());
            q
        }
        Err(e) => {
            set_status(status, |s| s.qmp = format!("error: {e}"));
            ctx.request_repaint();
            return;
        }
    };
    ctx.request_repaint();
    let mut shots = 0u32;
    while let Ok(cmd) = rx.recv() {
        match cmd {
            QmpCmd::Input(ev) => {
                if let Err(e) = q.send_input(&ev) {
                    set_status(status, |s| s.qmp = format!("error: {e}"));
                    ctx.request_repaint();
                }
            }
            QmpCmd::Screenshot => {
                let t0 = Instant::now();
                shots += 1;
                let ppm = cfg.tmp_dir.join("ui-screendump.ppm");
                let res = (|| -> Result<String, Box<dyn std::error::Error>> {
                    q.screendump_ppm(&ppm)?;
                    let img = f0_console::image::parse_ppm(&std::fs::read(&ppm)?)?;
                    let png = f0_console::image::encode_png(&img)?;
                    let out = cfg
                        .capture_dir
                        .join(format!("ui-screenshot-{shots:03}.png"));
                    std::fs::write(&out, png)?;
                    let _ = std::fs::remove_file(&ppm);
                    Ok(format!(
                        "screenshot {}x{} in {} ms: {}",
                        img.width,
                        img.height,
                        t0.elapsed().as_millis(),
                        out.display()
                    ))
                })();
                let text = res.unwrap_or_else(|e| format!("screenshot failed: {e}"));
                println!("SCREENSHOT {text}");
                set_status(status, |s| s.screenshot = text);
                ctx.request_repaint();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(k: egui::Key, pressed: bool) -> egui::Event {
        egui::Event::Key {
            key: k,
            physical_key: Some(k),
            pressed,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }

    #[test]
    fn keys_only_with_focus_and_release_on_loss() {
        let mut st = InputState::default();
        let none = |_p: egui::Pos2| None;
        let ev = [key(egui::Key::ControlLeft, true), key(egui::Key::T, true)];
        assert!(st.translate(&ev, false, &none, (1280, 800)).is_empty());
        let out = st.translate(&ev, true, &none, (1280, 800));
        assert_eq!(
            out,
            vec![
                InputEvent::Key {
                    qcode: "ctrl",
                    down: true
                },
                InputEvent::Key {
                    qcode: "t",
                    down: true
                }
            ]
        );
        let rel = st.release_all();
        assert_eq!(
            rel,
            vec![
                InputEvent::Key {
                    qcode: "t",
                    down: false
                },
                InputEvent::Key {
                    qcode: "ctrl",
                    down: false
                }
            ]
        );
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
        let out = st.translate(&ev, false, &to_fb, (1280, 800));
        assert_eq!(
            out,
            vec![
                InputEvent::Abs { x: 32767, y: 0 },
                InputEvent::Btn {
                    button: Button::Left,
                    down: true
                },
                InputEvent::Btn {
                    button: Button::Left,
                    down: false
                }
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
        let out = st.translate(&ev, false, &inside, (100, 100));
        assert!(out.iter().any(|e| matches!(
            e,
            InputEvent::Btn {
                button: Button::WheelUp,
                ..
            }
        )));
        // Outside the image (header, sidebar): no forwarding.
        let ev = [
            egui::Event::PointerMoved(egui::pos2(500.0, 10.0)),
            wheel.clone(),
        ];
        assert!(st.translate(&ev, false, &inside, (100, 100)).is_empty());
        let ev = [egui::Event::PointerGone, wheel];
        assert!(st.translate(&ev, false, &inside, (100, 100)).is_empty());
    }

    #[test]
    fn paste_does_not_forward_the_text() {
        let mut st = InputState::default();
        let none = |_p: egui::Pos2| None;
        let out = st.translate(&[egui::Event::Paste("secret".into())], true, &none, (1, 1));
        assert_eq!(
            out,
            vec![
                InputEvent::Key {
                    qcode: "v",
                    down: true
                },
                InputEvent::Key {
                    qcode: "v",
                    down: false
                }
            ]
        );
    }
}
