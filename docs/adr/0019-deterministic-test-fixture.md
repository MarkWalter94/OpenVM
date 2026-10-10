# ADR-0019 — Deterministic test fixture

- Status: Accepted (definition); implementation by F3 (roadmap, section 9)
- Date: 2026-10-09
- Owner: to be assigned

## Context

In F0 a deterministic graphical fixture must be defined that actually runs in the guest: application, sequence of actions, verifiable expected result; not a QEMU mock. The fixture is implemented and distributed via the approved data medium by F3, before the bridge and rollback regression tests (roadmap, section 9). The F5 exit criterion requires ten runs with restore and no infrastructure errors.

The definition uses only what `F0-04` and `F0-05` have shown to work in the reference guest (ADR-0011) in a live session, without installation: opening the terminal with Ctrl+Alt+T, typing with the US layout, an absolute click in the welcome window with an observable language change, the live session's passwordless `sudo` and the guest writing to the `virtio-blk` disk attached to the overlay.

## Decision

### Environment and initial state

- Guest: the ISO of ADR-0011 (SHA-256 `601e30fb…1bda1f`) booted in a live session; VM profile of ADR-0011 (`pc-q35-11.1`, `-cpu qemu64`, 2 vCPUs, 4 GiB, `-nodefaults -nic none`, `virtio-vga`, `virtio-tablet-pci`, `virtio-keyboard-pci`).
- Fixture baseline: **empty** (all zeros) data disk `base.qcow2` attached as `virtio-blk` (`/dev/vda` in the guest) and a copy of the OVMF vars, per ADR-0005. Each run uses a new overlay and a new copy of the vars.
- Approved data medium (ADR-0008) with volume label `LIGHTVM_FX` containing the `fx.sh` script described below. Until the data medium details are settled, the same instructions are typed directly into the terminal (typed variant, the one demonstrated in `F0-04`).
- Applications used: the live session's "Welcome to Ubuntu" window, GNOME Terminal, `bash`, `sudo`, `head`, the guest's `dd`. No network, no guest agent.

### Fixture script (`fx.sh`)

Only ASCII characters that can be typed with the declared US layout. Behaviour:

- `fx.sh mostra`: hides the cursor (`\033[?25l`), clears the screen and prints, with 24-bit colour ANSI sequences, a band of four solid blocks, each 12 columns wide and 3 rows high, side by side on the same terminal row:
  - B1 = RGB (1, 254, 3), B2 = RGB (251, 2, 249), B3 = RGB (4, 5, 250): fixed reference colours;
  - B4 = colour computed by a command with known output: RGB (`$((6*7))`, `$((200+13))`, `$((3**4))`) = (42, 213, 81);
  - B5, below the band, 36 columns wide and 3 rows high: **disk state**. If the first 11 bytes of `/dev/vda` read with `sudo head -c 11 /dev/vda` are `LIGHTVM-FX1`, B5 = RGB (250, 250, 6) ("marker present"); otherwise B5 = RGB (6, 250, 250) ("marker absent").
- `fx.sh scrivi`: `printf 'LIGHTVM-FX1' | sudo dd of=/dev/vda bs=512 count=1 conv=fsync status=none`, then `fx.sh mostra`.

### Sequence of actions

Expressed with the v0.2 catalogue tools (screenshot, mouse, keyboard). Coordinates for the 1280×800 display observed in `F0-04`.

| Step | Action | Wait |
| --- | --- | --- |
| A0 | Boot from the baseline | Ready when two consecutive screenshots 5 s apart are identical and have at least 99% non-black pixels; timeout 300 s |
| A1 | Left click at (662, 326), "Deutsch" entry in the language list | Screenshot after 2 s |
| A2 | Left click at (658, 422), "English" entry | Screenshot after 2 s |
| A3 | Keys `ctrl-alt-t` | Terminal open: screenshot different from that of A2 within 15 s |
| A4 | Typing `sh /media/ubuntu/LIGHTVM_FX/fx.sh mostra` (or the equivalent body in the typed variant), Enter | Screenshot after two consecutive captures 1 s apart are identical; timeout 20 s |
| A5 | Typing `sh /media/ubuntu/LIGHTVM_FX/fx.sh scrivi`, Enter | As A4 |
| A6 | Stop the VM with the ordinary escalation (ADR-0009, `F0-06` §2d) | End of process observed |
| A7 | New startup **without** restore, then A3 and A4 | As above |
| A8 | Restore from the baseline (new overlay and new vars), startup, then A3 and A4 | As above |

### Expected result and verification

The verifier is a deterministic program run by the backend on the 24-bit PPM screenshots produced by `screendump` (no resizing, no lossy compression). It does not use OCR, approximate image comparison or a model's judgement.

| Check | After | Criterion |
| --- | --- | --- |
| V1 mouse | A1 | The control pixel of the "Deutsch" row has exactly the selection colour recorded in calibration and that of the "English" row does not |
| V2 mouse | A2 | Opposite condition to V1 |
| V3 keyboard and known output | A4 | For each of B1…B4 there is a connected set of pixels of that exact colour whose bounding box is full (pixel count = area), with an area of at least 12×3 cells at the minimum cell size declared in calibration; the four boxes are at the same height and in order B1, B2, B3, B4 from left to right; B5 is "marker absent" |
| V4 write | A5 | V3 with B5 "marker present" |
| V5 persistence | A7 | V3 with B5 "marker present" |
| V6 restore | A8 | V3 with B5 "marker absent" |

Calibration, run once per ISO hash and resolution and recorded in the fixture manifest: coordinates and colour of the V1/V2 control pixels derived from the reference screenshots; minimum terminal cell size; check that the six fixture colours (B1…B4 and the two values of B5) do not appear in any pixel of the reference screenshots without the fixture.

Outcomes, fixed before the run:

| Outcome | Condition |
| --- | --- |
| `passed` | V1…V6 met |
| `failed` | Infrastructure working (valid screenshots, QMP and process normal) and at least one V check not met |
| `timed_out` | A wait in the sequence exceeds its timeout |
| `infrastructure_error` | QMP error, invalid screenshot, unexpected QEMU exit, storage or restore error |
| `inconclusive` | Calibration missing or not valid for the current hash and resolution; fixture colours present in the reference screenshots |
| `cancelled` | Stop requested by the user, revocation or Emergency Stop during the run |

The persistent effect that the restore must undo is the `LIGHTVM-FX1` marker in the first sector of `/dev/vda`, that is in the run's overlay: the baseline stays all zeros (also verifiable with the baseline hash, ADR-0005).

Verification of the criteria:

1. Offline, without network and without guest agent: met (`-nic none` profile, no agent).
2. Independent of animations, times of day, notifications: the verifier looks for exact colours not present in the reference desktop; the terminal cursor is hidden; the clock and notifications do not touch the fixture colours; waits use stability between captures, not fixed times.
3. Automatic verification without an LLM: met.
4. Persistent effect on the disk undone by the restore: met (V5, V6).
5. Input limited to the declared US layout and to ASCII: met.

## Alternatives considered

- Preinstalled text editor with verification of the displayed text: would require OCR or approximate region comparison; excluded.
- Small dedicated application distributed on the data medium: controlled behaviour, but requires building and distributing an executable in the guest; not needed, because an `sh` script with ANSI sequences gives a pixel-deterministic result.
- File in the home directory as persistent effect: in the live session the home is in RAM and does not persist; the first sector of the data disk does.
- Baseline with Ubuntu installed on disk: not yet tested (ADR-0011, criterion 2); the fixture uses the live session with an empty data disk, already demonstrated in `F0-05`.

## Consequences

- The fixture becomes part of the F3–F6 regression tests and is the workload for the F5 exit criterion.
- A change of ISO, resolution or CPU model requires a new calibration and a new validation.
- The mouse step depends on the live session's welcome window: with an installed baseline it must be replaced with an equivalent mouse action and the ADR must be updated.
- The path `/media/ubuntu/LIGHTVM_FX` depends on the automatic mounting of the second CD-ROM in the live session, not yet tested; it is tied to the data medium details still open in ADR-0008.

## Limits

- Elements not yet tested in the guest: printing 24-bit colour blocks in GNOME Terminal, automatic mounting of the data medium, reading `/dev/vda` with `head`. Tested: Ctrl+Alt+T, US typing, clicks at (662, 326) and (658, 422) with language change, `sudo dd` on `/dev/vda` (`F0-04` §4; `F0-05` §8.1).
- The coordinates apply to 1280×800 and to the ISO of ADR-0011; the selection colour and the control pixels are fixed at calibration.
- In `F0-05` §8.1 an input sent before the desktop was lost: the A0 readiness criterion is meant to avoid this and must be validated at implementation.

## Required evidence

- Definition: based on `docs/verification/F0-04-console-rfb-input.md` Results §4 and `docs/verification/F0-05-baseline-overlay-firmware.md` §8.1.
- Implementation and ten real runs with restore: dedicated verification file by F3 (name to be assigned in the phase in which it is run).

## References

Roadmap, sections 4, 6.4, 8, 9, 10 (F0, F3, F5), 17. Review: `docs/verification/F0-09-evidence-review.md`.
