//! Test headless della UI con `egui_kittest` (ROADMAP 13: candidato framework di
//! test UI). Pilota l'interfaccia attraverso l'albero AccessKit, senza finestra,
//! senza GPU e senza supervisor: non verifica rendering nativo né IPC.

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use lightvm_desktop::{LightVmApp, Section, UiTheme};

fn harness(app: LightVmApp) -> Harness<'static, LightVmApp> {
    Harness::builder()
        .with_size(egui::vec2(1280.0, 800.0))
        .build_ui_state(|ui, app: &mut LightVmApp| app.ui(ui), app)
}

#[test]
fn albero_accesskit_contiene_le_voci_di_navigazione() {
    let mut h = harness(LightVmApp::new(false));
    h.run();
    for label in ["Dashboard", "Console", "Settings"] {
        // `get_by_role_and_label` fallisce se il nodo non esiste o non è unico.
        // Il ruolo serve a distinguere la voce di navigazione dal titolo della sezione.
        let node = h.get_by_role_and_label(Role::Button, label);
        assert_eq!(node.accesskit_node().label().as_deref(), Some(label));
    }
}

#[test]
fn click_su_dashboard_cambia_sezione() {
    let mut h = harness(LightVmApp::new(false));
    h.run();
    assert_eq!(h.state().section(), Section::Console);
    h.get_by_role_and_label(Role::Button, "Dashboard").click();
    h.run();
    assert_eq!(h.state().section(), Section::Dashboard);
}

#[test]
fn tema_scuro_predefinito_e_passaggio_al_chiaro() {
    let mut h = harness(LightVmApp::new(false));
    h.run();
    assert_eq!(h.state().theme(), UiTheme::Dark);
    assert_eq!(h.ctx.theme(), egui::Theme::Dark);
    h.get_by_role_and_label(Role::Button, "Settings").click();
    h.run();
    h.get_by_role_and_label(Role::Button, "Chiaro").click();
    h.run();
    assert_eq!(h.state().theme(), UiTheme::Light);
    assert_eq!(h.ctx.theme(), egui::Theme::Light);
}

/// Test negativo F0-04 (ROADMAP 7.4, righe "Host" e "File"): nessuna sezione
/// della UI espone campi di testo che possano finire nella riga di comando di
/// QEMU. La console reale riceve i percorsi dei socket solo da riga di comando.
#[test]
fn nessun_campo_di_testo_in_alcuna_sezione() {
    let mut h = harness(LightVmApp::new(false));
    h.run();
    for label in ["Dashboard", "Console", "Settings"] {
        h.get_by_role_and_label(Role::Button, label).click();
        h.run();
        let campi = h.query_all_by_role(Role::TextInput).count()
            + h.query_all_by_role(Role::MultilineTextInput).count()
            + h.query_all_by_role(Role::SearchInput).count()
            + h.query_all_by_role(Role::SpinButton).count();
        assert_eq!(campi, 0, "campi di input nella sezione {label}");
    }
}

/// Come sopra, con la console reale configurata (socket inesistenti: la
/// connessione fallisce e lo stato lo mostra). Anche in questo caso nessun
/// campo di testo e nessun controllo che costruisca opzioni QEMU.
#[test]
fn nessun_campo_di_testo_con_console_reale_configurata() {
    use lightvm_desktop::console::ConsoleConfig;
    use lightvm_desktop::keymap::KeyboardLayout;
    let dir = std::env::temp_dir().join("lightvm-f0-04-inesistente");
    let cfg = ConsoleConfig {
        vnc: rfb_client::Transport::Unix {
            path: dir.join("vnc.sock"),
            imp: rfb_client::UnixImpl::Socket2,
        },
        qmp_socket: dir.join("qmp.sock"),
        capture_dir: dir.clone(),
        tmp_dir: dir,
        layout: KeyboardLayout::It,
        max_fps: 30,
    };
    let mut h = harness(LightVmApp::with_console(cfg));
    for _ in 0..20 {
        h.step();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    for label in ["Console", "Dashboard", "Settings"] {
        h.get_by_role_and_label(Role::Button, label).click();
        h.run();
        let campi = h.query_all_by_role(Role::TextInput).count()
            + h.query_all_by_role(Role::MultilineTextInput).count()
            + h.query_all_by_role(Role::SearchInput).count()
            + h.query_all_by_role(Role::SpinButton).count();
        assert_eq!(campi, 0, "campi di input nella sezione {label}");
    }
    h.get_by_role_and_label(Role::Button, "Console").click();
    h.run();
    // L'unico controllo d'azione della console è "Screenshot".
    h.get_by_role_and_label(Role::Button, "Screenshot");
}
