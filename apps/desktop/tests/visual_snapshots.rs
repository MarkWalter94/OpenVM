//! F1-19 (UI part), extended in F2: `egui_kittest` graphical snapshots of
//! the main views, compared with the PNGs in `tests/snapshots/` (ADR-0022,
//! criterion 4).
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
//! The VMs, the media and the console frame are test data of this file
//! (stand-in supervisor), not of the product path.

mod common;

use common::{FakeSupervisor, connected, deterministic_frame, vm};
use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable as _;
use lightvm_desktop::console::{ConsoleTarget, LiveConsole};
use lightvm_desktop::render::{Attempt, RenderingInfo};
use lightvm_desktop::service::ServiceState;
use lightvm_desktop::settings::UiSettings;
use lightvm_desktop::vm_settings::{VmEdit, vm_settings_ui};
use lightvm_desktop::{LightVmApp, Section, UiTheme, dashboard_ui, status_bar_ui, theme};
use lightvm_proto::VmState;

/// Snapshots are rendered one at a time: with several wgpu devices created
/// in parallel on WARP the test process was terminated with
/// `STATUS_ACCESS_VIOLATION` (observed in F1-05, cause not investigated).
fn serial() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn catalog() -> ServiceState {
    let mut dev = vm(1, "ubuntu-dev", VmState::Running);
    dev.config.vcpus = 4;
    dev.config.memory_mib = 8192;
    let mut installed = vm(2, "ubuntu-baseline", VmState::Stopped);
    installed.run_id = Some(common::run_id(2));
    installed.config.install_media = Some(common::media_id(1));
    let mut runner = vm(3, "build-runner", VmState::Starting);
    runner.disk_used_bytes = None;
    let mut s = connected(vec![dev, installed, runner]);
    s.last_outcome = Some("start: succeeded".into());
    s
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

fn app(state: ServiceState) -> (LightVmApp, common::Handles) {
    let (fake, hs) = FakeSupervisor::new(state);
    let mut app = LightVmApp::new(false);
    app.set_supervisor(fake);
    app.rendering = glow_info();
    // Fixed path: the default one depends on the user's profile.
    app.set_settings(
        UiSettings {
            catalog: Some(r"D:\LightVM\catalog".into()),
            qemu_dir: Some(r"D:\LightVM\qemu-11.1.0".into()),
            ..UiSettings::default()
        },
        None,
        None,
    );
    (app, hs)
}

fn harness(app: LightVmApp, size: egui::Vec2) -> Harness<'static, LightVmApp> {
    let mut h = Harness::builder()
        .with_size(size)
        .build_ui_state(|ui, app: &mut LightVmApp| app.ui(ui), app);
    h.run();
    // No blinking text cursor in the images.
    h.ctx
        .all_styles_mut(|s| s.visuals.text_cursor.blink = false);
    h.run();
    h
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
    let (fake, _hs) = FakeSupervisor::new(connected(vec![vm(1, "ubuntu-dev", VmState::Running)]));
    let console = LiveConsole::synthetic(
        ConsoleTarget {
            vm_id: common::vm_id(1),
            vm_name: "ubuntu-dev".into(),
            run_id: common::run_id(1),
        },
        deterministic_frame(640, 400),
    );
    let mut app = LightVmApp::with_live_console(console);
    app.set_supervisor(fake);
    app.set_section(Section::Console);
    app.rendering = warp_fallback_info();
    let mut h = harness(app, egui::vec2(1100.0, 700.0));
    h.snapshot("console_synthetic_frame");
}

#[test]
fn snapshot_console_empty() {
    let _serial = serial();
    let (mut app, _hs) = app(connected(vec![vm(1, "ubuntu-dev", VmState::Stopped)]));
    app.set_section(Section::Console);
    let mut h = harness(app, egui::vec2(1100.0, 420.0));
    h.snapshot("console_empty");
}

#[test]
fn snapshot_dashboard() {
    let _serial = serial();
    let (app, _hs) = app(catalog());
    let mut h = harness(app, egui::vec2(1100.0, 640.0));
    h.snapshot("dashboard");
}

/// Smallest supported window: nothing overlaps or is cut.
#[test]
fn snapshot_dashboard_small_window() {
    let _serial = serial();
    let (app, _hs) = app(catalog());
    let mut h = harness(app, egui::vec2(900.0, 600.0));
    h.snapshot("dashboard_900x600");
}

#[test]
fn snapshot_dashboard_with_supervisor() {
    let _serial = serial();
    let state = catalog();
    let mut h = Harness::builder()
        .with_size(egui::vec2(980.0, 520.0))
        .build_ui(|ui| {
            dashboard_ui(ui, &state, 4242, true);
        });
    theme::install(&h.ctx);
    h.run();
    h.snapshot("dashboard_supervisor");
}

#[test]
fn snapshot_vm_settings() {
    let _serial = serial();
    let mut v = vm(2, "ubuntu-baseline", VmState::Stopped);
    v.config.install_media = Some(common::media_id(1));
    let media = connected(vec![]).media;
    let mut m = VmEdit::new(v.vm_id, 1, v.config);
    let mut h = Harness::builder()
        .with_size(egui::vec2(980.0, 700.0))
        .build_ui(|ui| {
            vm_settings_ui(ui, &mut m, true, 16, &media);
        });
    theme::install(&h.ctx);
    h.run();
    h.snapshot("vm_settings");
}

fn open_wizard(h: &mut Harness<'_, LightVmApp>) {
    h.get_by_role_and_label(Role::Button, "New VM").click();
    h.run();
}

fn next(h: &mut Harness<'_, LightVmApp>) {
    h.get_by_role_and_label(Role::Button, "Next").click();
    h.run();
}

#[test]
fn snapshot_wizard_steps() {
    let _serial = serial();
    let (mut app, _hs) = app(catalog());
    app.set_file_picker(Some(Box::new(|_t: &str| None)));
    let mut h = harness(app, egui::vec2(1100.0, 640.0));
    open_wizard(&mut h);
    h.get_by_role(Role::TextInput).type_text("ubuntu-dev");
    h.run();
    h.snapshot("wizard_name_invalid");
    for _ in 0.."dev".len() {
        h.key_press(egui::Key::Backspace);
    }
    h.get_by_role(Role::TextInput).type_text("lab");
    h.run();
    h.snapshot("wizard_name");
    next(&mut h);
    next(&mut h);
    h.get_by_label("ubuntu-24.04.3-live-server-amd64.iso")
        .click();
    h.run();
    h.snapshot("wizard_install_media");
    next(&mut h);
    next(&mut h);
    h.snapshot("wizard_resources");
    next(&mut h);
    h.snapshot("wizard_network");
    next(&mut h);
    h.snapshot("wizard_summary");
}

#[test]
fn snapshot_dialog_delete() {
    let _serial = serial();
    let (app, _hs) = app(catalog());
    let mut h = harness(app, egui::vec2(1100.0, 640.0));
    h.get_by_role_and_label(Role::Button, "More actions for ubuntu-baseline")
        .click();
    h.run();
    h.get_by_role_and_label(Role::Button, "Delete…").click();
    h.run();
    h.snapshot("dialog_delete");
}

#[test]
fn snapshot_dialog_close() {
    let _serial = serial();
    let (app, _hs) = app(catalog());
    let mut h = harness(app, egui::vec2(1100.0, 640.0));
    h.input_mut()
        .viewports
        .entry(egui::ViewportId::ROOT)
        .or_default()
        .events
        .push(egui::ViewportEvent::Close);
    h.step();
    h.run();
    h.snapshot("dialog_close");
}

#[test]
fn snapshot_settings() {
    let _serial = serial();
    let (mut app, _hs) = app(catalog());
    app.set_section(Section::Settings);
    let mut h = harness(app, egui::vec2(1100.0, 2300.0));
    h.snapshot("settings");
}

#[test]
fn snapshot_settings_light_theme() {
    let _serial = serial();
    let (mut app, _hs) = app(catalog());
    app.set_section(Section::Settings);
    app.set_theme(UiTheme::Light);
    let mut h = harness(app, egui::vec2(1100.0, 700.0));
    h.snapshot("settings_light");
}

#[test]
fn snapshot_dashboard_light_theme() {
    let _serial = serial();
    let (mut app, _hs) = app(catalog());
    app.set_theme(UiTheme::Light);
    let mut h = harness(app, egui::vec2(1100.0, 640.0));
    h.snapshot("dashboard_light");
}

/// F3: a stopped VM on an overlay of its first baseline, with a second,
/// not verified baseline taken on it; a running VM without baselines.
fn baselines_catalog() -> ServiceState {
    use lightvm_desktop::service::BaselineList;
    let mut lab = vm(1, "ubuntu-lab", VmState::Stopped);
    lab.run_id = Some(common::run_id(1));
    lab.baselines.current = Some(common::baseline_id(1));
    lab.baselines.count = 2;
    lab.baselines.bytes = 7 << 30;
    lab.baselines.overlay_bytes = 300 << 20;
    let mut s = connected(vec![lab, vm(2, "build-runner", VmState::Running)]);
    if let Some(h) = &mut s.host {
        h.baseline_available = true;
    }
    let mut first = common::baseline(1, 1, "Clean install");
    first.current = true;
    first.in_use = true;
    let mut second = common::baseline(2, 1, "Before the update");
    second.clean_shutdown = false;
    second.derived_from = Some(common::baseline_id(1));
    s.baselines.insert(
        common::vm_id(1),
        BaselineList {
            loaded: true,
            items: vec![first, second],
            error: None,
        },
    );
    s
}

fn baselines_app(state: ServiceState) -> LightVmApp {
    let (mut app, _hs) = app(state);
    app.set_fixed_time(Some(
        lightvm_proto::UtcTimestamp::from_unix_millis(1_791_709_920_000).unwrap(),
    ));
    app
}

fn open_baselines(h: &mut Harness<'_, LightVmApp>, run: bool) {
    h.get_by_role_and_label(Role::Button, "More actions for ubuntu-lab")
        .click();
    if run {
        h.run();
    } else {
        h.run_steps(6);
    }
    h.get_by_role_and_label(Role::Button, "Baselines of ubuntu-lab")
        .click();
    if run {
        h.run();
    } else {
        h.run_steps(6);
    }
}

#[test]
fn snapshot_dashboard_with_baselines() {
    let _serial = serial();
    let mut h = harness(
        baselines_app(baselines_catalog()),
        egui::vec2(1100.0, 560.0),
    );
    h.snapshot("dashboard_baselines");
}

#[test]
fn snapshot_baselines_page() {
    let _serial = serial();
    let mut h = harness(
        baselines_app(baselines_catalog()),
        egui::vec2(1100.0, 640.0),
    );
    open_baselines(&mut h, true);
    h.snapshot("baselines");
}

#[test]
fn snapshot_baselines_page_light_theme() {
    let _serial = serial();
    let mut app = baselines_app(baselines_catalog());
    app.set_theme(UiTheme::Light);
    let mut h = harness(app, egui::vec2(1100.0, 640.0));
    open_baselines(&mut h, true);
    h.snapshot("baselines_light");
}

#[test]
fn snapshot_baselines_empty() {
    let _serial = serial();
    let mut s = baselines_catalog();
    s.vms[0].baselines = Default::default();
    s.baselines
        .get_mut(&common::vm_id(1))
        .unwrap()
        .items
        .clear();
    let mut h = harness(baselines_app(s), egui::vec2(1100.0, 520.0));
    open_baselines(&mut h, true);
    h.snapshot("baselines_empty");
}

/// Long operation in progress: spinner, explanation, changes disabled. The
/// spinner keeps asking for frames: fixed number of steps.
#[test]
fn snapshot_baselines_creating() {
    let _serial = serial();
    let mut s = baselines_catalog();
    s.vms[0].baselines.operation = Some(lightvm_proto::ipc::StorageOperation::CreateBaseline);
    let mut h = Harness::builder()
        .with_size(egui::vec2(1100.0, 700.0))
        .build_ui_state(|ui, app: &mut LightVmApp| app.ui(ui), baselines_app(s));
    h.run_steps(4);
    h.ctx
        .all_styles_mut(|s| s.visuals.text_cursor.blink = false);
    open_baselines(&mut h, false);
    h.run_steps(4);
    h.snapshot("baselines_creating");
}

#[test]
fn snapshot_baseline_dialogs() {
    let _serial = serial();
    let (fake, hs) = FakeSupervisor::new(baselines_catalog());
    let mut app = baselines_app(baselines_catalog());
    app.set_supervisor(fake);
    let mut h = harness(app, egui::vec2(1100.0, 640.0));
    open_baselines(&mut h, true);
    h.get_by_role_and_label(Role::Button, "Create a baseline of ubuntu-lab")
        .click();
    h.run();
    h.snapshot("dialog_baseline_create");
    h.get_by_role_and_label(Role::Button, "Create baseline")
        .click();
    h.run_steps(4);
    hs.update(|s| {
        s.baseline = Some(lightvm_desktop::service::BaselineRequest {
            vm_id: common::vm_id(1),
            op: lightvm_desktop::service::BaselineOp::Create {
                label: lightvm_proto::Label::parse("Baseline 3 \u{b7} 2026-10-11 09:12 UTC")
                    .unwrap(),
                allow_unclean: false,
            },
            status: lightvm_desktop::service::RequestStatus::Failed(
                lightvm_desktop::service::Failure::coded(
                    lightvm_proto::ErrorCode::StateConflict,
                    "baseline: state_conflict: unclean",
                )
                .with_detail("reason", "unclean_shutdown"),
            ),
        });
    });
    h.run();
    h.snapshot("dialog_baseline_unclean");
    h.key_press(egui::Key::Escape);
    h.run();
    hs.update(|s| s.baseline = None);
    // The outcome notice goes away: the rows move up before the click.
    h.run();
    h.get_by_role_and_label(Role::Button, "Restore baseline Before the update")
        .click();
    h.run();
    h.snapshot("dialog_baseline_restore");
}
