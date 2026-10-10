//! Pannello Impostazioni VM (ADR-0017) pilotato headless con `egui_kittest`:
//! nessun campo di testo, salvataggio solo con modifiche e VM spenta, budget
//! scelto dal menu.

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use lightvm_desktop::impostazioni_vm::{EsitoModifica, ModificaVm, impostazioni_vm_ui};
use lightvm_proto::{Firmware, GuestOs, NetworkProfile, VmConfig, VmId, VmLimits, VmName};

struct Prova {
    m: ModificaVm,
    spenta: bool,
    esiti: Vec<EsitoModifica>,
}

fn config() -> VmConfig {
    VmConfig {
        name: VmName::parse("ubuntu-test").unwrap(),
        guest_os: GuestOs::UbuntuLtsX86_64,
        vcpus: 2,
        memory_mib: 2048,
        disk_gib: 16,
        network: NetworkProfile::Offline,
        install_media: None,
        data_media: None,
        firmware: Firmware::Uefi,
        limits: VmLimits::default(),
    }
}

fn harness(spenta: bool) -> Harness<'static, Prova> {
    Harness::builder()
        .with_size(egui::vec2(900.0, 600.0))
        .build_ui_state(
            |ui, p: &mut Prova| {
                if let Some(e) = impostazioni_vm_ui(ui, &mut p.m, p.spenta, 16) {
                    p.esiti.push(e);
                }
            },
            Prova {
                m: ModificaVm::nuova(VmId::new_v7(), 3, config()),
                spenta,
                esiti: Vec::new(),
            },
        )
}

fn abilitato(h: &Harness<'_, Prova>, label: &str) -> bool {
    !h.get_by_role_and_label(Role::Button, label)
        .accesskit_node()
        .is_disabled()
}

#[test]
fn nessun_campo_di_testo_e_valori_mostrati() {
    let mut h = harness(true);
    h.run();
    let campi = h.query_all_by_role(Role::TextInput).count()
        + h.query_all_by_role(Role::MultilineTextInput).count()
        + h.query_all_by_role(Role::SpinButton).count();
    assert_eq!(campi, 0);
    h.get_by_label("Risorse e limiti di ubuntu-test");
    h.get_by_label("2 vCPU");
    h.get_by_label("2048 MiB");
    h.get_by_label("16 GiB");
    h.get_by_label("nessun tetto (100 %)");
    // I cursori hanno un nome accessibile anche senza testo accanto.
    for nome in ["Processori virtuali", "Memoria", "Disco", "CPU massima"] {
        h.get_by_role_and_label(Role::Slider, nome);
    }
}

#[test]
fn salva_solo_con_modifiche_e_budget_dal_menu() {
    let mut h = harness(true);
    h.run();
    assert!(!abilitato(&h, "Salva impostazioni"));
    assert_eq!(h.query_all_by_role(Role::ComboBox).count(), 2);
    h.get_by_role_and_label(Role::ComboBox, "Budget di storage")
        .click();
    h.run();
    h.get_by_label("20 GiB").click();
    h.run();
    assert_eq!(h.state().m.config.limits.storage_budget_gib, Some(20));
    assert!(abilitato(&h, "Salva impostazioni"));
    h.get_by_role_and_label(Role::Button, "Salva impostazioni")
        .click();
    h.run();
    assert_eq!(h.state().esiti, [EsitoModifica::Salva]);
    // "Limiti predefiniti" riporta i limiti ai valori iniziali.
    h.get_by_role_and_label(Role::Button, "Limiti predefiniti")
        .click();
    h.run();
    assert_eq!(h.state().m.config.limits, VmLimits::default());
    assert!(!abilitato(&h, "Salva impostazioni"));
}

#[test]
fn vm_accesa_non_modificabile() {
    let mut h = harness(false);
    h.state_mut().m.config.vcpus = 4;
    h.run();
    h.get_by_label("Spegnere la VM per modificare risorse e limiti.");
    assert!(!abilitato(&h, "Salva impostazioni"));
    assert!(!abilitato(&h, "Limiti predefiniti"));
    assert!(abilitato(&h, "Chiudi"));
    h.get_by_role_and_label(Role::Button, "Chiudi").click();
    h.run();
    assert_eq!(h.state().esiti, [EsitoModifica::Annulla]);
}

/// Aprire il pannello non modifica la configurazione: valori fuori
/// intervallo o fuori passo (per esempio da riga di comando) restano tali e
/// "Salva impostazioni" resta disabilitato finché l'utente non cambia nulla.
#[test]
fn aprire_il_pannello_non_crea_modifiche() {
    let mut c = config();
    c.vcpus = 32; // oltre i 16 processori dell'host di prova
    c.memory_mib = 3000; // fuori passo (256 MiB)
    c.limits.cpu_max_percent = 33; // fuori passo (5 %)
    let mut h = Harness::builder()
        .with_size(egui::vec2(900.0, 600.0))
        .build_ui_state(
            |ui, p: &mut Prova| {
                if let Some(e) = impostazioni_vm_ui(ui, &mut p.m, p.spenta, 16) {
                    p.esiti.push(e);
                }
            },
            Prova {
                m: ModificaVm::nuova(VmId::new_v7(), 3, c.clone()),
                spenta: true,
                esiti: Vec::new(),
            },
        );
    h.run();
    assert_eq!(h.state().m.config, c);
    assert!(!abilitato(&h, "Salva impostazioni"));
}
