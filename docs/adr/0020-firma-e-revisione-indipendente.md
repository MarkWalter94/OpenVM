# ADR-0020 — Responsabilità di firma e revisione indipendente

- Stato: Proposta
- Data: 2026-10-09
- Responsabile: da assegnare
- Da chiudere entro: F0 (verifica della disponibilità e assegnazione delle responsabilità)

## Contesto

F6 richiede distribuzione Windows pubblica con firma del codice e checksum verificabili, misure equivalenti di provenienza nel target Linux alternativo e una revisione indipendente mirata su boundary MCP, QMP, path, console e processi (roadmap, sezione 10 F6). Credenziali di firma mancanti bloccano la pubblicazione e non autorizzano dichiarazioni di firma inesistenti. Il rischio "hardware, firma o revisione indipendente indisponibili" va verificato in F0; il silenzio non è una verifica superata (sezione 14).

## Decisione

Da prendere. Va registrato:

- chi detiene e custodisce le credenziali di firma Windows e con quale tipo di certificato o servizio;
- misure di provenienza per Linux (checksum firmati, firma dei pacchetti);
- chi esegue la revisione indipendente, con quale indipendenza rispetto agli sviluppatori e con quale perimetro;
- dove sono conservate le chiavi (mai nel repository né sui runner con guest non fidati, sezione 13).

Criteri:

1. Disponibilità confermata per iscritto, non presunta.
2. Separazione tra chi sviluppa i boundary e chi li rivede.
3. Chiavi di firma fuori dai runner che eseguono guest non fidati.

## Alternative considerate

- Firma: certificato di firma del codice gestito dal progetto, oppure servizio di firma gestito; checksum SHA-256 pubblicati in ogni caso.
- Revisione: revisore esterno, oppure revisore interno non coinvolto nello sviluppo dei boundary; la roadmap richiede "indipendente" senza specificare la forma.

## Conseguenze

- Se firma o revisione non sono disponibili, la pubblicazione v0.2 resta bloccata; lo sviluppo F1–F5 può proseguire con il rischio registrato in `docs/STATUS.md`.

## Prove richieste

- Registrazione della disponibilità (documento o dichiarazione) in `docs/STATUS.md`; nessun file di verifica tecnica richiesto in F0.

## Riferimenti

Roadmap, sezioni 10 (F6), 13, 14, 17.
