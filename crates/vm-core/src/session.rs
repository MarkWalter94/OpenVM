//! Registry of agent sessions (ROADMAP 7.3, 9).
//!
//! The grant and the lease are checked by the core (i.e. by the supervisor), never by
//! the client. A terminated session does not become active again: resuming the agent
//! requires a new authorization.
//!
//! Denial codes (semantics in `lightvm_proto::ErrorCode`):
//! - `unauthenticated`: missing token, unknown session, wrong token,
//!   session terminated by revocation, Emergency Stop, client loss, UI
//!   close, host resume, restore or any cause other than expiry;
//!   token reused after a supervisor restart (no session survives);
//! - `session_expired`: grant or lease expired (also after termination by expiry);
//! - `permission_denied`: valid session but VM outside the allowlist, capability not
//!   granted or mutation while paused.

use std::collections::BTreeMap;

use lightvm_proto::{
    Capability, Grant, LEASE_MAX_MILLIS, Lease, PublicError, Quotas, SessionId, SessionState,
    SessionToken, TerminationCause, UtcTimestamp, VmId,
};

/// Single message for every `unauthenticated` denial of an agent session.
pub const UNAUTHENTICATED_MESSAGE: &str = "invalid session credential";

/// Code with which the non-forwarded operations of a session terminated with
/// the given cause are discarded: the same the second check would give.
#[must_use]
pub fn discard_code_for(cause: TerminationCause) -> lightvm_proto::ErrorCode {
    match cause {
        TerminationCause::GrantExpired | TerminationCause::LeaseExpired => {
            lightvm_proto::ErrorCode::SessionExpired
        }
        _ => lightvm_proto::ErrorCode::Unauthenticated,
    }
}

/// Quota consumption counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SessionUsage {
    /// Accepted operations.
    pub operations: u32,
    /// Accepted input events.
    pub input_events: u32,
    /// Accepted screenshots.
    pub screenshots: u32,
}

/// Agent session in the supervisor's memory.
#[derive(Clone, Debug)]
pub struct Session {
    /// Identifier.
    pub id: SessionId,
    token: SessionToken,
    /// Issued grant.
    pub grant: Grant,
    /// Current lease.
    pub lease: Lease,
    /// State.
    pub state: SessionState,
    /// Termination cause, if terminated.
    pub termination: Option<TerminationCause>,
    /// Creation instant.
    pub created_at: UtcTimestamp,
    /// Quota consumption.
    pub usage: SessionUsage,
}

impl Session {
    /// `true` if the presented token matches (constant-time comparison).
    #[must_use]
    pub fn token_matches(&self, presented: &SessionToken) -> bool {
        self.token.ct_eq(presented)
    }

    /// Expiry reason at instant `now`, if expired.
    #[must_use]
    pub fn expiry_at(&self, now: UtcTimestamp) -> Option<TerminationCause> {
        if now >= self.grant.expires_at {
            Some(TerminationCause::GrantExpired)
        } else if !self.lease.is_valid_at(now) {
            Some(TerminationCause::LeaseExpired)
        } else {
            None
        }
    }
}

/// Session creation request, approved by the user in the app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionSpec {
    /// Allowed VMs.
    pub vm_allowlist: std::collections::BTreeSet<VmId>,
    /// Granted capabilities.
    pub capabilities: lightvm_proto::CapabilitySet,
    /// Granted quotas.
    pub quotas: Quotas,
    /// Absolute expiry of the grant.
    pub expires_at: UtcTimestamp,
    /// Lease duration in ms (1..=10000).
    pub lease_millis: i64,
    /// Network endpoints granted with `network.endpoints` (names of the
    /// VM's endpoints; empty without the capability). ADR-0033.
    pub network_endpoints: std::collections::BTreeSet<String>,
}

/// Session registry.
#[derive(Clone, Debug, Default)]
pub struct SessionRegistry {
    sessions: BTreeMap<SessionId, Session>,
}

impl SessionRegistry {
    /// Session by id.
    #[must_use]
    pub fn get(&self, id: SessionId) -> Option<&Session> {
        self.sessions.get(&id)
    }

    pub(crate) fn get_mut(&mut self, id: SessionId) -> Option<&mut Session> {
        self.sessions.get_mut(&id)
    }

    /// Non-terminated sessions.
    pub fn live(&self) -> impl Iterator<Item = &Session> {
        self.sessions
            .values()
            .filter(|s| s.state != SessionState::Terminated)
    }

    /// All registered sessions (including terminated ones, for reports).
    pub fn all(&self) -> impl Iterator<Item = &Session> {
        self.sessions.values()
    }

    pub(crate) fn insert(
        &mut self,
        id: SessionId,
        token: SessionToken,
        grant: Grant,
        lease: Lease,
        now: UtcTimestamp,
    ) {
        self.sessions.insert(
            id,
            Session {
                id,
                token,
                grant,
                lease,
                state: SessionState::Active,
                termination: None,
                created_at: now,
                usage: SessionUsage::default(),
            },
        );
    }

    /// Pure check of a call: token, state, expiries, allowlist, pause.
    /// The capability against the effective policy is checked by the caller.
    pub(crate) fn check(
        &self,
        id: SessionId,
        token: Option<&SessionToken>,
        vm_id: Option<VmId>,
        capability: Capability,
        now: UtcTimestamp,
    ) -> Result<(), PublicError> {
        // Missing token, nonexistent session, wrong token and terminated session
        // (not by expiry) give the same code and the same message: the
        // client cannot tell which sessions exist.
        let unauthenticated = || PublicError::unauthenticated(UNAUTHENTICATED_MESSAGE);
        let Some(token) = token else {
            return Err(unauthenticated());
        };
        let session = self
            .sessions
            .get(&id)
            .filter(|s| s.token_matches(token))
            .ok_or_else(unauthenticated)?;
        if session.state == SessionState::Terminated {
            return Err(match session.termination {
                Some(TerminationCause::GrantExpired | TerminationCause::LeaseExpired) => {
                    PublicError::session_expired("session expired")
                }
                _ => unauthenticated(),
            });
        }
        // Expiry takes precedence over pause: an expired credential is no longer valid.
        if session.expiry_at(now).is_some() {
            return Err(PublicError::session_expired("grant or lease expired"));
        }
        if session.state == SessionState::Paused && capability.is_mutation() {
            return Err(PublicError::permission_denied("session paused"));
        }
        if let Some(vm) = vm_id
            && !session.grant.vm_allowlist.contains(&vm)
        {
            return Err(PublicError::permission_denied(
                "VM not authorized by the grant",
            ));
        }
        Ok(())
    }

    /// Renews the lease. An already expired lease is not renewed.
    pub(crate) fn renew(
        &mut self,
        id: SessionId,
        token: &SessionToken,
        lease_millis: i64,
        now: UtcTimestamp,
    ) -> Result<Lease, PublicError> {
        self.check(id, Some(token), None, Capability::VmStatus, now)?;
        let lease = Lease::new(id, now, lease_millis.min(LEASE_MAX_MILLIS))?;
        if let Some(s) = self.sessions.get_mut(&id) {
            s.lease = lease;
        }
        Ok(lease)
    }

    /// Live sessions expired at instant `now`, with the cause.
    #[must_use]
    pub fn due_expirations(&self, now: UtcTimestamp) -> Vec<(SessionId, TerminationCause)> {
        self.live()
            .filter_map(|s| s.expiry_at(now).map(|c| (s.id, c)))
            .collect()
    }

    /// Marks the session as terminated. `false` if already terminated or nonexistent.
    pub(crate) fn terminate(&mut self, id: SessionId, cause: TerminationCause) -> bool {
        match self.sessions.get_mut(&id) {
            Some(s) if s.state != SessionState::Terminated => {
                s.state = SessionState::Terminated;
                s.termination = Some(cause);
                true
            }
            _ => false,
        }
    }

    /// Live sessions that include the VM in the allowlist.
    #[must_use]
    pub fn live_for_vm(&self, vm_id: VmId) -> Vec<SessionId> {
        self.live()
            .filter(|s| s.grant.vm_allowlist.contains(&vm_id))
            .map(|s| s.id)
            .collect()
    }
}
