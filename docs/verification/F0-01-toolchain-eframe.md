# F0-01 — Toolchain Rust, workspace e spike eframe

Fase: F0, passo 1 (ROADMAP sezione 15: "verificare toolchain; build minima").
Data di esecuzione: 9 ottobre 2026. Host: Windows 11 (unico host provato in questa verifica).

Legenda degli esiti: **verificato** = comando eseguito con l'esito riportato; **fallito** = comando eseguito con esito negativo; **non eseguito** = prova non svolta, con motivo.

## 1. Sintesi

| Prova | Esito |
| --- | --- |
| Workspace Cargo (sezione 6.2) creato e compilato, edizione 2024, resolver 3 | verificato |
| `cargo build --release -p lightvm-desktop` senza warning | verificato, ma solo con l'ambiente MSVC di Build Tools 2022 (vedi 4.1) |
| `cargo build` dalla shell senza ambiente MSVC impostato | **fallito**: `LNK1104 msvcrt.lib` (vedi 4.1) |
| Build con toolchain GNU (`stable-x86_64-pc-windows-gnu`) | verificato |
| `cargo clippy --workspace --all-targets --features lightvm-desktop/glow -- -D warnings` | verificato (nessun warning) |
| `cargo fmt --all --check` | verificato |
| Smoke wgpu (Vulkan, NVIDIA) | verificato |
| Smoke wgpu con `WGPU_BACKEND=dx12` e `WGPU_BACKEND=gl` | verificato |
| Smoke glow (OpenGL 3.3, NVIDIA) | verificato |
| wgpu su rasterizzatore software (WARP, "Microsoft Basic Render Driver") | verificato, con CPU molto alta durante l'animazione |
| glow senza accelerazione hardware (`--glow-hw off`) | **fallito**: `egui_glow requires opengl 2.0+` |
| Sessione RDP / host senza GPU | non eseguito |
| Test `egui_kittest` headless con albero AccessKit | verificato (3 test superati) |
| AccessKit con screen reader reale | non eseguito (previsto in F2) |
| Avvisi di canonicalizzazione dei percorsi (ROADMAP sezione 2) | non riprodotti |
| Build e smoke su Linux | non eseguito (nessun host Linux usato in questo passo) |

## 2. Ambiente

Comandi: `Get-CimInstance Win32_OperatingSystem`, `Win32_Processor`, `Win32_VideoController`, `Win32_ComputerSystem`, `query session`, `rustc -Vv`, `cargo -V`, `rustup show`, `vswhere.exe -products *`.

| Voce | Valore rilevato |
| --- | --- |
| OS | Microsoft Windows 11 Pro 10.0.26200, build 26200 |
| Macchina | HP ZBook Fury 16 G9 Mobile Workstation PC |
| CPU | 12th Gen Intel Core i7-12800HX, 16 core, 24 processori logici |
| RAM | 31,6 GiB visibili |
| GPU 1 | Intel UHD Graphics, driver 32.0.101.7077 |
| GPU 2 | NVIDIA RTX A2000 8GB Laptop GPU, driver 32.0.15.9658 (WDDM), 596.58 (Vulkan/GL) |
| Display | 1920×1080 su entrambi gli adattatori, scala 1.0 (`pixels_per_point=1.00`) |
| Sessione | console locale (`query session`: `>console MOrtali Attivo`), non RDP |
| rustup | 1.29.1 |
| rustc / cargo | 1.99.0 (b940084d7 2026-09-28) / 1.99.0 (5f94df478 2026-08-27) |
| Toolchain predefinita utente | `stable-x86_64-pc-windows-gnu` (`~/.rustup/settings.toml`), mentre il "default host" è `x86_64-pc-windows-msvc` |
| Toolchain attiva nel workspace | `1.99.0-x86_64-pc-windows-msvc (overridden by 'E:\OpenVM\rust-toolchain.toml')` |
| Linker GNU | WinLibs GCC 16.1.0 (MinGW-W64 x86_64-ucrt-posix-seh) nel PATH |
| Visual Studio rilevati | Build Tools 2022 17.14.41; VS Enterprise 2026 18.10.0; VS Enterprise 2022 17.14.40; VS Enterprise 2019 16.11.60 |
| Windows SDK | 10.0.26100.0 |

Nota: alla prima invocazione di cargo nel workspace, rustup ha **installato automaticamente** nel profilo utente la toolchain `1.99.0-x86_64-pc-windows-msvc` richiesta da `rust-toolchain.toml` (messaggio `warn: the missing active toolchain ... has been auto-installed`, con avviso che l'auto-installazione è deprecata). È un'installazione utente in `~/.rustup`, non di sistema. Stesso commit di compilatore (b940084d7) della toolchain GNU già presente.

## 3. Workspace creato

| Percorso | Contenuto |
| --- | --- |
| `Cargo.toml` | workspace, `resolver = "3"`, edizione 2024, `rust-version = "1.99"`, licenza MIT, `[workspace.lints]` (rust: `unsafe_code = deny`, `missing_debug_implementations`, `unreachable_pub`, `rust_2018_idioms`, `unused_qualifications`; clippy: `all`, `dbg_macro`, `todo`, `unimplemented`, `undocumented_unsafe_blocks = deny`) |
| `Cargo.lock` | 394 pacchetti, nessuna sorgente git |
| `rust-toolchain.toml` | `channel = "1.99.0"`, componenti `rustfmt`, `clippy`, profilo `minimal` |
| `apps/desktop/` | crate `lightvm-desktop` (lib + bin), spike eframe |
| `crates/vm-core`, `qemu-backend`, `vm-storage`, `lightvm-service`, `lightvm-proto` | scheletri: solo `lib.rs` con commento di responsabilità, nessuna API |
| `tests/{integration,security,e2e}/README.md` | segnaposto |

`crates/mcp-server` non è stato creato: la roadmap lo assegna a F5. Non è stato creato `.cargo/config.toml`.

### Versioni dei crate scelti

Verificate con `cargo search` e `cargo info <crate>@<versione>` il 9 ottobre 2026; fissate con `=` in `[workspace.dependencies]`.

| Crate | Versione | Note |
| --- | --- | --- |
| egui | =0.36.2 | ultima su crates.io al momento della verifica |
| eframe | =0.36.2 | `rust-version` 1.95; feature usate: `accesskit`, `default_fonts`, `wayland`, `x11`, più `wgpu` (predefinita del crate desktop) o `glow` |
| egui_extras | =0.36.2 | senza feature predefinite (non ancora usato nel codice) |
| egui_kittest | =0.36.2 | dev-dependency; trascina `kittest 0.4.0` (l'ultima su crates.io è 0.5.0, ma egui_kittest 0.36.2 richiede 0.4) |

Versioni transitive risolte nel lockfile: egui-wgpu 0.36.2, egui_glow 0.36.2, egui-winit 0.36.2, wgpu 30.0.1, glow 0.17.0, glutin 0.32.3, winit 0.30.13, accesskit 0.24.1, accesskit_winit 0.32.2, accesskit_windows 0.32.1.

Feature di eframe escluse di proposito: `links` (apre URL nel browser, in contrasto con ROADMAP 6.0 "mai link apribili automaticamente"), `persistence`, `web_screen_reader`.

### App desktop (spike)

- Finestra "LightVM" 1460×860 punti, tema scuro predefinito, passaggio al chiaro in Settings.
- Pannello laterale con Dashboard, Console, Settings; pannello centrale con texture 1280×720 generata in memoria e aggiornata al massimo 10 volte al secondo tramite `ctx.request_repaint_after` (nessun repaint continuo).
- Opzioni: `--smoke SECONDI`, `--renderer wgpu|glow` (se entrambe le feature sono attive), `--static` (framebuffer fermo, per misurare l'idle), `--wgpu-adapter cpu|auto`, `--glow-hw preferred|required|off`.
- Lo smoke stampa righe `SMOKE ...` con esito, backend e adattatore, versioni di egui/eframe/backend lette dal `Cargo.lock` in `build.rs`, dimensione finestra, frame disegnati, aggiornamenti del framebuffer, tempo al primo frame, istogramma degli intervalli tra frame e cause di repaint di egui.

Il framebuffer è simulato: non è una console reale e non soddisfa nessun criterio sulla console (ROADMAP 6.4).

## 4. Build

### 4.1 Linker MSVC: fallimento senza ambiente impostato

Comando (Git Bash, nessuna variabile MSVC impostata):

```text
cargo build --release -p lightvm-desktop
```

Esito: **fallito** per tutti gli script di build.

```text
= note: "C:\\Program Files\\Microsoft Visual Studio\\18\\Enterprise\\VC\\Tools\\MSVC\\14.51.36231\\bin\\HostX64\\x64\\link.exe" ...
= note: LINK : fatal error LNK1104: impossibile aprire il file 'msvcrt.lib'
```

Causa identificata: rustc individua automaticamente l'installazione più recente (VS Enterprise 2026, MSVC 14.51.36231), che contiene `link.exe` ma non le librerie CRT. Verifica con `ls .../VC/Tools/MSVC/<ver>/lib/x64/msvcrt.lib`:

| Installazione | `link.exe` | `lib/x64/msvcrt.lib` |
| --- | --- | --- |
| VS Enterprise 2026, MSVC 14.51.36231 | presente | assente |
| VS Enterprise 2022, MSVC 14.44.35207 | presente | assente |
| Build Tools 2022, MSVC 14.44.35207 | presente | presente |
| VS Enterprise 2019, MSVC 14.29.30133 | assente | assente |

Contromisura usata, senza modifiche al sistema: ogni comando cargo con target MSVC è stato eseguito in un `cmd` che prima chiama `C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat` (wrapper temporaneo nella scratchpad della sessione, non nel repository). `vcvars64.bat` stampa `"vswhere.exe" non è riconosciuto...`, avviso innocuo. Con questo ambiente:

```text
cargo build --release -p lightvm-desktop
    Finished `release` profile [optimized + debuginfo] target(s)
```

Esito: **verificato**, nessun warning (ricompilazione del crate desktop dopo l'ultima modifica, output senza righe `warning`).

Correzioni proposte (decisione dell'utente, non applicate):
1. installare in VS 2026 il componente "MSVC Build Tools x64/x86" con librerie (o rimuovere l'installazione parziale), così che il rilevamento automatico trovi un toolset completo; oppure
2. lavorare sempre da un "Developer PowerShell/Command Prompt" di Build Tools 2022 e ripetere la stessa impostazione nella CI; oppure
3. adottare la toolchain GNU (vedi 4.2), che però cambia ABI, formato dei simboli di debug e strumenti di firma/diagnostica rispetto all'ecosistema Windows standard.

### 4.2 Toolchain GNU

```text
cargo +stable-x86_64-pc-windows-gnu build --release -p lightvm-desktop --target-dir target/gnu
    Finished `release` profile [optimized + debuginfo] target(s) in 2m 21s
```

Esito: **verificato**; lo smoke wgpu (3 s) del binario GNU termina con `SMOKE esito=ok`, backend Vulkan NVIDIA, `primo_frame_ms=1028`. Eseguibile 102,5 MiB contro 13,3 MiB del MSVC: la differenza deriva dalle informazioni di debug DWARF incorporate (profilo release con `debug = "line-tables-only"`); con MSVC finiscono nel PDB separato.

### 4.3 Lint e formattazione

```text
cargo fmt --all --check                                                           -> exit 0
cargo clippy --workspace --all-targets --features lightvm-desktop/glow -- -D warnings  -> exit 0
cargo clippy -p lightvm-desktop --all-targets --no-default-features --features glow    -> nessun warning
```

Esito: **verificato** (ambiente MSVC di 4.1).

## 5. Smoke e misure per backend

### Metodo

- Script PowerShell temporaneo (scratchpad della sessione). Avvio: N esecuzioni consecutive di `lightvm-desktop.exe --smoke 0.5` misurate con `Measure-Command`, più il valore `primo_frame_ms` stampato dall'app (dall'ingresso in `main` al primo passaggio UI). Il totale di `Measure-Command` include l'attesa di 0,5 s dello smoke e la chiusura: non è il tempo di avvio.
- Memoria: processo avviato con `--smoke 16`; `WorkingSet64` e `PrivateMemorySize64` di `System.Diagnostics.Process` (gli stessi di `Get-Process`) letti dopo 3 s.
- CPU: differenza di `TotalProcessorTime` tra 3 s e 13 s (finestra di 10 s), in percentuale di un core logico.
- Solo avvii a caldo (binario e driver già in cache). Avvii a freddo non misurati: richiederebbero riavvio o svuotamento della cache.
- Finestra in primo piano, nessuna interazione con mouse o tastiera durante le misure.
- Build: `target\release` (feature `wgpu`) e `target\glow\release` (`--no-default-features --features glow`), entrambe MSVC.

Il campione è piccolo e su un solo host: indica ordini di grandezza, non è la misura formale della sezione 13 (che chiede, tra l'altro, 5 minuti di idle e somma UI + supervisor).

### Risultati

| Configurazione | Adattatore riportato dall'app | Primo frame p50 / p95 / max (ms) | Working set a 3 s | Private bytes a 3 s | CPU 3–13 s, animazione 10 Hz | CPU 3–13 s, `--static` |
| --- | --- | --- | --- | --- | --- | --- |
| wgpu predefinito (20 avvii) | `backend=Vulkan adapter="NVIDIA RTX A2000 8GB Laptop GPU" tipo=DiscreteGpu driver_info="596.58"` | 705 / 919 / 1005 | 177,6 MiB | 396,3 MiB | 7,18% | 0,16% |
| wgpu, `WGPU_BACKEND=dx12` (10 avvii) | `backend=Dx12 adapter="NVIDIA RTX A2000 8GB Laptop GPU" tipo=DiscreteGpu driver="32.0.15.9658"` | 501 / 538 / 538 | 179,2 MiB | 357,8 MiB | 3,90% | non misurato |
| wgpu, DX12 + `--wgpu-adapter cpu` (WARP, 10 avvii) | `backend=Dx12 adapter="Microsoft Basic Render Driver" tipo=Cpu driver="10.0.26100.9278"` | 448 / 482 / 482 | 124,8 MiB | 376,3 MiB | **785,73%** (circa 7,9 core logici) | 0,16% |
| glow (20 avvii) | `vendor="NVIDIA Corporation" renderer="NVIDIA RTX A2000 8GB Laptop GPU/PCIe/SSE2" versione_gl="3.3.0 NVIDIA 596.58"` | 294 / 326 / 347 | 82,9 MiB | 81,4 MiB | 7,49% | 0,47% |

Totale `Measure-Command` (include 0,5 s di smoke e chiusura), p50/p95: wgpu 1069/1333 ms; glow 638/653 ms; wgpu DX12 784/820 ms; WARP 1246/1307 ms.

Per tutte le configurazioni con GPU lo smoke riporta `finestra_punti=1460x860 pixels_per_point=1.00 finestra_px=1460x860`. Su 16 s di esecuzione animata: wgpu Vulkan `frame=159 aggiornamenti_fb=152`, glow `frame=165 aggiornamenti_fb=157`, wgpu DX12 `frame=164 aggiornamenti_fb=156`; con WARP `frame=89 aggiornamenti_fb=88`, cioè circa 5,5 aggiornamenti al secondo invece di 10: il rasterizzatore software non regge il ritmo richiesto. Con `--static`: 7–9 frame in 16 s.

Altri smoke eseguiti (4–5 s ciascuno, esito `ok`): `WGPU_BACKEND=gl` → `backend=Gl adapter="NVIDIA RTX A2000 8GB Laptop GPU/PCIe/SSE2" driver_info="4.6.0 NVIDIA 596.58"`; glow `--glow-hw required` → stesso adattatore NVIDIA.

Osservazioni:
- Il processo usa la GPU NVIDIA discreta in tutti i casi con wgpu/glow predefiniti (`PowerPreference::HighPerformance` predefinito in egui-wgpu). Su un portatile ibrido questo tiene accesa la dGPU; `WGPU_POWER_PREF=low` permetterebbe di scegliere l'integrata, non provato.
- La CPU con animazione (4–7,5% di un core) è dominata dalla generazione del pattern 1280×720 e dalla copia in una nuova `ColorImage` a ogni aggiornamento, non dal rendering. Non è rappresentativa del client display reale.
- Con wgpu i private bytes (358–396 MiB) superano di molto il working set; con glow le due misure sono vicine (circa 80 MiB). L'obiettivo della sezione 13 (memoria manager ≤250 MiB, UI + supervisor) va valutato dichiarando quale metrica si usa: con wgpu il working set della sola UI rientra, i private bytes no.
- Primo frame più lento con wgpu/Vulkan (p50 705 ms) che con glow (294 ms) o wgpu/DX12 (501 ms).

### Problema trovato e corretto durante lo spike

La prima versione richiedeva il repaint a 100 ms dall'ultimo aggiornamento e controllava `elapsed >= 100 ms`. Misura: 244 frame per 43 aggiornamenti in 5 s, con 106 intervalli tra frame sotto 2 ms. Causa, letta nel sorgente di egui 0.36.2 (`context.rs`, `request_repaint_after`): egui anticipa ogni repaint ritardato della durata prevista di un frame (`predicted_dt`), quindi il risveglio arriva prima della scadenza e l'app richiedeva repaint immediati a raffica. Correzione: scadenze su griglia fissa di 100 ms con tolleranza pari a `predicted_dt` + 3 ms. Dopo la correzione: 52 frame per 43 aggiornamenti (wgpu) e 61 per 48 (glow) in 5 s. È un'indicazione utile anche per il client display reale: il ritmo di repaint va controllato con una misura, non dedotto dal codice.

## 6. Rendering software e host senza accelerazione

| Prova | Comando | Esito |
| --- | --- | --- |
| Elenco adattatori wgpu | `lightvm-desktop.exe --smoke 4 --wgpu-adapter cpu` (stampa gli adattatori su stderr) | Vulkan: NVIDIA, Intel UHD 770; DX12: NVIDIA, Intel UHD, **Microsoft Basic Render Driver (Cpu)**; GL: NVIDIA |
| wgpu su WARP | `--wgpu-adapter cpu` (con e senza `WGPU_BACKEND=dx12`) | verificato: `adapter="Microsoft Basic Render Driver" tipo=Cpu`; CPU elevata con animazione (sezione 5) |
| wgpu, adattatore CPU solo Vulkan | `WGPU_BACKEND=vulkan` + `--wgpu-adapter cpu` | fallito come atteso: `Adapter selection failed: nessun adattatore wgpu di tipo CPU disponibile` (nessun driver Vulkan software installato) |
| glow senza accelerazione | `--glow-hw off` | fallito: `egui_glow: OpenGL: egui_glow requires opengl 2.0+.` Senza accelerazione WGL offre solo l'implementazione Microsoft GDI OpenGL 1.1 |
| OpenGL software alternativo | ricerca di `C:\Windows\System32\opengl32sw.dll` | assente; nessun Mesa/llvmpipe installato e, per regola del compito, nessuna installazione eseguita |

Opzioni di fallback disponibili in eframe/egui-wgpu 0.36.2 (lette nel sorgente):
- variabili d'ambiente lette da `WgpuSetup::CreateNew` predefinito: `WGPU_BACKEND` (`vulkan`, `dx12`, `metal`, `opengl`, `webgpu`), `WGPU_POWER_PREF` (`low`, `high`, `none`), più flag di debug/validazione;
- `NativeOptions::wgpu_options.wgpu_setup` → `WgpuSetupCreateNew::native_adapter_selector` per scegliere l'adattatore (usato da `--wgpu-adapter cpu`);
- `force_fallback_adapter` è fisso a `false` in egui-wgpu e non è esposto;
- `NativeOptions::glow_options.hardware_acceleration` (`Required`, `Preferred`, `Off`) per glow.

Non verificato su questo host: sessione RDP, macchina senza GPU o con driver di base, VM. In una sessione RDP Windows espone normalmente un adattatore remoto (o WARP); l'esito va provato, non dedotto. Proposta per una prova successiva: stesso binario, smoke in sessione RDP verso questo host e su una VM senza GPU, registrando l'adattatore scelto automaticamente e quello forzato con `--wgpu-adapter cpu`.

## 7. Test UI headless con egui_kittest

Comando (ambiente MSVC di 4.1): `cargo test -p lightvm-desktop`

```text
running 3 tests
test albero_accesskit_contiene_le_voci_di_navigazione ... ok
test click_su_dashboard_cambia_sezione ... ok
test tema_scuro_predefinito_e_passaggio_al_chiaro ... ok
test result: ok. 3 passed; 0 failed; 0 ignored
```

Esito: **verificato**. File: `apps/desktop/tests/kittest_ui.rs`. I test costruiscono la UI con `Harness::builder().with_size(1280×800).build_ui_state(...)`, cercano i nodi AccessKit con `get_by_role_and_label(Role::Button, "Dashboard")` (e Console, Settings), cliccano una voce e cambiano tema.

Problema incontrato: `get_by_label("Console")` fallisce perché trova due nodi (il pulsante di navigazione e il titolo della sezione). Il test usa quindi ruolo più etichetta. Messaggio originale: `If you were expecting multiple nodes, use query_all instead of query.`

Limiti: il test non usa GPU né finestra nativa, non verifica il rendering né un client UIA reale (Narratore, NVDA). Le istantanee grafiche (feature `snapshot` + `wgpu` di egui_kittest) non sono state provate.

## 8. Avvisi di canonicalizzazione del percorso

La ricognizione della sezione 2 della roadmap segnala avvisi di canonicalizzazione emessi da Rust/Cargo. In questa verifica **non sono stati riprodotti**. Prove eseguite, tutte senza avvisi:

| Condizione | Comandi |
| --- | --- |
| Git Bash, cwd `E:\OpenVM` | `rustc -Vv`, `cargo -V`, `rustup show`, build complete |
| PowerShell, cwd `E:\OpenVM\docs` ed `E:\` | `rustc -V`, `cargo -V` |
| `cmd /c`, cwd `E:\OpenVM` | `rustc -V`, `cargo -V` |
| cwd UNC `\\localhost\e$\OpenVM` (Git Bash) | `rustc -V`, `cargo -V`, `rustup show active-toolchain` |
| cwd `E:\CPro\packages\rtvc` (directory con override rustup) | `rustc -V`, `cargo -V` |

Elementi rilevati che possono spiegare l'avviso originale, senza prova diretta:
- `E:` è un volume NTFS locale (`Win32_LogicalDisk DriveType=3`), nessun `subst` né unità di rete; `~/.cargo` e `~/.rustup` non sono link o junction; nessuna variabile `CARGO_*`/`RUSTUP_*` impostata.
- `~/.rustup/settings.toml` contiene un override con percorso verbatim: `'\\?\E:\CPro\packages\rtvc' = "stable-x86_64-pc-windows-gnu"`. Anche `rustfmt` stampa percorsi `\\?\E:\OpenVM\...`. È il formato restituito da `std::fs::canonicalize` su Windows; da solo non ha prodotto avvisi nelle prove.
- Ipotesi più probabile: la ricognizione è stata eseguita con una cwd o in un ambiente (per esempio una sandbox con accesso limitato alle directory padre, o una cwd non più esistente) in cui la canonicalizzazione falliva. Non è stato possibile ricreare quella condizione.

Correzione proposta: nessuna modifica al sistema finché l'avviso non ricompare. Se ricompare, registrare il testo esatto, la cwd e la shell, ed eseguire `rustup show` e `cargo -vV` nella stessa condizione. Se l'avviso cita l'override verbatim, rimuoverlo e ricrearlo con `rustup override unset --path E:\CPro\packages\rtvc` e `rustup override set ...` (azione dell'utente, non eseguita).

## 9. Limiti della verifica

- Un solo host fisico, sessione console locale, GPU NVIDIA e Intel con driver installati. Nessuna prova su Linux, RDP, VM o macchina senza GPU.
- Misure su campioni piccoli, solo a caldo, finestre di 10 s (la sezione 13 chiede 5 minuti di idle e 20 avvii a freddo e a caldo).
- Il framebuffer è un pattern generato: le misure di CPU con animazione non valgono per la console RFB reale.
- AccessKit verificato solo tramite l'albero in egui_kittest, non con uno screen reader.
- La build MSVC funziona solo con l'ambiente di Build Tools 2022 caricato manualmente (sezione 4.1).
- I numeri dipendono dalle versioni di driver indicate nella sezione 2.

## 10. Decisioni proposte per l'ADR (egui/eframe e backend)

Da valutare e registrare nell'ADR dall'agente o dalla persona responsabile; questo documento non le decide.

1. **Versioni:** fissare egui, eframe, egui_extras ed egui_kittest a `=0.36.2`, con `Cargo.lock` versionato e nessuna dipendenza git. Aggiornare tutte e quattro insieme, ripetendo questa verifica.
2. **Feature di eframe:** `default-features = false` con `accesskit`, `default_fonts`, `wayland`, `x11` e un backend. Escludere `links` (ROADMAP 6.0) e `persistence` finché non serve.
3. **Backend predefinito su Windows (decisione dell'utente):** due opzioni misurate:
   - *glow*: primo frame più rapido (p50 294 ms), memoria molto più bassa (circa 83 MiB di working set, 81 MiB privati), binario più piccolo (6,8 MiB contro 13,3), ma nessun ripiego software su Windows (`--glow-hw off` fallisce) e OpenGL è un'API meno curata sui driver Windows;
   - *wgpu*: ripiego software verificato (WARP tramite DX12), backend multipli (Vulkan, DX12, GL), ma private bytes intorno a 360–400 MiB e avvio più lento con Vulkan.
   Proposta tecnica: wgpu come backend di prodotto per avere il ripiego WARP, con DX12 preferito su Windows (primo frame e CPU migliori di Vulkan in questa misura), e glow mantenuto compilabile come alternativa diagnostica. La scelta va confermata dall'utente alla luce del budget di memoria della sezione 13.
4. **Ripiego software:** usare WARP soltanto quando non c'è un adattatore hardware, perché con la console animata a 10 Hz consuma circa 7,9 core logici e non mantiene i 10 aggiornamenti al secondo. Se adottato, dichiararlo come modalità degradata in `docs/COMPATIBILITY.md`, prevedere una frequenza di console ridotta e verificarlo in RDP e su VM prima di dichiararlo.
5. **Selezione dell'adattatore:** decidere tra `HighPerformance` (predefinito, attiva la dGPU sui portatili ibridi) e `LowPower` per un'app che resta spesso inattiva.
6. **Toolchain Windows (decisione dell'utente):** target MSVC (proposto, coerente con PDB, firma del codice e strumenti Windows) richiede un toolset MSVC completo e rilevabile automaticamente o un ambiente "Developer" esplicito in sviluppo e in CI; in alternativa target GNU, già funzionante su questo host.
7. **Framework di test UI:** egui_kittest 0.36.2 è utilizzabile per test headless via AccessKit; le istantanee grafiche e i test su Linux restano da provare entro F1.
8. **Ritmo di repaint:** il client display reale deve pianificare i repaint su scadenze che tengano conto dell'anticipo `predicted_dt` di egui e la verifica deve misurare i frame effettivi (lo smoke conta frame e intervalli).

## 11. File prodotti da questo passo

- `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`
- `apps/desktop/Cargo.toml`, `apps/desktop/build.rs`, `apps/desktop/src/lib.rs`, `apps/desktop/src/main.rs`, `apps/desktop/tests/kittest_ui.rs`
- `crates/{vm-core,qemu-backend,vm-storage,lightvm-service,lightvm-proto}/{Cargo.toml,src/lib.rs}`
- `tests/{integration,security,e2e}/README.md`
- questo documento
