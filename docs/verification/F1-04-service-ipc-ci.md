# F1-04 — `lightvm-service`: IPC locale autenticato, istanza singola, skeleton del supervisor e CI

- Data di esecuzione (UTC): 2026-10-09, dalle 14:30 alle 15:20 circa
- Esecutore: agente di sviluppo (sessione Claude Code), su richiesta del responsabile del progetto
- Passo della roadmap: sezione 10, F1 ("Skeleton del supervisor e test headless senza dipendenza dalla UI", "CI Windows/Linux; build macOS … senza dichiarare supporto operativo"); sezioni 6.1, 6.3, 7.1, 7.3, 7.4 (righe "Sessioni", "IPC/console", "Lifecycle"), 9 (adapter stdio, named pipe con ACL / socket locale, rate limit e code limitate), 13 (pipeline di verifica)
- ADR collegati: ADR-0002, ADR-0006, ADR-0009, ADR-0015 (parte B in sospeso), ADR-0023 (non modificati)
- Esito complessivo: **parziale** — trasporto, handshake, istanza singola, revoca, rate limit, arresto controllato e rilevamento della morte del genitore verificati su Windows e su Linux (WSL2) con backend e catalogo **finti**; rifiuto di un processo a integrità bassa verificato su Windows; workflow CI scritti e validati in locale ma **non eseguiti** su GitHub (nessun repository remoto); la sequenza CI completa sul workspace fallisce oggi per cause esterne a questo flusso (vedi Risultati, punto 6).

Esiti ammessi: **verificato**, **fallito**, **non eseguito** (sezione 17). Log grezzi in `lab/logs/` (fuori dal repository): `f1-04-windows-test.txt`, `f1-04-windows-checks.txt`, `f1-04-pipe-sddl.txt`, `f1-04-linux-test.txt`, `f1-04-linux-pidfd.txt`, `f1-04-ci-windows.txt`, `f1-04-ci-linux.txt`, `f1-04-cargo-deny.txt`, `f1-04-yaml-check.txt`.

## Scopo

Portare `crates/lightvm-service` da scheletro a skeleton funzionante del supervisor (ROADMAP 6.1, 10 F1):

1. trasporto IPC locale autenticato con permessi OS restrittivi e verifica dell'identità del peer;
2. protocollo con framing limitato, versione negoziata, handshake con credenziale, errori stabili;
3. istanza singola per catalogo;
4. ciclo del supervisor con `Catalog` e `Backend` come tratti, dispatcher tipizzato, eventi, registro delle connessioni con revoca, arresto controllato, Emergency Stop su percorso prioritario;
5. avvio come figlio della UI con credenziale iniziale fuori dagli argomenti e rilevamento della morte del genitore;
6. CI Windows/Linux, portabilità macOS, schemi, licenze e advisory.

Non dimostra: processi QEMU, Job Object, QMP o storage reali (le implementazioni di `Catalog` e `Backend` sono finte, sezione 10 F1: "test sintetici e mock sono ammessi per errori/protocollo"); pairing con approvazione dell'utente, grant e lease (F4); adapter MCP (F5); isolamento rispetto a un secondo utente locale (nessun secondo account disponibile, come in F0-04); esecuzione dei workflow su runner GitHub.

## Ambiente

| Voce | Valore |
| --- | --- |
| Host | HP ZBook Fury 16 G9, Intel Core i7-12800HX, 32 GiB RAM (come F0-01/F0-04) |
| OS e build | Windows 11 Pro 10.0.26200; utente di dominio non amministratore, shell a integrità media (`S-1-16-8192`) |
| Linux | WSL2, kernel `6.18.33.1-microsoft-standard-WSL2`, Ubuntu 26.04, `XDG_RUNTIME_DIR=/run/user/1000`; build con linker zig come in F0-07 (`lab/wsl/env.sh`), `CARGO_TARGET_DIR=lab/wsl/target` |
| Acceleratore e stato | non usato (nessun QEMU in questa prova) |
| Toolchain | Rust 1.99.0 (`b940084d7 2026-09-28`), `x86_64-pc-windows-msvc` con ambiente Build Tools 2022 (`vcvars64.bat`, F0-01 §4.1); `x86_64-unknown-linux-gnu` in WSL |
| Binario QEMU / firmware / guest | non applicabile |
| Sessione grafica | non applicabile (test headless) |
| Strumenti di verifica | cargo-deny 0.20.2 (profilo utente); Node 24.15.0 con js-yaml 4.1.0 (scratchpad); actionlint 1.7.7 (scratchpad, SHA-256 dello zip `7f12f1801bca3d480d67aaf7774f4c2a6359a3ca8eebe382c95c10c9704aa731` uguale al file checksums della release) |
| Binari provati | `target\debug\lightvm-service.exe` SHA-256 `f4f67bf7f7ff27311311985d9018a3251e26a7852e12dc0612a7ee711966a15f`; `target\debug\lightvm-ipc-probe.exe` SHA-256 `6c1a4187755d052f614c5c4ba2d4934c519035d7cccd7f9a36c8b4a483bcb97b` |

Dipendenze aggiunte a `lightvm-service` (versioni esatte, nessuna dipendenza git; licenze dall'inventario `cargo deny list`):

| Crate | Versione | Licenza | Uso |
| --- | --- | --- | --- |
| tokio | 1.53.2 (già nel lockfile) | MIT | runtime asincrono, socket Unix, canali |
| serde, serde_json | 1.0.229, 1.0.151 | MIT OR Apache-2.0 | payload JSON |
| getrandom | 0.3.4 (già nel lockfile) | MIT OR Apache-2.0 | credenziali e nomi casuali dal generatore del sistema operativo |
| lightvm-proto | path | MIT | `PublicError`, `ErrorCode`, `ProtocolError`, `ReplyError` |
| interprocess (solo Windows) | 2.4.4 | 0BSD OR MIT OR Apache-2.0 | named pipe asincrona con descrittore di sicurezza, conversione SDDL, PID e sessione del client |
| sysinfo (solo Windows) | 0.39.6 | MIT | SID dell'utente proprietario di un processo (proprio e del peer) |
| widestring (solo Windows) | 1.2.1 | MIT OR Apache-2.0 | stringa SDDL UTF-16 per `interprocess` |
| rustix (solo Unix) | 1.1.5 (già nel lockfile) | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | `geteuid`, `pidfd_open`, `poll`, `getppid` |

Nessun `unsafe` nel crate (`unsafe_code = "deny"` del workspace, nessuna deroga): le chiamate Win32 e Linux sono incapsulate nei crate sopra.

## Progetto

### Trasporto scelto e motivazione

**Windows: named pipe con ACL**, non socket Unix in directory privata.

- La named pipe consente di fissare il descrittore di sicurezza **alla creazione** (nessuna finestra tra creazione e ACL) e di includervi l'etichetta di integrità; il socket Unix su Windows richiederebbe una directory privata con DACL impostata via API (con `icacls` non è accettabile in produzione) e non offre credenziali del peer.
- Si ottiene in modo sicuro senza `unsafe` diretto: `interprocess` 2.4.4 espone `SecurityDescriptor::deserialize(SDDL)` e `PipeListenerOptions::security_descriptor`, imposta `FILE_FLAG_FIRST_PIPE_INSTANCE` sulla prima istanza (se il nome esiste già la creazione fallisce invece di condividere la pipe) e `PIPE_REJECT_REMOTE_CLIENTS` (`accept_remote(false)`), e fornisce `client_process_id`/`client_session_id`.
- Descrittore applicato: `O:<SID>D:P(A;;GA;;;<SID>)S:(ML;;NWNRNX;;;ME)`, dove `<SID>` è il SID dell'utente del processo servizio (letto con `sysinfo`, validato come `S-1-` seguito solo da cifre e trattini per escludere iniezioni nell'SDDL). DACL protetta con un solo ACE per l'utente corrente (nessun accesso per SYSTEM, Administrators, Everyone o pacchetti AppContainer); etichetta obbligatoria di integrità media con no-write-up/no-read-up/no-execute-up: un processo a integrità bassa non apre la pipe.
- Dopo l'accettazione: la sessione di logon del client deve coincidere con quella del servizio e il SID del processo client (dal PID riportato dal kernel per la pipe) deve coincidere con quello del servizio; altrimenti la connessione viene chiusa prima di leggere qualsiasi byte.
- Nome della pipe casuale (`\\.\pipe\lightvm-<128 bit esadecimali>`): non è un segreto, la protezione è l'ACL più la credenziale.

**Linux: socket Unix** in `$XDG_RUNTIME_DIR/lightvm` (ripiego `<catalogo>/.run`, o `--runtime-dir`). La directory viene creata con `0700` o, se esiste, deve essere una directory (non un collegamento simbolico), dell'utente effettivo e senza permessi per gruppo e altri; il socket viene portato a `0600` (la directory protegge la finestra tra `bind` e `chmod`). Dopo l'accettazione l'UID del peer da `SO_PEERCRED` (`tokio::net::UnixStream::peer_cred`) deve coincidere con l'UID effettivo del servizio. Percorso del socket limitato a 100 byte (limite di `sun_path`) con errore esplicito. Il socket viene rimosso all'arresto. Lo stesso codice compila su macOS (`getpeereid`), senza supporto dichiarato.

### Protocollo

- **Framing:** 4 byte big-endian di lunghezza più payload JSON. Lunghezza zero → errore di protocollo `malformed_message`; lunghezza oltre il limite (predefinito 64 KiB, configurabile) → `message_too_large` e chiusura, **senza leggere né allocare il payload**. La lettura avviene in un task dedicato che consegna frame completi su un canale limitato (4 frame): nessuna lettura viene interrotta a metà frame e la coda per connessione è limitata.
- **Messaggi:** `{"type":"hello",...}` e `{"type":"request","request_id":N,"body":{"op":...}}` dal client; `hello_ok`, `response`, `error`, `event`, `closing` dal servizio. Ogni richiesta porta un `request_id` restituito nella risposta. Operazioni sconosciute (per esempio `qmp_execute`) non sono deserializzabili → `malformed_message`, nessun effetto.
- **Versione:** il client offre un elenco (`protocol_versions`), il servizio sceglie la prima supportata in ordine di preferenza (oggi solo `1`); nessuna versione comune → errore di protocollo `unsupported_version` e chiusura.
- **Handshake:** il primo messaggio deve essere `hello` entro 5 s. Verifiche in ordine: identità OS (già nel trasporto), versione, credenziale. Credenziale assente, malformata, sconosciuta, scaduta o revocata → errore operativo `permission_denied` e chiusura, senza registrare la connessione e senza alcun effetto sul backend; i motivi non sono distinti verso il client. Una richiesta prima di `hello` → `permission_denied` e chiusura. Un secondo `hello` → `malformed_message`.
- **Credenziali:** 32 byte dal generatore del sistema operativo (`getrandom`), trasmessi in esadecimale, confrontati a tempo costante, azzerati al rilascio, mai in `Debug`, mai negli argomenti di processo. Vivono solo in memoria: un riavvio del servizio le invalida tutte. Ruoli: `ui` (credenziale iniziale) e `pairing` (emessa su richiesta della UI con scadenza di 10 minuti, da consegnare fuori banda; **nessun permesso VM**, solo `ping` e `get_capabilities`, ADR-0006).
- **Errori:** dalla fonte unica `lightvm-proto`: `ReplyError::Operational(PublicError{code, message, retryable})` con i codici stabili (`permission_denied`, `session_expired`, `vm_busy`, `unsupported`, `resource_limit`, `state_conflict`, …) e `ReplyError::Protocol(ProtocolError{code, message})` (`malformed_message`, `unsupported_version`, `message_too_large`, …), mai confusi.
- **Limiti (sezione 9):** rate limit per connessione a token bucket (predefinito 50 in raffica, 100/s) → `resource_limit` senza effetto; coda verso il supervisor condivisa e limitata (64 posti) con rifiuto immediato `resource_limit` se piena; connessioni simultanee limitate (16) → `resource_limit` e chiusura.
- **Doppio controllo (sezione 7.3):** il ruolo è verificato al ricevimento; il thread del supervisor ricontrolla che la connessione non sia stata revocata immediatamente prima di eseguire.

### Supervisor

- Un thread dedicato possiede `Box<dyn Catalog>` e `Box<dyn Backend>` (unico proprietario, nessun lock condiviso sullo stato VM) e serve i comandi in ordine: `ping`, `get_capabilities`, `list_vms`, perdita del genitore, arresto. `subscribe` iscrive la connessione al canale di eventi (`service_state`, `vm_state`, `connection_revoked`, `agent_sessions_stopped`, `events_lost` per i client lenti).
- **Registro delle connessioni con revoca:** `list_connections`, `revoke_connection` (solo UI). La revoca chiude la connessione con `closing{reason: revoked}`, invalida la credenziale se il ruolo non è `ui`, interrompe l'eventuale richiesta in attesa e pubblica `connection_revoked`.
- **Emergency Stop (ADR-0009):** richiesta `emergency_stop` (solo UI) o `ServiceHandle::emergency_stop`. Revoca prima tutte le connessioni e credenziali non UI, poi chiama `EmergencyStop::emergency_stop_all` del backend su un thread bloccante: non occupa posti in coda e non passa dal thread del supervisor. Il backend reale dovrà terminare l'albero QEMU con primitive OS e dichiarare `completed` solo dopo averne osservato la fine.
- **Arresto controllato:** `shutdown` (solo UI), riga `shutdown` sullo stdin o `ServiceHandle::shutdown`: stop dell'accettazione; `closing{reason: service_stopping}` a tutte le connessioni dopo aver consegnato gli eventi già pubblicati; il supervisor completa l'operazione in corso, risponde `state_conflict` a quelle in coda, chiama `stop_agent_sessions(Shutdown)` e `shutdown()` del backend; attesa limitata (30 s, poi il thread non viene più atteso); rilascio del lock; rimozione del socket su Unix.
- **Istanza singola:** lock esclusivo su `<catalogo>/.lightvm-service.lock` con `std::fs::File::try_lock` (`LockFileEx` su Windows, `flock` su Linux), rilasciato dal sistema operativo alla chiusura dell'handle anche se il processo muore. Seconda istanza → codice di uscita 3 e messaggio "un'altra istanza di lightvm-service sta già usando questo catalogo (lock: …)", nessuna riga `ready` e nessuna credenziale emessa.

### Avvio come figlio della UI e morte del genitore

Contratto del binario `lightvm-service` (nessun segreto negli argomenti, sezione 9):

```text
lightvm-service --catalog <dir> [--runtime-dir <dir>] [--parent-watch stdin|none] [--orphan-policy shutdown|keep-manual-vms]
stdout: una riga {"event":"ready","pid":…,"endpoint":"…","protocol_versions":[1],"credential":"<64 hex>"}
stdin:  pipe di vita; EOF = genitore terminato; riga "shutdown" = arresto controllato
stderr: diagnostica senza segreti (scritture che non vanno mai in panico se la pipe è chiusa)
uscita: 0 regolare, 2 uso errato, 3 catalogo già in uso, 4 altri errori di avvio
```

**Consegna della credenziale UI: stdout del figlio.** È una pipe anonima creata dalla UI e posseduta solo da lei (equivale a un handle ereditato, senza file su disco da proteggere né da cancellare); la credenziale non compare negli argomenti né nell'ambiente. Alternativa scartata: file in directory privata (su Windows richiede una DACL via API sulla directory, da cancellare in modo affidabile anche dopo un crash).

**Rilevamento della morte del genitore:**

- Meccanismo principale, identico su Windows e Linux: **pipe di vita sullo stdin**. Quando la UI termina (anche per crash o kill) il sistema operativo chiude l'unica estremità di scrittura e il servizio legge EOF. Non dipende dal PID (nessun riuso possibile) e non richiede `unsafe`.
- **Linux, in aggiunta:** `pidfd_open(getppid())` più `poll`, come in F0-07, con il controllo `getppid()` ripetuto dopo l'apertura per escludere un PID già riusato. Copre il caso in cui l'estremità di scrittura dello stdin sia stata ereditata per errore da un altro processo (verificato sotto). Non si usa `PR_SET_PDEATHSIG`: invierebbe un segnale invece di avviare l'arresto controllato.
- **Windows:** l'attesa su un handle del genitore (`OpenProcess` + `WaitForSingleObject`) richiederebbe FFI `unsafe` nel crate o un handle ereditato convertito con `from_raw_handle` (anch'esso `unsafe`); la pipe di vita offre la stessa semantica legata alla vita del processo. Limite: se la UI lasciasse ereditare l'estremità di scrittura dello stdin del servizio a un altro processo, la morte della UI non verrebbe rilevata finché quel processo vive. La UI deve quindi creare la pipe non ereditabile verso altri figli (comportamento predefinito di `std::process::Command`).
- Alla perdita del genitore: revoca di connessioni e credenziali non UI, `Backend::stop_agent_sessions(ParentLost)`, hook `Backend::on_parent_lost(policy)` per le VM manuali (**ADR-0015 parte B in sospeso**), evento `agent_sessions_stopped{reason: parent_lost}`, poi `OrphanPolicy`: predefinito `shutdown` (arresto controllato: senza UI la credenziale iniziale non è recuperabile, quindi oggi nessun ricollegamento è possibile); `keep-manual-vms` lascia il servizio attivo per la futura decisione.

## Procedura

1. Build e lint del crate su Windows, nell'ambiente MSVC di F0-01 (wrapper `cmd` che chiama `vcvars64.bat`, nella scratchpad):

   ```text
   cargo fmt --all --check            (verificato per lightvm-service; vedi punto 6 per il workspace)
   cargo clippy -p lightvm-service --all-targets -- -D warnings
   cargo test -p lightvm-service -- --nocapture --test-threads=4
   ```

2. Prove manuali Windows con lo script versionato `crates/lightvm-service/scripts/f1-04-windows-checks.ps1` (dopo `cargo build -p lightvm-service --bins`):

   ```text
   powershell -NoProfile -ExecutionPolicy Bypass -File crates/lightvm-service/scripts/f1-04-windows-checks.ps1
   ```

   Lo script copia `lightvm-ipc-probe.exe` e `whoami.exe` in una directory temporanea, marca le copie con `icacls <file> /setintegritylevel Low` (approccio di F0-04), avvia il servizio con stdin/stdout reindirizzati, legge la riga `ready` (la credenziale non viene stampata), esegue il probe a integrità bassa e media passando la credenziale **sullo stdin**, prova credenziale errata e assente, una seconda istanza, l'arresto via stdin e infine il kill reale del genitore (PowerShell intermedio che avvia il servizio e viene terminato con `Stop-Process -Force`).

3. Lettura del descrittore della pipe da PowerShell: `[System.IO.Directory]::GetAccessControl(<endpoint>).GetSecurityDescriptorSddlForm('All')`.

4. Linux (WSL2), con `lab/wsl/env.sh` e `CARGO_TARGET_DIR=lab/wsl/target`: `cargo clippy -p lightvm-service --all-targets -- -D warnings` e tre esecuzioni di `cargo test -p lightvm-service`.

5. Linux, kill reale del genitore con la pipe di vita tenuta aperta da un altro processo: `bash -c "sleep 1000 | lightvm-service --catalog $D --runtime-dir $D/run > $D/ready.txt"` in background, poi `kill -9` della `bash` genitore (lo `sleep` resta vivo e tiene aperto lo stdin del servizio: solo il pidfd può rilevare la morte).

6. Replica manuale della sequenza CI su tutto il workspace (Windows e WSL): `cargo fmt --all --check`; `cargo clippy --workspace --all-targets --locked -- -D warnings`; `cargo clippy -p lightvm-desktop --no-default-features --features glow --all-targets --locked -- -D warnings`; `cargo test --workspace --locked` (Windows) / `--no-fail-fast` (Linux); `cargo build --release --locked -p lightvm-desktop`; `cargo test --locked -p lightvm-proto --test schema_snapshot`.

7. `cargo deny check` dalla radice con `deny.toml`; validazione dei workflow con js-yaml (sintassi, `runs-on`/`steps`, azioni fissate a SHA) e actionlint 1.7.7 (`-shellcheck= -pyflakes=`, strumenti non disponibili sull'host).

## Risultati

### 1. Test headless (`cargo test -p lightvm-service`)

Trasporto reale (named pipe su Windows, socket Unix su Linux), catalogo e backend finti. 18 test unitari su Windows (17 su Linux: i test del trasporto differiscono per piattaforma) e 20 test di integrazione in `crates/lightvm-service/tests/ipc.rs`.

| Test di integrazione | Cosa verifica | Windows | Linux (WSL2) |
| --- | --- | --- | --- |
| `valid_handshake_and_typed_requests` | `hello_ok` con versione 1 e ruolo `ui`; `ping`, `get_capabilities`, `list_vms` | verificato | verificato |
| `invalid_credential_is_denied_and_closed` | credenziale casuale e credenziale malformata → `permission_denied`, chiusura, nessuna connessione registrata, nessuna chiamata al backend | verificato | verificato |
| `missing_credential_and_request_before_hello_are_denied` | `hello` senza credenziale e `emergency_stop` prima di `hello` → `permission_denied`, chiusura, nessun Emergency Stop eseguito | verificato | verificato |
| `unsupported_protocol_version_is_rejected` | versioni `[0,2,99]` → `unsupported_version`, chiusura; `[7,1]` → negoziata la 1 | verificato | verificato |
| `oversized_frame_is_rejected_before_and_after_handshake` | intestazione da 1 MiB prima dell'handshake e frame da 8 KiB con limite 4 KiB dopo → `message_too_large`, chiusura | verificato | verificato |
| `malformed_message_is_a_protocol_error_and_connection_survives` | `op: qmp_execute` → `malformed_message`; la connessione resta utilizzabile | verificato | verificato |
| `two_clients_with_different_roles` | UI e pairing contemporanei; il pairing ottiene solo `ping`; `list_vms`, `subscribe`, `emergency_stop`, `shutdown`, `create_pairing_credential`, `revoke_connection` → `permission_denied`; `list_connections` mostra 2 connessioni con PID del peer | verificato | verificato |
| `revoking_a_connection_closes_it_and_invalidates_the_credential` | revoca dalla UI → `closing{revoked}` e chiusura del client; evento `connection_revoked` all'iscritto; credenziale non più accettata; revoca di un id inesistente → `state_conflict` | verificato | verificato |
| `second_instance_on_same_catalog_is_rejected` | seconda `start` → `AlreadyRunning`; dopo l'arresto il riavvio riesce | verificato | verificato (vedi nota) |
| `credential_is_not_valid_after_restart` | credenziale UI della prima esecuzione rifiutata dopo il riavvio (riga "Sessioni" della 7.4) | verificato | verificato |
| `rate_limit_per_connection` | raffica 3, ricarica 0,5/s: 3 `pong` e 3 `resource_limit` su 6 richieste; un'altra connessione ha il proprio bucket | verificato | verificato |
| `bounded_queue_and_emergency_stop_does_not_wait_for_it` | coda da 1 posto occupata da un `list_vms` bloccato nel catalogo: `ping` → `resource_limit` immediato; `emergency_stop` completato con supervisor bloccato (785 µs su Windows), connessione di pairing revocata; sblocco → `list_vms` risponde | verificato | verificato |
| `connection_limit_and_handshake_timeout` | limite 1 connessione → `resource_limit`; client muto chiuso al timeout di handshake (300 ms) | verificato | verificato |
| `controlled_shutdown_via_ipc_closes_connections_and_releases_lock` | `shutdown` → `shutting_down`; l'altra connessione riceve `closing{service_stopping}`; backend: `stop_agent_sessions:Shutdown` poi `shutdown`; lock libero; nuove connessioni rifiutate | verificato | verificato |
| `parent_lost_in_process_stops_agent_sessions_then_shuts_down` | `parent_lost`: backend `stop_agent_sessions:ParentLost`, `on_parent_lost:ShutdownService`; pairing revocato; evento `agent_sessions_stopped{parent_lost}` consegnato prima della chiusura | verificato | verificato |
| `binary::second_binary_instance_exits_with_clear_error` | secondo processo sullo stesso catalogo: uscita 3, messaggio chiaro, stdout vuoto | verificato | verificato |
| `binary::parent_death_closes_sessions_and_releases_lock` | binario reale; chiusura dello stdin (morte del genitore simulata): eventi `agent_sessions_stopped{parent_lost}` e `service_state{stopping}`, `closing{service_stopping}`, uscita 0 in 26,5 ms (Windows), lock libero | verificato | verificato |
| `binary::lock_released_when_service_is_killed` | kill del servizio: il lock del catalogo si libera | verificato | verificato |
| `binary::shutdown_command_on_stdin` | riga `shutdown` sullo stdin → uscita 0 | verificato | verificato |
| `binary::bad_arguments_exit_2_without_secrets` | argomento sconosciuto → uscita 2, stdout vuoto | verificato | verificato |

Esecuzioni: Windows 3 serie complete + 1 con output (tutte `20 passed`, log `f1-04-windows-test.txt`); Linux 3 serie consecutive dopo la correzione sotto (tutte `18|17 passed` unitari e `20 passed` integrazione, log `f1-04-linux-test.txt`). `cargo clippy -p lightvm-service --all-targets -- -D warnings`: nessun avviso su Windows e su Linux.

Nota (Linux): la prima esecuzione su WSL ha dato **fallito** per `second_instance_on_same_catalog_is_rejected` (`riavvio dopo il rilascio del lock: Lock(AlreadyRunning …)`); da solo il test passava 5 volte su 5. Causa: un altro test, in parallelo, avvia processi figli; tra `fork` ed `exec` il figlio possiede per un istante una copia del descrittore del lock (`O_CLOEXEC` lo chiude solo all'`exec`) e `flock` appartiene alla descrizione di file aperto condivisa. I test che verificano il rilascio del lock ora riprovano per al massimo 2 s. Conseguenza per il prodotto: nessuna, il lock è tenuto per tutta la vita del servizio; un nuovo avvio immediatamente dopo l'arresto può però vedere il catalogo occupato per un istante se il servizio sta avviando figli: la UI deve riprovare brevemente su codice di uscita 3 dopo un arresto appena concluso.

### 2. Processo a integrità bassa (Windows)

| Prova | Esito | Evidenza (`lab/logs/f1-04-windows-checks.txt`) |
| --- | --- | --- |
| Livello effettivo della copia marcata | verificato | `whoami-low.exe /groups`: `Etichetta obbligatoria\Livello obbligatorio basso S-1-16-4096`; shell: `S-1-16-8192` |
| Probe a integrità bassa, credenziale valida | verificato (rifiutato) | `uscita=10 PROBE connessione=rifiutata errore_os=Some(5) messaggio="Accesso negato. (os error 5)"`: il sistema operativo rifiuta l'apertura della pipe prima di qualsiasi scambio |
| Controllo a integrità media, credenziale valida | verificato | `uscita=0 PROBE connessione=ok handshake=ok connection_id=1 ruolo=Ui ping=pong` |
| Integrità media, credenziale errata / assente | verificato (rifiutato) | `uscita=11 … Operational(PublicError { code: PermissionDenied, message: "credenziale assente o non valida", … })` |
| Seconda istanza sullo stesso catalogo | verificato | `uscita=3 lightvm-service: un'altra istanza di lightvm-service sta già usando questo catalogo (lock: …\.lightvm-service.lock)` |
| Arresto via stdin | verificato | `uscita servizio=0 stderr: lightvm-service: arresto completato` |
| Kill reale del genitore (`Stop-Process -Force` del PowerShell intermedio) | verificato | `servizio terminato dopo il kill del genitore: True in 42 ms`; `processi lightvm-service residui per questa prova: 0` (prima esecuzione: 29 ms) |
| DACL effettiva della pipe | verificato | `O:S-1-5-21-…-7907G:DUD:P(A;;FA;;;S-1-5-21-…-7907)`: un solo ACE, per l'utente corrente (`f1-04-pipe-sddl.txt`). La SACL con l'etichetta non è leggibile da questa API senza privilegi: l'effetto è dimostrato dalla riga del probe a integrità bassa |
| Secondo utente locale | **non eseguito** | nessun secondo account e nessuna elevazione, come in F0-04. Da eseguire con `runas /user:<altro> lightvm-ipc-probe.exe --endpoint …` |

### 3. Linux: permessi e pidfd

| Prova | Esito | Evidenza (`lab/logs/f1-04-linux-pidfd.txt`) |
| --- | --- | --- |
| Permessi di directory e socket | verificato | `permessi directory privata: 700 socket: 600`; test unitari: directory `0755` e collegamento simbolico rifiutati |
| Kill della `bash` genitore con lo stdin tenuto aperto da `sleep` | verificato | `ppid_servizio=459` (la bash); dopo `kill -9`: `servizio terminato dopo il kill del genitore in 27 ms (sleep ancora vivo: si)`; stderr `processo genitore terminato`, `arresto completato`; `socket residui: 0` |
| Verifica `SO_PEERCRED` con UID diverso | **non eseguito** | nessun secondo utente nella distro senza `sudo`; il controllo è nel codice (`transport/unix.rs`) ma solo il ramo positivo è stato esercitato |

### 4. CI (`.github/workflows/ci.yml`, `.github/dependabot.yml`, `deny.toml`)

| Job | Runner | Contenuto |
| --- | --- | --- |
| `build-test` | `windows-latest`, `ubuntu-latest` (matrice, `fail-fast: false`) | toolchain dal canale di `rust-toolchain.toml` (`rustup toolchain install <canale> --component rustfmt,clippy`); su Ubuntu `build-essential pkg-config` e librerie runtime di eframe/winit (`libxkbcommon0 libxkbcommon-x11-0 libwayland-* libx11-* libxcursor1 libxrandr2 libxi6 libxcb1 libegl1 libgl1`); cache `Swatinem/rust-cache`; `cargo fmt --all --check`; `cargo clippy --workspace --all-targets --locked -- -D warnings`; clippy di `lightvm-desktop` con `glow`; `cargo test --workspace --locked --no-fail-fast` (il test con QEMU reale resta `#[ignore]`; i test kittest sono headless, nessun display virtuale); `cargo build --release --locked -p lightvm-desktop` |
| `macos-portability` | `macos-latest`, `continue-on-error: true` | "portabilità, nessun supporto dichiarato": `cargo check --workspace --locked` |
| `schema` | `ubuntu-latest` | `cargo test --locked -p lightvm-proto --test schema_snapshot` (test creato dall'agente proto) e `git diff --exit-code -- crates/lightvm-proto/schema` |
| `cargo-deny` | `ubuntu-latest` | `cargo install cargo-deny --version 0.20.2 --locked`, poi `cargo deny --locked check` (licenze, advisory RustSec, sorgenti, duplicati) |

Proprietà comuni: `permissions: contents: read`, nessun segreto, nessun accesso al laboratorio, `persist-credentials: false`, azioni fissate a commit SHA (`actions/checkout` v7.0.1, `Swatinem/rust-cache` v2.9.2, SHA letti con `git ls-remote`), `concurrency` con annullamento delle esecuzioni superate. Nessun runner ha un acceleratore: boot, QMP e console reali, Emergency Stop su QEMU, cicli start/stop del criterio di uscita F1 e limiti di risorse non sono verificabili in questa CI e restano ai runner fisici (F0-07, "CI minima per F1"). Dependabot: `cargo` settimanale con gruppi `ui` (egui/eframe/wgpu/winit insieme, ADR-0014) e `cargo-minor-patch`, più `github-actions` settimanale.

| Verifica locale | Esito | Evidenza |
| --- | --- | --- |
| Sintassi e struttura YAML (js-yaml 4.1.0) | verificato | `OK .github/workflows/ci.yml` (4 job, azioni fissate a SHA), `OK .github/dependabot.yml` (`f1-04-yaml-check.txt`) |
| actionlint 1.7.7 | verificato | nessun problema, `EXIT=0` (shellcheck e pyflakes non disponibili, disattivati) |
| `cargo deny check` | verificato | `advisories ok, bans ok, licenses ok, sources ok` (avvisi non bloccanti per versioni duplicate, da eframe/wgpu/windows-sys) (`f1-04-cargo-deny.txt`) |
| Esecuzione su GitHub | **non eseguito** | nessun repository remoto (regola del compito); in particolare non sono provati: `rustup toolchain install` sui runner, il problema `LNK1104 msvcrt.lib` di F0-01 (atteso assente sui runner con Visual Studio completo), i pacchetti apt, il job macOS |

### 5. Integrazione con `lightvm-proto`

Integrati da `lightvm-proto`: `ErrorCode`, `PublicError`, `ProtocolError`, `ProtocolErrorCode`, `ReplyError` (riesportati da `lightvm_service::compat`). Restano locali in `compat` con `TODO(F1)`: involucro IPC (`ClientMessage`, `ServerMessage`, `Request`, `ResponseBody`, `ServiceEvent`), `Role`, `VmSummary`/`VmState` (in `lightvm-proto` esistono `VmState` e i modelli del catalogo, da adottare quando `vm-storage` implementerà `Catalog`). La credenziale resta il tipo locale `Credential` (in `lightvm-proto` esiste `SessionToken([u8; 32])`, da allineare con l'agente proto). Differenza da allineare: `lightvm-proto` prevede il codice di protocollo `unauthenticated` per "credenziale IPC assente o non valida", mentre la sezione 7.4 e questo compito chiedono `permission_denied`; il servizio usa `permission_denied`.

### 6. Replica della sequenza CI sul workspace (2026-10-09, ~15:00 UTC)

Windows (`f1-04-ci-windows.txt`) — esito per passo:

| Passo | Esito | Causa |
| --- | --- | --- |
| `cargo fmt --all --check` | **fallito** | differenze di formattazione in `crates/lightvm-proto`, `crates/qemu-backend`, `crates/vm-core`, `crates/vm-storage` (lavoro in corso di altri agenti); nessuna in `lightvm-service` |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | **fallito** | `crates\qemu-backend\src\cmdline.rs:803:9` `collapsible_if` (lavoro in corso di altri agenti) |
| clippy `lightvm-desktop` con `glow` | verificato | `EXIT=0` |
| `cargo test --workspace --locked` | **fallito** | `lightvm-proto` `schema_snapshot::committed_schemas_match_generated`: schemi committati diversi dai generati (lavoro in corso dell'agente proto); `cargo test` si ferma al primo binario fallito, per questo in CI si usa `--no-fail-fast` |
| `cargo build --release --locked -p lightvm-desktop` | verificato | `EXIT=0` |
| `schema_snapshot` | **fallito** | stessa causa del test |

Linux (`f1-04-ci-linux.txt`): vedi la sezione aggiunta in coda.

## Limiti

- Backend e catalogo finti: nessun processo QEMU, Job Object, QMP o file VM. L'Emergency Stop verifica solo il percorso prioritario nel servizio, non la terminazione dell'albero dei processi né la latenza della sezione 13.
- Nessun secondo utente locale su Windows né su Linux: il rifiuto di un altro utente si basa sull'ACL e sul controllo dell'UID, non su una prova eseguita.
- Le stesse garanzie non coprono un processo malevolo dello stesso utente a integrità media (sezione 7.1): può aprire la pipe o il socket; lo ferma solo la credenziale, che non può leggere dallo stdout privato della UI salvo accesso alla memoria di processi dello stesso utente.
- `GA` nell'ACE concede all'utente corrente anche la creazione di ulteriori istanze della pipe: un processo dello stesso utente potrebbe accodare una propria istanza (squatting) e ricevere connessioni destinate al servizio; è fuori dal threat model (stesso utente) ma va riesaminato con l'adapter MCP, che dovrà autenticare anche il server (per esempio confrontando `server_process_id` con il PID annunciato dalla UI).
- Su Windows la morte del genitore è rilevata solo tramite la pipe di vita (vedi Progetto); non è stata provata la variante con handle ereditato del genitore.
- Le directory temporanee dei test (`%TEMP%\lightvm-service-tests`, `/tmp/lightvm-service-tests`) non vengono rimosse.
- Su Windows i test di integrazione impiegano circa 2,4–3,7 s contro 0,65 s su Linux: l'arresto attende fino a 2 s la chiusura delle connessioni; non indagato oltre.
- In WSL la build usa il linker zig (F0-07), non `build-essential` come in CI.
- I workflow non sono stati eseguiti su GitHub.

## Azioni richieste

1. Pubblicare il repository ed eseguire la CI; registrare l'esito per job (in particolare Windows MSVC, apt su Ubuntu e macOS) in un file di verifica successivo.
2. Allineare con l'agente proto: spostare l'involucro IPC in `lightvm-proto` con schema generato, decidere `permission_denied` contro `unauthenticated` per l'handshake, adottare `SessionToken` e `VmState`.
3. Implementare `Catalog` in `vm-storage` e `Backend`/`EmergencyStop` in `qemu-backend` (Job Object su Windows, meccanismo ADR-0015 parte A su Linux), poi ripetere i test di arresto e perdita del genitore con QEMU reale e misurare l'Emergency Stop (sezione 13).
4. Decidere ADR-0015 parte B e implementare `on_parent_lost` di conseguenza; se si sceglie il ricollegamento della UI serve un canale per riemettere la credenziale UI.
5. Prova con un secondo utente locale (Windows: `runas`; Linux: altro UID) contro la pipe e il socket.
6. Pairing reale (F4): codice verificabile nell'app, approvazione dell'utente, grant con scadenza e lease ≤ 10 s; autenticazione del server da parte dell'adapter MCP (F5) e scoperta dell'endpoint da parte dell'adapter.
7. Fuzzing mirato del parser dei frame e dei messaggi (sezione 13).
8. La UI che avvia il servizio deve riprovare brevemente su codice di uscita 3 subito dopo un arresto (nota Linux del punto 1).

## Decisioni proposte

- **ADR-0002 / sezione 9 (trasporto):** su Windows named pipe con DACL limitata al SID dell'utente, etichetta di integrità media `NW/NR/NX`, client remoti rifiutati e verifica di sessione e SID del peer, tramite `interprocess`/`sysinfo` senza `unsafe` nel crate; su Unix socket in directory `0700` con verifica dell'UID del peer.
- **Avvio del supervisor:** credenziale iniziale della UI sullo stdout del figlio, pipe di vita sullo stdin come meccanismo comune di rilevamento della morte del genitore, pidfd in aggiunta su Linux.
- **ADR-0023:** adottare `cargo-deny` con `deny.toml` (licenze permissive, eccezione per i font di `epaint_default_fonts`, sole sorgenti crates.io, advisory senza eccezioni) e Dependabot settimanale raggruppato.

## File prodotti

- `crates/lightvm-service/Cargo.toml`, `src/{lib,main,compat,frame,credential,ratelimit,lock,server,supervisor,client,parent,test_support}.rs`, `src/transport/{mod,windows,unix}.rs`, `src/bin/lightvm-ipc-probe.rs`, `tests/ipc.rs`, `scripts/f1-04-windows-checks.ps1`
- `.github/workflows/ci.yml`, `.github/dependabot.yml`, `deny.toml`
- `docs/verification/F1-04-service-ipc-ci.md` (questo file)
- `lab/logs/f1-04-*` (fuori dal repository)
