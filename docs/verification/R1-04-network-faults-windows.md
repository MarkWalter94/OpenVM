# R1-04 — Windows HTTPS, isolated gateway panic and DNS boundary

Date: 2026-10-18. Windows host, WHPX. Complements the product/grant path
R1-02 and the separately packaged supervisor termination/recovery R1-03.
R1 remains incomplete for the unexecuted cases listed below.

## Windows PE, native HTTPS and refusals — passed

`windows11_real::filtered_windows::winpe_filtered_https_and_forbidden_destinations`
uses a new private VM, the previously qualified Windows 11 runtime bundle,
the owner's approved Windows installation ISO, and the actual QemuRun and
NetStack. Existing Windows/BitLocker VMs are untouched. The guest is Windows
PE, not a completed Windows installation or browser.

A separate native C probe (`common/filtered_winpe.c`) uses Winsock's
resolver and WinHTTP. MSVC build: `/W4 /WX /O2 /MT
/D_WIN32_WINNT=0x0601`, linked with `winhttp.lib` and `ws2_32.lib`.
No certificate checks are disabled. The guest sees only one Internet
endpoint, `example.com:443` at virtual IPv4 `10.0.2.101`, in addition to
the private authenticated Tools channels. Host resolution is validated and
pinned before attaching the stack.

- DNS returned `10.0.2.101`; WinHTTP returned HTTPS **200**, 577 body bytes,
  error 0, using its normal certificate verification.
- An ungranted DNS name, direct `1.1.1.1:443`, gateway `10.0.2.2:443` and
  endpoint port 80 were all inaccessible.
- A per-run nonce and the log returned through Tools. WinPE requires the
  existing lab's 64-bit Tools build; this is not qualification of the
  packaged 32-bit Windows Tools executable.
- First attempt passed in **23.78 s**. The same test with only its host
  process's `HTTP_PROXY`, `HTTPS_PROXY` and `ALL_PROXY` pointed at a loopback
  sentinel also passed in **22.00 s**; the sentinel received no connection.
  `NO_PROXY` was empty. Host Windows proxy settings were not changed.
- Both owned QemuRun shutdowns reported zero active processes afterward.

Probe executable, bundle manifest and test executable hashes are recorded
in the evidence provenance. This is backend/stack-level Windows evidence,
not the packaged supervisor, UI settings/grants or an installed browser.

### Configured Windows current-user PAC — passed and restored

A further test configures an actual current-user Windows PAC, restricted
to `example.com`; other names return DIRECT. The baseline had no explicit
proxy/PAC. The native `host_proxy_probe.c` canary verifies through
[WinHttpGetIEProxyConfigForCurrentUser](https://learn.microsoft.com/en-us/windows/win32/api/winhttp/nf-winhttp-winhttpgetieproxyconfigforcurrentuser)
and [WinHttpGetProxyForUrl](https://learn.microsoft.com/en-us/windows/win32/api/winhttp/nf-winhttp-winhttpgetproxyforurl)
that Windows actually selects the loopback proxy for this URL. The real
WinPE HTTPS/denial test then runs while that configuration is active.
The gateway never connects to the proxy sentinel and HTTPS still returns
200. The final complete cycle **passed in 24.20 s** for the VM test,
with successful orchestration and restoration verification.

`run_system_proxy_qualification.ps1` restores the original connection
flags (9) and original AutoConfigURL presence/value in `finally`, then
checks the native Windows configuration against its initial value.
`system-proxy-restored.json` records `restored=true`; no explicit PAC/proxy
remains enabled. The helper refuses an existing explicit proxy/PAC and
accepts only a loopback PAC URL. The PAC fixture runs hidden and is stopped
through its owned process handle. The Python server and native canary are
lab tools, not shipped components.

The preceding system-proxy attempt's VM test also passed (22.16 s), but
its PowerShell finalization failed because it had opened the registry
handle read-only. The native API had already restored the active flags
and disabled the PAC. The original URL absence was then verified/restored,
the script changed to a writable handle and the full cycle repeated.
That orchestration failure is preserved in `system-proxy-attempt-3-status.json`,
not counted as a passing complete cycle. Machine-wide static WinHTTP proxy
configuration and authenticated/enterprise PACs were not tested.

## Isolated worker panic with a real Linux guest — passed

`guest-net` unit-test executable, ignored test
`stack::real_panic_tests::real_filtered_guest_loses_egress_on_isolated_gateway_panic`:
approved Ubuntu live kernel/initrd/ISO, one e1000 NIC connected only to the
production stack's private stream socket. A serial console drives the
disposable guest; the service IPC is not part of this test.

The guest opens a normally verified TLS connection to example.com and
gets HTTP 200. With that connection active, a **test-build-only** command
panics the network worker, while the host test and QEMU stay alive.
The worker and runtime/link cleanup completed in **22 ms**; the active
connection count became zero. The old guest TLS request subsequently
timed out (10-second socket bound), and a fresh connection failed with
Linux result 11. No successful old or new request followed the panic.
The VM remained alive and usable over serial until explicit owned cleanup.
Complete test: **passed, 39.79 s**. This is not a claim of a guest-visible
RST or of the TLS client reporting failure in 22 ms.
After making the fixture socket/log names unique per process for repeatable
qualification, a second complete attempt also passed: **39.16 s**, worker
cleanup **40 ms**, old TLS timeout and new-connection failure again observed.
Both attempts are preserved separately.
A third complete run after the SYN/RST slot fix below also passed:
**38.66 s**, worker cleanup **56 ms**, VM alive, old TLS timeout and
fresh connection failure. All three real panic attempts passed.

A second test with a smoltcp guest and a real local host TCP connection
verifies host EOF/reset and closed guest input after the worker panic,
within 3 seconds. Production `NetStack` previously cleared its running
flag only on ordinary return; an unwind could leave it reporting alive.
A drop guard now clears status and the open-connection count after runtime
and link cleanup, including unexpected unwind. The panic command and both
fault harnesses exist only under `cfg(test)`; release builds gain no crash
switch. The ordinary integration/real-guest suites share extracted fixture
helpers, retaining their existing tests.

## DNS answer changes — controlled boundary passed

`adapters::network::dns_real::real_windows_dns_rebinding_answer_is_rejected_by_public_policy`
starts a private loopback DNS fixture at `127.0.0.54:53`, then invokes
Windows `Resolve-DnsName` against that server explicitly. No machine DNS,
hosts file, route or firewall setting changes. The same name first returns
`1.1.1.1`, then `127.0.0.54`, with TTL zero. The actual production
`public_address` policy accepts the first and rejects the rebound private
answer. **Passed in 2.11 s**. Fixture shutdown releases its thread and port.
The [Windows resolver's server override](https://learn.microsoft.com/en-us/powershell/module/dnsclient/resolve-dnsname)
is query-local. Separate portable tests reject mixed public/private answers
in either order, IPv6-only/empty replies and oversized replies.

This verifies real DNS transport and Windows resolver output at the policy
boundary. It does **not** replace the production `ToSocketAddrs` resolver
globally or qualify a rebinding domain end to end through the normal
configured DNS server and a running product VM. A public rbndr.us attempt
through the normal system resolver produced only resolution errors across
eight samples; recorded as an unavailable prerequisite, not a pass.

## Remaining cases and cleanup

### Connection-slot exhaustion found during review — fixed

Claude's concurrent review identified that a guest could send SYN, receive
SYN-ACK, then send RST before its final ACK. smoltcp returns that socket to
LISTEN, but the gateway kept its already-started host connection and slot.
Repeated attempts could exhaust the connection limit. A regression with a
real local host TCP server withheld ACKs and reproduced the fault before
the fix: timeout waiting for the first aborted handshake's slot to clear.

Finished LISTEN sockets are now reaped regardless of host state, cancelling
the host task, releasing the slot and recording ResetByGuest. Reaping also
runs before a new SYN so RST and replacement SYN in one input batch cannot
reuse the old host stream. The regression aborts six handshakes with a
two-slot limit and then verifies normal echo traffic; a separate unit test
checks the same-batch replacement. After the fix: 13 unit tests and 11
integration tests passed (the real-guest panic test remains explicitly ignored
in the ordinary suite). The before/after logs are retained.

### Shared addresses / domain fronting — implementation limitation

The current gateway checks the pinned IP and port; it does not validate
HTTP Host, TLS SNI or encrypted HTTP authority. A public CDN address can
serve multiple websites, so controlling its DNS entry does not enforce a
strict domain allowlist against a guest choosing a different authority over
the permitted socket. ADR-0037, the user guide and the settings help now
state this boundary. This is an open security/design limitation for stronger
website isolation, not a new approved weakening of R1. No TLS interception
was introduced and no hostname-exclusivity guarantee is made.

### Other unqualified cases

- Real split/full VPN and a persistent connection during an actual host route
  mutation remain **not executed**.
  The current token is not Administrator; route mutation requires an
  elevated lab session. Current-user system PAC and proxy environment are
  verified above; machine-wide static WinHTTP and enterprise proxy variants
  remain outside these tested configurations.
- A completed installed Windows guest/browser and rebinding through the
  product's normal resolver path remain open, as does stronger domain
  isolation on shared public addresses.
- On an elevated lab, route testing must use only a new `/32` route for
  the pinned public test endpoint, preserving the original default route
  and recording the exact created route for removal in `finally`. A live
  connection and new connection must fail once the route violates policy.
  No default route or active VPN was changed on this host.
- The user's renewed request to clean the three C: lvnet catalogs was
  again rejected by automatic approval review: **`blocked by policy`**.
  No alternate deletion method was attempted. The prior 5.31 GiB and its
  verified R1-03 evidence remain; all new VM scratch/builds used E:.

Evidence: `E:\OpenVM-tests\network-20261018\evidence`, with native guest
logs/screenshots, proxy sentinel result, real-panic serial/host logs,
controlled DNS console, failed external DNS samples, regression results,
source/binary provenance and a verified `SHA256SUMS.json` inventory.
No owned QEMU, MCP client or DNS fixture was left running.
Concurrent review changes outside the owned network files are not certified
by these earlier binaries; the recorded hashes identify the tested artifacts.
