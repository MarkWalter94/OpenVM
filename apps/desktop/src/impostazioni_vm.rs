//! Impostazioni di una VM (ADR-0017): risorse (vCPU, RAM, disco) e limiti di
//! esecuzione (tetto CPU, margine di memoria, budget di storage). Si
//! modificano solo a VM spenta; il disco si può solo ingrandire. Nessun campo
//! di testo: cursori senza riquadro del valore e menu di scelta. I limiti sono
//! applicati dal supervisor, la UI li propone soltanto.

use lightvm_proto::{
    MAX_MEMORY_MARGIN_MIB, MIN_CPU_MAX_PERCENT, MIN_MEMORY_MARGIN_MIB, VmConfig, VmId, VmLimits,
};

/// Memoria guest minima proposta (il core rifiuta valori minori).
pub const RAM_MIN_MIB: u64 = 512;
/// Memoria guest massima proposta dal cursore; il core applica comunque i
/// limiti dell'host.
pub const RAM_MAX_MIB: u64 = 256 * 1024;
/// Disco massimo proposto dal cursore, in GiB.
pub const DISCO_MAX_GIB: u64 = 4096;
/// Margini di memoria proposti, in MiB.
pub const MARGINI_MIB: [u32; 8] = [256, 512, 768, 1024, 1536, 2048, 4096, 8192];
/// Budget di storage proposti, in GiB (oltre a "nessuno").
pub const BUDGET_GIB: [u64; 8] = [5, 10, 20, 50, 100, 200, 500, 1000];

/// Modifica in corso di una VM.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModificaVm {
    /// VM.
    pub vm_id: VmId,
    /// Revisione su cui si basa la modifica.
    pub revisione: u64,
    /// Configurazione al momento dell'apertura.
    pub originale: VmConfig,
    /// Configurazione modificata.
    pub config: VmConfig,
}

impl ModificaVm {
    /// Nuova modifica a partire dalla configurazione corrente.
    pub fn nuova(vm_id: VmId, revisione: u64, config: VmConfig) -> Self {
        Self {
            vm_id,
            revisione,
            originale: config.clone(),
            config,
        }
    }
}

/// Esito del pannello.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EsitoModifica {
    /// Salvare la configurazione modificata.
    Salva,
    /// Chiudere senza salvare.
    Annulla,
}

/// Modifiche richieste da riga di comando (`--apri-vm` con le opzioni delle
/// risorse e dei limiti); i campi assenti restano invariati.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ModificheVm {
    /// vCPU.
    pub vcpus: Option<u32>,
    /// RAM in MiB.
    pub memory_mib: Option<u64>,
    /// Disco in GiB.
    pub disk_gib: Option<u64>,
    /// Tetto CPU in percentuale.
    pub cpu_max_percent: Option<u8>,
    /// Margine di memoria in MiB.
    pub memory_margin_mib: Option<u32>,
    /// Budget di storage: `Some(None)` lo rimuove.
    pub storage_budget_gib: Option<Option<u64>>,
}

impl ModificheVm {
    /// Nessuna modifica richiesta.
    pub fn vuote(&self) -> bool {
        *self == Self::default()
    }

    /// Applica le modifiche a una configurazione.
    pub fn applica(&self, c: &mut VmConfig) {
        if let Some(v) = self.vcpus {
            c.vcpus = v;
        }
        if let Some(v) = self.memory_mib {
            c.memory_mib = v;
        }
        if let Some(v) = self.disk_gib {
            c.disk_gib = v;
        }
        if let Some(v) = self.cpu_max_percent {
            c.limits.cpu_max_percent = v;
        }
        if let Some(v) = self.memory_margin_mib {
            c.limits.memory_margin_mib = v;
        }
        if let Some(v) = self.storage_budget_gib {
            c.limits.storage_budget_gib = v;
        }
    }
}

/// Configurazione predefinita di una VM creata da riga di comando: Ubuntu LTS,
/// 2 vCPU, 2048 MiB, disco 16 GiB, profilo offline, UEFI, limiti predefiniti.
pub fn config_predefinita(nome: &str) -> Result<VmConfig, String> {
    Ok(VmConfig {
        name: lightvm_proto::VmName::parse(nome)
            .map_err(|e| format!("nome di VM non valido \"{nome}\": {e}"))?,
        guest_os: lightvm_proto::GuestOs::UbuntuLtsX86_64,
        vcpus: 2,
        memory_mib: 2048,
        disk_gib: 16,
        network: lightvm_proto::NetworkProfile::Offline,
        install_media: None,
        data_media: None,
        firmware: lightvm_proto::Firmware::Uefi,
        limits: VmLimits::default(),
    })
}

fn riga(ui: &mut egui::Ui, nome: &str, valore: String) {
    ui.label(nome);
    ui.label(valore);
}

/// Pannello delle impostazioni della VM. `spenta`: la VM è ferma e
/// modificabile; `max_vcpu`: processori logici dell'host.
pub fn impostazioni_vm_ui(
    ui: &mut egui::Ui,
    m: &mut ModificaVm,
    spenta: bool,
    max_vcpu: u32,
) -> Option<EsitoModifica> {
    let mut esito = None;
    ui.label(
        egui::RichText::new(format!("Impostazioni di {}", m.originale.name.as_str())).strong(),
    );
    if !spenta {
        ui.colored_label(
            ui.visuals().warn_fg_color,
            "Spegnere la VM per modificare risorse e limiti.",
        );
    }
    let disco_min = m.originale.disk_gib;
    ui.add_enabled_ui(spenta, |ui| {
        egui::Grid::new("lightvm_impostazioni_vm")
            .num_columns(3)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                let c = &mut m.config;
                riga(ui, "vCPU", c.vcpus.to_string());
                ui.add(
                    egui::Slider::new(&mut c.vcpus, 1..=max_vcpu.max(1))
                        .show_value(false)
                        .text("vCPU"),
                );
                ui.end_row();

                riga(ui, "Memoria (RAM)", format!("{} MiB", c.memory_mib));
                ui.add(
                    egui::Slider::new(&mut c.memory_mib, RAM_MIN_MIB..=RAM_MAX_MIB)
                        .step_by(256.0)
                        .logarithmic(true)
                        .show_value(false)
                        .text("Memoria"),
                );
                ui.end_row();

                riga(ui, "Disco", format!("{} GiB", c.disk_gib));
                ui.add(
                    egui::Slider::new(&mut c.disk_gib, disco_min..=DISCO_MAX_GIB.max(disco_min))
                        .logarithmic(true)
                        .show_value(false)
                        .text("Disco"),
                )
                .on_hover_text("Il disco si può solo ingrandire");
                ui.end_row();

                let l = &mut c.limits;
                riga(
                    ui,
                    "CPU massima",
                    if l.cpu_max_percent >= 100 {
                        "nessun tetto (100 %)".into()
                    } else {
                        format!("{} % dei vCPU", l.cpu_max_percent)
                    },
                );
                ui.add(
                    egui::Slider::new(&mut l.cpu_max_percent, MIN_CPU_MAX_PERCENT..=100)
                        .step_by(5.0)
                        .show_value(false)
                        .text("CPU massima"),
                );
                ui.end_row();

                riga(
                    ui,
                    "Margine di memoria",
                    format!("{} MiB oltre la RAM", l.memory_margin_mib),
                );
                egui::ComboBox::from_id_salt("lightvm_margine_memoria")
                    .selected_text(format!("{} MiB", l.memory_margin_mib))
                    .show_ui(ui, |ui| {
                        for v in MARGINI_MIB {
                            if (MIN_MEMORY_MARGIN_MIB..=MAX_MEMORY_MARGIN_MIB).contains(&v) {
                                ui.selectable_value(
                                    &mut l.memory_margin_mib,
                                    v,
                                    format!("{v} MiB"),
                                );
                            }
                        }
                    });
                ui.end_row();

                let budget =
                    |b: Option<u64>| b.map_or_else(|| "nessuno".to_owned(), |g| format!("{g} GiB"));
                riga(ui, "Budget di storage", budget(l.storage_budget_gib));
                egui::ComboBox::from_id_salt("lightvm_budget_storage")
                    .selected_text(budget(l.storage_budget_gib))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut l.storage_budget_gib, None, "nessuno");
                        for g in BUDGET_GIB {
                            ui.selectable_value(
                                &mut l.storage_budget_gib,
                                Some(g),
                                format!("{g} GiB"),
                            );
                        }
                    });
                ui.end_row();
            });
    });
    ui.horizontal(|ui| {
        let cambiata = m.config != m.originale;
        if ui
            .add_enabled(spenta && cambiata, egui::Button::new("Salva impostazioni"))
            .clicked()
        {
            esito = Some(EsitoModifica::Salva);
        }
        if ui.button("Chiudi").clicked() {
            esito = Some(EsitoModifica::Annulla);
        }
        if ui
            .add_enabled(spenta, egui::Button::new("Limiti predefiniti"))
            .clicked()
        {
            m.config.limits = VmLimits::default();
        }
    });
    esito
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> VmConfig {
        VmConfig {
            name: lightvm_proto::VmName::parse("prova").unwrap(),
            guest_os: lightvm_proto::GuestOs::UbuntuLtsX86_64,
            vcpus: 2,
            memory_mib: 2048,
            disk_gib: 16,
            network: lightvm_proto::NetworkProfile::Offline,
            install_media: None,
            data_media: None,
            firmware: lightvm_proto::Firmware::Uefi,
            limits: VmLimits::default(),
        }
    }

    #[test]
    fn modifiche_da_riga_di_comando() {
        let mut c = cfg();
        assert!(ModificheVm::default().vuote());
        let m = ModificheVm {
            vcpus: Some(4),
            disk_gib: Some(32),
            cpu_max_percent: Some(50),
            storage_budget_gib: Some(Some(20)),
            ..ModificheVm::default()
        };
        assert!(!m.vuote());
        m.applica(&mut c);
        assert_eq!((c.vcpus, c.memory_mib, c.disk_gib), (4, 2048, 32));
        assert_eq!(c.limits.cpu_max_percent, 50);
        assert_eq!(c.limits.memory_margin_mib, 1024);
        assert_eq!(c.limits.storage_budget_gib, Some(20));
        ModificheVm {
            storage_budget_gib: Some(None),
            ..ModificheVm::default()
        }
        .applica(&mut c);
        assert_eq!(c.limits.storage_budget_gib, None);
    }
}
