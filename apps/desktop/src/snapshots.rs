//! Snapshots of a VM (ADR-0032): the VMware-style snapshot manager. A tree
//! with "you are here", a take with name and description (VM off or
//! running), a revert (VM off only: the page offers to shut the VM down
//! normally first, and to snapshot the current state before it), a delete
//! and a rename. The view executes nothing: it returns the chosen
//! [`SnapshotAction`]; the confirmations and the commands are in `lib.rs`.
//!
//! Never presented as saving memory: a snapshot holds the disk state (plus
//! the UEFI variables and the configuration), and a running snapshot is
//! like a power cut at that instant. Reverting restarts the guest from the
//! saved disk state with a normal boot.

use std::time::Duration;

use egui::WidgetType;
use lightvm_proto::ipc::{SnapshotSummary, SnapshotTree, StorageOperation, VmSummary};
use lightvm_proto::{ErrorCode, Label, SnapshotId, UtcTimestamp, VmState};

use crate::dashboard::{format_bytes, vm_state};
use crate::service::{Failure, RequestStatus, SnapshotList, SnapshotOp, SnapshotRequest};
use crate::theme::{self, Tone};

/// Description under the page title.
pub const DESCRIPTION: &str = "Named points of the VM's disk, as in VMware: take one any time, revert to it, delete it, the others are kept. No memory is ever saved by a snapshot: reverting one restarts the guest from that disk state with a normal boot. A snapshot taken while the VM runs is like a power cut at that instant.";

/// Elapsed time from which the duration of a long operation is shown.
const SHOW_ELAPSED_AFTER: Duration = Duration::from_secs(5);

/// Choice made in the Snapshots page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapshotAction {
    /// Back to the Dashboard.
    Back,
    /// Emergency Stop (also cancels a running snapshot operation).
    EmergencyStop,
    /// Re-read the VM and its snapshots.
    Refresh,
    /// New snapshot with a name and description (asks for confirmation).
    Take,
    /// Revert to a snapshot (asks for confirmation; the VM must be off,
    /// the confirmation offers to shut it down normally first).
    Revert(SnapshotId),
    /// Delete a snapshot (asks for confirmation).
    Delete(SnapshotId),
    /// New name and description of a snapshot (asks for confirmation).
    Rename(SnapshotId),
}

/// What the page needs to know.
#[derive(Clone, Copy, Debug)]
pub struct SnapshotsEnv<'a> {
    /// VM.
    pub vm: &'a VmSummary,
    /// Snapshots as last read, if requested.
    pub list: Option<&'a SnapshotList>,
    /// Last snapshot operation requested by the UI, if it concerns this VM.
    pub request: Option<&'a SnapshotRequest>,
    /// A snapshot operation requested by the UI is waiting for its outcome
    /// (on any VM): only one at a time.
    pub any_pending: bool,
    /// Supervisor connected.
    pub active: bool,
    /// The supervisor can take snapshots (QEMU disk image tool).
    pub available: bool,
    /// Time since the UI sent the running operation, if it did.
    pub elapsed: Option<Duration>,
}

/// Date and time of a timestamp, to the minute: `2026-10-11 09:12 UTC` (the
/// baselines' formatting: same clock, same shape).
pub fn format_time(t: UtcTimestamp) -> String {
    crate::baselines::format_time(t)
}

/// Label generated for an automatic snapshot (the current state kept before
/// a revert): `Snapshot N · <date> UTC`, with `N` the first number not used
/// by an existing name.
pub fn auto_label(existing: &[SnapshotSummary], now: UtcTimestamp) -> Label {
    let mut n = existing.len() + 1;
    let taken = |n: usize| {
        let prefix = format!("Snapshot {n} ");
        existing
            .iter()
            .any(|s| s.name.as_str().starts_with(&prefix) || s.name.as_str() == prefix.trim_end())
    };
    while taken(n) {
        n += 1;
    }
    let text = format!("Snapshot {n} \u{b7} {}", format_time(now));
    Label::parse(text).unwrap_or_else(|_| Label::parse("Snapshot").expect("valid label"))
}

/// A storage operation on the VM that is a snapshot operation (the service
/// reports it in `VmSummary::baselines.operation`).
pub fn is_snapshot_op(op: &StorageOperation) -> bool {
    matches!(
        op,
        StorageOperation::TakeSnapshot
            | StorageOperation::RevertSnapshot
            | StorageOperation::DeleteSnapshot
    )
}

/// Duration, readable: `42 s`, `3 min 05 s`.
pub fn format_elapsed(d: Duration) -> String {
    crate::baselines::format_elapsed(d)
}

/// Why snapshot changes are not possible right now, if they are not.
/// (Taking and deleting work with the VM off or running; only the revert
/// needs the VM off, handled by the confirmation.)
pub fn blocked_reason(env: &SnapshotsEnv<'_>) -> Option<&'static str> {
    let v = env.vm;
    if !env.active {
        Some("The supervisor is not connected")
    } else if !env.available {
        Some("Snapshots are not available: the supervisor has no QEMU disk image tool")
    } else if v.baselines.operation.as_ref().is_some_and(is_snapshot_op)
        && env.request.is_none_or(|r| !r.status.is_pending())
    {
        Some("A snapshot operation is in progress on this VM")
    } else if env.any_pending {
        Some("Another snapshot operation is in progress")
    } else {
        None
    }
}

/// Plain-language explanation of a failed snapshot operation.
pub fn error_text(op: &SnapshotOp, f: &Failure) -> String {
    let what = match op {
        SnapshotOp::Take => "The snapshot was not taken",
        SnapshotOp::Revert(_) => "The VM was not reverted",
        SnapshotOp::Delete(_) => "The snapshot was not deleted",
        SnapshotOp::Rename(_) => "The snapshot was not renamed",
    };
    let Some(code) = f.code else {
        // Connection or protocol problem: the supervisor's own words.
        return format!("{what}: {}.", f.message);
    };
    let detail = |k: &str| f.details.get(k).and_then(|v| v.parse::<u64>().ok());
    let msg = f.message.to_ascii_lowercase();
    match (code, f.reason()) {
        (ErrorCode::StateConflict, Some("snapshots_unavailable")) => {
            format!(
                "{what}: snapshots are not available for this VM now. {}.",
                f.message
            )
        }
        (ErrorCode::StateConflict, _) if msg.contains("shut it down") => format!(
            "{what}: the VM is running, it must be stopped. The Snapshots page offers to shut it down normally first (and to take a snapshot of the current state before it)."
        ),
        (ErrorCode::StateConflict, _) if msg.contains("emergency stop") => format!(
            "{what}: cancelled by the Emergency Stop. Nothing was changed, the VM is as it was."
        ),
        (ErrorCode::StateConflict, _) if msg.contains("completed before the cancel") => format!(
            "{what}: it was cancelled after the disk operation had already finished, so its result is kept. The snapshot is in the list."
        ),
        (ErrorCode::StateConflict, _) if f.details.contains_key("state") => {
            format!(
                "{what}: the VM is not in the state the operation needs. Stop it and try again."
            )
        }
        (ErrorCode::StateConflict, _) => format!("{what}: {}.", f.message),
        (ErrorCode::VmBusy, _) => format!(
            "{what}: the VM is busy (a start, a stop or another operation is in progress, or one of its disks is open in another program). Try again when it is free."
        ),
        (ErrorCode::ResourceLimit, _) => match (
            detail("needed_bytes"),
            detail("free_bytes"),
            detail("budget_gib"),
        ) {
            (needed, _, Some(budget)) => format!(
                "{what}: it would exceed the VM's storage budget of {budget} GiB{}. Lower the VM's usage or raise the budget in Edit.",
                needed.map_or_else(String::new, |n| format!(
                    " (it needs about {})",
                    format_bytes(n)
                ))
            ),
            (Some(needed), Some(free), None) => format!(
                "{what}: not enough free space on the catalog drive. It needs about {}, {} is free and LightVM keeps a reserve.",
                format_bytes(needed),
                format_bytes(free)
            ),
            _ => format!("{what}: not enough storage ({}).", f.message),
        },
        (ErrorCode::InvalidArgument, _) => format!("{what}: {}.", f.message),
        (ErrorCode::Unsupported, _) => format!(
            "{what}: this supervisor cannot handle snapshots (the QEMU disk image tool is missing). Check the QEMU folder in Settings."
        ),
        (ErrorCode::NotFound, _) => format!("{what}: the VM or the snapshot no longer exists."),
        (ErrorCode::InfrastructureError, _) => format!(
            "{what}: the disk image tool or the host storage failed ({}). Nothing was changed.",
            f.message
        ),
        (ErrorCode::Unauthenticated, _) => format!(
            "{what}: the connection to the supervisor was closed during the operation. Nothing was changed."
        ),
        (ErrorCode::PermissionDenied, _) => {
            format!("{what}: this connection is not allowed to manage snapshots.")
        }
        _ => format!("{what}: {}.", f.message),
    }
}

/// One line of snapshot facts for the Dashboard row (`None` when the VM has
/// no snapshot and no operation).
pub fn dashboard_line(v: &VmSummary, list: Option<&SnapshotList>) -> Option<String> {
    let s = &v.snapshots;
    let mut parts = Vec::new();
    if s.count > 0 {
        parts.push(format!(
            "{} {}",
            s.count,
            if s.count == 1 {
                "snapshot"
            } else {
                "snapshots"
            }
        ));
    }
    if let Some(id) = s.current {
        parts.push(match list.and_then(|l| l.tree.as_ref()) {
            Some(t) => t
                .snapshots
                .iter()
                .find(|s| s.snapshot_id == id)
                .map_or_else(
                    || "you are on a deleted snapshot".to_owned(),
                    |s| format!("you are here: \u{201c}{}\u{201d}", s.name.as_str()),
                ),
            None => "you are on a snapshot".to_owned(),
        });
    }
    (!parts.is_empty()).then(|| parts.join(" \u{b7} "))
}

/// Depth of a snapshot in the tree (0 = a root).
pub fn depth_of(tree: &SnapshotTree, id: SnapshotId) -> usize {
    let mut depth = 0;
    let mut parent = tree.snapshots.iter().find(|s| s.snapshot_id == id);
    while let Some(s) = parent
        && let Some(p) = s.parent
    {
        depth += 1;
        parent = tree.snapshots.iter().find(|s| s.snapshot_id == p);
    }
    depth
}

/// Name of a snapshot of the list, if known.
pub fn name_of(list: Option<&SnapshotList>, id: SnapshotId) -> Option<&str> {
    list?
        .tree
        .as_ref()?
        .snapshots
        .iter()
        .find(|s| s.snapshot_id == id)
        .map(|s| s.name.as_str())
}

fn button(
    ui: &mut egui::Ui,
    enabled: bool,
    b: egui::Button<'_>,
    name: &str,
    hover: &str,
) -> egui::Response {
    let r = ui
        .add_enabled(enabled, b)
        .on_hover_text(hover)
        .on_disabled_hover_text(hover);
    theme::accessible_name(&r, WidgetType::Button, name);
    r
}

/// Snapshots page of a VM.
pub fn snapshots_ui(ui: &mut egui::Ui, env: &SnapshotsEnv<'_>) -> Option<SnapshotAction> {
    let p = theme::Palette::of(ui);
    let v = env.vm;
    let name = v.name.as_str();
    let mut choice = None;
    let blocked = blocked_reason(env);

    theme::page_header(ui, &format!("Snapshots \u{b7} {name}"), DESCRIPTION, |ui| {
        let stop = theme::danger(ui, "Emergency Stop");
        if ui
            .add_enabled(env.active, stop)
            .on_hover_text(
                "Immediately terminates all VMs and cancels a running snapshot operation",
            )
            .clicked()
        {
            choice = Some(SnapshotAction::EmergencyStop);
        }
        let take_hover = blocked
            .unwrap_or("Save the VM's disk state as a named snapshot, the VM off or running");
        if button(
            ui,
            blocked.is_none(),
            theme::primary(ui, "Take snapshot\u{2026}"),
            &format!("Take a snapshot of {name}"),
            take_hover,
        )
        .clicked()
        {
            choice = Some(SnapshotAction::Take);
        }
        if ui
            .add_enabled(env.active, egui::Button::new("Refresh"))
            .clicked()
        {
            choice = Some(SnapshotAction::Refresh);
        }
        let back = ui.add(egui::Button::new("Back"));
        theme::accessible_name(&back, WidgetType::Button, "Back to the Dashboard");
        if back.clicked() {
            choice = Some(SnapshotAction::Back);
        }
    });

    progress_or_outcome(ui, env);

    // Summary of the current position.
    theme::card(ui, |ui| {
        let caption = |ui: &mut egui::Ui, t: &str| {
            ui.label(egui::RichText::new(t).small().color(p.text_weak));
        };
        let current = env
            .list
            .and_then(|l| l.tree.as_ref())
            .and_then(|t| t.current);
        egui::Grid::new("lightvm_snapshot_summary")
            .num_columns(4)
            .min_col_width(120.0)
            .spacing([24.0, 4.0])
            .show(ui, |ui| {
                caption(ui, "VM state");
                caption(ui, "You are here");
                caption(ui, "Snapshots");
                caption(ui, "Memory");
                ui.end_row();
                let (state, tone) = vm_state(v.state);
                match v.baselines.operation {
                    Some(op) if is_snapshot_op(&op) => {
                        theme::pill(ui, crate::baselines::operation_label(op), Tone::Warning)
                    }
                    _ => theme::pill(ui, state, tone),
                };
                match current {
                    Some(id) => {
                        let l = name_of(env.list, id).unwrap_or("a snapshot");
                        ui.scope(|ui| {
                            ui.set_max_width(340.0);
                            ui.add(
                                egui::Label::new(format!(
                                    "the state descends from \u{201c}{l}\u{201d}"
                                ))
                                .truncate(),
                            )
                            .on_hover_text(
                                "The last snapshot taken or reverted to: the current disk state came from it",
                            );
                        });
                    }
                    None => {
                        ui.label("\u{2014}")
                            .on_hover_text("No snapshot was ever taken or reverted to on this VM");
                    }
                };
                if v.snapshots.count == 0 {
                    ui.label("None");
                } else {
                    ui.label(format!(
                        "{} \u{b7} {} of own files",
                        v.snapshots.count,
                        format_bytes(
                            env.list
                                .and_then(|l| l.tree.as_ref())
                                .map_or(0, |t| t.snapshots.iter().map(|s| s.files_bytes).sum())
                        )
                    ));
                }
                ui.label("never saved")
                    .on_hover_text(
                        "A snapshot holds the disk state only: the guest restarts from it with a normal boot",
                    );
                ui.end_row();
            });
    });
    if let Some(r) = blocked
        && v.baselines.operation.is_none()
        && !env.any_pending
    {
        ui.add_space(8.0);
        theme::notice(ui, &format!("{r}."), Tone::Neutral);
    }

    ui.add_space(18.0);
    theme::group_title(ui, "Snapshots");
    theme::card(ui, |ui| {
        let Some(list) = env.list else {
            loading(ui);
            return;
        };
        if let Some(e) = &list.error {
            theme::notice(
                ui,
                &format!("The snapshots could not be read: {}", e.message),
                Tone::Danger,
            );
            ui.add_space(8.0);
        }
        if !list.loaded {
            loading(ui);
            return;
        }
        let Some(tree) = list.tree.as_ref() else {
            loading(ui);
            return;
        };
        if !tree.available {
            theme::notice(
                ui,
                tree.unavailable_reason
                    .as_deref()
                    .unwrap_or("Snapshots are not available for this VM right now"),
                Tone::Neutral,
            );
            ui.add_space(8.0);
        }
        if tree.snapshots.is_empty() {
            crate::empty_state(
                ui,
                "No snapshots yet.",
                "Take one any time, the VM off or running. Reverting to it later brings the VM's disk back to that point (no memory is saved: the guest restarts from it with a normal boot).",
            );
            return;
        }
        for (i, s) in tree.snapshots.iter().enumerate() {
            if i > 0 {
                theme::divider(ui);
            }
            ui.push_id(s.snapshot_id, |ui| {
                snapshot_row(ui, tree, s, blocked, &mut choice);
            });
        }
    });
    choice
}

fn loading(ui: &mut egui::Ui) {
    ui.vertical_centered(|ui| {
        ui.add_space(14.0);
        ui.spinner();
        ui.label("Reading the snapshots\u{2026}");
        ui.add_space(14.0);
    });
}

/// Running operation (with the time since it was sent) or outcome of the
/// last one on this VM.
fn progress_or_outcome(ui: &mut egui::Ui, env: &SnapshotsEnv<'_>) {
    let p = theme::Palette::of(ui);
    let v = env.vm;
    let pending = env.request.filter(|r| r.status.is_pending());
    let running = v.baselines.operation.filter(is_snapshot_op).or_else(|| {
        pending.and_then(|r| match &r.op {
            SnapshotOp::Take => Some(StorageOperation::TakeSnapshot),
            SnapshotOp::Revert(_) => Some(StorageOperation::RevertSnapshot),
            SnapshotOp::Delete(_) => Some(StorageOperation::DeleteSnapshot),
            SnapshotOp::Rename(_) => None,
        })
    });
    if let Some(op) = running {
        let (title, body) = match op {
            StorageOperation::TakeSnapshot => {
                if v.state == VmState::Running {
                    (
                        "Taking the snapshot (the VM is running)\u{2026}",
                        "The disk is snapshotted exactly as the guest left it, like a power cut at that instant: crash-consistent. A thumbnail of the screen is kept.",
                    )
                } else {
                    (
                        "Taking the snapshot\u{2026}",
                        "LightVM saves the VM's disk state exactly as it is, with a copy of the UEFI variables. No memory is saved: the guest will restart from this state with a normal boot.",
                    )
                }
            }
            StorageOperation::RevertSnapshot => (
                "Reverting to the snapshot\u{2026}",
                "LightVM applies the snapshot to the VM's disk, restores the UEFI variables and the saved configuration. No memory is saved: the next start boots the guest from the saved disk state with a normal boot.",
            ),
            _ => (
                "Deleting the snapshot\u{2026}",
                "The snapshot is removed from the VM's disk. The other snapshots keep their state.",
            ),
        };
        theme::card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(egui::RichText::new(title).color(p.text));
                if let Some(e) = env.elapsed.filter(|e| *e >= SHOW_ELAPSED_AFTER) {
                    ui.label(
                        egui::RichText::new(format_elapsed(e))
                            .small()
                            .color(p.text_weak),
                    );
                }
            });
            ui.add(egui::Label::new(egui::RichText::new(body).small().color(p.text_weak)).wrap());
            ui.add(
                egui::Label::new(
                    egui::RichText::new(
                        "The VM cannot be started or changed meanwhile. There is no time limit; only the Emergency Stop cancels the operation.",
                    )
                    .small()
                    .color(p.text_weak),
                )
                .wrap(),
            );
        });
        ui.add_space(12.0);
        return;
    }
    let Some(r) = env.request else { return };
    let label = |id: SnapshotId| {
        name_of(env.list, id).map_or_else(
            || "the snapshot".to_owned(),
            |l| format!("\u{201c}{l}\u{201d}"),
        )
    };
    let (text, tone) = match (&r.status, &r.op) {
        (RequestStatus::Done(_), SnapshotOp::Take) => {
            ("Snapshot taken.".to_owned(), Tone::Positive)
        }
        (RequestStatus::Done(id), SnapshotOp::Revert(_)) => (
            format!(
                "Reverted {}: the next start boots the guest from that disk state, with a normal boot.",
                label(*id)
            ),
            Tone::Positive,
        ),
        (RequestStatus::Done(_), SnapshotOp::Delete(_)) => (
            "Snapshot deleted. The other snapshots keep their state.".to_owned(),
            Tone::Positive,
        ),
        (RequestStatus::Done(_), SnapshotOp::Rename(_)) => {
            ("Snapshot renamed.".to_owned(), Tone::Positive)
        }
        (RequestStatus::Failed(f), _) => (error_text(&r.op, f), Tone::Danger),
        _ => return,
    };
    theme::notice(ui, &text, tone);
    ui.add_space(12.0);
}

fn snapshot_row(
    ui: &mut egui::Ui,
    tree: &SnapshotTree,
    s: &SnapshotSummary,
    blocked: Option<&'static str>,
    choice: &mut Option<SnapshotAction>,
) {
    let p = theme::Palette::of(ui);
    let name = s.name.as_str();
    const ACTIONS: f32 = 250.0;
    ui.horizontal(|ui| {
        ui.set_min_height(46.0);
        let left = (ui.available_width() - ACTIONS - 12.0).max(150.0);
        ui.allocate_ui_with_layout(
            egui::vec2(left, 46.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.set_width(left);
                ui.spacing_mut().item_spacing.y = 3.0;
                let depth = depth_of(tree, s.snapshot_id);
                let parent_name = s
                    .parent
                    .and_then(|p| tree.snapshots.iter().find(|s| s.snapshot_id == p))
                    .map(|s| s.name.as_str());
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    if depth > 0 {
                        ui.label(egui::RichText::new("\u{21b3}").color(p.text_weak))
                            .on_hover_text(format!(
                                "taken on \u{201c}{}\u{201d}",
                                parent_name.unwrap_or("a deleted snapshot")
                            ));
                    }
                    ui.add(
                        egui::Label::new(egui::RichText::new(name).size(15.0).color(p.text))
                            .truncate(),
                    );
                    if Some(s.snapshot_id) == tree.current {
                        theme::pill(ui, "You are here", Tone::Accent).on_hover_text(
                            "The VM's current state descends from this snapshot",
                        );
                    }
                    if s.taken_while_running {
                        theme::pill(ui, "While running", Tone::Warning).on_hover_text(
                            "Taken while the VM ran: like a power cut at that instant, the disk is crash-consistent",
                        );
                    } else if s.clean_shutdown {
                        theme::pill(ui, "Clean", Tone::Positive).on_hover_text(
                            "Taken with the VM off, after an observed clean shutdown of the guest (or before any run)",
                        );
                    }
                    if s.firmware_saved {
                        theme::pill(ui, "Firmware saved", Tone::Neutral)
                            .on_hover_text("A copy of the UEFI variables is kept with the snapshot");
                    }
                });
                let mut details = vec![format!("Created {}", format_time(s.created_at))];
                if !s.description.is_empty() {
                    details.push(s.description.lines().next().unwrap_or_default().to_owned());
                }
                details.push(format!("disk {} GiB", s.disk_gib));
                details.push(format!("{} of own files", format_bytes(s.files_bytes)));
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
                ui.spacing_mut().item_spacing.x = 6.0;
                let delete_hover = blocked
                    .unwrap_or("Removes this snapshot from the VM's disk (asks for confirmation); the others keep their state");
                if button(
                    ui,
                    blocked.is_none(),
                    egui::Button::new(egui::RichText::new("Delete\u{2026}").color(p.danger))
                        .min_size(egui::vec2(72.0, 0.0)),
                    &format!("Delete snapshot \u{201c}{name}\u{201d}"),
                    delete_hover,
                )
                .clicked()
                {
                    *choice = Some(SnapshotAction::Delete(s.snapshot_id));
                }
                let rename_hover = blocked
                    .unwrap_or("New name and description (asks for confirmation)");
                if button(
                    ui,
                    blocked.is_none(),
                    egui::Button::new("Rename\u{2026}").min_size(egui::vec2(72.0, 0.0)),
                    &format!("Rename snapshot \u{201c}{name}\u{201d}"),
                    rename_hover,
                )
                .clicked()
                {
                    *choice = Some(SnapshotAction::Rename(s.snapshot_id));
                }
                let revert_hover = match blocked {
                    Some(r) => r,
                    None if Some(s.snapshot_id) == tree.current => {
                        "The VM's current state already descends from this snapshot"
                    }
                    None => "Brings the VM's disk back to this snapshot, with its UEFI variables and configuration; the VM must be stopped (asks for confirmation)",
                };
                if button(
                    ui,
                    blocked.is_none(),
                    egui::Button::new("Revert to\u{2026}").min_size(egui::vec2(84.0, 0.0)),
                    &format!("Revert to snapshot \u{201c}{name}\u{201d}"),
                    revert_hover,
                )
                .clicked()
                {
                    *choice = Some(SnapshotAction::Revert(s.snapshot_id));
                }
            },
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightvm_proto::ipc::SnapshotTree;

    fn snap(
        n: u8,
        name: &str,
        parent: Option<SnapshotId>,
        running: bool,
        clean: bool,
    ) -> SnapshotSummary {
        SnapshotSummary {
            snapshot_id: SnapshotId::from_unix_millis(1_760_000_500_000 + u64::from(n), [n; 10]),
            vm_id: lightvm_proto::VmId::from_unix_millis(1_760_000_000_001, [1; 10]),
            parent,
            name: Label::parse(name).unwrap(),
            description: String::new(),
            created_at: UtcTimestamp::from_unix_millis(
                1_791_619_200_000 + i64::from(n) * 3_600_000,
            )
            .unwrap(),
            taken_while_running: running,
            clean_shutdown: clean,
            firmware_saved: true,
            has_thumbnail: running,
            disk_gib: 16,
            config_revision: 3,
            files_bytes: 1 << 20,
        }
    }

    #[test]
    fn times_and_labels_are_readable() {
        let t = UtcTimestamp::from_unix_millis(1_791_710_000_000).unwrap();
        let s = format_time(t);
        assert!(s.ends_with(" UTC") && s.len() == 20, "{s}");
        let a = snap(1, "Snapshot 1", None, false, true);
        let b = snap(
            2,
            "Snapshot 2 \u{b7} 2026-10-10 08:00 UTC",
            Some(a.snapshot_id),
            true,
            false,
        );
        let l = auto_label(&[a.clone(), b.clone()], t);
        assert_eq!(l.as_str(), format!("Snapshot 3 \u{b7} {s}"));
        assert_eq!(format_elapsed(Duration::from_secs(42)), "42 s");
    }

    #[test]
    fn the_tree_depth_follows_the_parents() {
        let a = snap(1, "a", None, false, true);
        let b = snap(2, "b", Some(a.snapshot_id), false, true);
        let c = snap(3, "c", Some(b.snapshot_id), true, false);
        let tree = SnapshotTree {
            snapshots: vec![a.clone(), b.clone(), c.clone()],
            current: Some(c.snapshot_id),
            available: true,
            unavailable_reason: None,
        };
        assert_eq!(depth_of(&tree, a.snapshot_id), 0);
        assert_eq!(depth_of(&tree, b.snapshot_id), 1);
        assert_eq!(depth_of(&tree, c.snapshot_id), 2);
    }

    #[test]
    fn errors_are_explained() {
        let take = SnapshotOp::Take;
        let fr = Failure::coded(ErrorCode::ResourceLimit, "x")
            .with_detail("needed_bytes", "4294967296")
            .with_detail("free_bytes", "1073741824");
        assert!(
            error_text(&take, &fr).contains("4.0 GiB")
                && error_text(&take, &fr).contains("1.0 GiB is free"),
        );
        let fr = Failure::coded(ErrorCode::StateConflict, "x")
            .with_detail("reason", "snapshots_unavailable");
        assert!(
            error_text(&take, &fr).contains("snapshots are not available"),
            "snapshots are not available"
        );
        let revert = SnapshotOp::Revert(SnapshotId::new_v7());
        let fr = Failure::coded(
            ErrorCode::StateConflict,
            "reverting to a snapshot needs the VM stopped: shut it down first",
        );
        assert!(
            error_text(&revert, &fr).contains("offers to shut it down normally first"),
            "offers to shut it down normally first"
        );
    }

    #[test]
    fn description_limit_is_checked_by_the_proto() {
        use lightvm_proto::ipc::{MAX_SNAPSHOT_DESCRIPTION_CHARS, validate_snapshot_description};
        assert!(validate_snapshot_description("ok\nand a tab\there").is_ok());
        assert!(
            validate_snapshot_description(&"x".repeat(MAX_SNAPSHOT_DESCRIPTION_CHARS + 1)).is_err()
        );
        assert!(validate_snapshot_description("bell \u{7}").is_err());
    }
}
