# F1-04 — `lightvm-service`: authenticated local IPC, single instance, supervisor skeleton and CI

- Execution date (UTC): 2026-10-09, from about 14:30 to 15:20
- Executed by: development agent (Claude Code session), at the request of the project owner
- Roadmap step: section 10, F1 ("Supervisor skeleton and headless tests without dependency on the UI", "Windows/Linux CI; macOS build … without declaring operational support"); sections 6.1, 6.3, 7.1, 7.3, 7.4 (rows "Sessions", "IPC/console", "Lifecycle"), 9 (stdio adapter, named pipe with ACL / local socket, rate limit and bounded queues), 13 (verification pipeline)
- Related ADRs: ADR-0002, ADR-0006, ADR-0009, ADR-0015 (part B pending), ADR-0023, ADR-0026 (not modified)
- Update of 2026-10-09 (from about 15:20 to 15:50 UTC): real integration of the composition root, IPC envelope in `lightvm-proto`, minimal audit, end-to-end test with real QEMU; see the dated section at the end "Composition root integration". The previous sections remain as recorded (first run, with fake backend and catalog).
- Overall outcome (first run): **partial** — transport, handshake, single instance, revocation, rate limit, controlled shutdown and parent death detection verified on Windows and on Linux (WSL2) with a **fake** backend and catalog; rejection of a low-integrity process verified on Windows; CI workflows written and validated locally but **not run** on GitHub (no remote repository); the full CI sequence on the workspace fails today for causes external to this stream (see Results, point 6).

Allowed outcomes: **verified**, **failed**, **not executed** (section 17). Raw logs in `lab/logs/` (outside the repository): `f1-04-windows-test.txt`, `f1-04-windows-checks.txt`, `f1-04-pipe-sddl.txt`, `f1-04-linux-test.txt`, `f1-04-linux-pidfd.txt`, `f1-04-ci-windows.txt`, `f1-04-ci-linux.txt`, `f1-04-cargo-deny.txt`, `f1-04-yaml-check.txt`.

## Purpose

Bring `crates/lightvm-service` from a skeleton to a working supervisor skeleton (ROADMAP 6.1, 10 F1):

1. authenticated local IPC transport with restrictive OS permissions and verification of the peer's identity;
2. protocol with bounded framing, negotiated version, handshake with credential, stable errors;
3. single instance per catalog;
4. supervisor loop with `Catalog` and `Backend` as traits, typed dispatcher, events, connection registry with revocation, controlled shutdown, Emergency Stop on a priority path;
5. startup as a child of the UI with the initial credential outside the arguments and detection of the parent's death;
6. Windows/Linux CI, macOS portability, schemas, licences and advisories.

It does not demonstrate: real QEMU processes, Job Object, QMP or storage (the `Catalog` and `Backend` implementations are fake, section 10 F1: "synthetic tests and mocks are allowed for errors/protocol"); pairing with user approval, grants and leases (F4); MCP adapter (F5); isolation from a second local user (no second account available, as in F0-04); running the workflows on GitHub runners.

## Environment

| Item | Value |
| --- | --- |
| Host | HP ZBook Fury 16 G9, Intel Core i7-12800HX, 32 GiB RAM (as in F0-01/F0-04) |
| OS and build | Windows 11 Pro 10.0.26200; non-administrator domain user, shell at medium integrity (`S-1-16-8192`) |
| Linux | WSL2, kernel `6.18.33.1-microsoft-standard-WSL2`, Ubuntu 26.04, `XDG_RUNTIME_DIR=/run/user/1000`; build with the zig linker as in F0-07 (`lab/wsl/env.sh`), `CARGO_TARGET_DIR=lab/wsl/target` |
| Accelerator and state | not used (no QEMU in this test) |
| Toolchain | Rust 1.99.0 (`b940084d7 2026-09-28`), `x86_64-pc-windows-msvc` with the Build Tools 2022 environment (`vcvars64.bat`, F0-01 §4.1); `x86_64-unknown-linux-gnu` in WSL |
| QEMU binary / firmware / guest | not applicable |
| Graphics session | not applicable (headless tests) |
| Verification tools | cargo-deny 0.20.2 (user profile); Node 24.15.0 with js-yaml 4.1.0 (scratchpad); actionlint 1.7.7 (scratchpad, SHA-256 of the zip `7f12f1801bca3d480d67aaf7774f4c2a6359a3ca8eebe382c95c10c9704aa731` equal to the release checksums file) |
| Binaries tested | `target\debug\lightvm-service.exe` SHA-256 `f4f67bf7f7ff27311311985d9018a3251e26a7852e12dc0612a7ee711966a15f`; `target\debug\lightvm-ipc-probe.exe` SHA-256 `6c1a4187755d052f614c5c4ba2d4934c519035d7cccd7f9a36c8b4a483bcb97b` |

Dependencies added to `lightvm-service` (exact versions, no git dependencies; licences from the `cargo deny list` inventory):

| Crate | Version | Licence | Use |
| --- | --- | --- | --- |
| tokio | 1.53.2 (already in the lockfile) | MIT | async runtime, Unix sockets, channels |
| serde, serde_json | 1.0.229, 1.0.151 | MIT OR Apache-2.0 | JSON payloads |
| getrandom | 0.3.4 (already in the lockfile) | MIT OR Apache-2.0 | credentials and random names from the operating system generator |
| lightvm-proto | path | MIT | `PublicError`, `ErrorCode`, `ProtocolError`, `ReplyError` |
| interprocess (Windows only) | 2.4.4 | 0BSD OR MIT OR Apache-2.0 | async named pipe with security descriptor, SDDL conversion, client PID and session |
| sysinfo (Windows only) | 0.39.6 | MIT | SID of the user owning a process (own and peer) |
| widestring (Windows only) | 1.2.1 | MIT OR Apache-2.0 | UTF-16 SDDL string for `interprocess` |
| rustix (Unix only) | 1.1.5 (already in the lockfile) | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | `geteuid`, `pidfd_open`, `poll`, `getppid` |

No `unsafe` in the crate (`unsafe_code = "deny"` of the workspace, no exemptions): the Win32 and Linux calls are encapsulated in the crates above.

## Design

### Chosen transport and rationale

**Windows: named pipe with ACL**, not a Unix socket in a private directory.

- The named pipe makes it possible to set the security descriptor **at creation** (no window between creation and ACL) and to include the integrity label in it; the Unix socket on Windows would require a private directory with a DACL set via API (with `icacls` it is not acceptable in production) and does not offer peer credentials.
- It is obtained safely without direct `unsafe`: `interprocess` 2.4.4 exposes `SecurityDescriptor::deserialize(SDDL)` and `PipeListenerOptions::security_descriptor`, sets `FILE_FLAG_FIRST_PIPE_INSTANCE` on the first instance (if the name already exists creation fails instead of sharing the pipe) and `PIPE_REJECT_REMOTE_CLIENTS` (`accept_remote(false)`), and provides `client_process_id`/`client_session_id`.
- Descriptor applied: `O:<SID>D:P(A;;GA;;;<SID>)S:(ML;;NWNRNX;;;ME)`, where `<SID>` is the SID of the user of the service process (read with `sysinfo`, validated as `S-1-` followed only by digits and dashes to rule out injections into the SDDL). Protected DACL with a single ACE for the current user (no access for SYSTEM, Administrators, Everyone or AppContainer packages); mandatory medium integrity label with no-write-up/no-read-up/no-execute-up: a low-integrity process cannot open the pipe.
- After acceptance: the client's logon session must match the service's and the SID of the client process (from the PID reported by the kernel for the pipe) must match the service's; otherwise the connection is closed before reading any byte.
- Random pipe name (`\\.\pipe\lightvm-<128 hexadecimal bits>`): it is not a secret, the protection is the ACL plus the credential.

**Linux: Unix socket** in `$XDG_RUNTIME_DIR/lightvm` (fallback `<catalog>/.run`, or `--runtime-dir`). The directory is created with `0700` or, if it exists, it must be a directory (not a symbolic link), owned by the effective user and without permissions for group and others; the socket is set to `0600` (the directory protects the window between `bind` and `chmod`). After acceptance the peer's UID from `SO_PEERCRED` (`tokio::net::UnixStream::peer_cred`) must match the effective UID of the service. Socket path limited to 100 bytes (limit of `sun_path`) with an explicit error. The socket is removed on shutdown. The same code compiles on macOS (`getpeereid`), without declared support.

### Protocol

- **Framing:** 4 big-endian length bytes plus a JSON payload. Zero length → protocol error `malformed_message`; length beyond the limit (default 64 KiB, configurable) → `message_too_large` and close, **without reading or allocating the payload**. Reading happens in a dedicated task that delivers complete frames on a bounded channel (4 frames): no read is interrupted mid-frame and the per-connection queue is bounded.
- **Messages:** `{"type":"hello",...}` and `{"type":"request","request_id":N,"body":{"op":...}}` from the client; `hello_ok`, `response`, `error`, `event`, `closing` from the service. Every request carries a `request_id` returned in the response. Unknown operations (for example `qmp_execute`) cannot be deserialized → `malformed_message`, no effect.
- **Version:** the client offers a list (`protocol_versions`), the service picks the first supported one in order of preference (today only `1`); no common version → protocol error `unsupported_version` and close.
- **Handshake:** the first message must be `hello` within 5 s. Checks in order: OS identity (already in the transport), version, credential. Credential missing, malformed, unknown, expired or revoked → operational error `permission_denied` and close, without registering the connection and without any effect on the backend; the reasons are not distinguished towards the client. A request before `hello` → `permission_denied` and close. A second `hello` → `malformed_message`.
- **Credentials:** 32 bytes from the operating system generator (`getrandom`), transmitted in hexadecimal, compared in constant time, zeroed on drop, never in `Debug`, never in process arguments. They live only in memory: a restart of the service invalidates them all. Roles: `ui` (initial credential) and `pairing` (issued at the UI's request with a 10-minute expiry, to be delivered out of band; **no VM permissions**, only `ping` and `get_capabilities`, ADR-0006).
- **Errors:** from the single source `lightvm-proto`: `ReplyError::Operational(PublicError{code, message, retryable})` with the stable codes (`permission_denied`, `session_expired`, `vm_busy`, `unsupported`, `resource_limit`, `state_conflict`, …) and `ReplyError::Protocol(ProtocolError{code, message})` (`malformed_message`, `unsupported_version`, `message_too_large`, …), never mixed up.
- **Limits (section 9):** per-connection token-bucket rate limit (default burst 50, 100/s) → `resource_limit` without effect; shared bounded queue towards the supervisor (64 slots) with immediate `resource_limit` rejection when full; simultaneous connections limited (16) → `resource_limit` and close.
- **Double check (section 7.3):** the role is checked on receipt; the supervisor thread rechecks that the connection has not been revoked immediately before executing.

### Supervisor

- A dedicated thread owns `Box<dyn Catalog>` and `Box<dyn Backend>` (single owner, no shared lock on the VM state) and serves commands in order: `ping`, `get_capabilities`, `list_vms`, parent loss, shutdown. `subscribe` subscribes the connection to the event channel (`service_state`, `vm_state`, `connection_revoked`, `agent_sessions_stopped`, `events_lost` for slow clients).
- **Connection registry with revocation:** `list_connections`, `revoke_connection` (UI only). Revocation closes the connection with `closing{reason: revoked}`, invalidates the credential if the role is not `ui`, interrupts any pending request and publishes `connection_revoked`.
- **Emergency Stop (ADR-0009):** request `emergency_stop` (UI only) or `ServiceHandle::emergency_stop`. It first revokes all non-UI connections and credentials, then calls the backend's `EmergencyStop::emergency_stop_all` on a blocking thread: it takes no queue slots and does not go through the supervisor thread. The real backend will have to terminate the QEMU tree with OS primitives and declare `completed` only after observing its end.
- **Controlled shutdown:** `shutdown` (UI only), a `shutdown` line on stdin or `ServiceHandle::shutdown`: accepting stops; `closing{reason: service_stopping}` to all connections after delivering the events already published; the supervisor completes the operation in progress, answers `state_conflict` to queued ones, calls `stop_agent_sessions(Shutdown)` and the backend's `shutdown()`; bounded wait (30 s, after which the thread is no longer waited for); release of the lock; removal of the socket on Unix.
- **Single instance:** exclusive lock on `<catalog>/.lightvm-service.lock` with `std::fs::File::try_lock` (`LockFileEx` on Windows, `flock` on Linux), released by the operating system when the handle is closed even if the process dies. Second instance → exit code 3 and the message "un'altra istanza di lightvm-service sta già usando questo catalogo (lock: …)", no `ready` line and no credential issued.

### Startup as a child of the UI and parent death

Contract of the `lightvm-service` binary (no secrets in the arguments, section 9):

```text
lightvm-service --catalog <dir> [--runtime-dir <dir>] [--parent-watch stdin|none] [--orphan-policy shutdown|keep-manual-vms]
stdout: one line {"event":"ready","pid":…,"endpoint":"…","protocol_versions":[1],"credential":"<64 hex>"}
stdin:  lifeline pipe; EOF = parent terminated; line "shutdown" = controlled shutdown
stderr: diagnostics without secrets (writes that never panic if the pipe is closed)
exit:   0 normal, 2 wrong usage, 3 catalog already in use, 4 other startup errors
```

**Delivery of the UI credential: the child's stdout.** It is an anonymous pipe created by the UI and owned only by it (equivalent to an inherited handle, with no file on disk to protect or delete); the credential appears neither in the arguments nor in the environment. Rejected alternative: a file in a private directory (on Windows it requires a DACL via API on the directory, to be deleted reliably even after a crash).

**Detection of the parent's death:**

- Main mechanism, identical on Windows and Linux: **lifeline pipe on stdin**. When the UI terminates (even through a crash or kill) the operating system closes the only write end and the service reads EOF. It does not depend on the PID (no reuse possible) and does not require `unsafe`.
- **Linux, in addition:** `pidfd_open(getppid())` plus `poll`, as in F0-07, with the `getppid()` check repeated after opening to rule out an already reused PID. It covers the case where the write end of stdin was inherited by mistake by another process (verified below). `PR_SET_PDEATHSIG` is not used: it would send a signal instead of starting the controlled shutdown.
- **Windows:** waiting on a handle of the parent (`OpenProcess` + `WaitForSingleObject`) would require `unsafe` FFI in the crate or an inherited handle converted with `from_raw_handle` (also `unsafe`); the lifeline pipe offers the same semantics tied to the lifetime of the process. Limit: if the UI let another process inherit the write end of the service's stdin, the UI's death would not be detected while that process lives. The UI must therefore create the pipe non-inheritable towards other children (default behaviour of `std::process::Command`).
- On parent loss: revocation of non-UI connections and credentials, `Backend::stop_agent_sessions(ParentLost)`, hook `Backend::on_parent_lost(policy)` for manual VMs (**ADR-0015 part B pending**), event `agent_sessions_stopped{reason: parent_lost}`, then `OrphanPolicy`: default `shutdown` (controlled shutdown: without the UI the initial credential cannot be recovered, so today no reattachment is possible); `keep-manual-vms` leaves the service running for the future decision.

## Procedure

1. Build and lint of the crate on Windows, in the MSVC environment of F0-01 (`cmd` wrapper that calls `vcvars64.bat`, in the scratchpad):

   ```text
   cargo fmt --all --check            (verified for lightvm-service; see point 6 for the workspace)
   cargo clippy -p lightvm-service --all-targets -- -D warnings
   cargo test -p lightvm-service -- --nocapture --test-threads=4
   ```

2. Manual tests on Windows with the versioned script `crates/lightvm-service/scripts/f1-04-windows-checks.ps1` (after `cargo build -p lightvm-service --bins`):

   ```text
   powershell -NoProfile -ExecutionPolicy Bypass -File crates/lightvm-service/scripts/f1-04-windows-checks.ps1
   ```

   The script copies `lightvm-ipc-probe.exe` and `whoami.exe` into a temporary directory, marks the copies with `icacls <file> /setintegritylevel Low` (approach from F0-04), starts the service with stdin/stdout redirected, reads the `ready` line (the credential is not printed), runs the probe at low and medium integrity passing the credential **on stdin**, tries a wrong and a missing credential, a second instance, shutdown via stdin and finally the real kill of the parent (intermediate PowerShell that starts the service and is terminated with `Stop-Process -Force`).

3. Reading the pipe's descriptor from PowerShell: `[System.IO.Directory]::GetAccessControl(<endpoint>).GetSecurityDescriptorSddlForm('All')`.

4. Linux (WSL2), with `lab/wsl/env.sh` and `CARGO_TARGET_DIR=lab/wsl/target`: `cargo clippy -p lightvm-service --all-targets -- -D warnings` and three runs of `cargo test -p lightvm-service`.

5. Linux, real kill of the parent with the lifeline pipe kept open by another process: `bash -c "sleep 1000 | lightvm-service --catalog $D --runtime-dir $D/run > $D/ready.txt"` in the background, then `kill -9` of the parent `bash` (the `sleep` stays alive and keeps the service's stdin open: only the pidfd can detect the death).

6. Manual replica of the CI sequence on the whole workspace (Windows and WSL): `cargo fmt --all --check`; `cargo clippy --workspace --all-targets --locked -- -D warnings`; `cargo clippy -p lightvm-desktop --no-default-features --features glow --all-targets --locked -- -D warnings`; `cargo test --workspace --locked` (Windows) / `--no-fail-fast` (Linux); `cargo build --release --locked -p lightvm-desktop`; `cargo test --locked -p lightvm-proto --test schema_snapshot`.

7. `cargo deny check` from the root with `deny.toml`; validation of the workflows with js-yaml (syntax, `runs-on`/`steps`, actions pinned to SHA) and actionlint 1.7.7 (`-shellcheck= -pyflakes=`, tools not available on the host).

## Results

### 1. Headless tests (`cargo test -p lightvm-service`)

Real transport (named pipe on Windows, Unix socket on Linux), fake catalog and backend. 18 unit tests on Windows (17 on Linux: the transport tests differ per platform) and 20 integration tests in `crates/lightvm-service/tests/ipc.rs`.

| Integration test | What it verifies | Windows | Linux (WSL2) |
| --- | --- | --- | --- |
| `valid_handshake_and_typed_requests` | `hello_ok` with version 1 and role `ui`; `ping`, `get_capabilities`, `list_vms` | verified | verified |
| `invalid_credential_is_denied_and_closed` | random credential and malformed credential → `permission_denied`, close, no connection registered, no call to the backend | verified | verified |
| `missing_credential_and_request_before_hello_are_denied` | `hello` without credential and `emergency_stop` before `hello` → `permission_denied`, close, no Emergency Stop executed | verified | verified |
| `unsupported_protocol_version_is_rejected` | versions `[0,2,99]` → `unsupported_version`, close; `[7,1]` → 1 negotiated | verified | verified |
| `oversized_frame_is_rejected_before_and_after_handshake` | 1 MiB header before the handshake and 8 KiB frame with a 4 KiB limit after → `message_too_large`, close | verified | verified |
| `malformed_message_is_a_protocol_error_and_connection_survives` | `op: qmp_execute` → `malformed_message`; the connection remains usable | verified | verified |
| `two_clients_with_different_roles` | UI and pairing at the same time; pairing gets only `ping`; `list_vms`, `subscribe`, `emergency_stop`, `shutdown`, `create_pairing_credential`, `revoke_connection` → `permission_denied`; `list_connections` shows 2 connections with the peer's PID | verified | verified |
| `revoking_a_connection_closes_it_and_invalidates_the_credential` | revocation from the UI → `closing{revoked}` and the client closed; `connection_revoked` event to the subscriber; credential no longer accepted; revocation of a nonexistent id → `state_conflict` | verified | verified |
| `second_instance_on_same_catalog_is_rejected` | second `start` → `AlreadyRunning`; after shutdown the restart succeeds | verified | verified (see note) |
| `credential_is_not_valid_after_restart` | UI credential from the first run rejected after the restart (row "Sessions" of 7.4) | verified | verified |
| `rate_limit_per_connection` | burst 3, refill 0.5/s: 3 `pong` and 3 `resource_limit` out of 6 requests; another connection has its own bucket | verified | verified |
| `bounded_queue_and_emergency_stop_does_not_wait_for_it` | 1-slot queue occupied by a `list_vms` blocked in the catalog: `ping` → immediate `resource_limit`; `emergency_stop` completed with the supervisor blocked (785 µs on Windows), pairing connection revoked; unblocking → `list_vms` responds | verified | verified |
| `connection_limit_and_handshake_timeout` | limit of 1 connection → `resource_limit`; silent client closed at the handshake timeout (300 ms) | verified | verified |
| `controlled_shutdown_via_ipc_closes_connections_and_releases_lock` | `shutdown` → `shutting_down`; the other connection receives `closing{service_stopping}`; backend: `stop_agent_sessions:Shutdown` then `shutdown`; lock free; new connections rejected | verified | verified |
| `parent_lost_in_process_stops_agent_sessions_then_shuts_down` | `parent_lost`: backend `stop_agent_sessions:ParentLost`, `on_parent_lost:ShutdownService`; pairing revoked; event `agent_sessions_stopped{parent_lost}` delivered before the close | verified | verified |
| `binary::second_binary_instance_exits_with_clear_error` | second process on the same catalog: exit 3, clear message, empty stdout | verified | verified |
| `binary::parent_death_closes_sessions_and_releases_lock` | real binary; closing of stdin (simulated parent death): events `agent_sessions_stopped{parent_lost}` and `service_state{stopping}`, `closing{service_stopping}`, exit 0 in 26.5 ms (Windows), lock free | verified | verified |
| `binary::lock_released_when_service_is_killed` | kill of the service: the catalog lock is released | verified | verified |
| `binary::shutdown_command_on_stdin` | `shutdown` line on stdin → exit 0 | verified | verified |
| `binary::bad_arguments_exit_2_without_secrets` | unknown argument → exit 2, empty stdout | verified | verified |

Runs: Windows 3 complete series + 1 with output (all `20 passed`, log `f1-04-windows-test.txt`); Linux 3 consecutive series after the fix below (18 unit tests before the integration of the `lightvm-proto` errors, which removed a redundant local test; `20 passed` integration; log `f1-04-linux-test.txt`) plus one series after the integration within `cargo test --workspace` (17 unit, 20 integration; `f1-04-ci-linux.txt`). `cargo clippy -p lightvm-service --all-targets -- -D warnings`: no warnings on Windows and on Linux.

Note (Linux): the first run on WSL gave **failed** for `second_instance_on_same_catalog_is_rejected` (`riavvio dopo il rilascio del lock: Lock(AlreadyRunning …)`); on its own the test passed 5 times out of 5. Cause: another test, in parallel, starts child processes; between `fork` and `exec` the child briefly owns a copy of the lock descriptor (`O_CLOEXEC` closes it only at `exec`) and `flock` belongs to the shared open file description. The tests that verify the lock release now retry for at most 2 s. Consequence for the product: none, the lock is held for the whole lifetime of the service; however, a new start immediately after shutdown may see the catalog busy for an instant if the service is starting children: the UI must retry briefly on exit code 3 after a shutdown that has just finished.

### 2. Low-integrity process (Windows)

| Test | Outcome | Evidence (`lab/logs/f1-04-windows-checks.txt`) |
| --- | --- | --- |
| Effective level of the marked copy | verified | `whoami-low.exe /groups`: `Etichetta obbligatoria\Livello obbligatorio basso S-1-16-4096`; shell: `S-1-16-8192` |
| Low-integrity probe, valid credential | verified (rejected) | `uscita=10 PROBE connessione=rifiutata errore_os=Some(5) messaggio="Accesso negato. (os error 5)"`: the operating system refuses to open the pipe before any exchange |
| Medium-integrity control, valid credential | verified | `uscita=0 PROBE connessione=ok handshake=ok connection_id=1 ruolo=Ui ping=pong` |
| Medium integrity, wrong / missing credential | verified (rejected) | `uscita=11 … Operational(PublicError { code: PermissionDenied, message: "credenziale assente o non valida", … })` |
| Second instance on the same catalog | verified | `uscita=3 lightvm-service: un'altra istanza di lightvm-service sta già usando questo catalogo (lock: …\.lightvm-service.lock)` |
| Shutdown via stdin | verified | `uscita servizio=0 stderr: lightvm-service: arresto completato` |
| Real kill of the parent (`Stop-Process -Force` of the intermediate PowerShell) | verified | `servizio terminato dopo il kill del genitore: True in 42 ms`; `processi lightvm-service residui per questa prova: 0` (first run: 29 ms) |
| Effective DACL of the pipe | verified | `O:S-1-5-21-…-7907G:DUD:P(A;;FA;;;S-1-5-21-…-7907)`: a single ACE, for the current user (`f1-04-pipe-sddl.txt`). The SACL with the label cannot be read by this API without privileges: the effect is demonstrated by the low-integrity probe row |
| Second local user | **not executed** | no second account and no elevation, as in F0-04. To be run with `runas /user:<other> lightvm-ipc-probe.exe --endpoint …` |

### 3. Linux: permissions and pidfd

| Test | Outcome | Evidence (`lab/logs/f1-04-linux-pidfd.txt`) |
| --- | --- | --- |
| Directory and socket permissions | verified | `permessi directory privata: 700 socket: 600`; unit tests: `0755` directory and symbolic link rejected |
| Kill of the parent `bash` with stdin kept open by `sleep` | verified | `ppid_servizio=459` (the bash); after `kill -9`: `servizio terminato dopo il kill del genitore in 27 ms (sleep ancora vivo: si)`; stderr `processo genitore terminato`, `arresto completato`; `socket residui: 0` |
| `SO_PEERCRED` check with a different UID | **not executed** | no second user in the distro without `sudo`; the check is in the code (`transport/unix.rs`) but only the positive branch was exercised |

### 4. CI (`.github/workflows/ci.yml`, `.github/dependabot.yml`, `deny.toml`)

| Job | Runner | Content |
| --- | --- | --- |
| `build-test` | `windows-latest`, `ubuntu-latest` (matrix, `fail-fast: false`) | toolchain from the channel in `rust-toolchain.toml` (`rustup toolchain install <channel> --component rustfmt,clippy`); on Ubuntu `build-essential pkg-config` and eframe/winit runtime libraries (`libxkbcommon0 libxkbcommon-x11-0 libwayland-* libx11-* libxcursor1 libxrandr2 libxi6 libxcb1 libegl1 libgl1`); cache `Swatinem/rust-cache`; `cargo fmt --all --check`; `cargo clippy --workspace --all-targets --locked -- -D warnings`; clippy of `lightvm-desktop` with `glow`; `cargo test --workspace --locked --no-fail-fast` (the test with real QEMU stays `#[ignore]`; the kittest tests are headless, no virtual display); `cargo build --release --locked -p lightvm-desktop` |
| `macos-portability` | `macos-latest`, `continue-on-error: true` | "portability, no declared support": `cargo check --workspace --locked` |
| `schema` | `ubuntu-latest` | `cargo test --locked -p lightvm-proto --test schema_snapshot` (test created by the proto agent) and `git diff --exit-code -- crates/lightvm-proto/schema` |
| `cargo-deny` | `ubuntu-latest` | `cargo install cargo-deny --version 0.20.2 --locked`, then `cargo deny --locked check` (licences, RustSec advisories, sources, duplicates) |

Common properties: `permissions: contents: read`, no secrets, no access to the lab, `persist-credentials: false`, actions pinned to commit SHAs (`actions/checkout` v7.0.1, `Swatinem/rust-cache` v2.9.2, SHAs read with `git ls-remote`), `concurrency` with cancellation of superseded runs. No runner has an accelerator: real boot, QMP and console, Emergency Stop on QEMU, start/stop cycles of the F1 exit criterion and resource limits are not verifiable in this CI and remain for the physical runners (F0-07, "minimal CI for F1"). Dependabot: weekly `cargo` with groups `ui` (egui/eframe/wgpu/winit together, ADR-0014) and `cargo-minor-patch`, plus weekly `github-actions`.

| Local check | Outcome | Evidence |
| --- | --- | --- |
| YAML syntax and structure (js-yaml 4.1.0) | verified | `OK .github/workflows/ci.yml` (4 jobs, actions pinned to SHA), `OK .github/dependabot.yml` (`f1-04-yaml-check.txt`) |
| actionlint 1.7.7 | verified | no problems, `EXIT=0` (shellcheck and pyflakes not available, disabled) |
| `cargo deny check` | verified | `advisories ok, bans ok, licenses ok, sources ok` (non-blocking warnings for duplicate versions, from eframe/wgpu/windows-sys) (`f1-04-cargo-deny.txt`) |
| Run on GitHub | **not executed** | no remote repository (task rule); in particular the following are not tested: `rustup toolchain install` on the runners, the `LNK1104 msvcrt.lib` problem of F0-01 (expected absent on runners with full Visual Studio), the apt packages, the macOS job |

### 5. Integration with `lightvm-proto`

Integrated from `lightvm-proto`: `ErrorCode`, `PublicError`, `ProtocolError`, `ProtocolErrorCode`, `ReplyError` (re-exported from `lightvm_service::compat`). Still local in `compat` with `TODO(F1)`: IPC envelope (`ClientMessage`, `ServerMessage`, `Request`, `ResponseBody`, `ServiceEvent`), `Role`, `VmSummary`/`VmState` (in `lightvm-proto` there are `VmState` and the catalog models, to be adopted when `vm-storage` implements `Catalog`). The credential remains the local type `Credential` (in `lightvm-proto` there is `SessionToken([u8; 32])`, to be aligned with the proto agent). Difference to align: `lightvm-proto` provides the protocol code `unauthenticated` for "IPC credential missing or invalid", while section 7.4 and this task ask for `permission_denied`; the service uses `permission_denied`.

### 6. Replica of the CI sequence on the workspace (2026-10-09, ~15:00 UTC)

Windows (`f1-04-ci-windows.txt`) — outcome per step:

| Step | Outcome | Cause |
| --- | --- | --- |
| `cargo fmt --all --check` | **failed** | formatting differences in `crates/lightvm-proto`, `crates/qemu-backend`, `crates/vm-core`, `crates/vm-storage` (work in progress by other agents); none in `lightvm-service` |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | **failed** | `crates\qemu-backend\src\cmdline.rs:803:9` `collapsible_if` (work in progress by other agents) |
| clippy `lightvm-desktop` with `glow` | verified | `EXIT=0` |
| `cargo test --workspace --locked` | **failed** | `lightvm-proto` `schema_snapshot::committed_schemas_match_generated`: committed schemas different from the generated ones (work in progress by the proto agent); `cargo test` stops at the first failed binary, which is why CI uses `--no-fail-fast` |
| `cargo build --release --locked -p lightvm-desktop` | verified | `EXIT=0` |
| `schema_snapshot` | **failed** | same cause as the test |

Linux, WSL2 with the zig linker (`f1-04-ci-linux.txt`, 14:58–15:06 UTC, run a few minutes after the Windows one on the same evolving tree):

| Step | Outcome | Cause / evidence |
| --- | --- | --- |
| `cargo fmt --all --check` | **failed** | the same four crates of other agents; no differences in `lightvm-service` |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | **failed** | `crates/qemu-backend/src/paths.rs:127:9` `unused import`, `crates/qemu-backend/src/cmdline.rs:803:9` `collapsible_if` |
| clippy `lightvm-desktop` with `glow` | verified | `EXIT=0` |
| `cargo test --workspace --locked --no-fail-fast` | verified | `EXIT=0`, 40 test binaries `ok`, no `FAILED`; `lightvm-service` 17 unit and 20 integration |
| `cargo build --release --locked -p lightvm-desktop` | verified | `EXIT=0` in 169 s |
| `schema_snapshot` | verified | `3 passed` (the proto agent had meanwhile updated the schemas) |

For `lightvm-service` alone the required conditions are met on both systems: `cargo fmt -p lightvm-service --check` without differences (and no differences in this stream's files in `cargo fmt --all --check`), `cargo clippy -p lightvm-service --all-targets -- -D warnings` without warnings, `cargo test -p lightvm-service` passed.

## Limits

- Fake backend and catalog: no QEMU process, Job Object, QMP or VM files. The Emergency Stop verifies only the priority path in the service, not the termination of the process tree nor the section 13 latency.
- No second local user on Windows or on Linux: the rejection of another user relies on the ACL and the UID check, not on an executed test.
- The same guarantees do not cover a malicious process of the same user at medium integrity (section 7.1): it can open the pipe or the socket; it is stopped only by the credential, which it cannot read from the UI's private stdout short of accessing the memory of processes of the same user.
- `GA` in the ACE also grants the current user the creation of further instances of the pipe: a process of the same user could queue its own instance (squatting) and receive connections meant for the service; this is outside the threat model (same user) but must be re-examined with the MCP adapter, which will also have to authenticate the server (for example by comparing `server_process_id` with the PID announced by the UI).
- On Windows the parent's death is detected only through the lifeline pipe (see Design); the variant with an inherited parent handle was not tested.
- The tests' temporary directories (`%TEMP%\lightvm-service-tests`, `/tmp/lightvm-service-tests`) are not removed.
- On Windows the integration tests take about 2.4–3.7 s versus 0.65 s on Linux: shutdown waits up to 2 s for connections to close; not investigated further.
- In WSL the build uses the zig linker (F0-07), not `build-essential` as in CI.
- The workflows were not run on GitHub.

## Required actions

1. Publish the repository and run the CI; record the outcome per job (in particular Windows MSVC, apt on Ubuntu and macOS) in a subsequent verification file.
2. ~~Align with the proto agent: move the IPC envelope into `lightvm-proto` with a generated schema, decide `permission_denied` versus `unauthenticated` for the handshake, adopt `SessionToken` and `VmState`.~~ **Closed on 2026-10-09** (section at the end, points 1–2): envelope in `lightvm_proto::ipc` with `ipc_*.json` schemas, `unauthenticated` according to ADR-0026, proto's `VmState`/`VmId`/`RunId` adopted. `SessionToken` remains for agent sessions (F4): the channel credential remains the local type `Credential` (zeroed on drop).
3. Implement `Catalog` in `vm-storage` and `Backend`/`EmergencyStop` in `qemu-backend` (Job Object on Windows, ADR-0015 part A mechanism on Linux), then repeat the shutdown and parent loss tests with real QEMU and measure the Emergency Stop (section 13). **Partly closed on 2026-10-09** (section at the end, points 3–6): real adapters in the service and the cycle create → start → status → stop → Emergency Stop with real QEMU on Windows/WHPX, 2 runs. Remaining: parent loss with real QEMU, repeated Emergency Stop measurement (section 13), Linux.
4. Decide ADR-0015 part B and implement `on_parent_lost` accordingly; if reattaching the UI is chosen, a channel to reissue the UI credential is needed.
5. Test with a second local user (Windows: `runas`; Linux: another UID) against the pipe and the socket.
6. Real pairing (F4): code verifiable in the app, user approval, grant with expiry and lease ≤ 10 s; authentication of the server by the MCP adapter (F5) and discovery of the endpoint by the adapter.
7. Targeted fuzzing of the frame and message parser (section 13).
8. The UI that starts the service must retry briefly on exit code 3 right after a shutdown (Linux note of point 1).
9. Decide who holds the guard handles of the run's files (`qemu_backend::StorageGuards`, today used through `QemuRun::start`, or `vm_storage::Catalog::guard_run`) when disk and baseline arrive (F1-09, F1-12).
10. Fill in the `vm-storage` and `qemu-backend` APIs the gaps of the section at the end (point 7), then slim down the adapters: creation of the primary disk, `insert_vm` with the core's identifier, `&self` reads of the catalog, explicit ACL of the private socket directory.

## Proposed decisions

- **ADR-0002 / section 9 (transport):** on Windows a named pipe with a DACL limited to the user's SID, medium integrity label `NW/NR/NX`, remote clients rejected and verification of the peer's session and SID, through `interprocess`/`sysinfo` without `unsafe` in the crate; on Unix a socket in a `0700` directory with verification of the peer's UID.
- **Supervisor startup:** initial UI credential on the child's stdout, lifeline pipe on stdin as the common mechanism to detect the parent's death, pidfd in addition on Linux.
- **ADR-0023:** adopt `cargo-deny` with `deny.toml` (permissive licences, exception for the `epaint_default_fonts` fonts, crates.io sources only, advisories without exceptions) and weekly grouped Dependabot.

## Files produced

- `crates/lightvm-service/Cargo.toml`, `src/{lib,main,frame,credential,ratelimit,lock,server,supervisor,client,parent,test_support,engine,fakes}.rs`, `src/adapters/{mod,catalog,backend,audit}.rs` (`src/compat.rs` removed on 2026-10-09), `src/transport/{mod,windows,unix}.rs`, `src/bin/lightvm-ipc-probe.rs`, `tests/ipc.rs`, `tests/core_path.rs`, `scripts/f1-04-windows-checks.ps1`
- `crates/lightvm-proto/src/ipc.rs`, `crates/lightvm-proto/schema/ipc_client_message.json`, `crates/lightvm-proto/schema/ipc_server_message.json`; in `crates/lightvm-proto/src/{lib,schema}.rs` only `pub mod ipc;` and the addition of the IPC schemas to the list
- `.github/workflows/ci.yml`, `.github/dependabot.yml`, `deny.toml`
- `docs/verification/F1-04-service-ipc-ci.md` (this file)
- `lab/logs/f1-04-*` (outside the repository)

## Composition root integration (2026-10-09, from about 15:20 to 15:50 UTC)

Section appended at the end (rule 8 of `docs/verification/README.md`): the previous sections remain as recorded.

### Purpose

Close required action 2 and, in part, 3: IPC envelope with a single source in `lightvm-proto`; `unauthenticated`/`permission_denied` semantics (ADR-0026); `vm_core::Core` in the service with the `vm-storage` SQLite catalog, the `qemu-backend` backend, system clock and identifiers and file audit (F1-10); headless tests of the real path and an end-to-end test without the UI with real QEMU (F1-11).

It does not demonstrate: parent loss with real QEMU; repeated cycles of the F1 exit criterion (F1-12); disk, ISO or boot of a guest operating system (the VM stays in the UEFI firmware); statistical measurement of the Emergency Stop (section 13); Linux; run on GitHub.

### Environment

As above (HP ZBook Fury 16 G9, Windows 11 Pro 10.0.26200, non-administrator user, Rust 1.99.0 MSVC with `vcvars64.bat`), plus:

| Item | Value |
| --- | --- |
| Accelerator and state | WHPX, verified by the `qemu-backend` probe (`query-accelerators`) |
| QEMU binary | `lab/qemu/qemu-11.1.0/qemu-system-x86_64.exe` 11.1.0, SHA-256 `47d57a6072e0bb3bd98f87926eb129eb1736dfe818c67b3b81ef7ce4edd0b3cd`; `qemu-img.exe` SHA-256 `9d930582d0d574bb106d89982c547688e10b8f356192d302cc3d0ae4e8674534` (equal to the hashes approved in `qemu_backend::probe`) |
| Firmware | `share/edk2-x86_64-code.fd` SHA-256 `33090cc07675baa5190d9f1e84bf5176b33bcbfa9bacac522961150cdb6dbb2a`; variables template `share/edk2-i386-vars.fd` copied per run |
| Guest / media | none: VM without disk or ISO, `-cpu qemu64`, 1 vCPU, 1024 MiB |
| Graphics session | headless (no UI, `-display none`) |

### Design

1. **IPC envelope in `lightvm_proto::ipc`.** `ClientMessage`, `ServerMessage`, `Request`, `ResponseBody`, `ServiceEvent`, `Role`, `VmSummary`, `Capabilities`, `ConnectionInfo`, `EmergencyStopReport`, `ServiceState`, `StopReason`, `CloseReason`, `SUPPORTED_PROTOCOL_VERSIONS`. Reused from the proto: `VmId`, `RunId`, `VmName`, `VmState`, `VmConfig`, `VmRecord`, `VmRequest`, `Admission`, `OperationRecord`, `ReplyError`. New requests: `vm_status`, `create_vm`, `submit_vm` (response after the backend's outcome). Generated schemas `schema/ipc_client_message.json` and `schema/ipc_server_message.json`, included in the list of `schema::public_schemas` and therefore in the `schema_snapshot` test; the channel credential is `writeOnly` in the schema and remains free text, so that a malformed credential has the same outcome as a wrong one. `crates/lightvm-service/src/compat.rs` was removed.
2. **Errors (ADR-0026).** Credential missing, malformed, unknown, revoked or from a previous start, request before `hello` and revoked connection with a queued request → `unauthenticated`; `pairing` role on operations reserved to the UI → `permission_denied`. No provisional mapping.
3. **Composition root.** `main.rs` composes before the tokio runtime: `SqliteStore::open(--catalog)` (lock and migrations of `vm-storage`; catalog already in use → exit 3 with the same message), `FileAuditSink` (`--audit-log`; default `%LOCALAPPDATA%\LightVM\audit.jsonl` on Windows, `$XDG_STATE_HOME/lightvm/audit.jsonl` on Unix), `ManagedMediaInspector`; with `--qemu-dir` the `qemu-backend` probe (binary hashes, versioned q35 machine, CPU `qemu64`, real accelerator) and `CoreEngine` with `QemuVmBackend`; without `--qemu-dir` `CoreEngine` with `UnavailableBackend` and host capabilities without an accelerator (`vm.start` → `unsupported`, with audit). The supervisor thread owns `Box<dyn VmEngine>` and `Box<dyn Backend>`; `submit_vm` runs `Core::submit` and then `Core::execute_next` until the queue is empty.
4. **Emergency Stop.** Priority path unchanged (revocation of non-UI, termination outside the supervisor) plus: an Emergency Stop generation that makes mutations already queued towards the supervisor be discarded (`state_conflict`); shared registry of the `qemu_backend::EmergencyStop` handlers (Job Object) for the real termination; after the termination the `EmergencyReconcile` command records the stop in the core with `Core::emergency_stop_with` (audit `accepted` and `succeeded`, process end confirmed), without the response to the client waiting for it.
5. **Controlled shutdown of the service.** Before the closes: `stop_agent_sessions(Shutdown)`, stop of the running VMs through the core (`stop`/`force_stop`, with audit), then `Backend::shutdown` (terminates any unconfirmed runs).
6. **Parent loss: behaviour unchanged** (ADR-0015 part B not decided): revocation of non-UI, `stop_agent_sessions(ParentLost)`, hook `on_parent_lost(policy)` without its own actions in the QEMU host, then with the default policy `shutdown` the controlled shutdown of point 5, which also stops the manual VMs; `keep-manual-vms` leaves service and VMs running. The decision is up to the project owner.
7. **Gaps in other APIs covered in the adapters** (`src/adapters/`):
   - `vm-storage`: `Tx::insert_vm(&NewVm) -> VmId` generates the identifier, while the core chooses it with `IdGenerator`: the core's `VmRecord` is kept in `config_json` and the adapter keeps the mapping core identifier → storage identifier; `Catalog::read` requires `&mut self` while the reads of `CatalogStore` are `&self` (catalog in a `RefCell`); `OperationRecord` does not keep `session_id` and `set_operation_state` does not update `run_id`; `end_run` uses the storage's instant instead of `ended_at`; `MediaRecord` does not keep the OS identity of the file.
   - `qemu-backend`: no creation of the primary disk (`qemu-img create`), so VMs are started without a disk; run directory and private socket directory created by the service with an inherited ACL; per-run copy of the UEFI variables done by the service; screenshots (`CapturedScreenshot`) and input not wired (host capabilities declared absent, the core does not forward them); no API to probe memory and free space (memory read with `sysinfo` or `/proc/meminfo`, free space declared as 256 GiB in `QemuSetup`).
   - Process exits: `QemuRun::observation()` is used; exits caused by the Emergency Stop are left to the `force_stop` confirmation in the core.
   - File guard handles: those of `qemu_backend::StorageGuards` inside `QemuRun::start` (today without a disk: empty list), not `vm_storage::Catalog::guard_run`; open decision (action 9).

### Procedure

```text
cargo fmt -p lightvm-service -p lightvm-proto -- --check
cargo clippy -p lightvm-service -p lightvm-proto --all-targets -- -D warnings
cargo test -p lightvm-service -p lightvm-proto            (3 consecutive series, plus one with a log)
cargo check --workspace --all-targets
set LIGHTVM_E2E_QEMU_DIR=E:\OpenVM\lab\qemu\qemu-11.1.0
cargo test -p lightvm-service --test core_path -- --ignored --nocapture   (2 runs)
```

Schemas regenerated with `LIGHTVM_UPDATE_SCHEMAS=1 cargo test -p lightvm-proto --test schema_snapshot`, which rewrites all the files in `schema/`: the non-IPC files turn out to be those generated from the proto agent's current source.

### Results

| Test | Outcome | Evidence |
| --- | --- | --- |
| `cargo fmt … --check`, `cargo clippy … -D warnings` | verified | no differences, no warnings |
| `cargo test -p lightvm-service -p lightvm-proto` | verified | green series (`lab/logs/f1-04-integrazione-test.txt`): proto 4 unit (tests of `ipc`), 6 `error_codes`, 18 `roundtrip`, 3 `schema_snapshot`; service 15 unit, 20 `ipc.rs`, 3 `core_path.rs` (+1 ignored) |
| `cargo check --workspace --all-targets` | verified | no errors, at 15:44 UTC |
| `tests/ipc.rs` (20 tests; real core with in-memory catalog and audit, fake backend and host) | verified | the same 20 tests as the first run; assertions updated to `unauthenticated` for a wrong, malformed, missing or revoked credential, after a restart and for a request before `hello`; the `pairing` role remains `permission_denied` |
| Race in the tests | failed, then verified after a fix | in one series `revoking_a_connection_closes_it_and_invalidates_the_credential` failed: the subscriber received `service_state{running}`, published by the supervisor thread after the `subscribe`. The initial event is now published before the thread starts; subsequent series green |
| `core_path::create_start_stop_emergency_and_denials_are_persisted_and_audited` | verified | IPC → core → SQLite in a temporary directory → file audit, fake backend: creation, creation denial (`invalid_argument`), start, state `running`, stop, stop denial with the VM off (`state_conflict`), Emergency Stop with final state `stopped`; `vm_state` events; audit rows `vm_create` `succeeded`/`denied`, `vm_start` and `vm_stop` `accepted`/`forwarded`/`succeeded`, `vm_stop` `denied`, `emergency_stop` `accepted`/`succeeded`, increasing `seq`, credential absent from the file; after the restart the VM is read from SQLite (off) and the controlled shutdown of the service stops the started VM, with audit |
| `core_path::without_qemu_start_is_unsupported_and_audited` | verified | without QEMU: `vm.start` → `unsupported`, VM `stopped`, audit `vm_start` `denied` `unsupported` |
| `core_path::pairing_role_cannot_touch_vms` | verified | `create_vm` and `vm_status` from the `pairing` role → `permission_denied`, no call to the backend |
| End-to-end without UI with real QEMU (`core_path::e2e_real_qemu_create_start_status_stop`, `#[ignore]`) | verified (2 runs) | `lab/logs/f1-04-e2e-qemu.txt` (second run): probe 2.4 s (`accelerator_available: true`, backend `qemu 11.1.0`, accelerator `whpx`); handshake; `create_vm`; start completed in 378 ms (first run 527 ms) with process alive, QMP ready, accelerator and absence of NIC verified by `qemu-backend`; state `running` also after 2 s; stop in 3.18 s (3.26 s) with escalation `system_powerdown` (3 s, the firmware does not respond) → `quit`; state `stopped`; second start and Emergency Stop: response in 56 ms (148 ms), `processes_terminated: 1`, `completed: true`, then state `stopped`; 20 audit rows with `vm_start`, `vm_stop` and `emergency_stop` `succeeded`. No QEMU process left over from this test at the end |
| Parent loss with real QEMU | **not executed** | — |
| Linux (WSL2) for this section | **not executed** | — |
| CI on GitHub | **not executed** | no remote repository; no new job (the real test is `#[ignore]`, the existing CI runs the others) |

### Limits

- A single machine and two real runs: no statistics; the times given are single samples.
- The real VM has no disk nor operating system: the ordinary stop always goes through `quit` after the `system_powerdown` timeout (3 s in the test; 60 s default in `QemuSetup`).
- The Emergency Stop is recorded in the core after the termination: if the supervisor is busy, reads already queued see the previous state until the reconciliation.
- IPC role denials (`permission_denied` for `pairing`) happen in the service and do not go through the core: they do not appear in the audit.
- The audit `seq` restarts from 1 at every supervisor start in the same file.
- Private QEMU socket directory with an inherited ACL (root `--runtime-dir` or `<catalog>/p`).


## Update of 2026-10-10 (evening): general review of the supervisor and core code

Section appended at the end (rule 8). Read-only review of `lightvm-proto`, `lightvm-service`, `qemu-backend`, `vm-core`, `vm-storage`, `rfb-client`, `lightvm-mcp`; no problems found in the QEMU argument grammar, Job Object, pipe ACL, credentials, frame limits, catalog paths, RFB decoder. Problems found and fixed:

| Problem | Fix | Test |
| --- | --- | --- |
| Exit of a VM not requested (power-off from the guest, crash) published only at a client's next request | the supervisor observes the backend every second without commands and publishes `vm_state` | `core_path.rs`: `unrequested_exit_is_published_without_requests` |
| Single shutdown budget for the service (30 s) with VMs stopped one after another: forced termination from the second VM on | budget for each VM with a process | existing tests; not tested with several real VMs |
| Existing disk rejected at start and on edit if free space drops below its size | free space limits only creation and growth | `scenarios.rs`: `existing_disk_survives_less_free_space` |
| Catalog error during forwarding: operation lost and VM stuck in `starting` | operation discarded with the error and previous state restored; at the end of forwarding the state follows the backend's outcome | existing tests (no fault injection in the fake catalog) |
| Response over 64 KiB (many VMs) closed the connection without an error | `resource_limit` error for that request | — |
| Run that exited during the termination of the shutdown escalation never released (subsequent start rejected) | released if the process has exited, at shutdown and before a new start | — |
| Private socket directory never removed | removed on release and on a failed start; `runs/<run_id>/` stays for diagnostics (outside the storage budget, documented) | — |
| Unreadable catalog row: skipped on open but fatal in the listing (service cannot start) | skipped with diagnostics in the listing too | — |
| Automatic Emergency Stop on UI loss recorded in the audit as a user action | actor `system` | existing tests |
| Expired or revoked credentials never removed | removed at every issuance | existing tests |
| Header of `adapters/backend.rs` outdated (disk not created) | updated | — |

Left as is, with a comment in the code: the core's index of `operation_id`s does not shrink, so an id that has left the idempotency window cannot be reused for another request.
