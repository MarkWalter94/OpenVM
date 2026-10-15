# ADR-0034 — Integration features grantable to agent sessions

- Status: Accepted (2026-10-15): direction set; evidence pending
- Date: 2026-10-15
- Owner: project owner (decision); implementation recorded below

## Context

ADR-0031 introduced shared folders, clipboard, file drag and drop and USB for manual use and disconnected them before any agent session. The real use case needs the session to receive new builds (≈750 MB per build) and to return result files (for example `apertura-*.log`). The project owner decided: *everything can be enabled through permission requests and settings*.

## Decision

Each integration is a **session capability**, off unless granted:

| Capability | Scope in the grant |
| --- | --- |
| `share.read` / `share.write` | named shared folders of the VM, each read-only or read-write |
| `clipboard` | host↔guest clipboard while the session runs |
| `files.transfer` | file transfer host→guest / guest→host (limits on size and count) |
| `usb` | named USB devices already attached by the user |
| `network.endpoints` | allowlist endpoints (ADR-0033) |

- The client requests them; the user approves or reduces them in the app like the existing capabilities; the supervisor enforces them on every operation and disconnects them on revocation, expiry, Emergency Stop or session end (the ADR-0031 disconnection path, now applied to what was not granted or is revoked).
- The VM settings decide what *can* be granted (a folder not configured on the VM cannot be requested); defaults stay off.
- Every granted capability is shown in the session indicator and recorded in the session report; transfers and folder writes are audited (names and sizes, not contents).
- Section 7.2 remains the default AI profile; these are declared, explicitly granted exceptions.

ADR-0031's "manual use only" rule is replaced by this ADR.

## Required evidence

- A session without the capability cannot use the feature (negative tests per capability); revocation and Emergency Stop disconnect within the stop budget; read-only folders not writable from the guest; report and audit list every granted capability and transfer.
- Real case: a build delivered through a read-only share and a log retrieved through a write share in an agent session. **Not verified.**

## References

Roadmap, sections 7.2, 7.3, 9, 12 (R6); ADR-0031 (amended), ADR-0033.
