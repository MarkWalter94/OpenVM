//! Session reports (ROADMAP 9, F5), recorded by the supervisor thread.
//!
//! One report per agent session, opened at the approval and kept under
//! `<catalog>/sessions/<session_id>/`: `report.json` (canonical JSON, see
//! [`lightvm_proto::report::SessionReport`]), `summary.json` (for the list)
//! and `shots/NNNN.png` (the agent's screenshots, full PNG as produced by the
//! backend, within the retention limits). Reports survive service restarts;
//! sessions do not.
//!
//! What is recorded comes from the service, not from the agent: every agent
//! call with its outcome (refusals included), lifecycle steps, VM state
//! changes, screenshots by hash. The adapter may add bounded notes (MCP tool
//! name, outcome, a summary that never contains typed text) and the agent a
//! declaration: both are labelled as such and never set the outcome. The
//! outcome comes from the termination cause, an automatic verifier or the
//! explicit judgement of the local user ([`OutcomeSource`]).
//!
//! No host path is ever placed in a report or returned to an agent; the
//! export destination is chosen by the user in the app (UI role only).

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use lightvm_proto::report::{
    AgentDeclaration, FixtureMarker, LogEntry, LogSource, MAX_DECLARATION_CHARS, MAX_LOG_ENTRIES,
    MAX_QEMU_LOG_LINES, MAX_RATIONALE_CHARS, MAX_REPORT_EVENTS, MAX_REPORT_EVENTS_PAGE,
    MAX_REPORT_SCREENSHOT_BYTES, MAX_REPORT_SCREENSHOTS, MAX_REPORTS_KEPT, MAX_TOOL_NAME_CHARS,
    MAX_TOOL_NOTE_DETAIL_BYTES, OutcomeSource, REPORT_FORMAT, ReportBaseline, ReportEvent,
    ReportEventBody, ReportFormat, ReportVersions, SessionReport, SessionReportSummary,
    SessionVerifier, Verdict,
};
use lightvm_proto::{
    CapabilitySet, ErrorCode, GuestOs, OperationId, PolicyRevision, PublicError, Quotas, RunId,
    Screenshot, SessionId, SessionState, Sha256Digest, TerminationCause, TestOutcome, UtcTimestamp,
    VmId, VmState,
};

/// Name of the directory of the reports in the catalog.
pub const SESSIONS_DIR: &str = "sessions";
const REPORT_FILE: &str = "report.json";
const SUMMARY_FILE: &str = "summary.json";
const SHOTS_DIR: &str = "shots";
/// Name and version of the fixture verifier recorded in verdicts.
pub const FIXTURE_VERIFIER: &str = "adr-0019-fixture-pixel v1 (lightvm-fixture)";

/// Facts of a new report, gathered at the approval.
#[derive(Debug, Clone)]
pub(crate) struct OpenReport {
    pub(crate) tools: lightvm_proto::ToolGrant,
    pub(crate) session_id: SessionId,
    pub(crate) vm_id: VmId,
    pub(crate) vm_name: String,
    pub(crate) client_name: String,
    pub(crate) objective: String,
    pub(crate) started_at: UtcTimestamp,
    pub(crate) capabilities: CapabilitySet,
    pub(crate) network_endpoints: Vec<String>,
    pub(crate) quotas: Quotas,
    pub(crate) expires_at: UtcTimestamp,
    pub(crate) policy_revision: PolicyRevision,
    pub(crate) guest_os: GuestOs,
    pub(crate) vm_manifest_sha256: Sha256Digest,
    pub(crate) vm_config_revision: u64,
    pub(crate) baseline: Option<ReportBaseline>,
    pub(crate) run_id: Option<RunId>,
    pub(crate) versions: ReportVersions,
}

/// An agent call and its outcome, seen by the service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CallRecord {
    pub(crate) session_id: SessionId,
    pub(crate) call: &'static str,
    pub(crate) vm_id: Option<VmId>,
    pub(crate) run_id: Option<RunId>,
    pub(crate) operation_id: Option<OperationId>,
    pub(crate) ok: bool,
    pub(crate) error: Option<ErrorCode>,
    /// Successful screenshot: its bytes are taken from the engine.
    pub(crate) screenshot: bool,
}

#[derive(Debug)]
struct Live {
    report: SessionReport,
    dirty: bool,
    shot_bytes: u64,
}

/// Reserve kept on the catalog volume for the reports (ROADMAP 8: "una
/// riserva per manifest ed esito minimo"): released when a report cannot be
/// written because the volume is full, so the termination and the outcome
/// still reach the disk; re-created later when space is back.
const RESERVE_FILE: &str = ".reserve";
/// Size of the reserve: a full report at the event limit is a few MiB.
pub(crate) const RESERVE_BYTES: u64 = 8 << 20;
/// Minimum interval between two attempts to re-create a released reserve.
const RESERVE_RETRY: std::time::Duration = std::time::Duration::from_secs(60);

/// Reports of the catalog.
#[derive(Debug, Default)]
pub(crate) struct ReportStore {
    /// `<catalog>/sessions`; `None` = in memory only (tests without catalog).
    dir: Option<PathBuf>,
    /// Reports of live sessions (and of terminated ones not yet written).
    live: BTreeMap<SessionId, Live>,
    /// Size of the reserve file ([`RESERVE_BYTES`]; smaller in tests).
    reserve_bytes: u64,
    /// Last attempt to re-create the reserve after a release.
    reserve_attempt: std::cell::Cell<Option<std::time::Instant>>,
    /// Interval between attempts ([`RESERVE_RETRY`]; zero in tests).
    reserve_retry: std::time::Duration,
    /// Simulated volume (tests only): writes beyond its free bytes fail
    /// with the "disk full" OS error.
    #[cfg(test)]
    volume: Option<std::sync::Arc<tests::Volume>>,
}

fn sha256(bytes: &[u8]) -> Sha256Digest {
    use sha2::Digest as _;
    Sha256Digest::from_bytes(sha2::Sha256::digest(bytes).into())
}

/// Plain text: control characters removed (terminal sequences, markup is
/// left to the viewers, which show it as text), at most `n` characters.
pub(crate) fn plain(s: &str, n: usize) -> String {
    s.chars().filter(|c| !c.is_control()).take(n).collect()
}

/// Default outcome from the termination cause (ROADMAP 9: completing the
/// conversation never yields `passed`).
pub(crate) fn termination_outcome(cause: TerminationCause) -> TestOutcome {
    match cause {
        TerminationCause::Completed | TerminationCause::ClientLost | TerminationCause::Restored => {
            TestOutcome::Inconclusive
        }
        TerminationCause::Revoked
        | TerminationCause::EmergencyStop
        | TerminationCause::UiClosed
        | TerminationCause::HostResumed => TestOutcome::Cancelled,
        TerminationCause::GrantExpired | TerminationCause::LeaseExpired => TestOutcome::TimedOut,
        TerminationCause::VmTerminated | TerminationCause::SupervisorLost => {
            TestOutcome::InfrastructureError
        }
    }
}

fn not_found() -> PublicError {
    PublicError::not_found("session report not found")
}

fn io_error(what: &str, e: &std::io::Error) -> PublicError {
    // The detail (with paths) stays on stderr.
    crate::diag!("lightvm-service: reports: {what}: {e}");
    PublicError::infrastructure(format!("report {what} failed"))
}

/// Canonical bytes of a report and its hash (computed with
/// `report_sha256` absent).
fn canonical(report: &SessionReport) -> (Vec<u8>, Sha256Digest) {
    let mut r = report.clone();
    r.report_sha256 = None;
    let bytes = serde_json::to_vec(&r).unwrap_or_default();
    let digest = sha256(&bytes);
    r.report_sha256 = Some(digest);
    (serde_json::to_vec_pretty(&r).unwrap_or_default(), digest)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

impl ReportStore {
    /// Store under `dir` (created if missing); `None` = memory only.
    pub(crate) fn new(dir: Option<PathBuf>) -> Self {
        if let Some(d) = &dir
            && let Err(e) = std::fs::create_dir_all(d)
        {
            crate::diag!("lightvm-service: reports: directory not created: {e}");
        }
        let mut s = Self {
            dir,
            live: BTreeMap::new(),
            reserve_bytes: RESERVE_BYTES,
            reserve_attempt: std::cell::Cell::new(None),
            reserve_retry: RESERVE_RETRY,
            #[cfg(test)]
            volume: None,
        };
        s.recover();
        s.prune();
        s
    }

    /// Writes `bytes` to `path` (atomic replace). In tests the simulated
    /// volume may refuse it as full.
    fn write_file(&self, path: &Path, bytes: &[u8]) -> std::io::Result<()> {
        #[cfg(test)]
        if let Some(v) = &self.volume {
            v.take(bytes.len() as u64)?;
        }
        write_atomic(path, bytes)
    }

    fn reserve_path(&self) -> Option<PathBuf> {
        self.dir.as_ref().map(|d| d.join(RESERVE_FILE))
    }

    /// Creates the reserve file if missing (best effort; a failure is
    /// retried after [`RESERVE_RETRY`]). Returns whether it exists.
    fn ensure_reserve(&self) -> bool {
        let Some(p) = self.reserve_path() else {
            return false;
        };
        if std::fs::metadata(&p).is_ok_and(|m| m.len() >= self.reserve_bytes) {
            return true;
        }
        let now = std::time::Instant::now();
        if self
            .reserve_attempt
            .get()
            .is_some_and(|t| now.duration_since(t) < self.reserve_retry)
        {
            return false;
        }
        self.reserve_attempt.set(Some(now));
        let zeros = vec![0u8; usize::try_from(self.reserve_bytes).unwrap_or(0)];
        match self.write_file(&p, &zeros) {
            Ok(()) => {
                self.reserve_attempt.set(None);
                true
            }
            Err(e) => {
                let _ = std::fs::remove_file(p.with_extension("tmp"));
                crate::diag!("lightvm-service: reports: storage reserve not created: {e}");
                false
            }
        }
    }

    /// Releases the reserve (volume full): `true` if there was one.
    fn release_reserve(&self) -> bool {
        let Some(p) = self.reserve_path() else {
            return false;
        };
        if std::fs::remove_file(&p).is_err() {
            return false;
        }
        #[cfg(test)]
        if let Some(v) = &self.volume {
            v.give_back(self.reserve_bytes);
        }
        // Released now: the next re-creation waits for the retry interval.
        self.reserve_attempt.set(Some(std::time::Instant::now()));
        true
    }

    fn session_dir(&self, id: SessionId) -> Option<PathBuf> {
        self.dir.as_ref().map(|d| d.join(id.to_string()))
    }

    /// Reports left `active`/`paused` by a service that ended without
    /// closing them (crash): the session did not survive, cause
    /// `supervisor_lost`.
    fn recover(&mut self) {
        for s in self.summaries() {
            if s.state != SessionState::Terminated
                && let Ok(mut r) = self.load(s.session_id)
            {
                let now = UtcTimestamp::now();
                Self::finish(&mut r, TerminationCause::SupervisorLost, now);
                let _ = self.save(&r);
            }
        }
    }

    /// Keeps at most [`MAX_REPORTS_KEPT`] reports: the oldest terminated
    /// ones are removed with their screenshots.
    fn prune(&mut self) {
        let mut all = self.summaries();
        if all.len() <= MAX_REPORTS_KEPT {
            return;
        }
        all.sort_by_key(|s| s.started_at);
        let excess = all.len() - MAX_REPORTS_KEPT;
        for s in all
            .iter()
            .filter(|s| s.state == SessionState::Terminated)
            .take(excess)
        {
            if let Some(d) = self.session_dir(s.session_id) {
                let _ = std::fs::remove_dir_all(d);
            }
        }
    }

    fn summaries(&self) -> Vec<SessionReportSummary> {
        let Some(dir) = &self.dir else {
            return Vec::new();
        };
        let Ok(rd) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        rd.flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .filter_map(|e| std::fs::read(e.path().join(SUMMARY_FILE)).ok())
            .filter_map(|b| serde_json::from_slice::<SessionReportSummary>(&b).ok())
            .collect()
    }

    fn load(&self, id: SessionId) -> Result<SessionReport, PublicError> {
        if let Some(l) = self.live.get(&id) {
            return Ok(l.report.clone());
        }
        let d = self.session_dir(id).ok_or_else(not_found)?;
        let bytes = std::fs::read(d.join(REPORT_FILE)).map_err(|_| not_found())?;
        serde_json::from_slice(&bytes)
            .map_err(|_| PublicError::infrastructure("stored session report not readable"))
    }

    fn save_full(&self, r: &SessionReport) -> Result<(), PublicError> {
        let Some(d) = self.session_dir(r.session_id) else {
            return Ok(());
        };
        std::fs::create_dir_all(&d).map_err(|e| io_error("directory", &e))?;
        let (bytes, _) = canonical(r);
        let report = d.join(REPORT_FILE);
        self.write_file(&report, &bytes).map_err(|e| {
            let _ = std::fs::remove_file(report.with_extension("tmp"));
            io_error("write", &e)
        })?;
        let summary = serde_json::to_vec(&r.summary()).unwrap_or_default();
        let path = d.join(SUMMARY_FILE);
        self.write_file(&path, &summary).map_err(|e| {
            let _ = std::fs::remove_file(path.with_extension("tmp"));
            io_error("write", &e)
        })
    }

    /// Writes a report. F6, full volume: if the write fails the storage
    /// reserve is released and the write repeated; if it still fails, a
    /// minimal report (header, termination, outcome and verdicts, without
    /// the event list, counted as dropped) is written, so the termination
    /// and the outcome stay traceable (ROADMAP 8). The previous file is
    /// never left half-written (atomic replace).
    fn save(&self, r: &SessionReport) -> Result<(), PublicError> {
        let first = match self.save_full(r) {
            Ok(()) => {
                self.ensure_reserve();
                return Ok(());
            }
            Err(e) => e,
        };
        if self.release_reserve() {
            crate::diag!(
                "lightvm-service: reports: volume full, storage reserve released for the reports"
            );
            if self.save_full(r).is_ok() {
                return Ok(());
            }
        }
        let mut minimal = r.clone();
        minimal.events_dropped = minimal
            .events_dropped
            .saturating_add(u32::try_from(minimal.events.len()).unwrap_or(u32::MAX));
        minimal.events.clear();
        self.save_full(&minimal).map_err(|_| first)?;
        crate::diag!(
            "lightvm-service: reports: volume full, minimal report written (events dropped)"
        );
        Ok(())
    }

    /// Opens the report of a new session.
    pub(crate) fn open(&mut self, o: OpenReport) {
        // The storage reserve is created with the first report (catalogs
        // without agent sessions do not pay for it).
        self.ensure_reserve();
        // A new report may push an old one out of the retention limit.
        self.prune();
        let now = UtcTimestamp::now();
        let mut report = SessionReport {
            report_format: REPORT_FORMAT.to_owned(),
            session_id: o.session_id,
            vm_id: o.vm_id,
            vm_name: plain(&o.vm_name, 128),
            client_name: plain(&o.client_name, 64),
            objective: plain(&o.objective, 512),
            started_at: o.started_at,
            ended_at: None,
            state: SessionState::Active,
            termination: None,
            outcome: TestOutcome::Inconclusive,
            outcome_source: OutcomeSource::Pending,
            run_ids: o.run_id.into_iter().collect(),
            baseline_id: o.baseline.as_ref().map(|b| b.baseline_id),
            baseline: o.baseline,
            guest_os: o.guest_os,
            vm_manifest_sha256: o.vm_manifest_sha256,
            vm_config_revision: o.vm_config_revision,
            policy_revision: o.policy_revision,
            capabilities: o.capabilities,
            network_endpoints: o.network_endpoints,
            tools: o.tools,
            quotas: o.quotas,
            expires_at: o.expires_at,
            versions: o.versions,
            agent_declaration: None,
            verdicts: Vec::new(),
            errors: BTreeMap::new(),
            screenshots_retained: 0,
            screenshots_dropped: 0,
            events_total: 0,
            events_dropped: 0,
            events: Vec::new(),
            report_sha256: None,
        };
        Self::push(
            &mut report,
            now,
            ReportEventBody::Session {
                state: SessionState::Active,
                termination: None,
            },
        );
        if let Err(e) = self.save(&report) {
            crate::diag!(
                "lightvm-service: reports: report not written: {}",
                e.message
            );
        }
        self.live.insert(
            o.session_id,
            Live {
                report,
                dirty: false,
                shot_bytes: 0,
            },
        );
    }

    fn push(report: &mut SessionReport, at: UtcTimestamp, body: ReportEventBody) -> u64 {
        if report.events_total >= MAX_REPORT_EVENTS {
            report.events_dropped = report.events_dropped.saturating_add(1);
            return 0;
        }
        report.events_total += 1;
        let seq = u64::from(report.events_total);
        report.events.push(ReportEvent { seq, at, body });
        seq
    }

    fn live_mut(&mut self, id: SessionId) -> Option<&mut Live> {
        self.live
            .get_mut(&id)
            .filter(|l| l.report.state != SessionState::Terminated)
    }

    /// `true` if the session has a live report.
    pub(crate) fn is_live(&self, id: SessionId) -> bool {
        self.live
            .get(&id)
            .is_some_and(|l| l.report.state != SessionState::Terminated)
    }

    /// An agent call and its outcome.
    pub(crate) fn call(&mut self, c: &CallRecord) {
        let Some(l) = self.live_mut(c.session_id) else {
            return;
        };
        let r = &mut l.report;
        if let Some(run) = c.run_id
            && !r.run_ids.contains(&run)
        {
            r.run_ids.push(run);
        }
        if let Some(code) = c.error {
            *r.errors.entry(code.as_str().to_owned()).or_default() += 1;
        }
        Self::push(
            r,
            UtcTimestamp::now(),
            ReportEventBody::Call {
                call: c.call.to_owned(),
                vm_id: c.vm_id,
                run_id: c.run_id,
                operation_id: c.operation_id,
                ok: c.ok,
                error_code: c.error,
            },
        );
        l.dirty = true;
    }

    /// A screenshot of the agent: hash always, file within the limits.
    pub(crate) fn screenshot(&mut self, id: SessionId, meta: &Screenshot, png: &[u8]) {
        let dir = self.session_dir(id);
        let Some(l) = self
            .live
            .get(&id)
            .filter(|l| l.report.state != SessionState::Terminated)
        else {
            return;
        };
        let shot = l.report.screenshots_retained + l.report.screenshots_dropped;
        let bytes = png.len() as u64;
        let mut retained = l.report.screenshots_retained < MAX_REPORT_SCREENSHOTS
            && l.shot_bytes.saturating_add(bytes) <= MAX_REPORT_SCREENSHOT_BYTES
            && sha256(png) == meta.sha256;
        if retained && let Some(d) = dir {
            let shots = d.join(SHOTS_DIR);
            let path = shots.join(format!("{shot:04}.png"));
            let ok = std::fs::create_dir_all(&shots).and_then(|()| self.write_file(&path, png));
            if let Err(e) = ok {
                // Storage unavailable (volume full...): the hash stays in the
                // report, no partial file is left; screenshots are not
                // promised when the storage is unavailable (ROADMAP 8) and
                // the reserve is kept for the report itself.
                let _ = std::fs::remove_file(path.with_extension("tmp"));
                crate::diag!("lightvm-service: reports: screenshot not stored: {e}");
                retained = false;
            }
        }
        let Some(l) = self.live_mut(id) else {
            return;
        };
        let r = &mut l.report;
        if retained {
            r.screenshots_retained += 1;
            l.shot_bytes += bytes;
        } else {
            r.screenshots_dropped += 1;
        }
        if !r.run_ids.contains(&meta.run_id) {
            r.run_ids.push(meta.run_id);
        }
        // Recording instant (events stay in time order; the capture
        // instant precedes the call outcome recorded just before).
        Self::push(
            r,
            UtcTimestamp::now(),
            ReportEventBody::Screenshot {
                shot,
                run_id: meta.run_id,
                width: meta.geometry.width,
                height: meta.geometry.height,
                sha256: meta.sha256,
                bytes,
                retained,
            },
        );
        l.dirty = true;
    }

    /// A state change of a VM: recorded in the live reports of that VM.
    pub(crate) fn vm_state(&mut self, vm_id: VmId, state: VmState, run_id: Option<RunId>) {
        let now = UtcTimestamp::now();
        for l in self.live.values_mut() {
            if l.report.vm_id != vm_id || l.report.state == SessionState::Terminated {
                continue;
            }
            if let Some(run) = run_id
                && !l.report.run_ids.contains(&run)
                && state.has_process()
            {
                l.report.run_ids.push(run);
            }
            Self::push(
                &mut l.report,
                now,
                ReportEventBody::VmState { state, run_id },
            );
            l.dirty = true;
        }
    }

    /// A network event of a VM (ADR-0033): recorded in the live reports of
    /// that VM.
    pub(crate) fn network(
        &mut self,
        vm_id: VmId,
        action: lightvm_proto::AuditAction,
        detail: &lightvm_proto::NetworkAudit,
    ) {
        let now = UtcTimestamp::now();
        let action = name_of(&action);
        for l in self.live.values_mut() {
            if l.report.vm_id != vm_id || l.report.state == SessionState::Terminated {
                continue;
            }
            Self::push(
                &mut l.report,
                now,
                ReportEventBody::Network {
                    action: action.clone(),
                    detail: detail.clone(),
                },
            );
            l.dirty = true;
        }
    }

    pub(crate) fn tools(&mut self, record: &crate::engine::ToolsRecord) {
        for l in self.live.values_mut() {
            if l.report.vm_id == record.vm_id && l.report.state != SessionState::Terminated {
                Self::push(
                    &mut l.report,
                    UtcTimestamp::now(),
                    ReportEventBody::Tools {
                        action: name_of(&record.action),
                        outcome: record.outcome,
                        detail: record.detail.clone(),
                    },
                );
                l.dirty = true;
            }
        }
    }

    fn finish(r: &mut SessionReport, cause: TerminationCause, at: UtcTimestamp) {
        r.state = SessionState::Terminated;
        r.termination = Some(cause);
        r.ended_at = Some(at);
        Self::push(
            r,
            at,
            ReportEventBody::Session {
                state: SessionState::Terminated,
                termination: Some(cause),
            },
        );
        if r.outcome_source == OutcomeSource::Pending {
            let outcome = termination_outcome(cause);
            r.outcome = outcome;
            r.outcome_source = OutcomeSource::Termination;
            r.verdicts.push(Verdict {
                outcome,
                source: OutcomeSource::Termination,
                verifier: None,
                decided_by: "service".into(),
                rationale: format!(
                    "default for the termination cause `{}`; no verifier or user judgement yet",
                    cause_name(cause)
                ),
                at,
            });
            Self::push(
                r,
                at,
                ReportEventBody::Verdict {
                    outcome,
                    source: OutcomeSource::Termination,
                },
            );
        }
    }

    /// Lifecycle change of a session. Returns `true` if a report changed in
    /// a way the UI should re-read (termination).
    pub(crate) fn session_changed(
        &mut self,
        id: SessionId,
        state: SessionState,
        termination: Option<TerminationCause>,
    ) -> bool {
        let Some(l) = self.live_mut(id) else {
            return false;
        };
        let now = UtcTimestamp::now();
        if state == SessionState::Terminated {
            let cause = termination.unwrap_or(TerminationCause::Completed);
            Self::finish(&mut l.report, cause, now);
            l.dirty = true;
            self.flush();
            // Written: the terminated report is read from disk from now on.
            // Not written (storage unavailable even after the reserve): it
            // stays in memory, still dirty, and the next flush retries.
            let written = self.live.get(&id).is_some_and(|l| !l.dirty);
            if self.dir.is_some() && written {
                self.live.remove(&id);
            }
            return true;
        }
        if l.report.state != state {
            l.report.state = state;
            Self::push(
                &mut l.report,
                now,
                ReportEventBody::Session {
                    state,
                    termination: None,
                },
            );
            l.dirty = true;
        }
        false
    }

    /// Declaration of the agent: a claim, never the outcome.
    pub(crate) fn declare(&mut self, id: SessionId, claimed: TestOutcome, summary: &str) -> u64 {
        let Some(l) = self.live_mut(id) else {
            return 0;
        };
        let now = UtcTimestamp::now();
        l.report.agent_declaration = Some(AgentDeclaration {
            claimed_outcome: claimed,
            summary: plain(summary, MAX_DECLARATION_CHARS),
            at: now,
        });
        l.dirty = true;
        Self::push(
            &mut l.report,
            now,
            ReportEventBody::Declaration {
                claimed_outcome: claimed,
            },
        )
    }

    /// Note of the adapter (bounded; never typed text by contract).
    pub(crate) fn tool_note(
        &mut self,
        id: SessionId,
        tool: &str,
        ok: bool,
        error_code: Option<&str>,
        detail: serde_json::Value,
    ) -> u64 {
        let Some(l) = self.live_mut(id) else {
            return 0;
        };
        let detail = match serde_json::to_vec(&detail) {
            Ok(b) if b.len() <= MAX_TOOL_NOTE_DETAIL_BYTES => detail,
            _ => serde_json::json!({ "omitted": "detail over 512 bytes" }),
        };
        let tool = plain(tool, MAX_TOOL_NAME_CHARS);
        let error_code = error_code.map(|c| plain(c, 64));
        if let Some(c) = &error_code {
            *l.report.errors.entry(c.clone()).or_default() += 1;
        }
        // The adapter declares the MCP revision it negotiated.
        if let Some(v) = detail.get("mcp_protocol").and_then(|v| v.as_str()) {
            l.report.versions.mcp_protocol = Some(plain(v, 32));
        }
        l.dirty = true;
        Self::push(
            &mut l.report,
            UtcTimestamp::now(),
            ReportEventBody::Tool {
                tool,
                ok,
                error_code,
                detail,
            },
        )
    }

    /// Summary of a live session's report (agent call `report`).
    pub(crate) fn summary(&self, id: SessionId) -> Option<SessionReportSummary> {
        self.live.get(&id).map(|l| l.report.summary())
    }

    /// LightVM entries of a session for `vm.logs`.
    pub(crate) fn log_entries(
        &self,
        id: SessionId,
        after: Option<u64>,
        limit: Option<u32>,
    ) -> (Vec<LogEntry>, u64) {
        let limit = limit.unwrap_or(50).clamp(1, MAX_LOG_ENTRIES) as usize;
        let after = after.unwrap_or(0);
        let Some(l) = self.live.get(&id) else {
            return (Vec::new(), after);
        };
        let entries: Vec<LogEntry> = l
            .report
            .events
            .iter()
            .filter(|e| e.seq > after)
            .take(limit)
            .map(|e| LogEntry {
                seq: e.seq,
                at: e.at,
                source: LogSource::Lightvm,
                text: event_text(&e.body),
            })
            .collect();
        let last = entries.last().map_or(after, |e| e.seq);
        (entries, last)
    }

    /// Writes the reports changed since the last call.
    pub(crate) fn flush(&mut self) {
        let dirty: Vec<SessionId> = self
            .live
            .iter()
            .filter(|(_, l)| l.dirty)
            .map(|(id, _)| *id)
            .collect();
        let mut written = Vec::new();
        for id in dirty {
            match self.live.get(&id).map(|l| self.save(&l.report)) {
                Some(Ok(())) => written.push(id),
                Some(Err(e)) => crate::diag!(
                    "lightvm-service: reports: report not written (retried at the next flush): {}",
                    e.message
                ),
                None => {}
            }
        }
        // A report that could not be written stays dirty: retried later.
        for id in written {
            if let Some(l) = self.live.get_mut(&id) {
                l.dirty = false;
            }
        }
        // Terminated reports kept in memory because they were not written:
        // once written they are read from disk like the others.
        if self.dir.is_some() {
            self.live
                .retain(|_, l| l.dirty || l.report.state != SessionState::Terminated);
        }
    }

    // ----- UI ------------------------------------------------------------

    /// Reports, newest first.
    pub(crate) fn list(&self) -> Vec<SessionReportSummary> {
        let mut by_id: BTreeMap<SessionId, SessionReportSummary> = self
            .summaries()
            .into_iter()
            .map(|s| (s.session_id, s))
            .collect();
        for (id, l) in &self.live {
            by_id.insert(*id, l.report.summary());
        }
        let mut v: Vec<_> = by_id.into_values().collect();
        v.sort_by_key(|a| std::cmp::Reverse(a.started_at));
        v.truncate(MAX_REPORTS_KEPT);
        v
    }

    /// Header of a report (events empty).
    pub(crate) fn header(&self, id: SessionId) -> Result<SessionReport, PublicError> {
        let mut r = self.load(id)?;
        let (_, digest) = canonical(&r);
        r.report_sha256 = Some(digest);
        r.events.clear();
        Ok(r)
    }

    /// Full report (canonical, with events).
    pub(crate) fn full(&self, id: SessionId) -> Result<SessionReport, PublicError> {
        let mut r = self.load(id)?;
        let (_, digest) = canonical(&r);
        r.report_sha256 = Some(digest);
        Ok(r)
    }

    /// A page of events.
    pub(crate) fn events(
        &self,
        id: SessionId,
        offset: u32,
        limit: u32,
    ) -> Result<(u32, Vec<ReportEvent>), PublicError> {
        if !(1..=MAX_REPORT_EVENTS_PAGE).contains(&limit) {
            return Err(PublicError::invalid_argument(format!(
                "limit outside 1..={MAX_REPORT_EVENTS_PAGE}"
            )));
        }
        let r = self.load(id)?;
        let events = r
            .events
            .into_iter()
            .skip(offset as usize)
            .take(limit as usize)
            .collect();
        Ok((r.events_total, events))
    }

    /// Stored PNG of screenshot `shot`.
    pub(crate) fn screenshot_png(
        &self,
        id: SessionId,
        shot: u32,
    ) -> Result<(Vec<u8>, Sha256Digest), PublicError> {
        let r = self.load(id)?;
        let sha = r
            .events
            .iter()
            .find_map(|e| match &e.body {
                ReportEventBody::Screenshot {
                    shot: s,
                    sha256,
                    retained: true,
                    ..
                } if *s == shot => Some(*sha256),
                _ => None,
            })
            .ok_or_else(|| PublicError::not_found("screenshot not stored"))?;
        let d = self.session_dir(id).ok_or_else(not_found)?;
        let png = std::fs::read(d.join(SHOTS_DIR).join(format!("{shot:04}.png")))
            .map_err(|_| PublicError::not_found("screenshot not stored"))?;
        if sha256(&png) != sha {
            return Err(PublicError::state_conflict("stored screenshot altered")
                .with_detail("reason", "integrity"));
        }
        Ok((png, sha))
    }

    /// Records a verdict (user or verifier) on a report, live or stored.
    pub(crate) fn set_verdict(
        &mut self,
        id: SessionId,
        verdict: Verdict,
    ) -> Result<SessionReport, PublicError> {
        if verdict.rationale.trim().is_empty() {
            return Err(PublicError::invalid_argument("rationale required"));
        }
        let mut r = self.load(id)?;
        let v = Verdict {
            rationale: plain(&verdict.rationale, MAX_RATIONALE_CHARS),
            ..verdict
        };
        r.outcome = v.outcome;
        r.outcome_source = v.source;
        Self::push(
            &mut r,
            v.at,
            ReportEventBody::Verdict {
                outcome: v.outcome,
                source: v.source,
            },
        );
        r.verdicts.push(v);
        self.store_back(r)
    }

    fn store_back(&mut self, r: SessionReport) -> Result<SessionReport, PublicError> {
        let id = r.session_id;
        match self.live.get_mut(&id) {
            Some(l) => {
                l.report = r;
                l.dirty = true;
            }
            None => self.save(&r)?,
        }
        self.flush();
        self.header(id)
    }

    /// Runs a verifier on the session's evidence.
    pub(crate) fn verify(
        &mut self,
        id: SessionId,
        verifier: SessionVerifier,
    ) -> Result<SessionReport, PublicError> {
        let r = self.load(id)?;
        let SessionVerifier::FixtureMarker { expected } = verifier;
        let last = r.events.iter().rev().find_map(|e| match &e.body {
            ReportEventBody::Screenshot { shot, retained, .. } => Some((*shot, *retained)),
            _ => None,
        });
        let (outcome, rationale) = match last {
            None => (
                TestOutcome::Inconclusive,
                "the session has no screenshot to verify".to_owned(),
            ),
            Some((shot, false)) => (
                TestOutcome::Inconclusive,
                format!("the last screenshot ({shot}) was not stored (retention limits)"),
            ),
            Some((shot, true)) => match self.screenshot_png(id, shot) {
                Err(e) => (
                    TestOutcome::InfrastructureError,
                    format!("last screenshot ({shot}) not readable: {}", e.message),
                ),
                Ok((png, sha)) => {
                    let (o, why) = fixture_check(&png, expected);
                    (
                        o,
                        format!(
                            "last screenshot {shot} (sha256 {}): {why}",
                            &sha.to_hex()[..16]
                        ),
                    )
                }
            },
        };
        self.set_verdict(
            id,
            Verdict {
                outcome,
                source: OutcomeSource::Verifier,
                verifier: Some(format!(
                    "{FIXTURE_VERIFIER}, expected marker {}",
                    match expected {
                        FixtureMarker::Present => "present",
                        FixtureMarker::Absent => "absent",
                    }
                )),
                decided_by: "verifier".into(),
                rationale,
                at: UtcTimestamp::now(),
            },
        )
    }

    /// Deletes the report of a terminated session with its screenshots.
    pub(crate) fn delete(&mut self, id: SessionId) -> Result<(), PublicError> {
        if self.is_live(id) {
            return Err(PublicError::state_conflict("the session is still live"));
        }
        self.load(id)?;
        self.live.remove(&id);
        if let Some(d) = self.session_dir(id) {
            std::fs::remove_dir_all(d).map_err(|e| io_error("deletion", &e))?;
        }
        Ok(())
    }

    /// Export to a file chosen by the user: `(bytes, sha256, screenshots)`.
    pub(crate) fn export(
        &self,
        id: SessionId,
        format: ReportFormat,
        destination: &str,
        overwrite: bool,
        include_screenshots: bool,
    ) -> Result<(u64, Sha256Digest, u32), PublicError> {
        let dest = validate_destination(destination, overwrite)?;
        let r = self.full(id)?;
        let bytes = match format {
            ReportFormat::Json => canonical(&r).0,
            ReportFormat::Markdown => render_markdown(&r).into_bytes(),
        };
        let mut shots = 0;
        if include_screenshots {
            let stem = dest
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "report".into());
            let sdir = dest.with_file_name(format!("{stem}-screenshots"));
            match std::fs::symlink_metadata(&sdir) {
                Ok(m) if !m.is_dir() || !overwrite => {
                    return Err(PublicError::state_conflict(
                        "the screenshots folder already exists",
                    ));
                }
                Ok(_) => {}
                Err(_) => std::fs::create_dir(&sdir).map_err(|e| io_error("export", &e))?,
            }
            for e in &r.events {
                if let ReportEventBody::Screenshot {
                    shot,
                    sha256: sha,
                    retained: true,
                    ..
                } = &e.body
                    && let Ok((png, _)) = self.screenshot_png(id, *shot)
                {
                    let name = format!("{shot:03}-{}.png", &sha.to_hex()[..12]);
                    write_atomic(&sdir.join(name), &png).map_err(|e| io_error("export", &e))?;
                    shots += 1;
                }
            }
        }
        write_atomic(&dest, &bytes).map_err(|e| io_error("export", &e))?;
        Ok((bytes.len() as u64, sha256(&bytes), shots))
    }
}

/// Outcome of the ADR-0019 pixel verifier on a PNG.
pub(crate) fn fixture_check(png: &[u8], expected: FixtureMarker) -> (TestOutcome, String) {
    use lightvm_fixture::verify::{Check, DEFAULT_CALIBRATION, Frame, Marker};
    let Some((w, h, rgb)) = decode_rgb(png) else {
        return (
            TestOutcome::InfrastructureError,
            "stored PNG not decodable".into(),
        );
    };
    let frame = Frame {
        width: w,
        height: h,
        rgb: &rgb,
    };
    let want = match expected {
        FixtureMarker::Present => Marker::Present,
        FixtureMarker::Absent => Marker::Absent,
    };
    match frame.check(DEFAULT_CALIBRATION) {
        Check::Ok(m) if m == want => (
            TestOutcome::Passed,
            format!("V3 band B1..B4 found, B5 marker {m:?} as expected ({w}x{h})"),
        ),
        Check::Ok(m) => (
            TestOutcome::Failed,
            format!("V3 band found but B5 marker {m:?}, expected {want:?}"),
        ),
        Check::NotFound(why) => (
            TestOutcome::Failed,
            format!("fixture output not found on the screen: {why}"),
        ),
    }
}

/// PNG to 24-bit RGB (8-bit RGB or RGBA).
fn decode_rgb(png_bytes: &[u8]) -> Option<(usize, usize, Vec<u8>)> {
    let decoder = png::Decoder::new(std::io::Cursor::new(png_bytes));
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    if info.bit_depth != png::BitDepth::Eight {
        return None;
    }
    buf.truncate(info.buffer_size());
    let rgb = match info.color_type {
        png::ColorType::Rgb => buf,
        png::ColorType::Rgba => buf
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2]])
            .collect(),
        _ => return None,
    };
    Some((info.width as usize, info.height as usize, rgb))
}

/// Validates an export destination chosen by the user: absolute local
/// path of a new regular file (or an existing one with `overwrite`) in an
/// existing directory; no UNC/device paths, no `..`, no links.
pub(crate) fn validate_destination(s: &str, overwrite: bool) -> Result<PathBuf, PublicError> {
    let bad = |why: &str| {
        PublicError::invalid_argument(format!("destination not valid: {why}"))
            .with_detail("field", "destination")
    };
    if s.is_empty() || s.len() > 1024 || s.chars().any(char::is_control) {
        return Err(bad("empty, too long or with control characters"));
    }
    if s.starts_with(r"\\") || s.starts_with("//") {
        return Err(bad("network and device paths are not accepted"));
    }
    let p = Path::new(s);
    if !p.is_absolute() {
        return Err(bad("an absolute path is required"));
    }
    if p.components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(bad("`..` is not accepted"));
    }
    let name = p.file_name().ok_or_else(|| bad("no file name"))?;
    let stem = Path::new(name)
        .file_stem()
        .map(|s| s.to_string_lossy().to_ascii_uppercase())
        .unwrap_or_default();
    const RESERVED: [&str; 22] = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    if RESERVED.contains(&stem.as_str()) {
        return Err(bad("reserved device name"));
    }
    let parent = p.parent().ok_or_else(|| bad("no parent directory"))?;
    if !std::fs::metadata(parent).is_ok_and(|m| m.is_dir()) {
        return Err(bad("the folder does not exist"));
    }
    match std::fs::symlink_metadata(p) {
        Ok(m) if !m.is_file() => Err(bad("not a regular file")),
        Ok(_) if !overwrite => Err(PublicError::state_conflict(
            "the destination exists (confirm to overwrite)",
        )),
        _ => Ok(p.to_path_buf()),
    }
}

fn cause_name(c: TerminationCause) -> String {
    serde_json::to_value(c)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn name_of<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_value(v)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "?".into())
}

/// One-line text of a network event (ADR-0033): addresses and counts.
fn network_text(action: &str, d: &lightvm_proto::NetworkAudit) -> String {
    let mut s = format!("network {}", plain(action, 64));
    for (k, v) in [
        ("mode", d.mode.as_deref()),
        ("endpoint", d.endpoint.as_deref()),
        ("destination", d.destination.as_deref()),
        ("target", d.target.as_deref()),
        ("reason", d.reason.as_deref()),
    ] {
        if let Some(v) = v {
            s.push_str(&format!(" {k}={}", plain(v, 256)));
        }
    }
    if let (Some(up), Some(down)) = (d.bytes_from_guest, d.bytes_to_guest) {
        s.push_str(&format!(" bytes_from_guest={up} bytes_to_guest={down}"));
    }
    s
}

/// One-line plain text of an event (`vm.logs`, Markdown).
pub(crate) fn event_text(b: &ReportEventBody) -> String {
    match b {
        ReportEventBody::Session { state, termination } => match termination {
            Some(t) => format!("session {} ({})", name_of(state), name_of(t)),
            None => format!("session {}", name_of(state)),
        },
        ReportEventBody::Call {
            call,
            ok,
            error_code,
            operation_id,
            run_id,
            ..
        } => {
            let mut s = format!("call {call}: {}", if *ok { "ok" } else { "refused/failed" });
            if let Some(c) = error_code {
                s.push_str(&format!(" ({})", c.as_str()));
            }
            if let Some(o) = operation_id {
                s.push_str(&format!(" op {o}"));
            }
            if let Some(r) = run_id {
                s.push_str(&format!(" run {r}"));
            }
            s
        }
        ReportEventBody::Tool {
            tool,
            ok,
            error_code,
            detail,
        } => {
            let mut s = format!("tool {tool}: {}", if *ok { "ok" } else { "error" });
            if let Some(c) = error_code {
                s.push_str(&format!(" ({c})"));
            }
            if !detail.is_null() {
                s.push(' ');
                s.push_str(&plain(&detail.to_string(), 512));
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
            "screenshot {shot}: {width}x{height}, {bytes} bytes, sha256 {}{}",
            sha256.to_hex(),
            if *retained { "" } else { " (not stored)" }
        ),
        ReportEventBody::VmState { state, run_id } => match run_id {
            Some(r) => format!("vm {} (run {r})", name_of(state)),
            None => format!("vm {}", name_of(state)),
        },
        ReportEventBody::Verdict { outcome, source } => {
            format!("verdict {} ({})", name_of(outcome), name_of(source))
        }
        ReportEventBody::Declaration { claimed_outcome } => {
            format!("agent declaration: claims {}", name_of(claimed_outcome))
        }
        ReportEventBody::Network { action, detail } => network_text(action, detail),
        ReportEventBody::Tools {
            action,
            outcome,
            detail,
        } => format!(
            "Tools {} {:?}: {} {} bytes",
            plain(action, 64),
            outcome,
            plain(detail.feature.as_str(), 64),
            detail.bytes.unwrap_or(0)
        ),
        ReportEventBody::Note { text } => format!("note: {}", plain(text, 512)),
    }
}

/// Inline code span of untrusted text: no markup, no link (control
/// characters and backticks removed).
fn md_code(s: &str) -> String {
    let t: String = s
        .chars()
        .filter(|c| !c.is_control())
        .map(|c| if c == '`' { '\'' } else { c })
        .collect();
    if t.is_empty() {
        "(empty)".into()
    } else {
        format!("`{t}`")
    }
}

/// Fenced block of untrusted multi-line text.
fn md_block(s: &str) -> String {
    let t: String = s
        .chars()
        .filter(|c| *c == '\n' || !c.is_control())
        .map(|c| if c == '`' { '\'' } else { c })
        .collect();
    format!("```text\n{t}\n```\n")
}

/// Markdown view of a report: every text from agents, guest or client is
/// in a code span or block (shown as text, never markup or links).
pub(crate) fn render_markdown(r: &SessionReport) -> String {
    let mut m = String::new();
    let o = |x: &Option<String>| x.clone().unwrap_or_else(|| "-".into());
    m.push_str("# LightVM session report\n\n");
    m.push_str(&format!(
        "Outcome: **{}** (source: {})\n\n",
        name_of(&r.outcome),
        name_of(&r.outcome_source)
    ));
    m.push_str("| Field | Value |\n| --- | --- |\n");
    let rows: Vec<(&str, String)> = vec![
        ("Report format", md_code(&r.report_format)),
        ("Session", md_code(&r.session_id.to_string())),
        (
            "VM",
            format!("{} {}", md_code(&r.vm_name), md_code(&r.vm_id.to_string())),
        ),
        ("Client", md_code(&r.client_name)),
        ("Guest OS", md_code(&name_of(&r.guest_os))),
        ("Started (UTC)", md_code(&r.started_at.to_string())),
        (
            "Ended (UTC)",
            md_code(&r.ended_at.map_or_else(|| "-".into(), |t| t.to_string())),
        ),
        ("State", md_code(&name_of(&r.state))),
        (
            "Termination",
            md_code(&r.termination.map_or_else(|| "-".into(), cause_name)),
        ),
        (
            "Runs",
            md_code(
                &r.run_ids
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
        ),
        (
            "Baseline",
            md_code(&r.baseline.as_ref().map_or_else(
                || "standalone disk (no baseline)".into(),
                |b| {
                    format!(
                        "{} {} disk sha256 {}",
                        b.baseline_id,
                        b.label,
                        b.disk_sha256.to_hex()
                    )
                },
            )),
        ),
        (
            "VM manifest sha256",
            md_code(&r.vm_manifest_sha256.to_hex()),
        ),
        (
            "VM config revision",
            md_code(&r.vm_config_revision.to_string()),
        ),
        ("Policy revision", md_code(&r.policy_revision.0.to_string())),
        (
            "Capabilities",
            md_code(
                &r.capabilities
                    .iter()
                    .map(|c| c.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
        ),
        ("Grant expiry (UTC)", md_code(&r.expires_at.to_string())),
        (
            "Versions",
            md_code(&format!(
                "service {}, IPC {}, MCP {}, backend {}, accelerator {}",
                r.versions.service,
                r.versions.ipc_protocol,
                o(&r.versions.mcp_protocol),
                o(&r.versions.backend),
                o(&r.versions.accelerator)
            )),
        ),
        (
            "Screenshots",
            md_code(&format!(
                "{} stored, {} not stored",
                r.screenshots_retained, r.screenshots_dropped
            )),
        ),
        (
            "Events",
            md_code(&format!(
                "{} recorded, {} dropped",
                r.events_total, r.events_dropped
            )),
        ),
        (
            "Report sha256",
            md_code(&r.report_sha256.map_or_else(|| "-".into(), |d| d.to_hex())),
        ),
    ];
    for (k, v) in rows {
        m.push_str(&format!("| {k} | {v} |\n"));
    }
    m.push_str("\n## Objective\n\n");
    m.push_str(&md_block(&r.objective));
    m.push_str("\n## Verdicts\n\n");
    if r.verdicts.is_empty() {
        m.push_str("None yet: the outcome is pending.\n");
    }
    for v in &r.verdicts {
        m.push_str(&format!(
            "- {} — **{}** by {} ({}){}\n",
            md_code(&v.at.to_string()),
            name_of(&v.outcome),
            md_code(&v.decided_by),
            name_of(&v.source),
            v.verifier
                .as_ref()
                .map_or_else(String::new, |x| format!(", verifier {}", md_code(x)))
        ));
        m.push_str(&format!("\n{}\n", md_block(&v.rationale)));
    }
    m.push_str("\n## Agent declaration (claim, not an outcome)\n\n");
    match &r.agent_declaration {
        None => m.push_str("None.\n"),
        Some(d) => {
            m.push_str(&format!(
                "Claimed outcome: {} at {}\n\n",
                md_code(&name_of(&d.claimed_outcome)),
                md_code(&d.at.to_string())
            ));
            m.push_str(&md_block(&d.summary));
        }
    }
    m.push_str("\n## Errors by code\n\n");
    if r.errors.is_empty() {
        m.push_str("None.\n");
    }
    for (k, v) in &r.errors {
        m.push_str(&format!("- {}: {v}\n", md_code(k)));
    }
    m.push_str("\n## Events\n\n| # | UTC | Event |\n| --- | --- | --- |\n");
    for e in &r.events {
        m.push_str(&format!(
            "| {} | {} | {} |\n",
            e.seq,
            md_code(&e.at.to_string()),
            md_code(&event_text(&e.body).replace('|', "/"))
        ));
    }
    m
}

/// Lines of QEMU diagnostics for `vm.logs`: control characters and host
/// paths removed, bounded.
pub(crate) fn sanitize_qemu_lines(raw: &str) -> Vec<String> {
    let lines: Vec<String> = raw
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| {
            let l = match l.find(".exe: ") {
                Some(i) => &l[i + 6..],
                None => l,
            };
            plain(&strip_paths(l), 300)
        })
        .collect();
    let start = lines.len().saturating_sub(MAX_QEMU_LOG_LINES);
    lines[start..].to_vec()
}

/// Replaces host paths (drive, UNC, device and Unix absolute paths) with
/// `<path>`.
pub(crate) fn strip_paths(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for (i, tok) in s.split(' ').enumerate() {
        if i > 0 {
            out.push(' ');
        }
        let t = tok.trim_matches(|c| c == '\'' || c == '"' || c == ',' || c == '(' || c == ')');
        let b = t.as_bytes();
        let drive = b.len() >= 3
            && b[0].is_ascii_alphabetic()
            && b[1] == b':'
            && (b[2] == b'\\' || b[2] == b'/');
        let unc = t.starts_with(r"\\") || t.starts_with("//");
        let unix = t.starts_with('/') && t[1..].contains('/');
        let any = t.contains(":\\") || t.contains("=/") || t.contains("=C:") || t.contains("path=");
        if drive || unc || unix || any {
            out.push_str("<path>");
        } else {
            out.push_str(tok);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Simulated volume for the disk-full tests: every write takes its size
    /// from `free`; a write larger than what is left fails with the OS
    /// "disk full" error (`ERROR_DISK_FULL` on Windows, `ENOSPC` elsewhere),
    /// as the real file system would, before anything is renamed.
    #[derive(Debug)]
    pub(super) struct Volume {
        free: std::sync::Mutex<u64>,
    }

    impl Volume {
        pub(super) fn new(free: u64) -> std::sync::Arc<Self> {
            std::sync::Arc::new(Self {
                free: std::sync::Mutex::new(free),
            })
        }

        pub(super) fn take(&self, n: u64) -> std::io::Result<()> {
            let mut f = self.free.lock().unwrap();
            if n > *f {
                return Err(disk_full());
            }
            *f -= n;
            Ok(())
        }

        pub(super) fn give_back(&self, n: u64) {
            *self.free.lock().unwrap() += n;
        }

        fn set(&self, n: u64) {
            *self.free.lock().unwrap() = n;
        }
    }

    fn disk_full() -> std::io::Error {
        if cfg!(windows) {
            // ERROR_DISK_FULL
            std::io::Error::from_raw_os_error(112)
        } else {
            std::io::Error::from_raw_os_error(28)
        }
    }

    fn store_on_volume(name: &str, free: u64, reserve: u64) -> (ReportStore, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "lightvm-reports-{name}-{}-{}",
            std::process::id(),
            SessionId::new_v7()
        ));
        let mut s = ReportStore {
            dir: Some(dir.clone()),
            reserve_bytes: reserve,
            volume: Some(Volume::new(free)),
            ..ReportStore::default()
        };
        std::fs::create_dir_all(&dir).unwrap();
        assert!(s.ensure_reserve(), "reserve created");
        s.reserve_retry = std::time::Duration::ZERO;
        (s, dir)
    }

    fn on_disk(dir: &Path, id: SessionId) -> SessionReport {
        let b = std::fs::read(dir.join(id.to_string()).join(REPORT_FILE)).unwrap();
        serde_json::from_slice(&b).unwrap()
    }

    /// F6, disk full (injected): the OS error is the one the file system
    /// returns for a full volume, and std maps it to `StorageFull`.
    #[test]
    fn injected_error_is_the_disk_full_error() {
        assert_eq!(disk_full().kind(), std::io::ErrorKind::StorageFull);
    }

    /// F6, disk full while a session ends: the reserve is released and the
    /// terminated report (termination cause and outcome) reaches the disk;
    /// the reserve comes back once there is space again.
    #[test]
    fn full_volume_at_termination_uses_the_reserve() {
        let (mut s, dir) = store_on_volume("reserve", 1 << 20, 64 << 10);
        let id = open_report(&mut s);
        s.flush();
        // The volume fills up (another program, a growing overlay).
        s.volume.as_ref().unwrap().set(0);
        assert!(s.session_changed(
            id,
            SessionState::Terminated,
            Some(TerminationCause::Revoked)
        ));
        assert!(!dir.join(RESERVE_FILE).exists(), "reserve released");
        let r = on_disk(&dir, id);
        assert_eq!(r.state, SessionState::Terminated);
        assert_eq!(r.termination, Some(TerminationCause::Revoked));
        assert_eq!(r.outcome, TestOutcome::Cancelled);
        assert!(!r.events.is_empty(), "full report written with the reserve");
        assert!(!s.live.contains_key(&id), "written: read from disk");
        // Space is back: the next successful write re-creates the reserve.
        s.volume.as_ref().unwrap().set(1 << 20);
        let id2 = open_report(&mut s);
        s.flush();
        assert!(s.full(id2).is_ok());
        assert!(dir.join(RESERVE_FILE).exists(), "reserve re-created");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// F6: when even the reserve is not enough for the full report, a
    /// minimal report (header, termination, outcome, verdicts; events
    /// counted as dropped) is written; the previous file is never left
    /// half-written.
    #[test]
    fn full_volume_beyond_the_reserve_writes_a_minimal_report() {
        let (mut s, dir) = store_on_volume("minimal", 4 << 20, 16 << 10);
        let id = open_report(&mut s);
        for i in 0..1000 {
            s.tool_note(
                id,
                "vm_status",
                true,
                None,
                serde_json::json!({"note": format!("note {i} with some padding text")}),
            );
        }
        s.flush();
        let before = on_disk(&dir, id);
        assert!(before.events.len() > 1000);
        s.volume.as_ref().unwrap().set(0);
        s.session_changed(
            id,
            SessionState::Terminated,
            Some(TerminationCause::GrantExpired),
        );
        let r = on_disk(&dir, id);
        assert_eq!(r.termination, Some(TerminationCause::GrantExpired));
        assert_eq!(r.outcome, TestOutcome::TimedOut);
        assert!(r.events.is_empty(), "minimal report");
        assert!(r.events_dropped >= u32::try_from(before.events.len()).unwrap());
        assert!(!r.verdicts.is_empty());
        // No temporary left next to the report.
        assert!(!dir.join(id.to_string()).join("report.tmp").exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// F6: with no space at all (reserve already used, nothing writable)
    /// the terminated report stays in memory and is written by a later
    /// flush once space is back; screenshots are dropped to their hash.
    #[test]
    fn report_not_written_is_kept_and_retried() {
        let (mut s, dir) = store_on_volume("retry", 1 << 20, 4 << 10);
        let id = open_report(&mut s);
        s.flush();
        let png = vec![7u8; 10_000];
        let meta = Screenshot {
            vm_id: VmId::new_v7(),
            run_id: RunId::new_v7(),
            geometry: lightvm_proto::DisplayGeometry::new(1280, 800).unwrap(),
            format: lightvm_proto::ImageFormat::Png,
            sha256: sha256(&png),
            byte_len: png.len() as u64,
            captured_at: UtcTimestamp::now(),
        };
        std::fs::remove_file(dir.join(RESERVE_FILE)).unwrap();
        s.volume.as_ref().unwrap().set(0);
        s.screenshot(id, &meta, &png);
        assert!(
            !dir.join(id.to_string())
                .join(SHOTS_DIR)
                .join("0000.png")
                .exists()
        );
        assert!(
            !dir.join(id.to_string())
                .join(SHOTS_DIR)
                .join("0000.tmp")
                .exists()
        );
        s.session_changed(
            id,
            SessionState::Terminated,
            Some(TerminationCause::Revoked),
        );
        // Not on disk yet (still `active` there), kept in memory.
        assert_eq!(on_disk(&dir, id).state, SessionState::Active);
        assert!(s.live.contains_key(&id));
        assert_eq!(s.full(id).unwrap().outcome, TestOutcome::Cancelled);
        // Space is back.
        s.volume.as_ref().unwrap().set(1 << 20);
        s.flush();
        let r = on_disk(&dir, id);
        assert_eq!(r.state, SessionState::Terminated);
        assert_eq!((r.screenshots_retained, r.screenshots_dropped), (0, 1));
        assert!(!s.live.contains_key(&id));
        let _ = std::fs::remove_dir_all(dir);
    }

    fn open_report(store: &mut ReportStore) -> SessionId {
        let id = SessionId::new_v7();
        let vm = VmId::new_v7();
        store.open(OpenReport {
            tools: Default::default(),
            session_id: id,
            vm_id: vm,
            vm_name: "vm".into(),
            client_name: "client\u{1b}[31m".into(),
            objective: "objective".into(),
            started_at: UtcTimestamp::now(),
            capabilities: CapabilitySet::new(),
            network_endpoints: Vec::new(),
            quotas: lightvm_proto::ipc::AGENT_DEFAULT_QUOTAS,
            expires_at: UtcTimestamp::now(),
            policy_revision: PolicyRevision(1),
            guest_os: GuestOs::UbuntuLtsX86_64,
            vm_manifest_sha256: sha256(b"x"),
            vm_config_revision: 1,
            baseline: None,
            run_id: None,
            versions: ReportVersions::default(),
        });
        id
    }

    #[test]
    fn default_outcomes_by_termination_cause() {
        use TerminationCause as T;
        assert_eq!(termination_outcome(T::Completed), TestOutcome::Inconclusive);
        assert_eq!(termination_outcome(T::Revoked), TestOutcome::Cancelled);
        assert_eq!(
            termination_outcome(T::EmergencyStop),
            TestOutcome::Cancelled
        );
        assert_eq!(termination_outcome(T::LeaseExpired), TestOutcome::TimedOut);
        assert_eq!(termination_outcome(T::GrantExpired), TestOutcome::TimedOut);
        assert_eq!(
            termination_outcome(T::VmTerminated),
            TestOutcome::InfrastructureError
        );
        // No cause maps to `passed`.
        for c in [
            T::Completed,
            T::Revoked,
            T::EmergencyStop,
            T::GrantExpired,
            T::LeaseExpired,
            T::ClientLost,
            T::UiClosed,
            T::SupervisorLost,
            T::HostResumed,
            T::VmTerminated,
            T::Restored,
        ] {
            assert_ne!(termination_outcome(c), TestOutcome::Passed);
        }
    }

    #[test]
    fn declaration_never_sets_the_outcome_and_text_is_plain() {
        let mut s = ReportStore::new(None);
        let id = open_report(&mut s);
        s.declare(id, TestOutcome::Passed, "done <script>\u{7}");
        let r = s.full(id).unwrap();
        assert_eq!(r.outcome_source, OutcomeSource::Pending);
        assert_eq!(r.outcome, TestOutcome::Inconclusive);
        assert_eq!(r.client_name, "client[31m");
        assert_eq!(r.agent_declaration.unwrap().summary, "done <script>");
        assert!(s.session_changed(
            id,
            SessionState::Terminated,
            Some(TerminationCause::Completed)
        ));
        let r = s.full(id).unwrap();
        assert_eq!(r.outcome, TestOutcome::Inconclusive);
        assert_eq!(r.outcome_source, OutcomeSource::Termination);
    }

    #[test]
    fn tool_note_detail_is_bounded() {
        let mut s = ReportStore::new(None);
        let id = open_report(&mut s);
        let big = serde_json::json!({ "x": "a".repeat(2000) });
        s.tool_note(id, "vm_keyboard_type", true, None, big);
        let r = s.full(id).unwrap();
        let ReportEventBody::Tool { detail, .. } = &r.events.last().unwrap().body else {
            panic!()
        };
        assert!(detail.get("omitted").is_some());
    }

    #[test]
    fn markdown_escapes_untrusted_text() {
        let mut s = ReportStore::new(None);
        let id = open_report(&mut s);
        s.declare(
            id,
            TestOutcome::Passed,
            "[click](https://evil.example) <img src=x onerror=alert(1)> ```",
        );
        let md = render_markdown(&s.full(id).unwrap());
        assert!(!md.contains("``````"));
        // The declaration sits inside a fenced block: not a link, not HTML.
        let i = md.find("[click]").unwrap();
        let before = &md[..i];
        assert!(before.rfind("```text").unwrap() > before.rfind("\n## ").unwrap());
    }

    #[test]
    fn paths_are_stripped_from_qemu_lines() {
        let raw = "C:\\q\\qemu-system-x86_64.exe: -drive file=C:\\Users\\me\\vm.qcow2: error\n\
                   warning: /home/me/x/y missing\n\\\\server\\share\\f bad";
        let l = sanitize_qemu_lines(raw);
        assert_eq!(l.len(), 3);
        for x in &l {
            assert!(
                !x.contains("Users") && !x.contains("/home") && !x.contains("server"),
                "{x}"
            );
        }
    }

    #[test]
    fn destination_validation() {
        let dir = crate::test_support::temp_dir("rep-dest");
        let f = dir.join("r.json");
        let s = f.to_str().unwrap();
        assert!(validate_destination(s, false).is_ok());
        std::fs::write(&f, b"x").unwrap();
        assert!(validate_destination(s, false).is_err());
        assert!(validate_destination(s, true).is_ok());
        assert!(validate_destination("relative.json", false).is_err());
        assert!(validate_destination(r"\\server\share\r.json", false).is_err());
        assert!(validate_destination(dir.to_str().unwrap(), true).is_err());
        let up = format!(
            "{}{}..{}r.json",
            dir.display(),
            std::path::MAIN_SEPARATOR,
            std::path::MAIN_SEPARATOR
        );
        assert!(validate_destination(&up, false).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }
}
