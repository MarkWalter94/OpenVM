# ADR-0024 — Separate `rfb-client` crate used by the UI; `qemu-backend` stays in the supervisor

- Status: Accepted
- Date: 2026-10-09
- Owner: to be assigned

## Context

Section 6.2 of the roadmap lists `crates/qemu-backend/` with the responsibility "processes, QMP, probe, console". Section 6.4 runs the RFB client **in the UI process**, read-only, while section 6.1 assigns processes, QMP and VM files to the supervisor alone and establishes that the UI receives only what is strictly necessary and is not a trusted layer. Section 6.2 is an **initial** organization: it allows separate crates when they have stable responsibilities and dependencies, but the change must be recorded in an ADR (`F0-04`, Proposed decisions).

The `F0-04` spike produced `crates/rfb-client`: a `#![forbid(unsafe_code)]` library with RFB 3.8 handshake, VeNCrypt X.509 via rustls, ZRLE/Hextile/CopyRect/Raw/DesktopSize decoding, a session on a thread with a shared framebuffer; used by `apps/desktop`. The `read_only.rs` test verifies that the client sends only messages of type 0, 2 and 3.

## Decision

1. The RFB client lives in the library crate **`crates/rfb-client`**, used by the UI (`apps/desktop`). It does not depend on `qemu-backend`, `vm-storage`, `lightvm-service` nor on any QMP or process management code.
2. **`crates/qemu-backend` stays in the supervisor**: starting QEMU with `-vnc unix:` in the private per-run directory, creating the directory and its ACL, issuing the display credential (ADR-0013), single QMP client (ADR-0012), input and `screendump` of the authorized bridge, screendump fallback. The word "console" in section 6.2 is understood as this server side of the console.
3. The UI receives from the supervisor, through typed IPC, only the display endpoint and credential; it never receives the QMP endpoint. User input from the UI goes to the supervisor through IPC and from there to QMP (section 6.4).
4. The read-only property is verified in the `rfb-client` crate in isolation: closed set of client messages (types 0, 2, 3) with an exhaustive `match`, tests on the bytes written, no input, clipboard or power control symbols.
5. The crate's dependencies (rustls 0.23.45 with ring 0.17.14, socket2 0.6.5, uds_windows 1.2.1, flate2 1.1.10) are inventoried separately (ADR-0023).

Consistency with the roadmap: section 6.4 is respected (client in the UI process, read-only by construction, input via QMP); section 6.1 is respected better than with the client inside `qemu-backend`, because the UI does not link code capable of starting processes or speaking QMP; section 6.2 remains valid as the direction of dependencies (UI → protocol; supervisor → backend), with one more crate.

## Alternatives considered

- RFB client inside `qemu-backend` as in the literal section 6.2: the UI would depend on the supervisor's crate and would link process and QMP code, contrary to section 6.1; read-only behaviour would be harder to verify in isolation.
- Module inside `apps/desktop`: possible, but it prevents testing and inventorying the client independently of the UI and reusing it in test tools.
- Third-party VNC client: allowed only if maintained and inventoried (section 6.4); the internal client (about 2 000 lines including tests) covers the required subset and is already verified on the real guest.

## Consequences

- `docs/ARCHITECTURE.md` lists `crates/rfb-client` in the crate structure and in the direction of dependencies (`apps/desktop → rfb-client`, `apps/desktop → lightvm-proto`).
- Every change to `rfb-client` must keep `tests/read_only.rs` green (negative "Console" test, section 7.4).
- Requirements recorded for the client: stopping the session on Windows through a response provoked from the server, rejection of encodings not announced, discarding of ServerCutText, `None` accepted only on a private Unix socket.
- The spike code is a starting point: the transition to product happens in F1/F2 with the display credential issued by the supervisor.

## Required evidence

- `docs/verification/F0-04-console-rfb-input.md` Product code, Results §1–§3, §6 (verified).
- Display credential delivered via IPC and revoked: by F2 (ADR-0013).

## References

Roadmap, sections 6.1, 6.2, 6.4, 7.4, 17. Review: `docs/verification/F0-09-evidence-review.md`.
