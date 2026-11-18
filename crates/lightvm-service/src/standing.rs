//! Standing grant on the service side (ADR-0039): validation of what the
//! user grants and conformity of the requests presented with its pairing
//! credential. Pure rules, without I/O; the server holds the live grant,
//! the catalog keeps it (policy and credential hash only).
//!
//! A request presented with the standing credential is approved without a
//! click only when it conforms:
//! - the requested VMs are none (no preference) or exactly the grant's VM;
//! - the requested endpoints are within the grant's (none = all of them);
//! - the capabilities granted are the grant's ∩ the requested ones, never
//!   more than the grant, and not empty;
//! - the declared client name matches, when the grant sets one;
//! - the grant is active (not suspended), not expired, and no other session
//!   is live (checked by the server).
//!
//! A request that does not conform is **denied**, never queued for a manual
//! approval: an unattended agent must not pile up requests waiting for a
//! click.

use std::time::Duration;

use lightvm_proto::ipc::{
    AGENT_DEFAULT_QUOTAS, AGENT_MAX_SCREENSHOT_BYTES, MAX_SESSION_DURATION_S, MIN_LEASE_MS,
};
use lightvm_proto::{
    Capability, CapabilitySet, LEASE_MAX_MILLIS, MAX_STANDING_GRANT_S, PublicError, StandingGrant,
    StandingGrantId, StandingGrantSpec, StandingGrantState, UtcTimestamp, VmId,
    standing::MAX_STANDING_CLIENT_NAME_CHARS,
};

/// Denial of a request presented with the standing credential, with a
/// stable reason in `detail.reason`.
pub(crate) fn denied(reason: &'static str, message: &str) -> PublicError {
    PublicError::permission_denied(message.to_owned()).with_detail("reason", reason)
}

/// Checks what the user grants and builds the grant (state active).
pub(crate) fn build(
    spec: StandingGrantSpec,
    grant_id: StandingGrantId,
    now: UtcTimestamp,
    vm_config_revision: u64,
    min_duration: Duration,
) -> Result<StandingGrant, PublicError> {
    let min = u32::try_from(min_duration.as_secs())
        .unwrap_or(u32::MAX)
        .max(1);
    if !(min..=MAX_STANDING_GRANT_S).contains(&spec.duration_s) {
        return Err(PublicError::invalid_argument(format!(
            "duration_s outside {min}..={MAX_STANDING_GRANT_S}"
        )));
    }
    if spec.capabilities.is_empty() {
        return Err(PublicError::invalid_argument("no capability granted"));
    }
    let lease = spec.lease_ms.unwrap_or(10_000);
    if !(i64::from(MIN_LEASE_MS)..=LEASE_MAX_MILLIS).contains(&i64::from(lease)) {
        return Err(PublicError::invalid_argument(format!(
            "lease_ms outside {MIN_LEASE_MS}..={LEASE_MAX_MILLIS}"
        )));
    }
    let mut capabilities = spec.capabilities;
    let network_endpoints: Vec<String> = spec
        .network_endpoints
        .iter()
        .map(|n| n.trim().trim_end_matches('.').to_ascii_lowercase())
        .filter(|n| !n.is_empty())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    if network_endpoints.len() > lightvm_proto::MAX_NETWORK_ENDPOINTS {
        return Err(PublicError::invalid_argument("too many network endpoints"));
    }
    if network_endpoints.is_empty() {
        // ADR-0033: the capability without any endpoint grants nothing.
        capabilities.remove(&Capability::NetworkEndpoints);
    } else if !capabilities.contains(&Capability::NetworkEndpoints) {
        return Err(PublicError::invalid_argument(
            "network endpoints granted without the network.endpoints capability",
        ));
    }
    if capabilities.is_empty() {
        return Err(PublicError::invalid_argument("no capability granted"));
    }
    let client_name = match spec.client_name.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(n)
            if n.chars().count() > MAX_STANDING_CLIENT_NAME_CHARS
                || n.contains(char::is_control) =>
        {
            return Err(PublicError::invalid_argument("client name not valid"));
        }
        Some(n) => Some(n.to_owned()),
    };
    let mut quotas = spec.quotas.unwrap_or(AGENT_DEFAULT_QUOTAS);
    quotas.max_screenshot_bytes = quotas.max_screenshot_bytes.min(AGENT_MAX_SCREENSHOT_BYTES);
    let expires_at = now
        .checked_add_millis(i64::from(spec.duration_s) * 1000)
        .ok_or_else(|| PublicError::invalid_argument("expiry out of range"))?;
    Ok(StandingGrant {
        grant_id,
        vm_id: spec.vm_id,
        capabilities,
        tools: spec.tools,
        network_endpoints,
        quotas,
        lease_ms: lease,
        give_input: spec.give_input,
        client_name,
        created_at: now,
        expires_at,
        vm_config_revision,
        state: StandingGrantState::Active,
        sessions_approved: 0,
        last_session_at: None,
    })
}

/// What a conforming request receives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Conforming {
    pub(crate) capabilities: CapabilitySet,
    pub(crate) network_endpoints: Vec<String>,
    /// Session duration: `min(86 400 s, time left of the grant)`.
    pub(crate) duration_ms: i64,
}

/// Conformity of a request with the grant (the server checks besides that
/// no other session is live).
pub(crate) fn conform(
    g: &StandingGrant,
    now: UtcTimestamp,
    client_name: &str,
    requested_vms: &[VmId],
    requested_capabilities: &CapabilitySet,
    requested_endpoints: &[String],
) -> Result<Conforming, PublicError> {
    match g.state {
        StandingGrantState::Active => {}
        StandingGrantState::Suspended => {
            return Err(denied(
                "standing_grant_suspended",
                "the standing grant is suspended: the configuration of its VM changed; confirm it again in the app",
            ));
        }
        _ => return Err(denied("standing_grant_ended", "the standing grant ended")),
    }
    let left = g.expires_at.millis_since(now);
    if left <= 0 {
        return Err(denied(
            "standing_grant_expired",
            "the standing grant expired",
        ));
    }
    if let Some(expected) = &g.client_name
        && expected != client_name
    {
        return Err(denied(
            "standing_client_mismatch",
            "the client name differs from the one of the standing grant",
        ));
    }
    if requested_vms.iter().any(|v| *v != g.vm_id) {
        return Err(denied(
            "standing_vm_mismatch",
            "the standing grant covers another VM",
        ));
    }
    let wanted: Vec<String> = requested_endpoints
        .iter()
        .map(|n| n.trim().trim_end_matches('.').to_ascii_lowercase())
        .filter(|n| !n.is_empty())
        .collect();
    if let Some(extra) = wanted.iter().find(|n| !g.network_endpoints.contains(n)) {
        return Err(denied(
            "standing_endpoint_not_granted",
            &format!("network endpoint {extra:?} not in the standing grant"),
        ));
    }
    let network_endpoints = if wanted.is_empty() {
        g.network_endpoints.clone()
    } else {
        let mut w = wanted;
        w.sort();
        w.dedup();
        w
    };
    let mut capabilities: CapabilitySet = g
        .capabilities
        .intersection(requested_capabilities)
        .copied()
        .collect();
    if network_endpoints.is_empty() {
        capabilities.remove(&Capability::NetworkEndpoints);
    }
    if capabilities.is_empty() {
        return Err(denied(
            "standing_no_capability",
            "none of the requested capabilities is in the standing grant",
        ));
    }
    let duration_ms = left.min(i64::from(MAX_SESSION_DURATION_S) * 1000);
    Ok(Conforming {
        capabilities,
        network_endpoints,
        duration_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> StandingGrantSpec {
        StandingGrantSpec {
            vm_id: VmId::new_v7(),
            capabilities: [
                Capability::VmList,
                Capability::VmStatus,
                Capability::VmScreenshot,
                Capability::NetworkEndpoints,
            ]
            .into_iter()
            .collect(),
            tools: Default::default(),
            network_endpoints: vec!["SQL.lan.".into(), "sql.lan".into()],
            quotas: None,
            lease_ms: None,
            give_input: false,
            duration_s: 3600,
            client_name: None,
        }
    }

    fn grant() -> StandingGrant {
        build(
            spec(),
            StandingGrantId::new_v7(),
            UtcTimestamp::now(),
            3,
            Duration::from_secs(60),
        )
        .unwrap()
    }

    #[test]
    fn duration_is_bounded_to_seven_days() {
        let mut s = spec();
        s.duration_s = MAX_STANDING_GRANT_S + 1;
        let e = build(
            s,
            StandingGrantId::new_v7(),
            UtcTimestamp::now(),
            1,
            Duration::from_secs(60),
        )
        .unwrap_err();
        assert_eq!(e.code, lightvm_proto::ErrorCode::InvalidArgument);
        let mut s = spec();
        s.duration_s = MAX_STANDING_GRANT_S;
        let g = build(
            s,
            StandingGrantId::new_v7(),
            UtcTimestamp::now(),
            1,
            Duration::from_secs(60),
        )
        .unwrap();
        assert_eq!(
            g.expires_at.millis_since(g.created_at),
            i64::from(MAX_STANDING_GRANT_S) * 1000
        );
        let mut s = spec();
        s.duration_s = 59;
        assert!(
            build(
                s,
                StandingGrantId::new_v7(),
                UtcTimestamp::now(),
                1,
                Duration::from_secs(60)
            )
            .is_err()
        );
    }

    #[test]
    fn spec_is_normalised() {
        let g = grant();
        assert_eq!(g.network_endpoints, vec!["sql.lan".to_string()]);
        assert_eq!(g.lease_ms, 10_000);
        assert_eq!(g.vm_config_revision, 3);
        assert_eq!(g.state, StandingGrantState::Active);
        let mut s = spec();
        s.network_endpoints.clear();
        let g = build(
            s,
            StandingGrantId::new_v7(),
            UtcTimestamp::now(),
            1,
            Duration::from_secs(60),
        )
        .unwrap();
        assert!(!g.capabilities.contains(&Capability::NetworkEndpoints));
        let mut s = spec();
        s.capabilities.remove(&Capability::NetworkEndpoints);
        assert!(
            build(
                s,
                StandingGrantId::new_v7(),
                UtcTimestamp::now(),
                1,
                Duration::from_secs(60)
            )
            .is_err()
        );
        let mut s = spec();
        s.lease_ms = Some(10);
        assert!(
            build(
                s,
                StandingGrantId::new_v7(),
                UtcTimestamp::now(),
                1,
                Duration::from_secs(60)
            )
            .is_err()
        );
    }

    #[test]
    fn conforming_requests_get_the_intersection() {
        let g = grant();
        let all: CapabilitySet = Capability::ALL.into_iter().collect();
        let c = conform(&g, UtcTimestamp::now(), "x", &[], &all, &[]).unwrap();
        assert_eq!(c.capabilities, g.capabilities);
        assert_eq!(c.network_endpoints, vec!["sql.lan".to_string()]);
        assert!(c.duration_ms <= 3_600_000 && c.duration_ms > 3_500_000);
        let few: CapabilitySet = [Capability::VmList, Capability::VmInput]
            .into_iter()
            .collect();
        let c = conform(&g, UtcTimestamp::now(), "x", &[g.vm_id], &few, &[]).unwrap();
        assert_eq!(c.capabilities, [Capability::VmList].into_iter().collect());
    }

    #[test]
    fn non_conforming_requests_are_denied_with_a_reason() {
        let g = grant();
        let all: CapabilitySet = Capability::ALL.into_iter().collect();
        let reason = |e: PublicError| {
            assert_eq!(e.code, lightvm_proto::ErrorCode::PermissionDenied);
            e.details
                .and_then(|d| d.get("reason").cloned())
                .unwrap_or_default()
        };
        let now = UtcTimestamp::now();
        assert_eq!(
            reason(conform(&g, now, "x", &[VmId::new_v7()], &all, &[]).unwrap_err()),
            "standing_vm_mismatch"
        );
        assert_eq!(
            reason(conform(&g, now, "x", &[], &all, &["other.lan".into()]).unwrap_err()),
            "standing_endpoint_not_granted"
        );
        let none: CapabilitySet = [Capability::VmInput].into_iter().collect();
        assert_eq!(
            reason(conform(&g, now, "x", &[], &none, &[]).unwrap_err()),
            "standing_no_capability"
        );
        let mut named = g.clone();
        named.client_name = Some("agent".into());
        assert_eq!(
            reason(conform(&named, now, "other", &[], &all, &[]).unwrap_err()),
            "standing_client_mismatch"
        );
        assert!(conform(&named, now, "agent", &[], &all, &[]).is_ok());
        let mut s = g.clone();
        s.state = StandingGrantState::Suspended;
        assert_eq!(
            reason(conform(&s, now, "x", &[], &all, &[]).unwrap_err()),
            "standing_grant_suspended"
        );
        let later = g.expires_at.checked_add_millis(1).unwrap();
        assert_eq!(
            reason(conform(&g, later, "x", &[], &all, &[]).unwrap_err()),
            "standing_grant_expired"
        );
    }
}
