//! Agent sessions on the service side (F4): agent access switch, pending
//! session requests with their verification code, binding of an approved
//! session to the agent connection, and the **gate** of each session.
//!
//! The gate is the priority point of the pause and of the revocation
//! (ROADMAP 7.3): the UI's pause, revocation and Emergency Stop flip it
//! synchronously, before anything goes through the supervisor queue, and
//! both the connection (on receipt) and the supervisor thread (right before
//! executing) check it, in addition to the core's own two checks. A session
//! terminated by the core for any other reason (expiry, restore, Emergency
//! Stop) is reported by the supervisor ([`AgentRegistry::terminated`]).
//!
//! Every change made here (pause, termination) is also queued for the core
//! ([`crate::engine::SessionGate`]): the engine applies it before its next
//! admission and before every forwarding, so even a request already past
//! the gate and waiting inside the supervisor is refused by the core's own
//! checks once the gate has changed.
//!
//! Nothing here holds the session token: it goes from the approval to the
//! core and to the waiting agent only.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use lightvm_proto::ipc::{
    ConnectionId, MAX_CLIENT_NAME_CHARS, MAX_PURPOSE_CHARS, ResponseBody, SessionRequestId,
    SessionRequestInfo, SessionRequestOutcome,
};
use lightvm_proto::{
    CapabilitySet, PublicError, ReplyError, SessionId, TerminationCause, UtcTimestamp, VmId,
};

use crate::engine::{GateChange, SessionGate};
use tokio::sync::{oneshot, watch};

/// Pending requests kept at most (all connections).
pub const MAX_PENDING_REQUESTS: usize = 8;

/// Decision delivered to the agent waiting with `await_session`.
pub(crate) type Decision = Result<ResponseBody, ReplyError>;

/// State of a session as seen by the gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Gate {
    Active,
    Paused,
    Terminated,
}

#[derive(Debug)]
struct Pending {
    info: SessionRequestInfo,
    expires: Instant,
    /// Approval in progress (not listed, not decidable twice).
    approving: bool,
    /// Decision taken before the agent awaited it.
    decision: Option<Decision>,
    /// Agent waiting with `await_session`.
    waiter: Option<oneshot::Sender<Decision>>,
}

#[derive(Debug)]
struct Bound {
    connection_id: ConnectionId,
    gate: Gate,
}

#[derive(Debug)]
struct ConnState {
    /// Raised when the session bound to the connection is terminated: the
    /// connection closes (`closing { reason: revoked }`).
    session_end: watch::Sender<bool>,
    session: Option<SessionId>,
    request: Option<SessionRequestId>,
}

#[derive(Debug, Default)]
struct Inner {
    next_request: SessionRequestId,
    pending: BTreeMap<SessionRequestId, Pending>,
    sessions: HashMap<SessionId, Bound>,
    conns: HashMap<ConnectionId, ConnState>,
    /// Changes for the core, in order.
    changes: Vec<(SessionId, GateChange)>,
}

/// Registry shared by the server tasks and the supervisor thread.
#[derive(Debug, Default)]
pub struct AgentRegistry {
    enabled: AtomicBool,
    inner: Mutex<Inner>,
    ttl: Duration,
}

/// Request closed (expired, withdrawn, denied) to report as event and audit.
pub(crate) type Closed = Vec<(SessionRequestId, SessionRequestOutcome)>;

fn lock(m: &Mutex<Inner>) -> MutexGuard<'_, Inner> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Six random digits shown as `123-456`.
fn verification_code() -> Result<String, PublicError> {
    let mut b = [0u8; 8];
    getrandom::fill(&mut b)
        .map_err(|_| PublicError::resource_limit("random generator not available"))?;
    let n = u64::from_le_bytes(b) % 1_000_000;
    Ok(format!("{:03}-{:03}", n / 1000, n % 1000))
}

fn truncate(s: &str, n: usize) -> String {
    // Control characters are dropped: the texts are shown as plain text.
    s.chars().filter(|c| !c.is_control()).take(n).collect()
}

fn utc_after(d: Duration) -> UtcTimestamp {
    let now = UtcTimestamp::now();
    now.checked_add_millis(i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(now)
}

impl AgentRegistry {
    /// Registry with the switch off and the given request lifetime.
    pub fn new(ttl: Duration) -> Self {
        Self {
            enabled: AtomicBool::new(false),
            inner: Mutex::default(),
            ttl,
        }
    }

    /// Agent access switch.
    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::SeqCst)
    }

    pub(crate) fn set_enabled(&self, on: bool) -> bool {
        self.enabled.swap(on, Ordering::SeqCst)
    }

    /// An agent (pairing) connection is registered: the receiver is raised
    /// when its session is terminated.
    pub(crate) fn register_conn(&self, connection_id: ConnectionId) -> watch::Receiver<bool> {
        let (tx, rx) = watch::channel(false);
        lock(&self.inner).conns.insert(
            connection_id,
            ConnState {
                session_end: tx,
                session: None,
                request: None,
            },
        );
        rx
    }

    /// Purges expired requests (not awaited ones included).
    fn purge(inner: &mut Inner) -> Closed {
        let now = Instant::now();
        let gone: Vec<SessionRequestId> = inner
            .pending
            .iter()
            .filter(|(_, p)| now >= p.expires && !p.approving)
            .map(|(id, _)| *id)
            .collect();
        let mut closed = Vec::new();
        for id in gone {
            if let Some(p) = inner.pending.remove(&id) {
                if let Some(c) = inner.conns.get_mut(&p.info.connection_id)
                    && c.request == Some(id)
                {
                    c.request = None;
                }
                let decided = p.decision.is_some();
                if let Some(w) = p.waiter {
                    let _ = w.send(Err(expired()));
                }
                if !decided {
                    closed.push((id, SessionRequestOutcome::Expired));
                }
            }
        }
        closed
    }

    /// New pending request of a connection.
    pub(crate) fn open_request(
        &self,
        connection_id: ConnectionId,
        peer_pid: Option<u32>,
        client_name: &str,
        purpose: &str,
        requested_vms: Vec<VmId>,
        requested_capabilities: CapabilitySet,
        requested_endpoints: Vec<String>,
    ) -> Result<(SessionRequestInfo, Closed), PublicError> {
        if !self.enabled() {
            return Err(
                PublicError::permission_denied("agent access disabled in the app")
                    .with_detail("reason", "agent_access_disabled"),
            );
        }
        let code = verification_code()?;
        let mut inner = lock(&self.inner);
        let closed = Self::purge(&mut inner);
        let busy = inner
            .conns
            .get(&connection_id)
            .map(|c| c.session.is_some() || c.request.is_some());
        match busy {
            None => return Err(PublicError::state_conflict("connection not registered")),
            Some(true) => {
                return Err(PublicError::state_conflict(
                    "a session request or a session already exists on this connection",
                ));
            }
            Some(false) => {}
        }
        if inner.pending.len() >= MAX_PENDING_REQUESTS {
            return Err(PublicError::resource_limit(
                "too many pending session requests",
            ));
        }
        inner.next_request += 1;
        let request_id = inner.next_request;
        let info = SessionRequestInfo {
            request_id,
            connection_id,
            client_name: truncate(client_name, MAX_CLIENT_NAME_CHARS),
            peer_pid,
            purpose: truncate(purpose, MAX_PURPOSE_CHARS),
            requested_vms: requested_vms.into_iter().take(16).collect(),
            requested_capabilities,
            requested_endpoints: requested_endpoints
                .into_iter()
                .take(lightvm_proto::MAX_NETWORK_ENDPOINTS)
                .map(|n| truncate(&n, lightvm_proto::MAX_ENDPOINT_NAME_CHARS))
                .collect(),
            verification_code: code,
            requested_at: UtcTimestamp::now(),
            expires_at: utc_after(self.ttl),
        };
        inner.pending.insert(
            request_id,
            Pending {
                info: info.clone(),
                expires: Instant::now() + self.ttl,
                approving: false,
                decision: None,
                waiter: None,
            },
        );
        if let Some(c) = inner.conns.get_mut(&connection_id) {
            c.request = Some(request_id);
        }
        Ok((info, closed))
    }

    /// Pending (undecided) requests, oldest first.
    pub(crate) fn list(&self) -> (Vec<SessionRequestInfo>, Closed) {
        let mut inner = lock(&self.inner);
        let closed = Self::purge(&mut inner);
        let v = inner
            .pending
            .values()
            .filter(|p| !p.approving && p.decision.is_none())
            .map(|p| p.info.clone())
            .collect();
        (v, closed)
    }

    /// Waits for the decision on a request of this connection.
    pub(crate) fn await_decision(
        &self,
        connection_id: ConnectionId,
        request_id: SessionRequestId,
    ) -> Result<AwaitDecision, PublicError> {
        let mut inner = lock(&self.inner);
        let _ = Self::purge(&mut inner);
        let Some(p) = inner.pending.get_mut(&request_id) else {
            return Err(PublicError::not_found("session request not found"));
        };
        if p.info.connection_id != connection_id {
            return Err(PublicError::not_found("session request not found"));
        }
        if let Some(d) = p.decision.take() {
            inner.pending.remove(&request_id);
            if let Some(c) = inner.conns.get_mut(&connection_id)
                && c.request == Some(request_id)
            {
                c.request = None;
            }
            return Ok(AwaitDecision::Ready(Box::new(d)));
        }
        if p.waiter.is_some() {
            return Err(PublicError::state_conflict("already awaited"));
        }
        let (tx, rx) = oneshot::channel();
        p.waiter = Some(tx);
        let left = p.expires.saturating_duration_since(Instant::now());
        Ok(AwaitDecision::Wait(rx, left))
    }

    /// Takes a pending request for approval: `(connection, peer info)`.
    pub(crate) fn begin_approval(
        &self,
        request_id: SessionRequestId,
    ) -> Result<(SessionRequestInfo, Closed), PublicError> {
        let mut inner = lock(&self.inner);
        let closed = Self::purge(&mut inner);
        let p = inner
            .pending
            .get_mut(&request_id)
            .filter(|p| !p.approving && p.decision.is_none())
            .ok_or_else(|| PublicError::not_found("session request not found"))?;
        p.approving = true;
        Ok((p.info.clone(), closed))
    }

    /// Approval failed in the supervisor: the request is pending again.
    pub(crate) fn abort_approval(&self, request_id: SessionRequestId) {
        if let Some(p) = lock(&self.inner).pending.get_mut(&request_id) {
            p.approving = false;
        }
    }

    /// Binds an approved session to the agent connection and delivers the
    /// grant. `false` if the connection is gone (the caller ends the session).
    pub(crate) fn complete_approval(
        &self,
        request_id: SessionRequestId,
        session_id: SessionId,
        granted: ResponseBody,
    ) -> bool {
        let mut inner = lock(&self.inner);
        let Some(mut p) = inner.pending.remove(&request_id) else {
            return false;
        };
        let conn = p.info.connection_id;
        let Some(c) = inner.conns.get_mut(&conn) else {
            return false;
        };
        c.request = None;
        c.session = Some(session_id);
        inner.sessions.insert(
            session_id,
            Bound {
                connection_id: conn,
                gate: Gate::Active,
            },
        );
        match p.waiter.take() {
            Some(w) => {
                if let Err(Ok(back)) = w.send(Ok(granted)) {
                    // The awaiting task is gone (connection closing): keep
                    // the grant for a new `await_session`.
                    p.decision = Some(Ok(back));
                    p.approving = false;
                    if let Some(c) = inner.conns.get_mut(&conn) {
                        c.request = Some(request_id);
                    }
                    inner.pending.insert(request_id, p);
                }
            }
            None => {
                p.decision = Some(Ok(granted));
                p.approving = false;
                if let Some(c) = inner.conns.get_mut(&conn) {
                    c.request = Some(request_id);
                }
                inner.pending.insert(request_id, p);
            }
        }
        true
    }

    /// Denies a pending request (user, or agent access turned off).
    pub(crate) fn deny(&self, request_id: SessionRequestId, reason: &str) -> bool {
        let mut inner = lock(&self.inner);
        let Some(p) = inner.pending.get_mut(&request_id) else {
            return false;
        };
        if p.approving || p.decision.is_some() {
            return false;
        }
        let e = Err(ReplyError::Operational(
            PublicError::permission_denied("session request denied").with_detail("reason", reason),
        ));
        match p.waiter.take() {
            Some(w) => {
                let _ = w.send(e);
                let conn = p.info.connection_id;
                inner.pending.remove(&request_id);
                if let Some(c) = inner.conns.get_mut(&conn)
                    && c.request == Some(request_id)
                {
                    c.request = None;
                }
            }
            None => p.decision = Some(e),
        }
        true
    }

    /// Ids of every undecided pending request.
    pub(crate) fn pending_ids(&self) -> Vec<SessionRequestId> {
        lock(&self.inner)
            .pending
            .iter()
            .filter(|(_, p)| !p.approving && p.decision.is_none())
            .map(|(id, _)| *id)
            .collect()
    }

    /// Check of an agent call on receipt (connection) and before executing
    /// (supervisor): session bound to the connection, not terminated, not
    /// paused for a mutation. Same messages as the core.
    pub(crate) fn check(
        &self,
        connection_id: ConnectionId,
        session_id: SessionId,
        mutation: bool,
    ) -> Result<(), PublicError> {
        let inner = lock(&self.inner);
        match inner.sessions.get(&session_id) {
            Some(b) if b.connection_id == connection_id => match b.gate {
                Gate::Terminated => Err(unauthenticated()),
                Gate::Paused if mutation => Err(PublicError::permission_denied("session paused")),
                _ => Ok(()),
            },
            _ => Err(unauthenticated()),
        }
    }

    /// Pause on the gate (priority): `false` if not active.
    pub(crate) fn pause(&self, session_id: SessionId) -> bool {
        let mut inner = lock(&self.inner);
        match inner.sessions.get_mut(&session_id) {
            Some(b) if b.gate == Gate::Active => {
                b.gate = Gate::Paused;
                inner.changes.push((session_id, GateChange::Pause));
                true
            }
            _ => false,
        }
    }

    /// Gate back to active after the core resumed the session.
    pub(crate) fn resume(&self, session_id: SessionId) {
        if let Some(b) = lock(&self.inner).sessions.get_mut(&session_id)
            && b.gate == Gate::Paused
        {
            b.gate = Gate::Active;
        }
    }

    /// Terminates a session on the gate and closes its connection (unless
    /// `keep_connection`, for the agent's own `end_session`); `change` is
    /// queued for the core (`None` when the core already terminated it).
    /// Returns the connection if the session was live.
    pub(crate) fn terminate(
        &self,
        session_id: SessionId,
        keep_connection: bool,
        change: Option<GateChange>,
    ) -> Option<ConnectionId> {
        let mut inner = lock(&self.inner);
        let b = inner.sessions.get_mut(&session_id)?;
        if b.gate == Gate::Terminated {
            return None;
        }
        b.gate = Gate::Terminated;
        let conn = b.connection_id;
        if let Some(ch) = change {
            inner.changes.push((session_id, ch));
        }
        if let Some(c) = inner.conns.get_mut(&conn)
            && c.session == Some(session_id)
        {
            c.session = None;
            if !keep_connection {
                let _ = c.session_end.send(true);
            }
        }
        Some(conn)
    }

    /// Session terminated by the core (expiry, Emergency Stop, restore...):
    /// gate closed and connection closed.
    pub(crate) fn terminated(&self, session_id: SessionId) {
        let _ = self.terminate(session_id, false, None);
    }

    /// Revocation decided by the user (priority point).
    pub(crate) fn revoke(&self, session_id: SessionId) -> Option<ConnectionId> {
        self.terminate(
            session_id,
            false,
            Some(GateChange::Terminate {
                cause: TerminationCause::Revoked,
                stop: false,
            }),
        )
    }

    /// Terminates every live session on the gate (Emergency Stop, UI loss,
    /// agent access off) with `cause`. Returns their connections.
    pub(crate) fn terminate_all(&self, cause: TerminationCause, stop: bool) -> Vec<ConnectionId> {
        let ids: Vec<SessionId> = lock(&self.inner)
            .sessions
            .iter()
            .filter(|(_, b)| b.gate != Gate::Terminated)
            .map(|(id, _)| *id)
            .collect();
        ids.into_iter()
            .filter_map(|id| self.terminate(id, false, Some(GateChange::Terminate { cause, stop })))
            .collect()
    }

    /// The connection closed: its pending request is withdrawn and its live
    /// session (if any) must end with `client_lost`.
    pub(crate) fn connection_closed(
        &self,
        connection_id: ConnectionId,
    ) -> (Option<SessionRequestId>, Option<SessionId>) {
        let mut inner = lock(&self.inner);
        let Some(c) = inner.conns.remove(&connection_id) else {
            return (None, None);
        };
        let mut withdrawn = None;
        if let Some(r) = c.request
            && let Some(p) = inner.pending.remove(&r)
        {
            // An approval in progress completes against a closed connection
            // and ends the session it created (`complete_approval` false).
            if p.decision.is_none() {
                withdrawn = Some(r);
            }
        }
        let mut live = None;
        if let Some(s) = c.session
            && let Some(b) = inner.sessions.get_mut(&s)
            && b.gate != Gate::Terminated
        {
            b.gate = Gate::Terminated;
            live = Some(s);
            // ROADMAP 7.3: client lost → revocation and stop.
            inner.changes.push((
                s,
                GateChange::Terminate {
                    cause: TerminationCause::ClientLost,
                    stop: true,
                },
            ));
        }
        (withdrawn, live)
    }
}

impl SessionGate for AgentRegistry {
    fn take_changes(&self) -> Vec<(SessionId, GateChange)> {
        std::mem::take(&mut lock(&self.inner).changes)
    }
}

/// Outcome of [`AgentRegistry::await_decision`].
#[derive(Debug)]
pub(crate) enum AwaitDecision {
    /// Already decided.
    Ready(Box<Decision>),
    /// Wait on the receiver, at most the duration.
    Wait(oneshot::Receiver<Decision>, Duration),
}

pub(crate) fn unauthenticated() -> PublicError {
    PublicError::unauthenticated(vm_core::session::UNAUTHENTICATED_MESSAGE)
}

pub(crate) fn expired() -> ReplyError {
    ReplyError::Operational(
        PublicError::session_expired("session request expired")
            .with_detail("reason", "request_expired"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reg() -> AgentRegistry {
        let r = AgentRegistry::new(Duration::from_secs(60));
        r.set_enabled(true);
        r
    }

    #[test]
    fn requests_need_the_switch_and_one_per_connection() {
        let r = AgentRegistry::new(Duration::from_secs(60));
        let _rx = r.register_conn(1);
        let e = r
            .open_request(
                1,
                None,
                "a",
                "b",
                Vec::new(),
                CapabilitySet::new(),
                Vec::new(),
            )
            .unwrap_err();
        assert_eq!(e.code, lightvm_proto::ErrorCode::PermissionDenied);
        r.set_enabled(true);
        let (info, _) = r
            .open_request(
                1,
                Some(9),
                "n\u{7}ame",
                "p",
                Vec::new(),
                CapabilitySet::new(),
                vec!["SQL.lan".into()],
            )
            .unwrap();
        assert_eq!(info.client_name, "name");
        assert_eq!(info.verification_code.len(), 7);
        assert!(
            r.open_request(
                1,
                None,
                "a",
                "b",
                Vec::new(),
                CapabilitySet::new(),
                Vec::new()
            )
            .is_err()
        );
        // Unknown connection.
        assert!(
            r.open_request(
                2,
                None,
                "a",
                "b",
                Vec::new(),
                CapabilitySet::new(),
                Vec::new()
            )
            .is_err()
        );
    }

    #[test]
    fn deny_before_await_is_delivered_later() {
        let r = reg();
        let _rx = r.register_conn(1);
        let (info, _) = r
            .open_request(
                1,
                None,
                "a",
                "b",
                Vec::new(),
                CapabilitySet::new(),
                Vec::new(),
            )
            .unwrap();
        assert!(r.deny(info.request_id, "denied"));
        assert!(r.list().0.is_empty());
        match r.await_decision(1, info.request_id).unwrap() {
            AwaitDecision::Ready(d) if matches!(*d, Err(ReplyError::Operational(_))) => {
                let Err(ReplyError::Operational(e)) = *d else {
                    unreachable!()
                };
                assert_eq!(e.code, lightvm_proto::ErrorCode::PermissionDenied);
            }
            other => panic!("{other:?}"),
        }
        // Another connection cannot await it.
        assert!(r.await_decision(2, info.request_id).is_err());
    }

    #[test]
    fn expired_requests_are_purged() {
        let r = AgentRegistry::new(Duration::from_millis(1));
        r.set_enabled(true);
        let _rx = r.register_conn(1);
        let (info, _) = r
            .open_request(
                1,
                None,
                "a",
                "b",
                Vec::new(),
                CapabilitySet::new(),
                Vec::new(),
            )
            .unwrap();
        std::thread::sleep(Duration::from_millis(5));
        let (list, closed) = r.list();
        assert!(list.is_empty());
        assert_eq!(
            closed,
            vec![(info.request_id, SessionRequestOutcome::Expired)]
        );
    }

    #[test]
    fn gate_checks_binding_pause_and_termination() {
        let r = reg();
        let mut end = r.register_conn(1);
        let (info, _) = r
            .open_request(
                1,
                None,
                "a",
                "b",
                Vec::new(),
                CapabilitySet::new(),
                Vec::new(),
            )
            .unwrap();
        r.begin_approval(info.request_id).unwrap();
        // Not approvable twice while in progress.
        assert!(r.begin_approval(info.request_id).is_err());
        let s = SessionId::from_unix_millis(1, [1; 10]);
        assert!(r.complete_approval(info.request_id, s, ResponseBody::Pong));
        assert!(r.check(1, s, true).is_ok());
        // Bound to connection 1 only.
        assert!(r.check(2, s, false).is_err());
        assert!(r.pause(s));
        assert!(r.check(1, s, false).is_ok());
        assert_eq!(
            r.check(1, s, true).unwrap_err().code,
            lightvm_proto::ErrorCode::PermissionDenied
        );
        r.resume(s);
        assert!(r.check(1, s, true).is_ok());
        assert_eq!(r.revoke(s), Some(1));
        assert!(*end.borrow_and_update());
        assert_eq!(
            r.check(1, s, false).unwrap_err().code,
            lightvm_proto::ErrorCode::Unauthenticated
        );
        assert_eq!(r.revoke(s), None);
        // The pause and the revocation are queued for the core, in order.
        assert_eq!(
            r.take_changes(),
            vec![
                (s, GateChange::Pause),
                (
                    s,
                    GateChange::Terminate {
                        cause: TerminationCause::Revoked,
                        stop: false
                    }
                )
            ]
        );
    }

    #[test]
    fn closing_the_connection_withdraws_and_ends() {
        let r = reg();
        let _rx = r.register_conn(1);
        let (info, _) = r
            .open_request(
                1,
                None,
                "a",
                "b",
                Vec::new(),
                CapabilitySet::new(),
                Vec::new(),
            )
            .unwrap();
        assert_eq!(r.connection_closed(1), (Some(info.request_id), None));
        let _rx = r.register_conn(2);
        let (info, _) = r
            .open_request(
                2,
                None,
                "a",
                "b",
                Vec::new(),
                CapabilitySet::new(),
                Vec::new(),
            )
            .unwrap();
        r.begin_approval(info.request_id).unwrap();
        let s = SessionId::from_unix_millis(1, [2; 10]);
        assert!(r.complete_approval(info.request_id, s, ResponseBody::Pong));
        let (_, live) = r.connection_closed(2);
        assert_eq!(live, Some(s));
        assert!(r.check(2, s, false).is_err());
    }
}
