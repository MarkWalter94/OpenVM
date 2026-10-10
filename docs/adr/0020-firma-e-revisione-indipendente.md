# ADR-0020 — Responsabilità di firma e revisione indipendente

- Stato: Accettata (2026-10-10): **firma del codice e revisione indipendente non disponibili**
- Data: 2026-10-09; decisione: 2026-10-10
- Responsabile: responsabile del progetto

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

### Decisione del 2026-10-10

Dichiarazione del responsabile del progetto (azione U-7): **non si firma il codice** e **non c'è un revisore indipendente** disponibile.

Conseguenze (roadmap sezione 10 F6, sezione 14 e tabella dei rischi):

- F0 si chiude su questo punto: la disponibilità è verificata (negativa) e registrata, non presunta.
- Sviluppo e prove F1–F5 non sono bloccati.
- **La pubblicazione pubblica prevista in F6 resta bloccata** finché mancano firma Windows con checksum verificabili e revisione indipendente mirata su boundary MCP, QMP, path, console e processi: la roadmap non ammette di dichiarare una firma inesistente né di saltare la revisione.
- Fino ad allora gli artefatti sono distribuiti solo come build non firmate per uso interno, con checksum SHA-256 pubblicati accanto e la dicitura "non firmato".
- Nessuna chiave di firma esiste quindi nessuna chiave è custodita (criterio 3 soddisfatto per assenza).

## Alternative considerate

- Firma: certificato di firma del codice gestito dal progetto, oppure servizio di firma gestito; checksum SHA-256 pubblicati in ogni caso.
- Revisione: revisore esterno, oppure revisore interno non coinvolto nello sviluppo dei boundary; la roadmap richiede "indipendente" senza specificare la forma.

## Conseguenze

- Se firma o revisione non sono disponibili, la pubblicazione v0.2 resta bloccata; lo sviluppo F1–F5 può proseguire con il rischio registrato in `docs/STATUS.md`.

## Prove richieste

- Registrazione della disponibilità (documento o dichiarazione) in `docs/STATUS.md`; nessun file di verifica tecnica richiesto in F0.

## Riferimenti

Roadmap, sezioni 10 (F6), 13, 14, 17.
