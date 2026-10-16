# R6-06 — Guest network: NAT in manual use, endpoint allowlist (ADR-0033)

- Execution date (UTC): 2026-10-15
- Executed by: development agent on the project host
- Roadmap step: section 12, R6; sections 7.2 and 7.4 (network row)
- Related ADRs: ADR-0033 (decision and implementation), ADR-0034 (`network.endpoints`), ADR-0004, ADR-0027, ADR-0023
- Status: implementation verified in simulation/test, at the stack level with a real Linux guest, and partially through the product (grant, attached stack, allowed set, audit, revocation; guest traffic through a product-started stack not observed, §6); the real case (Windows guest, WCF service and SQL Server) **not verified**

## Purpose

Record what was built for ADR-0033 and what was executed to verify it: the supervisor-owned network of the endpoint allowlist (`crates/guest-net`), the NAT mode for manual use, the `network.endpoints` session capability, and their audit. What this file does NOT demonstrate: a Windows guest using the allowlist (no Windows guest with a .NET client was run), SQL Server or WCF traffic, throughput on a Windows guest, NAT reaching the Internet (see §4).

## Environment

| Item | Value |
| --- | --- |
| Host | Intel Core i7-12800HX, 31.6 GiB RAM; Windows 11 Pro 10.0.26200; Hyper-V/WHPX; host LAN 192.168.101.134/23 |
| QEMU | 11.1.0 (`lab/qemu/qemu-11.1.0`, ADR-0010) |
| Guest | Ubuntu 26.04.1 desktop live (`lab/media/ubuntu-26.04.1-desktop-amd64.iso`, SHA-256 as in R2c-01); for the stack-level tests booted with the kernel and initrd extracted from the ISO (`lab/integration-spike/ubuntu-boot/casper`, as in R6-04 §1), console on the serial port |
| Limits | 2 vCPU, 2 GiB, process priority BelowNormal, one guest at a time (another agent used the host in parallel) |
| Scratch and logs | `E:\OpenVM\lab\net\` (git-ignored); evidence files named below are in `lab/net/logs/` |
| Toolchain | Rust 1.99.0, MSVC 14.x environment, `CARGO_BUILD_JOBS=3` |

## 1. Design as built

See ADR-0033 *Implementation*. In short: Offline (no NIC) stays the default; NAT is `-netdev user` with the profile's inbox-driver NIC and is never given to a run under an agent session; the endpoint allowlist attaches the NIC with `-netdev stream` (AF_UNIX `net.sock` in the run's private directory) to `guest-net`, a smoltcp stack behind a frame filter that answers DHCP (no router) and DNS (allowed endpoint names only), terminates the guest's TCP to an allowed `virtual address:port` and opens one host connection per guest connection to the approved `host:port`, and drops or refuses everything else. The allowed set follows the sessions at run time; network events are audited and recorded in the session reports.

## 2. Tests in simulation (no QEMU)

| Item | Test | Outcome |
| --- | --- | --- |
| DNS: names, search domain, `A`/`AAAA`, `NXDOMAIN`, malformed queries never panic | `guest-net` `dns::tests` | verified in simulation/test |
| DHCP: DISCOVER/OFFER, REQUEST/ACK, NAK, other server, INFORM, no router option, malformed messages | `guest-net` `dhcp::tests` | verified in simulation/test |
| Frame classification (TCP SYN to an endpoint, to another address, DNS to the stack and elsewhere, DHCP, IPv6, other MAC, spoofed source, truncated) | `guest-net` `frame::tests` | verified in simulation/test |
| QEMU stream framing over a socket; close with a silent peer | `guest-net` `transport::tests` | verified in simulation/test |
| Stack against a second smoltcp interface as the guest: DHCP lease without router; DNS only for allowed names; six concurrent connections each with its own host connection and its own replies; 1 MB up and 700 kB down on one connection; refusals (RST) of a configured but not allowed endpoint, another port, the gateway and DNS addresses; no answer for other addresses; UDP and IPv6 dropped; revocation resets live connections and unresolves names; connection limit; unreachable target; stop; bandwidth limit; invalid configurations | `guest-net` `tests/stack.rs` (9 tests) | verified in simulation/test |
| Core: settings validation (modes, endpoints, Windows on ARM offline only); the network of each run (NAT in manual use, allowlist or no NIC under a session); session refused on a running NAT VM; granted endpoints must belong to the VM; allowed endpoints follow the sessions and never widen after one ends; network audit details | `vm-core` `tests/network.rs` (6 tests) | verified in simulation/test |
| QEMU command lines of the three modes on every profile; only `-netdev user` (no `hostfwd`, `guestfwd`, `smb`) or `-netdev stream` server on a Unix socket in the private directory; the audit refuses anything else; post-start check of exactly one NIC on the expected back end | `qemu-backend` `cmdline::tests::network_command_lines`, `run::tests` | verified in simulation/test |
| Through the real IPC with the real core and a fake backend: manual use allows every endpoint; a session granted one endpoint reduces the set at once; the agent's `vm_status` lists the granted endpoint; a connection record is audited with the session and appears in the report; revocation empties the set; the next start is manual use again; a session on a running NAT VM is refused (`nat_running`); approved while stopped the next run gets the session's allowlist; without the capability the run has no NIC; endpoints not of the VM or without the capability refused | `lightvm-service` `tests/network.rs` (3 tests) | verified in simulation/test |
| MCP: the `network_endpoints` hint reaches the app; the granted endpoints in `vm_status` | `lightvm-mcp` `tests/stdio.rs` | verified in simulation/test |
| UI: network mode menu and endpoint editor (validation keeps "Save settings" disabled; virtual address assigned; Windows on ARM offline only); approval form offers the VM's endpoints unticked, ticking one sends `network.endpoints` with exactly that endpoint; NAT running flagged | `lightvm-desktop` `tests/vm_settings_ui.rs`, `tests/agents_ui.rs` | verified in simulation/test |

## 3. Endpoint allowlist with a real Linux guest (stack level)

Test `guest-net` `tests/real_guest.rs::endpoint_allowlist_with_a_real_linux_guest` (`#[ignore]`, run with `--ignored`). QEMU is started by the test with `-netdev stream,id=n0,server=on,addr.type=unix,addr.path=lab/net/s/net.sock -device e1000,netdev=n0,mac=52:54:00:12:34:56`; the test connects to the socket and runs `guest_net::NetStack` with two endpoints, `sql.lan` (`10.0.2.101:1433`) and `wcfhost` (`10.0.2.102:8733`), both mapped to a line server on the host loopback, first with only `sql.lan` allowed. The guest logs in on the serial console, gets its lease, then a hostile probe adds `default via 10.0.2.2` and runs `probe.py` (pushed in base64). A decoy listener on all host interfaces must never see a connection. Evidence: `lab/net/logs/allowlist-real.txt`.

Guest view after DHCP: `10.0.2.15/24` on `enp0s2`, route `10.0.2.0/24` only (**no default route** from the stack's DHCP), DNS `10.0.2.3` (systemd-resolved used it: `getaddrinfo` resolved `sql.lan`). Probe results (3 s timeout per probe; `allowlist-real-final.txt`):

| Check (only `sql.lan` allowed) | Result |
| --- | --- |
| DNS `sql.lan` (`A`) to `10.0.2.3` | `10.0.2.101` |
| DNS `sql.lan` (`AAAA`) | empty `NOERROR` |
| DNS `wcfhost` (configured, not allowed), `example.com` | `NXDOMAIN` |
| `getaddrinfo("sql.lan")` through the guest's resolver | `10.0.2.101` |
| TCP `sql.lan:1433` by name, `10.0.2.101:1433` by virtual address | connected, reply of the host server (`ECHO 1 …`, `ECHO 2 …`: one host connection each) |
| 8 concurrent connections, requests sent in reverse order | every reply on its own socket, 8 distinct host connections |
| 16 MiB guest → host and host → guest on one connection (checksummed) | correct; 12.6 MiB/s up, 13.0 MiB/s down (guest Python, e1000, shared host) |
| TCP `10.0.2.102:8733` (configured, not allowed), `10.0.2.101:80` (other port), `10.0.2.2:<decoy>`, `10.0.2.2:135` | refused (RST from the stack) |
| TCP host LAN address `192.168.101.134` ports decoy/135/445; Internet `1.1.1.1:443`, `8.8.8.8:53` (with the default route the probe added) | no answer (dropped; time-out) |
| DNS over UDP to `8.8.8.8`, `1.1.1.1` | no answer |
| TCP over IPv6 (`2606:4700:4700::1111`, `fec0::2`) | fails in the guest: no IPv6 address or route (the stack sends no router advertisement); the guest's IPv6 frames (to multicast addresses) are dropped |
| ICMP echo `10.0.2.2` / `1.1.1.1` | answered by the stack itself / no answer |
| Decoy listener on every host interface | **0 connections** |
| `sql.lan` and `wcfhost` allowed (run-time change): DNS `wcfhost` and `wcfhost.lightvm.internal`, TCP `wcfhost:8733` by name | `10.0.2.102`; connected, host reply |
| Nothing allowed (revocation): DNS `sql.lan`; TCP `10.0.2.101:1433` | `NXDOMAIN`; refused |

Stack counters at the end: 12 proxied connections, 20 refused attempts, 2 DHCP replies, 5 names answered and 113 refused (the guest's own background lookups included), 16 777 339 bytes from the guest and 16 777 405 to it. Records: 12 connections (endpoint, `10.0.2.101:1433 -> 127.0.0.1:<port>`, guest port, byte counts, `closed`) and the refused attempts (`not_allowed` for `10.0.2.102:8733` and, after the revocation, `10.0.2.101:1433`; `not_an_endpoint` for the others). The host line server saw exactly the 12 proxied connections.

Outcome: **verified on hardware (Windows, WHPX)**, at the stack level (QEMU started by the test, not by the service).

**Finding during the run (fixed).** The first runs showed some guest connections closed by the stack right after the handshake, before their data: the stack took "cannot receive" in SYN-RECEIVED for a FIN from the guest and half-closed the host connection; the host server then closed it. The fix treats only the states after a FIN from the guest (CLOSE-WAIT, LAST-ACK, CLOSING, TIME-WAIT, CLOSED) as a half-close. The in-memory tests had not shown it because there the handshake completed before the host connection. The diagnostic frame trace (`LIGHTVM_NET_TRACE`) was added for this analysis.

## 4. NAT with a real Linux guest

Test `guest-net` `tests/real_guest.rs::nat_with_a_real_linux_guest`: the same guest with `-netdev user,id=n0 -device e1000,...` (the product's NAT form). Evidence: `lab/net/logs/nat-real.txt`.

Guest view: `10.0.2.15/24`, **default route via `10.0.2.2`** and DNS `10.0.2.3` from slirp's DHCP.

| Check | Result |
| --- | --- |
| TCP host server on the host loopback through `10.0.2.2:<port>` | connected, host reply |
| TCP decoy listener on every host interface through `10.0.2.2` | connected (NAT reaches host services, as intended) |
| 4 concurrent connections | each its own host connection and reply |
| 16 MiB both ways | correct; 19.0 MiB/s up, 28.0 MiB/s down (same guest, shared host) |
| TCP `1.1.1.1:443` (Internet) | connected |
| DNS over UDP to `10.0.2.3` | answered (forwarded by slirp to the host resolver) |

Outcome: **verified on hardware (Windows, WHPX)** at the QEMU back-end level with the product's NAT form (`-netdev user` without forwarding rules, `e1000`). That a run under an agent session never gets NAT is verified by the core and IPC tests (§2); the product start with NAT is verified in §5 (command and post-start check).

## 5. Product command lines under WHPX

Test `qemu-backend` `tests/real_qemu.rs::real_network_modes` (`LIGHTVM_QEMU_DIR`, lab under `lab/net/qb`): the product's `VmLaunchSpec` with `GuestNetwork::Nat` and `GuestNetwork::Supervisor` starts under WHPX and passes the post-start check. `info network` observed:

| Mode | `info network` | Socket |
| --- | --- | --- |
| NAT | `e1000.0: index=0,type=nic,model=e1000,macaddr=52:54:00:12:34:56` / `net0: index=0,type=user,net=10.0.2.0,restrict=off` | none |
| Supervisor | `e1000.0: index=0,type=nic,model=e1000,macaddr=52:54:00:12:34:56` / `net0: index=0,type=stream,listening` | `net.sock` in the run's private directory; the supervisor connected |

Outcome: verified on hardware (Windows, WHPX).

## 6. End to end through the product (service, agent session)

Test `lightvm-service` `tests/f4_real.rs::e2e_real_qemu_network_endpoints` (`LIGHTVM_E2E_QEMU_DIR`, `#[ignore]`): real service (SQLite catalog, file audit, `QemuVmBackend`), a VM on the endpoint allowlist with `sql.lan` (host `localhost`) and `wcf` (`127.0.0.1`), each mapped to its own line server on the host loopback; an agent session granted `network.endpoints` = `sql.lan`; the user starts the VM while the session is live (Ubuntu live ISO, 2 vCPU, 2 GiB, QEMU process lowered to BelowNormal), the agent renews its lease, then is meant to type `/dev/tcp` probes in a guest shell. Three runs; evidence `lab/net/logs/net-e2e.txt` and the screenshots next to it.

| Item | Observed |
| --- | --- |
| The run under the session gets the session's network | `network_setup` with `mode: endpoints` (the VM's mode would also be `endpoints`; NAT under a session is covered by §2) |
| Endpoints resolved at the start, address recorded | `sql.lan` → `10.0.2.100:<p>` resolved from `localhost` to `127.0.0.1:<p>`; `wcf` → `127.0.0.1:<q>`; all audited with the session id |
| Stack attached to QEMU's `net.sock` by the real backend | no `network not attached` failure record; QEMU passed the post-start check (`type=stream`) |
| Allowed set applied from the grant | `network_policy` `allowed: sql.lan` (session id recorded) |
| The agent sees only the granted endpoint | `vm_status`: `sql.lan`, `10.0.2.100`, port `<p>` |
| Revocation | `network_policy` `allowed: none` |
| Guest traffic through the product-started stack | **not observed**: in the first run the live desktop appeared after 173 s (top bar with the wired-network icon) but the terminal did not open in time; in the next two runs (host shared with another agent's guest) the desktop did not settle within 600–900 s at 2 GiB and the text console on tty3 had no login prompt. No connection reached either server; no guest command ran. |

Outcome: **partial**. Verified on hardware: the product path from the grant to the attached stack and its allowed set, the audit with the session, the agent's view, and the revocation. Guest traffic through a stack started by the product is not verified end to end here; it is the same code (`guest_net::transport::attach` on QEMU's AF_UNIX `-netdev stream` socket, `NetStack`) that carried the guest traffic of §3, where only the launcher of QEMU differs. To complete: rerun with a guest that runs a probe by itself at boot (for example a data ISO with a script, or an installed guest with a service), or with more memory.

## 7. Licences

`cargo deny --locked check`: `advisories ok, bans ok, licenses ok, sources ok` (22 non-blocking `duplicate` warnings, as before; the only one touching the new crates is `bitflags`, through `smoltcp`). Output: `lab/net/logs/cargo-deny.txt`. New packages: `smoltcp` 0.14.0 and `managed` 0.8.0 (`0BSD`), `heapless` 0.9.3, `hash32` 0.3.1, `stable_deref_trait` 1.2.1 (`MIT OR Apache-2.0`), `byteorder` 1.5.0 (`Unlicense OR MIT`); inventory entry in ADR-0023.

## 8. What is not verified

- A Windows guest (XP `rtl8139`; Vista, 7, 10 `e1000`) obtaining its lease and reaching an endpoint; the WPF client reaching a WCF service and SQL Server on the host (the real case of ADR-0033).
- NAT reaching the Internet from a guest started by the product (the stack-level NAT run covers the QEMU back end only; see §4 for the Internet row).
- Throughput of the allowlist with a Windows guest and with several guests at once; the bandwidth limit with a real guest (not set by the service).
- Long-running connections (hours), guest suspend/resume, a host endpoint that closes idle connections (each guest connection has its own host connection, so this is expected to behave like a direct connection; not measured).
