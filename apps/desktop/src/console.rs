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

use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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

/// Minimum interval between two input batches sent to the supervisor: at
/// most 50 requests per second, half of the per-connection rate limit of the
/// service (100 requests/s, burst 50), so a fast mouse or a high refresh
/// rate never makes the service refuse a batch with key releases in it.
pub const INPUT_INTERVAL: Duration = Duration::from_millis(20);

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
///
/// Only a fresh press (not an autorepeat) is a chord, and a key whose press
/// already reached the guest (`forwarded`) is never swallowed: holding Enter
/// and then adding Ctrl+Alt neither toggles full screen at every autorepeat
/// nor eats the Enter release the guest is waiting for.
pub fn take_chords(
    events: &mut Vec<egui::Event>,
    swallow: &mut Vec<egui::Key>,
    forwarded: &dyn Fn(egui::Key) -> bool,
) -> Chords {
    let mut c = Chords::default();
    events.retain(|e| {
        let egui::Event::Key {
            key,
            physical_key,
            pressed,
            repeat,
            modifiers,
        } = e
        else {
            return true;
        };
        let k = physical_key.unwrap_or(*key);
        if !matches!(k, egui::Key::Enter | egui::Key::Escape) {
            return true;
        }
        if forwarded(k) && !swallow.contains(&k) {
            return true;
        }
        if *pressed && !*repeat && is_chord(*modifiers) {
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
        // Autorepeat of a swallowed key; any other press or release passes
        // (an autorepeat with Ctrl+Alt of a key held before is not a chord).
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
    /// Wheel movement not yet sent, in notches (touchpads report fractions).
    wheel_acc: f32,
}

/// Points of a [`egui::MouseWheelUnit::Point`] scroll that make one wheel
/// notch (touchpads and precise wheels report points).
pub const WHEEL_POINTS_PER_NOTCH: f32 = 50.0;
/// Notches of a [`egui::MouseWheelUnit::Page`] scroll.
pub const WHEEL_NOTCHES_PER_PAGE: f32 = 5.0;
/// Most notches sent for a single wheel event.
const MAX_NOTCHES_PER_EVENT: u32 = 10;

/// Wheel movement in notches (positive: up).
pub fn wheel_notches(unit: egui::MouseWheelUnit, delta_y: f32) -> f32 {
    match unit {
        egui::MouseWheelUnit::Point => delta_y / WHEEL_POINTS_PER_NOTCH,
        egui::MouseWheelUnit::Line => delta_y,
        egui::MouseWheelUnit::Page => delta_y * WHEEL_NOTCHES_PER_PAGE,
    }
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

    /// The press of this physical key was forwarded and its release not yet.
    pub fn is_forwarded(&self, key: egui::Key) -> bool {
        keymap::qcode(key).is_some_and(|q| self.pressed_keys.contains(&q))
    }

    /// Sets whether the pointer is over the console image (from its current
    /// position, before the events of the frame are translated).
    pub fn set_pointer_inside(&mut self, inside: bool) {
        if !inside {
            self.wheel_acc = 0.0;
        }
        self.pointer_inside = inside;
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
                egui::Event::PointerGone => {
                    self.pointer_inside = false;
                    self.wheel_acc = 0.0;
                }
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
                egui::Event::MouseWheel { unit, delta, .. } => {
                    if !self.pointer_inside || !delta.y.is_finite() {
                        self.wheel_acc = 0.0;
                        continue;
                    }
                    let notches = wheel_notches(*unit, delta.y);
                    if notches == 0.0 {
                        continue;
                    }
                    // A change of direction drops the remainder of the other one.
                    if notches.signum() != self.wheel_acc.signum() {
                        self.wheel_acc = 0.0;
                    }
                    self.wheel_acc += notches;
                    let whole = self.wheel_acc.trunc();
                    if whole == 0.0 {
                        continue;
                    }
                    self.wheel_acc -= whole;
                    let b = if whole > 0.0 {
                        MouseButton::WheelUp
                    } else {
                        MouseButton::WheelDown
                    };
                    let n = (whole.abs() as u32).min(MAX_NOTCHES_PER_EVENT);
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
    /// Input not sent yet, oldest first: the last batch keeps growing until
    /// it is sent (see [`INPUT_INTERVAL`]).
    pending: VecDeque<InputBatch>,
    /// Instant the last batch was handed over for sending.
    last_sent: Option<Instant>,
    /// Frame to upload (tests only: no connection).
    synthetic: Option<egui::ColorImage>,
    /// Copy of the synthetic frame, for screenshots in tests.
    synthetic_copy: Option<egui::ColorImage>,
    /// Rectangle of the image in the last frame.
    image_rect: Option<egui::Rect>,
    disconnected_at: Option<Instant>,
    /// A popup (layout list) was open in the previous frame: the click that
    /// closes it does not reach the guest either.
    popup_was_open: bool,
    /// An agent session owns the input of the VM (F4): nothing the user
    /// types or clicks is queued.
    input_blocked: bool,
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
            pending: VecDeque::new(),
            last_sent: None,
            synthetic: None,
            synthetic_copy: None,
            image_rect: None,
            disconnected_at: None,
            popup_was_open: false,
            input_blocked: false,
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

    /// Marks the display session as ended at `at` (headless tests of the
    /// reconnection; the real console learns it from the RFB session).
    #[doc(hidden)]
    pub fn simulate_disconnect(&mut self, at: Instant) {
        self.link = Link::Disconnected(Some("test disconnection".into()));
        self.disconnected_at = Some(at);
        self.release_all();
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

    /// The press of this physical key reached the guest and its release
    /// not yet.
    pub fn is_forwarded(&self, key: egui::Key) -> bool {
        self.input.is_forwarded(key)
    }

    /// The console image had keyboard focus in the last frame.
    pub fn has_focus(&self) -> bool {
        self.had_focus
    }

    /// Next input batch to send to the supervisor, if one is waiting and
    /// [`INPUT_INTERVAL`] has passed since the previous one. Events queued
    /// meanwhile accumulate in the batch (consecutive pointer moves are
    /// coalesced into the last one; keys and buttons are never dropped).
    pub fn take_input(&mut self, now: Instant) -> Option<InputBatch> {
        if self.pending.is_empty() || self.next_input_in(now).is_some_and(|d| !d.is_zero()) {
            return None;
        }
        self.last_sent = Some(now);
        self.pending.pop_front()
    }

    /// Wait before the next batch can be sent; `None` if nothing waits.
    pub fn next_input_in(&self, now: Instant) -> Option<Duration> {
        if self.pending.is_empty() {
            return None;
        }
        Some(self.last_sent.map_or(Duration::ZERO, |t| {
            (t + INPUT_INTERVAL).saturating_duration_since(now)
        }))
    }

    /// Input is waiting to be sent.
    pub fn has_pending_input(&self) -> bool {
        !self.pending.is_empty()
    }

    /// All the waiting batches at once, regardless of the interval (close).
    fn drain_input(&mut self) -> Vec<InputBatch> {
        self.pending.drain(..).collect()
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
        for ev in events {
            let open = self.pending.back_mut().filter(|b| {
                b.geometry == geometry && b.events.len() < MAX_INPUT_EVENTS_PER_REQUEST
            });
            match open {
                Some(b) => {
                    // Absolute positions: only the last of consecutive moves counts.
                    if matches!(ev, InputEvent::PointerMove(_))
                        && let Some(last @ InputEvent::PointerMove(_)) = b.events.last_mut()
                    {
                        *last = ev;
                    } else {
                        b.events.push(ev);
                    }
                }
                None => self.pending.push_back(InputBatch {
                    vm_id: self.target.vm_id,
                    run_id: self.target.run_id,
                    geometry,
                    events: vec![ev],
                }),
            }
        }
    }

    /// An agent owns the input of the VM (`true`) or the user again
    /// (`false`). On the handover the keys and buttons the user held are
    /// forgotten and the input not sent yet is dropped: the supervisor
    /// releases them in the guest and would refuse the user's input anyway.
    pub fn set_input_blocked(&mut self, blocked: bool) {
        if blocked && !self.input_blocked {
            let _ = self.input.release_all();
            self.pending.clear();
            self.had_focus = false;
        }
        self.input_blocked = blocked;
    }

    /// The input of the VM belongs to an agent session.
    pub fn input_blocked(&self) -> bool {
        self.input_blocked
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
        self.drain_input()
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
        // The transport (with the display token) moves into the session: no
        // copy of the token stays in the console.
        let Some(transport) = self.transport.take() else {
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
    /// removed from the input of this frame by the app; `modal_open`: a
    /// dialog of the app covers the window, so the pointer belongs to it.
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        layout: KeyboardLayout,
        fullscreen: bool,
        chords: Chords,
        input_error: Option<&str>,
        modal_open: bool,
    ) -> Option<ConsoleAction> {
        let ctx = ui.ctx().clone();
        if let Some(frame) = self.synthetic.take() {
            self.texture =
                Some(ctx.load_texture("console-synthetic", frame, egui::TextureOptions::LINEAR));
            self.stats.texture_uploads += 1;
        } else if self.session.is_none() && self.transport.is_some() {
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
        if self.input_blocked {
            // The agent has control: the image takes no keyboard focus.
            if resp.has_focus() {
                resp.surrender_focus();
            }
        } else if resp.clicked() || resp.drag_started() {
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
        let focused = resp.has_focus() && window_focused && !chords.leave && !self.input_blocked;
        if self.had_focus && !focused {
            self.release_all();
        }
        self.had_focus = focused;
        self.image_rect = Some(rect);

        let connected = matches!(self.link, Link::Connected(_));
        if connected && let Some(g) = self.display_geometry() {
            // The pointer belongs to the guest only over the image itself:
            // not over a popup (layout list, menus), a tooltip area or a
            // dialog drawn above it, and not while a dialog is open.
            let layer = ui.layer_id();
            let popup_open = ctx.any_popup_open();
            let blocked = modal_open || popup_open || self.popup_was_open || self.input_blocked;
            self.popup_was_open = popup_open;
            let ctx2 = ctx.clone();
            let to_fb = move |p: egui::Pos2| {
                if blocked || ctx2.layer_id_at(p).is_some_and(|l| l != layer) {
                    return None;
                }
                to_framebuffer(rect, geom, p)
            };
            let hover = ctx.input(|i| i.pointer.hover_pos());
            self.input
                .set_pointer_inside(hover.and_then(&to_fb).is_some());
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
        let c = take_chords(&mut ev, &mut swallow, &|_| false);
        assert!(c.toggle_fullscreen && !c.leave);
        assert_eq!(ev.len(), 1);
        let mut ev = vec![k(egui::Key::Enter, false)];
        assert_eq!(
            take_chords(&mut ev, &mut swallow, &|_| false),
            Chords::default()
        );
        assert!(ev.is_empty(), "release of the chord key not forwarded");
        // Plain Enter is not a chord.
        let mut ev = vec![k(egui::Key::Enter, true), k(egui::Key::Enter, false)];
        assert_eq!(
            take_chords(&mut ev, &mut swallow, &|_| false),
            Chords::default()
        );
        assert_eq!(ev.len(), 2);
    }

    fn key_mod(
        k: egui::Key,
        pressed: bool,
        repeat: bool,
        modifiers: egui::Modifiers,
    ) -> egui::Event {
        egui::Event::Key {
            key: k,
            physical_key: Some(k),
            pressed,
            repeat,
            modifiers,
        }
    }

    fn ctrl_alt() -> egui::Modifiers {
        egui::Modifiers {
            ctrl: true,
            alt: true,
            command: true,
            ..Default::default()
        }
    }

    /// Finding 4: an autorepeat is never a chord, and a key whose press
    /// reached the guest keeps its release.
    #[test]
    fn autorepeat_is_not_a_chord_and_forwarded_keys_keep_their_release() {
        let mut swallow = Vec::new();
        // Enter held (forwarded to the guest), then Ctrl+Alt added: the
        // autorepeats carry Ctrl+Alt.
        let held = |k: egui::Key| k == egui::Key::Enter;
        let mut ev = vec![
            key_mod(egui::Key::Enter, true, true, ctrl_alt()),
            key_mod(egui::Key::Enter, true, true, ctrl_alt()),
            key_mod(egui::Key::Enter, false, false, ctrl_alt()),
        ];
        let c = take_chords(&mut ev, &mut swallow, &held);
        assert_eq!(c, Chords::default(), "autorepeat toggled full screen");
        assert_eq!(ev.len(), 3, "events of a forwarded key swallowed");
        assert!(swallow.is_empty());

        // Even without the forwarded information, a repeat is not a chord.
        let mut ev = vec![key_mod(egui::Key::Enter, true, true, ctrl_alt())];
        assert_eq!(
            take_chords(&mut ev, &mut swallow, &|_| false),
            Chords::default()
        );
        assert_eq!(ev.len(), 1);

        // A real chord: the press toggles once, its autorepeats and its
        // release are swallowed.
        let mut ev = vec![
            key_mod(egui::Key::Enter, true, false, ctrl_alt()),
            key_mod(egui::Key::Enter, true, true, ctrl_alt()),
            key_mod(egui::Key::Enter, true, true, ctrl_alt()),
        ];
        let c = take_chords(&mut ev, &mut swallow, &|_| false);
        assert!(c.toggle_fullscreen);
        assert!(ev.is_empty());
        let mut ev = vec![key_mod(egui::Key::Enter, false, false, ctrl_alt())];
        take_chords(&mut ev, &mut swallow, &|_| false);
        assert!(ev.is_empty());
        assert!(swallow.is_empty());
    }

    /// Through `InputState`: Enter forwarded, then the autorepeat with
    /// Ctrl+Alt, then the release reaches the guest.
    #[test]
    fn held_enter_is_released_in_the_guest() {
        let mut st = InputState::default();
        let none = |_p: egui::Pos2| None;
        let mut swallow = Vec::new();
        let mut frame = vec![key_mod(
            egui::Key::Enter,
            true,
            false,
            egui::Modifiers::NONE,
        )];
        take_chords(&mut frame, &mut swallow, &|k| st.is_forwarded(k));
        let mut out = st.translate(&frame, true, &none, g(640, 480));
        let mut frame = vec![
            key_mod(egui::Key::Enter, true, true, ctrl_alt()),
            key_mod(egui::Key::Enter, false, false, ctrl_alt()),
        ];
        let c = take_chords(&mut frame, &mut swallow, &|k| st.is_forwarded(k));
        assert_eq!(c, Chords::default());
        out.extend(st.translate(&frame, true, &none, g(640, 480)));
        assert_eq!(out, vec![qk("ret", true), qk("ret", false)]);
        assert!(st.pressed_keys().is_empty());
    }

    fn wheel(unit: egui::MouseWheelUnit, dy: f32) -> egui::Event {
        egui::Event::MouseWheel {
            unit,
            delta: egui::vec2(0.0, dy),
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::NONE,
        }
    }

    /// Finding 10: touchpad scrolling (points) becomes whole notches, with
    /// the remainder kept; lines and pages are converted by their unit.
    #[test]
    fn wheel_is_converted_by_unit_and_accumulated() {
        use egui::MouseWheelUnit::{Line, Page, Point};
        let mut st = InputState::default();
        let inside = |p: egui::Pos2| Some((p.x as u32, p.y as u32));
        let mut tr = |ev: &[egui::Event]| st.translate(ev, false, &inside, g(100, 100));
        let mut ev = vec![egui::Event::PointerMoved(egui::pos2(5.0, 5.0))];
        // A touchpad gesture: 40 small events of 2.5 points = 100 points.
        ev.extend((0..40).map(|_| wheel(Point, 2.5)));
        let out = tr(&ev);
        let ups = out
            .iter()
            .filter(|e| **e == btn(MouseButton::WheelUp, true))
            .count();
        assert_eq!(ups, 2, "100 points = 2 notches, not 40 clicks: {out:?}");
        assert_eq!(out.len(), 1 + 2 * 2, "one move and two notches");
        // Remainder kept: 30 + 20 points complete a notch.
        assert!(tr(&[wheel(Point, 30.0)]).is_empty());
        let up = vec![
            btn(MouseButton::WheelUp, true),
            btn(MouseButton::WheelUp, false),
        ];
        assert_eq!(tr(&[wheel(Point, 20.0)]), up);
        // Lines: one notch each; pages: several; down is WheelDown.
        assert_eq!(
            tr(&[wheel(Line, -1.0)]),
            vec![
                btn(MouseButton::WheelDown, true),
                btn(MouseButton::WheelDown, false)
            ]
        );
        assert_eq!(
            tr(&[wheel(Page, 1.0)]).len(),
            2 * WHEEL_NOTCHES_PER_PAGE as usize
        );
        // Half a notch then the other direction: the remainder is dropped.
        assert!(tr(&[wheel(Line, 0.6)]).is_empty());
        let out = tr(&[wheel(Line, -0.6)]);
        assert!(out.is_empty(), "{out:?}");
    }

    fn console() -> LiveConsole {
        LiveConsole::synthetic(
            ConsoleTarget {
                vm_id: VmId::new_v7(),
                vm_name: "t".into(),
                run_id: RunId::new_v7(),
            },
            egui::ColorImage::filled([1280, 800], egui::Color32::BLACK),
        )
    }

    /// Finding 1: 300 frames per second of mouse movement for two seconds,
    /// with keys pressed and released meanwhile: at most one request every
    /// 20 ms (≤ 50/s, under the service limit of 100/s), every key and
    /// button event delivered in order, the last position delivered.
    #[test]
    fn input_is_paced_coalesced_and_never_drops_keys() {
        let mut c = console();
        let t0 = Instant::now();
        let frame = Duration::from_micros(3333);
        let mut sent: Vec<(Duration, InputBatch)> = Vec::new();
        let mut expected = Vec::new();
        let pos = |i: u32| PointerMove {
            x: (i * 50 % 32768) as u16,
            y: (i * 30 % 32768) as u16,
        };
        for i in 0..600u32 {
            let now = t0 + frame * i;
            let mut ev = vec![InputEvent::PointerMove(pos(i))];
            if i % 7 == 0 {
                let down = i % 14 == 0;
                ev.push(qk("a", down));
                expected.push(qk("a", down));
            }
            if i % 45 == 0 {
                ev.push(btn(MouseButton::Left, true));
                ev.push(InputEvent::PointerMove(PointerMove { x: 1, y: 1 }));
                ev.push(btn(MouseButton::Left, false));
                expected.push(btn(MouseButton::Left, true));
                expected.push(btn(MouseButton::Left, false));
            }
            c.queue(ev);
            if let Some(b) = c.take_input(now) {
                sent.push((now - t0, b));
            }
        }
        // Drain what is left at the pace of the interval.
        let mut now = t0 + frame * 600;
        while c.has_pending_input() {
            now += c.next_input_in(now).unwrap();
            if let Some(b) = c.take_input(now) {
                sent.push((now - t0, b));
            }
        }
        // Pace: no two requests closer than the interval, so at most 50 in
        // any second (the service admits 100/s with a burst of 50).
        for w in sent.windows(2) {
            assert!(
                w[1].0 - w[0].0 >= INPUT_INTERVAL,
                "{:?} {:?}",
                w[0].0,
                w[1].0
            );
        }
        for (i, (t, _)) in sent.iter().enumerate() {
            let in_second = sent[i..]
                .iter()
                .take_while(|(u, _)| *u < *t + Duration::from_secs(1))
                .count();
            assert!(in_second <= 50, "{in_second} requests in one second");
        }
        // Every key and button event delivered, in order.
        let all: Vec<InputEvent> = sent.iter().flat_map(|(_, b)| b.events.clone()).collect();
        let delivered: Vec<InputEvent> = all
            .iter()
            .filter(|e| !matches!(e, InputEvent::PointerMove(_)))
            .cloned()
            .collect();
        assert_eq!(delivered, expected);
        // Coalesced: far fewer moves than the 600 frames, never two in a row.
        let moves = all
            .iter()
            .filter(|e| matches!(e, InputEvent::PointerMove(_)))
            .count();
        assert!(moves < 300, "{moves} moves sent");
        for (_, b) in &sent {
            assert!(b.events.len() <= MAX_INPUT_EVENTS_PER_REQUEST);
            for w in b.events.windows(2) {
                assert!(
                    !(matches!(w[0], InputEvent::PointerMove(_))
                        && matches!(w[1], InputEvent::PointerMove(_))),
                    "consecutive moves not coalesced"
                );
            }
        }
        // The last position reaches the guest.
        assert_eq!(
            all.iter()
                .rev()
                .find(|e| matches!(e, InputEvent::PointerMove(_))),
            Some(&InputEvent::PointerMove(pos(599)))
        );
    }

    /// A batch is split only above the per-request maximum.
    #[test]
    fn batches_split_only_above_the_maximum() {
        let mut c = console();
        let t0 = Instant::now();
        let n = MAX_INPUT_EVENTS_PER_REQUEST + 10;
        let ev: Vec<InputEvent> = (0..n).map(|i| qk("a", i % 2 == 0)).collect();
        c.queue(ev);
        let first = c.take_input(t0).unwrap();
        assert_eq!(first.events.len(), MAX_INPUT_EVENTS_PER_REQUEST);
        assert!(
            c.take_input(t0 + Duration::from_millis(5)).is_none(),
            "paced"
        );
        let second = c.take_input(t0 + INPUT_INTERVAL).unwrap();
        assert_eq!(second.events.len(), 10);
        assert!(!c.has_pending_input());
        // Close hands over everything at once (key releases included).
        c.queue(vec![qk("b", true)]);
        c.queue(vec![qk("b", false)]);
        assert_eq!(c.close().len(), 1);
    }
}
