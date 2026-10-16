//! `guest-net`: the supervisor-owned guest network of the endpoint allowlist
//! (ADR-0033).
//!
//! QEMU attaches the guest NIC with `-netdev stream` to a socket owned by the
//! supervisor; this crate is the other end. It is a small user-mode network
//! (the gvproxy model), not a router:
//!
//! - **addresses**: guest `10.0.2.15/24`, gateway `10.0.2.2` (no default
//!   route is offered), DNS `10.0.2.3`, one virtual address per approved
//!   endpoint in `10.0.2.0/24`;
//! - **DHCP** ([`dhcp`]): a fixed lease for the guest with mask, DNS server
//!   and search domain, never a router;
//! - **DNS** ([`dns`]): only the names of the endpoints allowed at that moment
//!   resolve, to their virtual addresses; every other name is `NXDOMAIN`; no
//!   query is forwarded anywhere;
//! - **ARP** for the gateway, the DNS server and the virtual addresses; ICMP
//!   echo to them is answered by the stack itself;
//! - **TCP**: the guest's connections to an allowed `virtual address:port`
//!   are terminated here (smoltcp) and each one opens **its own** host
//!   connection to the approved `host:port` (resolved by the caller before
//!   the start). A connection to anything else is refused (RST) or dropped;
//! - **everything else is dropped**: other IPv4 destinations, every UDP
//!   datagram except DHCP and DNS to `10.0.2.3`, IPv6, other protocols.
//!
//! The allowed set changes at run time ([`NetStack::set_allowed`]):
//! connections to an endpoint that leaves the set are reset at once. Limits:
//! concurrent connections and bytes per second ([`NetConfig`]). Every
//! proxied connection and every refused attempt is reported as a
//! [`NetRecord`] (endpoint, addresses, byte counts, outcome; never content).

pub mod dhcp;
pub mod dns;
mod frame;
mod stack;
pub mod transport;

use std::collections::BTreeSet;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::{Duration, SystemTime};

pub use stack::{NetStack, StackError};

/// Network of the guest.
pub const SUBNET: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 0);
/// Prefix length of [`SUBNET`].
pub const PREFIX_LEN: u8 = 24;
/// Address leased to the guest.
pub const GUEST_IP: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 15);
/// Address of the stack (DHCP server, ARP, ICMP); never offered as a router.
pub const GATEWAY_IP: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 2);
/// DNS server of the guest (answered by the stack).
pub const DNS_IP: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 3);
/// MAC address of the stack.
pub const GATEWAY_MAC: [u8; 6] = [0x52, 0x55, 0x0a, 0x00, 0x02, 0x02];
/// MAC address QEMU gives the guest NIC (`mac=` of the device).
pub const GUEST_MAC: [u8; 6] = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
/// Search domain offered by DHCP: a single-label endpoint name `sql` also
/// resolves as `sql.lightvm.internal` (`.internal` is reserved for private
/// use).
pub const DOMAIN_SUFFIX: &str = "lightvm.internal";
/// First virtual address the settings assign to an endpoint.
pub const FIRST_VIRTUAL_IP: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 100);
/// Last virtual address the settings may assign to an endpoint.
pub const LAST_VIRTUAL_IP: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 254);
/// DHCP lease, in seconds.
pub const LEASE_SECONDS: u32 = 86_400;
/// Default limit of concurrent proxied connections.
pub const DEFAULT_MAX_CONNECTIONS: usize = 64;
/// Default host connection timeout.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Refused attempts reported individually per stack (later ones are only
/// counted in [`NetStats::refused`]).
pub const MAX_DENIED_RECORDS: usize = 256;
/// Records kept until [`NetStack::take_records`] (older ones are counted in
/// [`NetStats::records_dropped`]).
pub const MAX_PENDING_RECORDS: usize = 10_000;

/// `true` if `ip` may be the virtual address of an endpoint: inside
/// [`FIRST_VIRTUAL_IP`]..=[`LAST_VIRTUAL_IP`] (so never the guest, the
/// gateway, the DNS server or the broadcast address).
#[must_use]
pub fn is_virtual_ip(ip: Ipv4Addr) -> bool {
    // .99 is reserved for the authenticated Tools channel. Public VM
    // endpoints still start at .100 (lightvm-proto validation).
    ip == Ipv4Addr::new(10, 0, 2, 99) || (FIRST_VIRTUAL_IP..=LAST_VIRTUAL_IP).contains(&ip)
}

/// An approved endpoint with its host address already resolved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EndpointSpec {
    /// Name chosen by the user (DNS name inside the guest; compared without
    /// regard to case).
    pub name: String,
    /// Virtual address in the guest network.
    pub virtual_ip: Ipv4Addr,
    /// TCP port, the same in the guest and on the host.
    pub port: u16,
    /// Host address the connections are opened to.
    pub target: SocketAddr,
}

impl EndpointSpec {
    /// Virtual socket address in the guest network.
    #[must_use]
    pub fn virtual_addr(&self) -> SocketAddrV4 {
        SocketAddrV4::new(self.virtual_ip, self.port)
    }
}

/// Configuration of a stack.
#[derive(Clone, Debug)]
pub struct NetConfig {
    /// Tools beside NAT must not replace the guest's Internet DNS server.
    pub advertise_dns: bool,
    /// Endpoints of the VM (resolved). The allowed subset is given
    /// separately and can change at run time.
    pub endpoints: Vec<EndpointSpec>,
    /// Concurrent proxied connections; a new one beyond the limit is
    /// refused (RST).
    pub max_connections: usize,
    /// Bytes per second over all connections, both directions together
    /// (`None` = no limit).
    pub max_bytes_per_second: Option<u64>,
    /// Timeout of the host connection.
    pub connect_timeout: Duration,
}

impl NetConfig {
    /// Configuration with the default limits.
    #[must_use]
    pub fn new(endpoints: Vec<EndpointSpec>) -> Self {
        Self {
            advertise_dns: true,
            endpoints,
            max_connections: DEFAULT_MAX_CONNECTIONS,
            max_bytes_per_second: None,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
        }
    }

    /// Checks names, addresses and ports: names unique (case-insensitive),
    /// virtual addresses in the allowed range, `virtual address:port` unique.
    pub fn validate(&self) -> Result<(), String> {
        let mut names = BTreeSet::new();
        let mut addrs = BTreeSet::new();
        for e in &self.endpoints {
            dns::check_name(&e.name)?;
            if !names.insert(dns::normalize(&e.name)) {
                return Err(format!("endpoint name `{}` repeated", e.name));
            }
            if !is_virtual_ip(e.virtual_ip) {
                return Err(format!(
                    "virtual address {} outside {FIRST_VIRTUAL_IP}..={LAST_VIRTUAL_IP}",
                    e.virtual_ip
                ));
            }
            if e.port == 0 {
                return Err("port 0".into());
            }
            if !addrs.insert(e.virtual_addr()) {
                return Err(format!("virtual endpoint {} repeated", e.virtual_addr()));
            }
        }
        if self.max_connections == 0 {
            return Err("max_connections is 0".into());
        }
        Ok(())
    }
}

/// Why a proxied connection ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ConnOutcome {
    /// Closed by either side after the exchange.
    Closed,
    /// Reset by the guest.
    ResetByGuest,
    /// The host endpoint refused the connection, was unreachable or did not
    /// answer within the timeout (the guest connection is reset).
    TargetUnreachable,
    /// The host connection failed after it was established (the guest
    /// connection is reset).
    TargetError,
    /// The endpoint left the allowed set (session ended, revoked, expired,
    /// Emergency Stop): the guest connection is reset.
    Revoked,
    /// The stack stopped (end of the run).
    Stopped,
}

impl ConnOutcome {
    /// Stable name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Closed => "closed",
            Self::ResetByGuest => "reset_by_guest",
            Self::TargetUnreachable => "target_unreachable",
            Self::TargetError => "target_error",
            Self::Revoked => "revoked",
            Self::Stopped => "stopped",
        }
    }
}

/// Why a guest connection attempt was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DenyReason {
    /// TCP to a virtual endpoint that is configured but not allowed now.
    NotAllowed,
    /// TCP to an address or port that is no endpoint (the gateway, the DNS
    /// server, another port of a virtual address, or an address outside the
    /// guest network).
    NotAnEndpoint,
    /// TCP to an allowed endpoint beyond [`NetConfig::max_connections`].
    ConnectionLimit,
}

impl DenyReason {
    /// Stable name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotAllowed => "not_allowed",
            Self::NotAnEndpoint => "not_an_endpoint",
            Self::ConnectionLimit => "connection_limit",
        }
    }
}

/// A proxied connection, reported when it ends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnRecord {
    /// Endpoint name.
    pub endpoint: String,
    /// Virtual address the guest connected to.
    pub virtual_addr: SocketAddrV4,
    /// Host address the connection was opened to.
    pub target: SocketAddr,
    /// Source port of the guest.
    pub guest_port: u16,
    /// When the guest's SYN arrived.
    pub opened_at: SystemTime,
    /// When the connection ended.
    pub closed_at: SystemTime,
    /// Bytes the guest sent to the endpoint.
    pub bytes_from_guest: u64,
    /// Bytes the endpoint sent to the guest.
    pub bytes_to_guest: u64,
    /// How it ended.
    pub outcome: ConnOutcome,
}

/// A refused guest connection attempt (TCP SYN).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeniedAttempt {
    /// When.
    pub at: SystemTime,
    /// Destination the guest tried.
    pub destination: SocketAddrV4,
    /// Why it was refused.
    pub reason: DenyReason,
}

/// Something to audit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NetRecord {
    /// A proxied connection ended.
    Connection(ConnRecord),
    /// A connection attempt was refused.
    Denied(DeniedAttempt),
}

/// Counters of a stack.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NetStats {
    /// Frames received from the guest.
    pub frames_in: u64,
    /// Frames sent to the guest.
    pub frames_out: u64,
    /// DHCP replies sent.
    pub dhcp_replies: u64,
    /// DNS queries answered with an address.
    pub dns_answered: u64,
    /// DNS queries answered without an address (unknown or not allowed name,
    /// other record types).
    pub dns_refused: u64,
    /// Proxied connections opened.
    pub connections: u64,
    /// Proxied connections open now.
    pub open_connections: u64,
    /// TCP connection attempts refused or dropped.
    pub refused: u64,
    /// UDP datagrams dropped (everything except DHCP and DNS to the stack).
    pub udp_dropped: u64,
    /// IPv6 frames dropped.
    pub ipv6_dropped: u64,
    /// Other frames dropped (other protocols, other destinations).
    pub other_dropped: u64,
    /// Bytes from the guest to the endpoints.
    pub bytes_from_guest: u64,
    /// Bytes from the endpoints to the guest.
    pub bytes_to_guest: u64,
    /// Records lost because nobody took them in time.
    pub records_dropped: u64,
}
