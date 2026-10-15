//! VM settings panel (ADR-0017) driven headless with `egui_kittest`: no text
//! field, save only with changes and the VM stopped, budget chosen from the
//! menu, attached media (F2) removed or replaced.

mod common;

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use lightvm_desktop::service::MediaState;
use lightvm_desktop::vm_settings::{EditOutcome, VmEdit, vm_settings_ui};
use lightvm_proto::{Firmware, GuestOs, NetworkProfile, VmConfig, VmId, VmLimits, VmName};

struct Fixture {
    m: VmEdit,
    stopped: bool,
    outcomes: Vec<EditOutcome>,
    media: MediaState,
}

fn config() -> VmConfig {
    VmConfig {
        name: VmName::parse("ubuntu-test").unwrap(),
        guest_os: GuestOs::UbuntuLtsX86_64,
        vcpus: 2,
        memory_mib: 2048,
        disk_gib: 16,
        network: NetworkProfile::Offline,
        endpoints: Vec::new(),
        install_media: None,
        data_media: None,
        firmware: Firmware::Uefi,
        limits: VmLimits::default(),
        imported: None,
    }
}

fn harness_for(c: VmConfig, stopped: bool) -> Harness<'static, Fixture> {
    Harness::builder()
        .with_size(egui::vec2(900.0, 900.0))
        .build_ui_state(
            |ui, p: &mut Fixture| {
                if let Some(e) = vm_settings_ui(ui, &mut p.m, p.stopped, 16, &p.media) {
                    p.outcomes.push(e);
                }
            },
            Fixture {
                m: VmEdit::new(VmId::new_v7(), 3, c),
                stopped,
                outcomes: Vec::new(),
                media: common::connected(vec![]).media,
            },
        )
}

fn harness(stopped: bool) -> Harness<'static, Fixture> {
    harness_for(config(), stopped)
}

fn enabled(h: &Harness<'_, Fixture>, label: &str) -> bool {
    !h.get_by_role_and_label(Role::Button, label)
        .accesskit_node()
        .is_disabled()
}

#[test]
fn no_text_field_and_values_shown() {
    let mut h = harness(true);
    h.run();
    let fields = h.query_all_by_role(Role::TextInput).count()
        + h.query_all_by_role(Role::MultilineTextInput).count()
        + h.query_all_by_role(Role::SpinButton).count();
    assert_eq!(fields, 0);
    h.get_by_label("Resources and limits of ubuntu-test");
    h.get_by_label("2 vCPU");
    h.get_by_label("2048 MiB");
    h.get_by_label("16 GiB");
    h.get_by_label("no cap (100 %)");
    // The sliders have an accessible name even without text next to them.
    for name in ["Virtual processors", "Memory", "Disk", "Maximum CPU"] {
        h.get_by_role_and_label(Role::Slider, name);
    }
}

#[test]
fn save_only_with_changes_and_budget_from_menu() {
    let mut h = harness(true);
    h.run();
    assert!(!enabled(&h, "Save settings"));
    assert_eq!(h.query_all_by_role(Role::ComboBox).count(), 5);
    h.get_by_role_and_label(Role::ComboBox, "Storage budget")
        .click();
    h.run();
    h.get_by_label("20 GiB").click();
    h.run();
    assert_eq!(h.state().m.config.limits.storage_budget_gib, Some(20));
    assert!(enabled(&h, "Save settings"));
    h.get_by_role_and_label(Role::Button, "Save settings")
        .click();
    h.run();
    assert_eq!(h.state().outcomes, [EditOutcome::Save]);
    // "Default limits" brings the limits back to the initial values.
    h.get_by_role_and_label(Role::Button, "Default limits")
        .click();
    h.run();
    assert_eq!(h.state().m.config.limits, VmLimits::default());
    assert!(!enabled(&h, "Save settings"));
}

#[test]
fn running_vm_not_editable() {
    let mut h = harness(false);
    h.state_mut().m.config.vcpus = 4;
    h.run();
    h.get_by_label("Stop the VM to change resources and limits.");
    assert!(!enabled(&h, "Save settings"));
    assert!(!enabled(&h, "Default limits"));
    assert!(enabled(&h, "Close"));
    h.get_by_role_and_label(Role::Button, "Close").click();
    h.run();
    assert_eq!(h.state().outcomes, [EditOutcome::Cancel]);
}

/// Opening the panel does not change the configuration: values out of range
/// or off step (for example from the command line) stay as they are and
/// "Save settings" stays disabled until the user changes something.
#[test]
fn opening_the_panel_creates_no_changes() {
    let mut c = config();
    c.vcpus = 32; // beyond the 16 processors of the test host
    c.memory_mib = 3000; // off step (256 MiB)
    c.limits.cpu_max_percent = 33; // off step (5 %)
    let mut h = harness_for(c.clone(), true);
    h.run();
    assert_eq!(h.state().m.config, c);
    assert!(!enabled(&h, "Save settings"));
}

/// Installation media attached: "Remove" detaches it (the VM then boots from
/// its disk), the menu picks another approved one; saved with the rest.
#[test]
fn media_removed_or_replaced() {
    let mut c = config();
    c.install_media = Some(common::media_id(1));
    let mut h = harness_for(c, true);
    h.run();
    h.get_by_label("ubuntu-24.04.3-live-server-amd64.iso (3.0 GiB)");
    assert!(enabled(&h, "Remove the installation media"));
    assert!(!enabled(&h, "Remove the data media"));
    h.get_by_role_and_label(Role::Button, "Remove the installation media")
        .click();
    h.run();
    assert_eq!(h.state().m.config.install_media, None);
    assert!(enabled(&h, "Save settings"));
    h.get_by_role_and_label(Role::ComboBox, "Choose the data media")
        .click();
    h.run();
    h.get_by_label("tools.iso (3.0 GiB)").click();
    h.run();
    assert_eq!(h.state().m.config.data_media, Some(common::media_id(2)));
    h.get_by_role_and_label(Role::Button, "Save settings")
        .click();
    h.run();
    assert_eq!(h.state().outcomes, [EditOutcome::Save]);
}

#[test]
fn media_not_editable_while_running() {
    let mut c = config();
    c.install_media = Some(common::media_id(1));
    let mut h = harness_for(c, false);
    h.run();
    assert!(!enabled(&h, "Remove the installation media"));
}

/// Focuses the text field `label` and types `text` into it.
fn type_into(h: &mut Harness<'_, Fixture>, label: &str, text: &str) {
    h.get_by_label(label).click();
    h.run_steps(2);
    h.get_by_label(label).type_text(text);
    h.run_steps(2);
}

/// ADR-0033: the network mode from the menu; the endpoint allowlist needs
/// at least one valid endpoint before "Save settings" is enabled; the
/// virtual address is assigned by the panel. The endpoint fields are the
/// only text fields of the panel.
#[test]
fn network_mode_and_endpoints() {
    let mut h = harness(true);
    h.run();
    h.get_by_role_and_label(Role::ComboBox, "Network mode")
        .click();
    h.run();
    h.get_by_label("Endpoint allowlist").click();
    h.run();
    assert_eq!(h.state().m.config.network, NetworkProfile::Endpoints);
    h.get_by_label("the endpoint allowlist mode needs at least one endpoint");
    assert!(!enabled(&h, "Save settings"));
    h.get_by_role_and_label(Role::Button, "Add endpoint")
        .click();
    h.run();
    assert_eq!(h.query_all_by_role(Role::TextInput).count(), 3);
    assert!(!enabled(&h, "Save settings"), "empty endpoint");
    type_into(&mut h, "Name of endpoint 1", "sql.lan");
    h.run();
    type_into(&mut h, "Host of endpoint 1", "127.0.0.1");
    h.run();
    type_into(&mut h, "Port of endpoint 1", "1433");
    h.run();
    h.run();
    let e = &h.state().m.config.endpoints[0];
    assert_eq!(
        (e.name.as_str(), e.host.as_str(), e.port, e.virtual_ip),
        (
            "sql.lan",
            "127.0.0.1",
            1433,
            std::net::Ipv4Addr::new(10, 0, 2, 100)
        )
    );
    h.get_by_label("guest 10.0.2.100");
    assert!(enabled(&h, "Save settings"));
    // A wildcard name is refused.
    type_into(&mut h, "Name of endpoint 1", "*");
    h.run();
    h.run();
    assert!(!enabled(&h, "Save settings"));
    h.get_by_role_and_label(Role::Button, "Remove endpoint 1")
        .click();
    h.run();
    assert!(h.state().m.config.endpoints.is_empty());
}

/// Windows on ARM has no network adapter: any mode other than offline is
/// refused before saving.
#[test]
fn arm64_guests_stay_offline() {
    let mut c = config();
    c.guest_os = GuestOs::Windows11Arm64;
    c.network = NetworkProfile::Nat;
    let mut h = harness_for(c, true);
    h.state_mut().m.original.network = NetworkProfile::Offline;
    h.run();
    h.get_by_label("Windows 11 ARM64 guests can only be offline.");
    assert!(!enabled(&h, "Save settings"));
}
