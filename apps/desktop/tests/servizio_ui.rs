//! Pannello del supervisor (F1-11) pilotato headless con `egui_kittest`: stato
//! mostrato, pulsanti abilitati solo quando l'azione è ammessa, comando
//! restituito al clic. Nessun processo: lo stato è costruito nel test.

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use lightvm_desktop::servizio::{Comando, StatoServizio};
use lightvm_desktop::servizio_ui;
use lightvm_proto::ipc::VmSummary;
use lightvm_proto::{VmId, VmName, VmState};

struct Prova {
    stato: StatoServizio,
    vivo: bool,
    scelte: Vec<Comando>,
}

fn vm(nome: &str, state: VmState) -> VmSummary {
    VmSummary {
        vm_id: VmId::new_v7(),
        name: VmName::parse(nome).unwrap(),
        state,
        run_id: None,
    }
}

fn connesso(vms: Vec<VmSummary>) -> StatoServizio {
    StatoServizio {
        connessione: "connesso".into(),
        connesso: true,
        backend: Some("qemu".into()),
        acceleratore: Some("whpx".into()),
        vms,
        ultimo_esito: None,
    }
}

fn harness(stato: StatoServizio, vivo: bool) -> Harness<'static, Prova> {
    Harness::builder()
        .with_size(egui::vec2(1000.0, 600.0))
        .build_ui_state(
            |ui, p: &mut Prova| {
                if let Some(c) = servizio_ui(ui, &p.stato, 4242, p.vivo) {
                    p.scelte.push(c);
                }
            },
            Prova {
                stato,
                vivo,
                scelte: Vec::new(),
            },
        )
}

fn abilitato(h: &Harness<'_, Prova>, label: &str) -> Vec<bool> {
    h.get_all_by_role_and_label(Role::Button, label)
        .map(|n| !n.accesskit_node().is_disabled())
        .collect()
}

#[test]
fn stato_e_acceleratore_sono_mostrati_come_testo() {
    let mut h = harness(connesso(vec![]), true);
    h.run();
    h.get_by_label("lightvm-service (PID 4242): connesso");
    h.get_by_label("backend: qemu");
    h.get_by_label("acceleratore: whpx");
    h.get_by_label("Nessuna VM nel catalogo del supervisor.");
}

#[test]
fn avvia_solo_se_ferma_arresta_solo_se_attiva() {
    let ferma = vm("ferma", VmState::Stopped);
    let attiva = vm("attiva", VmState::Running);
    let in_avvio = vm("in-avvio", VmState::Starting);
    let mut h = harness(
        connesso(vec![ferma.clone(), attiva.clone(), in_avvio]),
        true,
    );
    h.run();
    assert_eq!(abilitato(&h, "Avvia"), [true, false, false]);
    assert_eq!(abilitato(&h, "Arresta"), [false, true, false]);

    h.get_all_by_role_and_label(Role::Button, "Avvia")
        .next()
        .unwrap()
        .click();
    h.run();
    h.get_all_by_role_and_label(Role::Button, "Arresta")
        .nth(1)
        .unwrap()
        .click();
    h.run();
    assert_eq!(
        h.state().scelte,
        [Comando::Avvia(ferma.vm_id), Comando::Arresta(attiva.vm_id)]
    );
}

#[test]
fn emergency_stop_restituisce_il_comando() {
    let mut h = harness(connesso(vec![vm("a", VmState::Running)]), true);
    h.run();
    h.get_by_role_and_label(Role::Button, "⏹ Emergency Stop")
        .click();
    h.run();
    assert_eq!(h.state().scelte, [Comando::EmergencyStop]);
}

#[test]
fn nessuna_azione_con_servizio_terminato_o_non_connesso() {
    for (vivo, connesso_) in [(false, true), (true, false)] {
        let mut stato = connesso(vec![
            vm("ferma", VmState::Stopped),
            vm("attiva", VmState::Running),
        ]);
        stato.connesso = connesso_;
        let mut h = harness(stato, vivo);
        h.run();
        for label in ["Aggiorna", "⏹ Emergency Stop", "Avvia", "Arresta"] {
            assert!(
                abilitato(&h, label).iter().all(|a| !a),
                "{label} abilitato con vivo={vivo} connesso={connesso_}"
            );
        }
    }
    let mut h = harness(connesso(vec![]), false);
    h.run();
    h.get_by_label("lightvm-service (PID 4242) terminato");
}
