//! Session reports in the AI Workspace (F5; ROADMAP 9 "Report", 10 F5
//! "Report JSON/Markdown e stati di esito distinti").
//!
//! - Session history: every report kept by the supervisor (live and past
//!   sessions, newest first) with the outcome and where it comes from.
//! - Report sections of the session page: outcome with its source and the
//!   verdict history, the agent's declaration (labelled as a claim, never
//!   the outcome), limits, versions and evidence, errors, events by page,
//!   screenshots by hash shown only on request, export and deletion.
//!
//! Everything the agent wrote (client name, objective, declaration, tool
//! notes) is shown as plain text, never markup or links. The views execute
//! nothing: they return an [`AgentAction`]. Screenshots are read only when
//! the user asks, kept in memory and never written by the app.

use egui::WidgetType;
use lightvm_proto::report::{OutcomeSource, ReportEvent, ReportEventBody, SessionReport};
use lightvm_proto::{SessionId, SessionState, TestOutcome};

use crate::agents::{AgentAction, AgentsEnv, capability_dangerous, capability_text, clock};
use crate::baselines::format_time;
use crate::service::{
    EVENTS_PAGE, ReportOp, ReportOutcome, RequestStatus, ShotImage, termination_text,
};
use crate::theme::{self, Tone};

/// Rationale sent with a verdict of the local user. The supervisor needs a
/// non-blank one; the app has no text field, so it is this fixed text and
/// the verdict is attributed by the supervisor to "local user".
pub const USER_VERDICT_RATIONALE: &str =
    "Judged by the local user in the LightVM app after reviewing the session report.";

/// Outcomes the user can choose as a verdict.
pub const VERDICTS: [(TestOutcome, &str); 3] = [
    (TestOutcome::Passed, "Passed: the objective was met"),
    (TestOutcome::Failed, "Failed: the objective was not met"),
    (
        TestOutcome::Inconclusive,
        "Inconclusive: the evidence is not enough to say",
    ),
];

/// Readable outcome.
pub fn outcome_text(o: TestOutcome) -> &'static str {
    match o {
        TestOutcome::Passed => "Passed",
        TestOutcome::Failed => "Failed",
        TestOutcome::Inconclusive => "Inconclusive",
        TestOutcome::Cancelled => "Cancelled",
        TestOutcome::TimedOut => "Timed out",
        TestOutcome::InfrastructureError => "Infrastructure error",
    }
}

/// Colour of an outcome (the text always says which one it is).
pub fn outcome_tone(o: TestOutcome) -> Tone {
    match o {
        TestOutcome::Passed => Tone::Positive,
        TestOutcome::Failed => Tone::Danger,
        TestOutcome::Inconclusive | TestOutcome::Cancelled => Tone::Neutral,
        TestOutcome::TimedOut | TestOutcome::InfrastructureError => Tone::Warning,
    }
}

/// Where the outcome comes from, short (lists).
pub fn source_short(s: OutcomeSource) -> &'static str {
    match s {
        OutcomeSource::Pending => "pending: session running",
        OutcomeSource::Termination => "from how the session ended",
        OutcomeSource::Verifier => "by the automatic verifier",
        OutcomeSource::User => "your verdict",
    }
}

/// Where the outcome comes from, in full (session page).
pub fn source_long(r: &SessionReport) -> String {
    match r.outcome_source {
        OutcomeSource::Pending => {
            "Pending: the session is still running. Until it ends the outcome reads inconclusive."
                .into()
        }
        OutcomeSource::Termination => format!(
            "Default from how the session ended ({}). Nobody judged it yet.",
            r.termination.map_or("ended", termination_text)
        ),
        OutcomeSource::Verifier => {
            let name = r
                .verdicts
                .last()
                .and_then(|v| v.verifier.clone())
                .unwrap_or_else(|| "automatic verifier".into());
            format!("Decided by the automatic verifier \u{201c}{name}\u{201d}.")
        }
        OutcomeSource::User => {
            let at = r
                .verdicts
                .last()
                .map_or_else(String::new, |v| format!(" on {}", format_time(v.at)));
            format!("Your verdict, recorded as decided by \u{201c}local user\u{201d}{at}.")
        }
    }
}

/// Outcome pill with its source next to it; the accessible name says both.
pub fn outcome_badge(ui: &mut egui::Ui, outcome: TestOutcome, source: OutcomeSource) {
    let r = theme::pill(ui, outcome_text(outcome), outcome_tone(outcome));
    theme::accessible_name(
        &r,
        WidgetType::Label,
        &format!(
            "Outcome: {}, {}",
            outcome_text(outcome),
            source_short(source)
        ),
    );
    small(ui, source_short(source));
}

fn small(ui: &mut egui::Ui, text: &str) {
    let p = theme::Palette::of(ui);
    ui.add(egui::Label::new(egui::RichText::new(text).small().color(p.text_weak)).wrap());
}

fn plain(ui: &mut egui::Ui, text: &str) {
    let p = theme::Palette::of(ui);
    ui.add(egui::Label::new(egui::RichText::new(text).color(p.text)).wrap());
}

fn button(ui: &mut egui::Ui, enabled: bool, b: egui::Button<'_>, name: &str, hover: &str) -> bool {
    let r = ui
        .add_enabled(enabled, b)
        .on_hover_text(hover)
        .on_disabled_hover_text(hover);
    theme::accessible_name(&r, WidgetType::Button, name);
    r.clicked()
}

/// Start and end of a session: `2026-10-11 09:04 UTC → 09:20 UTC`.
pub fn span_text(
    started: lightvm_proto::UtcTimestamp,
    ended: Option<lightvm_proto::UtcTimestamp>,
) -> String {
    let start = format_time(started);
    match ended {
        None => format!("{start} \u{2192} running"),
        Some(e) => {
            let end = format_time(e);
            // Same day: the time only.
            match (start.get(..10), end.get(..10), end.get(11..)) {
                (Some(a), Some(b), Some(t)) if a == b => format!("{start} \u{2192} {t}"),
                _ => format!("{start} \u{2192} {end}"),
            }
        }
    }
}

/// Accessible name of the button opening a report from the history.
pub fn open_report_name(client: &str, started: lightvm_proto::UtcTimestamp) -> String {
    format!(
        "Open the report of {client}, started {}",
        format_time(started)
    )
}

/// Session history: the reports kept by the supervisor, newest first.
pub fn history_ui(ui: &mut egui::Ui, env: &AgentsEnv<'_>, choice: &mut Option<AgentAction>) {
    let r = &env.state.reports;
    if let Some(e) = &r.error {
        theme::notice(
            ui,
            &format!("Reports not read: {}", e.message),
            if e.unsupported {
                Tone::Neutral
            } else {
                Tone::Warning
            },
        );
        ui.add_space(6.0);
    }
    // Ended sessions the supervisor has no report for (a supervisor
    // without reports): listed as before, without outcome.
    let unreported: Vec<_> = env
        .state
        .agents
        .sessions
        .iter()
        .filter(|s| s.state == SessionState::Terminated && r.summary(s.session_id).is_none())
        .collect();
    if r.list.is_empty() && unreported.is_empty() {
        crate::empty_state(
            ui,
            if r.loaded {
                "No session reports yet."
            } else {
                "Reports not read yet."
            },
            "Each approved session gets a report: what the agent did, its screenshots by hash and the outcome. Reports stay until you delete them (the 100 most recent are kept).",
        );
        return;
    }
    let p = theme::Palette::of(ui);
    let mut first = true;
    for s in &r.list {
        if !first {
            theme::divider(ui);
        }
        first = false;
        ui.push_id(("report", s.session_id), |ui| {
            ui.horizontal(|ui| {
                ui.add(
                    egui::Label::new(egui::RichText::new(&s.client_name).color(p.text)).truncate(),
                );
                outcome_badge(ui, s.outcome, s.outcome_source);
                match s.state {
                    SessionState::Active => {
                        theme::pill(ui, "Running", Tone::Positive);
                    }
                    SessionState::Paused => {
                        theme::pill(ui, "Paused", Tone::Warning);
                    }
                    SessionState::Terminated => {}
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if button(
                        ui,
                        true,
                        theme::subtle("Report"),
                        &open_report_name(&s.client_name, s.started_at),
                        "Objective, actions, screenshots, outcome; export or delete",
                    ) {
                        *choice = Some(AgentAction::Details(s.session_id));
                    }
                });
            });
            let ended = s.termination.map_or(String::new(), |c| {
                format!(" \u{b7} {}", termination_text(c))
            });
            small(
                ui,
                &format!(
                    "VM {} \u{b7} {}{ended} \u{b7} {} events \u{b7} {} screenshots",
                    s.vm_name,
                    span_text(s.started_at, s.ended_at),
                    crate::agents::thousands(u64::from(s.events)),
                    s.screenshots
                ),
            );
        });
    }
    for s in unreported {
        if !first {
            theme::divider(ui);
        }
        first = false;
        ui.push_id(("ended", s.session_id), |ui| {
            ui.horizontal(|ui| {
                ui.add(
                    egui::Label::new(egui::RichText::new(&s.client_name).color(p.text)).truncate(),
                );
                theme::pill(ui, "Ended", Tone::Neutral);
                if button(
                    ui,
                    true,
                    theme::subtle("Details"),
                    &format!("Details of the session of {}", s.client_name),
                    "Objective, limits and events of the session",
                ) {
                    *choice = Some(AgentAction::Details(s.session_id));
                }
            });
            let cause = s.termination.map_or("ended", termination_text);
            small(
                ui,
                &format!(
                    "VM {} \u{b7} started {} \u{b7} {cause} \u{b7} no report",
                    env.vm_name(s.vm_id),
                    format_time(s.created_at)
                ),
            );
        });
    }
}

/// Readable name of an agent call recorded by the supervisor.
pub fn call_text(call: &str) -> String {
    match call {
        "list_vms" => "list the VMs".into(),
        "vm_status" => "read the VM state".into(),
        "screenshot" => "take a screenshot".into(),
        "operation" => "read an operation".into(),
        "list_baselines" => "list the baselines".into(),
        "snapshot" => "create a baseline".into(),
        "restore" => "restore a baseline".into(),
        "logs" => "read the logs".into(),
        "end_session" => "end the session".into(),
        "submit.start" => "start the VM".into(),
        "submit.stop" => "stop the VM".into(),
        "submit.force_stop" => "force-stop the VM".into(),
        "submit.reset" => "reset the VM".into(),
        "submit.input" => "keyboard and mouse input".into(),
        other => other.to_owned(),
    }
}

/// Short SHA-256: first 16 hex digits.
pub fn short_hash(h: &lightvm_proto::Sha256Digest) -> String {
    let x = h.to_hex();
    format!("{}\u{2026}", &x[..16.min(x.len())])
}

/// One line describing a report event (plain text).
pub fn event_text(e: &ReportEventBody) -> String {
    match e {
        ReportEventBody::Session { state, termination } => match state {
            SessionState::Active => "Session active".into(),
            SessionState::Paused => "Session paused".into(),
            SessionState::Terminated => format!(
                "Session ended: {}",
                termination.map_or("ended", termination_text)
            ),
        },
        ReportEventBody::Call {
            call,
            ok,
            error_code,
            ..
        } => {
            let what = call_text(call);
            match (ok, error_code) {
                (true, _) => format!("Agent: {what}"),
                (false, Some(c)) => format!("Agent: {what} \u{2014} refused ({})", c.as_str()),
                (false, None) => format!("Agent: {what} \u{2014} failed"),
            }
        }
        ReportEventBody::Tool {
            tool,
            ok,
            error_code,
            detail,
        } => {
            let mut s = format!(
                "Tool {tool}: {}",
                match (ok, error_code) {
                    (true, _) => "done".to_owned(),
                    (false, Some(c)) => format!("error ({c})"),
                    (false, None) => "error".to_owned(),
                }
            );
            if !detail.is_null() {
                let d = detail.to_string();
                let d: String = d.chars().filter(|c| !c.is_control()).take(160).collect();
                s.push_str(" \u{b7} ");
                s.push_str(&d);
            }
            s
        }
        ReportEventBody::Screenshot {
            shot,
            width,
            height,
            sha256,
            bytes,
            retained,
            ..
        } => format!(
            "Screenshot #{} \u{b7} {width}\u{d7}{height} \u{b7} {} \u{b7} SHA-256 {}{}",
            shot + 1,
            crate::dashboard::format_bytes(*bytes),
            short_hash(sha256),
            if *retained {
                ""
            } else {
                " \u{b7} not stored (retention limit)"
            }
        ),
        ReportEventBody::VmState { state, .. } => {
            format!("VM {}", crate::dashboard::vm_state(*state).0.to_lowercase())
        }
        ReportEventBody::Verdict { outcome, source } => format!(
            "Outcome set to {} ({})",
            outcome_text(*outcome),
            source_short(*source)
        ),
        ReportEventBody::Declaration { claimed_outcome } => format!(
            "The agent declared {} (its claim, not the outcome)",
            outcome_text(*claimed_outcome).to_lowercase()
        ),
        ReportEventBody::Note { text } => text.clone(),
    }
}

/// Events with time, in a grid; screenshot events get a "Show" button.
pub fn events_grid(
    ui: &mut egui::Ui,
    id: &str,
    session_id: SessionId,
    events: &[ReportEvent],
    show_buttons: bool,
    choice: &mut Option<AgentAction>,
) {
    let p = theme::Palette::of(ui);
    egui::Grid::new(id)
        .num_columns(3)
        .spacing([14.0, 4.0])
        .show(ui, |ui| {
            for e in events {
                ui.label(
                    egui::RichText::new(format!("{}", e.seq))
                        .small()
                        .color(p.text_weak),
                );
                ui.label(egui::RichText::new(clock(e.at)).color(p.text_weak));
                ui.horizontal(|ui| {
                    ui.add(egui::Label::new(event_text(&e.body)).wrap());
                    if show_buttons
                        && let ReportEventBody::Screenshot {
                            shot,
                            retained: true,
                            ..
                        } = e.body
                        && button(
                            ui,
                            true,
                            theme::subtle("Show"),
                            &format!("Show screenshot {}", shot + 1),
                            "Read the stored screenshot and show it here; nothing is saved",
                        )
                    {
                        *choice = Some(AgentAction::ShowShot(session_id, shot));
                    }
                });
                ui.end_row();
            }
        });
}

/// The screenshot image, fitted to `max_width`.
pub fn shot_image(ui: &mut egui::Ui, tex: &egui::TextureHandle, img: &ShotImage, max_width: f32) {
    let [w, h] = img.size;
    let (w, h) = (w.max(1) as f32, h.max(1) as f32);
    let scale = (max_width / w).min(1.0);
    let r = ui.add(egui::Image::new((
        tex.id(),
        egui::vec2(w * scale, h * scale),
    )));
    theme::accessible_name(
        &r,
        WidgetType::Image,
        &format!(
            "Screenshot taken by the agent, {}\u{d7}{}",
            img.size[0], img.size[1]
        ),
    );
}

/// Notice for the last report action on this report, if any.
fn action_notice(ui: &mut egui::Ui, env: &AgentsEnv<'_>, id: SessionId) {
    let Some(a) = &env.state.reports.action else {
        return;
    };
    if a.op.session() != id {
        return;
    }
    let (text, tone) = match (&a.status, a.op) {
        (RequestStatus::Done(ReportOutcome::Updated), ReportOp::Verdict(_)) => {
            ("Your verdict was recorded.".to_owned(), Tone::Positive)
        }
        (RequestStatus::Done(ReportOutcome::Updated), _) => (
            "The automatic check ran: its result is the outcome above.".to_owned(),
            Tone::Positive,
        ),
        (RequestStatus::Done(ReportOutcome::Exported(x)), _) => (export_text(x), Tone::Positive),
        (RequestStatus::Failed(f), op) => (
            format!(
                "{}: {}",
                match op {
                    ReportOp::Verdict(_) => "The verdict was not recorded",
                    ReportOp::Verify(_) => "The automatic check did not run",
                    ReportOp::Export(_) => "The report was not exported",
                    ReportOp::Delete(_) => "The report was not deleted",
                },
                f.message
            ),
            Tone::Danger,
        ),
        _ => return,
    };
    theme::notice(ui, &text, tone);
    ui.add_space(10.0);
}

/// Where an export was written, readable.
pub fn export_text(x: &crate::service::ExportDone) -> String {
    let shots = match x.screenshots {
        0 => String::new(),
        n => {
            let stem = x
                .path
                .file_stem()
                .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
            format!(
                " {n} screenshot{} written to the folder {stem}-screenshots next to it.",
                if n == 1 { "" } else { "s" }
            )
        }
    };
    format!(
        "Report written to {} ({}, SHA-256 {}).{shots}",
        x.path.display(),
        crate::dashboard::format_bytes(x.bytes),
        short_hash(&x.sha256)
    )
}

/// Report sections of the session page.
pub fn report_sections(
    ui: &mut egui::Ui,
    env: &AgentsEnv<'_>,
    r: &SessionReport,
    choice: &mut Option<AgentAction>,
) {
    let id = r.session_id;
    let live = r.state != SessionState::Terminated;
    let busy = env
        .state
        .reports
        .action
        .as_ref()
        .is_some_and(|a| a.status.is_pending());
    action_notice(ui, env, id);

    theme::group_title(ui, "Outcome");
    theme::card(ui, |ui| {
        ui.horizontal(|ui| {
            let o = theme::pill(ui, outcome_text(r.outcome), outcome_tone(r.outcome));
            theme::accessible_name(
                &o,
                WidgetType::Label,
                &format!(
                    "Outcome: {}, {}",
                    outcome_text(r.outcome),
                    source_short(r.outcome_source)
                ),
            );
            plain(ui, &source_long(r));
        });
        small(
            ui,
            "Distinct outcomes: passed, failed, inconclusive, cancelled, timed out, infrastructure error. It comes from how the session ended, from an automatic verifier or from your verdict, never from what the agent says.",
        );
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            if button(
                ui,
                env.active && !busy,
                egui::Button::new("Set verdict…"),
                "Set your verdict",
                "Record your own judgement: passed, failed or inconclusive",
            ) {
                *choice = Some(AgentAction::Verdict(id));
            }
            let shots = r.screenshots_retained > 0;
            if button(
                ui,
                env.active && !busy && shots,
                egui::Button::new("Run automatic check…"),
                "Run the automatic check",
                if shots {
                    "Pixel check of the LightVM test fixture on the agent's last screenshot"
                } else {
                    "Needs a stored screenshot"
                },
            ) {
                *choice = Some(AgentAction::Verify(id));
            }
        });
        if !r.verdicts.is_empty() {
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new("Verdict history (the last one is the outcome)")
                    .small()
                    .color(theme::Palette::of(ui).text_weak),
            );
            egui::Grid::new("lightvm_report_verdicts")
                .num_columns(3)
                .spacing([14.0, 4.0])
                .show(ui, |ui| {
                    for v in &r.verdicts {
                        ui.label(format_time(v.at));
                        ui.label(outcome_text(v.outcome));
                        let who = match &v.verifier {
                            Some(name) => format!("{} ({name})", v.decided_by),
                            None => v.decided_by.clone(),
                        };
                        ui.add(egui::Label::new(format!("{who}: {}", v.rationale)).wrap());
                        ui.end_row();
                    }
                });
        }
    });

    ui.add_space(14.0);
    theme::group_title(ui, "Agent's declaration");
    theme::card(ui, |ui| {
        theme::notice(
            ui,
            "What the agent claims about its own work. It is not the outcome and LightVM does not use it to decide one.",
            Tone::Neutral,
        );
        ui.add_space(6.0);
        match &r.agent_declaration {
            None => small(ui, "The agent made no declaration."),
            Some(d) => {
                let claim = format!(
                    "The agent claims: {}",
                    outcome_text(d.claimed_outcome).to_lowercase()
                );
                plain(ui, &claim);
                // Plain text from the agent: a label, never markup.
                ui.add(egui::Label::new(egui::RichText::new(&d.summary).italics()).wrap());
                small(ui, &format!("Declared on {}", format_time(d.at)));
            }
        }
    });

    ui.add_space(14.0);
    theme::group_title(ui, "Limits and policy");
    theme::card(ui, |ui| {
        theme::row(ui, "Virtual machine", None, |ui| {
            plain(ui, &r.vm_name);
        });
        theme::row(ui, "Allowed", None, |ui| {
            for c in &r.capabilities {
                ui.horizontal(|ui| {
                    ui.label(capability_text(*c));
                    if capability_dangerous(*c) {
                        theme::pill(ui, "Can lose unsaved work", Tone::Danger);
                    }
                });
            }
        });
        theme::row(ui, "Time", None, |ui| {
            ui.label(format!(
                "Approved {} \u{b7} granted until {}",
                format_time(r.started_at),
                format_time(r.expires_at)
            ));
            if let Some(e) = r.ended_at {
                ui.label(format!(
                    "Ended {}: {}",
                    format_time(e),
                    r.termination.map_or("ended", termination_text)
                ));
            }
        });
        theme::row(ui, "Limits", None, |ui| {
            let q = &r.quotas;
            ui.add(
                egui::Label::new(format!(
                    "{} operations \u{b7} {} input events \u{b7} {} screenshots of up to {} \u{b7} {} actions waiting",
                    crate::agents::thousands(u64::from(q.max_operations)),
                    crate::agents::thousands(u64::from(q.max_input_events)),
                    crate::agents::thousands(u64::from(q.max_screenshots)),
                    crate::dashboard::format_bytes(q.max_screenshot_bytes),
                    q.max_queued_operations
                ))
                .wrap(),
            );
        });
        theme::row(ui, "Policy revision", None, |ui| {
            ui.label(format!("{}", r.policy_revision.0));
        });
    });

    ui.add_space(14.0);
    theme::group_title(ui, "Versions and evidence");
    theme::card(ui, |ui| {
        let v = &r.versions;
        theme::row(ui, "Versions", None, |ui| {
            let mut parts = vec![
                format!("LightVM service {}", v.service),
                format!("IPC protocol {}", v.ipc_protocol),
            ];
            if let Some(m) = &v.mcp_protocol {
                parts.push(format!("MCP {m}"));
            }
            if let Some(b) = &v.backend {
                parts.push(format!("backend {b}"));
            }
            if let Some(a) = &v.accelerator {
                parts.push(format!("accelerator {a}"));
            }
            ui.add(egui::Label::new(parts.join(" \u{b7} ")).wrap());
        });
        theme::row(ui, "VM configuration", None, |ui| {
            ui.label(format!(
                "Revision {} \u{b7} SHA-256 {}",
                r.vm_config_revision,
                short_hash(&r.vm_manifest_sha256)
            ));
            ui.label(match r.guest_os {
                lightvm_proto::GuestOs::UbuntuLtsX86_64 => "Guest: Ubuntu LTS x86_64".to_owned(),
                lightvm_proto::GuestOs::OtherLinuxX86_64 => "Guest: other Linux x86_64".to_owned(),
                os => format!("Guest: {}", crate::dashboard::os_label(os)),
            });
        });
        theme::row(ui, "Started from", None, |ui| match &r.baseline {
            None => {
                ui.label("No baseline: the VM's own disk");
            }
            Some(b) => {
                plain(ui, &b.label);
                small(
                    ui,
                    &format!(
                        "Created {}{} \u{b7} disk SHA-256 {}",
                        format_time(b.created_at),
                        if b.clean_shutdown {
                            ""
                        } else {
                            " after an unclean stop"
                        },
                        short_hash(&b.disk_sha256)
                    ),
                );
            }
        });
        theme::row(ui, "Runs", None, |ui| {
            ui.label(match r.run_ids.len() {
                0 => "The VM did not run during the session".to_owned(),
                1 => "1 run of the VM".to_owned(),
                n => format!("{n} runs of the VM"),
            });
        });
        theme::row(ui, "Errors", None, |ui| {
            if r.errors.is_empty() {
                ui.label("No refused or failed call");
            } else {
                let list: Vec<String> = r
                    .errors
                    .iter()
                    .map(|(code, n)| format!("{code}: {n}"))
                    .collect();
                ui.add(egui::Label::new(list.join(" \u{b7} ")).wrap());
            }
        });
        if let Some(h) = &r.report_sha256 {
            theme::row(ui, "Report", None, |ui| {
                ui.label(format!("SHA-256 {}", short_hash(h)));
                small(
                    ui,
                    "Detects changes to the stored report; it is not a seal (anyone with your account can rewrite both).",
                );
            });
        }
    });

    ui.add_space(14.0);
    theme::group_title(ui, "Actions recorded");
    theme::card(ui, |ui| events_ui(ui, env, r, choice));

    ui.add_space(14.0);
    theme::group_title(ui, "Screenshots");
    theme::card(ui, |ui| screenshots_ui(ui, env, r, choice));

    ui.add_space(14.0);
    theme::group_title(ui, "Export and retention");
    theme::card(ui, |ui| {
        small(
            ui,
            "Export writes the report as JSON (for tools) or Markdown (to read) to a file you choose. Typed text is never in it: only its length and SHA-256.",
        );
        small(
            ui,
            "Deleting removes the report and its stored screenshots from this computer; copies you exported are not touched. A live session's report cannot be deleted.",
        );
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            if button(
                ui,
                env.active && !busy,
                egui::Button::new("Export…"),
                "Export the report",
                "Choose JSON or Markdown, then where to save it",
            ) {
                *choice = Some(AgentAction::Export(id));
            }
            if button(
                ui,
                env.active && !busy && !live,
                theme::danger(ui, "Delete report…"),
                "Delete the report and its screenshots",
                if live {
                    "The session is still running"
                } else {
                    "Removes the report and its stored screenshots"
                },
            ) {
                *choice = Some(AgentAction::DeleteReport(id));
            }
        });
    });
}

fn events_ui(
    ui: &mut egui::Ui,
    env: &AgentsEnv<'_>,
    r: &SessionReport,
    choice: &mut Option<AgentAction>,
) {
    let id = r.session_id;
    let reports = &env.state.reports;
    if let Some(e) = reports.events_errors.get(&id) {
        theme::notice(
            ui,
            &format!("Events not read: {}", e.message),
            Tone::Warning,
        );
    }
    let Some(page) = reports.events.get(&id) else {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label("Reading the events…");
        });
        return;
    };
    if page.total == 0 {
        small(ui, "No action recorded.");
        return;
    }
    let first = page.offset + 1;
    let last = page.offset + u32::try_from(page.events.len()).unwrap_or(0);
    ui.horizontal(|ui| {
        ui.label(format!(
            "Events {first}\u{2013}{last} of {}",
            crate::agents::thousands(u64::from(page.total))
        ));
        if button(
            ui,
            page.offset > 0,
            egui::Button::new("Previous"),
            "Previous events",
            "Earlier events",
        ) {
            *choice = Some(AgentAction::EventsPage(
                id,
                page.offset.saturating_sub(EVENTS_PAGE),
            ));
        }
        if button(
            ui,
            last < page.total,
            egui::Button::new("Next"),
            "Next events",
            "Later events",
        ) {
            *choice = Some(AgentAction::EventsPage(id, page.offset + EVENTS_PAGE));
        }
    });
    ui.add_space(4.0);
    events_grid(ui, "lightvm_report_events", id, &page.events, true, choice);
    if r.events_dropped > 0 {
        small(
            ui,
            &format!(
                "{} later events were not recorded (limit of 20,000 per session).",
                crate::agents::thousands(u64::from(r.events_dropped))
            ),
        );
    }
    small(
        ui,
        "Recorded by the LightVM supervisor, not by the agent: every agent call (refusals included), tool results, screenshots by hash and changes of state.",
    );
}

fn screenshots_ui(
    ui: &mut egui::Ui,
    env: &AgentsEnv<'_>,
    r: &SessionReport,
    choice: &mut Option<AgentAction>,
) {
    let total = r.screenshots_retained + r.screenshots_dropped;
    plain(
        ui,
        &match total {
            0 => "The agent took no screenshot.".to_owned(),
            _ => format!(
                "{total} taken by the agent \u{b7} {} stored{}",
                r.screenshots_retained,
                if r.screenshots_dropped > 0 {
                    format!(
                        " \u{b7} {} not stored (retention limits; hash kept)",
                        r.screenshots_dropped
                    )
                } else {
                    String::new()
                }
            ),
        },
    );
    small(
        ui,
        "Screenshots can show anything that was on the guest screen, passwords included. They are kept with the report in the LightVM catalog until you delete it, and shown here only when you press Show next to one of them in the actions above; the app never saves them anywhere.",
    );
    let Some(shown) = env.shown.filter(|s| s.session_id == r.session_id) else {
        return;
    };
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label(format!("Screenshot {}", shown.shot + 1));
        if button(
            ui,
            true,
            theme::subtle("Hide"),
            "Hide the screenshot",
            "Removes it from the screen",
        ) {
            *choice = Some(AgentAction::HideShot);
        }
    });
    match env.state.reports.shot(shown.session_id, shown.shot) {
        Some(RequestStatus::Done(img)) => {
            if let Some(tex) = shown.texture {
                shot_image(ui, tex, img, ui.available_width().min(720.0));
            }
            small(
                ui,
                &format!(
                    "{}\u{d7}{} \u{b7} {} \u{b7} SHA-256 {} (checked)",
                    img.size[0],
                    img.size[1],
                    crate::dashboard::format_bytes(img.png_bytes as u64),
                    img.sha256.to_hex()
                ),
            );
        }
        Some(RequestStatus::Failed(f)) => {
            theme::notice(
                ui,
                &format!("Screenshot not shown: {}", f.message),
                Tone::Warning,
            );
        }
        _ => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Reading the screenshot…");
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_outcome_has_its_own_text() {
        let all = [
            TestOutcome::Passed,
            TestOutcome::Failed,
            TestOutcome::Inconclusive,
            TestOutcome::Cancelled,
            TestOutcome::TimedOut,
            TestOutcome::InfrastructureError,
        ];
        let texts: std::collections::BTreeSet<_> = all.iter().map(|o| outcome_text(*o)).collect();
        assert_eq!(texts.len(), all.len());
        assert_eq!(source_short(OutcomeSource::User), "your verdict");
        assert!(USER_VERDICT_RATIONALE.chars().count() <= 2000);
    }

    #[test]
    fn spans_and_calls_are_readable() {
        let t = |ms| lightvm_proto::UtcTimestamp::from_unix_millis(ms).unwrap();
        assert_eq!(
            span_text(t(1_791_709_440_000), Some(t(1_791_710_400_000))),
            "2026-10-11 09:04 UTC \u{2192} 09:20 UTC"
        );
        assert_eq!(
            span_text(t(1_791_709_440_000), None),
            "2026-10-11 09:04 UTC \u{2192} running"
        );
        assert_eq!(call_text("submit.input"), "keyboard and mouse input");
        assert_eq!(call_text("<b>x</b>"), "<b>x</b>");
        let e = ReportEventBody::Call {
            call: "submit.reset".into(),
            vm_id: None,
            run_id: None,
            operation_id: None,
            ok: false,
            error_code: Some(lightvm_proto::ErrorCode::PermissionDenied),
        };
        assert_eq!(
            event_text(&e),
            "Agent: reset the VM \u{2014} refused (permission_denied)"
        );
    }
}
