# R6-08 — Vista VMware import and direct VB6 command

Date: 2026-10-20. Host: Windows/WHPX. This is a real guest qualification,
using a separate catalog under `lab/vista-vb6-20261020/catalog`.

The source was `D:\Windows Vista-VS2010\Windows Vista-VS2010.vmx`, powered
off, with a single VMware snapshot. The production authenticated supervisor
preview/import operations flattened the current VMDK chain into a managed
QCOW2 disk and enabled the guest's own AHCI driver in the converted disk.
The original VMware VM was opened read-only and was not started or changed.

Created VM: **Vista VB6**, ID `01a159a0-5788-74a5-b55e-5d5d3877b81c`.
Windows Vista Business SP2 x86, version 6.0.6002; BIOS, 2 vCPU, 2048 MiB RAM,
40 GiB disk, standard VGA, offline network. The isolated Tools endpoint is
available independently of external networking. Vista displayed its native
activation reminder following the hardware change; "Activate later" was
initially selected without changing licensing configuration, allowing the
successful compiler run below. A subsequent reboot reported an expired
activation period and blocked normal desktop use. The VM was stopped; no
licensing bypass was attempted. The owner must legitimately reactivate
Windows after the hardware migration before further use. The final corrected
Tools package is on the VM's virtual CD; reinstall it after activation to
replace the first installed version. Optional device-driver
notifications were dismissed; no external drivers were downloaded.

Guest Tools were built as a lab-only freestanding PE32, subsystem 5.1, from
the current Windows sources. The local x64 MinGW compiler's i386 code
generation and Windows SDK x86 import libraries were used; the lab supplied
unsigned 64-bit division and stack-page-probing helpers. The binary imports
only the expected Win32 DLLs and no UCRT. A native PE32 smoke test verified
the helpers and command output/exit status. This is not a shipped signed
Tools package, nor certification of every legacy Windows release.

Tools were enabled with `guest_exec` and `files_transfer`. A new, independent
VB6 Standard EXE project was uploaded through the bounded inbox API. The
existing user projects and VB6 installation were not modified. The compiler
was the guest's installed
`C:\Program Files\Microsoft Visual Studio\VB98\VB6.EXE`.

The authenticated `GuestTools` IPC channel started `BUILD2.CMD` with
`exec_start`, then read the result through `exec_poll`. The script used
`VB6.EXE /make Hello2.vbp /out build2.log`, waited for the compiler and ran
the resulting `OpenVMVB6Probe.exe`. The module's `Sub Main` wrote a small
result file. No terminal focus or simulated typing was used for this
compilation/execution; console input was needed only for initial Tools setup.

Observed final result:

```text
BUILD_EXIT=0
Compilazione di 'OpenVMVB6Probe.exe' eseguita correttamente.
OPENVM_VB6_EXEC_OK
6 * 7 = 42
```

The command completed with exit code 0, empty stderr, no truncation and no
timeout. The result and 16384-byte executable were downloaded through the
outbox channel. Evidence remains in the ignored lab directory:
`vb6-success.json`, `vb6-result.txt`, `OpenVMVB6Probe.exe`, project sources,
screenshots, import result and supervisor audit. Credentials and bootstrap
secrets are excluded from this document and Git.

The real run exposed and drove fixes for three legacy/lifecycle cases:

- Vista/7's compatibility assistant can place Tools in a Job Object. Those
  systems cannot nest jobs: children must break away before entering the
  execution job. Windows 8+ keeps normal nested-job behavior, including
  parents that prohibit breakaway. Native tests cover both paths.
- An idle control peer can disappear while Tools is updated. The host now
  notices idle EOF instead of leaving the next handshake behind a stale
  connection. A regression test reconnects without losing an RPC.
- Installing from optical media copied the read-only attribute to the local
  executable, preventing an update. The installer clears that attribute on
  its own target before replacement and on the newly installed copy.

Native Windows execution lifecycle tests and the 12 authenticated control
tests pass. This qualification uses the production user IPC path; it does
not claim a separate end-to-end MCP agent-session approval test.
