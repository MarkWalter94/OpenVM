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

## 3. Real Linux guest (stack level, Windows/WHPX)

`guest-net` `tests/real_guest.rs` `filtered_internet_with_a_real_linux_guest`
(`--ignored`; lab Ubuntu 26.04.1 live, QEMU 11.1.0, WHPX, host Internet).
The test resolves `example.com` like the supervisor (every IPv4 answer
must pass the policy; the first is pinned) and starts the stack with it and
with `rebind.example.net` pinned to a loopback service of the host (the DNS
rebinding / host case), both public-only. The guest adds a default route
itself. Passed on 2026-10-16 in 42 s:

```text
== pinned example.com -> 104.20.23.154:443
dns web                                    ANSWER rcode=0 an=1 ip=10.0.2.101
dns other name                             ANSWER rcode=3 an=0 ip=-
https web                                  TLS TLSv1.3 HTTP/1.1 200 OK
http web                                   REFUSED
tcp web:22 (other port)                    REFUSED
dns rebind                                 ANSWER rcode=0 an=1 ip=10.0.2.102
tcp rebind:443 (pinned to loopback)        CONNECTED recv-err=ConnectionResetError
tcp gateway 10.0.2.2:decoy                 REFUSED
tcp host LAN ip:decoy                      TIMEOUT
tcp Internet 1.1.1.1:443                   TIMEOUT
dns udp 8.8.8.8                            TIMEOUT
tcp IPv6 [2606:4700:4700::1111]:443        FAIL OSError
icmp 1.1.1.1                               NOREPLY
persist first                              HTTP/1.1 200 OK
persist after revoke                       FAIL ConnectionResetError
loopback (rebind) connections: 0
decoy connections: 0
conn rebind.example.net -> 127.0.0.1:49895 up=0 down=0 target_unreachable
conn example.com -> 104.20.23.154:443 ... revoked
```

The certificate of the real site was validated by the guest (TLS with the
default trust store and SNI `example.com`). The rebind name completes the
guest-side TCP handshake before the stack resets it; no byte from the
loopback service reached the guest and the service never accepted a
connection. Evidence file: `E:\OpenVM-tests\lab\net\logs\filtered-real.txt`.

## 4. Not verified

Historical list; subsequent scopes and results are recorded in R1-02
(product/grants), R1-03 (packaged supervisor crash) and
[R1-04](R1-04-network-faults-windows.md) (Windows PE, isolated worker panic
and controlled Windows DNS/policy boundary). The normal-resolver rebinding,
VPN, additional proxy configurations, host route mutation and installed browser
cases still remain open.

The product path end to end (VM settings, service resolution at run start,
agent session grant) with a real guest; DNS rebinding through a real
resolver; host VPN (split and full tunnel) and a system proxy; a route
change closing a persistent connection; gateway (supervisor) crash closing
connectivity; Windows guests and browsers; macOS/Linux host route checks.
