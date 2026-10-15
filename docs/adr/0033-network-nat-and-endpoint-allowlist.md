# ADR-0033 — Guest network: NAT in manual use, endpoint allowlist for agent sessions

- Status: Accepted (2026-10-15): direction set; evidence pending
- Date: 2026-10-15
- Owner: project owner (decision); implementation recorded below

## Context

The first real use case is a WPF client (.NET 4.8) in a Windows guest that must reach a WCF server and SQL Server running on the host or on the LAN, driven by an agent. ADR-0004 gives the MVP no NIC; R1 (filtered Internet) is a separate milestone and names the access to host development servers as a future "Custom Network" exception. The project owner decided: full NAT when the user drives the VM, and an allowlist of approved endpoints when an agent session runs.

## Decision

Three network modes per VM, chosen in the VM settings; **Offline stays the default** (ADR-0004).

1. **Offline** — no NIC (unchanged).
2. **NAT (manual use only)** — QEMU user-mode network (`-nic user`, inbox-driver NIC of the guest profile: e1000 or rtl8139 for old guests), like VMware NAT: the guest reaches the host, the LAN and the Internet. Not available to agent sessions: before a session starts on such a VM the supervisor switches it to Endpoint allowlist or Offline (VM stopped) or refuses the session; never a hidden change while running.
3. **Endpoint allowlist** — `-nic user,restrict=on` with only `guestfwd` rules: each rule maps a virtual address and TCP port in the guest network (for example `10.0.2.101:1433`) to one **host:port approved by the user** in the app (for example `sqlserver.lan:1433`, `127.0.0.1:8733` for a WCF ServiceHost). No routing, no DNS server for arbitrary names, no UDP, no other destination. The guest addresses the endpoints by their virtual address (LightVM Tools can add `hosts` entries with the names the user chose). The host side connection is made by QEMU to the approved destination only; the destination is resolved and checked at start (no loopback/LAN restriction beyond the user's explicit list, which is the point of the exception).

**Agent sessions.** The allowlist is a session capability: the grant lists the endpoints the session may use (a subset of the VM's approved ones), approved in the app like the other capabilities, shown in the session and recorded in the report. Without it the session runs Offline. This is a declared exception to section 7.2 item 2 and the "Custom Network" of R1, recorded in the roadmap.

**What it does not claim.** An allowed endpoint can receive any data present in the guest (exfiltration to that endpoint is possible by design). It is not R1 filtered Internet: no general egress, no content inspection.

## Required evidence

- Allowlist: from the guest only the listed virtual endpoints answer; host services not listed, LAN, Internet, IPv4/IPv6, DNS, UDP unreachable (section 7.4 network row repeated with the allowlist on); a grant without the capability gives an Offline guest.
- NAT: refused for agent sessions; manual use reaches LAN/Internet.
- Real case: Windows guest client reaching a WCF service and SQL Server on the host through the allowlist. **Not verified.**

## References

Roadmap, sections 7.2, 7.4, 12 (R1, R6); ADR-0004 (amended: Offline remains the default), ADR-0031, ADR-0034.
