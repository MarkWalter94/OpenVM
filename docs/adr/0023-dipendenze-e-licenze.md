# ADR-0023 — Dipendenze e licenze

- Stato: Accettata per politica delle licenze, sorgenti e controlli in CI (prove in `docs/verification/F1-04-service-ipc-ci.md` e `docs/verification/F1-07-licenze-packaging.md`); formato e strumento dell'SBOM e revisione dei font di egui aperti fino a F6
- Data: 2026-10-09
- Responsabile: da assegnare
- Da chiudere entro: F1 (politica e controlli: chiusa); verifica finale, SBOM e inventario di terze parti in F6

## Contesto

Il codice originale di LightVM è distribuito con licenza MIT; le licenze dei componenti terzi vanno inventariate e la compatibilità della distribuzione verificata in F0/F6 (roadmap, sezione 6.0). Versioni fissate e lockfile (sezione 6.3). F6 richiede SBOM, inventario licenze e gestione vulnerabilità. Una versione vulnerabile non si congela per rispettare il testo (sezione 17).

## Decisione

1. **Licenze ammesse** per le dipendenze collegate nel binario: elenco chiuso in `deny.toml` — `MIT`, `Apache-2.0`, `Apache-2.0 WITH LLVM-exception`, `BSD-2-Clause`, `BSD-3-Clause`, `BSL-1.0`, `ISC`, `Unicode-3.0`, `Unlicense`, `Zlib`, `0BSD`. Tutto il resto è negato (GPL, LGPL, AGPL, MPL, EPL, CDDL, SSPL, licenze sconosciute). Un'espressione `OR` passa se contiene un ramo ammesso, che è quello usato (per esempio `self_cell`: `Apache-2.0 OR GPL-2.0-only`).
2. **Eccezioni nominative**: solo `epaint_default_fonts` (`OFL-1.1`, `Ubuntu-font-1.0`, font incorporati da egui), con revisione umana richiesta prima di F6.
3. **Sorgenti**: solo crates.io; nessuna dipendenza git, nessun registro alternativo.
4. **Crate e feature vietati** (`[bans]`): client e stack HTTP (`reqwest`, `hyper`), `oauth2`, TLS di sistema (`openssl-sys`, `native-tls`), `aws-lc-sys`; feature HTTP/OAuth/client dell'SDK MCP (ADR-0021). Ogni voce porta la motivazione.
5. **Strumenti in CI**: `cargo-deny` 0.20.2 (licenze, advisory RustSec senza eccezioni, sorgenti, divieti, duplicati come avviso) nel job `cargo-deny`; Dependabot settimanale con gruppi.
6. **Inventario riproducibile** dal lockfile con `cargo tree -e normal --target all --format "{p}|{l}"` (procedura in `F1-07`); in F6 inventario di terze parti con testi di licenza e avvisi, e SBOM.
7. **Componenti esterni non redistribuiti** (QEMU, firmware, guest): documentati nella guida con versione, provenienza e hash, non inclusi nell'installer MVP.
8. **Nuove dipendenze**: accettate solo con versione esatta, licenza nell'elenco, nessuna feature di rete non necessaria, manutenzione attiva e necessità motivata nel commento del `Cargo.toml`; per librerie VNC/RFB vale ADR-0003/ADR-0024.

Da decidere ancora (F6): strumento e formato dell'SBOM (candidati `cargo-cyclonedx`, `cargo-about`; nessuno provato); trattamento dei font di egui.

Criteri:

1. Ogni dipendenza ha licenza nota e compatibile con la distribuzione MIT del binario. — soddisfatto sul lockfile attuale (`F1-07` §2–§3): nessuna licenza copyleft obbligatoria, unico caso non standard i font di egui.
2. Advisory noti controllati a ogni modifica. — job `cargo-deny` definito; verificato in locale, CI non ancora eseguita (`F1-08`).
3. Inventario riproducibile dal lockfile. — soddisfatto (`F1-07`, procedura).

## Alternative considerate

`cargo-deny` per licenze e advisory (scelto), `cargo-audit` per advisory (coperto da `cargo-deny`), `cargo-about` o `cargo-cyclonedx` per inventario e SBOM (da provare in F6). La scelta degli strumenti non sostituisce la revisione umana delle licenze non standard.

## Conseguenze

- Una dipendenza con licenza incompatibile o non manutenuta viene sostituita o il codice reso interno.
- La gestione vulnerabilità in `docs/SECURITY.md` si appoggia a questi controlli.
- Il pacchetto Linux dichiara le librerie caricate con `dlopen` elencate in `F1-07` §6 (F1-14).
- Lo spike `f0-console`, oggi dipendenza di `apps/desktop`, va rimosso dal grafo di prodotto prima di F6.

## Prove richieste

- Primo controllo con `cargo-deny`: `docs/verification/F1-04-service-ipc-ci.md`.
- Inventario completo (incluso `rfb-client` e l'SDK MCP), politica aggiornata ed elenco `dlopen`: `docs/verification/F1-07-licenze-packaging.md`.
- F6: inventario di terze parti e SBOM sull'artefatto finale.

## Riferimenti

Roadmap, sezioni 6.0, 6.3, 6.4, 10 (F0, F1, F6), 14, 17.
