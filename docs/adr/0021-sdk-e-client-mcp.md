# ADR-0021 — SDK e client MCP

- Stato: Proposta
- Data: 2026-10-09
- Responsabile: da assegnare
- Da chiudere entro: F1 (avviata in F0, attività non bloccante)

## Contesto

Baseline di protocollo MCP 2025-11-25. In F0 si seleziona una release dell'SDK Rust ufficiale che la supporti, provata con le versioni dei client prescelti; negoziazioni non supportate rifiutate; nessuna dipendenza dal ramo principale (roadmap, sezione 9). F5 richiede almeno due client reali con versioni documentate.

## Decisione

Da prendere. Va registrato:

- release esatta dell'SDK Rust ufficiale e supporto dichiarato e provato per 2025-11-25;
- due client MCP di riferimento con versione, OS e data della verifica;
- comportamento sulle versioni di protocollo non supportate (rifiuto esplicito).

Criteri:

1. Release pubblicata, non ramo principale; lockfile.
2. Trasporto stdio funzionante con entrambi i client sull'host target.
3. Contenuto immagine e risultati strutturati gestiti dai client come previsto dalla specifica.
4. Licenza compatibile (ADR-0023).

## Alternative considerate

- SDK Rust ufficiale (indicato dalla roadmap) contro implementazione interna del protocollo: la seconda solo se l'SDK non supporta la revisione richiesta, da motivare.
- Client: candidati con supporto stdio da individuare e provare; nessun client è considerato compatibile prima della prova.

## Conseguenze

- Il crate `mcp-server` viene aggiunto in F5 (sezione 6.2); la scelta in F0/F1 serve a evitare sorprese di compatibilità.
- Configurazioni di esempio pubblicate solo per client realmente provati (sezione 9).

## Prove richieste

- File di verifica dedicato in F0 o F1 (nome da assegnare, ad esempio `F1-nn-sdk-client-mcp.md`).

## Riferimenti

Roadmap, sezioni 3, 6.2, 9, 10 (F0, F1, F5), 14, 17.
