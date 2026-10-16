//! "New VM" wizard (F2) in the whole app, driven headless with a stand-in
//! supervisor: the only text field of the UI, live validation, media from
//! the registry and import through the (fake) file picker, resources bounded
//! by the host, offline network only, creation.

mod common;

use common::{FakeSupervisor, connected, text_fields, vm};
use egui::accesskit::{Role, Toggled};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use lightvm_desktop::service::{Failure, RequestStatus, ServiceCommand};
use lightvm_desktop::{LightVmApp, Section};
use lightvm_proto::{Firmware, GuestArch, GuestOs, MediaKind, NetworkProfile, VmState};

/// An x86 host (every build but Apple Silicon macOS): the x86 systems.
fn x86_host(os: GuestOs, _: Option<GuestArch>) -> bool {
    !os.is_aarch64()
}

/// An Apple Silicon host (ADR-0028): Windows 11 ARM64 only.
fn arm_host(os: GuestOs, _: Option<GuestArch>) -> bool {
    os.is_aarch64()
}

/// App on an x86 host whatever the build host: the x86 flows below are
/// covered on every platform.
fn app_with(state: lightvm_desktop::service::ServiceState) -> (LightVmApp, common::Handles) {
    app_on(state, x86_host)
}

fn app_on(
    state: lightvm_desktop::service::ServiceState,
    host: lightvm_desktop::wizard::GuestSupport,
) -> (LightVmApp, common::Handles) {
    let (fake, h) = FakeSupervisor::new(state);
    let mut app = LightVmApp::new(false);
    app.set_supervisor(fake);
    app.set_guest_support(host);
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

/// Path returned by the fake file picker, in the host's own syntax.
#[cfg(windows)]
const PICKED_ISO: &str = r"C:\Users\x\Downloads\new-installer.iso";
#[cfg(not(windows))]
const PICKED_ISO: &str = "/Users/x/Downloads/new-installer.iso";

#[test]
fn full_flow_creates_the_vm() {
    let (mut app, hs) = app_with(connected(vec![vm(1, "ubuntu-dev", VmState::Stopped)]));
    let picked = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
    let p2 = picked.clone();
    app.set_file_picker(Some(Box::new(move |title: &str| {
        p2.borrow_mut().push(title.to_owned());
        Some(std::path::PathBuf::from(PICKED_ISO))
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
    // Installation media: approved list, Next only with a choice.
    h.get_by_label("Step 2 of 7: Installation media");
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
            path: PICKED_ISO.into(),
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

    // System: nothing detected in the imported media, the user chooses.
    h.get_by_label("Step 3 of 7: System");
    h.get_by_label("Not recognized: choose the system");
    // Nothing preselected: no radio on, Next disabled with the hint.
    for os in GuestOs::ALL
        .into_iter()
        .filter(|o| o.is_windows() && !o.is_aarch64())
    {
        assert!(
            !toggled(
                &h,
                if os == GuestOs::Windows11X64 {
                    "Windows 11 (64-bit)"
                } else {
                    lightvm_desktop::dashboard::os_label(os)
                }
            ),
            "{os:?}"
        );
    }
    assert!(!toggled(&h, "Ubuntu LTS (x86-64), the reference system"));
    assert!(!toggled(
        &h,
        "Other Linux (x86-64), without compatibility guarantees"
    ));
    h.get_by_label(lightvm_desktop::wizard::CHOOSE_SYSTEM_HINT);
    assert!(!enabled(&h, "Next"), "no system chosen");
    h.get_by_label("Other Linux (x86-64), without compatibility guarantees")
        .click();
    h.run_steps(4);
    h.get_by_label("virtio disk, virtio display and input");
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
    // Template of Other Linux.
    h.get_by_label("2 vCPU");
    h.get_by_label("2 GiB");
    h.get_by_label("20 GiB");
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
    h.get_by_label("UEFI");
    h.get_by_label("virtio disk, virtio display and input");
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
    assert_eq!((c.vcpus, c.memory_mib, c.disk_gib), (2, 2048, 20));
    assert_eq!(c.network, NetworkProfile::Offline);
    assert_eq!(c.firmware, Firmware::Uefi);
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
            ..Failure::default()
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
            ..Failure::default()
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
    h.get_by_label("Step 2 of 7: Installation media");
}

fn toggled(h: &Harness<'_, LightVmApp>, label: &str) -> bool {
    h.get_by_label(label).accesskit_node().toggled() == Some(Toggled::True)
}

fn win_state() -> lightvm_desktop::service::ServiceState {
    let mut s = connected(vec![]);
    s.media.records.extend([
        common::detected_media(
            5,
            "Win10_22H2_English_x64.iso",
            GuestOs::Windows10,
            Some(GuestArch::X86_64),
            Some(19045),
            Some("Windows 10 Pro"),
        ),
        common::detected_media(
            6,
            "Win10_22H2_English_x32.iso",
            GuestOs::Windows10,
            Some(GuestArch::X86),
            Some(19045),
            None,
        ),
        common::detected_media(
            7,
            "en_windows_xp_professional_with_service_pack_3_x86.iso",
            GuestOs::WindowsXp,
            Some(GuestArch::X86),
            None,
            None,
        ),
    ]);
    s
}

/// Starts the wizard and stops on the installation media step.
fn to_media(
    state: lightvm_desktop::service::ServiceState,
) -> (Harness<'static, LightVmApp>, common::Handles) {
    let (app, hs) = app_with(state);
    let mut h = harness(app);
    h.run_steps(4);
    h.get_by_role_and_label(Role::Button, "New VM").click();
    h.run_steps(4);
    h.get_by_role(Role::TextInput).type_text("win");
    h.run_steps(4);
    next(&mut h);
    h.get_by_label("Step 2 of 7: Installation media");
    (h, hs)
}

/// ADR-0027: the system step is prefilled from the detection, Windows 8.1
/// and 10 offer UEFI or Legacy BIOS, the summary shows the hardware profile.
#[test]
fn windows_media_prefills_the_system_step() {
    let (app, hs) = app_with(win_state());
    let mut h = harness(app);
    h.run_steps(4);
    h.get_by_role_and_label(Role::Button, "New VM").click();
    h.run_steps(4);
    h.get_by_role(Role::TextInput).type_text("win10");
    h.run_steps(4);
    next(&mut h);
    // The detection is visible in the media list too.
    let detail = " · Windows 10, 64-bit, build 19045 — Windows 10 Pro";
    assert!(
        h.query_by(|n| n.label().or(n.value()).is_some_and(|l| l.ends_with(detail)))
            .is_some(),
        "{:#?}",
        h.root()
    );
    h.get_by_label("Win10_22H2_English_x64.iso").click();
    h.run_steps(4);
    next(&mut h);

    h.get_by_label("Step 3 of 7: System");
    h.get_by_label("Detected: Windows 10, 64-bit, build 19045 — Windows 10 Pro");
    for l in ["Linux", "Windows"] {
        h.get_by_label(l);
    }
    for os in GuestOs::ALL
        .into_iter()
        .filter(|o| o.is_windows() && !o.is_aarch64())
    {
        h.get_by_role_and_label(
            Role::RadioButton,
            if os == GuestOs::Windows11X64 {
                "Windows 11 (64-bit)"
            } else {
                lightvm_desktop::dashboard::os_label(os)
            },
        );
    }
    // An x86 host does not offer Windows 11 ARM64.
    assert!(
        h.query_by_role_and_label(Role::RadioButton, "Windows 11 (ARM64)")
            .is_none()
    );
    h.get_by_role_and_label(
        Role::RadioButton,
        "Ubuntu LTS (x86-64), the reference system",
    );
    h.get_by_label(lightvm_desktop::wizard::X86_HOST_NOTE);
    assert!(toggled(&h, "Windows 10"));
    assert!(toggled(&h, "UEFI"), "x64 media: UEFI by default");
    assert!(!toggled(&h, "Legacy BIOS (SeaBIOS)"));
    h.get_by_label("AHCI disk, standard VGA, USB tablet");
    h.get_by_label("Legacy BIOS (SeaBIOS)").click();
    h.run_steps(4);
    assert!(toggled(&h, "Legacy BIOS (SeaBIOS)"));
    next(&mut h);
    next(&mut h);

    // Resources suggested for Windows 10.
    h.get_by_label("Step 5 of 7: Resources");
    h.get_by_label("Template: Windows 10 64-bit — 2 vCPU, 4 GiB RAM, 64 GiB disk. Minimum: 2 GiB of memory and 32 GiB of disk.");
    h.get_by_label("4 GiB");
    h.get_by_label("64 GiB");
    next(&mut h);
    next(&mut h);

    h.get_by_label("Step 7 of 7: Summary");
    h.get_by_label("Windows 10");
    h.get_by_label("Legacy BIOS (SeaBIOS)");
    h.get_by_label("AHCI disk, standard VGA, USB tablet");
    h.get_by_label("Offline");
    hs.clear();
    h.get_by_role_and_label(Role::Button, "Create").click();
    h.run_steps(4);
    let sent = hs.actions();
    let [ServiceCommand::Create(c)] = sent.as_slice() else {
        panic!("expected one Create: {sent:?}");
    };
    assert_eq!(
        (c.guest_os, c.firmware),
        (GuestOs::Windows10, Firmware::Bios)
    );
    assert_eq!((c.vcpus, c.memory_mib, c.disk_gib), (2, 4096, 64));
    assert_eq!(c.install_media, Some(common::media_id(5)));
    assert_eq!(c.network, NetworkProfile::Offline);
}

#[test]
fn windows_10_x86_defaults_to_bios_and_xp_firmware_is_fixed() {
    let (mut h, _hs) = to_media(win_state());
    h.get_by_label("Win10_22H2_English_x32.iso").click();
    h.run_steps(4);
    next(&mut h);
    h.get_by_label("Detected: Windows 10, 32-bit, build 19045");
    assert!(toggled(&h, "Legacy BIOS (SeaBIOS)"), "x86 media: BIOS");
    assert!(!toggled(&h, "UEFI"));

    // Back to the media: another detected system is applied.
    h.get_by_role_and_label(Role::Button, "Back").click();
    h.run_steps(4);
    h.get_by_label("en_windows_xp_professional_with_service_pack_3_x86.iso")
        .click();
    h.run_steps(4);
    next(&mut h);
    h.get_by_label("Detected: Windows XP, 32-bit");
    assert!(toggled(&h, "Windows XP"));
    // Single firmware: shown, not selectable.
    assert!(
        h.query_by_role_and_label(Role::RadioButton, "UEFI")
            .is_none()
    );
    assert!(
        h.query_by_role_and_label(Role::RadioButton, "Legacy BIOS (SeaBIOS)")
            .is_none()
    );
    h.get_by_label("Legacy BIOS (SeaBIOS)");
    h.get_by_label("IDE disk, standard VGA, USB tablet");
    let w = h.state().wizard().unwrap();
    assert_eq!(
        (w.os, w.firmware),
        (Some(GuestOs::WindowsXp), Firmware::Bios)
    );
    assert_eq!((w.vcpus, w.memory_mib, w.disk_gib), (1, 1024, 16));
}

/// The system suggests the resources until the user moves a slider; an
/// explicit choice of the system survives a change of media.
#[test]
fn system_change_updates_resources_until_edited() {
    let (mut h, _hs) = to_media(win_state());
    h.get_by_label("ubuntu-24.04.3-live-server-amd64.iso")
        .click();
    h.run_steps(4);
    next(&mut h);
    h.get_by_label("Not recognized: choose the system");
    h.get_by_label("Windows 7").click();
    h.run_steps(4);
    assert!(toggled(&h, "Windows 7"));
    h.get_by_label("Legacy BIOS (SeaBIOS)");
    next(&mut h);
    next(&mut h);
    h.get_by_label("Step 5 of 7: Resources");
    h.get_by_label("4 GiB");
    h.get_by_label("40 GiB");
    // The user edits the memory with the keyboard.
    h.get_by_role_and_label(Role::Slider, "Memory").focus();
    h.run_steps(2);
    h.key_press(egui::Key::ArrowRight);
    h.run_steps(4);
    let edited = h.state().wizard().unwrap().memory_mib;
    assert!(edited > 4096, "memory {edited}");
    assert!(h.state().wizard().unwrap().resources_edited);
    for _ in 0..2 {
        h.get_by_role_and_label(Role::Button, "Back").click();
        h.run_steps(4);
    }
    h.get_by_label("Windows XP").click();
    h.run_steps(4);
    let w = h.state().wizard().unwrap();
    assert_eq!(w.os, Some(GuestOs::WindowsXp));
    assert_eq!(
        (w.memory_mib, w.disk_gib),
        (edited, 40),
        "edited values kept"
    );
    // Explicit choice: a new media does not change the system.
    h.get_by_role_and_label(Role::Button, "Back").click();
    h.run_steps(4);
    h.get_by_label("Win10_22H2_English_x64.iso").click();
    h.run_steps(4);
    next(&mut h);
    h.get_by_label("Detected: Windows 10, 64-bit, build 19045 — Windows 10 Pro");
    assert!(toggled(&h, "Windows XP"));
}

/// Detected system, then back to an unrecognized media without an explicit
/// choice: the selection is cleared and Next waits for a choice.
#[test]
fn switching_to_unrecognized_media_clears_the_detected_system() {
    let (mut h, _hs) = to_media(win_state());
    h.get_by_label("Win10_22H2_English_x64.iso").click();
    h.run_steps(4);
    next(&mut h);
    assert!(toggled(&h, "Windows 10"));
    assert!(enabled(&h, "Next"));
    h.get_by_role_and_label(Role::Button, "Back").click();
    h.run_steps(4);
    h.get_by_label("ubuntu-24.04.3-live-server-amd64.iso")
        .click();
    h.run_steps(4);
    next(&mut h);
    h.get_by_label("Not recognized: choose the system");
    assert!(!toggled(&h, "Windows 10"));
    assert_eq!(h.state().wizard().unwrap().os, None);
    h.get_by_label(lightvm_desktop::wizard::CHOOSE_SYSTEM_HINT);
    assert!(!enabled(&h, "Next"));
    assert!(h.state().wizard().unwrap().config().is_none());
    h.get_by_label("Windows 7").click();
    h.run_steps(4);
    assert!(enabled(&h, "Next"));
}

/// Standard template of the system for the media architecture, the 4 GiB
/// cap of 32-bit Windows and "Reset to template" after edits.
#[test]
fn resources_start_from_the_template_and_can_be_reset() {
    let (mut h, _hs) = to_media(win_state());
    h.get_by_label("Win10_22H2_English_x32.iso").click();
    h.run_steps(4);
    next(&mut h);
    next(&mut h);
    next(&mut h);
    h.get_by_label("Step 5 of 7: Resources");
    h.get_by_label("Template: Windows 10 32-bit — 2 vCPU, 2 GiB RAM, 40 GiB disk. Minimum: 2 GiB of memory and 32 GiB of disk.");
    h.get_by_label(lightvm_desktop::wizard::MEMORY_CAP_NOTE);
    assert!(!enabled(&h, "Reset to template"), "already at the template");
    h.get_by_role_and_label(Role::Slider, "Memory").focus();
    h.run_steps(2);
    for _ in 0..40 {
        h.key_press(egui::Key::ArrowRight);
        h.run_steps(1);
    }
    h.run_steps(2);
    let w = h.state().wizard().unwrap();
    assert_eq!(w.memory_mib, 4096, "32-bit Windows: at most 4 GiB");
    assert!(w.resources_edited);
    assert!(enabled(&h, "Reset to template"));
    h.get_by_role_and_label(Role::Button, "Reset to template")
        .click();
    h.run_steps(4);
    let w = h.state().wizard().unwrap();
    assert_eq!((w.vcpus, w.memory_mib, w.disk_gib), (2, 2048, 40));
    assert!(!w.resources_edited);
    assert!(!enabled(&h, "Reset to template"));
}

fn arm_state() -> lightvm_desktop::service::ServiceState {
    let mut s = win_state();
    s.media.records.push(common::detected_media(
        8,
        "Win11_25H2_EnglishInternational_Arm64.iso",
        GuestOs::Windows11Arm64,
        Some(GuestArch::Aarch64),
        Some(26200),
        Some("Windows 11 Home"),
    ));
    s
}

/// ADR-0028: on Apple Silicon the detected Windows 11 ARM64 media prefills
/// the system step; only Windows 11 ARM64 is offered (no Linux column),
/// the firmware is UEFI with Secure Boot, the template 4 vCPU / 6 GiB /
/// 64 GiB.
#[test]
fn windows_11_arm64_media_prefills_on_apple_silicon() {
    let (app, hs) = app_on(arm_state(), arm_host);
    let mut h = harness(app);
    h.run_steps(4);
    h.get_by_role_and_label(Role::Button, "New VM").click();
    h.run_steps(4);
    h.get_by_role(Role::TextInput).type_text("win11");
    h.run_steps(4);
    next(&mut h);
    // x86 media are marked in the list.
    let x86 = " · Windows 10, 64-bit, build 19045 — Windows 10 Pro — not supported on this host";
    assert!(
        h.query_by(|n| n.label().or(n.value()).is_some_and(|l| l.ends_with(x86)))
            .is_some()
    );
    h.get_by_label("Win11_25H2_EnglishInternational_Arm64.iso")
        .click();
    h.run_steps(4);
    next(&mut h);

    h.get_by_label("Step 3 of 7: System");
    h.get_by_label("Detected: Windows 11, ARM64, build 26200 — Windows 11 Home");
    assert!(toggled(&h, "Windows 11 (ARM64)"));
    h.get_by_label("Windows");
    assert!(h.query_by_label("Linux").is_none(), "empty column hidden");
    for os in GuestOs::ALL.into_iter().filter(|o| !o.is_aarch64()) {
        assert!(
            h.query_by_role_and_label(Role::RadioButton, lightvm_desktop::dashboard::os_label(os))
                .is_none(),
            "{os:?} offered"
        );
    }
    assert!(
        h.query_by_role_and_label(
            Role::RadioButton,
            "Ubuntu LTS (x86-64), the reference system"
        )
        .is_none()
    );
    h.get_by_label(lightvm_desktop::wizard::ARM_HOST_NOTE);
    h.get_by_label("UEFI with Secure Boot");
    h.get_by_label(
        "NVMe disk, ramfb display, USB keyboard and tablet, TPM 2.0, UEFI with Secure Boot",
    );
    assert!(enabled(&h, "Next"));
    next(&mut h);
    next(&mut h);
    h.get_by_label("Step 5 of 7: Resources");
    h.get_by_label(
        "Template: Windows 11 ARM64 — 4 vCPU, 6 GiB RAM, 64 GiB disk. Minimum: 4 GiB of memory and 32 GiB of disk.",
    );
    next(&mut h);
    next(&mut h);
    h.get_by_label("Step 7 of 7: Summary");
    h.get_by_label("Windows 11");
    hs.clear();
    h.get_by_role_and_label(Role::Button, "Create").click();
    h.run_steps(4);
    let sent = hs.actions();
    let [ServiceCommand::Create(c)] = sent.as_slice() else {
        panic!("expected one Create: {sent:?}");
    };
    assert_eq!(
        (c.guest_os, c.firmware),
        (GuestOs::Windows11Arm64, Firmware::Uefi)
    );
    assert_eq!((c.vcpus, c.memory_mib, c.disk_gib), (4, 6144, 64));
    assert_eq!(c.install_media, Some(common::media_id(8)));
}

/// A detected system the host cannot run is shown but not preselected: on
/// Apple Silicon x86 Windows media, on an x86 host Windows 11 ARM64 media.
#[test]
fn media_for_the_other_architecture_is_not_preselected() {
    for (host, file, label) in [
        (
            arm_host as lightvm_desktop::wizard::GuestSupport,
            "Win10_22H2_English_x64.iso",
            "Detected: Windows 10, 64-bit, build 19045 — Windows 10 Pro — not supported on this host",
        ),
        (
            x86_host,
            "Win11_25H2_EnglishInternational_Arm64.iso",
            "Detected: Windows 11, ARM64, build 26200 — Windows 11 Home — not supported on this host",
        ),
    ] {
        let (app, _hs) = app_on(arm_state(), host);
        let mut h = harness(app);
        h.run_steps(4);
        h.get_by_role_and_label(Role::Button, "New VM").click();
        h.run_steps(4);
        h.get_by_role(Role::TextInput).type_text("other");
        h.run_steps(4);
        next(&mut h);
        h.get_by_label(file).click();
        h.run_steps(4);
        next(&mut h);
        h.get_by_label(label);
        assert_eq!(h.state().wizard().unwrap().os, None, "{file}");
        h.get_by_label(lightvm_desktop::wizard::CHOOSE_SYSTEM_HINT);
        assert!(!enabled(&h, "Next"));
    }
}

/// Without an override the wizard offers the systems of the build host.
#[test]
fn default_offer_follows_the_build_host() {
    let (fake, _h) = FakeSupervisor::new(win_state());
    let mut app = LightVmApp::new(false);
    app.set_supervisor(fake);
    let mut h = harness(app);
    h.run_steps(4);
    h.get_by_role_and_label(Role::Button, "New VM").click();
    h.run_steps(4);
    h.get_by_role(Role::TextInput).type_text("host");
    h.run_steps(4);
    next(&mut h);
    h.get_by_label("ubuntu-24.04.3-live-server-amd64.iso")
        .click();
    h.run_steps(4);
    next(&mut h);
    h.get_by_label("Step 3 of 7: System");
    let offered = |h: &Harness<'_, LightVmApp>, label: &str| {
        h.query_by_role_and_label(Role::RadioButton, label)
            .is_some()
    };
    assert_eq!(
        offered(&h, "Windows 11 (ARM64)"),
        GuestOs::Windows11Arm64.runs_on_this_host()
    );
    assert_eq!(
        offered(&h, "Ubuntu LTS (x86-64), the reference system"),
        GuestOs::UbuntuLtsX86_64.runs_on_this_host()
    );
    assert_eq!(
        offered(&h, "Windows 10"),
        GuestOs::Windows10.runs_on_this_host()
    );
}

/// A Windows host (WHPX): x86 systems, but not Windows Vista x64 (ADR-0027).
fn windows_host(os: GuestOs, arch: Option<GuestArch>) -> bool {
    !os.is_aarch64() && !(os == GuestOs::WindowsVista && arch == Some(GuestArch::X86_64))
}

fn vista_state() -> lightvm_desktop::service::ServiceState {
    let mut s = win_state();
    s.media.records.extend([
        common::detected_media(
            9,
            "vista_x64.iso",
            GuestOs::WindowsVista,
            Some(GuestArch::X86_64),
            Some(6003),
            None,
        ),
        common::detected_media(
            10,
            "vista_x86.iso",
            GuestOs::WindowsVista,
            Some(GuestArch::X86),
            Some(6003),
            None,
        ),
    ]);
    s
}

/// ADR-0027: on a Windows host Windows Vista x64 media are marked, not
/// preselected, and cannot create a Vista VM even when the user picks
/// Vista; the System step says why. The 32-bit media create it.
#[test]
fn vista_x64_media_is_refused_on_windows_hosts() {
    let (app, hs) = app_on(vista_state(), windows_host);
    let mut h = harness(app);
    h.run_steps(4);
    h.get_by_role_and_label(Role::Button, "New VM").click();
    h.run_steps(4);
    h.get_by_role(Role::TextInput).type_text("vista");
    h.run_steps(4);
    next(&mut h);
    let x64 = " · Windows Vista, 64-bit, build 6003 — not supported on this host";
    assert!(
        h.query_by(|n| n.label().or(n.value()).is_some_and(|l| l.ends_with(x64)))
            .is_some()
    );
    h.get_by_label("vista_x64.iso").click();
    h.run_steps(4);
    next(&mut h);

    h.get_by_label("Step 3 of 7: System");
    h.get_by_label("Detected: Windows Vista, 64-bit, build 6003 — not supported on this host");
    h.get_by_label(lightvm_desktop::wizard::VISTA_X64_NOT_SUPPORTED);
    assert_eq!(h.state().wizard().unwrap().os, None);
    // Vista stays offered (its 32-bit edition runs), but not from this media.
    h.get_by_role_and_label(Role::RadioButton, "Windows Vista")
        .click();
    h.run_steps(4);
    assert!(toggled(&h, "Windows Vista"));
    h.get_by_label(lightvm_desktop::wizard::VISTA_X64_NOT_SUPPORTED);
    assert!(!enabled(&h, "Next"));

    // The 32-bit media: the choice stays, nothing to say.
    h.get_by_role_and_label(Role::Button, "Back").click();
    h.run_steps(4);
    h.get_by_label("vista_x86.iso").click();
    h.run_steps(4);
    next(&mut h);
    h.get_by_label("Detected: Windows Vista, 32-bit, build 6003");
    assert!(
        h.query_by_label(lightvm_desktop::wizard::VISTA_X64_NOT_SUPPORTED)
            .is_none()
    );
    assert!(enabled(&h, "Next"));
    for _ in 0..4 {
        next(&mut h);
    }
    h.get_by_label("Step 7 of 7: Summary");
    hs.clear();
    h.get_by_role_and_label(Role::Button, "Create").click();
    h.run_steps(4);
    let sent = hs.actions();
    let [ServiceCommand::Create(c)] = sent.as_slice() else {
        panic!("expected one Create: {sent:?}");
    };
    assert_eq!(c.guest_os, GuestOs::WindowsVista);
    assert_eq!(c.install_media, Some(common::media_id(10)));
}
