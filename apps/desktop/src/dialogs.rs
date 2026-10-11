//! Modal dialogs (ROADMAP F2, "consistent dialogs"): confirmation of
//! destructive actions (forced stop, reset, deletion, baseline restore and
//! deletion), baseline creation with the explicit consent for an unclean
//! stop (F3) and closing the app with running VMs. Same frame, same button order (Cancel, then the
//! action), Esc and a click outside cancel, keyboard focus on Cancel when
//! the dialog opens.

use crate::theme::{self, Tone};

/// Width of the dialogs.
const WIDTH: f32 = 440.0;

/// Choice in a dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    /// Confirmed.
    Confirm,
    /// Cancelled (button, Esc or click outside).
    Cancel,
}

/// Content of a confirmation dialog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfirmSpec {
    /// Title (question).
    pub title: String,
    /// Explanation of the consequences.
    pub body: String,
    /// Text of the confirmation button.
    pub confirm: String,
    /// The action destroys data: danger style.
    pub destructive: bool,
}

fn frame(ui: &egui::Ui) -> egui::Frame {
    let p = theme::Palette::of(ui);
    egui::Frame::new()
        .fill(p.surface)
        .stroke(egui::Stroke::new(1.0, p.border))
        .corner_radius(egui::CornerRadius::same(10))
        .inner_margin(egui::Margin::same(20))
        .shadow(ui.visuals().window_shadow)
}

/// Shows a confirmation dialog. `pending`: the action was sent and its
/// outcome is awaited (buttons disabled); `error`: outcome of a failed
/// attempt; `focus_cancel`: give keyboard focus to Cancel (first frame).
pub fn confirm(
    ui: &egui::Ui,
    id: &str,
    spec: &ConfirmSpec,
    pending: bool,
    error: Option<&str>,
    focus_cancel: bool,
) -> Option<Choice> {
    confirm_with_consent(ui, id, spec, pending, error, focus_cancel, None)
}

/// As [`confirm`], with an optional consent checkbox (its text and value):
/// the action button is enabled only once it is ticked.
pub fn confirm_with_consent(
    ui: &egui::Ui,
    id: &str,
    spec: &ConfirmSpec,
    pending: bool,
    error: Option<&str>,
    focus_cancel: bool,
    mut consent: Option<(&str, &mut bool)>,
) -> Option<Choice> {
    let ctx = ui.ctx().clone();
    let mut choice = None;
    let modal = egui::Modal::new(egui::Id::new(id))
        .frame(frame(ui))
        .show(&ctx, |ui| {
            ui.set_width(WIDTH);
            let p = theme::Palette::of(ui);
            ui.label(egui::RichText::new(&spec.title).size(17.0).color(p.text));
            ui.add_space(6.0);
            ui.add(egui::Label::new(egui::RichText::new(&spec.body).color(p.text_weak)).wrap());
            if let Some((text, value)) = consent.as_mut() {
                ui.add_space(10.0);
                ui.add_enabled(!pending, egui::Checkbox::new(value, *text));
            }
            if let Some(e) = error {
                ui.add_space(8.0);
                theme::notice(ui, e, Tone::Danger);
            }
            if pending {
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Waiting for the supervisor…");
                });
            }
            ui.add_space(16.0);
            let consented = consent.as_ref().is_none_or(|(_, v)| **v);
            buttons(
                ui,
                &spec.confirm,
                spec.destructive,
                !pending,
                consented,
                focus_cancel,
                &mut choice,
            );
        });
    if choice.is_none() && !pending && modal.should_close() {
        choice = Some(Choice::Cancel);
    }
    choice
}

fn buttons(
    ui: &mut egui::Ui,
    confirm: &str,
    destructive: bool,
    enabled: bool,
    confirm_enabled: bool,
    focus_cancel: bool,
    choice: &mut Option<Choice>,
) {
    ui.horizontal(|ui| {
        let size = egui::vec2(96.0, 0.0);
        let spacing = ui.spacing().item_spacing.x;
        let confirm_w = (confirm.len() as f32 * 7.5 + 28.0).max(size.x);
        ui.add_space((ui.available_width() - size.x - confirm_w - spacing).max(0.0));
        let cancel = ui.add_enabled(enabled, egui::Button::new("Cancel").min_size(size));
        if focus_cancel {
            cancel.request_focus();
        }
        if cancel.clicked() {
            *choice = Some(Choice::Cancel);
        }
        let b = if destructive {
            theme::danger(ui, confirm)
        } else {
            theme::primary(ui, confirm)
        };
        if ui
            .add_enabled(
                enabled && confirm_enabled,
                b.min_size(egui::vec2(confirm_w, 0.0)),
            )
            .clicked()
        {
            *choice = Some(Choice::Confirm);
        }
    });
}

/// Phase of the app close with running VMs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CloseView {
    /// Question: stop them and quit, or cancel.
    Ask {
        /// Names of the running VMs.
        names: Vec<String>,
    },
    /// Orderly stop in progress.
    Stopping {
        /// VMs and their state, readable.
        vms: Vec<(String, String)>,
        /// VMs already stopped.
        stopped: usize,
        /// Seconds since the start.
        elapsed_s: u64,
    },
    /// The supervisor is being shut down.
    Finishing,
}

/// Dialog shown when the window is closed with running VMs.
pub fn close(ui: &egui::Ui, view: &CloseView, focus_cancel: bool) -> Option<Choice> {
    let ctx = ui.ctx().clone();
    let mut choice = None;
    let modal = egui::Modal::new(egui::Id::new("lightvm_close_dialog"))
        .frame(frame(ui))
        .show(&ctx, |ui| {
            ui.set_width(WIDTH);
            let p = theme::Palette::of(ui);
            match view {
                CloseView::Ask { names } => {
                    let title = if names.len() == 1 {
                        "1 VM is running".to_owned()
                    } else {
                        format!("{} VMs are running", names.len())
                    };
                    ui.label(egui::RichText::new(title).size(17.0).color(p.text));
                    ui.add_space(6.0);
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(format!(
                                "{}. LightVM asks the guests to shut down in an orderly way, terminates the ones that do not within the supervisor's timeout, then quits.",
                                names.join(", ")
                            ))
                            .color(p.text_weak),
                        )
                        .wrap(),
                    );
                    ui.add_space(16.0);
                    buttons(ui, "Stop them and quit", false, true, true, focus_cancel, &mut choice);
                }
                CloseView::Stopping {
                    vms,
                    stopped,
                    elapsed_s,
                } => {
                    ui.label(egui::RichText::new("Stopping the VMs…").size(17.0).color(p.text));
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(format!(
                            "{stopped} of {} stopped · {elapsed_s} s",
                            vms.len()
                        ));
                    });
                    ui.add_space(4.0);
                    for (name, state) in vms {
                        ui.label(
                            egui::RichText::new(format!("{name}: {state}")).color(p.text_weak),
                        );
                    }
                }
                CloseView::Finishing => {
                    ui.label(egui::RichText::new("Closing LightVM…").size(17.0).color(p.text));
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Stopping the supervisor.");
                    });
                }
            }
        });
    if matches!(view, CloseView::Ask { .. }) && choice.is_none() && modal.should_close() {
        choice = Some(Choice::Cancel);
    }
    choice
}
