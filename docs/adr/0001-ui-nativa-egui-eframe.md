# ADR-0001 — UI nativa Rust con egui/eframe e console come texture

- Stato: Accettata
- Data: 2026-10-09
- Responsabile: da assegnare

## Contesto

L'interfaccia deve mostrare il framebuffer del guest e alcuni form. Una WebView con gateway WebSocket e CSP amplierebbe la superficie di attacco senza un beneficio funzionale (roadmap, sezione 3, riga "Stack UI"). La roadmap conferma Rust su tutta l'applicazione (sezione 6.0).

## Decisione

- UI nativa con egui ed eframe (winit più backend wgpu o glow); AccessKit per l'accessibilità; egui_extras per tabelle e immagini; egui_commonmark per la vista Markdown dei report.
- Il desktop guest è una texture egui disegnata dal client display interno (ADR-0003).
- Nessuna WebView, nessun runtime web, nessun motore HTML o interprete di script; nessuna dipendenza applicativa da Tauri, Electron, Docker o Proxmox.
- Testo proveniente dal guest, log e report resi come testo o Markdown limitato: mai markup attivo, mai link aperti automaticamente.
- Repaint solo su evento o aggiornamento del framebuffer; nessun polling continuo aggressivo.
- File e cartelle aperti solo tramite selettore nativo; nessun comando host generico dalla UI.

Versioni di egui/eframe e scelta del backend di rendering sono in ADR-0014.

## Alternative considerate

- WebView (Tauri/Electron) con gateway WebSocket e client VNC web: esclusa dalla roadmap (sezioni 3 e 6.0).
- Toolkit nativi non Rust: esclusi dalla conferma di Rust su tutta l'applicazione (sezione 6.0).

## Conseguenze

- WebView, gateway WebSocket e CSP fuori dal perimetro di sicurezza.
- Il rendering dipende da GPU/driver: serve un comportamento verificato su host senza accelerazione grafica, ad esempio sessioni RDP (sezione 10 F0; rischio in sezione 14).
- Accessibilità tramite AccessKit da verificare con uno screen reader per piattaforma (F2).
- I test UI usano un framework compatibile con egui (ADR-0022).

## Prove richieste

- Build minima reale di eframe su Windows e Linux, backend di rendering, rendering software: `docs/verification/F0-01-toolchain-eframe.md` (verificato su Windows; RDP non eseguito) e `docs/verification/F0-07-linux-kvm.md` §2–§3 (build Linux in WSL2, **[build]**). Console come texture egui con framebuffer reale: `docs/verification/F0-04-console-rfb-input.md` Risultati §3 (verificato). Decisioni conseguenti in ADR-0014 e ADR-0024.
- Test negativo sui report con HTML/script, sequenze terminale e link (sezione 7.4, riga "Report"): da eseguire entro F5.

## Riferimenti

Roadmap, sezioni 3, 6.0, 6.4, 7.4, 10 (F0, F2), 14.
