# R1-03 — Packaged supervisor crash with filtered Internet

Date: 2026-10-17. Windows host, WHPX, installed Ubuntu 24.04.5 fixture
from R6-07b. Test: `installed_linux::filtered_crash::packaged_filtered_supervisor_crash_and_recovery`
in `crates/lightvm-service/tests/f4_real.rs`; implementation and guest
application in `common/filtered_crash.rs` and `filtered_crash_probe.py`.

## Scope and method

The test launches the actual supervisor from experimental ZIP
`0.0.0-alpha.20261017.3` (F6-04), rather than an in-process service.
The release executable SHA-256 is
`ed017cd0ec6803a791643728edc043c8a6fe3a542328ce81fbab9b6af4d2c93d`.
The guest uses 2 vCPUs, 3072 MiB, NVMe and the ordinary QEMU 11.1.0
runtime. Its VMware wrapper is synthetic; it is not a VMware-origin OS
installation. The fixture source disk stays unchanged on E:.

Through normal IPC the test imports the guest and data ISO, selects
FilteredInternet, enables Tools file transfer and configures only
`example.com:443` at virtual IPv4 `10.0.2.101`. A first clean shutdown
completes and persists NoCloud's installation of the lab services. The
next boot is the crash subject. No extra network interface, shell RPC,
host routing/proxy change or physical USB device is used.

The guest compiles a Python zip application delivered through Tools.
It verifies DNS pinning, normal TLS certificate verification and HTTP 200;
direct `1.1.1.1:443`, host gateway `10.0.2.2:443` and virtual endpoint
port 80 must be unreachable. It then holds a second verified TLS
connection open and publishes a fresh nonce. The host confirms an actual
ESTABLISHED port-443 connection owned by this supervisor, records only
that process's socket rows and identifies its own QEMU child.

The test kills its owned supervisor child handle and requires its QEMU
child to disappear within 10 seconds, its old IPC endpoint to reject
connections, and no established sockets to remain for the old process.
It restarts the same release service and catalog, requires an
Unauthenticated protocol reply for the old credential and HelloOk for the
new one, verifies stopped state without automatic respawn and checks that
the filtered configuration survived. Only an explicit Start may create a
new run and boot. The guest repeats HTTPS and refusals, returns its
application and build log through Tools, and shuts down normally. The
host checks the application SHA-256 and unchanged source fixture disk.
Ready credentials remain in memory and are never written to evidence.

## Result and evidence

**Passed: 1 test, 90.87 seconds.** The active supervisor PID 21924 had
QEMU child PID 23196 and an established remote HTTPS connection. QEMU
disappeared **117 ms** after termination of its supervisor. The old IPC
and established sockets were gone; credential rotation, stopped-state
reconciliation without respawn, retained policy and explicit new run all
passed. Both HTTPS checks returned HTTP/1.1 200 with TLS 1.3, and all three
forbidden connection attempts failed before and after recovery. The returned
zip application's checksum and unchanged fixture-source checksum passed.

The successful run's small evidence is under
`E:\OpenVM-tests\filtered-crash-20261017\passed`: setup/first/recovered boot
records, both network reports, crash record, reconciled VM configuration,
service and QEMU logs, audit, returned application/build log and results.
`run-3.log` records the actual test result. `SHA256SUMS.json` covers 49
evidence files, all re-read and verified. No owned supervisor or QEMU
process remained. Rust test compilation (`cargo rustc --test f4_real`),
Clippy with warnings denied, Rust formatting and Python AST syntax passed.
The initial broader cargo-test build could not replace Claude's running
debug service; the test binary compiled, and the later test-only build
completed successfully without interrupting that service.

## Preliminary attempts and diagnosis

- Attempt 1: crash containment passed (166 ms), then the harness wrongly
  expected a transport error for an invalid credential. The IPC API returns
  an explicit error message successfully; corrected the assertion to require
  the protocol code `Unauthenticated`.
- Attempt 2: containment passed (114 ms), authentication and reconciliation
  passed, but guest Tools did not return after power loss during first-boot
  provisioning. Read-only disk inspection found both new systemd units,
  the lab launcher/probe and installed Tools program at zero bytes.
  The guest had reached its login prompt. The test now completes a clean
  shutdown before injecting power loss into the installed system. This
  does not qualify crash durability of initial installation or file writes.
  Attempt 2 ended with a ready-file timeout (342.67 seconds).

Logs and diagnostic screenshot/file sizes are preserved under
`E:\OpenVM-tests\filtered-crash-20261017`. Preliminary failures are not passes.

## Limits

This terminates the whole supervisor, including the gateway thread. It
does not inject an isolated gateway-thread panic while the service stays
alive. The VM is manually started; agent grant/revocation evidence remains
R1-02. No VPN, system proxy, real DNS rebinding, Windows guest or route
mutation is qualified by this test. Abrupt termination is a power loss;
guest application consistency and crash-safe installation are not promised.

## Reproduction

Set `LIGHTVM_INSTALLED_LINUX_FIXTURE` to the R6-07b prepared fixture,
`LIGHTVM_INSTALLED_SERVICE_EXE` to the extracted release supervisor,
`LIGHTVM_E2E_QEMU_DIR` to the approved runtime and
`LIGHTVM_FILTERED_CRASH_ROOT` to a fresh short private catalog directory.
Run the named ignored test from the physical E: target path with
`--ignored --nocapture --test-threads=1`. Preserve the small evidence and
remove only this test's disposable catalog afterward.

## Cleanup limitation on this host

Automatic approval review rejected the native PowerShell cleanup request
for the two preliminary C: catalogs with **`blocked by policy`**. No retry
or alternate deletion mechanism was used. All three stopped disposable
catalogs remain at `C:\lvnet-20261017`, `C:\lvnet2-20261017` and
`C:\lvnet3-20261017`, totaling 5,702,124,977 logical bytes (about 5.31 GiB).
Their small evidence was copied to E: and verified independently of the
catalogs. `cleanup-pending.json` records the retained paths and sizes.
Claude's VM files/processes were not touched.
