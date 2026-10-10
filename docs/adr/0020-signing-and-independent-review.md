# ADR-0020 — Signing responsibility and independent review

- Status: Accepted (2026-10-10): **code signing and independent review not available**
- Date: 2026-10-09; decision: 2026-10-10
- Owner: project owner

## Context

F6 requires a public Windows distribution with code signing and verifiable checksums, equivalent provenance measures on the alternative Linux target and a targeted independent review of the MCP, QMP, path, console and process boundaries (roadmap, section 10 F6). Missing signing credentials block publication and do not authorize claims of a non-existent signature. The risk "hardware, signing or independent review unavailable" must be verified in F0; silence is not a passed verification (section 14).

## Decision

To be taken. To be recorded:

- who holds and safeguards the Windows signing credentials and with which type of certificate or service;
- provenance measures for Linux (signed checksums, package signing);
- who performs the independent review, with what independence from the developers and with what scope;
- where the keys are kept (never in the repository nor on runners with untrusted guests, section 13).

Criteria:

1. Availability confirmed in writing, not assumed.
2. Separation between those who develop the boundaries and those who review them.
3. Signing keys kept off the runners that execute untrusted guests.

### Decision of 2026-10-10

Statement of the project owner (action U-7): **the code is not signed** and **no independent reviewer** is available.

Consequences (roadmap section 10 F6, section 14 and the risk table):

- F0 closes on this point: availability is verified (negative) and recorded, not assumed.
- Development and testing in F1–F5 are not blocked.
- **The public release planned in F6 remains blocked** as long as Windows signing with verifiable checksums and a targeted independent review of the MCP, QMP, path, console and process boundaries are missing: the roadmap does not allow declaring a non-existent signature nor skipping the review.
- Until then artifacts are distributed only as unsigned builds for internal use, with SHA-256 checksums published alongside and the label "unsigned".
- No signing key exists, so no key is safeguarded (criterion 3 met by absence).

## Alternatives considered

- Signing: code signing certificate managed by the project, or a managed signing service; SHA-256 checksums published in any case.
- Review: external reviewer, or an internal reviewer not involved in developing the boundaries; the roadmap requires "independent" without specifying the form.

## Consequences

- If signing or review are not available, the v0.2 release remains blocked; F1–F5 development can proceed with the risk recorded in `docs/STATUS.md`.

## Required evidence

- Record of availability (document or statement) in `docs/STATUS.md`; no technical verification file required in F0.

## References

Roadmap, sections 10 (F6), 13, 14, 17.
