# ADR-0004 — Offline MVP profile with no NIC and no host shares

- Status: Accepted; Offline remains the default, NAT (manual use) and endpoint allowlist (grantable) added by ADR-0033
- Date: 2026-10-09
- Owner: to be assigned

## Context

NAT or a proxy configured in the guest do not enforce a network policy; the `restrict=on` option is not enough (roadmap, sections 3 and 12 R1). Shared folders, clipboard and passthrough open paths to the host. Filtered networking has a dedicated milestone (R1).

## Decision

- No guest NIC in the MVP, with explicit exclusion of QEMU's default network devices; no forwarding.
- No shared folders, clipboard, USB passthrough, physical disk, guest agent or host shell.
- Allowlisted virtual HID devices (for example an absolute pointer) permitted.
- The wizard exposes only the "Offline" network (section 10 F2).
- The only way files enter the guest is the read-only data medium (ADR-0008).
- The offline profile is the default from the first generation of QEMU commands (F1).

## Alternatives considered

- User-mode NAT with `restrict=on`: not a verified filter (section 12 R1).
- Proxy configured in the guest: can be bypassed by the guest (section 3).
- Filtered networking already in the MVP: deferred to R1 (section 4).

## Consequences

- The first demonstration test uses an application already present in the guest or provided via installation media or the data medium (section 4); downloading Firefox is not an acceptance test.
- The generated QEMU configuration must be inspected and compared with the observed devices.

## Required evidence

- "No unexpected NIC" (F1 exit criterion).
- "Network" negative test from section 7.4: guest to host, LAN, Internet, IPv6.
- First observations on devices in F0: `docs/verification/F0-02-qemu-whpx-qmp.md` §5 (verified: without `-nodefaults` q35 gets `e1000e` on a `user` network; with `-nodefaults -nic none` no NIC; `query-rx-filter` is not a reliable indicator, `info network` and `query-pci` are used instead).

## References

Roadmap, sections 3, 4, 7.2 (points 2 and 3), 7.4, 10 (F1, F2), 12 (R1).
