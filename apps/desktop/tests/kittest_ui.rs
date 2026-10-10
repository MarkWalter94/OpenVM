//! Headless UI tests with `egui_kittest` (ROADMAP 13: UI test framework
//! candidate). They drive the interface through the AccessKit tree, without a
//! window, without GPU and without supervisor: they verify neither native
//! rendering nor IPC.

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use lightvm_desktop::{LightVmApp, Section, UiTheme};

fn harness(app: LightVmApp) -> Harness<'static, LightVmApp> {
    harness_with_height(app, 800.0)
}

/// The Settings page scrolls: tests using all its controls render it in
/// full, because kittest does not scroll to the node.
fn harness_with_height(app: LightVmApp, height: f32) -> Harness<'static, LightVmApp> {
    Harness::builder()
        .with_size(egui::vec2(1280.0, height))
        .build_ui_state(|ui, app: &mut LightVmApp| app.ui(ui), app)
}

#[test]
fn accesskit_tree_contains_the_navigation_items() {
    let mut h = harness(LightVmApp::new(false));
    h.run();
    for label in ["Dashboard", "Console", "Settings"] {
        // `get_by_role_and_label` fails if the node does not exist or is not unique.
        // The role distinguishes the navigation item from the section title.
        let node = h.get_by_role_and_label(Role::Button, label);
        assert_eq!(node.accesskit_node().label().as_deref(), Some(label));
    }
}

#[test]
fn starts_on_dashboard_and_click_on_console_changes_section() {
    let mut h = harness(LightVmApp::new(false));
    h.run();
    assert_eq!(h.state().section(), Section::Dashboard);
    h.get_by_role_and_label(Role::Button, "Console").click();
    h.run();
    assert_eq!(h.state().section(), Section::Console);
}

/// Without a real console and without `--simulated-console` the Console page
/// shows a declared empty state: no simulated image (ROADMAP F2 exit
/// criterion: no screen with simulated data).
#[test]
fn default_console_page_shows_empty_state_and_no_image() {
    let mut h = harness(LightVmApp::new(false));
    h.run();
    h.get_by_role_and_label(Role::Button, "Console").click();
    h.run();
    h.get_by_label("No console connected.");
    assert_eq!(h.query_all_by_role(Role::Image).count(), 0);
    assert!(
        h.query_by_label("Simulated console framebuffer").is_none(),
        "simulated framebuffer shown by default"
    );
    assert_eq!(h.state().fb_updates(), 0);
}

/// With the simulated console enabled the page shows the generated
/// framebuffer (measurements only).
#[test]
fn simulated_console_is_opt_in() {
    let mut app = LightVmApp::new(false);
    app.set_simulated_console(true);
    app.set_section(Section::Console);
    let mut h = harness(app);
    h.run();
    h.get_by_label("Simulated console framebuffer");
    assert!(h.state().fb_updates() > 0);
}

#[test]
fn dark_theme_by_default_and_switch_to_light() {
    let mut h = harness(LightVmApp::new(false));
    h.run();
    assert_eq!(h.state().theme(), UiTheme::Dark);
    assert_eq!(h.ctx.theme(), egui::Theme::Dark);
    h.get_by_role_and_label(Role::Button, "Settings").click();
    h.run();
    h.get_by_role_and_label(Role::Button, "Light").click();
    h.run();
    assert_eq!(h.state().theme(), UiTheme::Light);
    assert_eq!(h.ctx.theme(), egui::Theme::Light);
    // The chosen theme is a saved setting.
    assert_eq!(h.state().settings().theme, UiTheme::Light);
}

/// Label text is not selectable: no copy to the host clipboard (ROADMAP: no
/// clipboard features).
#[test]
fn labels_not_selectable() {
    let mut h = harness(LightVmApp::new(false));
    h.run();
    for theme in [egui::Theme::Dark, egui::Theme::Light] {
        assert!(!h.ctx.style_of(theme).interaction.selectable_labels);
    }
}

/// F0-04 negative test (ROADMAP 7.4, rows "Host" and "File"): no section of
/// the UI exposes text fields that could end up in the QEMU command line
/// (the only text field, the VM name of the wizard, is tested in
/// `wizard_ui.rs`).
#[test]
fn no_text_field_in_any_section() {
    let mut h = harness(LightVmApp::new(false));
    h.run();
    for label in ["Dashboard", "Console", "Settings"] {
        h.get_by_role_and_label(Role::Button, label).click();
        h.run();
        let fields = h.query_all_by_role(Role::TextInput).count()
            + h.query_all_by_role(Role::MultilineTextInput).count()
            + h.query_all_by_role(Role::SearchInput).count()
            + h.query_all_by_role(Role::SpinButton).count();
        assert_eq!(fields, 0, "input fields in the {label} section");
    }
}

/// Settings: policy on unexpected UI close and preferred backend, chosen
/// with radio buttons and saved to file.
#[test]
fn settings_saves_policy_and_rendering() {
    use lightvm_desktop::settings::{RenderingPreference, UiSettings};
    use lightvm_proto::ipc::ParentLossPolicy;
    let dir = std::env::temp_dir().join(format!(
        "lightvm-kittest-settings-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    ));
    let file = dir.join("ui-settings.json");
    let mut app = LightVmApp::new(false);
    app.set_settings(UiSettings::default(), Some(file.clone()), None);
    let mut h = harness_with_height(app, 2400.0);
    h.run();
    h.get_by_role_and_label(Role::Button, "Settings").click();
    h.run();
    assert_eq!(
        h.state().settings().ui_loss_policy,
        ParentLossPolicy::KillVms
    );
    h.get_by_label("Orderly shutdown, then termination").click();
    h.run();
    h.get_by_label("wgpu (DX12/Vulkan) first: uses more memory")
        .click();
    h.run();
    let expected = UiSettings {
        ui_loss_policy: ParentLossPolicy::ShutdownVms { powerdown_s: 10 },
        rendering: RenderingPreference::Wgpu,
        ..UiSettings::default()
    };
    assert_eq!(h.state().settings(), &expected);
    assert_eq!(UiSettings::load(&file), (expected, None));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Settings, Supervisor section: folders chosen with the picker (fake in the
/// test), shown as non-editable text and saved; automatic start.
#[test]
fn settings_supervisor_chooses_folders_without_text_fields() {
    use lightvm_desktop::settings::UiSettings;
    use std::path::PathBuf;
    let dir = std::env::temp_dir().join(format!(
        "lightvm-kittest-supervisor-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    ));
    let file = dir.join("ui-settings.json");
    let mut app = LightVmApp::new(false);
    app.set_settings(UiSettings::default(), Some(file.clone()), None);
    let choices = std::rc::Rc::new(std::cell::RefCell::new(vec![
        PathBuf::from(r"D:\qemu-11.1.0"),
        PathBuf::from(r"D:\vm\catalog"),
    ]));
    let c2 = choices.clone();
    app.set_folder_picker(Some(Box::new(move |_title: &str| c2.borrow_mut().pop())));
    let mut h = harness_with_height(app, 2400.0);
    h.run();
    h.get_by_role_and_label(Role::Button, "Settings").click();
    h.run();
    h.get_by_label("not started");
    h.get_by_label("not set: VMs cannot be started");
    // Two visible "Browse…" buttons, distinguished by the accessible name.
    h.get_by_role_and_label(Role::Button, "Choose the catalog folder")
        .click();
    h.run();
    h.get_by_role_and_label(Role::Button, "Choose the QEMU folder")
        .click();
    h.run();
    h.get_by_label("Start the supervisor when LightVM opens")
        .click();
    h.run();
    let i = h.state().settings().clone();
    assert_eq!(i.catalog, Some(PathBuf::from(r"D:\vm\catalog")));
    assert_eq!(i.qemu_dir, Some(PathBuf::from(r"D:\qemu-11.1.0")));
    assert!(!i.start_supervisor);
    h.get_by_label(r"D:\vm\catalog");
    h.get_by_label(r"D:\qemu-11.1.0");
    assert_eq!(UiSettings::load(&file), (i, None));
    // "None" and "Default" bring back the initial values.
    h.get_by_role_and_label(Role::Button, "No QEMU distribution")
        .click();
    h.run();
    h.get_by_role_and_label(Role::Button, "Go back to the default catalog")
        .click();
    h.run();
    assert_eq!(h.state().settings().qemu_dir, None);
    assert_eq!(h.state().settings().catalog, None);
    let _ = std::fs::remove_dir_all(&dir);
}
