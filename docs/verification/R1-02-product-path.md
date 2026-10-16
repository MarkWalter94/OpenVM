# R1-02 — Filtered Internet through the product service and agent grant

Date: 2026-10-17. Passed on this Windows host with a real Ubuntu 26.04.1
live guest under WHPX. This is an in-process service harness using the
production CoreEngine, IPC, QemuVmBackend, agent grants and Tools transfer.
It does not qualify a separately packaged supervisor process.

Test: `filtered_product::filtered_internet_through_service_and_agent_grant`
in `crates/lightvm-service/tests/common/filtered_product.rs`, included by
`f4_real.rs`. Guest fixture: `common/filtered_probe.py`.

The VM uses its normal FilteredInternet profile, 2 vCPUs and 6144 MiB RAM.
The service configures three endpoints; the agent receives a narrower grant.
The guest adds a default route through 10.0.2.2 before exercising the policy.
No extra test NIC, kernel override or shell RPC is used. Keyboard input
starts the fixture from a data ISO; results return through production Tools.

Verified:

- `example.com` resolves to virtual endpoint 10.0.2.101. HTTPS with normal
  certificate verification and SNI returns HTTP 200 using TLS 1.3.
- The configured but ungranted `www.iana.org` endpoint, unknown names,
  another port, an ungranted virtual IP, direct Internet IP, host gateway,
  metadata address, external UDP DNS and IPv6 do not become reachable.
- The guest compiles and builds a Python zip application, executes it in
  the guest, and returns the application and build log through Tools.
  The host verifies its SHA-256 and the per-run nonce. The host does not
  execute the returned application.
- Revoking the agent session closes the persistent example.com TLS
  connection within the test's 15-second bound; the network audit records
  reason `revoked`. The user-started VM remains running until explicit
  test cleanup stops that specific VM/run.

Final run: 1 passed, 273.88 seconds. Log:
`E:\OpenVM-tests\r1-product-final.log`.
Evidence directory: `E:\OpenVM-tests\lab\r1-product\5180ba24db77`, containing
`guest-result.json`, `build.log`, `application.pyz`, `audit.jsonl`,
`results.txt` and `guest-0.png`.

Returned application SHA-256:
`06a8fde0760e9fdaf58661cb8e3dede015df40a656d0314ad6fb942457c23596`.
Run nonce: `01a146de-32dc-75c1-af08-fb63dfee3cae`.

Reproduction (PowerShell, with the MSVC environment wrapper available):

```powershell
$env:TEMP = 'E:\OpenVM-tests\tmp'
$env:TMP = $env:TEMP
$env:CARGO_TARGET_DIR = 'E:\OpenVM-tests\target'
$env:LIGHTVM_LAB = 'E:\OpenVM-tests\lab'
$env:LIGHTVM_E2E_QEMU_DIR = 'E:\OpenVM-tests\lab\qemu\qemu-11.1.0'
& E:\OpenVM-tests\with-msvc.ps1 @('test', '-p', 'lightvm-service',
  '--features', 'supervisor', '--test', 'f4_real',
  'filtered_internet_through_service', '--locked', '--', '--ignored',
  '--nocapture', '--test-threads=1')
```

The packaged Tools bundle must be beside the test executable in
`E:\OpenVM-tests\target\debug\deps\tools`. Launching through the C: target
junction is rejected by the Tools bundle's ancestor reparse-point guard;
the test must use the physical E: target directory.

Limits: `localtest.me` was blocked but did not resolve on the host, so this
run does not prove rejection of a real DNS rebinding response. Installed
Linux restart/rollback, Windows/browser behavior, host VPN/proxy/route
changes and gateway crash qualification remain separate work. The guest
application build is a live-guest check, not installed-guest qualification.
