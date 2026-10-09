//! F1-13: un evento `Paste` (testo degli appunti host prodotto da egui-winit
//! su Ctrl+V / Shift+Insert) non produce testo né nella console né in altri
//! widget, e la combinazione arriva al guest come tasti via QMP.
//!
//! Test headless con `egui_kittest` e console sintetica (nessun QEMU): il
//! canale QMP è sostituito da un `Receiver` locale. Non attraversa winit né il
//! sistema operativo: la lettura degli appunti dentro egui-winit avviene prima
//! dell'app e non è osservabile qui (vedi `docs/verification/F1-05-desktop-egui.md`).

use std::sync::mpsc::Receiver;

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable as _;
use f0_console::qmp::InputEvent;
use lightvm_desktop::console::{LiveConsole, QmpCmd};
use lightvm_desktop::keymap::KeyboardLayout;
use lightvm_desktop::{LightVmApp, Section};

const SEGRETO: &str = "testo-negli-appunti-host";

/// Stato di prova: l'app e un campo di testo esterno all'app, disegnato dopo
/// di essa nello stesso passaggio (rappresenta "qualsiasi altro widget").
struct Prova {
    app: LightVmApp,
    campo: String,
    disegna_app: bool,
}

fn ui_prova(ui: &mut egui::Ui, p: &mut Prova) {
    if p.disegna_app {
        p.app.ui(ui);
    }
    egui::Window::new("campo esterno")
        .fixed_pos([1000.0, 4.0])
        .show(ui.ctx(), |ui| {
            ui.add(egui::TextEdit::singleline(&mut p.campo).id_salt("campo_esterno"));
        });
}

fn frame_sintetico() -> egui::ColorImage {
    egui::ColorImage::filled([320, 200], egui::Color32::from_rgb(20, 40, 80))
}

fn harness(disegna_app: bool) -> (Harness<'static, Prova>, Receiver<QmpCmd>) {
    let (console, rx) = LiveConsole::sintetica(KeyboardLayout::It, frame_sintetico());
    let mut app = LightVmApp::with_live_console(console);
    app.set_section(Section::Console);
    let h = Harness::builder()
        .with_size(egui::vec2(1280.0, 800.0))
        .build_ui_state(
            ui_prova,
            Prova {
                app,
                campo: String::new(),
                disegna_app,
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

fn tasto(h: &Harness<'_, Prova>, k: egui::Key, pressed: bool, modifiers: egui::Modifiers) {
    h.event(egui::Event::Key {
        key: k,
        physical_key: Some(k),
        pressed,
        repeat: false,
        modifiers,
    });
}

/// Combinazione di incolla così come la produce egui-winit 0.36.2:
/// `ModifiersChanged` + pressione del modificatore (evento `Key`), poi
/// `Paste(testo)` **al posto** della pressione di `tasto`, poi rilascio di
/// `tasto` (evento `Key`) e del modificatore.
fn incolla_come_egui_winit(
    h: &mut Harness<'_, Prova>,
    modificatore: egui::Key,
    mods: egui::Modifiers,
    tasto_incolla: egui::Key,
) {
    h.event(egui::Event::ModifiersChanged(mods));
    tasto(h, modificatore, true, mods);
    h.step();
    h.event(egui::Event::Paste(SEGRETO.into()));
    h.step();
    tasto(h, tasto_incolla, false, mods);
    h.event(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
    tasto(h, modificatore, false, egui::Modifiers::NONE);
    h.step();
}

fn ctrl_v_come_egui_winit(h: &mut Harness<'_, Prova>) {
    incolla_come_egui_winit(h, egui::Key::ControlLeft, ctrl(), egui::Key::V);
}

fn solo_tasti(ev: &[InputEvent]) -> Vec<(&'static str, bool)> {
    ev.iter()
        .filter_map(|e| match e {
            InputEvent::Key { qcode, down } => Some((*qcode, *down)),
            _ => None,
        })
        .collect()
}

fn eventi_inviati(rx: &Receiver<QmpCmd>) -> Vec<InputEvent> {
    rx.try_iter()
        .flat_map(|c| match c {
            QmpCmd::Input(v) => v,
            QmpCmd::Screenshot => Vec::new(),
        })
        .collect()
}

/// Controllo: senza il filtro dell'app, lo stesso evento inserisce davvero il
/// testo in un campo con il focus. Dimostra che il test seguente non passa
/// per caso.
#[test]
fn controllo_senza_app_il_paste_inserisce_testo() {
    let (mut h, _rx) = harness(false);
    h.run();
    h.get_by_role(Role::TextInput).click();
    h.run();
    h.event(egui::Event::Paste(SEGRETO.into()));
    h.run();
    assert_eq!(h.state().campo, SEGRETO);
}

#[test]
fn paste_non_produce_testo_in_altri_widget() {
    let (mut h, rx) = harness(true);
    h.run();
    h.get_by_role(Role::TextInput).click();
    h.run();
    ctrl_v_come_egui_winit(&mut h);
    h.run();
    assert_eq!(
        h.state().campo,
        "",
        "il testo degli appunti è arrivato al campo"
    );
    assert_eq!(h.state().app.appunti_scartati(), 1);
    // La console non aveva il focus: nessun tasto inoltrato (il clic sul campo
    // può produrre solo eventi del puntatore, se cade sopra l'immagine).
    assert!(solo_tasti(&eventi_inviati(&rx)).is_empty());
}

#[test]
fn paste_con_console_a_fuoco_diventa_ctrl_v_via_qmp() {
    let (mut h, rx) = harness(true);
    h.run();
    h.get_by_label("Framebuffer della console del guest")
        .click();
    h.run();
    let _ = eventi_inviati(&rx); // movimento e clic del puntatore
    ctrl_v_come_egui_winit(&mut h);
    h.run();
    let ev = eventi_inviati(&rx);
    assert_eq!(
        solo_tasti(&ev),
        vec![("ctrl", true), ("v", true), ("v", false), ("ctrl", false)],
        "eventi QMP: {ev:?}"
    );
    assert_eq!(h.state().campo, "");
    assert_eq!(h.state().app.appunti_scartati(), 1);
    // Nessun testo verso il guest: solo tasti, mouse e pulsanti.
    assert!(!format!("{ev:?}").contains(SEGRETO));
}

#[test]
fn shift_insert_diventa_shift_insert_via_qmp() {
    let (mut h, rx) = harness(true);
    h.run();
    h.get_by_label("Framebuffer della console del guest")
        .click();
    h.run();
    let _ = eventi_inviati(&rx);
    let shift = egui::Modifiers {
        shift: true,
        ..Default::default()
    };
    incolla_come_egui_winit(&mut h, egui::Key::ShiftLeft, shift, egui::Key::Insert);
    h.run();
    assert_eq!(
        solo_tasti(&eventi_inviati(&rx)),
        vec![
            ("shift", true),
            ("insert", true),
            ("insert", false),
            ("shift", false)
        ]
    );
    assert_eq!(h.state().app.appunti_scartati(), 1);
}
