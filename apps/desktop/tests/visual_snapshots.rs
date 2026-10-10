//! F1-19 (UI part): `egui_kittest` graphical snapshots of the main panels,
//! compared with the PNGs in `tests/snapshots/` (ADR-0022, criterion 4).
//!
//! Rendering: egui_kittest's headless wgpu, which prefers a CPU-type adapter
//! (WARP on Windows, llvmpipe on Linux). Tolerance in `kittest.toml` (see
//! `docs/verification/F1-05-desktop-egui.md`).
//!
//! Updating the images, after an intended UI change:
//!
//! ```text
//! UPDATE_SNAPSHOTS=1 cargo test -p lightvm-desktop --test visual_snapshots
//! ```
//!
//! The VM list rows and the console frame are test data of this file, not of
//! the product path.

use egui_kittest::Harness;
use lightvm_desktop::console::LiveConsole;
use lightvm_desktop::keymap::KeyboardLayout;
use lightvm_desktop::render::{Attempt, RenderingInfo};
use lightvm_desktop::service::ServiceState;
use lightvm_desktop::settings::UiSettings;
use lightvm_desktop::vm_settings::{VmEdit, vm_settings_ui};
use lightvm_desktop::{
    LightVmApp, Section, UiTheme, VmRow, service_ui, status_bar_ui, theme, vm_list_ui,
};
use lightvm_proto::ipc::{ParentLossPolicy, VmSummary};
use lightvm_proto::{VmId, VmName, VmState};

/// Snapshots are rendered one at a time: with several wgpu devices created
/// in parallel on WARP the test process was terminated with
/// `STATUS_ACCESS_VIOLATION` (observed in F1-05, cause not investigated).
fn serial() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn test_rows() -> Vec<VmRow> {
    vec![
        VmRow {
            name: "ubuntu-test".into(),
            os: "Ubuntu 24.04 LTS".into(),
            state: "Running".into(),
            running: true,
        },
        VmRow {
            name: "ubuntu-baseline".into(),
            os: "Ubuntu 24.04 LTS".into(),
            state: "Stopped".into(),
            running: false,
        },
    ]
}

/// Deterministic frame: gradient with a grid every 32 pixels.
fn synthetic_frame() -> egui::ColorImage {
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

fn vm(name: &str, state: VmState, vcpus: u32, memory_mib: u64) -> VmSummary {
    VmSummary {
        vm_id: VmId::new_v7(),
        name: VmName::parse(name).unwrap(),
        state,
        run_id: None,
        config: lightvm_proto::VmConfig {
            name: VmName::parse(name).unwrap(),
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

fn connected_state() -> ServiceState {
    ServiceState {
        connection: "connected".into(),
        connected: true,
        backend: Some("qemu".into()),
        accelerator: Some("whpx".into()),
        vms: vec![
            vm("ubuntu-dev", VmState::Running, 4, 8192),
            vm("ubuntu-baseline", VmState::Stopped, 2, 2048),
            vm("build-runner", VmState::Starting, 2, 4096),
        ],
        last_outcome: Some("start of ubuntu-dev completed".into()),
        policy: Some(ParentLossPolicy::KillVms),
    }
}

fn warp_fallback_info() -> RenderingInfo {
    RenderingInfo {
        active: Some(Attempt::WgpuSoftware),
        detail: "wgpu backend=Dx12 adapter=\"Microsoft Basic Render Driver\" type=Cpu".into(),
        cpu_adapter: true,
        failed: vec![
            (Attempt::Glow, "egui_glow requires opengl 2.0+".into()),
            (
                Attempt::WgpuHardware,
                "no hardware wgpu adapter available".into(),
            ),
        ],
        forced: false,
    }
}

fn glow_info() -> RenderingInfo {
    RenderingInfo {
        active: Some(Attempt::Glow),
        detail: "glow gl_version=4.6".into(),
        cpu_adapter: false,
        failed: Vec::new(),
        forced: false,
    }
}

#[test]
fn snapshot_vm_list() {
    let _serial = serial();
    let rows = test_rows();
    let mut h = Harness::builder()
        .with_size(egui::vec2(640.0, 190.0))
        .build_ui(|ui| vm_list_ui(ui, &rows));
    theme::install(&h.ctx);
    h.run();
    h.snapshot("vm_list");
}

#[test]
fn snapshot_vm_list_empty() {
    let _serial = serial();
    let mut h = Harness::builder()
        .with_size(egui::vec2(640.0, 150.0))
        .build_ui(|ui| vm_list_ui(ui, &[]));
    theme::install(&h.ctx);
    h.run();
    h.snapshot("vm_list_empty");
}

#[test]
fn snapshot_status_bar_degraded() {
    let _serial = serial();
    let info = warp_fallback_info();
    let mut h = Harness::builder()
        .with_size(egui::vec2(900.0, 28.0))
        .build_ui(|ui| status_bar_ui(ui, &info));
    theme::install(&h.ctx);
    h.run();
    h.snapshot("status_bar_degraded");
}

#[test]
fn snapshot_status_bar_glow() {
    let _serial = serial();
    let info = glow_info();
    let mut h = Harness::builder()
        .with_size(egui::vec2(900.0, 28.0))
        .build_ui(|ui| status_bar_ui(ui, &info));
    theme::install(&h.ctx);
    h.run();
    h.snapshot("status_bar_glow");
}

#[test]
fn snapshot_console_synthetic_frame() {
    let _serial = serial();
    let (console, _rx) = LiveConsole::synthetic(KeyboardLayout::It, synthetic_frame());
    let mut app = LightVmApp::with_live_console(console);
    app.set_section(Section::Console);
    app.rendering = warp_fallback_info();
    let mut h = Harness::builder()
        .with_size(egui::vec2(1100.0, 700.0))
        .build_ui_state(|ui, app: &mut LightVmApp| app.ui(ui), app);
    h.run();
    h.snapshot("console_synthetic_frame");
}

#[test]
fn snapshot_console_empty() {
    let _serial = serial();
    let mut app = LightVmApp::new(false);
    app.set_section(Section::Console);
    app.rendering = glow_info();
    let mut h = Harness::builder()
        .with_size(egui::vec2(1100.0, 420.0))
        .build_ui_state(|ui, app: &mut LightVmApp| app.ui(ui), app);
    h.run();
    h.snapshot("console_empty");
}

#[test]
fn snapshot_dashboard_with_list() {
    let _serial = serial();
    let mut app = LightVmApp::new(false);
    app.set_vm_list(test_rows());
    app.set_section(Section::Dashboard);
    let mut h = Harness::builder()
        .with_size(egui::vec2(1100.0, 420.0))
        .build_ui_state(|ui, app: &mut LightVmApp| app.ui(ui), app);
    h.run();
    h.snapshot("dashboard_vm_list");
}

#[test]
fn snapshot_dashboard_with_supervisor() {
    let _serial = serial();
    let state = connected_state();
    let mut h = Harness::builder()
        .with_size(egui::vec2(980.0, 470.0))
        .build_ui(|ui| {
            service_ui(ui, &state, 4242, true);
        });
    theme::install(&h.ctx);
    h.run();
    h.snapshot("dashboard_supervisor");
}

#[test]
fn snapshot_vm_settings() {
    let _serial = serial();
    let v = vm("ubuntu-baseline", VmState::Stopped, 2, 2048);
    let mut m = VmEdit::new(v.vm_id, 1, v.config);
    let mut h = Harness::builder()
        .with_size(egui::vec2(980.0, 520.0))
        .build_ui(|ui| {
            vm_settings_ui(ui, &mut m, true, 16);
        });
    theme::install(&h.ctx);
    h.run();
    h.snapshot("vm_settings");
}

#[test]
fn snapshot_settings() {
    let _serial = serial();
    let mut app = LightVmApp::new(false);
    app.set_section(Section::Settings);
    // Fixed path: the default one depends on the user's profile.
    let s = UiSettings {
        catalog: Some(r"D:\LightVM\catalog".into()),
        ..UiSettings::default()
    };
    app.set_settings(s, None, None);
    app.rendering = glow_info();
    let mut h = Harness::builder()
        .with_size(egui::vec2(1100.0, 1420.0))
        .build_ui_state(|ui, app: &mut LightVmApp| app.ui(ui), app);
    h.run();
    h.snapshot("settings");
}

#[test]
fn snapshot_settings_light_theme() {
    let _serial = serial();
    let mut app = LightVmApp::new(false);
    app.set_section(Section::Settings);
    // Fixed path: the default one depends on the user's profile.
    let s = UiSettings {
        catalog: Some(r"D:\LightVM\catalog".into()),
        ..UiSettings::default()
    };
    app.set_settings(s, None, None);
    app.set_theme(UiTheme::Light);
    let mut h = Harness::builder()
        .with_size(egui::vec2(1100.0, 700.0))
        .build_ui_state(|ui, app: &mut LightVmApp| app.ui(ui), app);
    h.run();
    h.snapshot("settings_light");
}
