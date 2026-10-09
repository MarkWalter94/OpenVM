# ADR-0021 — SDK e client MCP

- Stato: Accettata per l'SDK (`rmcp` 3.5.1, prova in `docs/verification/F1-06-mcp-sdk.md`); scelta dei client MCP aperta fino alla prova con client reali (F5)
- Data: 2026-10-09
- Responsabile: da assegnare
- Da chiudere entro: F1 per l'SDK (chiusa); F5 per i due client di riferimento

## Contesto

Baseline di protocollo MCP 2025-11-25. In F0 si seleziona una release dell'SDK Rust ufficiale che la supporti, provata con le versioni dei client prescelti; negoziazioni non supportate rifiutate; nessuna dipendenza dal ramo principale (roadmap, sezione 9). F5 richiede almeno due client reali con versioni documentate.

## Decisione

**SDK:** `rmcp` **3.5.1** da crates.io (SDK Rust ufficiale, repository `modelcontextprotocol/rust-sdk`, licenza Apache-2.0), dichiarato con versione esatta `=3.5.1`, `default-features = false` e soltanto le feature `server`, `macros`, `transport-io`. Nessuna dipendenza git né dal ramo principale; lockfile del workspace.

Regole d'uso:

1. Il server restringe `supported_protocol_versions` alla sola **2025-11-25**: in `rmcp` 3.5.1 l'elenco predefinito comprende tutte le revisioni note (dalla 2024-11-05 alla 2026-07-28, che è `ProtocolVersion::LATEST`), quindi senza restrizione il server accetterebbe revisioni diverse dalla baseline.
2. Versioni non supportate: con `initialize` il server risponde con la 2025-11-25, come impone la specifica 2025-11-25 (il client che non la supporta deve disconnettersi); le richieste nel formato per-richiesta delle revisioni successive (metadati `_meta`, 2026-07-28) sono rifiutate con l'errore `-32022` "Unsupported protocol version". Nessuna sessione si conclude con una versione diversa dalla baseline. Un errore esplicito anche su `initialize` richiederebbe una deviazione dalla specifica, da motivare in un aggiornamento di questo ADR.
3. Errori operativi come risultato del tool con `isError: true`; errori di protocollo come errore JSON-RPC (tool sconosciuto → `-32602`); argomenti non validi come errore di esecuzione del tool (regola della 2025-11-25).
4. stdout contiene solo MCP; diagnostica su stderr.
5. Le feature di rete dell'SDK (HTTP, SSE, OAuth, client) sono vietate in `deny.toml` (ADR-0023).
6. L'SDK non valida gli argomenti contro lo schema pubblicato: la validazione resta all'adapter, con gli schemi di `lightvm-proto`.
7. Ogni aggiornamento di `rmcp` ripete il test di integrazione del crate di prova (`crates/lightvm-mcp`) prima di essere accettato.

**Client:** da prendere in F5. Va registrato: due client MCP di riferimento con versione, OS e data della verifica; comportamento dei client sulla controproposta di versione. Nessun client è considerato compatibile prima della prova.

Criteri:

1. Release pubblicata, non ramo principale; lockfile. — soddisfatto (3.5.1 da crates.io).
2. Trasporto stdio funzionante con entrambi i client sull'host target. — server su stdio verificato con un client JSON-RPC di prova su Windows e su Linux (WSL2); client reali non provati.
3. Contenuto immagine e risultati strutturati gestiti dai client come previsto dalla specifica. — risultati strutturati verificati lato server; contenuto immagine e client non provati.
4. Licenza compatibile (ADR-0023). — soddisfatto (Apache-2.0; dipendenze tutte permissive, `F1-06` §4).

## Alternative considerate

- SDK Rust ufficiale (indicato dalla roadmap) contro implementazione interna del protocollo: la seconda solo se l'SDK non supporta la revisione richiesta; non necessaria, perché `rmcp` 3.5.1 supporta la 2025-11-25.
- SDK non ufficiali presenti su crates.io (`rust-mcp-sdk` 2.0.0, `pmcp` 2.22.7, `mcp-core` 0.1.50, `ultrafast-mcp`): non provati; scartati perché la roadmap indica l'SDK ufficiale e questo soddisfa i criteri.
- Client: candidati con supporto stdio da individuare e provare in F5.

## Conseguenze

- Il crate `mcp-server` viene aggiunto in F5 (sezione 6.2); `crates/lightvm-mcp` è soltanto il banco di prova dell'SDK (tool finti `vm_list`, `vm_status`) e non è l'adapter di prodotto.
- L'SDK cambia rapidamente (major 3 introdotta con la revisione 2026-07-28): gli aggiornamenti proposti da Dependabot restano soggetti a revisione e al test di integrazione.
- Configurazioni di esempio pubblicate solo per client realmente provati (sezione 9).

## Prove richieste

- SDK: `docs/verification/F1-06-mcp-sdk.md` (verificato su stdio con client di prova; Windows e Linux in WSL2).
- Client: file di verifica dedicato in F5 con almeno due client reali.

## Riferimenti

Roadmap, sezioni 3, 6.2, 9, 10 (F0, F1, F5), 14, 17.
