//! Registro delle sessioni agente (ROADMAP 7.3, 9).
//!
//! Il grant e il lease sono verificati dal core (cioè dal supervisor), mai dal
//! client. Una sessione terminata non torna attiva: la ripresa dell'agente
//! richiede una nuova autorizzazione.
//!
//! Codici dei dinieghi (semantica in `lightvm_proto::ErrorCode`):
//! - `unauthenticated`: token assente, sessione sconosciuta, token errato,
//!   sessione terminata per revoca, Emergency Stop, perdita del client, chiusura
//!   UI, ripresa dell'host, ripristino o qualunque causa diversa dalla scadenza;
//!   token riusato dopo un riavvio del supervisor (nessuna sessione sopravvive);
//! - `session_expired`: grant o lease scaduti (anche dopo la terminazione per scadenza);
//! - `permission_denied`: sessione valida ma VM fuori allowlist, capability non
//!   concessa o mutazione durante la pausa.

use std::collections::BTreeMap;

use lightvm_proto::{
    Capability, Grant, LEASE_MAX_MILLIS, Lease, PublicError, Quotas, SessionId, SessionState,
    SessionToken, TerminationCause, UtcTimestamp, VmId,
};

/// Messaggio unico per ogni diniego `unauthenticated` di una sessione agente.
pub const UNAUTHENTICATED_MESSAGE: &str = "credenziale di sessione non valida";

/// Codice con cui si scartano le operazioni non inoltrate di una sessione
/// terminata con la causa data: lo stesso che darebbe la seconda verifica.
#[must_use]
pub fn discard_code_for(cause: TerminationCause) -> lightvm_proto::ErrorCode {
    match cause {
        TerminationCause::GrantExpired | TerminationCause::LeaseExpired => {
            lightvm_proto::ErrorCode::SessionExpired
        }
        _ => lightvm_proto::ErrorCode::Unauthenticated,
    }
}

/// Contatori di consumo delle quote.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SessionUsage {
    /// Operazioni accettate.
    pub operations: u32,
    /// Eventi di input accettati.
    pub input_events: u32,
    /// Screenshot accettati.
    pub screenshots: u32,
}

/// Sessione agente in memoria del supervisor.
#[derive(Clone, Debug)]
pub struct Session {
    /// Identificativo.
    pub id: SessionId,
    token: SessionToken,
    /// Grant emesso.
    pub grant: Grant,
    /// Lease corrente.
    pub lease: Lease,
    /// Stato.
    pub state: SessionState,
    /// Causa di terminazione, se terminata.
    pub termination: Option<TerminationCause>,
    /// Istante di creazione.
    pub created_at: UtcTimestamp,
    /// Consumo delle quote.
    pub usage: SessionUsage,
}

impl Session {
    /// `true` se il token presentato corrisponde (confronto a tempo costante).
    #[must_use]
    pub fn token_matches(&self, presented: &SessionToken) -> bool {
        self.token.ct_eq(presented)
    }

    /// Motivo di scadenza all'istante `now`, se scaduta.
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

/// Richiesta di creazione di una sessione, approvata dall'utente nell'app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionSpec {
    /// VM ammesse.
    pub vm_allowlist: std::collections::BTreeSet<VmId>,
    /// Capability concesse.
    pub capabilities: lightvm_proto::CapabilitySet,
    /// Quote concesse.
    pub quotas: Quotas,
    /// Scadenza assoluta del grant.
    pub expires_at: UtcTimestamp,
    /// Durata del lease in ms (1..=10000).
    pub lease_millis: i64,
}

/// Registro delle sessioni.
#[derive(Clone, Debug, Default)]
pub struct SessionRegistry {
    sessions: BTreeMap<SessionId, Session>,
}

impl SessionRegistry {
    /// Sessione per id.
    #[must_use]
    pub fn get(&self, id: SessionId) -> Option<&Session> {
        self.sessions.get(&id)
    }

    pub(crate) fn get_mut(&mut self, id: SessionId) -> Option<&mut Session> {
        self.sessions.get_mut(&id)
    }

    /// Sessioni non terminate.
    pub fn live(&self) -> impl Iterator<Item = &Session> {
        self.sessions
            .values()
            .filter(|s| s.state != SessionState::Terminated)
    }

    /// Tutte le sessioni registrate (incluse le terminate, per i report).
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

    /// Verifica pura di una chiamata: token, stato, scadenze, allowlist, pausa.
    /// La capability rispetto alla policy effettiva è verificata dal chiamante.
    pub(crate) fn check(
        &self,
        id: SessionId,
        token: Option<&SessionToken>,
        vm_id: Option<VmId>,
        capability: Capability,
        now: UtcTimestamp,
    ) -> Result<(), PublicError> {
        // Token assente, sessione inesistente, token errato e sessione terminata
        // (non per scadenza) danno lo stesso codice e lo stesso messaggio: il
        // client non distingue quali sessioni esistono.
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
                    PublicError::session_expired("sessione scaduta")
                }
                _ => unauthenticated(),
            });
        }
        // La scadenza prevale sulla pausa: una credenziale scaduta non è più valida.
        if session.expiry_at(now).is_some() {
            return Err(PublicError::session_expired("grant o lease scaduto"));
        }
        if session.state == SessionState::Paused && capability.is_mutation() {
            return Err(PublicError::permission_denied("sessione in pausa"));
        }
        if let Some(vm) = vm_id
            && !session.grant.vm_allowlist.contains(&vm)
        {
            return Err(PublicError::permission_denied(
                "VM non autorizzata dal grant",
            ));
        }
        Ok(())
    }

    /// Rinnova il lease. Un lease già scaduto non si rinnova.
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

    /// Sessioni vive scadute all'istante `now`, con la causa.
    #[must_use]
    pub fn due_expirations(&self, now: UtcTimestamp) -> Vec<(SessionId, TerminationCause)> {
        self.live()
            .filter_map(|s| s.expiry_at(now).map(|c| (s.id, c)))
            .collect()
    }

    /// Marca la sessione come terminata. `false` se già terminata o inesistente.
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

    /// Sessioni vive che includono la VM nell'allowlist.
    #[must_use]
    pub fn live_for_vm(&self, vm_id: VmId) -> Vec<SessionId> {
        self.live()
            .filter(|s| s.grant.vm_allowlist.contains(&vm_id))
            .map(|s| s.id)
            .collect()
    }
}
