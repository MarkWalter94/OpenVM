# ADR-0022 — UI test framework

- Status: Accepted (2026-10-10, project owner): egui_kittest with snapshots
- Date: 2026-10-09; acceptance: 2026-10-10
- Owner: project owner

## Context

The UI test framework is chosen and tested for each target; for egui the candidate is egui_kittest, which drives the interface through the AccessKit tree and produces comparable snapshots, alongside the headless tests of the core (roadmap, section 13). A UI test with a simulated supervisor does not verify IPC, native rendering or acceleration. Automation hooks only in test artifacts (sections 10 F6 and 13).

## Decision

To be taken. Main candidate: egui_kittest, **not yet verified**.

Criteria:

1. Compatible with the egui version pinned in ADR-0014.
2. Runs on Windows and Linux, including in CI without a GPU.
3. Interaction through the AccessKit tree (indirect verification of accessibility).
4. Comparable snapshots, stable across runs.
5. No automation hook included in the public artifact.

### Decision of 2026-10-10

Accepted **egui_kittest 0.36.2** (same version as egui, ADR-0014) with graphical snapshots in `apps/desktop/tests/snapshots/` and tolerance in `apps/desktop/kittest.toml`. Evidence: `docs/verification/F1-05-desktop-egui.md` (6 snapshots, headless UI tests via AccessKit, no automation hook in the public artifact: `egui_kittest` is only a `dev-dependency`). Declared risk: the stability of snapshots across different GPUs and runners (criterion 4) is not yet proven in CI; if it fails, the snapshots are regenerated per runner or the tolerance is raised, without giving up the AccessKit tests.

## Alternatives considered

- egui_kittest (the roadmap's candidate).
- Direct tests on the egui context without a dedicated framework: less coverage of interaction.
- Automation through the operating system's accessibility API on the real app: useful for installer tests, does not replace instrumented tests.

## Consequences

- UI tests use a simulated supervisor and are labelled as such; the real paths remain covered by the physical runners.
- The release build excludes test features (verified in F6).

## Required evidence

- F0 evidence on egui_kittest 0.36.2 (compatible with ADR-0014): headless tests via the AccessKit tree on Windows (`docs/verification/F0-01-toolchain-eframe.md` §7, 3 tests; `docs/verification/F0-04-console-rfb-input.md`, tests `kittest_ui` and `real_console`) and on Linux in WSL2 without a GPU (`docs/verification/F0-07-linux-kvm.md` §2, 4 tests). Criteria 1–3 met by this evidence; criterion 4 (graphical snapshots, `snapshot` feature) and criterion 5 not yet tested.
- Dedicated verification file in F1 for snapshots and closure (name to be assigned).

## References

Roadmap, sections 10 (F0, F1, F6), 13, 17.
