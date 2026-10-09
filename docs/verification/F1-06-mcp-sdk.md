# F1-06 — SDK Rust MCP: selezione e prova su stdio con la revisione 2025-11-25

- Data di esecuzione (UTC): 2026-10-09
- Esecutore: agente di sviluppo (sessione Claude Code), su richiesta del responsabile del progetto
- Passo della roadmap: sezione 9 (baseline MCP 2025-11-25, SDK Rust ufficiale, rifiuto delle negoziazioni non supportate, stdout solo MCP); sezione 10 F0 ("Chiudibili entro F1: SDK/client MCP compatibili con 2025-11-25") e F1; backlog F1-19 in `docs/RELEASE_PLAN.md`
- ADR collegati: ADR-0021 (aggiornata), ADR-0023 (licenze)
- Esito complessivo: **parziale** — SDK scelto e provato su stdio con un client JSON-RPC scritto a mano, su Windows e su Linux (WSL2); **nessun client MCP reale provato** (richiesto da ADR-0021 e da F5)

Esiti ammessi: **verificato**, **fallito**, **non eseguito** (sezione 17). Log grezzi in `lab/logs/f1-06/` (fuori dal repository): `test-windows.txt`, `test-linux-wsl.txt`, `trascrizione.txt`, `negoziazione.txt`, `tree-licenze.txt`, `comandi-finali.txt`, `check-workspace-errore.txt`.

## Scopo

Scegliere la release dell'SDK Rust per MCP da usare nell'adapter stdio (F5) e provare, prima di scrivere codice di prodotto, che:

1. supporti la revisione 2025-11-25 con trasporto stdio;
2. consenta di limitare la negoziazione alla sola 2025-11-25;
3. pubblichi tool con JSON Schema di ingresso e di uscita e restituisca risultati strutturati;
4. distingua errori operativi (risultato con `isError: true`) da errori di protocollo (errore JSON-RPC), come chiede la sezione 9;
5. scriva su stdout soltanto messaggi MCP;
6. abbia dipendenze con licenze compatibili con la distribuzione MIT (ADR-0023) e nessuna dipendenza di rete nel server.

Non dimostra: compatibilità con client MCP reali (nessuno provato), contenuto immagine (`image`) nei risultati, collegamento al supervisor, autorizzazione, grant e lease (F4–F5), prestazioni, robustezza contro input avversari oltre ai casi elencati sotto.

## Ambiente

| Voce | Valore |
| --- | --- |
| Host | host F0 di ADR-0018 (Intel Core i7-12800HX), condiviso con altri agenti |
| OS e build | Windows 11 Pro 10.0.26200; Linux in WSL2, kernel 6.18.33.1-microsoft-standard-WSL2, Ubuntu (distro predefinita) |
| Acceleratore e stato | non pertinente |
| Toolchain / versioni rilevanti | Rust 1.99.0 (`x86_64-pc-windows-msvc`, ambiente Build Tools 2022 caricato con `vcvars64.bat`); in WSL `cargo 1.99.0` con linker zig di `lab/wsl/env.sh` (F0-07); Node v24.15.0 per le trascrizioni |
| SDK | `rmcp` 3.5.1 da crates.io, feature `server`, `macros`, `transport-io` (senza le predefinite) |
| Binario QEMU, firmware, guest | non pertinenti |
| Sessione grafica | non pertinente (processo da console) |
| Rete | solo crates.io tramite cargo (`cargo info`, download dei crate); nessun accesso a GitHub o ad altri siti per questa prova |

## Confronto dei candidati

Fonte: `cargo info <crate>` su crates.io, 2026-10-09. La roadmap (sezione 9) indica l'SDK Rust ufficiale; le alternative servono solo se quello non supporta la revisione richiesta (ADR-0021).

| Crate | Versione | Licenza | Origine | Valutazione |
| --- | --- | --- | --- | --- |
| `rmcp` | 3.5.1 | Apache-2.0 | `modelcontextprotocol/rust-sdk` (SDK ufficiale), `rust-version` 1.88 | **scelto**: release pubblicata; costanti per 2024-11-05, 2025-03-26, 2025-06-18, **2025-11-25**, 2026-07-28; elenco delle versioni supportate restringibile dal server; trasporto stdio con feature separata; feature di rete (HTTP, OAuth, processo figlio) tutte opzionali |
| `mcp-server` | 0.1.0 | MIT | stesso campo `repository` di `rmcp` | escluso senza prova: le release correnti dell'SDK ufficiale sono pubblicate come `rmcp`; non approfondito |
| `rust-mcp-sdk` | 2.0.0 | MIT | `rust-mcp-stack` (non ufficiale) | non provato: non ufficiale; supporto della 2025-11-25 non verificato |
| `pmcp` | 2.22.7 | MIT | `paiml/pmcp` (non ufficiale) | non provato: non ufficiale; supporto della 2025-11-25 non verificato |
| `mcp-core` | 0.1.50 | Apache-2.0 | non ufficiale | non provato |
| `ultrafast-mcp` | 202506018.1.0 | MIT OR Apache-2.0 | non ufficiale | non provato; schema di versione non semantico |
| Implementazione interna | — | MIT | — | non necessaria: l'SDK ufficiale supporta la revisione |

Osservazione importante: in `rmcp` 3.5.1 `ProtocolVersion::LATEST` è **2026-07-28** (revisione senza `initialize`, con metadati per richiesta) e l'elenco predefinito delle versioni supportate le include tutte, dalla 2024-11-05 alla 2026-07-28. Senza restrizione esplicita il server accetterebbe revisioni diverse dalla baseline: il server di prova sovrascrive `supported_protocol_versions` con la sola 2025-11-25.

## Procedura

1. Crate `crates/lightvm-mcp` (membro del workspace tramite il glob `crates/*`, nessuna modifica al `Cargo.toml` di radice), senza `unsafe`, con lint del workspace:
   - `src/lib.rs`: `SpikeServer` con `#[tool_router]`/`#[tool_handler]`; tool `vm_list` (nessun argomento, risultato `{ vms: [...] }`) e `vm_status` (`{ vm_id }` con `deny_unknown_fields`, lunghezza 1–64; risultato strutturato con `simulated: true`); catalogo statico finto; VM sconosciuta → `Err(String)` (risultato con `isError: true`); `get_info` con `protocol_version = 2025-11-25` e capacità `tools`; `supported_protocol_versions` = `[2025-11-25]`.
   - `src/main.rs`: binario `lightvm-mcp-spike`, `serve(stdio())` su runtime tokio a thread singolo; diagnostica solo su stderr.
   - Dipendenze con versioni esatte: `rmcp =3.5.1` (`default-features = false`, `server`, `macros`, `transport-io`), `schemars =1.2.2`, `serde =1.0.229`, `serde_json =1.0.151`, `tokio =1.53.2` (`io-std`, `macros`, `rt`), allineate a quelle già presenti nel lockfile.
2. Test di integrazione `tests/stdio.rs`: avvia il binario come processo figlio (`CARGO_BIN_EXE_lightvm-mcp-spike`) e parla JSON-RPC 2.0 delimitato da newline con un client scritto con la sola libreria standard e `serde_json` (non usa il codice client dell'SDK). Timeout di 20 s per risposta. Ogni riga di stdout deve essere JSON-RPC 2.0. Test:
   - `initialize_tools_list_e_tools_call`: `initialize` (2025-11-25) → `notifications/initialized` → `tools/list` → `tools/call` di `vm_list`, `vm_status` esistente, `vm_status` sconosciuta, tool inesistente, argomento extra; poi chiusura di stdin e uscita con codice 0;
   - `versione_precedente_riceve_la_baseline`: `initialize` con 2025-06-18;
   - `revisione_successiva_senza_initialize_rifiutata`: `tools/list` con `_meta` della 2026-07-28;
   - `richiesta_prima_di_initialize_non_eseguita`: `tools/call` senza handshake.
3. Comandi (Windows, dalla radice, con l'ambiente MSVC caricato):

   ```text
   cargo fmt -p lightvm-mcp -- --check
   cargo clippy -p lightvm-mcp --all-targets -- -D warnings
   cargo test -p lightvm-mcp            # 3 esecuzioni
   cargo check --workspace --all-targets
   ```

4. Linux (WSL2): `. lab/wsl/env.sh && cargo test -p lightvm-mcp --locked`, 3 esecuzioni.
5. Trascrizioni del protocollo con uno script Node che invia i messaggi sullo stdin del binario di debug e registra stdout e stderr (`trascrizione.txt`, `negoziazione.txt`).
6. Licenze delle dipendenze: `cargo tree -p lightvm-mcp -e normal --prefix none --format "{p} {l}"` (target host) e `cargo deny check` sul workspace (vedi `F1-07-licenze-packaging.md`).

## Risultati

### 1. Test

| Voce | Esito | Evidenza |
| --- | --- | --- |
| Test unitari (`solo_la_baseline_e_supportata`, `catalogo_dichiarato_simulato`) | verificato | Windows e WSL2: `2 passed` in 3 esecuzioni su 3 |
| Test di integrazione su stdio (4 test) | verificato | Windows: `4 passed` in 3 esecuzioni su 3 (`test-windows.txt`); WSL2: `4 passed` in 3 esecuzioni su 3 (`test-linux-wsl.txt`) |
| `cargo fmt -p lightvm-mcp -- --check` | verificato | `EXIT=0` |
| `cargo clippy -p lightvm-mcp --all-targets -- -D warnings` | verificato | nessun avviso, `EXIT=0` |
| `cargo check --workspace --all-targets` (prima esecuzione) | **fallito** per cause esterne a questo crate | `crates\lightvm-service\tests\ipc.rs:10:22` `unresolved import lightvm_service::compat` e `:19:34` `unresolved import lightvm_service::supervisor::fake` (lavoro in corso di un altro agente; `check-workspace-errore.txt`) |
| `cargo check --workspace --all-targets` (ripetizione, 15:45 UTC) | verificato | `Finished`, `EXIT=0` (`comandi-finali.txt`; sezione 5) |

### 2. Protocollo (trascrizione su Windows)

| Passo | Esito | Risposta osservata |
| --- | --- | --- |
| `initialize` 2025-11-25 | verificato | `protocolVersion: "2025-11-25"`, `capabilities: {"tools":{}}`, `serverInfo.name: "lightvm-mcp-spike"` |
| `tools/list` | verificato | `vm_list` e `vm_status` con `inputSchema` e `outputSchema` di tipo `object`; dialetto `https://json-schema.org/draft/2020-12/schema` (predefinito nella 2025-11-25); `vm_status` con `required: ["vm_id"]`, `additionalProperties: false`, `minLength: 1`, `maxLength: 64` |
| `tools/call vm_list` | verificato | `structuredContent.vms` con 2 voci, `content[0]` testuale con lo stesso JSON, `isError: false` |
| `tools/call vm_status` (`vm-demo-1`) | verificato | `structuredContent` con `state: "stopped"`, `simulated: true` |
| VM sconosciuta (errore operativo) | verificato | `result.isError: true`, testo `vm_not_found: …`, nessun errore JSON-RPC |
| Tool inesistente (errore di protocollo) | verificato | `error.code: -32602`, `message: "tool not found"` |
| Argomento sconosciuto (`extra`) | verificato | `result.isError: true`, `failed to deserialize parameters: unknown field 'extra', expected 'vm_id'`; nessun `structuredContent` (errore di esecuzione del tool, come chiede la 2025-11-25 per la validazione degli input) |
| stdout solo MCP | verificato | ogni riga di stdout è JSON-RPC 2.0 (asserzione del test); stderr contiene solo `avvio su stdio…` e `chiusura (Closed)` |
| Chiusura di stdin | verificato | il server termina con codice 0 dopo la chiusura dello stdin |

### 3. Negoziazione della versione (`negoziazione.txt`)

| Richiesta del client | Esito | Risposta |
| --- | --- | --- |
| `initialize` con 2025-06-18 | verificato | risultato con `protocolVersion: "2025-11-25"` |
| `initialize` con 2026-07-28 | verificato | risultato con `protocolVersion: "2025-11-25"` |
| `initialize` con `1999-01-01` | verificato | risultato con `protocolVersion: "2025-11-25"` |
| `tools/list` con `_meta` 2026-07-28, senza `initialize` | verificato | `error.code: -32022`, `Unsupported protocol version`, `data.supported: ["2025-11-25"]` |
| `server/discover` con `_meta` 2026-07-28 | verificato | stesso errore `-32022` |
| `tools/call` prima di `initialize`, senza `_meta` | verificato | `error.code: -32602` (metadati obbligatori mancanti); il tool non viene eseguito |
| Valore JSON che non è un oggetto (`"{non json"`) | verificato | `error.code: -32600`, `Invalid request` |

Il server non conclude mai una sessione con una versione diversa da 2025-11-25. Con `initialize` la regola della specifica 2025-11-25 ("basic/lifecycle") impone di rispondere con un'altra versione supportata, non con un errore: il rifiuto effettivo spetta al client, che deve disconnettersi se non supporta la versione proposta. Le richieste nel formato della 2026-07-28 ricevono invece un errore esplicito.

### 4. Licenze delle dipendenze dell'SDK

`cargo tree -p lightvm-mcp -e normal` (target host Windows): 55 pacchetti di terze parti distinti, proc-macro inclusi (`tree-licenze.txt`).

| Licenza dichiarata | Pacchetti |
| --- | --- |
| `MIT OR Apache-2.0` | 31 |
| `MIT` | 15 (tra cui `tokio`, `tokio-util`, `tracing*`, `schemars*`, `darling*`, `bytes`, `slab`, `strsim`, `zmij`) |
| `Apache-2.0 OR MIT` | 4 |
| `Apache-2.0` | 2 (`rmcp`, `rmcp-macros`) |
| `Unlicense OR MIT` | 1 (`memchr`) |
| `MIT/Apache-2.0` | 1 (`ident_case`) |
| `(MIT OR Apache-2.0) AND Unicode-3.0` | 1 (`unicode-ident`, solo per le macro procedurali) |

Nessuna licenza copyleft. `rmcp` è solo Apache-2.0: compatibile con la distribuzione del binario sotto MIT, a condizione di riportarne testo e avvisi nell'inventario di terze parti (F6). Nessuna dipendenza di rete nel grafo: niente `reqwest`, `hyper`, `oauth2`, TLS; `deny.toml` ora vieta queste feature di `rmcp` e questi crate (F1-07). `uuid` e `getrandom` arrivano dalla feature `server` dell'SDK.

### 5. Ripetizione finale dei comandi (2026-10-09 15:45 UTC, `comandi-finali.txt`)

`cargo fmt -p lightvm-mcp -- --check` `EXIT=0`; `cargo clippy -p lightvm-mcp --all-targets -- -D warnings` senza avvisi; `cargo test -p lightvm-mcp` `2 passed` + `4 passed`; `cargo check --workspace --all-targets` `EXIT=0`.

## Limiti

- **Nessun client MCP reale.** Il client del test è scritto a mano: dimostra la conformità del server ai messaggi usati, non l'interoperabilità con client esistenti. ADR-0021 e la sezione 9 richiedono due client reali con versione documentata (F5).
- Contenuto immagine (`type: "image"`), notifiche `tools/list_changed`, cancellazione, progresso, `ping` e limiti di dimensione dei messaggi non provati.
- L'SDK non valida gli argomenti contro lo schema pubblicato: `minLength`/`maxLength` sono solo dichiarati; la validazione effettiva resta a carico dell'adapter (sezione 9: "risultati strutturati validati rispetto agli schemi").
- I messaggi di errore dell'SDK sono in inglese e includono il dettaglio di deserializzazione di serde; per i messaggi pubblici l'adapter dovrà mappare gli errori sui codici stabili di `lightvm-proto`.
- Lettura dello stdin: il trasporto stdio dell'SDK non è stato provato con messaggi molto grandi o con input ostili (fuzzing previsto dalla sezione 13).
- L'SDK è giovane e cambia rapidamente (major 3 con la revisione 2026-07-28): ogni aggiornamento va riprovato con questo test; Dependabot lo propone come aggiornamento `cargo` soggetto a revisione.
- In WSL2 la build usa il linker zig (F0-07), non la toolchain standard di Ubuntu.
- Host condiviso con altri agenti; il workspace era in evoluzione durante la prova.

## Azioni richieste

1. F5: provare almeno due client MCP reali con trasporto stdio (versione, OS, data) e aggiornare `docs/COMPATIBILITY.md`, sezione "Client MCP provati".
2. F5: nell'adapter, validare gli argomenti contro gli schemi generati da `lightvm-proto` prima di inoltrarli al supervisor e mappare gli errori su codici pubblici stabili.
3. Provare il contenuto immagine con limiti di byte (sezione 9) e il comportamento dei client scelti.
4. Fuzzing mirato del trasporto stdio e della deserializzazione dei parametri (sezione 13).
5. A ogni aggiornamento di `rmcp`: ripetere `cargo test -p lightvm-mcp` e verificare che `supported_protocol_versions` resti limitato alla baseline.

## Decisioni proposte

- **ADR-0021:** adottare `rmcp` 3.5.1 (`=3.5.1`, feature `server`, `macros`, `transport-io`, senza predefinite), con `supported_protocol_versions` limitato a `2025-11-25`; le feature HTTP/OAuth/client restano vietate in `deny.toml`. Scelta dei client rinviata a F5.
- Comportamento sulle versioni non supportate: con `initialize` controproposta della 2025-11-25 come impone la specifica (il client decide se disconnettersi); richieste nel formato per-richiesta di revisioni successive rifiutate con `-32022`. Se si vuole un errore esplicito anche su `initialize`, serve una deviazione motivata dalla specifica.

## File prodotti

- `crates/lightvm-mcp/Cargo.toml`, `src/lib.rs`, `src/main.rs`, `tests/stdio.rs`
- `Cargo.lock` (aggiornato da cargo con le nuove dipendenze)
- `docs/verification/F1-06-mcp-sdk.md` (questo file)
- `lab/logs/f1-06/*` (fuori dal repository)
