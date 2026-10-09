# ADR-0014 — Versioni di egui/eframe e backend di rendering

- Stato: Accettata (Windows); backend Linux rinviato a R2a con ADR-0016
- Data: 2026-10-09
- Responsabile: da assegnare

## Contesto

ADR-0001 fissa egui/eframe. La roadmap richiede di fissare in F0 le versioni di egui/eframe e dei backend (sezione 6.0) e di verificare la build minima su Windows e Linux, il backend (wgpu o glow) e il comportamento su host senza accelerazione grafica, ad esempio RDP (sezione 10 F0). Il rischio "rendering assente" prevede un fallback software verificato e un limite dichiarato (sezione 14). La memoria del manager ha l'obiettivo di 250 MiB per UI più supervisor (sezione 13); la metrica proposta in `F0-09` §5 sono i private bytes.

## Decisione

**Versioni** (fissate con `=` in `[workspace.dependencies]`, `Cargo.lock` versionato, nessuna dipendenza git):

| Crate | Versione |
| --- | --- |
| egui, eframe, egui_extras, egui_kittest | 0.36.2 |
| Transitive risolte | egui-wgpu 0.36.2, egui_glow 0.36.2, egui-winit 0.36.2, wgpu 30.0.1, glow 0.17.0, glutin 0.32.3, winit 0.30.13, accesskit 0.24.1, accesskit_winit 0.32.2, accesskit_windows 0.32.1 |
| Toolchain | Rust 1.99.0 (`rust-toolchain.toml`), edizione 2024 |

Le quattro versioni egui si aggiornano insieme ripetendo la verifica di `F0-01`. `egui_commonmark` non è ancora nel workspace: entra con una versione compatibile con egui 0.36.2, fissata allo stesso modo, quando serve la vista dei report.

**Feature di eframe:** `default-features = false` con `accesskit`, `default_fonts`, `wayland`, `x11` e i backend `glow` e `wgpu`. Escluse `links` (apertura di URL nel browser, contraria alla sezione 6.0), `persistence`, `web_screen_reader`.

**Backend su Windows:**

1. **Predefinito: glow** (OpenGL 3.3). Misure con la console reale: working set 91,8–94,5 MiB, private bytes 88,4–97,6 MiB; primo frame p50/p95 294/326 ms (spike, 20 avvii a caldo); CPU con animazione simile a wgpu (9–10% di un core). È l'unico backend misurato che rispetta l'obiettivo di memoria con la metrica dei private bytes, lasciando margine al supervisor.
2. **Selezionabile: wgpu con DX12** (`WGPU_BACKEND=dx12` o opzione equivalente). Private bytes 369–372 MiB con la sola UI: **deviazione dichiarata** dall'obiettivo di memoria della sezione 13 quando l'utente sceglie questo backend.
3. **Modalità degradata dichiarata: wgpu su WARP** ("Microsoft Basic Render Driver"), unico ripiego software verificato su Windows: glow senza accelerazione fallisce (`egui_glow requires opengl 2.0+`, perché senza driver WGL offre solo GDI OpenGL 1.1). WARP con animazione a 10 Hz consuma circa 7,9 processori logici e regge circa 5,5 aggiornamenti al secondo; private bytes 376 MiB. In questa modalità la frequenza della console è ridotta e l'interfaccia segnala la condizione.
4. Selezione automatica proposta: tentativo con glow; in caso di errore di inizializzazione, nuovo tentativo con wgpu DX12 su adattatore hardware e poi su WARP, con indicazione visibile del backend effettivo. Il ripiego automatico **non è ancora implementato né provato** (backlog F1/F2).
5. Adattatore wgpu: preferenza da decidere tra `HighPerformance` (predefinita, attiva la GPU discreta sui portatili ibridi) e `LowPower`; da misurare in F2.

**Motivazione della scelta rispetto all'alternativa (wgpu predefinito):** wgpu offre il ripiego WARP e più API, ma impegna circa 370 MiB già con la sola UI. Scegliere wgpu come predefinito significherebbe pubblicare in modo permanente una deviazione dall'obiettivo di memoria nella configurazione normale, per servire un caso (host senza GPU) che resta degradato anche con wgpu (CPU molto alta su WARP). glow predefinito tiene la configurazione normale entro l'obiettivo; la mancanza di ripiego software di glow è compensata dal ripiego su wgpu/WARP, con la sua deviazione dichiarata solo dove serve.

**Linux:** build verificata in WSL2 (driver C zig) con wgpu su llvmpipe (Vulkan e GL); glow non provato su Linux; nessun host fisico con GPU. Il backend predefinito Linux è deciso in R2a; entrambe le build restano compilabili in CI.

**Ritmo di repaint:** il client display pianifica i repaint tenendo conto dell'anticipo `predicted_dt` di egui (scadenze su griglia con tolleranza) e la verifica conta i frame effettivi; ogni `request_repaint()` dal thread RFB produce oggi due passaggi UI, da ridurre in F2.

Verifica dei criteri:

1. Build reale su Windows e Linux: soddisfatto (Windows con ambiente MSVC esplicito; Linux in WSL2 con toolchain non standard).
2. Avvio in sessione locale e RDP: locale soddisfatto; **RDP non eseguito**, limite dichiarato in `docs/COMPATIBILITY.md`.
3. Texture 1280×720 senza repaint continuo: soddisfatto (texture 1280×720 dello spike e framebuffer reale 1280×800 con aggiornamento per rettangoli).
4. Albero AccessKit esposto: soddisfatto via egui_kittest (screen reader in F2).
5. Versioni compatibili tra loro e con la toolchain: soddisfatto.

## Alternative considerate

- wgpu predefinito con DX12 (proposta tecnica di `F0-01` §10): scartata come predefinita per il budget di memoria, mantenuta selezionabile e come ripiego.
- wgpu con Vulkan: primo frame più lento (p50 705 ms) e CPU più alta di DX12 nello spike.
- glow solo, senza wgpu: escluso, lascerebbe gli host senza GPU privi di ripiego.

## Conseguenze

- Le misure di memoria e CPU del manager (sezione 13) si pubblicano per backend, con private bytes e working set distinti.
- La CI compila e testa entrambe le configurazioni (`--no-default-features --features glow` e wgpu).
- Su host senza GPU o in RDP la modalità effettiva va provata prima di dichiararla; fino ad allora è un limite.
- Un aggiornamento di versione richiede la ripetizione della prova minima.

## Prove richieste

- Build, smoke e misure per backend, WARP, glow senza accelerazione, egui_kittest: `docs/verification/F0-01-toolchain-eframe.md` §4–§7 (verificato; RDP non eseguito).
- Console reale con wgpu DX12 e glow, misure di CPU e memoria: `docs/verification/F0-04-console-rfb-input.md` Risultati §3, §7 (verificato).
- Build e smoke Linux in WSL2: `docs/verification/F0-07-linux-kvm.md` §2–§3 (verificato **[build]**).
- Sessione RDP e ripiego automatico glow → wgpu: da eseguire (backlog F1/F2).

## Riferimenti

Roadmap, sezioni 2, 6.0, 10 (F0, F2), 13, 14, 17. Riesame: `docs/verification/F0-09-riesame-prove.md`.
