//! Impostazioni di una VM (ADR-0017): risorse (vCPU, RAM, disco) e limiti di
//! esecuzione (tetto CPU, margine di memoria, budget di storage). Si
//! modificano solo a VM spenta; il disco si può solo ingrandire. Nessun campo
//! di testo: cursori senza riquadro del valore e menu di scelta. I limiti sono
//! applicati dal supervisor, la UI li propone soltanto.

use lightvm_proto::{
    MAX_MEMORY_MARGIN_MIB, MIN_CPU_MAX_PERCENT, MIN_MEMORY_MARGIN_MIB, VmConfig, VmId, VmLimits,
};

use egui::WidgetType;

use crate::tema::{self, Tono};

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

/// Cursore senza riquadro del valore, seguito dal valore come testo.
fn cursore<T: egui::emath::Numeric>(
    ui: &mut egui::Ui,
    nome: &str,
    valore: &mut T,
    intervallo: std::ops::RangeInclusive<T>,
    regola: impl FnOnce(egui::Slider<'_>) -> egui::Slider<'_>,
    testo: impl Fn(T) -> String,
) -> egui::Response {
    let p = tema::Palette::di(ui);
    ui.horizontal(|ui| {
        // `Edits`: aprire il pannello non corregge il valore salvato (fuori
        // intervallo o fuori passo resta com'è finché l'utente non sposta il
        // cursore), quindi nessuna modifica fantasma da salvare.
        let r = ui.add(
            regola(egui::Slider::new(valore, intervallo))
                .clamping(egui::SliderClamping::Edits)
                .show_value(false),
        );
        // Nome accessibile: senza testo accanto il cursore sarebbe anonimo.
        let (abilitato, v) = (r.enabled(), valore.to_f64());
        r.widget_info(|| egui::WidgetInfo::slider(abilitato, v, nome));
        ui.label(egui::RichText::new(testo(*valore)).color(p.testo));
        r
    })
    .inner
}

/// Didascalia di un gruppo di righe dentro la scheda.
fn gruppo(ui: &mut egui::Ui, testo: &str) {
    let p = tema::Palette::di(ui);
    ui.label(egui::RichText::new(testo).small().color(p.testo_debole));
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
    tema::titolo_gruppo(
        ui,
        &format!("Risorse e limiti di {}", m.originale.name.as_str()),
    );
    tema::scheda(ui, |ui| {
        if !spenta {
            tema::avviso(
                ui,
                "Spegnere la VM per modificare risorse e limiti.",
                Tono::Attenzione,
            );
            ui.add_space(6.0);
        }
        let disco_min = m.originale.disk_gib;
        ui.add_enabled_ui(spenta, |ui| {
            let c = &mut m.config;
            gruppo(ui, "Risorse");
            tema::riga(ui, "Processori virtuali", None, |ui| {
                cursore(
                    ui,
                    "Processori virtuali",
                    &mut c.vcpus,
                    1..=max_vcpu.max(1),
                    |s| s,
                    |v| format!("{v} vCPU"),
                )
            });
            tema::riga(ui, "Memoria (RAM)", None, |ui| {
                cursore(
                    ui,
                    "Memoria",
                    &mut c.memory_mib,
                    RAM_MIN_MIB..=RAM_MAX_MIB,
                    |s| s.step_by(256.0).logarithmic(true),
                    |v| format!("{v} MiB"),
                )
            });
            tema::riga(ui, "Disco", Some("Si può solo ingrandire."), |ui| {
                cursore(
                    ui,
                    "Disco",
                    &mut c.disk_gib,
                    disco_min..=DISCO_MAX_GIB.max(disco_min),
                    |s| s.logarithmic(true),
                    |v| format!("{v} GiB"),
                )
            });
            tema::divisore(ui);
            gruppo(ui, "Limiti di esecuzione, applicati dal supervisor");
            let l = &mut c.limits;
            tema::riga(
                ui,
                "CPU massima",
                Some("Percentuale del tempo dei vCPU."),
                |ui| {
                    cursore(
                        ui,
                        "CPU massima",
                        &mut l.cpu_max_percent,
                        MIN_CPU_MAX_PERCENT..=100,
                        |s| s.step_by(5.0),
                        |v| {
                            if v >= 100 {
                                "nessun tetto (100 %)".into()
                            } else {
                                format!("{v} %")
                            }
                        },
                    )
                },
            );
            tema::riga(
                ui,
                "Margine di memoria",
                Some("Memoria concessa al processo oltre la RAM del guest."),
                |ui| {
                    let r = egui::ComboBox::from_id_salt("lightvm_margine_memoria")
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
                    tema::nome_accessibile(&r.response, WidgetType::ComboBox, "Margine di memoria");
                },
            );
            let budget =
                |b: Option<u64>| b.map_or_else(|| "nessuno".to_owned(), |g| format!("{g} GiB"));
            tema::riga(
                ui,
                "Budget di storage",
                Some("Spazio massimo occupato dai file della VM."),
                |ui| {
                    let r = egui::ComboBox::from_id_salt("lightvm_budget_storage")
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
                    tema::nome_accessibile(&r.response, WidgetType::ComboBox, "Budget di storage");
                },
            );
        });
        tema::divisore(ui);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(spenta, tema::discreto("Limiti predefiniti"))
                .clicked()
            {
                m.config.limits = VmLimits::default();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let cambiata = m.config != m.originale;
                let salva = tema::primario(ui, "Salva impostazioni");
                if ui.add_enabled(spenta && cambiata, salva).clicked() {
                    esito = Some(EsitoModifica::Salva);
                }
                if ui.button("Chiudi").clicked() {
                    esito = Some(EsitoModifica::Annulla);
                }
            });
        });
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
