//! Agents page (the AI Workspace) and agent session indicator (F4, F5;
//! ROADMAP 7.3, 9, 10 F4/F5).
//!
//! - AI Workspace: the session running now at the top (objective, what the
//!   agent did so far as recorded by the supervisor, its latest screenshot
//!   only when the user asks for it), then the requests, the access switch
//!   and the session history ([`crate::reports`]).
//! - Agent access switch (off at every supervisor start), with what turning
//!   it on allows, and "Pair a client…" (pairing file, [`crate::pairing`]).
//! - Pending session requests: client name and purpose as plain text, what
//!   the agent asks for, the **verification code** shown large; approval
//!   form choosing exactly one VM, capabilities with plain explanations
//!   (the ones that can lose work marked), duration, limits, lease, input
//!   handover; denial.
//! - Sessions: VM, client, state, time left, limits used, input owner;
//!   pause/resume, input control, revocation (confirmed by the app), detail
//!   page with objective, limits, the session report (F5) and the session
//!   events seen by the app.
//! - [`banner_ui`]: indicator on every page (full screen console included)
//!   while a session is live, with Pause, Revoke, Take control and the
//!   Emergency Stop always reachable.
//!
//! The views execute nothing: they return an [`AgentAction`]. Nothing the
//! agent sends is interpreted: client name and purpose are labels, never
//! links or markup. The approval form has no text field.

use std::collections::BTreeSet;

use egui::WidgetType;
use lightvm_proto::ipc::{
    AGENT_DEFAULT_QUOTAS, SessionApproval, SessionInfo, SessionRequestId, SessionRequestInfo,
    VmSummary,
};
use lightvm_proto::{Capability, Quotas, SessionId, SessionState, UtcTimestamp, VmId};

use crate::service::{AgentOp, RequestStatus, ServiceState, termination_text};
use crate::theme::{self, Tone};

/// Title of the Agents page.
pub const TITLE: &str = "AI Workspace";

/// Description under the Agents title.
pub const DESCRIPTION: &str =
    "AI agents can control one VM at a time, only after you approve their request.";

/// What turning agent access on allows (shown next to the switch).
pub const ACCESS_HELP: &str = "Off every time LightVM starts. When on, agent clients paired with this app can ask for a session; nothing is granted until you approve a request here, choosing one VM, what the agent may do and for how long. Turning it off denies waiting requests and revokes active sessions; the VMs keep running.";

/// Capabilities offered in the approval form: capability, plain
/// explanation, the action can lose unsaved work in the guest.
pub const OFFERED: [(Capability, &str, bool); 7] = [
    (Capability::VmList, "See the VM in its list", false),
    (Capability::VmStatus, "Read the VM's state", false),
    (
        Capability::VmScreenshot,
        "See the screen (screenshots)",
        false,
    ),
    (
        Capability::VmInput,
        "Type and use the mouse, only while it has keyboard and mouse control",
        false,
    ),
    (Capability::VmStart, "Start the VM", false),
    (Capability::VmStop, "Stop or force-stop the VM", true),
    (Capability::VmReset, "Reset the VM", true),
];

/// Plain explanation of a capability.
pub fn capability_text(c: Capability) -> &'static str {
    match OFFERED.iter().find(|(x, _, _)| *x == c) {
        Some((_, t, _)) => t,
        None => match c {
            Capability::VmSnapshot => "Create a baseline with the VM off",
            Capability::VmRestore => "Restore a baseline with the VM off",
            Capability::VmLogs => "Read LightVM events and QEMU logs",
            _ => c.as_str(),
        },
    }
}

/// The capability can lose unsaved work in the guest.
pub fn capability_dangerous(c: Capability) -> bool {
    OFFERED.iter().any(|(x, _, d)| *x == c && *d)
}

/// Session durations offered: seconds and label.
pub const DURATIONS: [(u32, &str); 4] = [
    (15 * 60, "15 min"),
    (60 * 60, "1 hour"),
    (4 * 60 * 60, "4 hours"),
    (8 * 60 * 60, "8 hours"),
];

/// Lease lengths offered: milliseconds and label.
pub const LEASES: [(u32, &str); 3] = [(10_000, "10 s"), (5_000, "5 s"), (3_000, "3 s")];

/// Quota level of a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuotaPreset {
    /// For a short check.
    Small,
    /// The supervisor's default.
    Standard,
    /// For a long task.
    Large,
}

impl QuotaPreset {
    /// All levels, in order.
    pub const ALL: [Self; 3] = [Self::Small, Self::Standard, Self::Large];

    /// Visible name.
    pub fn label(self) -> &'static str {
        match self {
            Self::Small => "Small",
            Self::Standard => "Standard",
            Self::Large => "Large",
        }
    }

    /// Quotas of the level.
    pub fn quotas(self) -> Quotas {
        match self {
            Self::Small => Quotas {
                max_operations: 1_000,
                max_input_events: 20_000,
                max_screenshots: 200,
                max_screenshot_bytes: 4 * 1024 * 1024,
                max_queued_operations: 4,
            },
            Self::Standard => AGENT_DEFAULT_QUOTAS,
            Self::Large => Quotas {
                max_operations: 50_000,
                max_input_events: 1_000_000,
                max_screenshots: 10_000,
                max_screenshot_bytes: lightvm_proto::ipc::AGENT_MAX_SCREENSHOT_BYTES,
                max_queued_operations: 8,
            },
        }
    }

    /// One line on what the level allows.
    pub fn description(self) -> String {
        let q = self.quotas();
        format!(
            "{} operations, {} input events, {} screenshots",
            thousands(u64::from(q.max_operations)),
            thousands(u64::from(q.max_input_events)),
            thousands(u64::from(q.max_screenshots)),
        )
    }
}

/// Number with thousands separators: `10,000`.
pub fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Approval form of a session request (no text field).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApproveForm {
    /// Request reviewed.
    pub request_id: SessionRequestId,
    /// The single VM of the session.
    pub vm: Option<VmId>,
    /// Capabilities granted.
    pub caps: BTreeSet<Capability>,
    /// Duration in seconds.
    pub duration_s: u32,
    /// Limits.
    pub quotas: QuotaPreset,
    /// Lease in milliseconds.
    pub lease_ms: u32,
    /// Hand keyboard and mouse to the agent at once.
    pub give_input: bool,
    /// The user confirmed that the agent shows the same code.
    pub code_matches: bool,
    /// The approval was sent.
    pub sent: bool,
}

impl ApproveForm {
    /// Form for `req`: the VM preselected only if the agent asked for exactly
    /// one known VM; the capabilities it asked for that cannot lose work (or
    /// list, status and screenshots), never the ones that can.
    pub fn new(req: &SessionRequestInfo, vms: &[VmSummary]) -> Self {
        let known: Vec<VmId> = req
            .requested_vms
            .iter()
            .copied()
            .filter(|id| vms.iter().any(|v| v.vm_id == *id))
            .collect();
        let vm = (known.len() == 1).then(|| known[0]);
        let mut caps: BTreeSet<Capability> = req
            .requested_capabilities
            .iter()
            .copied()
            .filter(|c| OFFERED.iter().any(|(x, _, d)| x == c && !d))
            .collect();
        if caps.is_empty() {
            caps = [
                Capability::VmList,
                Capability::VmStatus,
                Capability::VmScreenshot,
            ]
            .into();
        }
        caps.insert(Capability::VmList);
        caps.insert(Capability::VmStatus);
        Self {
            request_id: req.request_id,
            vm,
            caps,
            duration_s: DURATIONS[1].0,
            quotas: QuotaPreset::Standard,
            lease_ms: LEASES[0].0,
            give_input: false,
            code_matches: false,
            sent: false,
        }
    }

    /// The approval, once the form is complete.
    pub fn approval(&self) -> Option<SessionApproval> {
        let vm_id = self.vm?;
        if self.caps.is_empty() || !self.code_matches {
            return None;
        }
        let quotas = match self.quotas {
            QuotaPreset::Standard => None,
            q => Some(q.quotas()),
        };
        Some(SessionApproval {
            vm_id,
            capabilities: self.caps.iter().copied().collect(),
            duration_s: self.duration_s,
            quotas,
            lease_ms: (self.lease_ms != LEASES[0].0).then_some(self.lease_ms),
            give_input: self.give_input && self.caps.contains(&Capability::VmInput),
            network_endpoints: Vec::new(),
        })
    }
}

/// Choice made on the Agents page, its session page or the indicator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentAction {
    /// Agent access on or off.
    SetAccess(bool),
    /// Open the "Pair a client" dialog.
    Pair,
    /// Open the approval form of a request.
    Review(SessionRequestId),
    /// Close the approval form.
    CancelReview,
    /// Approve a request.
    Approve(SessionRequestId, SessionApproval),
    /// Deny a request.
    Deny(SessionRequestId),
    /// Pause a session.
    Pause(SessionId),
    /// Resume a session.
    Resume(SessionId),
    /// Ask to revoke a session (confirmation dialog).
    Revoke(SessionId),
    /// Take keyboard and mouse back.
    TakeInput(VmId),
    /// Hand keyboard and mouse to a session.
    GiveInput(VmId, SessionId),
    /// Open the session page.
    Details(SessionId),
    /// Back to the Agents page.
    Back,
    /// Go to the Agents page (from the indicator).
    Open,
    /// Emergency Stop.
    EmergencyStop,
    /// Re-read access, requests, sessions and reports.
    Refresh,
    /// Read another page of the events of a report (offset).
    EventsPage(SessionId, u32),
    /// Show a stored screenshot of a report (read on demand).
    ShowShot(SessionId, u32),
    /// Hide the screenshot shown on the session page.
    HideShot,
    /// Show (`Some`) or hide the latest screenshot of a live session in the
    /// AI Workspace.
    WatchScreen(Option<SessionId>),
    /// Open the verdict dialog.
    Verdict(SessionId),
    /// Open the automatic check dialog.
    Verify(SessionId),
    /// Open the export dialog.
    Export(SessionId),
    /// Open the report deletion dialog.
    DeleteReport(SessionId),
}

/// A screenshot of a report the user asked to see, and its texture once
/// read and decoded.
#[derive(Clone, Copy)]
pub struct Shown<'a> {
    /// Session.
    pub session_id: SessionId,
    /// Screenshot index in the report.
    pub shot: u32,
    /// Texture, once the screenshot is read.
    pub texture: Option<&'a egui::TextureHandle>,
}

impl std::fmt::Debug for Shown<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shown")
            .field("session_id", &self.session_id)
            .field("shot", &self.shot)
            .field("texture", &self.texture.map(egui::TextureHandle::id))
            .finish()
    }
}

/// What the Agents views need besides the supervisor state.
#[derive(Clone, Copy, Debug)]
pub struct AgentsEnv<'a> {
    /// Supervisor state.
    pub state: &'a ServiceState,
    /// Supervisor connected.
    pub active: bool,
    /// Current instant (fixed in tests).
    pub now: UtcTimestamp,
    /// Screenshot shown on the session page.
    pub shown: Option<Shown<'a>>,
    /// Live session whose latest screenshot the AI Workspace shows.
    pub watch: Option<SessionId>,
    /// That screenshot, once its index is known.
    pub watch_shot: Option<Shown<'a>>,
}

impl<'a> AgentsEnv<'a> {
    /// Environment without screenshots.
    pub fn new(state: &'a ServiceState, active: bool, now: UtcTimestamp) -> Self {
        Self {
            state,
            active,
            now,
            shown: None,
            watch: None,
            watch_shot: None,
        }
    }

    /// Name of a VM of the catalog.
    pub fn vm_name(&self, id: VmId) -> String {
        self.state
            .vm(id)
            .map_or_else(|| "a deleted VM".into(), |v| v.name.as_str().to_owned())
    }

    /// A control of this kind is waiting for the supervisor.
    fn control_pending(&self, f: impl Fn(AgentOp) -> bool) -> bool {
        self.state
            .agents
            .control
            .as_ref()
            .is_some_and(|c| c.status.is_pending() && f(c.op))
    }
}

/// Time left until `until`, readable: `52 min left`, `1 h 05 min left`.
pub fn time_left(now: UtcTimestamp, until: UtcTimestamp) -> String {
    let Ok(ms) = u64::try_from(until.millis_since(now)) else {
        return "time is up".into();
    };
    if ms == 0 {
        return "time is up".into();
    }
    let s = ms.div_ceil(1000);
    if s < 60 {
        format!("{s} s left")
    } else if s < 3600 {
        format!("{} min left", s.div_ceil(60))
    } else {
        let m = s.div_ceil(60);
        format!("{} h {:02} min left", m / 60, m % 60)
    }
}

/// Countdown `m:ss` until `until` (requests).
fn countdown(now: UtcTimestamp, until: UtcTimestamp) -> String {
    let s = u64::try_from(until.millis_since(now))
        .unwrap_or(0)
        .div_ceil(1000);
    format!("{}:{:02}", s / 60, s % 60)
}

/// Clock time, to the second: `10:05:12 UTC`.
pub fn clock(t: UtcTimestamp) -> String {
    let s = t.to_string();
    s.get(11..19)
        .map_or_else(|| s.clone(), |h| format!("{h} UTC"))
}

fn state_pill(ui: &mut egui::Ui, s: &SessionInfo) {
    match s.state {
        SessionState::Active => theme::pill(ui, "Active", Tone::Positive),
        SessionState::Paused => theme::pill(ui, "Paused", Tone::Warning),
        SessionState::Terminated => theme::pill(ui, "Ended", Tone::Neutral),
    };
}

fn button(ui: &mut egui::Ui, enabled: bool, b: egui::Button<'_>, name: &str, hover: &str) -> bool {
    let r = ui
        .add_enabled(enabled, b)
        .on_hover_text(hover)
        .on_disabled_hover_text(hover);
    theme::accessible_name(&r, WidgetType::Button, name);
    r.clicked()
}

fn small(ui: &mut egui::Ui, text: &str) {
    let p = theme::Palette::of(ui);
    ui.add(egui::Label::new(egui::RichText::new(text).small().color(p.text_weak)).wrap());
}

/// AI Workspace: the session running now, requests, agent access and
/// pairing, session history.
pub fn agents_ui(
    ui: &mut egui::Ui,
    env: &AgentsEnv<'_>,
    form: &mut Option<ApproveForm>,
) -> Option<AgentAction> {
    let mut choice = None;
    let st = &env.state.agents;
    theme::page_header(ui, TITLE, DESCRIPTION, |ui| {
        if ui
            .add_enabled(env.active, egui::Button::new("Refresh"))
            .clicked()
        {
            choice = Some(AgentAction::Refresh);
        }
    });
    if let Some(e) = &st.error {
        theme::notice(ui, &e.message, Tone::Warning);
        ui.add_space(10.0);
    }
    if let Some(c) = &st.control
        && let RequestStatus::Failed(f) = &c.status
    {
        theme::notice(ui, &control_error(c.op, f), Tone::Danger);
        ui.add_space(10.0);
    }
    if let Some(a) = &env.state.reports.action
        && let crate::service::ReportOp::Delete(_) = a.op
        && let RequestStatus::Done(_) = a.status
    {
        theme::notice(
            ui,
            "Report deleted, with its stored screenshots.",
            Tone::Positive,
        );
        ui.add_space(10.0);
    }

    theme::group_title(ui, "Current session");
    let live: Vec<&SessionInfo> = st.live().collect();
    theme::card(ui, |ui| {
        if live.is_empty() {
            crate::empty_state(
                ui,
                "No agent session is active.",
                "One session at a time; it ends when its time runs out, when you revoke it or with the Emergency Stop.",
            );
            return;
        }
        for (i, s) in live.iter().enumerate() {
            if i > 0 {
                theme::divider(ui);
            }
            ui.push_id(("session", s.session_id), |ui| {
                workspace_session(ui, env, s, &mut choice);
            });
        }
    });

    ui.add_space(14.0);
    theme::group_title(ui, "Session requests");
    theme::card(ui, |ui| {
        if st.requests.is_empty() {
            crate::empty_state(
                ui,
                "No session requests.",
                if st.access == Some(true) {
                    "A paired agent client asks for a session; it appears here with its verification code."
                } else {
                    "Turn agent access on to let paired clients ask for a session."
                },
            );
            return;
        }
        for (i, r) in st.requests.iter().enumerate() {
            if i > 0 {
                theme::divider(ui);
            }
            ui.push_id(("request", r.request_id), |ui| {
                request_ui(ui, env, r, form, &mut choice);
            });
        }
    });

    ui.add_space(14.0);
    theme::group_title(ui, "Agent access");
    theme::card(ui, |ui| access_ui(ui, env, &mut choice));

    ui.add_space(14.0);
    theme::group_title(ui, "Session history");
    theme::card(ui, |ui| crate::reports::history_ui(ui, env, &mut choice));
    choice
}

/// Readable outcome of a failed control.
pub fn control_error(op: AgentOp, f: &crate::service::Failure) -> String {
    let what = match op {
        AgentOp::SetAccess(true) => "Agent access was not turned on",
        AgentOp::SetAccess(false) => "Agent access was not turned off",
        AgentOp::Deny(_) => "The request was not denied",
        AgentOp::Pause(_) => "The session was not paused",
        AgentOp::Resume(_) => "The session was not resumed",
        AgentOp::Revoke(_) => "The session was not revoked",
        AgentOp::InputOwner(_, lightvm_proto::ipc::InputOwner::User) => {
            "Keyboard and mouse control was not taken back"
        }
        AgentOp::InputOwner(..) => "Keyboard and mouse were not handed to the agent",
    };
    format!("{what}: {}", f.message)
}

fn access_ui(ui: &mut egui::Ui, env: &AgentsEnv<'_>, choice: &mut Option<AgentAction>) {
    let st = &env.state.agents;
    theme::row(ui, "Agent access", Some(ACCESS_HELP), |ui| {
        let pending = env.control_pending(|op| matches!(op, AgentOp::SetAccess(_)));
        let mut on = st.access == Some(true);
        let r = ui.add_enabled(
            env.active && st.access.is_some() && !pending,
            egui::Checkbox::new(&mut on, "Allow agents to request sessions"),
        );
        if r.changed() {
            *choice = Some(AgentAction::SetAccess(on));
        }
        let (text, tone) = match st.access {
            None => ("Not read yet", Tone::Neutral),
            Some(true) => ("On: paired clients can ask for a session", Tone::Warning),
            Some(false) => ("Off: agents cannot ask for sessions", Tone::Neutral),
        };
        theme::status(ui, text, tone);
        small(
            ui,
            "LightVM limits what an agent can do through LightVM only; it does not limit what the agent's own app can already do on this computer.",
        );
    });
    theme::divider(ui);
    theme::row(
        ui,
        "Pair a client",
        Some(
            "Writes a pairing credential, valid 10 minutes, to a file only your user account can read. The LightVM MCP adapter reads it from there. It is never shown, copied or logged, and it grants nothing until you approve a request.",
        ),
        |ui| {
            let on = st.access == Some(true);
            let hover = if on {
                "Create the pairing file for an agent client"
            } else {
                "Turn agent access on first"
            };
            if button(
                ui,
                env.active && on && !st.pairing.is_pending(),
                egui::Button::new("Pair a client…"),
                "Pair a client",
                hover,
            ) {
                *choice = Some(AgentAction::Pair);
            }
            match &st.pairing {
                RequestStatus::Done(f) => {
                    let expired = f.expires_at.millis_since(env.now) <= 0;
                    if expired {
                        small(ui, "The last pairing credential has expired.");
                    } else {
                        small(
                            ui,
                            &format!(
                                "Pairing file ready until {}: {}",
                                clock(f.expires_at),
                                f.path.display()
                            ),
                        );
                    }
                }
                RequestStatus::Failed(f) => {
                    theme::notice(ui, &format!("Pairing failed: {}", f.message), Tone::Danger);
                }
                RequestStatus::Pending => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Writing the pairing file…");
                    });
                }
                RequestStatus::Idle => {}
            }
        },
    );
}

fn requested_text(env: &AgentsEnv<'_>, r: &SessionRequestInfo) -> String {
    let vms = if r.requested_vms.is_empty() {
        "no VM in particular".to_owned()
    } else {
        r.requested_vms
            .iter()
            .map(|id| env.vm_name(*id))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let caps = if r.requested_capabilities.is_empty() {
        "nothing in particular".to_owned()
    } else {
        r.requested_capabilities
            .iter()
            .map(|c| capability_text(*c).to_lowercase())
            .collect::<Vec<_>>()
            .join("; ")
    };
    format!("Asks for {vms}: {caps}.")
}

/// The verification code, large, with its accessible name.
fn code_ui(ui: &mut egui::Ui, code: &str) {
    let p = theme::Palette::of(ui);
    let r = ui.add(egui::Label::new(
        egui::RichText::new(code)
            .size(30.0)
            .monospace()
            .strong()
            .color(p.text),
    ));
    theme::accessible_name(&r, WidgetType::Label, &format!("Verification code {code}"));
}

fn request_ui(
    ui: &mut egui::Ui,
    env: &AgentsEnv<'_>,
    r: &SessionRequestInfo,
    form: &mut Option<ApproveForm>,
    choice: &mut Option<AgentAction>,
) {
    let p = theme::Palette::of(ui);
    ui.horizontal(|ui| {
        // Plain text from the agent: a label, never markup.
        ui.add(
            egui::Label::new(egui::RichText::new(&r.client_name).size(15.0).color(p.text))
                .truncate(),
        );
        theme::pill(ui, "Waiting for you", Tone::Accent);
        ui.label(
            egui::RichText::new(format!("expires in {}", countdown(env.now, r.expires_at)))
                .small()
                .color(p.text_weak),
        );
    });
    ui.label(
        egui::RichText::new("Purpose stated by the agent")
            .small()
            .color(p.text_weak),
    );
    ui.add(egui::Label::new(egui::RichText::new(&r.purpose).color(p.text)).wrap());
    small(ui, &requested_text(env, r));
    ui.add_space(4.0);
    ui.label(
        egui::RichText::new("Verification code")
            .small()
            .color(p.text_weak),
    );
    code_ui(ui, &r.verification_code);
    small(
        ui,
        "Approve only if the agent shows exactly the same code. If it does not, deny the request.",
    );
    ui.add_space(6.0);
    let reviewing = form.as_ref().is_some_and(|f| f.request_id == r.request_id);
    if reviewing {
        if let Some(f) = form.as_mut() {
            approve_form_ui(ui, env, r, f, choice);
        }
        return;
    }
    let client = &r.client_name;
    ui.horizontal(|ui| {
        if button(
            ui,
            env.active,
            theme::primary(ui, "Review and approve…"),
            &format!("Review the request of {client}"),
            "Choose the VM, what the agent may do and for how long",
        ) {
            *choice = Some(AgentAction::Review(r.request_id));
        }
        if button(
            ui,
            env.active && !env.control_pending(|op| op == AgentOp::Deny(r.request_id)),
            theme::danger(ui, "Deny"),
            &format!("Deny the request of {client}"),
            "The agent is told the request was denied",
        ) {
            *choice = Some(AgentAction::Deny(r.request_id));
        }
    });
}

fn approve_form_ui(
    ui: &mut egui::Ui,
    env: &AgentsEnv<'_>,
    r: &SessionRequestInfo,
    f: &mut ApproveForm,
    choice: &mut Option<AgentAction>,
) {
    let approval = env
        .state
        .agents
        .approval
        .as_ref()
        .filter(|a| a.request_id == r.request_id);
    let pending = f.sent && approval.is_some_and(|a| a.status.is_pending());
    let editable = !pending;
    theme::divider(ui);
    theme::row(
        ui,
        "Virtual machine",
        Some("The agent gets exactly one VM; every other VM stays out of its reach."),
        |ui| {
            if env.state.vms.is_empty() {
                small(ui, "The catalog has no VM.");
            }
            for v in &env.state.vms {
                ui.horizontal(|ui| {
                    ui.add_enabled_ui(editable, |ui| {
                        ui.radio_value(&mut f.vm, Some(v.vm_id), v.name.as_str());
                    });
                    let (s, tone) = crate::dashboard::vm_state(v.state);
                    theme::pill(ui, s, tone);
                });
            }
        },
    );
    theme::divider(ui);
    theme::row(
        ui,
        "What it may do",
        Some("Anything not ticked is refused by the supervisor."),
        |ui| {
            for (c, text, dangerous) in OFFERED {
                ui.horizontal(|ui| {
                    let mut on = f.caps.contains(&c);
                    if ui
                        .add_enabled(editable, egui::Checkbox::new(&mut on, text))
                        .changed()
                    {
                        if on {
                            f.caps.insert(c);
                        } else {
                            f.caps.remove(&c);
                        }
                    }
                    if dangerous {
                        theme::pill(ui, "Can lose unsaved work", Tone::Danger);
                    }
                });
            }
        },
    );
    theme::divider(ui);
    let input = f.caps.contains(&Capability::VmInput);
    if !input {
        f.give_input = false;
    }
    theme::row(
        ui,
        "Keyboard and mouse",
        Some(
            "Only one of you types at a time. While the agent has control, your input in the console is not sent; you can take control back at any moment.",
        ),
        |ui| {
            ui.add_enabled(
                editable && input,
                egui::Checkbox::new(
                    &mut f.give_input,
                    "Give keyboard and mouse to the agent now",
                ),
            )
            .on_disabled_hover_text("Needs \u{201c}Type and use the mouse\u{201d}");
        },
    );
    theme::divider(ui);
    theme::row(
        ui,
        "Duration",
        Some("The session ends by itself after this time."),
        |ui| {
            ui.add_enabled_ui(editable, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    for (s, label) in DURATIONS {
                        ui.selectable_value(&mut f.duration_s, s, label);
                    }
                });
            });
        },
    );
    theme::divider(ui);
    theme::row(
        ui,
        "Limits",
        Some("Over the limit the supervisor refuses further actions."),
        |ui| {
            ui.add_enabled_ui(editable, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    for q in QuotaPreset::ALL {
                        ui.selectable_value(&mut f.quotas, q, q.label());
                    }
                });
            });
            small(ui, &f.quotas.description());
        },
    );
    theme::divider(ui);
    theme::row(
        ui,
        "If the agent stops answering",
        Some(
            "The agent must check in within this time; otherwise the session ends and its VM is stopped.",
        ),
        |ui| {
            ui.add_enabled_ui(editable, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    for (ms, label) in LEASES {
                        ui.selectable_value(&mut f.lease_ms, ms, label);
                    }
                });
            });
        },
    );
    theme::divider(ui);
    ui.add_enabled(
        editable,
        egui::Checkbox::new(
            &mut f.code_matches,
            format!("The agent shows the code {}", r.verification_code),
        ),
    );
    if f.sent
        && let Some(RequestStatus::Failed(e)) = approval.map(|a| &a.status)
    {
        ui.add_space(6.0);
        theme::notice(ui, &approval_error(e), Tone::Danger);
    }
    if pending {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label("Waiting for the supervisor…");
        });
    }
    ui.add_space(8.0);
    let client = &r.client_name;
    ui.horizontal(|ui| {
        if button(
            ui,
            !pending,
            egui::Button::new("Cancel"),
            "Cancel the review",
            "Back to the request; nothing is sent",
        ) {
            *choice = Some(AgentAction::CancelReview);
        }
        if button(
            ui,
            env.active && !pending,
            theme::danger(ui, "Deny"),
            &format!("Deny the request of {client}"),
            "The agent is told the request was denied",
        ) {
            *choice = Some(AgentAction::Deny(r.request_id));
        }
        let ready = f.approval();
        let hover = if f.vm.is_none() {
            "Choose the VM"
        } else if f.caps.is_empty() {
            "Tick at least one action"
        } else if !f.code_matches {
            "Confirm that the agent shows the same code"
        } else {
            "The agent receives the session"
        };
        if button(
            ui,
            env.active && !pending && ready.is_some(),
            theme::primary(ui, "Approve session"),
            &format!("Approve the session of {client}"),
            hover,
        ) && let Some(a) = ready
        {
            *choice = Some(AgentAction::Approve(r.request_id, a));
        }
    });
}

/// Readable outcome of a failed approval.
pub fn approval_error(f: &crate::service::Failure) -> String {
    use lightvm_proto::ErrorCode;
    match f.code {
        Some(ErrorCode::ResourceLimit) => {
            "Another agent session is active: only one at a time. End it first.".into()
        }
        Some(ErrorCode::NotFound) => {
            "The request is no longer waiting (expired, withdrawn or already decided), or the VM no longer exists.".into()
        }
        Some(ErrorCode::StateConflict) => {
            "The agent disconnected meanwhile: the request was withdrawn.".into()
        }
        _ => format!("Not approved: {}", f.message),
    }
}

/// Input owner of the session's VM, as a short text.
fn input_text(env: &AgentsEnv<'_>, s: &SessionInfo) -> &'static str {
    if env
        .state
        .agents
        .input_session(s.vm_id)
        .is_some_and(|x| x.session_id == s.session_id)
    {
        "agent"
    } else {
        "you"
    }
}

/// Usage line: `12 of 10,000 operations · 340 input events · 4 screenshots`.
pub fn usage_text(s: &SessionInfo) -> String {
    format!(
        "{} of {} operations \u{b7} {} of {} input events \u{b7} {} of {} screenshots",
        thousands(u64::from(s.usage.operations)),
        thousands(u64::from(s.quotas.max_operations)),
        thousands(u64::from(s.usage.input_events)),
        thousands(u64::from(s.quotas.max_input_events)),
        thousands(u64::from(s.usage.screenshots)),
        thousands(u64::from(s.quotas.max_screenshots)),
    )
}

/// Pause/Resume, input control and Revoke of a live session. In the
/// indicator (`banner`) the accessible names say "the agent session", so
/// they stay distinct from the same controls on the Agents page.
fn session_controls(
    ui: &mut egui::Ui,
    env: &AgentsEnv<'_>,
    s: &SessionInfo,
    banner: bool,
    choice: &mut Option<AgentAction>,
) {
    let name = |page: String, indicator: &str| {
        if banner { indicator.to_owned() } else { page }
    };
    let client = &s.client_name;
    let id = s.session_id;
    let busy = env.control_pending(|op| match op {
        AgentOp::Pause(x) | AgentOp::Resume(x) | AgentOp::Revoke(x) => x == id,
        AgentOp::InputOwner(vm, _) => vm == s.vm_id,
        _ => false,
    });
    let ok = env.active && !busy;
    if s.state == SessionState::Paused {
        if button(
            ui,
            ok,
            egui::Button::new("Resume"),
            &name(
                format!("Resume the session of {client}"),
                "Resume the agent session",
            ),
            "The agent can act again",
        ) {
            *choice = Some(AgentAction::Resume(id));
        }
    } else if button(
        ui,
        ok,
        egui::Button::new("Pause"),
        &name(
            format!("Pause the session of {client}"),
            "Pause the agent session",
        ),
        "The agent can still look, but its actions are refused; the VM keeps running",
    ) {
        *choice = Some(AgentAction::Pause(id));
    }
    let agent_input = input_text(env, s) == "agent";
    if agent_input {
        if button(
            ui,
            ok,
            egui::Button::new("Take control"),
            &name(
                format!("Take keyboard and mouse back from {client}"),
                "Take keyboard and mouse back from the agent",
            ),
            "Your keyboard and mouse go to the VM again; keys held by the agent are released",
        ) {
            *choice = Some(AgentAction::TakeInput(s.vm_id));
        }
    } else if s.capabilities.contains(&Capability::VmInput)
        && button(
            ui,
            ok && s.state == SessionState::Active,
            egui::Button::new("Give control"),
            &name(
                format!("Give keyboard and mouse to {client}"),
                "Give keyboard and mouse to the agent",
            ),
            "The agent types and uses the mouse; your input is not sent until you take control back",
        )
    {
        *choice = Some(AgentAction::GiveInput(s.vm_id, id));
    }
    if button(
        ui,
        ok,
        theme::danger(ui, "Revoke…"),
        &name(
            format!("Revoke the session of {client}"),
            "Revoke the agent session",
        ),
        "Ends the session at once; the VM keeps running",
    ) {
        *choice = Some(AgentAction::Revoke(id));
    }
}

/// A live session in the AI Workspace: who, on which VM, the objective,
/// what the agent did so far (recorded by the supervisor), its latest
/// screenshot when the user asks for it, and the controls.
fn workspace_session(
    ui: &mut egui::Ui,
    env: &AgentsEnv<'_>,
    s: &SessionInfo,
    choice: &mut Option<AgentAction>,
) {
    let p = theme::Palette::of(ui);
    ui.horizontal(|ui| {
        ui.add(
            egui::Label::new(egui::RichText::new(&s.client_name).size(15.0).color(p.text))
                .truncate(),
        );
        state_pill(ui, s);
        let input = input_text(env, s);
        theme::pill(
            ui,
            &format!("Keyboard and mouse: {input}"),
            if input == "agent" {
                Tone::Warning
            } else {
                Tone::Neutral
            },
        );
    });
    small(
        ui,
        &format!(
            "VM {} \u{b7} {}",
            env.vm_name(s.vm_id),
            time_left(env.now, s.expires_at)
        ),
    );
    ui.add_space(4.0);
    ui.label(
        egui::RichText::new("Objective stated by the agent")
            .small()
            .color(p.text_weak),
    );
    ui.add(egui::Label::new(egui::RichText::new(&s.purpose).color(p.text)).wrap());
    ui.add_space(4.0);
    ui.label(egui::RichText::new("So far").small().color(p.text_weak));
    small(ui, &usage_text(s));
    let reports = &env.state.reports;
    if let Some(tail) = reports.tail.get(&s.session_id)
        && !tail.events.is_empty()
    {
        let mut ignore = None;
        crate::reports::events_grid(
            ui,
            "lightvm_workspace_events",
            s.session_id,
            &tail.events,
            false,
            &mut ignore,
        );
        small(
            ui,
            &format!(
                "Last {} of {} actions recorded by LightVM; all of them are in the report.",
                tail.events.len(),
                thousands(u64::from(tail.total))
            ),
        );
    }
    ui.add_space(4.0);
    screen_ui(ui, env, s, choice);
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        session_controls(ui, env, s, false, choice);
        if button(
            ui,
            true,
            theme::subtle("Details"),
            &format!("Details of the session of {}", s.client_name),
            "Objective, limits, actions, screenshots and outcome of the session",
        ) {
            *choice = Some(AgentAction::Details(s.session_id));
        }
    });
}

/// Latest screenshot of the agent, only after the user asked for it.
fn screen_ui(
    ui: &mut egui::Ui,
    env: &AgentsEnv<'_>,
    s: &SessionInfo,
    choice: &mut Option<AgentAction>,
) {
    let header = env.state.reports.headers.get(&s.session_id);
    let taken = header.map_or(s.usage.screenshots, |h| {
        h.screenshots_retained + h.screenshots_dropped
    });
    if !s.capabilities.contains(&Capability::VmScreenshot) && taken == 0 {
        return;
    }
    if env.watch != Some(s.session_id) {
        if taken == 0 {
            small(ui, "The agent has taken no screenshot yet.");
        } else if button(
            ui,
            true,
            egui::Button::new("Show the agent's latest screenshot"),
            &format!("Show the latest screenshot of {}", s.client_name),
            "Shown here only, kept in memory, never saved by the app; it can show anything on the guest screen",
        ) {
            *choice = Some(AgentAction::WatchScreen(Some(s.session_id)));
        }
        return;
    }
    ui.horizontal(|ui| {
        ui.label("Latest screenshot taken by the agent");
        if button(
            ui,
            true,
            theme::subtle("Hide"),
            &format!("Hide the screenshots of {}", s.client_name),
            "Stop showing the agent's screenshots",
        ) {
            *choice = Some(AgentAction::WatchScreen(None));
        }
    });
    let Some(shown) = env.watch_shot.filter(|w| w.session_id == s.session_id) else {
        small(
            ui,
            if taken == 0 {
                "The agent has taken no screenshot yet."
            } else {
                "Reading the report\u{2026}"
            },
        );
        return;
    };
    match env.state.reports.shot(shown.session_id, shown.shot) {
        Some(RequestStatus::Done(img)) => {
            if let Some(tex) = shown.texture {
                crate::reports::shot_image(ui, tex, img, 360.0);
            }
            small(
                ui,
                &format!(
                    "Screenshot {} \u{b7} {}\u{d7}{} \u{b7} SHA-256 {}",
                    shown.shot + 1,
                    img.size[0],
                    img.size[1],
                    crate::reports::short_hash(&img.sha256)
                ),
            );
        }
        Some(RequestStatus::Failed(f)) => small(
            ui,
            &format!("Screenshot {} not shown: {}", shown.shot + 1, f.message),
        ),
        _ => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Reading the screenshot\u{2026}");
            });
        }
    }
}

/// Session page: objective, state and controls, the session report
/// (outcome, declaration, limits, versions, actions, screenshots, export,
/// deletion) and the session events seen by the app.
pub fn session_page_ui(
    ui: &mut egui::Ui,
    env: &AgentsEnv<'_>,
    id: SessionId,
) -> Option<AgentAction> {
    let mut choice = None;
    let info = env.state.agents.session(id);
    let reports = &env.state.reports;
    let report = reports.headers.get(&id);
    let summary = reports.summary(id);
    let client = info
        .map(|s| s.client_name.clone())
        .or_else(|| report.map(|r| r.client_name.clone()))
        .or_else(|| summary.map(|r| r.client_name.clone()));
    let Some(client) = client else {
        let why = match reports.header_errors.get(&id) {
            Some(f) if f.code == Some(lightvm_proto::ErrorCode::NotFound) => {
                "The report of this session no longer exists."
            }
            Some(_) => "The report of this session could not be read.",
            None => "The session is no longer listed.",
        };
        theme::page_header(ui, "Agent session", why, |ui| {
            if ui.button("Back to Agents").clicked() {
                choice = Some(AgentAction::Back);
            }
        });
        if let Some(f) = reports.header_errors.get(&id) {
            theme::notice(ui, &f.message, Tone::Warning);
        }
        return choice;
    };
    let p = theme::Palette::of(ui);
    let vm_name = match (info, report, summary) {
        (Some(s), _, _) if env.state.vm(s.vm_id).is_some() => env.vm_name(s.vm_id),
        (_, Some(r), _) => r.vm_name.clone(),
        (_, _, Some(r)) => r.vm_name.clone(),
        (Some(s), _, _) => env.vm_name(s.vm_id),
        _ => "a VM".into(),
    };
    theme::page_header(
        ui,
        &format!("Session \u{b7} {client}"),
        &format!("Agent session on {vm_name}"),
        |ui| {
            if ui.button("Back to Agents").clicked() {
                choice = Some(AgentAction::Back);
            }
        },
    );
    let purpose = info
        .map(|s| s.purpose.as_str())
        .or_else(|| report.map(|r| r.objective.as_str()))
        .or_else(|| summary.map(|r| r.objective.as_str()))
        .unwrap_or_default();
    theme::group_title(ui, "Objective");
    theme::card(ui, |ui| {
        ui.label(
            egui::RichText::new("Purpose stated by the agent")
                .small()
                .color(p.text_weak),
        );
        ui.add(egui::Label::new(egui::RichText::new(purpose).color(p.text)).wrap());
    });

    if let Some(s) = info {
        ui.add_space(14.0);
        theme::group_title(ui, "State");
        theme::card(ui, |ui| {
            ui.horizontal(|ui| {
                state_pill(ui, s);
                if s.state == SessionState::Terminated {
                    ui.label(s.termination.map_or("ended", termination_text));
                } else {
                    ui.label(time_left(env.now, s.expires_at));
                    ui.label(format!("\u{b7} keyboard and mouse: {}", input_text(env, s)));
                }
            });
            if s.state != SessionState::Terminated {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    session_controls(ui, env, s, false, &mut choice);
                });
                ui.add(egui::Label::new(usage_text(s)).wrap());
            }
        });
    }

    ui.add_space(14.0);
    match report {
        Some(r) => crate::reports::report_sections(ui, env, r, &mut choice),
        None => {
            theme::group_title(ui, "Report");
            theme::card(ui, |ui| match reports.header_errors.get(&id) {
                Some(f) => {
                    theme::notice(
                        ui,
                        &format!("Report not read: {}", f.message),
                        Tone::Warning,
                    );
                }
                None => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Reading the report\u{2026}");
                    });
                }
            });
            if let Some(s) = info {
                ui.add_space(14.0);
                theme::group_title(ui, "Limits");
                theme::card(ui, |ui| {
                    theme::row(ui, "Virtual machine", None, |ui| {
                        ui.label(env.vm_name(s.vm_id));
                    });
                    theme::row(ui, "Allowed", None, |ui| {
                        for c in &s.capabilities {
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
                            "From {} to {}",
                            crate::baselines::format_time(s.created_at),
                            crate::baselines::format_time(s.expires_at)
                        ));
                    });
                });
            }
        }
    }

    let entries: Vec<_> = env
        .state
        .agents
        .log
        .iter()
        .filter(|e| e.session_id == Some(id))
        .collect();
    if !entries.is_empty() {
        ui.add_space(14.0);
        theme::group_title(ui, "Seen by this app");
        theme::card(ui, |ui| {
            egui::Grid::new("lightvm_session_log")
                .num_columns(2)
                .spacing([18.0, 4.0])
                .show(ui, |ui| {
                    for e in entries {
                        ui.label(egui::RichText::new(clock(e.at)).color(p.text_weak));
                        ui.add(egui::Label::new(&e.text).wrap());
                        ui.end_row();
                    }
                });
            ui.add_space(4.0);
            small(
                ui,
                "Session events this app saw while it was open (request, approval, input handovers, pause, end).",
            );
        });
    }
    choice
}

/// Key of the rectangle of the indicator's Emergency Stop in the context
/// data, for the copy drawn above dialogs.
pub const BANNER_STOP_RECT: &str = "lightvm_banner_emergency_stop";

/// Width kept for the controls of the indicator.
const BANNER_BUTTONS_WIDTH: f32 = 490.0;

/// Indicator shown on every page while an agent session is live (with
/// Pause, Revoke, Take control and the Emergency Stop), or a waiting
/// request when there is none (`show_requests`: not on the Agents page).
pub fn banner_ui(
    ui: &mut egui::Ui,
    env: &AgentsEnv<'_>,
    show_requests: bool,
) -> Option<AgentAction> {
    let mut choice = None;
    let p = theme::Palette::of(ui);
    let st = &env.state.agents;
    let live: Vec<&SessionInfo> = st.live().collect();
    if live.is_empty() {
        if !show_requests {
            return None;
        }
        let r = st.requests.first()?;
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            theme::dot(ui, p.accent);
            let more = if st.requests.len() > 1 {
                format!(" (and {} more)", st.requests.len() - 1)
            } else {
                String::new()
            };
            ui.label(
                egui::RichText::new(format!(
                    "Agent \u{201c}{}\u{201d} asks for a session \u{b7} code {}{more}",
                    r.client_name, r.verification_code
                ))
                .color(p.text),
            );
            if button(
                ui,
                true,
                theme::primary(ui, "Review"),
                "Review the session requests",
                "Open the Agents page",
            ) {
                choice = Some(AgentAction::Open);
            }
        });
        return choice;
    }
    for s in live {
        ui.push_id(("banner", s.session_id), |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                let what = if s.state == SessionState::Paused {
                    "Agent session paused"
                } else {
                    "Agent session active"
                };
                let text = format!(
                    "{what} on {} \u{b7} {} \u{b7} keyboard and mouse: {} \u{b7} {}",
                    env.vm_name(s.vm_id),
                    s.client_name,
                    input_text(env, s),
                    time_left(env.now, s.expires_at)
                );
                // Text on the left (wrapping), the controls on the right:
                // they never overlap, whatever the window width.
                let text_width = (ui.available_width() - BANNER_BUTTONS_WIDTH).max(220.0);
                ui.allocate_ui_with_layout(
                    egui::vec2(text_width, 0.0),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.set_max_width(text_width);
                        theme::dot(ui, p.warning);
                        let r = ui
                            .add(egui::Label::new(egui::RichText::new(&text).color(p.text)).wrap());
                        theme::accessible_name(&r, WidgetType::Label, &text);
                    },
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    // Always enabled: the app asks the supervisor, which
                    // reports if it cannot.
                    let stop = ui.add(theme::danger(ui, "Emergency Stop")).on_hover_text(
                        "Immediately terminates all VMs and revokes the agent sessions",
                    );
                    theme::accessible_name(
                        &stop,
                        WidgetType::Button,
                        "Emergency Stop: stop all VMs and agent sessions",
                    );
                    // Where it is: a dialog draws it again at the same place.
                    ui.ctx()
                        .data_mut(|d| d.insert_temp(egui::Id::new(BANNER_STOP_RECT), stop.rect));
                    if stop.clicked() {
                        choice = Some(AgentAction::EmergencyStop);
                    }
                    session_controls(ui, env, s, true, &mut choice);
                    if button(
                        ui,
                        true,
                        theme::subtle("Details"),
                        "Open the agent session",
                        "Objective, limits and events of the session",
                    ) {
                        choice = Some(AgentAction::Details(s.session_id));
                    }
                });
            });
        });
    }
    choice
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readable_numbers_and_times() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(10_000), "10,000");
        assert_eq!(thousands(1_000_000), "1,000,000");
        let t = |ms| UtcTimestamp::from_unix_millis(ms).unwrap();
        assert_eq!(time_left(t(0), t(45_000)), "45 s left");
        assert_eq!(time_left(t(0), t(52 * 60_000)), "52 min left");
        assert_eq!(time_left(t(0), t(65 * 60_000)), "1 h 05 min left");
        assert_eq!(time_left(t(10), t(0)), "time is up");
        assert_eq!(countdown(t(0), t(252_000)), "4:12");
        assert_eq!(clock(t(1_791_709_920_000)), "09:12:00 UTC");
    }

    #[test]
    fn standard_limits_are_the_supervisor_default() {
        assert_eq!(QuotaPreset::Standard.quotas(), AGENT_DEFAULT_QUOTAS);
        for q in QuotaPreset::ALL {
            assert!(
                q.quotas().max_screenshot_bytes <= lightvm_proto::ipc::AGENT_MAX_SCREENSHOT_BYTES
            );
        }
    }

    #[test]
    fn dangerous_capabilities_are_never_preselected() {
        let req = SessionRequestInfo {
            request_id: 1,
            connection_id: 1,
            client_name: "c".into(),
            peer_pid: None,
            purpose: "p".into(),
            requested_vms: vec![],
            requested_capabilities: [Capability::VmReset, Capability::VmStop, Capability::VmInput]
                .into(),
            requested_endpoints: Vec::new(),
            verification_code: "123-456".into(),
            requested_at: UtcTimestamp::from_unix_millis(0).unwrap(),
            expires_at: UtcTimestamp::from_unix_millis(300_000).unwrap(),
        };
        let f = ApproveForm::new(&req, &[]);
        assert!(f.caps.contains(&Capability::VmInput));
        assert!(!f.caps.contains(&Capability::VmReset));
        assert!(!f.caps.contains(&Capability::VmStop));
        assert_eq!(f.vm, None);
        // Incomplete: no VM, code not confirmed.
        assert!(f.approval().is_none());
    }
}
