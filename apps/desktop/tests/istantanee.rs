//! F1-19 (parte UI): istantanee grafiche `egui_kittest` dei pannelli
//! principali, confrontate con i PNG in `tests/snapshots/` (ADR-0022,
//! criterio 4).
//!
//! Rendering: wgpu headless di egui_kittest, che preferisce un adattatore di
//! tipo CPU (WARP su Windows, llvmpipe su Linux). Tolleranza in `kittest.toml`
//! (vedi `docs/verification/F1-05-desktop-egui.md`).
//!
//! Aggiornamento delle immagini, dopo una modifica voluta della UI:
//!
//! ```text
//! UPDATE_SNAPSHOTS=1 cargo test -p lightvm-desktop --test istantanee
//! ```
//!
//! Le righe dell'elenco VM e il frame della console sono dati di prova di
//! questo file, non del percorso di prodotto.

use egui_kittest::Harness;
use lightvm_desktop::console::LiveConsole;
use lightvm_desktop::impostazioni::Impostazioni;
use lightvm_desktop::impostazioni_vm::{ModificaVm, impostazioni_vm_ui};
use lightvm_desktop::keymap::KeyboardLayout;
use lightvm_desktop::render::{InfoRendering, Tentativo};
use lightvm_desktop::servizio::StatoServizio;
use lightvm_desktop::{
    LightVmApp, Section, UiTheme, VmRiga, barra_di_stato_ui, elenco_vm_ui, servizio_ui, tema,
};
use lightvm_proto::ipc::{ParentLossPolicy, VmSummary};
use lightvm_proto::{VmId, VmName, VmState};

/// Le istantanee si rendono una alla volta: con più dispositivi wgpu creati
/// in parallelo su WARP il processo di test è terminato con
/// `STATUS_ACCESS_VIOLATION` (osservato in F1-05, causa non indagata).
fn seriale() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn righe_di_prova() -> Vec<VmRiga> {
    vec![
        VmRiga {
            nome: "ubuntu-prova".into(),
            os: "Ubuntu 24.04 LTS".into(),
            stato: "In esecuzione".into(),
            attiva: true,
        },
        VmRiga {
            nome: "ubuntu-baseline".into(),
            os: "Ubuntu 24.04 LTS".into(),
            stato: "Spenta".into(),
            attiva: false,
        },
    ]
}

/// Frame deterministico: gradiente con griglia ogni 32 pixel.
fn frame_sintetico() -> egui::ColorImage {
    let (w, h) = (640usize, 400usize);
    let mut px = Vec::with_capacity(w * h);
    for y in 0..h {
        for x in 0..w {
            px.push(if x % 32 == 0 || y % 32 == 0 {
                egui::Color32::from_gray(200)
            } else {
                egui::Color32::from_rgb((x * 255 / w) as u8, (y * 255 / h) as u8, 96)
            });
        }
    }
    egui::ColorImage::new([w, h], px)
}

fn vm(nome: &str, state: VmState, vcpus: u32, memory_mib: u64) -> VmSummary {
    VmSummary {
        vm_id: VmId::new_v7(),
        name: VmName::parse(nome).unwrap(),
        state,
        run_id: None,
        config: lightvm_proto::VmConfig {
            name: VmName::parse(nome).unwrap(),
            guest_os: lightvm_proto::GuestOs::UbuntuLtsX86_64,
            vcpus,
            memory_mib,
            disk_gib: 32,
            network: lightvm_proto::NetworkProfile::Offline,
            install_media: None,
            data_media: None,
            firmware: lightvm_proto::Firmware::Uefi,
            limits: Default::default(),
        },
        config_revision: 1,
    }
}

fn stato_connesso() -> StatoServizio {
    StatoServizio {
        connessione: "connesso".into(),
        connesso: true,
        backend: Some("qemu".into()),
        acceleratore: Some("whpx".into()),
        vms: vec![
            vm("ubuntu-dev", VmState::Running, 4, 8192),
            vm("ubuntu-baseline", VmState::Stopped, 2, 2048),
            vm("build-runner", VmState::Starting, 2, 4096),
        ],
        ultimo_esito: Some("avvio di ubuntu-dev completato".into()),
        politica: Some(ParentLossPolicy::KillVms),
    }
}

fn info_warp_ripiego() -> InfoRendering {
    InfoRendering {
        attivo: Some(Tentativo::WgpuSoftware),
        dettaglio: "wgpu backend=Dx12 adapter=\"Microsoft Basic Render Driver\" tipo=Cpu".into(),
        adattatore_cpu: true,
        falliti: vec![
            (Tentativo::Glow, "egui_glow requires opengl 2.0+".into()),
            (
                Tentativo::WgpuHardware,
                "nessun adattatore wgpu hardware disponibile".into(),
            ),
        ],
        forzato: false,
    }
}

#[test]
fn istantanea_elenco_vm() {
    let _seriale = seriale();
    let righe = righe_di_prova();
    let mut h = Harness::builder()
        .with_size(egui::vec2(640.0, 190.0))
        .build_ui(|ui| elenco_vm_ui(ui, &righe));
    tema::installa(&h.ctx);
    h.run();
    h.snapshot("elenco_vm");
}

#[test]
fn istantanea_elenco_vm_vuoto() {
    let _seriale = seriale();
    let mut h = Harness::builder()
        .with_size(egui::vec2(640.0, 150.0))
        .build_ui(|ui| elenco_vm_ui(ui, &[]));
    tema::installa(&h.ctx);
    h.run();
    h.snapshot("elenco_vm_vuoto");
}

#[test]
fn istantanea_barra_di_stato_degradata() {
    let _seriale = seriale();
    let info = info_warp_ripiego();
    let mut h = Harness::builder()
        .with_size(egui::vec2(900.0, 28.0))
        .build_ui(|ui| barra_di_stato_ui(ui, &info));
    tema::installa(&h.ctx);
    h.run();
    h.snapshot("barra_di_stato_degradata");
}

#[test]
fn istantanea_barra_di_stato_glow() {
    let _seriale = seriale();
    let info = InfoRendering {
        attivo: Some(Tentativo::Glow),
        dettaglio: "glow versione_gl=4.6".into(),
        adattatore_cpu: false,
        falliti: Vec::new(),
        forzato: false,
    };
    let mut h = Harness::builder()
        .with_size(egui::vec2(900.0, 28.0))
        .build_ui(|ui| barra_di_stato_ui(ui, &info));
    tema::installa(&h.ctx);
    h.run();
    h.snapshot("barra_di_stato_glow");
}

#[test]
fn istantanea_console_frame_sintetico() {
    let _seriale = seriale();
    let (console, _rx) = LiveConsole::sintetica(KeyboardLayout::It, frame_sintetico());
    let mut app = LightVmApp::with_live_console(console);
    app.set_section(Section::Console);
    app.rendering = info_warp_ripiego();
    let mut h = Harness::builder()
        .with_size(egui::vec2(1100.0, 700.0))
        .build_ui_state(|ui, app: &mut LightVmApp| app.ui(ui), app);
    h.run();
    h.snapshot("console_frame_sintetico");
}

#[test]
fn istantanea_dashboard_con_elenco() {
    let _seriale = seriale();
    let mut app = LightVmApp::new(false);
    app.set_elenco_vm(righe_di_prova());
    app.set_section(Section::Dashboard);
    let mut h = Harness::builder()
        .with_size(egui::vec2(1100.0, 420.0))
        .build_ui_state(|ui, app: &mut LightVmApp| app.ui(ui), app);
    h.run();
    h.snapshot("dashboard_elenco_vm");
}

#[test]
fn istantanea_dashboard_con_supervisor() {
    let _seriale = seriale();
    let stato = stato_connesso();
    let mut h = Harness::builder()
        .with_size(egui::vec2(980.0, 470.0))
        .build_ui(|ui| {
            servizio_ui(ui, &stato, 4242, true);
        });
    tema::installa(&h.ctx);
    h.run();
    h.snapshot("dashboard_supervisor");
}

#[test]
fn istantanea_impostazioni_vm() {
    let _seriale = seriale();
    let v = vm("ubuntu-baseline", VmState::Stopped, 2, 2048);
    let mut m = ModificaVm::nuova(v.vm_id, 1, v.config);
    let mut h = Harness::builder()
        .with_size(egui::vec2(980.0, 520.0))
        .build_ui(|ui| {
            impostazioni_vm_ui(ui, &mut m, true, 16);
        });
    tema::installa(&h.ctx);
    h.run();
    h.snapshot("impostazioni_vm");
}

#[test]
fn istantanea_settings() {
    let _seriale = seriale();
    let mut app = LightVmApp::new(false);
    app.set_section(Section::Settings);
    // Percorso fisso: quello predefinito dipende dal profilo dell'utente.
    let imp = Impostazioni {
        catalogo: Some(r"D:\LightVM\catalogo".into()),
        ..Impostazioni::default()
    };
    app.set_impostazioni(imp, None, None);
    app.rendering = InfoRendering {
        attivo: Some(Tentativo::Glow),
        dettaglio: "glow versione_gl=4.6".into(),
        adattatore_cpu: false,
        falliti: Vec::new(),
        forzato: false,
    };
    let mut h = Harness::builder()
        .with_size(egui::vec2(1100.0, 1420.0))
        .build_ui_state(|ui, app: &mut LightVmApp| app.ui(ui), app);
    h.run();
    h.snapshot("settings");
}

#[test]
fn istantanea_settings_tema_chiaro() {
    let _seriale = seriale();
    let mut app = LightVmApp::new(false);
    app.set_section(Section::Settings);
    // Percorso fisso: quello predefinito dipende dal profilo dell'utente.
    let imp = Impostazioni {
        catalogo: Some(r"D:\LightVM\catalogo".into()),
        ..Impostazioni::default()
    };
    app.set_impostazioni(imp, None, None);
    app.set_theme(UiTheme::Light);
    let mut h = Harness::builder()
        .with_size(egui::vec2(1100.0, 700.0))
        .build_ui_state(|ui, app: &mut LightVmApp| app.ui(ui), app);
    h.run();
    h.snapshot("settings_chiaro");
}
