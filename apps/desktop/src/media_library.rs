//! Imported media in Settings: the installation and data ISOs of the
//! registry with their size, the detected system and the VMs whose
//! configuration uses them, and their deletion, always confirmed. A medium
//! used by a VM cannot be deleted (button disabled with the reason; the
//! supervisor refuses it anyway, naming the VMs). The view executes
//! nothing: it returns the medium to delete; the dialog returns the command
//! to send.

use egui::WidgetType;
use lightvm_proto::ipc::VmSummary;
use lightvm_proto::{ErrorCode, MediaId, MediaKind, MediaRecord};

use crate::baselines::format_time;
use crate::dashboard::format_bytes;
use crate::dialogs;
use crate::service::{Failure, MediaState, RequestStatus, ServiceState};
use crate::theme::{self, Tone};

/// Names of the VMs whose configuration uses `id` (installation or data
/// media), in name order.
pub fn users(id: MediaId, vms: &[VmSummary]) -> Vec<&str> {
    let mut v: Vec<&str> = vms
        .iter()
        .filter(|vm| vm.config.install_media == Some(id) || vm.config.data_media == Some(id))
        .map(|vm| vm.name.as_str())
        .collect();
    v.sort_unstable();
    v
}

/// Names in quotes, at most three, then how many more.
fn quoted(names: &[&str]) -> String {
    let mut v: Vec<String> = names
        .iter()
        .take(3)
        .map(|n| format!("\u{201c}{n}\u{201d}"))
        .collect();
    if names.len() > 3 {
        v.push(format!("{} more", names.len() - 3));
    }
    v.join(", ")
}

/// Why a medium cannot be deleted now, if it cannot.
pub fn delete_refusal(
    id: MediaId,
    vms: &[VmSummary],
    connected: bool,
    deleting: bool,
) -> Option<String> {
    if !connected {
        return Some("The supervisor is not connected.".into());
    }
    if deleting {
        return Some("Another medium is being deleted.".into());
    }
    let u = users(id, vms);
    (!u.is_empty()).then(|| {
        format!(
            "Used by {}: remove it from the VM's Media settings, or delete the VM, first.",
            quoted(&u)
        )
    })
}

/// Plain-language explanation of a failed deletion.
pub fn error_text(f: &Failure) -> String {
    const WHAT: &str = "The medium was not deleted";
    match f.code {
        // Connection or protocol problem: the supervisor's own words.
        None => format!("{WHAT}: {}.", f.message),
        Some(ErrorCode::StateConflict) if f.reason() == Some("media_in_use") => {
            match f.message.find("the medium is used by") {
                // "the medium is used by …" → "it is used by …".
                Some(i) => format!("{WHAT}: it {}.", &f.message[i + "the medium ".len()..]),
                None => format!("{WHAT}: a VM uses it."),
            }
        }
        Some(ErrorCode::VmBusy) => format!(
            "{WHAT}: a running VM or another operation holds it open. Stop the VM and try again."
        ),
        Some(ErrorCode::NotFound) => "The medium is no longer in the registry.".into(),
        Some(_) => format!("{WHAT}: {}.", f.message),
    }
}

fn kind_label(k: MediaKind) -> &'static str {
    match k {
        MediaKind::InstallIso => "Installation",
        MediaKind::DataIso => "Data",
    }
}

/// Section of Settings: imported media, installation media first, most
/// recent first. Returns the medium whose deletion the user asked for.
pub fn media_ui(
    ui: &mut egui::Ui,
    media: &MediaState,
    vms: &[VmSummary],
    connected: bool,
    deleting: bool,
) -> Option<MediaId> {
    let mut choice = None;
    if let Some(e) = &media.error {
        let text = if e.unsupported {
            "This supervisor does not provide the media registry.".to_owned()
        } else {
            format!("The list could not be read: {}", e.message)
        };
        theme::notice(ui, &text, Tone::Danger);
        if media.records.is_empty() {
            return None;
        }
        ui.add_space(8.0);
    }
    if !media.loaded {
        ui.vertical_centered(|ui| {
            ui.add_space(14.0);
            if connected {
                ui.spinner();
                ui.label("Reading the media…");
            } else {
                ui.label("Connect the supervisor to see the imported media.");
            }
            ui.add_space(14.0);
        });
        return None;
    }
    if media.records.is_empty() {
        crate::empty_state(
            ui,
            "No imported media.",
            "ISOs you choose in the New VM wizard are copied here, read-only, and can be used by any VM.",
        );
        return None;
    }
    let mut list: Vec<&MediaRecord> = media.records.iter().collect();
    list.sort_by_key(|m| (m.kind, std::cmp::Reverse(m.approved_at)));
    for (i, m) in list.into_iter().enumerate() {
        if i > 0 {
            theme::divider(ui);
        }
        ui.push_id(m.id, |ui| {
            if media_row(ui, m, vms, connected, deleting) {
                choice = Some(m.id);
            }
        });
    }
    choice
}

/// One medium; `true` when Delete was clicked.
fn media_row(
    ui: &mut egui::Ui,
    m: &MediaRecord,
    vms: &[VmSummary],
    connected: bool,
    deleting: bool,
) -> bool {
    let p = theme::Palette::of(ui);
    let name = m.declared_origin.as_str();
    let used = users(m.id, vms);
    const ACTIONS: f32 = 90.0;
    let mut clicked = false;
    ui.horizontal(|ui| {
        ui.set_min_height(46.0);
        let left = (ui.available_width() - ACTIONS - 12.0).max(150.0);
        ui.allocate_ui_with_layout(
            egui::vec2(left, 46.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.set_width(left);
                ui.spacing_mut().item_spacing.y = 3.0;
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    ui.add(
                        egui::Label::new(egui::RichText::new(name).size(15.0).color(p.text))
                            .truncate(),
                    );
                    theme::pill(ui, kind_label(m.kind), Tone::Neutral);
                    if !used.is_empty() {
                        theme::pill(ui, "In use", Tone::Accent)
                            .on_hover_text("The configuration of a VM uses this medium");
                    }
                });
                let mut details = vec![format_bytes(m.size_bytes)];
                if let Some(d) = &m.detected_guest {
                    details.push(crate::wizard::detected_label(d));
                }
                details.push(format!("imported {}", format_time(m.approved_at)));
                details.push(if used.is_empty() {
                    "not used by any VM".to_owned()
                } else {
                    format!("used by {}", quoted(&used))
                });
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(details.join(" \u{b7} "))
                            .small()
                            .color(p.text_weak),
                    )
                    .wrap(),
                );
            },
        );
        ui.allocate_ui_with_layout(
            egui::vec2(ACTIONS, 46.0),
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                let refusal = delete_refusal(m.id, vms, connected, deleting);
                let hover = refusal.as_deref().unwrap_or(
                    "Removes this medium and its copy from the host (asks for confirmation)",
                );
                let r = ui
                    .add_enabled(
                        refusal.is_none(),
                        egui::Button::new(egui::RichText::new("Delete…").color(p.danger))
                            .min_size(egui::vec2(72.0, 0.0)),
                    )
                    .on_hover_text(hover)
                    .on_disabled_hover_text(hover);
                theme::accessible_name(&r, WidgetType::Button, &format!("Delete medium {name}"));
                clicked = r.clicked();
            },
        );
    });
    clicked
}

/// Confirmation of a media deletion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeleteDialog {
    media_id: MediaId,
    name: String,
    size: u64,
    /// The deletion was sent: waiting for the outcome.
    sent: bool,
    focus: bool,
}

impl DeleteDialog {
    /// Dialog for a medium of the list.
    pub fn new(m: &MediaRecord) -> Self {
        Self {
            media_id: m.id,
            name: m.declared_origin.as_str().to_owned(),
            size: m.size_bytes,
            sent: false,
            focus: true,
        }
    }
}

/// What the caller does after drawing the dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialogOutcome {
    /// Keep it open.
    Keep,
    /// Close it (cancelled or done).
    Close,
    /// Send the deletion of this medium.
    Delete(MediaId),
}

/// Draws the confirmation; the deletion is sent once, its outcome shown in
/// the dialog (a refusal keeps it open with the reason).
pub fn dialog_ui(ui: &egui::Ui, d: &mut DeleteDialog, state: &ServiceState) -> DialogOutcome {
    let mut error = None;
    let mut pending = false;
    if d.sent {
        match &state.delete_media {
            RequestStatus::Pending => {
                pending = true;
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(250));
            }
            RequestStatus::Done(id) if *id == d.media_id => return DialogOutcome::Close,
            RequestStatus::Failed(f) => error = Some(error_text(f)),
            _ => {}
        }
    }
    let exists = state.media.records.iter().any(|m| m.id == d.media_id);
    let refusal = if pending {
        None
    } else if !exists {
        Some("The medium is no longer in the registry.".to_owned())
    } else {
        delete_refusal(d.media_id, &state.vms, state.connected, false)
    };
    if error.is_none() {
        error.clone_from(&refusal);
    }
    let spec = dialogs::ConfirmSpec {
        title: format!("Delete \u{201c}{}\u{201d}?", d.name),
        body: format!(
            "LightVM's read-only copy of this medium ({}) is removed from the host permanently. The original file you imported it from is not touched. To use it again, import it again in the New VM wizard.",
            format_bytes(d.size)
        ),
        confirm: "Delete medium".into(),
        destructive: true,
    };
    let focus = std::mem::replace(&mut d.focus, false);
    match dialogs::confirm(
        ui,
        "lightvm_media_delete_dialog",
        &spec,
        pending,
        error.as_deref(),
        focus,
    ) {
        Some(dialogs::Choice::Cancel) => DialogOutcome::Close,
        Some(dialogs::Choice::Confirm) if refusal.is_none() => {
            d.sent = true;
            DialogOutcome::Delete(d.media_id)
        }
        _ => DialogOutcome::Keep,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_are_explained() {
        let f = Failure::coded(
            ErrorCode::StateConflict,
            "media deletion: state_conflict: the medium is used by 1 VM: \u{201c}web\u{201d}; remove it from their settings or delete them first",
        )
        .with_detail("reason", "media_in_use");
        assert_eq!(
            error_text(&f),
            "The medium was not deleted: it is used by 1 VM: \u{201c}web\u{201d}; remove it from their settings or delete them first."
        );
        let f = Failure::coded(ErrorCode::VmBusy, "x");
        assert!(error_text(&f).contains("Stop the VM"));
        let f = Failure::coded(ErrorCode::NotFound, "x");
        assert_eq!(error_text(&f), "The medium is no longer in the registry.");
        assert_eq!(
            error_text(&Failure::new("connection closed")),
            "The medium was not deleted: connection closed."
        );
    }

    #[test]
    fn names_are_quoted_and_capped() {
        assert_eq!(quoted(&["a"]), "\u{201c}a\u{201d}");
        assert_eq!(
            quoted(&["a", "b", "c", "d", "e"]),
            "\u{201c}a\u{201d}, \u{201c}b\u{201d}, \u{201c}c\u{201d}, 2 more"
        );
    }
}
