# ADR-0003 — Console with an internal RFB client on the protected channel and screendump fallback

- Status: Accepted (initial choice; section 6.4 requires its validation in F0)
- Date: 2026-10-09
- Owner: to be assigned

## Context

The console must show the guest inside the native app without a WebView, without a WebSocket gateway and without VNC endpoints lacking client authentication. The roadmap sets the direction and the fallback (section 6.4).

## Decision

- RFB (VNC) client written in Rust, running in the UI process, which receives from QEMU only framebuffer updates and draws them as an egui texture.
- RFB subset: handshake, pixel format, Raw, CopyRect and one compressed encoding; unexpected extensions rejected. Third-party VNC dependencies only if maintained and inventoried (ADR-0023), otherwise an internal client.
- Display read-only by construction: the client does not implement sending input, clipboard or power control. User input goes through the authorized bridge via QMP commands, like agent input.
- Display credential issued by the supervisor per run and per VM, distinct from QMP and administrative IPC, revoked with the session.
- Protected channel chosen in ADR-0013 (private OS socket or VeNCrypt X.509 with a client certificate).
- **Fallback:** if no channel protection can be verified on the first host, the framebuffer is reconstructed by the backend with QMP `screendump` at a limited rate and delivered to the UI via IPC, with the same authorization and revocation checks; latency measured and declared in `docs/COMPATIBILITY.md`.
- Never a VNC endpoint without client authentication. If the fallback also fails, the target is blocked.
- Keyboard and mouse read from winit with physical scancodes and mapped to QEMU qcodes for declared layouts; keys released on focus loss, disconnection or change of input owner; no automatic reading of the host clipboard.

## Alternatives considered

- Web VNC client in a WebView: excluded (ADR-0001).
- VNC with a traditional password or bind to localhost only: excluded, they are not authorization (section 6.4).
- SPICE display or other protocols: not provided for by the roadmap.

## Consequences

- The full console path must be prototyped before stabilizing the UI (section 6.4) and before extensive construction of the dashboard (section 15).
- The fallback entails higher latency and QMP load that must be accounted for.
- The "read-only" property requires a negative test (section 7.4, "Console" row).

## Required evidence

- Console, input and screenshots on the protected channel: `docs/verification/F0-04-console-rfb-input.md` (partial: RFB console, QMP input and screenshots verified on a Unix socket; physical keyboard through winit not run). The initial choice is validated on the first host; the screendump fallback is not needed but is measured (p95 19.2 ms). Transport in ADR-0013, placement of the client in ADR-0024.
- Read-only negative test of the RFB client: `crates/rfb-client/tests/read_only.rs` (verified in simulation/test, `F0-04` §6). Negative test on the channel with another user: pending (P-5 in `docs/verification/F0-09-evidence-review.md`); full tests by F2/F4.

## References

Roadmap, sections 6.4, 7.4, 10 (F0), 14, 15.
