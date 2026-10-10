# F1-05 — UI desktop: appunti host, selezione del backend di rendering, istantanee e prime misure con il supervisor

- Data di esecuzione (UTC): 2026-10-10, dalle 09:35 alle 10:10 circa
- Esecutore: agente di sviluppo (sessione Claude Code), su richiesta del responsabile del progetto
- Passo della roadmap: sezione 6.0 (UI nativa), 6.4 (console), 13 (misure); backlog `docs/RELEASE_PLAN.md` F1-13, F1-16, F1-18, F1-19 (istantanee)
- ADR collegati: ADR-0001, ADR-0003, ADR-0014, ADR-0018, ADR-0022 (non modificati)
- Esito complessivo: **parziale** — test automatici della UI verdi; selezione del backend con ripiego verificata su hardware; prime misure di UI + supervisor registrate (memoria con wgpu oltre l'obiettivo, come D-12); lettura degli appunti host dentro egui-winit **non bloccata**; prova manuale degli appunti, sessione RDP e avvii a freddo **non eseguiti**

Esiti ammessi: **verificato**, **fallito**, **non eseguito** (sezione 17). Log grezzi in `lab/logs/` (fuori dal repository): `f1-18/` (`riepilogo.txt`, `avvii-caldo-glow.csv`, `avvii-caldo-wgpu.csv`, `idle-*.csv`, `idle-*-stdout.txt`, `ambiente.txt`), `f1-18-idle300/`, `f1-05/smoke*.txt`.

Questo file chiude il flusso UI interrotto nel commit `aa04ccd` (codice di `appunti.rs`, `render.rs`, `librerie.rs`, istantanee): il codice era presente, le prove mancavano.

## Scopo

1. F1-13: un `Paste` prodotto da egui-winit non porta testo degli appunti host né nella console né in altri widget, e la combinazione arriva al guest come tasti.
2. F1-16: selezione automatica glow → wgpu (DX12) → WARP con indicazione visibile, e ripiego quando un backend fallisce.
3. F1-19 / ADR-0022: istantanee grafiche `egui_kittest` confrontate con file nel repository.
4. F1-18: memoria (private bytes e working set) e CPU in idle di UI + supervisor su 5 minuti; avvii della UI a caldo.

Non dimostra: blocco della lettura degli appunti host a monte di egui-winit, comportamento in RDP, avvio a freddo, accessibilità con screen reader reale, Linux.

## Ambiente

| Voce | Valore |
| --- | --- |
| Host | Intel Core i9-14900K (32 processori logici), 64 GB RAM, GPU NVIDIA (driver OpenGL "NVIDIA 577.00") |
| OS e build | Microsoft Windows 11 Pro 10.0.26200 |
| Profilo energetico | "Prestazioni eccellenti" (`powercfg /getactivescheme`) |
| Toolchain | Rust 1.99.0 MSVC (vedi `F1-03` §Ambiente); binari `target/release` del 2026-10-10 |
| UI | egui/eframe 0.36.2, wgpu 30.0.1, glow 0.17.0 |
| Sessione grafica | locale, scala 125 % (`pixels_per_point=1.25`), finestra 1460×860 punti |
| Supervisor | `lightvm-service` avviato dalla UI con `--catalogo` su un catalogo vuoto, senza `--qemu-dir` (backend "non configurato", nessuna VM) |
| Altre condizioni | host non dedicato; durante la seconda serie di idle con glow erano in esecuzione test di `vm-storage` e smoke di rendering (vedi Limiti) |

## Procedura

```text
cargo test --workspace --locked          (test della UI inclusi)
cargo build --release --locked -p lightvm-desktop -p lightvm-service

lightvm-desktop --renderer auto --smoke 1 --static
lightvm-desktop --glow-hw off --smoke 1 --static
lightvm-desktop --renderer warp --smoke 1 --static
lightvm-desktop --renderer glow,warp --glow-hw off --smoke 1 --static
```

Misure con uno script PowerShell (nella scratchpad della sessione, non nel repository):

- **avvio a caldo**: 20 esecuzioni consecutive per backend di `lightvm-desktop --renderer <glow|wgpu> --smoke 0.5 --static`; primo frame dall'ingresso in `main` (`SMOKE primo_frame_ms`) e durata del processo completo (cronometro esterno);
- **idle UI + supervisor**: `lightvm-desktop --renderer <glow|wgpu> --smoke S --static --catalogo … --runtime-dir … --audit-log …`; dopo 10 s, campioni ogni 5 s di `PrivateMemorySize64` e `WorkingSet64` della UI e del figlio `lightvm-service.exe`; CPU = tempo CPU consumato / tempo trascorso, in percentuale di un core logico. Due serie: `S = 300` (finestra effettiva 271 s, inferiore ai 5 minuti richiesti) e `S = 340` (finestra 311 s).

## Risultati

### Appunti host (F1-13)

| Voce | Esito | Evidenza |
| --- | --- | --- |
| `Paste` con la console a fuoco → `Ctrl+V` al guest come tasti, nessun testo | verificato (headless) | `tests/appunti_ui.rs::paste_con_console_a_fuoco_diventa_ctrl_v_via_qmp`: eventi inviati al canale QMP sostituito da un `Receiver` locale |
| `Shift+Insert` → `Shift+Insert` al guest | verificato (headless) | `shift_insert_diventa_shift_insert_via_qmp` |
| Nessun testo in altri widget | verificato (headless) | `paste_non_produce_testo_in_altri_widget` (campo di testo esterno all'app nello stesso passaggio); controllo: senza l'app il paste inserisce testo (`controllo_senza_app_il_paste_inserisce_testo`) |
| Buffer del `Paste` sovrascritto con zeri, tasto fisico sostituito | verificato (unitario) | `appunti::tests::incolla_diventa_tasto_v_senza_testo`, `shift_insert_diventa_insert`, `senza_incolla_nessuna_modifica` |
| **Lettura degli appunti host** | **non bloccata** | egui-winit 0.36.2 chiama `Clipboard::get()` su Ctrl+V prima che l'app riceva l'input; la feature `clipboard` non è disattivabile da eframe e l'unico gancio precedente richiede `unsafe` (motivazione in `apps/desktop/src/appunti.rs`). Il testo letto è scartato e azzerato, ma il processo della UI lo legge |
| Prova manuale con tastiera fisica e appunti reali | non eseguito | richiede la console reale con QEMU e una persona alla tastiera (P-4) |

### Backend di rendering (F1-16)

| Voce | Esito | Evidenza |
| --- | --- | --- |
| `auto` su hardware → glow | verificato | `SMOKE backend=glow … indicatore="Rendering: glow (OpenGL) · automatico"` |
| glow senza accelerazione (`--glow-hw off`) → ripiego su wgpu DX12 | verificato | `tentativo_fallito glow:egui_glow … requires opengl 2.0+`; `backend=wgpu … "Rendering: wgpu DX12 · ripiego automatico"` |
| WARP forzato | verificato | `backend=warp degradato=true forzato=true … "Rendering: wgpu WARP (software) · forzato"` |
| Elenco `glow,warp` con glow fallito → WARP, modalità degradata | verificato | `backend=warp degradato=true … "· ripiego automatico"` |
| Barra di stato con modalità degradata e tentativi falliti | verificato (istantanea) | `istantanee::istantanea_barra_di_stato_degradata`, `istantanea_barra_di_stato_glow`; `render::tests` (4 test) |
| Sessione RDP | non eseguito | limite L-1 invariato (U-8) |

### Istantanee `egui_kittest` (F1-19, ADR-0022)

| Voce | Esito | Evidenza |
| --- | --- | --- |
| 6 istantanee confrontate con i PNG in `apps/desktop/tests/snapshots/` | verificato | `tests/istantanee.rs`: elenco VM, elenco vuoto, barra di stato degradata e glow, console con frame sintetico, dashboard con elenco; `test result: ok. 6 passed` (renderer wgpu del test, tolleranza in `kittest.toml`) |
| Stesse istantanee su un altro host o GPU | non eseguito | la stabilità fra GPU diverse non è provata (rischio per la CI) |

### Misure (F1-18; host non dedicato, non misure di release)

| Misura | Obiettivo (sez. 13) | Serie | p50 | p95 | Massimo | Errori | Esito |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Avvio a caldo, primo frame — glow | p95 ≤ 3 s | 20 | 117 ms | 139 ms | 140 ms | 0 | entro l'obiettivo |
| Avvio a caldo, processo completo (`--smoke 0.5`) — glow | — | 20 | 633 ms | 642 ms | 645 ms | 0 | registrato |
| Avvio a caldo, primo frame — wgpu DX12 | p95 ≤ 3 s | 20 | 239 ms | 248 ms | 414 ms | 0 | entro l'obiettivo |
| Avvio a caldo, processo completo — wgpu DX12 | — | 20 | 700 ms | 732 ms | 739 ms | 0 | registrato |
| Avvio a freddo | p95 ≤ 3 s | — | — | — | — | — | non eseguito (svuotare la cache dei file richiede un riavvio o privilegi) |

| Idle UI + supervisor | Finestra | CPU UI (% core) | CPU supervisor | Private bytes UI / supervisor (ultimo campione) | Private bytes totali (max) | Working set UI / supervisor | Esito |
| --- | --- | --- | --- | --- | --- | --- | --- |
| glow, serie 1 | 271 s, 54 campioni | 0,006 % | 0 % | 98,0 / 2,4 MiB | 100,8 MiB | 67,1 / 14,1 MiB | entro gli obiettivi; finestra < 5 min |
| glow, serie 2 | 311 s, 62 campioni | 0,141 % | 0 % | 103,0 / 2,4 MiB | 105,5 MiB | 73,3 / 11,0 MiB | entro gli obiettivi (CPU < 1 %, memoria ≤ 250 MiB) |
| wgpu DX12, serie 1 | 271 s, 54 campioni | 0,069 % | 0 % | 489,1 / 2,4 MiB | 491,9 MiB | 183,1 / 10,9 MiB | CPU entro l'obiettivo; **memoria oltre 250 MiB** |
| wgpu DX12, serie 2 | 311 s, 62 campioni | 0,025 % | 0 % | 488,0 / 2,4 MiB | 491,8 MiB | 185,3 / 11,0 MiB | come sopra (deviazione D-12 confermata con il supervisor) |

In tutte e quattro le esecuzioni, a fine smoke la UI ha arrestato il supervisor con la riga `shutdown`: `SMOKE servizio_arresto codice=Some(0) forzato=false`; nessun `lightvm-service.exe` residuo.

## Limiti

- Un solo host; nessuna misura ripetuta su host dedicato (ADR-0018). La CPU di glow nella serie 2 (0,141 %) è più alta della serie 1 (0,006 %): durante la serie 2 giravano test e finestre di smoke sullo stesso desktop; causa non isolata.
- Il supervisor misurato è a riposo, senza VM né QEMU: la memoria di QEMU non è inclusa (sezione 13: si misura il manager).
- Le prove degli appunti sono headless: non attraversano winit né il sistema operativo.
- Le istantanee sono generate e confrontate sullo stesso host.

## Azioni richieste

1. F1-13: decidere come impedire la lettura degli appunti host dentro egui-winit prima di F2 (fork o patch di eframe/egui-winit senza `clipboard`, oppure filtro Win32 in un crate separato con `unsafe` revisionato) — `RELEASE_PLAN.md` F1-13.
2. D-12: ridurre i private bytes con wgpu (limiti del dispositivo, allocatore, rinuncia a wgpu come predefinito su Windows) o dichiarare glow come unico predefinito entro l'obiettivo.
3. Avvio a freddo dopo riavvio dell'host (20 campioni) e sessione RDP (U-8).
4. Prova manuale degli appunti con la console reale insieme a P-4.

## Aggiornamento del 2026-10-10: backend preferito come impostazione

Su richiesta del responsabile del progetto il backend di rendering è un'impostazione in Settings (salvata in `impostazioni-ui.json`, vale dal prossimo avvio; `--renderer` ha la precedenza): automatico (predefinito: glow → wgpu → WARP), glow per primo, wgpu per primo (memoria più alta, D-12), solo WARP. Ogni scelta tranne WARP mantiene il ripiego sugli altri backend. Verifica: `impostazioni::tests::ogni_preferenza_produce_un_piano_valido`, `campi_assenti_valgono_come_predefiniti`, `kittest_ui::settings_salva_politica_e_rendering` (headless). Lettura degli appunti host: **nessuna impostazione** — la roadmap esclude la clipboard dall'MVP (sezioni 4, 6.4, 7.2) e un ADR non può derogarvi; il blocco della lettura dentro egui-winit resta l'azione 1.

## Decisioni proposte

- **ADR-0022:** accettare `egui_kittest` con istantanee come framework di test UI per F1–F2, con il rischio dichiarato della stabilità fra GPU diverse da verificare in CI.


## Aggiornamento del 2026-10-10 (circa 12:00 UTC): lettura degli appunti host bloccata in egui-winit (F1-13)

Sezione aggiunta in coda (regola 8). L'esito "lettura degli appunti host non bloccata" della sezione Risultati è superato da questa prova.

### Modifica

`egui-winit` 0.36.2 è sostituito, tramite `[patch.crates-io]`, da una copia in `vendor/egui-winit` con due modifiche (descritte in `vendor/README.md`, marcate `LightVM` nei sorgenti): `Clipboard::get()` restituisce sempre `None` (copre anche `State::clipboard_text()`, usata da eframe per `RequestPaste`) e la combinazione di incolla non viene più trasformata in `Event::Paste`: prosegue come `Event::Key`, che la console inoltra al guest. Nessun `unsafe`. Il filtro dell'app (`apps/desktop/src/appunti.rs`) resta come seconda difesa.

### Procedura

Script nella scratchpad della sessione: imposta negli appunti di Windows un testo di riconoscimento, avvia `apps/desktop/scripts/prova-tastiera-u4.ps1 -Smoke 45` (console reale sulla ISO Ubuntu), porta in primo piano la finestra "LightVM" e invia con `WScript.Shell.SendKeys` tre Ctrl+V e uno Shift+Insert (eventi di tastiera reali di Windows → winit → egui-winit). Stessa prova con la patch disattivata (riga di `[patch.crates-io]` commentata), poi ripristinata.

### Risultati

| Voce | Esito | Evidenza (`lab/logs/f1-13-ctrl-v*.txt`) |
| --- | --- | --- |
| **Senza patch**: egui-winit legge gli appunti e genera `Paste` | verificato (riferimento) | `appunti_scartati=2` (eventi `Paste` intercettati e azzerati dal filtro dell'app) |
| **Con patch**: nessuna lettura degli appunti, nessun `Paste` | verificato | `appunti_scartati=0` con le stesse combinazioni |
| Workspace con la patch | verificato | `cargo test --workspace`: 356 passati, 0 falliti; clippy `-D warnings` pulito (workspace, varianti glow e wgpu) |
| Combinazione inoltrata al guest come tasti | non eseguito con tastiera di sistema | `eventi_input=0` in entrambe le prove: la console non aveva il focus della tastiera (serve un clic sull'immagine); coperto da P-4 (prova manuale) e dai test headless |

Limite: la patch va riapplicata a ogni aggiornamento di egui/eframe (ADR-0014, ADR-0023). Nota: la prova ha sovrascritto il contenuto degli appunti di Windows dell'host.
