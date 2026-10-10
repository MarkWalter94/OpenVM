//! F1-13: a `Paste` event (host clipboard text produced by egui-winit on
//! Ctrl+V / Shift+Insert) produces no text either in the console or in other
//! widgets, and the combination reaches the guest as keys via QMP.
//!
//! Headless test with `egui_kittest` and synthetic console (no QEMU): the QMP
//! channel is replaced by a local `Receiver`. It goes through neither winit
//! nor the operating system: the clipboard read inside egui-winit happens
//! before the app and is not observable here (see
//! `docs/verification/F1-05-desktop-egui.md`).

use std::sync::mpsc::Receiver;

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable as _;
use f0_console::qmp::InputEvent;
use lightvm_desktop::console::{LiveConsole, QmpCmd};
use lightvm_desktop::keymap::KeyboardLayout;
use lightvm_desktop::{LightVmApp, Section};

const SECRET: &str = "text-in-the-host-clipboard";

/// Test state: the app and a text field outside the app, drawn after it in
/// the same pass (it represents "any other widget").
struct Fixture {
    app: LightVmApp,
    field: String,
    draw_app: bool,
}

fn fixture_ui(ui: &mut egui::Ui, p: &mut Fixture) {
    if p.draw_app {
        p.app.ui(ui);
    }
    egui::Window::new("external field")
        .fixed_pos([1000.0, 4.0])
        .show(ui.ctx(), |ui| {
            ui.add(egui::TextEdit::singleline(&mut p.field).id_salt("external_field"));
        });
}

fn synthetic_frame() -> egui::ColorImage {
    egui::ColorImage::filled([320, 200], egui::Color32::from_rgb(20, 40, 80))
}

fn harness(draw_app: bool) -> (Harness<'static, Fixture>, Receiver<QmpCmd>) {
    let (console, rx) = LiveConsole::synthetic(KeyboardLayout::It, synthetic_frame());
    let mut app = LightVmApp::with_live_console(console);
    app.set_section(Section::Console);
    let h = Harness::builder()
        .with_size(egui::vec2(1280.0, 800.0))
        .build_ui_state(
            fixture_ui,
            Fixture {
                app,
                field: String::new(),
                draw_app,
            },
        );
    (h, rx)
}

fn ctrl() -> egui::Modifiers {
    egui::Modifiers {
        ctrl: true,
        command: true,
        ..Default::default()
    }
}

fn key(h: &Harness<'_, Fixture>, k: egui::Key, pressed: bool, modifiers: egui::Modifiers) {
    h.event(egui::Event::Key {
        key: k,
        physical_key: Some(k),
        pressed,
        repeat: false,
        modifiers,
    });
}

/// Paste combination as produced by egui-winit 0.36.2: `ModifiersChanged` +
/// press of the modifier (`Key` event), then `Paste(text)` **instead of** the
/// press of `paste_key`, then release of `paste_key` (`Key` event) and of
/// the modifier.
fn paste_like_egui_winit(
    h: &mut Harness<'_, Fixture>,
    modifier: egui::Key,
    mods: egui::Modifiers,
    paste_key: egui::Key,
) {
    h.event(egui::Event::ModifiersChanged(mods));
    key(h, modifier, true, mods);
    h.step();
    h.event(egui::Event::Paste(SECRET.into()));
    h.step();
    key(h, paste_key, false, mods);
    h.event(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
    key(h, modifier, false, egui::Modifiers::NONE);
    h.step();
}

fn ctrl_v_like_egui_winit(h: &mut Harness<'_, Fixture>) {
    paste_like_egui_winit(h, egui::Key::ControlLeft, ctrl(), egui::Key::V);
}

fn keys_only(ev: &[InputEvent]) -> Vec<(&'static str, bool)> {
    ev.iter()
        .filter_map(|e| match e {
            InputEvent::Key { qcode, down } => Some((*qcode, *down)),
            _ => None,
        })
        .collect()
}

fn sent_events(rx: &Receiver<QmpCmd>) -> Vec<InputEvent> {
    rx.try_iter()
        .flat_map(|c| match c {
            QmpCmd::Input(v) => v,
            QmpCmd::Screenshot => Vec::new(),
        })
        .collect()
}

/// Control: without the app's filter, the same event really inserts the text
/// into a focused field. It shows that the following test does not pass by
/// chance.
#[test]
fn control_without_app_paste_inserts_text() {
    let (mut h, _rx) = harness(false);
    h.run();
    h.get_by_role(Role::TextInput).click();
    h.run();
    h.event(egui::Event::Paste(SECRET.into()));
    h.run();
    assert_eq!(h.state().field, SECRET);
}

#[test]
fn paste_produces_no_text_in_other_widgets() {
    let (mut h, rx) = harness(true);
    h.run();
    h.get_by_role(Role::TextInput).click();
    h.run();
    ctrl_v_like_egui_winit(&mut h);
    h.run();
    assert_eq!(h.state().field, "", "the clipboard text reached the field");
    assert_eq!(h.state().app.discarded_pastes(), 1);
    // The console did not have focus: no key forwarded (the click on the
    // field can only produce pointer events, if it lands on the image).
    assert!(keys_only(&sent_events(&rx)).is_empty());
}

#[test]
fn paste_with_focused_console_becomes_ctrl_v_via_qmp() {
    let (mut h, rx) = harness(true);
    h.run();
    h.get_by_label("Guest console framebuffer").click();
    h.run();
    let _ = sent_events(&rx); // pointer movement and click
    ctrl_v_like_egui_winit(&mut h);
    h.run();
    let ev = sent_events(&rx);
    assert_eq!(
        keys_only(&ev),
        vec![("ctrl", true), ("v", true), ("v", false), ("ctrl", false)],
        "QMP events: {ev:?}"
    );
    assert_eq!(h.state().field, "");
    assert_eq!(h.state().app.discarded_pastes(), 1);
    // No text towards the guest: only keys, mouse and buttons.
    assert!(!format!("{ev:?}").contains(SECRET));
}

#[test]
fn shift_insert_becomes_shift_insert_via_qmp() {
    let (mut h, rx) = harness(true);
    h.run();
    h.get_by_label("Guest console framebuffer").click();
    h.run();
    let _ = sent_events(&rx);
    let shift = egui::Modifiers {
        shift: true,
        ..Default::default()
    };
    paste_like_egui_winit(&mut h, egui::Key::ShiftLeft, shift, egui::Key::Insert);
    h.run();
    assert_eq!(
        keys_only(&sent_events(&rx)),
        vec![
            ("shift", true),
            ("insert", true),
            ("insert", false),
            ("shift", false)
        ]
    );
    assert_eq!(h.state().app.discarded_pastes(), 1);
}
