//! "New VM" wizard (F2) in the whole app, driven headless with a stand-in
//! supervisor: the only text field of the UI, live validation, media from
//! the registry and import through the (fake) file picker, resources bounded
//! by the host, offline network only, creation.

mod common;

use common::{FakeSupervisor, connected, text_fields, vm};
use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use lightvm_desktop::service::{Failure, RequestStatus, ServiceCommand};
use lightvm_desktop::{LightVmApp, Section};
use lightvm_proto::{GuestOs, MediaKind, NetworkProfile, VmState};

fn app_with(state: lightvm_desktop::service::ServiceState) -> (LightVmApp, common::Handles) {
    let (fake, h) = FakeSupervisor::new(state);
    let mut app = LightVmApp::new(false);
    app.set_supervisor(fake);
    (app, h)
}

fn harness(app: LightVmApp) -> Harness<'static, LightVmApp> {
    Harness::builder()
        .with_size(egui::vec2(1100.0, 900.0))
        .build_ui_state(|ui, app: &mut LightVmApp| app.ui(ui), app)
}

fn enabled(h: &Harness<'_, LightVmApp>, label: &str) -> bool {
    !h.get_by_role_and_label(Role::Button, label)
        .accesskit_node()
        .is_disabled()
}

fn next(h: &mut Harness<'_, LightVmApp>) {
    h.get_by_role_and_label(Role::Button, "Next").click();
    h.run_steps(4);
}

/// ROADMAP 7.4 negative test, F2 version: in the whole UI there is exactly
/// one text field, the VM name, and only on the first step of the wizard.
#[test]
fn only_one_text_field_and_only_in_the_wizard() {
    let mut state = connected(vec![vm(1, "ubuntu-dev", VmState::Running)]);
    state.vms[0].config.install_media = Some(common::media_id(1));
    let (app, _h) = app_with(state);
    let mut h = harness(app);
    h.run_steps(4);
    for label in ["Dashboard", "Console", "Settings"] {
        h.get_by_role_and_label(Role::Button, label).click();
        h.run_steps(4);
        assert_eq!(text_fields(&h), 0, "text field in {label}");
    }
    // Edit panel and menus: none either.
    h.get_by_role_and_label(Role::Button, "Dashboard").click();
    h.run_steps(4);
    h.get_by_role_and_label(Role::Button, "Edit ubuntu-dev")
        .click();
    h.run_steps(4);
    assert_eq!(text_fields(&h), 0, "text field in the VM settings");
    h.get_by_role_and_label(Role::Button, "Close").click();
    h.run_steps(4);

    h.get_by_role_and_label(Role::Button, "New VM").click();
    h.run_steps(4);
    assert_eq!(text_fields(&h), 1);
    let field = h.get_by_role(Role::TextInput);
    assert_eq!(field.accesskit_node().label().as_deref(), Some("VM name"));
    // The name field has the keyboard focus when the wizard opens.
    assert!(field.accesskit_node().is_focused());
    field.type_text("lab-1");
    h.run_steps(4);
    next(&mut h);
    for _ in 0..6 {
        assert_eq!(text_fields(&h), 0, "text field after the first step");
        if h.query_by_role_and_label(Role::Button, "Next")
            .is_none_or(|n| n.accesskit_node().is_disabled())
        {
            break;
        }
        next(&mut h);
    }
}

#[test]
fn full_flow_creates_the_vm() {
    let (mut app, hs) = app_with(connected(vec![vm(1, "ubuntu-dev", VmState::Stopped)]));
    let picked = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
    let p2 = picked.clone();
    app.set_file_picker(Some(Box::new(move |title: &str| {
        p2.borrow_mut().push(title.to_owned());
        Some(std::path::PathBuf::from(
            r"C:\Users\x\Downloads\new-installer.iso",
        ))
    })));
    let mut h = harness(app);
    h.run_steps(4);
    h.get_by_role_and_label(Role::Button, "New VM").click();
    h.run_steps(4);
    h.get_by_label("Step 1 of 7: Name");
    assert!(!enabled(&h, "Next"), "empty name");
    assert!(!enabled(&h, "Back"));

    // Live validation: invalid characters, duplicate, then valid.
    h.get_by_role(Role::TextInput).type_text("bad/name");
    h.run_steps(4);
    h.get_by_label("Allowed characters: ASCII letters and digits, space, '-', '_', '.'.");
    assert!(!enabled(&h, "Next"));
    for _ in 0.."bad/name".len() {
        h.key_press(egui::Key::Backspace);
    }
    h.run_steps(4);
    h.get_by_role(Role::TextInput).type_text("Ubuntu-Dev");
    h.run_steps(4);
    h.get_by_label("A VM with this name already exists.");
    assert!(!enabled(&h, "Next"));
    for _ in 0.."Ubuntu-Dev".len() {
        h.key_press(egui::Key::Backspace);
    }
    h.get_by_role(Role::TextInput).type_text("lab-1");
    h.run_steps(4);
    h.get_by_label("Valid name");
    // Enter in the field goes to the next step.
    h.key_press(egui::Key::Enter);
    h.run_steps(4);
    h.get_by_label("Step 2 of 7: System");

    h.get_by_label("Other Linux (x86-64), without compatibility guarantees")
        .click();
    h.run_steps(4);
    next(&mut h);

    // Installation media: approved list, Next only with a choice.
    h.get_by_label("Step 3 of 7: Installation media");
    assert!(hs.sent().contains(&ServiceCommand::ListMedia));
    assert!(!enabled(&h, "Next"));
    h.get_by_label("ubuntu-24.04.3-live-server-amd64.iso");
    // Import through the picker: progress, then the new media is selected.
    h.get_by_role_and_label(Role::Button, "Import ISO…").click();
    h.run_steps(4);
    assert_eq!(picked.borrow().as_slice(), ["Choose the installation ISO"]);
    let import = hs
        .sent()
        .into_iter()
        .find(|c| matches!(c, ServiceCommand::ImportMedia { .. }))
        .expect("import sent");
    assert_eq!(
        import,
        ServiceCommand::ImportMedia {
            path: r"C:\Users\x\Downloads\new-installer.iso".into(),
            kind: MediaKind::InstallIso,
            origin: "new-installer.iso".into(),
        }
    );
    h.get_by_label("Importing new-installer.iso…");
    assert!(!enabled(&h, "Import ISO…"), "one import at a time");
    let new = common::media(9, MediaKind::InstallIso, "new-installer.iso");
    hs.update(|s| {
        s.media.records.push(new.clone());
        s.import = RequestStatus::Done(new.clone());
    });
    h.run_steps(4);
    assert!(h.query_by_label("Importing new-installer.iso…").is_none());
    assert!(enabled(&h, "Next"), "imported media selected");
    next(&mut h);

    // Data media: optional, "No data media" by default.
    h.get_by_label("Step 4 of 7: Data media");
    h.get_by_label("tools.iso").click();
    h.run_steps(4);
    next(&mut h);

    // Resources bounded by the host (8 CPU, 12 GiB usable, 180 GiB free).
    h.get_by_label("Step 5 of 7: Resources");
    h.get_by_label(
        "Bounded by this host: up to 8 vCPU, 12 GiB of memory and 180 GiB of disk per VM.",
    );
    for name in ["Virtual processors", "Memory", "Disk"] {
        h.get_by_role_and_label(Role::Slider, name);
    }
    h.get_by_label("2 vCPU");
    h.get_by_label("4 GiB");
    h.get_by_label("32 GiB");
    next(&mut h);

    // Network: only Offline selectable.
    h.get_by_label("Step 6 of 7: Network");
    let offline = h.get_by_label("Offline: no network adapter");
    assert!(!offline.accesskit_node().is_disabled());
    for l in [
        "Internet through a filtered NAT: not available in this version",
        "Bridged to the host network: not available in this version",
    ] {
        assert!(h.get_by_label(l).accesskit_node().is_disabled(), "{l}");
    }
    next(&mut h);

    h.get_by_label("Step 7 of 7: Summary");
    h.get_by_label("lab-1");
    h.get_by_label("Other Linux");
    h.get_by_label("new-installer.iso");
    h.get_by_label("Offline");
    hs.clear();
    h.get_by_role_and_label(Role::Button, "Create").click();
    h.run_steps(4);
    let sent = hs.actions();
    let [ServiceCommand::Create(c)] = sent.as_slice() else {
        panic!("expected one Create: {sent:?}");
    };
    assert_eq!(c.name.as_str(), "lab-1");
    assert_eq!(c.guest_os, GuestOs::OtherLinuxX86_64);
    assert_eq!(c.install_media, Some(new.id));
    assert_eq!(c.data_media, Some(common::media_id(2)));
    assert_eq!((c.vcpus, c.memory_mib, c.disk_gib), (2, 4096, 32));
    assert_eq!(c.network, NetworkProfile::Offline);
    h.get_by_label("Creating the VM and its disk…");
    assert!(!enabled(&h, "Creating…"));

    // Failure: shown, the wizard stays open and Create can be retried.
    hs.update(|s| s.create = RequestStatus::Failed(Failure::new("resource_limit: memory")));
    h.run_steps(4);
    h.get_by_label("Creation failed: resource_limit: memory");
    assert!(enabled(&h, "Create"));
    h.get_by_role_and_label(Role::Button, "Create").click();
    h.run_steps(4);
    hs.update(|s| s.create = RequestStatus::Done(common::vm_id(7)));
    h.run_steps(4);
    assert!(h.state().wizard().is_none(), "wizard closed after creation");
    assert_eq!(h.state().section(), Section::Dashboard);
    h.get_by_role_and_label(Role::Button, "New VM");
}

/// Supervisor without the F2 media operations: the wizard says so and does
/// not let the user continue without installation media.
#[test]
fn unsupported_media_registry_is_explained() {
    let mut state = connected(vec![]);
    state.media = lightvm_desktop::service::MediaState {
        loaded: false,
        records: Vec::new(),
        error: Some(Failure {
            unsupported: true,
            message: "approved media: unsupported: operation list_media not implemented yet".into(),
        }),
    };
    let (mut app, hs) = app_with(state);
    app.set_file_picker(Some(Box::new(|_t: &str| Some("D:\\x.iso".into()))));
    let mut h = harness(app);
    h.run_steps(4);
    h.get_by_role_and_label(Role::Button, "New VM").click();
    h.run_steps(4);
    h.get_by_role(Role::TextInput).type_text("x");
    h.run_steps(4);
    next(&mut h);
    next(&mut h);
    h.get_by_label("This supervisor does not provide the media registry yet.");
    assert!(!enabled(&h, "Next"));
    h.get_by_role_and_label(Role::Button, "Import ISO…").click();
    h.run_steps(4);
    hs.update(|s| {
        s.import = RequestStatus::Failed(Failure {
            unsupported: true,
            message: "import: unsupported".into(),
        });
    });
    h.run_steps(4);
    h.get_by_label("This supervisor cannot import ISO files yet.");
    // Cancel closes the wizard without creating anything.
    h.get_by_role_and_label(Role::Button, "Cancel").click();
    h.run_steps(4);
    assert!(h.state().wizard().is_none());
    assert!(
        !hs.sent()
            .iter()
            .any(|c| matches!(c, ServiceCommand::Create(_)))
    );
}

/// Keyboard only: Tab reaches the controls in reading order, Space/Enter
/// activate them.
#[test]
fn wizard_is_operable_with_the_keyboard() {
    let (app, _hs) = app_with(connected(vec![]));
    let mut h = harness(app);
    h.run_steps(4);
    h.get_by_role_and_label(Role::Button, "New VM").click();
    h.run_steps(4);
    h.get_by_role(Role::TextInput).type_text("kb");
    h.run_steps(4);
    // Field → Cancel → (Back disabled) → Next.
    let focused = |h: &Harness<'_, LightVmApp>| {
        h.query_by(|n| n.is_focused())
            .and_then(|n| n.accesskit_node().label())
    };
    h.key_press(egui::Key::Tab);
    h.run_steps(4);
    assert_eq!(focused(&h).as_deref(), Some("Cancel"));
    h.key_press(egui::Key::Tab);
    h.run_steps(4);
    assert_eq!(focused(&h).as_deref(), Some("Next"));
    h.key_press(egui::Key::Space);
    h.run_steps(4);
    h.get_by_label("Step 2 of 7: System");
}
