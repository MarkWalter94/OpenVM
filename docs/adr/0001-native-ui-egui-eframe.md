# ADR-0001 — Native Rust UI with egui/eframe and the console as a texture

- Status: Accepted
- Date: 2026-10-09
- Owner: to be assigned

## Context

The interface must display the guest framebuffer and a few forms. A WebView with a WebSocket gateway and CSP would widen the attack surface with no functional benefit (roadmap, section 3, "UI stack" row). The roadmap confirms Rust for the whole application (section 6.0).

## Decision

- Native UI with egui and eframe (winit plus the wgpu or glow backend); AccessKit for accessibility; egui_extras for tables and images; egui_commonmark for the Markdown view of reports.
- The guest desktop is an egui texture drawn by the internal display client (ADR-0003).
- No WebView, no web runtime, no HTML engine or script interpreter; no application dependency on Tauri, Electron, Docker or Proxmox.
- Text coming from the guest, logs and reports rendered as text or restricted Markdown: never active markup, never automatically opened links.
- Repaint only on an event or a framebuffer update; no aggressive continuous polling.
- Files and folders opened only through the native picker; no generic host commands from the UI.

The egui/eframe versions and the choice of rendering backend are in ADR-0014.

## Alternatives considered

- WebView (Tauri/Electron) with a WebSocket gateway and a web VNC client: excluded by the roadmap (sections 3 and 6.0).
- Non-Rust native toolkits: excluded by the confirmation of Rust for the whole application (section 6.0).

## Consequences

- WebView, WebSocket gateway and CSP outside the security perimeter.
- Rendering depends on the GPU/driver: verified behavior is needed on hosts without graphics acceleration, for example RDP sessions (section 10 F0; risk in section 14).
- Accessibility through AccessKit to be verified with one screen reader per platform (F2).
- UI tests use a framework compatible with egui (ADR-0022).

## Required evidence

- Real minimal eframe build on Windows and Linux, rendering backend, software rendering: `docs/verification/F0-01-toolchain-eframe.md` (verified on Windows; RDP not run) and `docs/verification/F0-07-linux-kvm.md` §2–§3 (Linux build in WSL2, **[build]**). Console as an egui texture with a real framebuffer: `docs/verification/F0-04-console-rfb-input.md` Results §3 (verified). Resulting decisions in ADR-0014 and ADR-0024.
- Negative test on reports with HTML/script, terminal sequences and links (section 7.4, "Report" row): to be run by F5.

## References

Roadmap, sections 3, 6.0, 6.4, 7.4, 10 (F0, F2), 14.
