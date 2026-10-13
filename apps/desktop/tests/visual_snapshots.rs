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
    // The reference snapshots show an x86 host (the wizard lists the x86
    // systems), whatever the build host.
    app.set_guest_support(|os| !os.is_aarch64());
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
#[cfg_attr(
    not(windows),
    ignore = "WARP fallback: the status bar label names WARP only on Windows"
)]
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
#[cfg_attr(
    not(windows),
    ignore = "WARP fallback: the status bar label names WARP only on Windows"
)]
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
    let mut h = harness(app, egui::vec2(1100.0, 760.0));
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
    h.get_by_label("ubuntu-24.04.3-live-server-amd64.iso")
        .click();
    h.run();
    h.snapshot("wizard_install_media");
    next(&mut h);
    h.get_by_label("Step 3 of 7: System");
    // Not recognized: nothing preselected, Next disabled until a choice.
    h.snapshot("wizard_system_unrecognized");
    h.get_by_label("Ubuntu LTS (x86-64), the reference system")
        .click();
    h.run();
    next(&mut h);
    next(&mut h);
    h.get_by_label("Step 5 of 7: Resources");
    h.snapshot("wizard_resources");
    next(&mut h);
    h.get_by_label("Step 6 of 7: Network");
    h.snapshot("wizard_network");
    next(&mut h);
    h.get_by_label("Step 7 of 7: Summary");
    h.snapshot("wizard_summary");
}

/// ADR-0027: Windows media detected at import, firmware choice, summary
/// with the hardware profile; dark and light themes.
#[test]
fn snapshot_wizard_windows() {
    let _serial = serial();
    let mut state = catalog();
    state.media.records.push(common::detected_media(
        5,
        "Win10_22H2_English_x64.iso",
        lightvm_proto::GuestOs::Windows10,
        Some(lightvm_proto::GuestArch::X86_64),
        Some(19045),
        Some("Windows 10 Pro"),
    ));
    let (app, _hs) = app(state);
    let mut h = harness(app, egui::vec2(1100.0, 820.0));
    open_wizard(&mut h);
    h.get_by_role(Role::TextInput).type_text("win10");
    h.run();
    next(&mut h);
    h.get_by_label("Win10_22H2_English_x64.iso").click();
    h.run();
    next(&mut h);
    h.snapshot("wizard_system_windows");
    h.state_mut().set_theme(UiTheme::Light);
    h.run();
    h.snapshot("wizard_system_windows_light");
    h.state_mut().set_theme(UiTheme::Dark);
    h.run();
    for _ in 0..4 {
        next(&mut h);
    }
    h.get_by_label("Step 7 of 7: Summary");
    h.snapshot("wizard_summary_windows");
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

fn fixed_qemu_settings() -> UiSettings {
    UiSettings {
        qemu_dir: Some(std::path::PathBuf::from("/opt/qemu-11.1")),
        catalog: Some(std::path::PathBuf::from("/srv/lightvm/catalog")),
        ..UiSettings::default()
    }
}

#[test]
fn snapshot_settings() {
    let _serial = serial();
    let (mut app, _hs) = app(catalog());
    // Chosen QEMU and catalog folders: the page does not depend on what this
    // host has installed (Homebrew QEMU is found automatically on macOS) nor
    // on the user profile path.
    app.set_settings(fixed_qemu_settings(), None, None);
    app.set_section(Section::Settings);
    let mut h = harness(app, egui::vec2(1100.0, 2300.0));
    h.snapshot("settings");
}

#[test]
fn snapshot_settings_light_theme() {
    let _serial = serial();
    let (mut app, _hs) = app(catalog());
    // Chosen QEMU and catalog folders: the page does not depend on what this
    // host has installed (Homebrew QEMU is found automatically on macOS) nor
    // on the user profile path.
    app.set_settings(fixed_qemu_settings(), None, None);
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

/// F4: catalog with agent access on and, optionally, a pending request and
/// a live session (with the agent holding the input), at a fixed instant.
fn agents_app(request: bool, live: Option<bool>) -> (LightVmApp, common::Handles) {
    let mut s = catalog();
    s.last_outcome = None;
    s.agents.access = Some(true);
    s.agents.sessions_loaded = true;
    if request {
        s.agents.requests = vec![common::session_request(3, "claude-desktop")];
    }
    if let Some(input) = live {
        let mut x = common::session(
            1,
            1,
            "claude-desktop",
            lightvm_proto::SessionState::Active,
            input,
        );
        x.capabilities.insert(lightvm_proto::Capability::VmReset);
        s.agents.sessions = vec![x];
        let mut ended = common::session(
            2,
            1,
            "test-runner",
            lightvm_proto::SessionState::Terminated,
            false,
        );
        ended.termination = Some(lightvm_proto::TerminationCause::LeaseExpired);
        s.agents.sessions.push(ended);
        if input {
            s.agents.input_owners.insert(
                common::vm_id(1),
                lightvm_proto::ipc::InputOwner::Agent {
                    session_id: common::session_id(1),
                },
            );
        }
        let sid = Some(common::session_id(1));
        s.agents
            .record(common::at(-8 * 60), sid, "Session started".into());
        s.agents.record(
            common::at(-7 * 60),
            sid,
            "Keyboard and mouse handed to the agent".into(),
        );
        s.agents
            .record(common::at(-3 * 60), sid, "Session paused".into());
        s.agents
            .record(common::at(-2 * 60), sid, "Session resumed".into());
    }
    let (mut app, hs) = app(s);
    app.set_fixed_time(Some(common::now()));
    (app, hs)
}

#[test]
fn snapshot_agents_request() {
    let _serial = serial();
    let (mut app, _hs) = agents_app(true, None);
    app.set_section(Section::Agents);
    let mut h = harness(app, egui::vec2(1100.0, 900.0));
    h.snapshot("agents_request");
}

#[test]
fn snapshot_agents_request_light_theme() {
    let _serial = serial();
    let (mut app, _hs) = agents_app(true, None);
    app.set_section(Section::Agents);
    app.set_theme(UiTheme::Light);
    let mut h = harness(app, egui::vec2(1100.0, 900.0));
    h.snapshot("agents_request_light");
}

#[test]
fn snapshot_agents_approval_form() {
    let _serial = serial();
    let (mut app, _hs) = agents_app(true, None);
    app.set_section(Section::Agents);
    let mut h = harness(app, egui::vec2(1100.0, 1700.0));
    h.get_by_role_and_label(Role::Button, "Review the request of claude-desktop")
        .click_accesskit();
    h.run();
    h.get_by_label("The agent shows the code 482-913")
        .click_accesskit();
    h.run();
    h.snapshot("agents_approval_form");
}

#[test]
fn snapshot_agents_sessions() {
    let _serial = serial();
    let (mut app, _hs) = agents_app(false, Some(true));
    app.set_section(Section::Agents);
    let mut h = harness(app, egui::vec2(1100.0, 1000.0));
    h.snapshot("agents_sessions");
}

#[test]
fn snapshot_dashboard_with_agent_session() {
    let _serial = serial();
    let (app, _hs) = agents_app(false, Some(true));
    let mut h = harness(app, egui::vec2(1100.0, 640.0));
    h.snapshot("dashboard_agent_session");
}

#[test]
fn snapshot_dashboard_with_session_request() {
    let _serial = serial();
    let (app, _hs) = agents_app(true, None);
    let mut h = harness(app, egui::vec2(1100.0, 640.0));
    h.snapshot("dashboard_session_request");
}

#[test]
fn snapshot_agent_session_page() {
    let _serial = serial();
    let (mut app, hs) = agents_app(false, Some(true));
    with_reports(&hs);
    app.set_section(Section::Agents);
    let mut h = harness(app, egui::vec2(1100.0, 2700.0));
    h.get_by_role_and_label(Role::Button, "Open the agent session")
        .click_accesskit();
    h.run();
    h.snapshot("agent_session_page");
}

#[test]
fn snapshot_console_agent_input() {
    let _serial = serial();
    let (fake, _hs) =
        FakeSupervisor::new(agents_app(false, Some(true)).0.service().unwrap().state());
    let console = LiveConsole::synthetic(
        ConsoleTarget {
            vm_id: common::vm_id(1),
            vm_name: "ubuntu-dev".into(),
            run_id: common::run_id(1),
        },
        deterministic_frame(1280, 800),
    );
    let mut app = LightVmApp::with_live_console(console);
    app.set_supervisor(fake);
    app.rendering = glow_info();
    app.set_fixed_time(Some(common::now()));
    app.set_section(Section::Console);
    let mut h = harness(app, egui::vec2(1100.0, 760.0));
    h.snapshot("console_agent_input");
}

#[test]
fn snapshot_dialog_revoke() {
    let _serial = serial();
    let (mut app, _hs) = agents_app(false, Some(false));
    app.set_section(Section::Agents);
    let mut h = harness(app, egui::vec2(1100.0, 760.0));
    h.get_by_role_and_label(Role::Button, "Revoke the agent session")
        .click_accesskit();
    h.run();
    h.snapshot("dialog_revoke");
}

/// F5: reports of the catalog of `agents_app`: the live session (pending),
/// the lease-expired one (timed out, from the termination) and older ones
/// with a verifier and a user verdict; the ended one has its events and a
/// declaration.
fn with_reports(hs: &common::Handles) {
    use lightvm_proto::TerminationCause as T;
    use lightvm_proto::TestOutcome as O;
    use lightvm_proto::report::OutcomeSource as S;
    hs.update(|s| {
        s.reports.loaded = true;
        let live = common::report(1, 1, "claude-desktop", None, O::Inconclusive, S::Pending);
        let mut ended = common::report(
            2,
            1,
            "test-runner",
            Some(T::LeaseExpired),
            O::TimedOut,
            S::Termination,
        );
        ended.started_at = common::at(-50 * 60);
        ended.ended_at = Some(common::at(-31 * 60));
        ended.agent_declaration = Some(lightvm_proto::report::AgentDeclaration {
            claimed_outcome: O::Passed,
            summary: "The file was saved in the home folder.".into(),
            at: common::at(-32 * 60),
        });
        let mut verified = common::report(
            3,
            1,
            "claude-code",
            Some(T::Completed),
            O::Passed,
            S::Verifier,
        );
        verified.started_at = common::at(-26 * 3600);
        verified.ended_at = Some(common::at(-25 * 3600));
        let mut judged = common::report(4, 1, "claude-code", Some(T::Revoked), O::Failed, S::User);
        judged.started_at = common::at(-50 * 3600);
        judged.ended_at = Some(common::at(-49 * 3600));
        let mut broken = common::report(
            5,
            1,
            "test-runner",
            Some(T::VmTerminated),
            O::InfrastructureError,
            S::Termination,
        );
        broken.started_at = common::at(-74 * 3600);
        broken.ended_at = Some(common::at(-73 * 3600));
        s.reports.list = [&live, &ended, &verified, &judged, &broken]
            .iter()
            .map(|r| r.summary())
            .collect();
        s.reports.tail.insert(
            live.session_id,
            lightvm_desktop::service::EventsPage {
                total: 42,
                offset: 37,
                events: common::report_events(37)[1..].to_vec(),
            },
        );
        s.reports.events.insert(
            live.session_id,
            lightvm_desktop::service::EventsPage {
                total: 42,
                offset: 0,
                events: common::report_events(1)[..5].to_vec(),
            },
        );
        s.reports.events.insert(
            ended.session_id,
            lightvm_desktop::service::EventsPage {
                total: 42,
                offset: 0,
                events: common::report_events(1),
            },
        );
        for r in [live, ended, verified, judged, broken] {
            s.reports.headers.insert(r.session_id, r);
        }
    });
}

/// Deterministic test image (not a real guest screen) as a decoded shot.
fn test_shot(w: usize, h: usize) -> lightvm_desktop::service::ShotImage {
    let img = deterministic_frame(w, h);
    let rgba: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
    lightvm_desktop::service::ShotImage {
        sha256: lightvm_proto::Sha256Digest::from_bytes([0x3e; 32]),
        png_bytes: 245_760,
        size: [w, h],
        rgba: std::sync::Arc::new(rgba),
    }
}

#[test]
fn snapshot_agents_workspace() {
    let _serial = serial();
    let (mut app, hs) = agents_app(false, Some(true));
    with_reports(&hs);
    app.set_section(Section::Agents);
    let mut h = harness(app, egui::vec2(1100.0, 1500.0));
    h.get_by_role_and_label(Role::Button, "Show the latest screenshot of claude-desktop")
        .click_accesskit();
    h.run_steps(2);
    hs.update(|s| {
        s.reports.set_shot(
            common::session_id(1),
            2,
            lightvm_desktop::service::RequestStatus::Done(test_shot(640, 400)),
        );
    });
    h.run();
    h.snapshot("agents_workspace");
}

#[test]
fn snapshot_report_page() {
    let _serial = serial();
    let (mut app, hs) = agents_app(false, Some(true));
    with_reports(&hs);
    app.set_section(Section::Agents);
    let mut h = harness(app, egui::vec2(1100.0, 3000.0));
    h.get_by_role_and_label(
        Role::Button,
        &lightvm_desktop::reports::open_report_name("test-runner", common::at(-50 * 60)),
    )
    .click_accesskit();
    h.run();
    h.get_by_role_and_label(Role::Button, "Show screenshot 1")
        .click_accesskit();
    h.run_steps(2);
    hs.update(|s| {
        s.reports.set_shot(
            common::session_id(2),
            0,
            lightvm_desktop::service::RequestStatus::Done(test_shot(640, 400)),
        );
    });
    h.run();
    h.snapshot("report_page");
}

#[test]
fn snapshot_report_dialogs() {
    let _serial = serial();
    let (mut app, hs) = agents_app(false, Some(true));
    with_reports(&hs);
    app.set_section(Section::Agents);
    let mut h = harness(app, egui::vec2(1100.0, 900.0));
    h.get_by_role_and_label(
        Role::Button,
        &lightvm_desktop::reports::open_report_name("test-runner", common::at(-50 * 60)),
    )
    .click_accesskit();
    h.run();
    h.get_by_role_and_label(Role::Button, "Set your verdict")
        .click_accesskit();
    h.run();
    h.get_by_label("Passed: the objective was met")
        .click_accesskit();
    h.run();
    h.snapshot("dialog_report_verdict");
    h.get_by_role_and_label(Role::Button, "Cancel")
        .click_accesskit();
    h.run();
    h.get_by_role_and_label(Role::Button, "Export the report")
        .click_accesskit();
    h.run();
    h.get_by_label("Also save the 2 stored screenshots in a folder next to it")
        .click_accesskit();
    h.run();
    h.snapshot("dialog_report_export");
    h.get_by_role_and_label(Role::Button, "Cancel")
        .click_accesskit();
    h.run();
    h.get_by_role_and_label(Role::Button, "Delete the report and its screenshots")
        .click_accesskit();
    h.run();
    h.snapshot("dialog_report_delete");
}
