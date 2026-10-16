//! Move of the VM catalog from Settings (Supervisor → **Move catalog…**).
//!
//! The UI never opens catalog files (ADR-0024): the move is run by the
//! supervisor executable in its `--move-catalog` mode, as a child process
//! that prints JSON progress lines and is cancelled with a `shutdown` line
//! on stdin (contract in `lightvm_service::catalog_move`). Flow:
//!
//! 1. the button is enabled only for a supervisor started by this window
//!    (not `--catalog` from the command line), with every VM stopped and the
//!    settings applied; otherwise it is disabled with the reason on hover;
//! 2. folder picker, then a confirmation that says what happens; an
//!    existing folder that is not empty gets the catalog in a `catalog`
//!    subfolder;
//! 3. on a background thread: controlled stop of the supervisor, then the
//!    move, with progress and **Cancel**;
//! 4. moved: the settings point to the new folder; in every case the
//!    supervisor is started again (on the new or on the unchanged catalog)
//!    and the outcome is shown.
//!
//! The move itself (checks, copy, SHA-256 verification, validation, removal
//! of the source only at the end) is `vm_storage::move_catalog`.

use std::io::{BufRead as _, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use egui::WidgetType;
use lightvm_service::catalog_move::MoveEvent;

use crate::dashboard::format_bytes;
use crate::dialogs::{self, Choice, ConfirmSpec};
use crate::theme::{self, Tone};
use crate::{LightVmApp, diag, service};

/// Database file whose presence marks a catalog folder (only its existence
/// is checked here).
const CATALOG_DB: &str = "catalog.sqlite3";

/// Move to run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MoveRequest {
    /// `lightvm-service` executable.
    pub executable: PathBuf,
    /// Current catalog.
    pub from: PathBuf,
    /// New catalog folder.
    pub to: PathBuf,
    /// Keep the original folder.
    pub keep_source: bool,
}

/// Last progress received.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MoveStatus {
    /// Phase name (`checking`, `copying`, ...); empty while the supervisor
    /// is being stopped.
    pub phase: String,
    /// Bytes done in the phase.
    pub done_bytes: u64,
    /// Bytes of the phase.
    pub total_bytes: u64,
}

/// Outcome of a move.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MoveOutcome {
    /// The catalog is in the new folder.
    Moved {
        /// Files moved.
        files: u64,
        /// Bytes moved.
        bytes: u64,
        /// VMs in the catalog.
        vms: u64,
        /// The original folder is gone.
        source_removed: bool,
        /// The original folder could not be removed completely.
        source_cleanup_error: Option<String>,
    },
    /// Nothing changed.
    Failed(String),
    /// Cancelled: nothing changed.
    Cancelled,
}

/// Runs a move to the end: request, progress sink, cancel flag. The app
/// uses [`child_runner`]; tests pass a fake.
pub type MoveRunner =
    Arc<dyn Fn(&MoveRequest, &dyn Fn(MoveStatus), &AtomicBool) -> MoveOutcome + Send + Sync>;

/// The move run by `lightvm-service --move-catalog`.
pub fn child_runner() -> MoveRunner {
    Arc::new(run_child)
}

fn run_child(req: &MoveRequest, progress: &dyn Fn(MoveStatus), cancel: &AtomicBool) -> MoveOutcome {
    let mut c = Command::new(&req.executable);
    c.arg("--move-catalog")
        .arg("--catalog")
        .arg(&req.from)
        .arg("--to")
        .arg(&req.to);
    if req.keep_source {
        c.arg("--keep-source");
    }
    c.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(diag::service_stderr().unwrap_or_else(Stdio::inherit));
    let mut child = match c.spawn() {
        Ok(ch) => ch,
        Err(e) => return MoveOutcome::Failed(format!("move not started: {e}")),
    };
    let (Some(mut stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        let _ = child.kill();
        let _ = child.wait();
        return MoveOutcome::Failed("move not started: no pipes to the process".into());
    };
    // Cancellation: a `shutdown` line; the pipe stays open until the end
    // (its EOF would cancel too).
    let done = Arc::new(AtomicBool::new(false));
    let cancel_flag = Arc::new(AtomicBool::new(false));
    let (d, cf) = (done.clone(), cancel_flag.clone());
    let watcher = std::thread::Builder::new()
        .name("catalog-move-cancel".into())
        .spawn(move || {
            while !d.load(Ordering::SeqCst) {
                if cf.load(Ordering::SeqCst) {
                    let _ = writeln!(stdin, "shutdown").and_then(|()| stdin.flush());
                    while !d.load(Ordering::SeqCst) {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        });
    let mut last = None;
    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else { break };
        if cancel.load(Ordering::SeqCst) {
            cancel_flag.store(true, Ordering::SeqCst);
        }
        match serde_json::from_str::<MoveEvent>(&line) {
            Ok(MoveEvent::MoveProgress {
                phase,
                done_bytes,
                total_bytes,
            }) => progress(MoveStatus {
                phase,
                done_bytes,
                total_bytes,
            }),
            Ok(e) => last = Some(e),
            Err(_) => {}
        }
    }
    let status = child.wait();
    done.store(true, Ordering::SeqCst);
    if let Ok(w) = watcher {
        let _ = w.join();
    }
    match last {
        Some(MoveEvent::MoveDone {
            files,
            bytes,
            vms,
            source_removed,
            source_cleanup_error,
            ..
        }) => MoveOutcome::Moved {
            files,
            bytes,
            vms,
            source_removed,
            source_cleanup_error,
        },
        Some(MoveEvent::MoveFailed {
            cancelled: true, ..
        }) => MoveOutcome::Cancelled,
        Some(MoveEvent::MoveFailed { message, .. }) => MoveOutcome::Failed(message),
        _ => MoveOutcome::Failed(match status {
            Ok(s) => format!("the move process ended without an outcome ({s})"),
            Err(e) => format!("the move process could not be awaited: {e}"),
        }),
    }
}

/// Where the catalog goes for a folder chosen in the picker: the folder
/// itself if empty, else a `catalog` subfolder (never mixed with other
/// files).
pub fn destination_for(chosen: &Path) -> PathBuf {
    let empty = std::fs::read_dir(chosen).map_or(true, |mut it| it.next().is_none());
    if empty {
        chosen.to_path_buf()
    } else {
        chosen.join(crate::settings::CATALOG_DIR)
    }
}

/// Readable phase.
pub fn phase_label(phase: &str) -> &'static str {
    match phase {
        "" => "Stopping the supervisor",
        "checking" => "Checking the catalog",
        "copying" => "Copying the files",
        "verifying" => "Verifying the copy (SHA-256)",
        "validating" => "Opening the moved catalog",
        "removing_source" => "Removing the old folder",
        _ => "Moving",
    }
}

/// Step of the move flow.
pub(crate) enum Stage {
    /// Waiting for the confirmation.
    Confirm { keep_source: bool, focus: bool },
    /// Stop of the supervisor and move in progress.
    Running {
        status: Arc<Mutex<MoveStatus>>,
        cancel: Arc<AtomicBool>,
        rx: Receiver<MoveOutcome>,
    },
    /// Outcome shown.
    Finished(MoveOutcome),
}

/// Move in progress (dialog open).
pub(crate) struct MoveFlow {
    from: PathBuf,
    to: PathBuf,
    /// The supervisor was running: it is started again at the end.
    restart_after: bool,
    stage: Stage,
}

impl LightVmApp {
    /// Source catalog the Move button would move, or why it is disabled.
    pub(crate) fn move_availability(&self, running_vms: usize) -> Result<PathBuf, String> {
        if self.cli_config.is_some() {
            return Err("The catalog was given on the command line (--catalog): start LightVM without --catalog to move it from here.".into());
        }
        if self.folder_picker.is_none() {
            return Err("Folder picker not available on this platform.".into());
        }
        if self.restart.is_some() || self.catalog_move.is_some() {
            return Err("Wait for the supervisor restart or stop to finish.".into());
        }
        if running_vms > 0 {
            return Err("Stop every VM first: the catalog moves only with all VMs stopped.".into());
        }
        let chosen = self.settings.effective_catalog();
        let source = match (&self.service, &self.applied_service) {
            (Some(_), None) => {
                return Err("The supervisor was not started by this window: stop it, then move the catalog.".into());
            }
            (Some(_), Some((applied, _))) => {
                if chosen.as_ref() != Some(applied) {
                    return Err("The catalog folder was changed but not applied: restart the supervisor first.".into());
                }
                applied.clone()
            }
            (None, _) => chosen.ok_or("The catalog folder cannot be determined.")?,
        };
        if !source.join(CATALOG_DB).is_file() {
            return Err("There is no catalog in this folder yet: nothing to move.".into());
        }
        Ok(source)
    }

    /// The Move catalog… button: chooses the destination and opens the
    /// confirmation.
    pub(crate) fn begin_catalog_move(&mut self) {
        let running = self.state().map_or(0, |s| s.active_vms().count());
        let from = match self.move_availability(running) {
            Ok(f) => f,
            Err(e) => {
                self.service_error = Some(e);
                return;
            }
        };
        let Some(chosen) = self
            .folder_picker
            .as_mut()
            .and_then(|f| f("Move the catalog to"))
        else {
            return;
        };
        self.catalog_move = Some(MoveFlow {
            to: destination_for(&chosen),
            from,
            restart_after: false,
            stage: Stage::Confirm {
                keep_source: false,
                focus: true,
            },
        });
    }

    /// A catalog move dialog is open.
    pub fn catalog_move_open(&self) -> bool {
        self.catalog_move.is_some()
    }

    /// The move itself is running (stop, copy, verification).
    pub fn catalog_move_running(&self) -> bool {
        matches!(
            self.catalog_move,
            Some(MoveFlow {
                stage: Stage::Running { .. },
                ..
            })
        )
    }

    /// Runs the move on a separate thread, after the controlled stop of the
    /// supervisor.
    fn run_catalog_move(&mut self, ctx: &egui::Context, keep_source: bool) {
        let Some((from, to)) = self
            .catalog_move
            .as_ref()
            .map(|f| (f.from.clone(), f.to.clone()))
        else {
            return;
        };
        let executable = match &self.service_exe {
            Some(p) => Some(p.clone()),
            None => service::ServiceConfig::executable_next_to_ui(),
        };
        let Some(executable) = executable else {
            if let Some(flow) = &mut self.catalog_move {
                flow.stage = Stage::Finished(MoveOutcome::Failed(
                    "supervisor executable not found".into(),
                ));
            }
            return;
        };
        let req = MoveRequest {
            executable,
            from,
            to,
            keep_source,
        };
        let runner = self.move_runner.clone().unwrap_or_else(child_runner);
        let status = Arc::new(Mutex::new(MoveStatus::default()));
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = std::sync::mpsc::channel();
        // Same clean-up as a restart: nothing may keep using the supervisor.
        self.close_console();
        let old = self.service.take();
        if let Some(flow) = &mut self.catalog_move {
            flow.restart_after = old.is_some();
        }
        self.applied_service = None;
        self.vm_edit = None;
        self.wizard = None;
        self.dialog = None;
        self.baselines_vm = None;
        self.baseline_dialog = None;
        self.agent_session = None;
        self.approve_form = None;
        self.revoke_dialog = None;
        self.pairing_dialog = None;
        self.service_error = None;
        let (st, c, ctx2) = (status.clone(), cancel.clone(), ctx.clone());
        let spawned = std::thread::Builder::new()
            .name("lightvm-catalog-move".into())
            .spawn(move || {
                if let Some(mut s) = old {
                    let _ = s.stop(service::STOP_TIMEOUT);
                }
                let outcome = if c.load(Ordering::SeqCst) {
                    MoveOutcome::Cancelled
                } else {
                    runner(
                        &req,
                        &|p| {
                            if let Ok(mut g) = st.lock() {
                                *g = p;
                            }
                            ctx2.request_repaint();
                        },
                        &c,
                    )
                };
                let _ = tx.send(outcome);
                ctx2.request_repaint();
            });
        let Some(flow) = &mut self.catalog_move else {
            return;
        };
        flow.stage = match spawned {
            Ok(_) => Stage::Running { status, cancel, rx },
            Err(e) => Stage::Finished(MoveOutcome::Failed(format!("move not started: {e}"))),
        };
    }

    /// Outcome of the background move, if arrived: settings updated and
    /// supervisor started again.
    fn collect_catalog_move(&mut self, ctx: &egui::Context) {
        let Some(MoveFlow {
            stage: Stage::Running { rx, .. },
            ..
        }) = &self.catalog_move
        else {
            return;
        };
        let outcome = match rx.try_recv() {
            Ok(o) => o,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => {
                MoveOutcome::Failed("catalog move interrupted".into())
            }
        };
        let Some(flow) = self.catalog_move.take() else {
            return;
        };
        if let MoveOutcome::Moved { .. } = outcome {
            self.settings.catalog = Some(flow.to.clone());
            self.save_settings();
        }
        let msg = match &outcome {
            MoveOutcome::Moved { .. } => format!(
                "catalog moved from {} to {}",
                flow.from.display(),
                flow.to.display()
            ),
            MoveOutcome::Failed(e) => format!("catalog move failed: {e}"),
            MoveOutcome::Cancelled => "catalog move cancelled".into(),
        };
        diag::log(&msg);
        if flow.restart_after {
            self.restart_in_background(ctx, true);
        }
        self.catalog_move = Some(MoveFlow {
            stage: Stage::Finished(outcome),
            ..flow
        });
    }

    /// Dialogs of the move: confirmation, progress, outcome.
    pub(crate) fn catalog_move_ui(&mut self, ui: &egui::Ui, ctx: &egui::Context) {
        self.collect_catalog_move(ctx);
        let Some(flow) = &mut self.catalog_move else {
            return;
        };
        match &mut flow.stage {
            Stage::Confirm { keep_source, focus } => {
                let spec = ConfirmSpec {
                    title: "Move the catalog?".into(),
                    body: format!(
                        "Every VM, disk, baseline, installation image and session report moves\nfrom {}\nto {}\n\nThe supervisor stops during the move and starts again afterwards. Every file is copied and checked (size and SHA-256), and the new catalog is opened, before the old folder is deleted; on the same drive the folder is just renamed. If anything fails, or you cancel, the catalog stays where it is.\n\nIt can take a while for large disks. The new folder gets the permissions of the folder that contains it: choose a place only your user account can read.",
                        flow.from.display(),
                        flow.to.display()
                    ),
                    confirm: "Move catalog".into(),
                    destructive: false,
                };
                let first = std::mem::take(focus);
                let mut keep = *keep_source;
                let choice = dialogs::confirm_custom(
                    ui,
                    "lightvm_catalog_move_confirm",
                    &spec,
                    false,
                    None,
                    first,
                    |ui, enabled| {
                        ui.add_space(10.0);
                        ui.add_enabled(
                            enabled,
                            egui::Checkbox::new(
                                &mut keep,
                                "Keep the original folder (a full copy stays there; delete it yourself later)",
                            ),
                        );
                        true
                    },
                );
                *keep_source = keep;
                match choice {
                    Some(Choice::Cancel) => self.catalog_move = None,
                    Some(Choice::Confirm) => self.run_catalog_move(ctx, keep),
                    None => {}
                }
            }
            Stage::Running { status, cancel, .. } => {
                let st = status.lock().map(|g| g.clone()).unwrap_or_default();
                let cancelling = cancel.load(Ordering::SeqCst);
                if progress_dialog(ui, &flow.to, &st, cancelling) {
                    cancel.store(true, Ordering::SeqCst);
                }
                ctx.request_repaint_after(Duration::from_millis(200));
            }
            Stage::Finished(outcome) => {
                let (title, body, tone) = outcome_text(outcome, &flow.from, &flow.to);
                if outcome_dialog(ui, &title, &body, tone) {
                    self.catalog_move = None;
                }
            }
        }
    }
}

/// Title, text and tone of the outcome.
fn outcome_text(o: &MoveOutcome, from: &Path, to: &Path) -> (String, String, Tone) {
    match o {
        MoveOutcome::Moved {
            files,
            bytes,
            vms,
            source_removed,
            source_cleanup_error,
        } => {
            let mut body = format!(
                "{} VMs, {} files ({}) are now in {}. LightVM uses this folder from now on.",
                vms,
                files,
                format_bytes(*bytes),
                to.display()
            );
            match source_cleanup_error {
                Some(e) => body.push_str(&format!(
                    "\n\nThe old folder {} could not be removed completely ({e}): delete what is left of it yourself.",
                    from.display()
                )),
                None if !source_removed => body.push_str(&format!(
                    "\n\nThe original folder {} was kept: it is no longer used.",
                    from.display()
                )),
                None => {}
            }
            ("Catalog moved".into(), body, Tone::Positive)
        }
        MoveOutcome::Failed(e) => (
            "Catalog not moved".into(),
            format!(
                "{e}\n\nNothing changed: the catalog is still in {}.",
                from.display()
            ),
            Tone::Danger,
        ),
        MoveOutcome::Cancelled => (
            "Move cancelled".into(),
            format!(
                "The partial copy was removed. The catalog is still in {}.",
                from.display()
            ),
            Tone::Warning,
        ),
    }
}

fn dialog_frame(ui: &egui::Ui) -> egui::Frame {
    let p = theme::Palette::of(ui);
    egui::Frame::new()
        .fill(p.surface)
        .stroke(egui::Stroke::new(1.0, p.border))
        .corner_radius(egui::CornerRadius::ZERO)
        .inner_margin(egui::Margin::same(20))
        .shadow(ui.visuals().window_shadow)
}

/// Progress of the move; returns `true` when Cancel is pressed. It cannot
/// be dismissed otherwise.
fn progress_dialog(ui: &egui::Ui, to: &Path, st: &MoveStatus, cancelling: bool) -> bool {
    let ctx = ui.ctx().clone();
    let mut cancel = false;
    egui::Modal::new(egui::Id::new("lightvm_catalog_move_progress"))
        .frame(dialog_frame(ui))
        .show(&ctx, |ui| {
            ui.set_width(440.0);
            let p = theme::Palette::of(ui);
            ui.label(
                egui::RichText::new("Moving the catalog…")
                    .size(17.0)
                    .color(p.text),
            );
            ui.add_space(6.0);
            ui.add(
                egui::Label::new(
                    egui::RichText::new(format!("To {}", to.display())).color(p.text_weak),
                )
                .wrap(),
            );
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(phase_label(&st.phase));
            });
            if st.total_bytes > 0 {
                ui.add_space(6.0);
                #[allow(clippy::cast_precision_loss, reason = "progress fraction")]
                let f = st.done_bytes as f32 / st.total_bytes as f32;
                let r = ui.add(egui::ProgressBar::new(f.clamp(0.0, 1.0)).text(format!(
                    "{} of {}",
                    format_bytes(st.done_bytes),
                    format_bytes(st.total_bytes)
                )));
                theme::accessible_name(&r, WidgetType::ProgressIndicator, "Catalog move progress");
            }
            ui.add_space(16.0);
            ui.horizontal(|ui| {
                let text = if cancelling {
                    "Cancelling…"
                } else {
                    "Cancel"
                };
                if ui
                    .add_enabled(
                        !cancelling,
                        egui::Button::new(text).min_size(egui::vec2(96.0, 0.0)),
                    )
                    .clicked()
                {
                    cancel = true;
                }
            });
        });
    cancel
}

/// Outcome of the move; returns `true` when it is closed.
fn outcome_dialog(ui: &egui::Ui, title: &str, body: &str, tone: Tone) -> bool {
    let ctx = ui.ctx().clone();
    let mut close = false;
    let modal = egui::Modal::new(egui::Id::new("lightvm_catalog_move_outcome"))
        .frame(dialog_frame(ui))
        .show(&ctx, |ui| {
            ui.set_width(440.0);
            let p = theme::Palette::of(ui);
            ui.label(egui::RichText::new(title).size(17.0).color(p.text));
            ui.add_space(8.0);
            theme::notice(ui, body, tone);
            ui.add_space(16.0);
            if ui
                .add(theme::primary(ui, "OK").min_size(egui::vec2(96.0, 0.0)))
                .clicked()
            {
                close = true;
            }
        });
    close || modal.should_close()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_empty_folder_gets_a_catalog_subfolder() {
        let d = std::env::temp_dir().join(format!("lightvm-move-dest-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        assert_eq!(destination_for(&d), d);
        std::fs::write(d.join("x.txt"), b"x").unwrap();
        assert_eq!(destination_for(&d), d.join("catalog"));
        assert_eq!(destination_for(&d.join("missing")), d.join("missing"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn every_phase_has_a_label() {
        for p in [
            "",
            "checking",
            "copying",
            "verifying",
            "validating",
            "removing_source",
        ] {
            assert_ne!(phase_label(p), "Moving", "{p}");
        }
    }
}
