# ADR-0013 — Protected console transport and fallback

- Status: Accepted (Windows); Linux part deferred to R2a with ADR-0016
- Date: 2026-10-09
- Owner: to be assigned

## Context

ADR-0003 sets the internal RFB client and the screendump fallback. The UI–QEMU channel for the display remains to be chosen. Compatibility of this path on Windows is an F0 requirement (roadmap, section 6.4). Binding to localhost alone and the traditional VNC password are not sufficient.

## Decision

On Windows with the build from ADR-0010, in the roadmap's order of preference:

1. **Primary: VNC over a Unix socket** (`-vnc unix:<private_dir>/vnc.sock`) in the per-run private directory created by the supervisor (same ACL rule as ADR-0012). On the private socket the client accepts the `None` security type only because the channel is a Unix socket; over TCP it rejects it.
2. **Documented alternative: loopback with VeNCrypt X.509** (`tls-creds-x509` with `verify-peer=on`, X509None subtype) and a client certificate managed with rustls. It is not the default because of two observed limitations: shutting down a TLS session does not interrupt the blocking read (the thread is abandoned after 2 s) and QEMU leaves connections in `CloseWait` for tens of seconds, including those with a rejected certificate. Before using it, a test with `connections=` and supervisor timeouts is needed.
3. **`screendump` fallback**: confirmed feasible by the measurements (end-to-end capture p95 19.2 ms at 1280×800, 120 samples, 0 errors). It is not needed on the first host; it remains the degraded mode if the RFB channel is not available.

The client uses `socket2` for AF_UNIX on Windows; session shutdown on a Unix socket with a single shared handle, blocking reads and a 1×1 update request to provoke a response from the server (measured stop 93–94 ms).

**Per-run display credential (section 6.4): implemented in F2 on Windows (`F2-01`).** The supervisor exposes a per-run display socket in a user-only directory and forwards to QEMU's `vnc.sock` only after the client presents a 32-byte ticket obtained over the authenticated IPC (`open_console`, UI role only); the ticket is distinct from QMP and from the IPC credential, valid for one run and revoked when the run ends, on Emergency Stop and when the UI connection is revoked. Second Windows user not tested (P-5). Original F0 text follows. **Obligation as written in F0:** In F0 the protection tested is the private directory's ACL and the rejection of a low-integrity process. With a Unix socket and the UI and supervisor running as the same user, the ACL does not distinguish the display credential from the QMP one: today the distinction comes from the fact that only the supervisor knows and occupies the QMP socket (ADR-0012). By F2 the supervisor must issue, for each run and VM, a display credential distinct from QMP and IPC, delivered to the UI and revoked with the session, with evidence. Candidates to test: a display directory separate from the QMP one with an integrity label that allows access to a UI running at reduced integrity; or VeNCrypt with a client certificate issued per run, after the shutdown problem is solved. If no candidate passes the test, this ADR reverts to Proposed.

Criteria check:

- A local process without a credential does not obtain the framebuffer: satisfied for a low-integrity process (Unix socket) and for a client with a certificate from a foreign CA (VeNCrypt); second user not tested (`F0-09`, P-5).
- Per-run, distinct, revocable credential: **not satisfied in F0** (obligation above).
- Operation with the build from ADR-0010 on the host: satisfied for both transports.
- Fallback: capture latency and load measured with the method from section 13; delivery via IPC does not exist yet.

## Alternatives considered

- Named pipe with ACLs for the display: the build does not offer VNC over a named pipe; not tested.
- VNC over TCP without TLS: QEMU offers the `None` type, no authentication; excluded and rejected by the client.
- VNC with a password, WebSocket gateway: excluded (section 6.4).

## Consequences

- `qemu-backend` starts QEMU with `-vnc unix:` in the private directory and delivers to the UI only the display socket path (never the QMP one).
- The Windows row of `docs/COMPATIBILITY.md` records the "RFB over private Unix socket" mode.
- With VeNCrypt: per-run CA, issuance and revocation of certificates by the supervisor, private keys only in the private directory.
- The choice may differ on Linux; the Ubuntu row remains "not verified".

## Required evidence

- Console, transports, negative tests and measurements: `docs/verification/F0-04-console-rfb-input.md` Results §1, §5, §6, §7 (verified with the stated limitations).
- VNC endpoint over a Unix socket and VeNCrypt with real QEMU: `docs/verification/F0-02-qemu-whpx-qmp.md` §6 (verified).
- Per-run display credential and revocation: `docs/verification/F2-01-service-console-media.md` (verified on hardware, Windows).
- Second user: pending P-5 in `docs/verification/F0-09-evidence-review.md`.

## References

Roadmap, sections 6.4, 7.1, 7.2 (point 6), 7.4, 10 (F0, F2), 13, 14, 17.
