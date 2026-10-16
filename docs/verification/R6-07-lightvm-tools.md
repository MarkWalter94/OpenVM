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

## 2026-10-16 desktop file-transfer follow-up (Windows)

The desktop now connects the existing per-run file RPC to the console.
`apps/desktop/src/transfers.rs` streams files on a worker using bounded
16 KiB chunks; file handles, hashing and host writes stay off the UI thread.
Only file names and chunks reach the supervisor/guest, never the selected
host paths. The shared Tools reply slot is reserved for one operation at a
time, so clipboard replies cannot be consumed as file data. The app polls
the worker even while another page is open.

- Send a file using a native picker (Windows/macOS), or drop up to 16 local
  files on the console. Uploads go to the guest Inbox and carry a SHA-256
  checked by the existing guest component before commit.
- Refresh the root of the guest Outbox, choose a regular file and receive
  it using a native save dialog. Downloads check chunk bounds, offsets,
  EOF and the listed length. The protocol has no checksum manifest or
  stable read handle for downloads; same-size guest edits are not detected.
- A 4 GiB per-file limit, progress and cancellation apply. Upload failure
  attempts guest cancellation. Downloads use a create-only sibling staging
  file, flush it and publish with a create-only hard link. Existing targets
  are preserved and local staging files are removed on handled failures.
  Filesystems without hard-link support fail publication without replacing
  a target. Sudden host/app death can leave a local staging file; startup
  recovery for user-selected download directories is not implemented.
- Run changes, disabled permissions, supervisor disconnection and agent
  input ownership cancel work. Queued non-cleanup requests are discarded
  after cancellation; every dispatched RPC retains the original VM/run.
  A completion interrupted after publication can leave a completed file;
  the UI tells the user to check before retrying.

Verification uses real host files and a simulated supervisor/guest, not a
real guest installation. Eight worker tests cover bounded streaming and
hashing, source mutation, cancellation, malformed/short data, zero-byte
files, name/size limits, cleanup and preservation of existing destinations.
Six headless UI tests cover disabled controls, picker upload, background
navigation, Outbox receive, host-to-guest drop, cancellation and run changes;
a seventh test renders the new `console_files` visual snapshot.

Verified on Windows with the MSVC build environment loaded:

```text
cargo fmt --all --check
cargo clippy -p lightvm-desktop --all-targets --locked -- -D warnings
cargo test -p lightvm-desktop --locked --lib --test transfers_ui --test console_ui --test agents_ui --test vm_settings_ui
git diff --check
```

All passed: 110 library tests, 7 transfer UI/snapshot tests, 13 console UI
tests, 10 agent UI tests and 8 VM-settings UI tests (148 total). The new
snapshot was rendered and visually inspected. This is a focused desktop
regression run, not a complete workspace or real-guest qualification.

Remaining priority: qualify the transfer on real Windows/Linux guests and
with a 750 MB build/log return; implement guest-to-host drag-out, USB with
UsbDk/usbredir, clipboard images/sync, mapping compatibility and snapshot
quiescence. Windows 11 x64 and filtered Internet follow completed Tools,
in that order. Runtime/package tests and the other outstanding evidence in
the 2026-10-15 handoff remain open.

## 2026-10-16 real guests, images, synchronization and snapshot flush

### Storage and reproducibility

At the owner's request, all generated build/test output uses
`E:\OpenVM-tests`: `target` and `lab/tools-real` in the checkout are junctions
to that drive; test `TEMP`/`TMP` point to its `tmp` folder. The completed Windows
ISO download was moved from Downloads to
`E:\OpenVM-tests\media\Windows11_Client_x64_it-it_26300_9457.iso`.
Existing QEMU and Ubuntu media were used read-only. No installed VM or host
USB device was modified. Tools real tests are explicit ignored tests in
`crates/lightvm-service/tests/tools_real.rs`, gated by `supervisor`.

### Real Ubuntu live/WHPX

`linux_tools_large_transfer_and_revocation` passed in 498.83 seconds with the
production ToolsRun, authenticated channels, production Python guest and
isolated guest network. No external network or port forwarding was enabled.
The test boots the approved Ubuntu live ISO with its extracted kernel/initrd;
the generated Tools ISO is mounted read-only. The payload is deterministic,
synthetic data, **not an actual application build**.

```text
bytes=750000000
upload_seconds=216.116
download_seconds=221.186
sha256=34f7d1a05e3630ed40ee76a9467877059d68eef0180487a1214341b5b58658e5
log_return=passed
revocation=passed
partial_upload_cleanup=passed
scope_reconnect=passed
flush=passed
webdav=passed
```

Artifacts: `E:\OpenVM-tests\lab\tools-real\01a1440b-a54c-758f-ba8d-eeee349629dd\`
contains `results.txt` and WebDAV probe output. The probe exercises directories,
file read/write/replace/rename/delete and rejects writes to a read-only share,
unknown shares and traversal. This tests the HTTP compatibility layer, not
every desktop redirector or drive mapping implementation.

The real test exposed Windows teardown problems in the host channel: closing a
cloned TCP socket did not reliably interrupt a blocked read, and the idle
control loop did not release the old connection after scope revocation. Both
channels now use nonblocking, deadline-bound cancellable I/O with a connection
epoch. The file channel keeps the original generation permit for its entire
connection, preventing an old connection from adopting new permissions.
Idle reconnect, partial handshake, partial frame and revoke regressions pass.

### Disposable Windows PE x64/WHPX

`windows_pe_tools_native_clipboard_and_file_channel` passed in 10.26 seconds
after extension to image streaming and hashed upload. It boots the user's
Windows 11 x64 setup ISO with a generated 64 MiB answer medium; there is no
installation disk, partitioning, product key or requirement bypass. Its
unattend file only starts the probe in the Windows PE phase. QEMU is killed
and waited on at test exit. Artifacts are in
`E:\OpenVM-tests\lab\tools-real\winpe-01a1442e-d05c-7344-abc5-772cf4ce12c0\`.

```text
lab_only_x64_tools=true
installed_windows=false
native_clipboard_text_and_rgba=passed
clipboard_image_stream_round_trip=passed
file_upload_sha256_commit=passed
file_channel_log_return=passed
```

The x64 Tools build is a **lab-only build of the production C source**, because
x64 WinPE cannot execute the shipped i686 binary. The native test uses a
create-only, noninteractive, current-user-private window station; it does not
access the host's interactive clipboard. It tests native text, RGBA/alpha and
invalid dimensions. The protocol test then streams a 128×64 RGBA image to
the guest, commits with SHA-256, captures it through an immutable token and
checks every returned byte. A 256 KiB file is uploaded through SHA-256 commit,
and the native test log is returned through the file channel.

The tests found and fixed native clipboard ownership: SetClipboardData needs
an owner window; Tools now opens the clipboard with a private message-only
window. The shipped i686/subsystem-5.1 build and allowed DLL import checks
pass, but **installed Windows and the i686 guest runtime remain unqualified**.

### Desktop and protocol behavior

- Explicit image copies use the `clipboard` capability independently of
  `files.transfer`. The protocol streams raw `LIMG`/RGBA in 16 KiB chunks,
  bounded to 4,194,304 pixels, 8192 per side and 16 MiB + 12 bytes. Uploads
  require exact length and SHA-256. Capture/upload tokens are per connection;
  disconnect clears them. Linux PNG conversion requires Pillow plus a working
  graphical-session wl-clipboard/xclip backend; real Linux image clipboard
  interoperability is not yet qualified.
- Text synchronization is off by default, requires a fresh opt-in per run,
  and runs only with console capture, viewport focus, user input ownership
  and permission. It shares the Tools reply slot with transfers. Focus loss
  discards pending guest text; a newer host copy wins over a delayed reply.
  Tests inject a fake clipboard, so they do not read the host clipboard.
- Windows native drag-out uses **Prepare drag…** on one Outbox entry. The
  worker downloads it to a private cache and exposes only that file through
  a Windows shell data object and OLE DoDragDrop in COPY mode. The cache is
  removed on explicit clear or app exit; keep LightVM open until the target
  finishes copying. This is an Outbox UI workflow, not interception of a drag
  inside guest Explorer. The prepared-row/cache/cleanup flow passes a headless
  UI test; an actual drop into host Explorer is still unverified. Other host
  platforms retain Receive.
- The initial third-party `drag` dependency pulled an unmaintained transitive
  package (`RUSTSEC-2024-0370`). It was removed before completion. The final
  implementation uses the existing Windows bindings 0.62.2, with no advisory
  exception. `cargo deny --locked check` passes advisories, bans, licences and
  sources (duplicate versions remain warnings).

### Snapshot flush

Before a running disk snapshot, configured Tools must acknowledge an internal
flush RPC. Active guest transfers, missing/disconnected Tools, failed flush or
an unrelated reply refuse the snapshot. The internal maintenance permit
requires a live generation but grants no file or clipboard access. Python
uses `os.sync`; native Windows flushes fixed volumes and requires elevation
inside the guest. Ubuntu live verifies both a successful flush and refusal
during an active transfer. This is **not filesystem freeze, VSS or application
quiescence**; writes can resume after the acknowledgement. Installed-guest
snapshot/restart/rollback qualification remains open.

### Focused automated checks

Windows/MSVC: 119 desktop library tests, 8 transfer UI/snapshot tests, 13 console
UI tests, 10 agent UI tests and 8 VM-settings UI tests passed (158 total).
The updated console snapshot was visually inspected. Tools: 8 boundary,
5 file-channel (including 52 MiB round trip), 8 control and 2 journal tests
passed. Core Tools scope tests (3), service bootstrap/scope test (1), protocol
tests (44 including schema checks and image boundary negatives), MCP library
and stdio tests (15 + 6), and Python guest tests (8) passed. All-targets clippy with warnings denied passes for
desktop, Tools and supervisor. The guest bundle build/import checks pass.
These checks do not establish complete workspace or installed-guest release
qualification.

### Remaining work

1. USB capture/redirection, explicit UsbDk installation UX and verified detach,
   including rejection of session approval when detach fails. No driver was
   installed and no host device was captured. A physical test needs an
   explicitly selected disposable device; E: is excluded from capture.
2. Installed Windows/Linux drive mapping, restart/rollback and a representative
   application build/log workflow; the existing tests use Ubuntu live and
   disposable Windows PE. Installer/package execution, external native drag,
   Linux image clipboard and WPF UI Automation remain to be qualified.
3. Windows 11 x64 TPM/Secure Boot integration and filtered Internet remain
   subsequent work, in the owner's stated order.

Owner steering afterwards (2026-10-16): physical USB testing is deferred;
Windows 11 TPM took priority. Product integration and Windows PE evidence
(TPM 2.0, Secure Boot, NV persistence and helper-failure shutdown) are now
recorded in `R6-05` §13. Installed Windows, warm reboot and TPM-aware
baseline/snapshot restore remain open; filtered Internet has not started.
