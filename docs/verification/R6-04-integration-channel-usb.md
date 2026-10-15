# R6-04 — Spike: LightVM Tools channel, shared folders, USB and XP/Vista toolchain

- Execution date (UTC): 2026-10-15
- Executed by: development agent on the project host
- Roadmap step: section 12, R6 ("Integrazione in uso manuale"); sections 7.2 and 7.4
- Related ADRs: ADR-0031 (host/guest integration in manual use), ADR-0027 (Windows guest profiles), ADR-0010 (bundled QEMU)
- Status: IN PROGRESS — findings are written incrementally while the spike runs

## Purpose

ADR-0031 leaves three choices to a spike: the guest channel of LightVM Tools, the shared-folder method on Windows guests, and the USB method. It also requires a toolchain that produces Windows tools running on XP and Vista. This file records what was executed on the project host, what was established from documentation and binaries only, and what remains unverified.

What this file does NOT demonstrate: any product code (none is written); any host driver installation (none is performed); anything about a guest system that was not booted here.

## Environment

| Item | Value |
| --- | --- |
| Host | Intel Core i7-12800HX, 31.6 GiB RAM; Windows 11 Pro 10.0.26200; Hyper-V/WHPX |
| QEMU | 11.1.0 (`v11.1.0-12130-ge470268ff4`), `lab/qemu/qemu-11.1.0` (ADR-0010) |
| Scratch data | `E:\OpenVM\lab\integration-spike\` (git-ignored by `/lab/`) |
| Limits applied to every guest | at most 2 vCPU and 2 GiB, process priority BelowNormal, one guest at a time |

## Findings

(sections appended below as each test completes)
