# ADR-0018 — Reference hardware

- Status: Accepted
- Date: 2026-10-09
- Owner: to be assigned

## Context

The section 13 measurements use an initial reference: physical host with at least 4 cores/8 threads, 16 GiB RAM and SSD, Linux guest with 2 vCPUs/4 GiB, 1280×720 display, at least 100 GiB free before installation and baseline. F0 records the actual CPU, OS, storage and capacity of the lab.

## Decision

The development host is the **first Windows measurement host**. Characteristics recorded with command and output in the verification files:

| Item | Recorded value | Section 13 reference | Evidence |
| --- | --- | --- | --- |
| Machine | HP ZBook Fury 16 G9 Mobile Workstation (laptop) | — | `F0-01` §2 |
| OS | Windows 11 Pro 10.0.26200, build 26200 | Windows 11 x86_64 | `F0-01` §2; `F0-08` |
| CPU | 12th Gen Intel Core i7-12800HX, 16 cores / 24 logical processors (hybrid architecture) | ≥ 4 cores / 8 threads | `F0-01` §2; `F0-08` |
| RAM | 31.6 GiB; commit limit 63.6 GiB | ≥ 16 GiB | `F0-08` |
| Lab storage | E: NTFS on Samsung SSD 970 EVO Plus 2 TB (NVMe), 744.2 GiB free | SSD, ≥ 100 GiB free | `F0-08` |
| GPU | NVIDIA RTX A2000 8GB Laptop (driver 32.0.15.9658) and Intel UHD Graphics | — | `F0-01` §2 |
| Virtualization | Hyper-V active, VBS/HVCI running, WHPX working (`query-accelerators` → `whpx`); `HypervisorPlatform` feature reported *Disabled* via CIM | WHPX available | `F0-02` §1, §4.1 |

The host exceeds the reference; measurements are run with the reference guest configuration (2 vCPUs / 4 GiB) and the actual guest resolution recorded (1280×800 observed).

Verification of the criteria:

1. Characteristics recorded with command and output: met.
2. Virtualization state and WHPX verified: met, with the CIM discrepancy declared.
3. Measurement conditions declared: **partially met**. The F0 tests declare a local session, a host shared with other agents (parallel QEMU instances, WSL) and the state of the Windows session; power profile, power supply and antivirus were not recorded. Every formal measurement (F1 and later) records them.

## Alternatives considered

- Host dedicated to the lab, separate from the development machine: preferable for stable measurements and for untrusted guests (section 13); not available in F0.
- Physical Linux host with KVM: not available (ADR-0016).

## Consequences

- The hybrid CPU requires measurements normalized per host, with a note on the architecture; comparisons with other hosts must be made with caution.
- It is a single Intel host: F6 requires at least two physical hosts of the certified target, including Intel and AMD if both are declared (section 10 F6).
- It is a laptop: power supply and thermal profile must be recorded in every formal measurement.
- The F0 measurements were run with parallel loads from other agents: the latencies are not release measurements.
- The host is also used for development: runners with untrusted guests must not contain production credentials (section 13).

## Required evidence

- `docs/verification/F0-01-toolchain-eframe.md` §2, `docs/verification/F0-02-qemu-whpx-qmp.md` §1, `docs/verification/F0-08-resource-limits-measurements.md` "Lab capacity" (verified).

## References

Roadmap, sections 10 (F0, F6), 13, 14, 17. Review: `docs/verification/F0-09-evidence-review.md`.
