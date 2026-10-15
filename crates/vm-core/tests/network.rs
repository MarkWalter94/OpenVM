//! ADR-0033 at the core level: network modes in the settings, the network
//! of a run decided at its start (NAT in manual use only, the endpoint
//! allowlist or no network under an agent session), the `network.endpoints`
//! capability and the allowed endpoints of a running VM.

mod common;

use std::collections::BTreeSet;
use std::net::Ipv4Addr;

use common::*;
use vm_core::CoreConfig;
use vm_core::proto::*;
use vm_core::session::SessionSpec;

fn roomy() -> CoreConfig {
    let mut c = config();
    c.global_limits.max_agent_sessions = 16;
    c.global_limits.max_running_vms = 4;
    c
}

fn endpoint(name: &str, port: u16, last: u8) -> NetworkEndpoint {
    NetworkEndpoint {
        name: name.into(),
        host: "127.0.0.1".into(),
        port,
        virtual_ip: Ipv4Addr::new(10, 0, 2, last),
    }
}

/// A VM with two endpoints in `mode`.
fn vm_with(h: &mut Harness, name: &str, mode: NetworkProfile) -> VmId {
    let m = h.install_media();
    let mut cfg = h.vm_config(name, Some(m));
    cfg.network = mode;
    cfg.endpoints = vec![
        endpoint("sql.lan", 1433, 101),
        endpoint("WcfHost", 8733, 102),
    ];
    h.core.create_vm(&Caller::User, cfg).unwrap().id
}

fn spec(vm: VmId, caps: &[Capability], endpoints: &[&str], h: &Harness) -> SessionSpec {
    SessionSpec {
        tools: Default::default(),
        vm_allowlist: [vm].into_iter().collect(),
        capabilities: caps.iter().copied().collect(),
        quotas: Quotas::UNBOUNDED,
        expires_at: UtcTimestamp::from_unix_millis(h.clock.0.get() + 3_600_000).unwrap(),
        lease_millis: 10_000,
        network_endpoints: endpoints.iter().map(|s| (*s).to_owned()).collect(),
    }
}

fn names(v: &[&str]) -> BTreeSet<String> {
    v.iter().map(|s| (*s).to_owned()).collect()
}

const BASE: [Capability; 4] = [
    Capability::VmList,
    Capability::VmStatus,
    Capability::VmStart,
    Capability::VmStop,
];

#[test]
fn settings_validate_modes_and_endpoints() {
    let mut h = Harness::new();
    let m = h.install_media();
    let mut cfg = h.vm_config("a", Some(m));
    cfg.network = NetworkProfile::Endpoints;
    let e = h.core.create_vm(&Caller::User, cfg.clone()).unwrap_err();
    assert_eq!(
        e.code,
        ErrorCode::InvalidArgument,
        "endpoints mode without endpoints"
    );
    cfg.endpoints = vec![endpoint("*.lan", 1, 101)];
    assert_eq!(
        h.core
            .create_vm(&Caller::User, cfg.clone())
            .unwrap_err()
            .code,
        ErrorCode::InvalidArgument
    );
    cfg.endpoints = vec![endpoint("sql", 0, 101)];
    assert_eq!(
        h.core
            .create_vm(&Caller::User, cfg.clone())
            .unwrap_err()
            .code,
        ErrorCode::InvalidArgument
    );
    cfg.endpoints = vec![endpoint("sql", 1433, 101)];
    h.core.create_vm(&Caller::User, cfg.clone()).unwrap();
    // NAT keeps the endpoint list (used when a session is granted it).
    cfg.name = VmName::parse("b").unwrap();
    cfg.network = NetworkProfile::Nat;
    h.core.create_vm(&Caller::User, cfg.clone()).unwrap();
    // Windows on ARM: offline only.
    cfg.name = VmName::parse("c").unwrap();
    cfg.guest_os = GuestOs::Windows11Arm64;
    cfg.memory_mib = 8192;
    cfg.disk_gib = 64;
    assert_eq!(
        h.core.create_vm(&Caller::User, cfg).unwrap_err().code,
        ErrorCode::Unsupported
    );
}

#[test]
fn manual_runs_use_the_mode_of_the_settings() {
    let mut h = Harness::with_config(roomy());
    let off = h.create_vm("off");
    let nat = vm_with(&mut h, "nat", NetworkProfile::Nat);
    let eps = vm_with(&mut h, "eps", NetworkProfile::Endpoints);
    for vm in [off, nat, eps] {
        h.start(vm);
    }
    assert_eq!(
        h.backend.networks,
        [
            NetworkProfile::Offline,
            NetworkProfile::Nat,
            NetworkProfile::Endpoints
        ]
    );
    assert_eq!(h.core.run_network(nat), Some(NetworkProfile::Nat));
    assert_eq!(
        h.core.allowed_endpoints(eps, false),
        names(&["sql.lan", "wcfhost"]),
        "manual use: every endpoint"
    );
    h.stop(nat);
    assert_eq!(h.core.run_network(nat), None);
}

#[test]
fn agent_sessions_never_get_nat() {
    let mut h = Harness::with_config(roomy());
    let nat = vm_with(&mut h, "nat", NetworkProfile::Nat);
    h.start(nat);
    // A running NAT VM refuses the session: no hidden change while it runs.
    let mut caps = BASE.to_vec();
    caps.push(Capability::NetworkEndpoints);
    let e = h
        .core
        .create_session(
            spec(nat, &caps, &["sql.lan"], &h),
            SessionToken::from_bytes([1; 32]),
        )
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::StateConflict);
    assert_eq!(
        e.details
            .as_ref()
            .and_then(|d| d.get("reason"))
            .map(String::as_str),
        Some("nat_running")
    );
    h.stop(nat);
    // Stopped: allowed; the next run (even started by the user) gets the
    // session's endpoint allowlist, not NAT.
    let (grant, _) = h
        .core
        .create_session(
            spec(nat, &caps, &["sql.lan"], &h),
            SessionToken::from_bytes([1; 32]),
        )
        .unwrap();
    assert_eq!(grant.network_endpoints, names(&["sql.lan"]));
    h.start(nat);
    assert_eq!(h.backend.networks.last(), Some(&NetworkProfile::Endpoints));
    assert_eq!(h.core.allowed_endpoints(nat, true), names(&["sql.lan"]));
}

#[test]
fn without_the_capability_the_session_runs_offline() {
    let mut h = Harness::with_config(roomy());
    let eps = vm_with(&mut h, "eps", NetworkProfile::Endpoints);
    let nat = vm_with(&mut h, "nat", NetworkProfile::Nat);
    for vm in [eps, nat] {
        let (sid, _) = h.session(&[vm], &BASE, 2);
        let s = h.core.sessions().get(sid).unwrap();
        assert!(s.grant.network_endpoints.is_empty());
        h.start(vm);
        assert_eq!(h.backend.networks.last(), Some(&NetworkProfile::Offline));
        assert!(h.core.allowed_endpoints(vm, true).is_empty());
    }
}

#[test]
fn granted_endpoints_must_belong_to_the_vm() {
    let mut h = Harness::with_config(roomy());
    let eps = vm_with(&mut h, "eps", NetworkProfile::Endpoints);
    let mut caps = BASE.to_vec();
    caps.push(Capability::NetworkEndpoints);
    let e = h
        .core
        .create_session(
            spec(eps, &caps, &["other.lan"], &h),
            SessionToken::from_bytes([3; 32]),
        )
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidArgument);
    // Endpoints without the capability.
    let e = h
        .core
        .create_session(
            spec(eps, &BASE, &["sql.lan"], &h),
            SessionToken::from_bytes([3; 32]),
        )
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidArgument);
    // The capability without endpoints grants nothing.
    let (g, _) = h
        .core
        .create_session(spec(eps, &caps, &[], &h), SessionToken::from_bytes([3; 32]))
        .unwrap();
    assert!(!g.capabilities.contains(&Capability::NetworkEndpoints));
    assert!(
        h.audit_events()
            .iter()
            .any(|e| e.action == AuditAction::SessionCreate && e.outcome == AuditOutcome::Denied)
    );
}

#[test]
fn allowed_endpoints_follow_the_sessions_of_a_running_vm() {
    let mut h = Harness::with_config(roomy());
    let eps = vm_with(&mut h, "eps", NetworkProfile::Endpoints);
    let run = h.start(eps);
    assert_eq!(
        h.core.allowed_endpoints(eps, false),
        names(&["sql.lan", "wcfhost"])
    );
    let mut caps = BASE.to_vec();
    caps.push(Capability::NetworkEndpoints);
    let (g, _) = h
        .core
        .create_session(
            spec(eps, &caps, &["wcfhost"], &h),
            SessionToken::from_bytes([4; 32]),
        )
        .unwrap();
    assert_eq!(h.core.allowed_endpoints(eps, true), names(&["wcfhost"]));
    h.core
        .revoke_session(g.session_id, TerminationCause::Revoked)
        .unwrap();
    // After the session: nothing until the next start (no widening).
    assert!(h.core.allowed_endpoints(eps, true).is_empty());
    // The audit carries network details.
    h.core.record_network(
        eps,
        Some(run),
        None,
        AuditAction::NetworkConnection,
        AuditOutcome::Succeeded,
        NetworkAudit {
            endpoint: Some("wcfhost".into()),
            destination: Some("10.0.2.102:8733".into()),
            target: Some("127.0.0.1:8733".into()),
            bytes_from_guest: Some(10),
            bytes_to_guest: Some(20),
            reason: Some("closed".into()),
            ..NetworkAudit::default()
        },
    );
    let ev = h.audit_events();
    let last = ev.last().unwrap();
    assert_eq!(last.action, AuditAction::NetworkConnection);
    assert_eq!(last.actor, Actor::System);
    assert_eq!(last.network.as_ref().unwrap().bytes_to_guest, Some(20));
}
