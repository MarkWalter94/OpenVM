# R6-07 — LightVM Tools: foundation and product integration

- Execution date: 2026-10-15
- Scope: ADR-0031 and ADR-0034, first implementation increment
- Current status: **partial product integration**, paused at the owner's
  explicit request for commit/push. The dated follow-up at the end supersedes
  the initial implementation limitations below; neither Tools nor Windows 11
  nor filtered Internet is declared complete.

The sections through "Proposed decisions" preserve the initial foundation
verification record. Current implementation and remaining work follow it.

## Purpose

Build the bounded, authenticated file-channel foundation before exposing
host folders to guest code. The owner's requested order is Tools first,
then Windows 11, then filtered Internet. This file does not close any of
those milestones or change the roadmap exit criteria.

## Environment

- macOS 27.0.1 (26A434), Apple Silicon arm64, pinned Rust 1.99.0 toolchain.
- Tests use temporary directories on the Mac and real loopback TCP sockets.
  No guest, QEMU, hypervisor, Windows host or physical USB device is used.
- Windows cross-check: `x86_64-pc-windows-gnu`, compiler check only, including
  the Windows filesystem branch and test targets; no Windows executable run.
- The owner confirmed that only this Mac is available for this work.

## Procedure

From the repository root, with the pinned toolchain on PATH:

```sh
cargo fmt --all --check
cargo test -p lightvm-tools --locked
cargo clippy -p lightvm-tools --all-targets --locked -- -D warnings
cargo check -p lightvm-tools --all-targets --target x86_64-pc-windows-gnu --locked
git diff --check
```

The workspace discovers the crate through `crates/*`. Its dependencies use
versions already locked by the workspace; no dependency version was upgraded.

## Results

| Check | Outcome | Evidence |
| --- | --- | --- |
| Unit/integration tests | verified in test: 14 passed, 0 failed | `crates/lightvm-tools/tests/boundaries.rs`, `tests/channel.rs` |
| Framing | verified in test | zero/oversized lengths and truncated frames refused; JSON payload bounded before allocation/serialization; unknown fields refused |
| Authentication and authorization | verified in test | wrong VM/run refused, old permits invalidated on grant replacement, expiry and revocation deny operations, read-only and unconfigured shares refused |
| Path confinement | verified in test on macOS | traversal/device/alternate-stream names refused; root, intermediate and file symlinks refused; replacing an open share's pathname cannot redirect its held directory handle |
| Transfer integrity | verified in test | contiguous chunks, exact length and SHA-256 required before publication; existing destinations never overwritten; bad hashes and out-of-order chunks refused |
| Large streaming transfer | verified in test over real loopback TCP | 52 MiB + 17 bytes uploaded and downloaded in 64 KiB chunks, checked against SHA-256; exceeds the WebDAV redirector's default 50 MB read limit without using that redirector |
| Cancellation/disconnect | verified in test | cancelled and disconnected uploads remove staging files; downloads publish only after checksum validation |
| Priority close | verified in test | close interrupts a half-written handshake; revocation does not wait on an admitted file operation; already forwarded syscalls remain distinct from new admissions |
| Resource bounds | verified in test | per-file and aggregate byte budget, file-count and concurrent-transfer limits; cancelled writes do not refund the byte budget |
| Diagnostics | verified in test | credential and file-payload Debug output redacted; filesystem errors returned as stable codes without host paths |
| Rust lint | verified | package clippy with warnings denied |
| Windows code | compile checked only | all-targets GNU Windows check succeeds; not a runtime, XP/Vista or filesystem verification |

An initial TCP test failed because Darwin accepted sockets inherited the
listener's nonblocking mode. The session now explicitly selects blocking
mode and bounded read/write timeouts; the complete TCP suite passed after
the correction. TCP_NODELAY avoids delayed-ACK stalls between frame headers
and payloads.

## Limits

- This is a library increment, not an installed guest component or a
  completed Tools feature. `FileServer` is not started by the product.
- The listener's loopback address is not an isolation boundary against
  other host processes. Run-bound authentication is required; guest access
  must eventually use a reserved internal endpoint in the supervisor stack.
- The file protocol currently supports listing, chunked reads and
  create-only verified uploads. It is not a complete WebDAV filesystem:
  overwrite, directory creation, rename, delete and drive mapping remain.
- Completed writes produce bounded in-memory records. The product's
  persistent audit/report sink, denial accounting and rate limits are not
  connected yet.
- Ordinary cancellation, disconnect and object teardown clean staging
  files. Process-kill recovery needs a persistent journal; no arbitrary
  `.lightvm-transfer-*` file is removed by scanning a user folder.
- Publication uses a hard link in the same directory, requiring filesystem
  support. A failure after linking (cleanup/directory flush) can leave a
  complete verified destination despite an error; the product must
  reconcile and report this uncertain commit through its journal.
- Windows directory handles reject reparse points and omit share-delete
  while walking the ancestor chain. Only compiler checking was done here.
- This Rust std library does not establish XP/Vista guest compatibility.
  The legacy Windows guest component still needs the direct-Win32 toolchain
  of ADR-0031. No clipboard, USB or UI Automation runtime is implemented.
- No full-workspace regression, installer test, real guest test or security
  review is claimed by these package-scoped results.

## Required actions

1. Wire the run-owned channel into the supervisor and its reserved network
   endpoint; rotate credentials on every start and rollback. Deliver the
   bootstrap and approved guest binaries through a Tools ISO.
2. Add persistent per-VM Tools settings and explicit session capabilities,
   named shares/devices, quotas, approval UI and MCP requests. Disconnect
   unauthorized integrations before session admission; connect the
   priority stop, lease expiry, UI/client loss and shutdown paths.
3. Complete filesystem operations, journal recovery and persistent audit;
   implement the guest mapping/transfer component for Windows and Linux.
4. Implement focused clipboard synchronization and the desktop transfer
   flow (progress, cancellation, drag and drop).
5. Implement USB redirection using the already decided UsbDk/usbredir path
   on Windows, including detach, revocation and return to the host.
6. Implement `ui.inspect`/`ui.act`, Windows UI Automation and Linux AT-SPI,
   bounded trees and supervisor checks on every action (ADR-0036).
7. Package/install the Tools and verify real guests (including legacy
   Windows), negative tests, a representative 750 MB build and log return.
8. Only after Tools are complete, proceed to Windows 11 x64 (ADR-0035), then
   filtered Internet (R1). Existing Windows 11 laboratory blockers stay
   open; NAT and endpoint allowlisting do not establish R1 completion.

## Proposed decisions

None. Existing accepted ADRs and the owner's priority order remain in force.

## 2026-10-15 follow-up and handoff

The run now owns a private Tools ISO containing bootstrap credentials and the
Windows/Python guest components. Reserved internal TCP endpoints use the
supervisor's isolated stack; a manual NAT run uses a separate subnet/NIC for
Tools. Tools-only DHCP does not advertise DNS. Host share roots stay in the
user's VM configuration and private recovery journal, never in guest bootstrap
or agent API. Start fails clearly when the separately built bundle is absent
or its hashes do not match. Build it beside the development supervisor with
`python3 packaging/tools/build.py`; release packaging takes `-ToolsBundle`.

VM settings and the session approval UI expose named shares and separate
clipboard, file-transfer, UI inspection/action permissions. Real scope changes
close channels and invalidate transfers; lease renewal keeps an unchanged scope.
Priority pause/revoke, connection loss and emergency paths suspend channels.
UI actions require current-run identity, the caller's last returned tree/element,
input ownership and operation/input quota. Guest replies are validated and
bounded before IPC. Debug formatting redacts clipboard/UI text and file bytes.
Console clipboard buttons are explicit one-shot text transfers with focus and
input-owner checks. This is not automatic clipboard synchronization.

The Windows guest uses direct Win32 (freestanding i686, PE subsystem 5.1),
WebDAV/drive mapping, clipboard, UI Automation and streaming inbox/outbox files.
The Python guest uses AT-SPI, desktop clipboard utilities, WebDAV and confined
guest folders. The bundle recipe checks DLL imports and writes SHA-256 sidecars;
Windows packaging scripts now stage/install it. No Windows runtime or installer
execution is claimed. UI Automation availability on legacy Windows is optional;
unsupported calls fail. WebDAV exclusive locks are not implemented.

Host share writes have a private durable journal prepared before staging data.
On the next Tools start, recovery removes only recorded staging files, never
final names, and refuses corrupt records or links. Ordinary teardown removes
staging and journal entries. Windows directory-metadata durability under host
power loss is not qualified. Publication can still succeed before a subsequent
cleanup/flush error: an error never proves a destination is absent.

Tools transfer metadata reaches the persistent audit and session reports;
clipboard/UI values and file contents are excluded. Session report headers and
UI session metadata retain named Tools grants. Public JSON schemas are updated.
A regression exposed a host-clock jump between supervisor commands being
masked by the next mark; checks now also cover that interval. The focused
host-resume regression passed after the correction.

Verified locally during this follow-up:

- `lightvm-tools`: 22 passing tests (10 boundaries, 4 file channel, 5 control,
  3 recovery), including the 52 MiB + 17 byte round trip and the production
  Python guest over real TCP with an isolated HOME.
- `vm-core --test tools`: 3 passing tests for scope, run, input owner and quota.
- `lightvm-service --test tools`: bootstrap excludes host paths, defaults deny
  access, changed scopes close live connections and deny removed shares.
- MCP: 15 library tests and 6 stdio tests passed.
- Guest Python: 4 filesystem tests passed. Native JSON codec: deterministic
  cases plus 50,000 malformed/random inputs under ASan/UBSan passed.
- Windows guest binary cross-build/import checks passed, with no UCRT imports.
- Workspace all-targets compilation passed before the final journal wiring;
  final commit checks are recorded below. Broader regression runs are not a
  complete workspace pass: a macOS canonical `/var` alias assertion was fixed,
  and the host-resume regression was rerun after its correction.

Next work, preserving the owner's priority:

1. Finish Tools: desktop file-transfer flow (streaming, limits, progress,
   cancellation, drag/drop), USB via the selected UsbDk/usbredir design with
   explicit driver installation and verified detach, clipboard images/sync,
   guest mapping compatibility and snapshot quiescence. Verify approval failure
   when required disconnection cannot be completed.
2. Add/extend product integration and UI tests, runtime/package tests on Windows,
   WPF/AT-SPI interaction and real guest verification. Exercise a 750 MB build,
   log return, directory/rename/delete/replace, restart/rollback and journal fault
   injection. The existing real-QEMU tests remain unexecuted on this Mac.
3. Finish Windows 11 x64 (ADR-0035), including TPM process ownership and state
   through baseline/snapshot/restore, without bypassing guest requirements.
4. Implement and qualify filtered Internet (R1). NAT and the endpoint allowlist
   are separate existing modes and do not establish R1 completion.

Final checkpoint before the owner's requested commit/push: all-targets clippy
with warnings denied passed for `lightvm-tools`, `lightvm-service`,
`lightvm-proto`, `vm-core`, `qemu-backend`, `lightvm-mcp` and
`lightvm-desktop`. The product Tools integration test passed again after the
final journal wiring; `git diff --check` passed. The Windows packaging
PowerShell scripts and new focused clipboard UI were not runtime tested.
