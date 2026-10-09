# ADR-0026 — Codici pubblici per autenticazione e autorizzazione

- Stato: Proposta
- Data: 2026-10-09
- Responsabile: da assegnare
- Da chiudere entro: F1

## Contesto

La sezione 6.3 della roadmap fissa errori pubblici stabili (`permission_denied`, `session_expired`, `vm_busy`, `unsupported`, `resource_limit`, `state_conflict`) e la sezione 9 chiede errori operativi distinti da errori del protocollo. La sezione 7.4 chiede il diniego senza effetto per token assente, scaduto, revocato o riusato dopo riavvio e il rifiuto della connessione per un processo senza credenziale, senza nominare un codice.

Fino a questo ADR `lightvm-proto` usava `permission_denied` sia per credenziali assenti o non valide sia per chiamanti identificati ma non autorizzati, e aveva in più un codice di **protocollo** `unauthenticated` per la credenziale IPC. Il servizio IPC (`docs/verification/F1-04-service-ipc-ci.md`) usa `permission_denied` per l'handshake e segnala la differenza come punto da allineare.

## Decisione (proposta)

1. Nuovo codice operativo `unauthenticated` in `ErrorCode`, aggiunto in coda (i nove codici precedenti restano invariati, schema rigenerato).
2. Ordine e significato dei tre codici:
   - `unauthenticated`: nessuna credenziale valida. Credenziale IPC o token di sessione assente, malformato, sconosciuto, revocato, riusato dopo un riavvio del supervisor, sessione terminata per qualunque causa diversa dalla scadenza, richiesta prima dell'handshake. Stesso codice e stesso messaggio per tutti i motivi.
   - `session_expired`: credenziale riconosciuta ma con grant o lease scaduti, anche dopo la terminazione per scadenza. Prevale sulla pausa.
   - `permission_denied`: chiamante identificato ma non autorizzato. VM fuori allowlist (anche se inesistente), capability assente nella policy effettiva, mutazione durante la pausa, ruolo IPC insufficiente, creazione VM da agente, supporto diverso da quello approvato.
3. Il codice di protocollo `ProtocolErrorCode::Unauthenticated` resta nel contratto per compatibilità ma non viene emesso: una credenziale rifiutata è l'esito operativo di una richiesta compresa.
4. Nessuno dei tre codici è ritentabile automaticamente (`retryable = false`).

Implementazione e test in `vm-core` per le sessioni agente; prova in `docs/verification/F1-01-proto-core.md`.

## Alternative considerate

- **Solo `permission_denied`** per tutti i dinieghi: è la scelta più conservativa sul piano dell'informazione, ma impedisce al client di distinguere "serve una nuova autorizzazione nell'app" da "questa azione non è nel grant" e confonde i dinieghi nell'audit.
- **`unauthenticated` come errore di protocollo**: già presente, ma la sezione 9 separa gli errori di protocollo (richiesta non compresa) da quelli operativi; una credenziale rifiutata non è un problema di formato.
- **Distinguere token assente, errato e revocato**: escluso, rivelerebbe al chiamante quali sessioni esistono o sono esistite.

## Conseguenze

- `lightvm-service`: l'handshake con credenziale assente, malformata, sconosciuta o revocata e le richieste prima di `hello` dovrebbero restituire `unauthenticated` invece di `permission_denied`; il ruolo insufficiente resta `permission_denied`. Il file `F1-04-service-ipc-ci.md` e i test `ipc.rs` vanno allineati dal flusso del servizio.
- L'adapter MCP (F5) mappa `unauthenticated` e `session_expired` su "serve una nuova autorizzazione nell'app" e `permission_denied` su "azione fuori dal grant".
- `docs/ARCHITECTURE.md` elenca i codici pubblici: va aggiunto `unauthenticated`.
- L'audit registra il codice del diniego: i tentativi con credenziali non valide sono distinguibili dai dinieghi di policy.

## Prove richieste

- Test sintetici in `lightvm-proto` (`error_codes.rs`) e `vm-core` (`negatives.rs`, `scenarios.rs`): registrati in `docs/verification/F1-01-proto-core.md`.
- Test del servizio IPC con il nuovo codice: da registrare dal flusso F1-04/F1-11.

## Riferimenti

- `docs/ROADMAP.md`, sezioni 6.3, 7.3, 7.4, 9.
- ADR-0006 (MCP stdio, pairing e grant).
