# ADR-0033 — Guest network: NAT in manual use, endpoint allowlist for agent sessions

- Status: Accepted (2026-10-15); implemented, evidence in `docs/verification/R6-06-guest-network.md`; the real case (Windows guest, WCF and SQL Server) not verified
- Date: 2026-10-15
- Owner: project owner (decision); implementation recorded below

## Context

The first real use case is a WPF client (.NET 4.8) in a Windows guest that must reach a WCF server and SQL Server running on the host or on the LAN, driven by an agent. ADR-0004 gives the MVP no NIC; R1 (filtered Internet) is a separate milestone and names the access to host development servers as a future "Custom Network" exception. The project owner decided: full NAT when the user drives the VM, and an allowlist of approved endpoints when an agent session runs.

## Decision

Three network modes per VM, chosen in the VM settings (VM stopped); **Offline stays the default** (ADR-0004).

1. **Offline** — no NIC (unchanged: `-nic none`, post-start check of `info network` and `query-pci`).
2. **NAT (manual use only)** — QEMU user-mode network (`-netdev user`, no `hostfwd`, no `guestfwd`, no SMB share) and the inbox-driver NIC of the guest profile, like VMware NAT: the guest reaches the host, the LAN and the Internet. Never given to a run under an agent session: a run started while a session targets the VM gets the session's endpoint allowlist or no NIC, and a session is refused while the VM runs with NAT (the user stops it first). Never a hidden change while running.
3. **Endpoint allowlist** — the guest NIC is attached with `-netdev stream` to a user-mode network owned by the supervisor (the gvproxy model). Each endpoint, approved by the user in the VM settings, is a name and a virtual address with one TCP port inside the guest network (for example `sql.lan`, `10.0.2.100:1433`) mapped to one host service (`sqlserver.corp.example:1433`, `127.0.0.1:8733`). Each guest connection to an allowed endpoint opens its own host connection to that service; everything else is refused or dropped: other addresses and ports, the host and LAN, the Internet, UDP except the stack's own DHCP and DNS, IPv6. The DNS answers only the names of the endpoints allowed at that moment.

**Agent sessions.** The allowlist is the session capability `network.endpoints` (ADR-0034): the grant lists the endpoints the session may use (a subset of the VM's), approved in the app like the other capabilities, shown in the session and recorded in the report. Without it the session's VM runs without network. This is a declared exception to section 7.2 item 2 and the "Custom Network" of R1, recorded in the roadmap.

**What it does not claim.** An allowed endpoint can receive any data present in the guest (exfiltration to that endpoint is possible by design). It is not R1 filtered Internet: no general egress, no content inspection.

## Implementation

**Why not `guestfwd`.** The first design used `-nic user,restrict=on` with `guestfwd` rules. The R6-04 spike (`docs/verification/R6-04-integration-channel-usb.md` §1) showed that `restrict=on` isolates correctly but that `guestfwd` to a TCP target is one host connection opened when QEMU starts and shared by every guest connection (concurrent connections receive each other's replies, no open/close per connection, QEMU refuses to start if the target is down, a service that closes idle connections breaks the rule). It cannot carry SQL Server, WCF or HTTP clients. `-netdev stream` is available in the bundled Windows build. The allowlist was therefore built as a supervisor-owned network.

**Supervisor network (`crates/guest-net`).** smoltcp 0.14.0 (0BSD; Ethernet, IPv4, TCP only) runs behind a frame filter that decides what reaches it:

- addresses: guest `10.0.2.15/24`, stack `10.0.2.2`, DNS `10.0.2.3`, endpoints in `10.0.2.100..=10.0.2.254` (assigned by the settings panel);
- DHCP answered by the crate: fixed lease, mask, DNS server and search domain `lightvm.internal`, **no router**;
- DNS answered by the crate: `A` for the names of the endpoints allowed now (`name` and `name.lightvm.internal`), empty `NOERROR` for other types of a known name, `NXDOMAIN` otherwise; nothing forwarded;
- ARP and ICMP echo only for the stack's addresses; TCP to an allowed endpoint: a listener is created for each SYN, the host connection opened per guest connection (connect timeout 10 s), data moved with backpressure both ways, half-close carried; TCP to anything else: RST (stack addresses) or dropped; UDP other than DHCP and DNS, IPv6, other protocols: dropped;
- limits: 64 concurrent connections per VM (beyond: RST), optional bytes per second (token bucket; not set by the service), guest connections that stop acknowledging for 120 s are aborted;
- the allowed set changes at run time: connections to endpoints that leave it are reset at once, their names stop resolving;
- records: one per proxied connection (endpoint, virtual and host address, guest port, bytes each way, outcome) and per refused attempt (the first 256 of a run, later ones counted); never content. A diagnostic frame trace (headers only) is written when `LIGHTVM_NET_TRACE` names a file.

**QEMU command (`qemu-backend`).** `GuestNetwork` none / NAT / supervisor; NIC of the profile: `e1000` (Linux, Windows Vista and later), `rtl8139` (Windows XP), none for Windows 11 ARM64 (offline only); fixed MAC `52:54:00:12:34:56`; the supervisor network is `-netdev stream,server=on,addr.type=unix,addr.path=<private dir>/net.sock` (the run's private directory, user-only ACL). The post-start check requires exactly one NIC on the expected back end (`type=user` or `type=stream`) and nothing else; the command audit of the tests accepts only these forms.

**Core and service.** The network of a run is decided by the core when the start is forwarded (`StartPlan` carries it): the VM's mode in manual use; with a live agent session on the VM, the endpoint allowlist if a session was granted endpoints and the VM has endpoints, otherwise none. A session is refused (`state_conflict`, `nat_running`) while the VM runs with NAT; granted endpoints must be endpoints of the VM; `network.endpoints` without endpoints is dropped. The backend resolves each endpoint's host when the run starts (5 s per name; an unresolvable endpoint refuses connections), connects to `net.sock` and starts the stack with nothing allowed. After every publication (every request and at least every second) the engine sets the allowed endpoints: every endpoint of the VM in manual use; the union of the granted ones while sessions target the VM; **none for the rest of the run once a session targeted it** (a session ending never widens the network). Revocation, expiry and the end of a session reset the connections within that publication; an Emergency Stop or the end of the run stops the stack. Network set-up (mode, each endpoint and the address it resolved to), connections, refusals and changes of the allowed set are audit events (`network_setup`, `network_connection`, `network_denied`, `network_policy`, actor `system`, with the session targeting the VM) and events of its session report.

**Settings, approval, MCP.** VM settings: mode menu and endpoint list (name, host, port; virtual address assigned; validated: DNS name syntax, no wildcard, port 1..=65535, unique names and virtual endpoints, at most 32), saved in the catalog configuration (`config_json`; no SQL schema change: older rows read with no endpoints). Approval form: the endpoints of the chosen VM, never ticked by default. MCP: `session_request` takes an optional `network_endpoints` hint; `vm_status`/`vm_list` list the granted endpoints (name, virtual address, port).

## Required evidence

- Allowlist: from the guest only the listed virtual endpoints answer; host services not listed, LAN, Internet, IPv4/IPv6, DNS, UDP unreachable (section 7.4 network row repeated with the allowlist on); a grant without the capability gives an Offline guest. — **verified** with the Ubuntu live guest (R6-06 §3) and the IPC tests (R6-06 §2).
- NAT: refused for agent sessions; manual use reaches LAN/Internet. — refusal verified (IPC tests); manual use reaching the host verified with the Ubuntu live guest (R6-06 §4).
- Real case: Windows guest client reaching a WCF service and SQL Server on the host through the allowlist. **Not verified.**

## References

Roadmap, sections 7.2, 7.4, 12 (R1, R6); ADR-0004 (amended: Offline remains the default), ADR-0023 (smoltcp), ADR-0027 (NIC of the profiles), ADR-0031, ADR-0034; `docs/verification/R6-04-integration-channel-usb.md` §1, `docs/verification/R6-06-guest-network.md`.
