# R1-01 — Filtered Internet, first increment (ADR-0037)

Date: 2026-10-16. Host: Windows 11 Pro (build 26300), MSVC toolchain,
build output on `E:\OpenVM-tests` at the owner's request.

This file records evidence only; it does not change the R1 release
conditions of `docs/ROADMAP.md` section 12.

## 1. What is implemented

- `NetworkProfile::FilteredInternet` in the protocol, schemas, core, service
  and desktop VM settings ("Filtered Internet (experimental)"; the host
  field is hidden because name and host must match).
- Configuration check (`lightvm-proto` `validate_network`): exact DNS names
  with a dot, name equal to host, no IP literal, TCP 80/443, at least one.
- Run start (`lightvm-service` `adapters/network.rs`): the supervisor
  resolves each name with a timeout, ignores IPv6 answers, refuses the
  endpoint if any IPv4 answer fails the policy, pins the first one and marks
  the endpoint public-only in the stack.
- Policy (`guest-net` `egress.rs`): public IPv4 only (special-use blocks
  refused as a whole), only through a default route, no more specific route
  covering the address; routing table read with `GetIpForwardTable2`; any
  failure closes egress; non-Windows hosts refuse everything.
- Stack (`guest-net` `stack.rs`): policy checked before connect, peer
  address checked after connect, and every second while the connection is
  open; a failure resets the guest connection.

## 2. Verified in simulation/test

```text
cargo test --locked -p guest-net -p lightvm-proto
cargo clippy --workspace --all-targets --locked --features lightvm-service/supervisor -- -D warnings
cargo test --workspace --locked --features lightvm-service/supervisor
```

All passed on 2026-10-16:

- `egress::tests::private_special_ipv6_ports_and_public_lan_are_closed`:
  RFC 1918, CGNAT, loopback, link-local (including `169.254.169.254`),
  benchmark, documentation, multicast, broadcast, IPv6 and IPv4-mapped IPv6
  are refused; a public address covered by a `/24` LAN route is refused;
  port 22 is refused; no default route or a `0.0.0.0/1` route refuses.
- `egress::tests::actual_windows_route_inventory_is_bounded_and_has_valid_prefixes`:
  the real routing table of this host is read and bounded.
- `stack.rs` `public_only_endpoint_never_reaches_a_host_target`: an endpoint
  marked public-only whose pinned target is loopback (the DNS rebinding /
  host case) is reset with `TargetUnreachable` and the host server never
  accepts a connection; an ordinary endpoint of the same stack still works.
- `roundtrip.rs` `network_endpoint_validation`: valid web names accepted;
  other ports, name different from host, IP literals, `localhost` and LAN
  names refused; an empty list refused.

## 3. Not verified

No real guest has used this mode yet. Still required by ROADMAP R1:
a real guest browsing an allowed HTTPS site; other names, host, LAN,
IPv6 and UDP/DNS bypass refused from inside the guest; DNS rebinding with a
real resolver; host VPN (split and full tunnel) and a system proxy;
revocation of a persistent connection; gateway (supervisor) crash closing
connectivity; macOS/Linux host route checks.
