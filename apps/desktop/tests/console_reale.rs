//! Prova d'integrazione F0-04 contro un QEMU reale (ignorata per default).
//!
//! Pilota la UI headless con `egui_kittest`: eventi tastiera egui con
//! `physical_key` → tabella `keymap` → QMP `input-send-event` → guest; poi il
//! pulsante "Screenshot" → QMP `screendump`. Non attraversa winit né il
//! sistema operativo (lo strato scancode → `egui::Key` resta non provato qui).
//!
//! Esecuzione (QEMU avviato con `lab/scripts/f0-04-boot.ps1`, terminale del
//! guest in primo piano):
//!
//! ```text
//! LIGHTVM_F004_DIR=E:/OpenVM/lab/vms/f0-04 cargo test -p lightvm-desktop --test console_reale -- --ignored --nocapture
//! ```

use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable as _;
use lightvm_desktop::LightVmApp;
use lightvm_desktop::console::ConsoleConfig;
use lightvm_desktop::keymap::KeyboardLayout;
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
#[ignore = "richiede QEMU reale avviato da lab/scripts/f0-04-boot.ps1"]
fn tastiera_e_screenshot_dalla_ui_verso_qemu_reale() {
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
    let mut h = Harness::builder()
        .with_size(egui::vec2(1460.0, 860.0))
        .with_max_steps(1_000_000)
        .build_ui_state(
            |ui, app: &mut LightVmApp| app.ui(ui),
            LightVmApp::with_console(cfg),
        );
    wait(&mut h, "geometria RFB", 15, |a| {
        a.live_console()
            .is_some_and(|c| c.geometry().0 > 0 && c.stats.texture_uploads > 0)
    });
    h.step();
    let geom = h.state().live_console().map(|c| c.geometry());
    println!("UI-REALE geometria={geom:?}");

    // Clic sull'immagine della console: assegna il focus della tastiera.
    h.get_by_label("Framebuffer della console del guest")
        .click();
    h.step();
    h.step();

    let testo: &[(egui::Key, bool)] = &[
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
    for (k, shift) in testo {
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
    // Tasto lasciato premuto e poi perdita di focus (clic su "Dashboard"):
    // la UI deve rilasciarlo da sola.
    key(&h, egui::Key::ShiftLeft, true);
    h.step();
    let premuti = h.state().live_console().map(|c| c.stats.input_events);
    h.get_by_role_and_label(Role::Button, "Dashboard").click();
    h.step();
    h.step();
    let dopo = h.state().live_console().map(|c| c.stats.input_events);
    println!("UI-REALE eventi_input prima_della_perdita_focus={premuti:?} dopo={dopo:?}");
    assert!(
        dopo > premuti,
        "nessun rilascio inviato alla perdita di focus"
    );
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
            "screenshot non prodotto"
        );
        std::thread::sleep(Duration::from_millis(50));
        h.step();
    }
    std::thread::sleep(Duration::from_millis(200));
    let s = h.state().live_console().map(|c| c.stats.clone());
    println!("UI-REALE screenshot={} stats={s:?}", shot.display());
}
