# ADR-0037 — Filtered Internet, first increment: pinned DNS names, public IPv4 TCP 80/443 through the supervisor stack

- Status: Accepted (2026-10-16): direction set and implemented; real-guest evidence pending
- Date: 2026-10-16
- Owner: project owner (decision); implementation recorded below

## Context

ROADMAP 12 (R1) asks for a single host-side mechanism: a private virtual
network towards a dedicated gateway, with no alternative path, closed by
default, limited in its first increment to a destination/port policy
(no HTTPS content inspection, no transparent TLS interception). It must
block host and LAN addresses even when public, loopback, link-local, private
ranges, IPv6 and UDP, keep DNS under gateway control, and survive VPN, system
proxy, redirect, DNS rebinding and persistent-connection cases. QEMU
`restrict=on` alone does not qualify.

ADR-0033 already gives each run a supervisor-owned stack (`crates/guest-net`):
the NIC is on `-netdev stream` to the supervisor, the guest has no router,
DNS answers only allowed names with virtual addresses, and each guest TCP
connection is a separate host connection to a pinned target. On 2026-10-16
the owner asked to proceed with the open items, filtered Internet included.

## Decision

Add the network mode `FilteredInternet` on top of the ADR-0033 stack:

- **Destinations are exact DNS names**, listed per VM (endpoint name equals
  host name, no wildcard, no IP literal, at least one dot), on **TCP 80 or
  443**. They are resolved by the supervisor once at run start and pinned
  for the run; AAAA answers are ignored, and every A answer must pass the
  policy, otherwise the endpoint is not offered (recorded in the audit).
- **Policy**: the address must be public IPv4 (a conservative subset of the
  IANA globally reachable space; special-use blocks are refused as a whole)
  and the host routing table must route it only through a default route. Any
  more specific route covering it (LAN, host, split VPN, full-tunnel
  `0.0.0.0/1` style routes) refuses it. A failure to read the routing table
  closes egress.
- **Every connection** is checked before connecting and after connecting
  (the peer must be exactly the pinned address), and while open it is
  rechecked every second: a route change closes it. Revocation and stack
  stop reset connections as in ADR-0033.
- The guest still has no router and no other DNS; UDP, ICMP and IPv6 have no
  path. The system HTTP proxy is not consulted. Agent sessions get this mode
  only through the `network.endpoints` grant (ADR-0034), never NAT.
- Windows hosts only for now (the route check uses `GetIpForwardTable2`);
  on other hosts every connection is refused.

## Alternatives considered

- **HTTP CONNECT proxy in the guest**: needs guest configuration the guest
  can remove (ROADMAP R1 excludes it) and does not cover non-proxy-aware
  clients.
- **NAT with `restrict=on` and host firewall rules**: rejected by the
  roadmap and dependent on host firewall state.
- **Wildcard domains / following redirects**: needs per-connection DNS or
  SNI inspection; out of the first increment. Redirects to other names fail
  unless those names are listed too.

## Consequences

- An allowed destination can receive any guest data (no exfiltration
  guarantee), as stated in ROADMAP R1 and ADR-0033.
- DNS pinning means a CDN address change during a long run is not
  followed; the user restarts the VM.
- The per-connection route poll costs one routing-table read per second per
  open connection (at most the stack's connection limit).
- R1 stays incomplete until the real-guest evidence below exists.

## Required evidence

`docs/verification/R1-01-filtered-internet.md`: unit/simulation evidence
(policy, configuration, stack refusal) and, still open, a real guest
browsing an allowed HTTPS site; refusal of other names, host, LAN, IPv6,
UDP/DNS bypass; DNS rebinding; VPN and system proxy on the host; revocation
of persistent connections; gateway crash closing connectivity.

## References

ROADMAP 7.2, 7.4, 12 (R1, R6); ADR-0004, ADR-0033, ADR-0034.
