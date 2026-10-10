//! F0-04 integration test against a real QEMU (ignored by default).
//!
//! Drives the UI headless with `egui_kittest`: egui keyboard events with
//! `physical_key` → `keymap` table → QMP `input-send-event` → guest; then the
//! "Screenshot" button → QMP `screendump`. It goes through neither winit nor
//! the operating system (the scancode → `egui::Key` layer remains untested
//! here).
//!
//! Run (QEMU started with `lab/scripts/f0-04-boot.ps1`, guest terminal in the
//! foreground):
//!
//! ```text
//! LIGHTVM_F004_DIR=E:/OpenVM/lab/vms/f0-04 cargo test -p lightvm-desktop --test real_console -- --ignored --nocapture
//! ```

use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable as _;
use lightvm_desktop::console::ConsoleConfig;
use lightvm_desktop::keymap::KeyboardLayout;
use lightvm_desktop::{LightVmApp, Section};
use rfb_client::{Transport, UnixImpl};

fn key(h: &Harness<'_, LightVmApp>, k: egui::Key, pressed: bool) {
    h.event(egui::Event::Key {
        key: k,
        physical_key: Some(k),
        pressed,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    });
}

fn wait(h: &mut Harness<'_, LightVmApp>, what: &str, secs: u64, ok: impl Fn(&LightVmApp) -> bool) {
    let t0 = Instant::now();
    while !ok(h.state()) {
        assert!(t0.elapsed() < Duration::from_secs(secs), "timeout: {what}");
        std::thread::sleep(Duration::from_millis(50));
        h.step();
    }
}

#[test]
#[ignore = "requires a real QEMU started by lab/scripts/f0-04-boot.ps1"]
fn keyboard_and_screenshot_from_ui_to_real_qemu() {
    let dir = PathBuf::from(std::env::var("LIGHTVM_F004_DIR").expect("LIGHTVM_F004_DIR"));
    let cfg = ConsoleConfig {
        vnc: Transport::Unix {
            path: dir.join("priv").join("vnc.sock"),
            imp: UnixImpl::Socket2,
        },
        qmp_socket: dir.join("priv").join("qmp.sock"),
        capture_dir: dir.clone(),
        tmp_dir: dir.join("priv"),
        layout: KeyboardLayout::Us,
        max_fps: 30,
    };
    // The app starts on the Dashboard: the console connects when shown.
    let mut app = LightVmApp::with_console(cfg);
    app.set_section(Section::Console);
    let mut h = Harness::builder()
        .with_size(egui::vec2(1460.0, 860.0))
        .with_max_steps(1_000_000)
        .build_ui_state(|ui, app: &mut LightVmApp| app.ui(ui), app);
    wait(&mut h, "RFB geometry", 15, |a| {
        a.live_console()
            .is_some_and(|c| c.geometry().0 > 0 && c.stats.texture_uploads > 0)
    });
    h.step();
    let geom = h.state().live_console().map(|c| c.geometry());
    println!("UI-REAL geometry={geom:?}");

    // Click on the console image: gives keyboard focus.
    h.get_by_label("Guest console framebuffer").click();
    h.step();
    h.step();

    let text: &[(egui::Key, bool)] = &[
        (egui::Key::E, false),
        (egui::Key::C, false),
        (egui::Key::H, false),
        (egui::Key::O, false),
        (egui::Key::Space, false),
        (egui::Key::U, true),
        (egui::Key::I, true),
        (egui::Key::Minus, false),
        (egui::Key::K, false),
        (egui::Key::I, false),
        (egui::Key::T, false),
        (egui::Key::T, false),
        (egui::Key::E, false),
        (egui::Key::S, false),
        (egui::Key::T, false),
        (egui::Key::Enter, false),
    ];
    for (k, shift) in text {
        if *shift {
            key(&h, egui::Key::ShiftLeft, true);
            h.step();
        }
        key(&h, *k, true);
        h.step();
        key(&h, *k, false);
        h.step();
        if *shift {
            key(&h, egui::Key::ShiftLeft, false);
            h.step();
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    // Key left pressed and then focus loss (click on "Dashboard"): the UI
    // must release it by itself.
    key(&h, egui::Key::ShiftLeft, true);
    h.step();
    let before = h.state().live_console().map(|c| c.stats.input_events);
    h.get_by_role_and_label(Role::Button, "Dashboard").click();
    h.step();
    h.step();
    let after = h.state().live_console().map(|c| c.stats.input_events);
    println!("UI-REAL input_events before_focus_loss={before:?} after={after:?}");
    assert!(after > before, "no release sent on focus loss");
    h.get_by_role_and_label(Role::Button, "Console").click();
    h.step();

    std::thread::sleep(Duration::from_millis(800));
    h.get_by_role_and_label(Role::Button, "Screenshot").click();
    h.step();
    let t0 = Instant::now();
    let shot = dir.join("ui-screenshot-001.png");
    while !shot.exists() {
        assert!(
            t0.elapsed() < Duration::from_secs(10),
            "screenshot not produced"
        );
        std::thread::sleep(Duration::from_millis(50));
        h.step();
    }
    std::thread::sleep(Duration::from_millis(200));
    let s = h.state().live_console().map(|c| c.stats.clone());
    println!("UI-REAL screenshot={} stats={s:?}", shot.display());
}
