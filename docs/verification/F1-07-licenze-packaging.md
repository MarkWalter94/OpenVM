# F1-07 — Inventario delle licenze, politica `cargo-deny` e dipendenze di packaging Linux

- Data di esecuzione (UTC): 2026-10-09
- Esecutore: agente di sviluppo (sessione Claude Code), su richiesta del responsabile del progetto
- Passo della roadmap: sezione 6.0 (licenze dei componenti terzi), 6.3 (versioni fissate e lockfile), 10 F0 ("dipendenze/licenze" chiudibili entro F1) e F1; backlog F1-19 e F1-14 in `docs/RELEASE_PLAN.md`
- ADR collegati: ADR-0023 (aggiornata), ADR-0021, ADR-0014, ADR-0024
- Esito complessivo: **verificato** per l'inventario dal lockfile e per `cargo deny check` sul workspace; **non eseguito** per l'SBOM e per la verifica a runtime delle librerie Linux elencate

Esiti ammessi: **verificato**, **fallito**, **non eseguito** (sezione 17). Log grezzi in `lab/logs/f1-07/` (fuori dal repository): `tree-normal-all.txt`, `tree-normal-build-all.txt`, `tree-prodotto.txt`, `tree-prodotto-x86_64-pc-windows-msvc.txt`, `tree-prodotto-x86_64-unknown-linux-gnu.txt`, `tree-rfb-client.txt`, `licenze-prodotto-conteggi.txt`, `cargo-deny-prima.txt`, `cargo-deny-dopo.txt`.

## Scopo

1. Inventario riproducibile dal lockfile delle licenze di tutte le dipendenze del workspace, inclusi `rfb-client` (ADR-0024) e il nuovo spike MCP (`lightvm-mcp`, F1-06).
2. Elenco delle licenze copyleft o incompatibili con la distribuzione MIT del binario.
3. Politica in `deny.toml` che blocchi licenze non ammesse e sorgenti diverse da crates.io.
4. Elenco delle librerie Linux caricate con `dlopen` da dichiarare nel packaging (F1-14).

Non dimostra: conformità legale (serve revisione umana delle licenze non standard, ADR-0023); SBOM; licenze dei componenti esterni non redistribuiti (QEMU, firmware, guest), che restano fuori dall'installer MVP; presenza effettiva delle librerie Linux a runtime su un host Ubuntu fisico.

## Ambiente

| Voce | Valore |
| --- | --- |
| Host | host F0 di ADR-0018, Windows 11 Pro 10.0.26200 |
| Toolchain | Rust 1.99.0, cargo 1.99.0 |
| Strumenti | `cargo tree` (cargo 1.99.0); `cargo-deny` 0.20.2 già installato nel profilo utente (nessuna installazione in questa prova); `cargo-about` e `cargo-cyclonedx` non installati e non usati |
| Lockfile | `Cargo.lock` del workspace al momento della prova, comprese le dipendenze di `lightvm-mcp` |
| Rete | crates.io tramite cargo; **database RustSec scaricato da GitHub** da `cargo deny check` alla prima esecuzione (comportamento predefinito dello strumento); la seconda esecuzione è `--frozen`, senza rete |

## Procedura

```text
# Inventario completo (tutti i membri, inclusi gli spike), dipendenze normali, tutti i target
cargo tree --workspace -e normal --target all --prefix none --format "{p}|{l}" | sort -u
# Con le dipendenze di build (strumenti usati da build.rs, non collegati nel binario)
cargo tree --workspace -e normal,build --target all --prefix none --format "{p}|{l}" | sort -u
# Insieme di prodotto (senza gli spike come radici), per target
cargo tree -p lightvm-desktop -p lightvm-service -p qemu-backend -p vm-core -p vm-storage \
  -p lightvm-proto -p rfb-client -p lightvm-mcp -e normal --target <triple> --prefix none --format "{p}|{l}"
# Solo rfb-client
cargo tree -p rfb-client -e normal --target all --prefix none --format "{p}|{l}"
# Politica
cargo deny --locked check          # prima della modifica di deny.toml
cargo deny --frozen check          # dopo la modifica, senza rete
# Feature del grafo Linux per l'elenco dlopen
cargo tree -p lightvm-desktop --target x86_64-unknown-linux-gnu -e features --prefix none
```

## Risultati

### 1. Dimensione dell'inventario

| Insieme | Pacchetti distinti (nome + versione) | Di cui crate del workspace |
| --- | --- | --- |
| Workspace completo, dipendenze normali, tutti i target | 426 | 12 |
| Insieme di prodotto, tutti i target | 423 | 9 |
| Insieme di prodotto, `x86_64-pc-windows-msvc` | 227 | 9 |
| Insieme di prodotto, `x86_64-unknown-linux-gnu` | 286 | 9 |
| `rfb-client`, tutti i target | 41 | 1 |
| Solo dipendenze di build aggiunte (`-e normal,build` rispetto a `-e normal`) | 17 | 0 |

I crate del workspace hanno tutti licenza MIT. Nell'insieme di prodotto compare lo spike `f0-console`, perché `apps/desktop` ne dipende (console reale F0-04): uno spike non dovrebbe entrare nel binario pubblicato (vedi Azioni).

### 2. Licenze dichiarate (insieme di prodotto, tutti i target)

Conteggio per espressione SPDX dichiarata, crate del workspace esclusi (`licenze-prodotto-conteggi.txt`).

| Espressione | Pacchetti |
| --- | --- |
| `MIT OR Apache-2.0` / `Apache-2.0 OR MIT` / `MIT/Apache-2.0` / `Apache-2.0/MIT` / `MIT / Apache-2.0` | 198 + 41 + 17 + 2 + 1 |
| `MIT` | 87 |
| `Apache-2.0` | 12 |
| `Zlib OR Apache-2.0 OR MIT`, `MIT OR Apache-2.0 OR Zlib`, `MIT OR Zlib OR Apache-2.0` | 10 + 6 + 2 |
| `Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT` | 7 |
| `Unlicense OR MIT` | 5 |
| `ISC` | 3 |
| `Zlib`, `BSL-1.0`, `0BSD` | 2 ciascuna |
| `BSD-3-Clause OR MIT OR Apache-2.0`, `BSD-3-Clause OR Apache-2.0`, `BSD-2-Clause OR Apache-2.0 OR MIT` | 2 ciascuna |
| `MIT OR Apache-2.0 OR LGPL-2.1-or-later` | 2 |
| `BSD-3-Clause`, `Apache-2.0 OR ISC OR MIT`, `Apache-2.0 OR GPL-2.0-only`, `Apache-2.0 AND MIT`, `Apache-2.0 AND ISC`, `0BSD OR MIT OR Apache-2.0`, `0BSD OR Apache-2.0`, `(MIT OR Apache-2.0) AND Unicode-3.0`, `(MIT OR Apache-2.0) AND OFL-1.1 AND Ubuntu-font-1.0` | 1 ciascuna |

### 3. Licenze copyleft, non standard o con obblighi da gestire

| Crate | Licenza | Target | Valutazione |
| --- | --- | --- | --- |
| `self_cell` 1.3.0 | `Apache-2.0 OR GPL-2.0-only` | tutti | scelta Apache-2.0; nessun obbligo copyleft |
| `r-efi` 5.3.0, 6.0.0 | `MIT OR Apache-2.0 OR LGPL-2.1-or-later` | solo UEFI (non presente nei grafi Windows e Linux) | scelta MIT o Apache-2.0 |
| `epaint_default_fonts` 0.36.2 | `(MIT OR Apache-2.0) AND OFL-1.1 AND Ubuntu-font-1.0` | tutti (font incorporati nella UI) | **non standard**: font con licenze proprie, non copyleft per il software ma con condizioni (testo di licenza da distribuire; restrizioni sul nome dei font modificati). Ammessa solo come eccezione per questo crate; revisione umana richiesta (ADR-0023) |
| `ring` 0.17.14 | `Apache-2.0 AND ISC` | tutti (da `rustls`, `rfb-client`) | permissiva; contiene codice C e assembly derivato da BoringSSL compilato nel binario; avvisi da riportare |
| `rmcp`, `rmcp-macros` 3.5.1; `winit` 0.30.13; `glutin` 0.32.3 e `glutin_*_sys`; `accesskit_winit` 0.32.2; `codespan-reporting` 0.13.1; `spirv` 0.4.0; `gethostname` 1.1.0 (Linux); `unicode-general-category` 1.1.0 | `Apache-2.0` | vari | permissiva, compatibile con la distribuzione MIT del binario; richiede testo della licenza e avvisi `NOTICE` eventuali nell'inventario di terze parti |
| `clipboard-win` 5.4.1, `error-code` 3.4.0 | `BSL-1.0` | Windows | permissiva |
| `libsqlite3-sys` 0.38.2 (feature `bundled` di `rusqlite`) | `MIT` (il crate); SQLite è di pubblico dominio | tutti | sorgente C di SQLite compilato nel binario; da riportare nell'SBOM come componente C |
| `unicode-ident` | `(MIT OR Apache-2.0) AND Unicode-3.0` | solo macro procedurali (compilazione) | non collegato nel binario |

**Esito: nessuna licenza copyleft obbligatoria e nessuna licenza incompatibile con MIT.** Le sole opzioni copyleft (GPL-2.0-only, LGPL-2.1-or-later) compaiono in espressioni `OR` con alternative permissive. Le licenze dei font di egui sono l'unico caso non standard.

### 4. `rfb-client` (ADR-0024)

41 pacchetti (`tree-rfb-client.txt`): `flate2`/`miniz_oxide`/`adler2`/`crc32fast`/`simd-adler32` (ZRLE), `socket2`, `uds_windows` (Windows), `rustls` + `ring` + `rustls-webpki` + `untrusted` + `subtle` + `zeroize` + `rustls-pki-types` (VeNCrypt), `tempfile`, `getrandom`, famiglie `windows-*` e `rustix`/`linux-raw-sys`. Licenze: tutte permissive (`ISC` per `rustls-webpki` e `untrusted`; `BSD-3-Clause` per `subtle`; `Apache-2.0 AND ISC` per `ring`; le altre MIT, Apache-2.0 o doppie). Nessun problema.

### 5. `cargo deny check`

| Esecuzione | Esito | Evidenza |
| --- | --- | --- |
| Prima della modifica (`--locked`, con aggiornamento del database RustSec) | verificato | `advisories ok, bans ok, licenses ok, sources ok`, `EXIT=0` (`cargo-deny-prima.txt`) |
| Dopo la modifica di `deny.toml` (`--frozen`) | verificato | `advisories ok, bans ok, licenses ok, sources ok`, `EXIT=0`; 22 avvisi `duplicate` non bloccanti (`bitflags`, `getrandom`, `windows-sys`, `syn`, `thiserror`, `accesskit_consumer`, famiglie `objc2`/`calloop`/`smithay-client-toolkit`, ecc.) (`cargo-deny-dopo.txt`) |
| Ripetizione finale sul lockfile corrente (`--frozen`, 15:51 UTC, dopo le modifiche di altri flussi al lockfile) | verificato | `advisories ok, bans ok, licenses ok, sources ok`, `EXIT=0`, 22 avvisi `duplicate` (`cargo-deny-finale.txt`) |
| Prova negativa delle nuove regole (crate o feature vietati introdotti apposta) | **non eseguito** | le regole sono verificate solo sul grafo attuale, che non contiene crate vietati |

Modifiche a `deny.toml`:

- commento che rende esplicito il diniego di tutto ciò che non è in `allow` (GPL, LGPL, AGPL, MPL, EPL, CDDL, SSPL, licenze sconosciute) e il trattamento delle espressioni `OR`;
- `[bans] deny`: `reqwest`, `hyper` (profilo offline, ADR-0004), `oauth2` (MCP su stdio senza OAuth HTTP), `openssl-sys`, `native-tls` (TLS solo via rustls, ADR-0013), `aws-lc-sys` (CMake/NASM sull'host di build, F0-04), ciascuno con motivazione;
- `[bans] features`: feature HTTP, OAuth e client di `rmcp` vietate (ADR-0021);
- `[sources]`: invariato nella sostanza (solo crates.io, nessun git, nessun registro sconosciuto), aggiunto `[sources.allow-org]` vuoto.

### 6. Librerie Linux caricate con `dlopen` (F1-14)

Dal grafo delle feature di `lightvm-desktop` per `x86_64-unknown-linux-gnu` (`winit` con `x11` e `wayland`, `xkbcommon-dl` con `x11`, `wayland-sys` con `dlopen` ed `egl`, `x11-dl`, `x11rb` con `dl-libxcb`, `glutin` con `egl` e `glx`, `wgpu-hal` con `vulkan` e `gles`) e dall'osservazione di F0-07 (panico X11 senza `libxkbcommon-x11.so.0`). I nomi dei pacchetti sono quelli di Ubuntu LTS. **Elenco derivato dal codice e dalle feature, non verificato con `ldd`/tracciamento su host Ubuntu fisico.**

| Libreria (soname) | Pacchetto Ubuntu | Caricata da | Quando serve |
| --- | --- | --- | --- |
| `libxkbcommon.so.0` | `libxkbcommon0` | `xkbcommon-dl` | sempre (tastiera X11 e Wayland) |
| `libxkbcommon-x11.so.0` | `libxkbcommon-x11-0` | `xkbcommon-dl` (feature `x11`) | sessioni X11; assente → panico (D-17) |
| `libwayland-client.so.0` | `libwayland-client0` | `wayland-sys` (`dlopen`) | sessioni Wayland |
| `libwayland-cursor.so.0` | `libwayland-cursor0` | `wayland-sys` tramite `winit`/`sctk` | cursori in Wayland |
| `libwayland-egl.so.1` | `libwayland-egl1` | `wayland-sys` (feature `egl`) | glow/GLES su Wayland |
| `libX11.so.6`, `libX11-xcb.so.1`, `libXcursor.so.1`, `libXrandr.so.2`, `libXi.so.6` | `libx11-6`, `libx11-xcb1`, `libxcursor1`, `libxrandr2`, `libxi6` | `x11-dl` (winit, glutin) | sessioni X11 |
| `libxcb.so.1` | `libxcb1` | `x11rb` (`dl-libxcb`) | sessioni X11 |
| `libEGL.so.1` | `libegl1` (+ driver Mesa `libegl-mesa0`) | `glutin` (`egl`), `wgpu-hal` (`gles`) | backend glow/GLES su EGL |
| `libGL.so.1` / GLX | `libgl1` (+ `libgl1-mesa-dri`) | `glutin` (`glx`) | backend glow su X11 senza EGL |
| `libvulkan.so.1` | `libvulkan1` (+ ICD, per esempio `mesa-vulkan-drivers`) | `wgpu-hal` (`vulkan`) | backend wgpu |

Non richiedono librerie di sistema: SQLite (incluso, `bundled`), TLS (`rustls` + `ring`, statici), AccessKit su Linux (`zbus` in Rust puro; serve solo un bus di sessione per l'accessibilità a runtime). Lo stesso elenco è usato nel workflow CI (`APT_RUNTIME`, `.github/workflows/ci.yml`) e dovrà comparire nelle dipendenze del pacchetto Linux (sezione 5, righe Ubuntu e Debian/Fedora; R2a).

## Limiti

- `cargo tree` riporta il campo `license` dichiarato nei manifest: non legge i file di licenza né rileva codice incluso con licenza diversa (per esempio sorgenti C incorporati). `cargo-deny` usa anche il riconoscimento dei file di licenza con soglia 0,8.
- Gli elenchi `cargo tree` sono stati estratti alle 15:2x UTC su un lockfile che altri flussi stavano modificando; le dipendenze di sviluppo (per esempio quelle delle istantanee egui_kittest) non sono incluse negli elenchi, ma lo sono nel controllo `cargo deny` finale.
- L'insieme "tutti i target" include piattaforme non dichiarate (macOS, wasm, UEFI): sovrastima prudente.
- Nessun SBOM prodotto; `cargo-about` e `cargo-cyclonedx` non installati (regola del compito).
- La prima esecuzione di `cargo deny` ha scaricato il database RustSec da GitHub.
- L'elenco `dlopen` non è verificato a runtime su Ubuntu fisico; lo smoke `smoke-render-software` in CI (non ancora eseguito, `F1-08-ci.md`) è la prima verifica prevista.
- Nessuna revisione legale.

## Azioni richieste

1. F6: generare l'inventario di terze parti con testi di licenza e avvisi (Apache-2.0 `NOTICE`, font OFL/Ubuntu, `ring`) e l'SBOM (candidati: `cargo-about`, `cargo-cyclonedx`), con il componente C di SQLite e di `ring` dichiarati.
2. Revisione umana delle licenze dei font di `epaint_default_fonts`, oppure sostituzione con font di licenza nota (ADR-0023).
3. Rimuovere la dipendenza di `apps/desktop` dallo spike `f0-console` prima di F6 (codice di prodotto non deve dipendere da `spikes/`).
4. F1-14 / R2a: verificare l'elenco `dlopen` con `ldd`, `LD_DEBUG=libs` o `strace -e openat` durante lo smoke su Ubuntu (CI e host fisico) e dichiararlo nel pacchetto.
5. Prova negativa di `deny.toml` (crate o feature vietati introdotti su un ramo di prova) alla prima esecuzione della CI.

## Decisioni proposte

- **ADR-0023:** politica = elenco chiuso di licenze permissive in `deny.toml` (MIT, Apache-2.0, Apache-2.0 WITH LLVM-exception, BSD-2/3-Clause, BSL-1.0, ISC, Unicode-3.0, Unlicense, Zlib, 0BSD), eccezione nominativa per i font di egui, espressioni `OR` risolte sul ramo permissivo; strumenti `cargo-deny` 0.20.2 in CI (licenze, advisory RustSec senza eccezioni, sorgenti solo crates.io, crate e feature vietati); SBOM e inventario di terze parti in F6 con strumento da provare; componenti esterni (QEMU, firmware, guest) documentati e non redistribuiti.
- **F1-14:** elenco delle librerie `dlopen` della sezione 6 come dipendenze dichiarate del pacchetto Linux.

## File prodotti

- `deny.toml` (aggiornato)
- `docs/verification/F1-07-licenze-packaging.md` (questo file)
- `lab/logs/f1-07/*` (fuori dal repository)
