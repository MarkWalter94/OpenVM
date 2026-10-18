//! Supervisor network of the endpoint allowlist (ADR-0033): resolution of
//! the approved endpoints at the start of a run, connection to QEMU's
//! `-netdev stream` socket in the run's private directory, the
//! [`guest_net::NetStack`] of the run, and the mapping of its records to
//! audit events.
//!
//! The stack starts with nothing allowed; the engine sets the allowed
//! endpoints right after the start and whenever the sessions of the VM
//! change ([`crate::engine::VmEngine::sync_network`]).

use std::collections::BTreeSet;
use std::net::{SocketAddr, ToSocketAddrs};
use std::path::Path;
use std::time::{Duration, Instant};

use guest_net::{ConnOutcome, EndpointSpec, NetConfig, NetRecord, NetStack};
use lightvm_proto::{
    AuditAction, AuditOutcome, NetworkAudit, NetworkEndpoint, NetworkProfile, RunId, VmId, VmRecord,
};

use crate::engine::NetworkRecord;

/// Time allowed to resolve one endpoint host name at the start of a run.
pub const RESOLVE_TIMEOUT: Duration = Duration::from_secs(5);
/// Time allowed to connect to QEMU's netdev socket after QEMU started.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Concurrent proxied connections per VM.
pub const MAX_CONNECTIONS: usize = guest_net::DEFAULT_MAX_CONNECTIONS;

/// Supervisor network of a run.
#[derive(Debug)]
pub(crate) struct NetRun {
    /// Run.
    pub(crate) run_id: RunId,
    /// Stack (absent if it could not be attached: the guest NIC has no
    /// peer and reaches nothing).
    pub(crate) stack: Option<NetStack>,
    /// Allowed set last applied (`None` = never set).
    pub(crate) applied: Option<BTreeSet<String>>,
    /// Reserved, authenticated channels; never grantable as public endpoints.
    pub(crate) internal: BTreeSet<String>,
}

impl NetRun {
    /// Applies `allowed` if it differs from the last one applied.
    pub(crate) fn set_allowed(&mut self, allowed: &BTreeSet<String>) -> bool {
        let Some(s) = &self.stack else {
            return false;
        };
        if self.applied.as_ref() == Some(allowed) {
            return false;
        }
        s.set_allowed(allowed.iter().cloned().chain(self.internal.iter().cloned()));
        self.applied = Some(allowed.clone());
        true
    }

    /// Stops the stack and returns its last records.
    pub(crate) fn stop(&mut self, vm_id: VmId) -> Vec<NetworkRecord> {
        let Some(mut s) = self.stack.take() else {
            return Vec::new();
        };
        s.stop();
        records(vm_id, self.run_id, s.take_records())
    }

    /// Records produced since the last call.
    pub(crate) fn take(&self, vm_id: VmId) -> Vec<NetworkRecord> {
        self.stack
            .as_ref()
            .map(|s| records(vm_id, self.run_id, s.take_records()))
            .unwrap_or_default()
    }
}

/// Name of a mode for the audit.
pub(crate) fn mode_name(m: NetworkProfile) -> &'static str {
    match m {
        NetworkProfile::Offline => "offline",
        NetworkProfile::Nat => "nat",
        NetworkProfile::Endpoints => "endpoints",
        NetworkProfile::FilteredInternet => "filtered_internet",
    }
}

/// Resolves `host:port` within [`RESOLVE_TIMEOUT`] (the system resolver
/// runs on its own thread; a late answer is dropped).
pub(crate) fn resolve(host: &str, port: u16) -> Result<SocketAddr, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    let h = host.to_owned();
    std::thread::Builder::new()
        .name("lightvm-resolve".into())
        .spawn(move || {
            let r = (h.as_str(), port)
                .to_socket_addrs()
                .map_err(|e| e.to_string())
                .and_then(|mut a| {
                    // IPv4 first: the usual LAN and loopback services.
                    let all: Vec<SocketAddr> = a.by_ref().collect();
                    all.iter()
                        .find(|s| s.is_ipv4())
                        .or_else(|| all.first())
                        .copied()
                        .ok_or_else(|| "no address".to_owned())
                });
            let _ = tx.send(r);
        })
        .map_err(|e| e.to_string())?;
    rx.recv_timeout(RESOLVE_TIMEOUT)
        .unwrap_or_else(|_| Err("timed out".into()))
}

/// Pin the DNS answer for the run. Every returned IPv4 address must pass
/// the public-route policy; AAAA answers are never used for this mode.
fn public_address(addresses: &[SocketAddr]) -> Result<SocketAddr, String> {
    public_address_checked(addresses, |address| {
        guest_net::egress::validate_target(address).map_err(str::to_owned)
    })
}

fn public_address_checked(
    addresses: &[SocketAddr],
    validate: impl Fn(SocketAddr) -> Result<(), String>,
) -> Result<SocketAddr, String> {
    if addresses.len() > 64 {
        return Err("DNS answer exceeds limit".into());
    }
    let mut selected = None;
    for &address in addresses.iter().filter(|a| a.is_ipv4()) {
        validate(address)?;
        selected.get_or_insert(address);
    }
    selected.ok_or_else(|| "no approved public IPv4 address".into())
}

fn resolve_public(host: &str, port: u16) -> Result<SocketAddr, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    let host = host.to_owned();
    std::thread::Builder::new()
        .name("lightvm-public-dns".into())
        .spawn(move || {
            let result = (host.as_str(), port)
                .to_socket_addrs()
                .map_err(|e| e.to_string())
                .and_then(|a| public_address(&a.take(65).collect::<Vec<_>>()));
            let _ = tx.send(result);
        })
        .map_err(|e| e.to_string())?;
    rx.recv_timeout(RESOLVE_TIMEOUT)
        .unwrap_or_else(|_| Err("DNS timed out".into()))
}

fn setup_record(
    vm_id: VmId,
    run_id: RunId,
    outcome: AuditOutcome,
    d: NetworkAudit,
) -> NetworkRecord {
    NetworkRecord {
        vm_id,
        run_id,
        action: AuditAction::NetworkSetup,
        outcome,
        detail: d,
    }
}

/// Network set-up of a run whose plan has the network mode of `vm`. For
/// the endpoint allowlist: resolves the endpoints, connects to `socket` and
/// starts the stack (nothing allowed yet). The records say what was set up
/// (one per endpoint, with the address it resolved to).
pub(crate) fn setup(
    vm: &VmRecord,
    run_id: RunId,
    socket: Option<&Path>,
    tools: Option<&crate::tools::ToolsRun>,
) -> (Option<NetRun>, Vec<NetworkRecord>) {
    let mode = vm.config.network;
    let mut recs = vec![setup_record(
        vm.id,
        run_id,
        AuditOutcome::Succeeded,
        NetworkAudit {
            mode: Some(mode_name(mode).into()),
            ..NetworkAudit::default()
        },
    )];
    if !mode.is_restricted() && tools.is_none() {
        return (None, recs);
    }
    let mut specs = Vec::new();
    for e in vm.config.endpoints.iter().filter(|_| mode.is_restricted()) {
        let r = if mode == NetworkProfile::FilteredInternet {
            resolve_public(&e.host, e.port)
        } else {
            resolve(&e.host, e.port)
        };
        recs.push(endpoint_record(vm.id, run_id, e, &r));
        if let Ok(target) = r {
            specs.push(EndpointSpec {
                name: e.name.clone(),
                virtual_ip: e.virtual_ip,
                port: e.port,
                target,
            });
        }
    }
    let internal = if let Some(t) = tools {
        specs.extend(t.endpoints());
        crate::tools::ToolsRun::allowed_endpoints()
    } else {
        BTreeSet::new()
    };
    let stack = socket
        .ok_or_else(|| "the run has no network socket".to_owned())
        .and_then(|p| connect(p).map_err(|e| format!("network socket: {e}")))
        .and_then(|s| guest_net::transport::attach(s).map_err(|e| e.to_string()))
        .and_then(|link| {
            let mut cfg = NetConfig::new(specs);
            cfg.advertise_dns = mode.is_restricted();
            if mode == NetworkProfile::FilteredInternet {
                cfg.public_endpoints = vm
                    .config
                    .endpoints
                    .iter()
                    .filter(|e| cfg.endpoints.iter().any(|s| s.name == e.name))
                    .map(NetworkEndpoint::key)
                    .collect();
            }
            cfg.max_connections = MAX_CONNECTIONS;
            NetStack::start(cfg, link, internal.clone()).map_err(|e| e.to_string())
        });
    match stack {
        Ok(s) => (
            Some(NetRun {
                run_id,
                stack: Some(s),
                applied: None,
                internal,
            }),
            recs,
        ),
        Err(e) => {
            crate::diag!("lightvm-service: endpoint allowlist not attached: {e}");
            recs.push(setup_record(
                vm.id,
                run_id,
                AuditOutcome::Failed,
                NetworkAudit {
                    mode: Some("endpoints".into()),
                    reason: Some("network stack not attached: the guest reaches nothing".into()),
                    ..NetworkAudit::default()
                },
            ));
            (
                Some(NetRun {
                    run_id,
                    stack: None,
                    applied: None,
                    internal,
                }),
                recs,
            )
        }
    }
}

fn endpoint_record(
    vm_id: VmId,
    run_id: RunId,
    e: &NetworkEndpoint,
    r: &Result<SocketAddr, String>,
) -> NetworkRecord {
    setup_record(
        vm_id,
        run_id,
        if r.is_ok() {
            AuditOutcome::Succeeded
        } else {
            AuditOutcome::Failed
        },
        NetworkAudit {
            mode: Some("endpoints".into()),
            endpoint: Some(e.name.clone()),
            destination: Some(format!("{}:{}", e.virtual_ip, e.port)),
            target: r.as_ref().ok().map(ToString::to_string),
            reason: Some(match r {
                Ok(_) => format!("{} resolved", e.host),
                Err(m) => format!(
                    "{} not resolved ({m}): the endpoint refuses connections",
                    e.host
                ),
            }),
            ..NetworkAudit::default()
        },
    )
}

/// Connects to QEMU's `-netdev stream` server (it listens as soon as QEMU
/// started).
fn connect(path: &Path) -> std::io::Result<socket2::Socket> {
    let t = Instant::now();
    loop {
        let s = socket2::Socket::new(socket2::Domain::UNIX, socket2::Type::STREAM, None)?;
        match s.connect(&socket2::SockAddr::unix(path)?) {
            Ok(()) => return Ok(s),
            Err(e) if t.elapsed() >= CONNECT_TIMEOUT => return Err(e),
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

/// Audit records of the stack's records.
pub(crate) fn records(vm_id: VmId, run_id: RunId, recs: Vec<NetRecord>) -> Vec<NetworkRecord> {
    recs.into_iter()
        .map(|r| match r {
            NetRecord::Connection(c) => NetworkRecord {
                vm_id,
                run_id,
                action: AuditAction::NetworkConnection,
                outcome: match c.outcome {
                    ConnOutcome::TargetUnreachable | ConnOutcome::TargetError => {
                        AuditOutcome::Failed
                    }
                    _ => AuditOutcome::Succeeded,
                },
                detail: NetworkAudit {
                    endpoint: Some(c.endpoint),
                    destination: Some(c.virtual_addr.to_string()),
                    target: Some(c.target.to_string()),
                    bytes_from_guest: Some(c.bytes_from_guest),
                    bytes_to_guest: Some(c.bytes_to_guest),
                    reason: Some(c.outcome.as_str().into()),
                    ..NetworkAudit::default()
                },
            },
            NetRecord::Denied(d) => NetworkRecord {
                vm_id,
                run_id,
                action: AuditAction::NetworkDenied,
                outcome: AuditOutcome::Denied,
                detail: NetworkAudit {
                    destination: Some(d.destination.to_string()),
                    reason: Some(d.reason.as_str().into()),
                    ..NetworkAudit::default()
                },
            },
        })
        .collect()
}

#[cfg(all(test, windows))]
#[path = "network_dns_real.rs"]
mod dns_real;

#[cfg(test)]
mod public_dns_tests {
    use super::*;

    fn select(addresses: &[SocketAddr]) -> Result<SocketAddr, String> {
        public_address_checked(addresses, |address| {
            guest_net::egress::validate_routes(
                address,
                &[guest_net::egress::Route {
                    address: std::net::Ipv4Addr::UNSPECIFIED,
                    prefix: 0,
                }],
            )
            .map_err(str::to_owned)
        })
    }

    #[test]
    fn mixed_public_and_private_dns_answers_are_rejected_in_either_order() {
        let public: SocketAddr = "1.1.1.1:443".parse().unwrap();
        for private in ["127.0.0.1:443", "10.0.0.1:443", "169.254.169.254:443"] {
            let private = private.parse().unwrap();
            assert!(select(&[public, private]).is_err());
            assert!(select(&[private, public]).is_err());
        }
        assert_eq!(select(&[public]).unwrap(), public);
        // A later private answer must not be accepted for a new run.
        assert!(select(&["127.0.0.1:443".parse().unwrap()]).is_err());
    }

    #[test]
    fn ipv6_empty_and_oversized_dns_answers_cannot_create_egress() {
        assert!(select(&[]).is_err());
        assert!(select(&["[2606:4700:4700::1111]:443".parse().unwrap()]).is_err());
        let public = "1.1.1.1:443".parse().unwrap();
        assert!(select(&vec![public; 65]).is_err());
        assert_eq!(
            select(&["[::1]:443".parse().unwrap(), public]).unwrap(),
            public
        );
    }
}
