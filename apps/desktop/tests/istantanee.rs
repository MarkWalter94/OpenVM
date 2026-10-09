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
use lightvm_desktop::keymap::KeyboardLayout;
use lightvm_desktop::render::{InfoRendering, Tentativo};
use lightvm_desktop::{LightVmApp, Section, VmRiga, barra_di_stato_ui, elenco_vm_ui};

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
        .with_size(egui::vec2(560.0, 140.0))
        .build_ui(|ui| elenco_vm_ui(ui, &righe));
    h.run();
    h.snapshot("elenco_vm");
}

#[test]
fn istantanea_elenco_vm_vuoto() {
    let _seriale = seriale();
    let mut h = Harness::builder()
        .with_size(egui::vec2(560.0, 60.0))
        .build_ui(|ui| elenco_vm_ui(ui, &[]));
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
        .with_size(egui::vec2(900.0, 360.0))
        .build_ui_state(|ui, app: &mut LightVmApp| app.ui(ui), app);
    h.run();
    h.snapshot("dashboard_elenco_vm");
}
