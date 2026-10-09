# ADR-0006 — MCP stdio con pairing e grant limitato, nessun QMP generico

- Stato: Accettata
- Data: 2026-10-09
- Responsabile: da assegnare

## Contesto

Con il trasporto stdio il client MCP avvia il server come sottoprocesso; LightVM non può governare quel processo, ma può governare ciò che l'adapter ottiene dal supervisor. QMP è un'interfaccia privilegiata capace di accedere al filesystem host (roadmap, sezioni 3 e 9).

## Decisione

- Baseline di protocollo MCP 2025-11-25; adapter `lightvm-mcp` su stdio, nessun trasporto HTTP nell'MVP.
- L'adapter non apre dischi né esegue QEMU: inoltra al supervisor via named pipe con ACL (Windows) o socket con permessi restrittivi (Unix).
- MCP disabilitato per difetto a ogni avvio; nessuna autorizzazione sopravvive al riavvio.
- La prima connessione crea solo una richiesta di pairing con identità e codice verificabili nell'app; il grant limitato (VM, strumenti, durata, quote) è emesso dal supervisor dopo approvazione dell'utente. Il pairing da solo non dà permessi.
- Scadenza e lease verificati dal supervisor; lease senza rinnovo valido non superiore a 10 secondi (sezione 7.3).
- Nessun tool `qmp.execute`, nessun passthrough host, nessun percorso host, nessuna gestione dei media, nessuna modifica della policy.
- Catalogo v0.2 come da sezione 9; `vm.create`, `vm.delete` e tool guest rinviati a R4.
- Credenziali nell'archivio credenziali OS; mai negli argomenti di processo, nei report o negli esempi; mai propagate a QEMU.
- `stdout` solo MCP; diagnostica su `stderr` senza segreti.

## Alternative considerate

- Streamable HTTP con OAuth: non adottato per l'MVP; MCP tratta i due trasporti diversamente (sezione 9).
- Inoltro QMP generico: escluso (sezioni 3 e 9).
- Adapter con accesso diretto ai file VM: escluso (sezione 6.1).

## Conseguenze

- SDK e client di riferimento da selezionare (ADR-0021).
- Rate limit, code limitate, timeout senza retry ciechi, limiti di byte sugli screenshot.
- LightVM non limita ciò che il client possiede già fuori da LightVM (sezione 7.1): va dichiarato all'utente.

## Prove richieste

- Test negativi "Sessioni", "VM", "Host", "Policy", "Lifecycle" della sezione 7.4: F4/F5.
- Criterio di uscita F5 con due client reali documentati.

## Riferimenti

Roadmap, sezioni 3, 4, 6.1, 7.1, 7.3, 7.4, 9, 10 (F4, F5).
