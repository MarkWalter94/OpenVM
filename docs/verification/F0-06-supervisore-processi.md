# F0-06 — Supervisore dei processi: Job Object, crash, Emergency Stop, identità

- Data di esecuzione (UTC): 2026-10-09
- Esecutore: agente di sviluppo (spike `spikes/f0-supervisor`)
- Passo della roadmap: sezione 15, passo 4 di F0 ("supervisione processi"); sezione 10 F0 ("modello di processo del supervisor… crash del supervisor"); sezioni 6.1, 6.3, 7.3, 13
- ADR collegati: ADR-0002, ADR-0009, ADR-0012, ADR-0015, ADR-0017
- Esito complessivo: **parziale** (supervisor, QEMU ed Emergency Stop verificati; crash della UI e test del canale QMP da processo estraneo non eseguiti)

## Scopo

Dimostrare sul primo host che un supervisor Rust:

1. avvia QEMU senza shell, con ambiente ripulito ed eredità degli handle limitata, dentro un Job Object con `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` e senza breakaway;
2. in caso di propria morte anomala fa terminare QEMU tramite il sistema operativo (scenario a);
3. rileva e qualifica la morte di QEMU causata dall'esterno (b);
4. esegue l'Emergency Stop con `TerminateJobObject` misurando la latenza fino all'uscita osservata (c) e l'arresto gentile con scalata (d);
5. riconosce QEMU tramite handle e tempo di creazione, non solo tramite PID.

Non dimostra: il processo UI separato e il suo crash (la UI non esiste ancora come processo che avvia il supervisor), l'IPC autenticato, il rifiuto di un processo estraneo sul socket QMP, il comportamento su Linux (`F0-07`), garanzie temporali del sistema operativo.

## Ambiente

| Voce | Valore |
| --- | --- |
| Host | Intel Core i7-12800HX (16 core / 24 thread), 31,6 GiB RAM, SSD NVMe |
| OS e build | Windows 11 Pro 10.0.26200; utente non amministratore, nessuna elevazione |
| Acceleratore | WHPX (`query-accelerators` → `whpx`) |
| Toolchain | Rust 1.99.0 MSVC (ambiente Build Tools 2022, `F0-01`); `windows-sys` 0.61.2, `uds_windows` 1.2.1, `serde_json` 1.0.151 |
| QEMU | 11.1.0 portabile (`lab/qemu/qemu-11.1.0/`, SHA-256 in `F0-02`) |
| Firmware | `edk2-x86_64-code.fd` sola lettura + copia per esecuzione di `edk2-i386-vars.fd` |
| Guest | `ubuntu-26.04.1-desktop-amd64.iso` (SHA-256 `601e30fb…1bda1f`), live, `-cpu qemu64 -smp 2 -m 4096` |
| Sessione | locale; QEMU headless (`-display none`), QMP su socket Unix in `lab/vms/f0-06/priv/` |
| Altre condizioni | Il processo dello spike non era già in un job (`IsProcessInJob` → `false`). Altri agenti eseguivano QEMU e WSL in parallelo sullo stesso host: le latenze sono misurate su un host condiviso; i residui sono contati sia sul totale dell'host sia sui soli QEMU di questo spike |

## Procedura

### Spawn

`spikes/f0-supervisor/src/win.rs`, funzione `spawn`: `CreateProcessW` con

- `lpApplicationName` = percorso assoluto di `qemu-system-x86_64.exe`; riga di comando costruita da un vettore di argomenti con quoting MSVC (test unitario `quoting_msvc`); nessuna shell;
- `STARTUPINFOEXW` con `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` = solo stdin (`NUL`), stdout e stderr (file in `lab/logs/<tag>.stdout.txt`/`.stderr.txt`, duplicati come ereditabili solo per la chiamata); `bInheritHandles=TRUE` ma limitato alla lista;
- `PROC_THREAD_ATTRIBUTE_JOB_LIST` = job anonimo creato dal supervisor: il processo è nel job **dalla creazione**, senza la finestra temporale di un `AssignProcessToJobObject` successivo;
- `CREATE_UNICODE_ENVIRONMENT` con blocco d'ambiente esplicito (nessuna eredità dall'ambiente del supervisor); `CREATE_NO_WINDOW` (variante `DETACHED_PROCESS` provata);
- directory di lavoro = directory di QEMU.

Job: `CreateJobObjectW(NULL, NULL)` (anonimo, handle non ereditabile), `JOBOBJECT_EXTENDED_LIMIT_INFORMATION.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` (`BREAKAWAY_OK` e `SILENT_BREAKAWAY_OK` assenti), porta di completamento associata per le notifiche (`NEW_PROCESS`, `EXIT_PROCESS`, `ACTIVE_PROCESS_ZERO`, limiti di memoria).

Il codice `unsafe` è confinato in `win.rs` con `#![allow(unsafe_code)]` a livello di crate motivato in `lib.rs` e un commento `SAFETY:` per ogni blocco.

### Comandi

```text
cargo build --release -p f0-supervisor          (ambiente Build Tools 2022)
target\release\f0-supervisor.exe env-probe                                  → lab/logs/f0-06-env-probe.log
target\release\f0-supervisor.exe pid-reuse                                  → lab/logs/f0-06-pid-reuse.log
target\release\f0-supervisor.exe scenario-a --cycles 5 --mode abort --delay-ms 15000 → f0-06-scenario-a-abort.log
target\release\f0-supervisor.exe scenario-a --cycles 5 --mode kill  --delay-ms 15000 → f0-06-scenario-a-kill.log
target\release\f0-supervisor.exe scenario-b --cycles 6                      → f0-06-scenario-b.log
target\release\f0-supervisor.exe scenario-c --samples 30 --wait-min-s 125 --wait-max-s 140 --label desktop → f0-06-scenario-c-desktop.log
target\release\f0-supervisor.exe scenario-c --samples 100 --wait-min-s 5 --wait-max-s 60 --label boot     → f0-06-scenario-c-boot.log
target\release\f0-supervisor.exe scenario-d --cycles 5 --boot-wait-s 130 --powerdown-timeout-s 45 --quit-timeout-s 5 → f0-06-scenario-d-boot130.log
target\release\f0-supervisor.exe scenario-d --cycles 5 --boot-wait-s 180 --powerdown-timeout-s 60 --quit-timeout-s 5 → f0-06-scenario-d.log
```

Dopo ogni ciclo lo spike conta i processi `qemu-system-x86_64.exe` con Toolhelp (equivalente di `Get-Process -Name`) e, per distinguere quelli di altri agenti, con `Get-CimInstance Win32_Process` filtrando la riga di comando su `lightvm-f0-0[568]`.

## Risultati

| Voce | Esito | Evidenza |
| --- | --- | --- |
| 1a. Spawn senza shell, argomenti strutturati, percorso esplicito | verificato | `CreateProcessW` con `lpApplicationName` assoluto; quoting MSVC con test unitario |
| 1b. Ambiente minimo necessario a QEMU | verificato | ambiente vuoto: QEMU esce con `-qmp unix:…: Failed to create Unix socket: Input/output error`; con il solo `SystemRoot` QMP e WHPX funzionano |
| 1c. Eredità limitata ai tre handle standard | verificato per costruzione, non osservato dall'esterno | `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`; nessuno strumento di enumerazione degli handle disponibile senza installazioni (vedi Limiti) |
| 1d. QEMU nel Job Object dalla creazione, `KILL_ON_JOB_CLOSE`, nessun breakaway | verificato | `IsProcessInJob=true`, `limit_flags=0x2000`, notifica `NEW_PROCESS` per QEMU |
| 1e. `conhost.exe` di QEMU nel job | verificato | con `CREATE_NO_WINDOW`: figlio `conhost.exe` presente, `IsProcessInJob=true`, PID nella lista del job; con `DETACHED_PROCESS`: nessun `conhost.exe` |
| 2a. Supervisor terminato con `abort()` → QEMU termina (5 cicli) | verificato | latenza p50 118,9 ms, max 136,0 ms; conhost terminato con QEMU; 0 residui |
| 2a'. Supervisor ucciso dall'esterno (`TerminateProcess`, 5 cicli) | verificato | latenza p50 110,4 ms, max 143,9 ms; 0 residui |
| 2b. QEMU ucciso dall'esterno → rilevato dal supervisor (6 cicli) | verificato | attesa sull'handle conclusa con codice 1 (`taskkill /F`) o 0xFFFFFFFF (`Stop-Process -Force`); notifiche `EXIT_PROCESS` per QEMU e conhost, `ACTIVE_PROCESS_ZERO`; 0 residui |
| 2c. Emergency Stop (`TerminateJobObject`) con guest in esecuzione: 30 campioni a 125–140 s di boot | verificato, entro l'obiettivo | p50 394,3 ms, p95 640,2 ms, max 652,0 ms; 0 errori; 0 residui (vedi nota su un campione) |
| 2c'. Emergency Stop: 100 campioni durante il boot (5–60 s) | verificato, entro l'obiettivo | p50 142,7 ms, p95 273,3 ms, max 354,5 ms; 0 errori, 0 timeout; 0 cicli con residui |
| 2d. Arresto gentile: `system_powerdown` → `quit` → terminazione | verificato come procedura; **arresto ACPI non ottenuto** | 5 + 5 cicli: nessun `SHUTDOWN` del guest entro 45 s (boot) e 60 s (desktop); `quit` conclude in 410–788 ms; terminazione del job mai necessaria; 0 residui |
| 3. Identità: PID riusato non scambiato per QEMU | verificato | PID 35072 riassegnato a un nuovo processo dopo 1 678 creazioni; tempo di creazione diverso → `Reused`; con l'handle aperto il PID non è stato riassegnato in 2 000 creazioni |
| Crash della UI | non eseguito | la UI non avvia ancora il supervisor come processo separato |
| Rifiuto di un processo estraneo sul socket QMP (ADR-0012) | non eseguito | richiede un secondo account o un processo con identità diversa |

### 1. Ambiente, console e appartenenza al job (`env-probe`)

```text
supervisor (questo processo) già in un job: Ok(false)
[f0-06-env-empty-nowindow] stderr: …qemu-system-x86_64.exe: -qmp unix:E:\OpenVM\lab\vms\f0-06\priv\…sock,server=on,wait=off: Failed to create Unix socket: Input/output error
[f0-06-env-empty-nowindow] messaggi_job=[("NEW_PROCESS", 40820), ("NEW_PROCESS", 44960), ("EXIT_PROCESS", 40820), ("EXIT_PROCESS", 44960), ("ACTIVE_PROCESS_ZERO", 0)]
[f0-06-env-systemrootonly-nowindow] env=[("SystemRoot", "C:\\WINDOWS")]
[f0-06-env-systemrootonly-nowindow] QMP ok accel={"enabled": "whpx", …} status={"running": false, "status": "prelaunch"}
[f0-06-env-systemrootonly-nowindow] qemu_nel_job=true pid_nel_job=[8160, 26184] figli=[(26184, "conhost.exe", Some(true))]
[f0-06-env-systemrootonly-nowindow] limit_flags=0x2000 (KILL_ON_JOB_CLOSE=0x2000, BREAKAWAY_OK=0x800, SILENT_BREAKAWAY_OK=0x1000)
[f0-06-env-systemrootonly-nowindow] stop ms_processo=56.4 ms_job_vuoto=56.5
[f0-06-env-systemrootonly-detached] qemu_nel_job=true pid_nel_job=[15908] figli=[]
[f0-06-env-systemrootonly-detached] stop ms_processo=50.5 ms_job_vuoto=50.5
```

- **Variabili d'ambiente:** l'unica variabile risultata necessaria è `SystemRoot` (senza di essa l'inizializzazione di Winsock fallisce e QEMU non crea il socket QMP). Non servono `PATH` (le DLL sono nella directory dell'eseguibile), `TEMP`/`TMP`, `USERPROFILE`, `APPDATA` per la configurazione usata (firmware con percorsi assoluti, `-no-user-config`). Funzioni non coperte (per esempio file temporanei di snapshot o display grafici) potrebbero richiederne altre.
- **Console:** con `CREATE_NO_WINDOW` Windows crea un `conhost.exe` figlio di QEMU, che risulta nel job (ereditato alla creazione) e termina con esso; con `DETACHED_PROCESS` non esiste alcun `conhost.exe` e QEMU funziona ugualmente con stdout/stderr su file. `DETACHED_PROCESS` riduce l'albero a un solo processo.
- Con la VM in pausa (`-S`, 512 MiB) `TerminateJobObject` porta all'uscita in circa 50–56 ms.

### 2a. Morte del supervisor

Orchestratore (`scenario-a`) → processo supervisor figlio (`supervise`) → QEMU nel job del supervisor, ISO in boot da 15 s. L'orchestratore, esterno al job, apre handle propri su supervisor, QEMU e conhost (verificando il tempo di creazione di QEMU) e misura dall'uscita osservata del supervisor all'uscita osservata di QEMU.

```text
[f0-06-a-abort-1] supervisor pid=38560 READY pid=5216 creation=134360179801608497 conhost=40604 conhost_in_job=true identita_qemu_coincide=true
[f0-06-a-abort-1] supervisor_uscita=Some(3221226505) qemu_uscita=Some(0) ms_da_morte_supervisor_a_uscita_qemu=… conhost_uscita=Some(Ok(Some(0)))
RIEPILOGO scenario a (abort): n=5 min=105.0 p50=118.9 p95=136.0 max=136.0 media=120.5 fallimenti=0
[f0-06-a-kill-1] TerminateProcess(supervisor) dall'esterno
[f0-06-a-kill-1] supervisor_uscita=Some(1) qemu_uscita=Some(0) ms_da_morte_supervisor_a_uscita_qemu=128.63 conhost_uscita=Some(Ok(Some(0)))
RIEPILOGO scenario a (kill): n=5 min=96.2 p50=110.4 p95=143.9 max=143.9 media=115.1 fallimenti=0
```

`abort()` termina il supervisor con 0xC0000409; QEMU chiuso dal job esce con codice 0. Dopo ogni ciclo: `residui_spike=0`. In una prima serie (scartata dalle statistiche, stessi esiti di terminazione) il conhost non era ancora stato creato al momento della lettura dei figli: lo spike ora attende fino a 3 s la sua comparsa.

### 2b. QEMU ucciso dall'esterno

```text
[f0-06-b-1] killer="taskkill.exe" … rilevato_dal_supervisor=Some(1) (0x1) messaggi_job=[("NEW_PROCESS", 1284), ("NEW_PROCESS", 25132), ("EXIT_PROCESS", 1284), ("EXIT_PROCESS", 25132), ("ACTIVE_PROCESS_ZERO", 0)] identita=Same
[f0-06-b-2] killer="powershell.exe" … rilevato_dal_supervisor=Some(4294967295) (0xFFFFFFFF) …
[f0-06-b-1] job attivi=0 totali=2 terminati=0
```

Il supervisor attende sull'handle di processo e ottiene il codice d'uscita; la causa si classifica così: `0xE5E5` = Emergency Stop del supervisor (codice scelto dallo spike), 0 = uscita ordinata (`quit`), qualunque altro valore senza azione del supervisor = terminazione esterna o crash (con `ABNORMAL_EXIT_PROCESS` per i codici di eccezione, osservato in `F0-08` per il limite di memoria). Il tempo dall'avvio del comando esterno all'osservazione è 309–1 035 ms, dominato dall'avvio di `taskkill`/`powershell`: non è una misura della latenza di rilevazione, che non è stata isolata. `identita=Same` dopo la morte indica che, finché il supervisor tiene l'handle, il PID resta associato all'oggetto processo terminato (vedi 3).

### 2c. Emergency Stop

Misura: da subito prima di `TerminateJobObject` al ritorno di `WaitForSingleObject` sull'handle di QEMU ("uscita QEMU") e fino a `ActiveProcesses = 0` nella contabilità del job ("job vuoto", include conhost). Il guest è in esecuzione (`query-status` → `running`) e lo stato dello schermo è registrato con `screendump` subito prima.

Serie "desktop" (`f0-06-scenario-c-desktop.log`, attesa casuale 125–140 s):

| Sottoinsieme | n | min | p50 | p95 | max |
| --- | --- | --- | --- | --- | --- |
| tutti i campioni | 30 | 296,5 ms | 394,3 ms | 640,2 ms | 652,0 ms |
| desktop raggiunto (≥ 99,9 % pixel non neri) | 23 | 326,5 ms | 423,7 ms | 640,2 ms | 652,0 ms |
| boot non ancora al desktop (≤ 1,8 %) | 7 | 296,5 ms | 311,6 ms | — | 394,3 ms |

"Job vuoto" coincide con "uscita QEMU" al decimo di millisecondo in tutti i campioni. Nessun errore o timeout. Codice d'uscita di QEMU = 58853 (0xE5E5), cioè quello passato a `TerminateJobObject`. Confronto con la sezione 13 (obiettivo p95 ≤ 2 s, non modificato): p95 640,2 ms, **entro l'obiettivo** su questo campione. Il numero di campioni della serie desktop (30) è inferiore ai 100 richiesti dalla sezione 13 per le latenze; la serie di boot sotto ha 100 campioni.

La latenza cresce con la memoria guest effettivamente residente: circa 50 ms con VM in pausa (512 MiB), 90–355 ms durante il boot (p50 143 ms), 300–650 ms con il desktop (working set circa 4 GiB): la distruzione della partizione WHPX e il rilascio della memoria dominano il tempo.

Serie "boot" (`f0-06-scenario-c-boot.log`, 100 campioni, attesa casuale 5–60 s dall'avvio, guest `running` in tutti i campioni, schermo dal firmware al logo di avvio):

| n | min | p50 | p95 | max | media | errori/timeout | cicli con residui |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 100 | 89,8 ms | 142,7 ms | 273,3 ms | 354,5 ms | 157,7 ms | 0 | 0 |

Soddisfa il numero minimo di campioni della sezione 13 per questa condizione di carico (boot) e resta entro l'obiettivo p95 ≤ 2 s.

Nota sul campione 1 della serie desktop: il conteggio `residui_spike=1` registrato alle 11:19:25 coincide con l'avvio della prova di esclusione di `F0-05` (QEMU `lightvm-f0-05-excl-t0`, avviato alle 11:19:24), eseguita in parallelo e riconosciuta dallo stesso filtro `lightvm-f0-0[568]`; il QEMU del campione era uscito (attesa sull'handle conclusa, job vuoto). Non è un residuo dello scenario c, ma la misura di quel campione è avvenuta con un secondo QEMU attivo.

### 2d. Arresto gentile con scalata

Procedura: `system_powerdown` via QMP, attesa dell'uscita fino al timeout; poi `quit` con attesa di 5 s; poi `TerminateJobObject`.

| Serie | Stato prima del comando | Timeout powerdown | `SHUTDOWN` dal guest | Fase conclusiva | Tempo di `quit` | Totale p50 / max |
| --- | --- | --- | --- | --- | --- | --- |
| `f0-06-scenario-d-boot130.log` (5 cicli) | 130 s, schermo ancora all'avvio (1,5–1,7 %) | 45 s | no | `quit` | 410–523 ms | 45 843 / 45 993 ms |
| `f0-06-scenario-d.log` (5 cicli) | 180 s, desktop (≥ 99,9 %) | 60 s | no | `quit` | 465–788 ms | 60 910 / 61 048 ms |

L'evento `POWERDOWN` è emesso subito; l'unico `SHUTDOWN` osservato è quello causato da `quit` (al timeout). Il live di Ubuntu non si spegne via ACPI entro 60 s, coerente con `F0-02` (9.2). La scalata funziona e non lascia residui; l'arresto gentile reale andrà riprovato con un guest installato.

### 3. Identità del processo (`pid-reuse`)

```text
QEMU pid=35072 creation=134360177668174621
dopo la morte, handle ancora aperto: check_pid=Same
fase 1 (handle QEMU aperto): 2000 processi creati, riuso del PID 35072 = false
handle chiuso: check_pid=Absent
fase 2: PID 35072 riusato dopo 1678 processi; check_pid=Reused { creation: 134360178683131401 } → scambio evitato=true
```

Meccanismo: il supervisor conserva l'handle restituito da `CreateProcessW` (identità primaria) e la coppia PID + tempo di creazione (`GetProcessTimes`) come identità serializzabile. Finché l'handle è aperto, Windows non riassegna il PID (fase 1); un controllo basato sul solo PID dopo la chiusura dell'handle troverebbe un processo estraneo (fase 2), riconosciuto come diverso dal tempo di creazione. Attenzione: con l'handle aperto `OpenProcess(pid)` riesce anche a processo terminato (`Same`); lo stato "vivo" va quindi letto dall'attesa sull'handle, non dall'esistenza del PID.

## Limiti

- Host condiviso con altri agenti (QEMU della console F0-04, WSL): latenze e tempi di boot sono influenzati dal carico; in particolare il desktop non è stato raggiunto in tutti i campioni entro le attese previste.
- L'eredità degli handle è garantita dalla lista esplicita passata a `CreateProcessW`, ma non è stata verificata enumerando gli handle del processo QEMU (servirebbe Sysinternals `handle.exe` o codice basato su API non documentate).
- La morte del supervisor è provata con `abort()` e `TerminateProcess`; non con crash per eccezione non gestita né con stack overflow (stesso meccanismo di chiusura degli handle, non provato).
- La latenza di rilevazione dello scenario b non è stata isolata.
- La UI e il suo crash, l'IPC autenticato, il test negativo sul socket QMP e la sospensione/ripresa dell'host non sono coperti.
- Emergency Stop misurato senza flood di input QMP concorrente (test negativo "Revoca" della sezione 7.4, previsto in F4).

## Azioni richieste

- Nessuna azione di amministratore necessaria per queste prove.
- Facoltativo: un secondo account locale non amministratore per il test negativo sul socket QMP (ADR-0012) e sulle ACL della directory privata.
- Backlog F1: portare in `crates/qemu-backend` lo spawn con `HANDLE_LIST` + `JOB_LIST`, la classificazione dei codici di uscita, la scalata di arresto e l'identità handle + tempo di creazione; cicli start/stop reali (criterio F1: almeno 20) con questo codice.

## Decisioni proposte

Per **ADR-0002**:

1. QEMU creato dal supervisor con `CreateProcessW` + `PROC_THREAD_ATTRIBUTE_JOB_LIST` (nessuna finestra fuori dal job) e `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` (solo stdin `NUL`, stdout/stderr su file); job anonimo con `KILL_ON_JOB_CLOSE`, senza `BREAKAWAY_OK`/`SILENT_BREAKAWAY_OK`, con porta di completamento per le notifiche.
2. `DETACHED_PROCESS` invece di `CREATE_NO_WINDOW`: nessun `conhost.exe`, albero di un solo processo (entrambe le varianti verificate nel job).
3. Ambiente di QEMU = solo `SystemRoot` (più eventuali variabili aggiunte per esplicita necessità, documentate); nessuna eredità dall'ambiente del supervisor.
4. Identità: handle di processo conservato per tutta la vita dell'esecuzione + PID + tempo di creazione + `run_id`; mai decisioni sul solo PID.

Per **ADR-0009**:

5. Emergency Stop = `TerminateJobObject(job, codice_dedicato)` seguito da attesa sull'handle di QEMU e su `ActiveProcesses = 0`; completamento dichiarato solo dopo entrambe. Misurato p95 640 ms con desktop live (30 campioni) e p95 273 ms durante il boot (100 campioni); timeout di attesa proposto 10 s con errore visibile.
6. Arresto ordinario: `system_powerdown` con timeout, poi `quit`, poi `TerminateJobObject`; con il live di Ubuntu l'ACPI non ha mai concluso lo spegnimento, quindi il timeout del powerdown va reso configurabile e la scalata deve essere sempre presente.

Per **ADR-0015** (parte B, VM manuali dopo crash della UI): su Windows la morte del supervisor termina sempre QEMU entro circa 150 ms (10 cicli), quindi il "ricollegamento" è possibile solo se il supervisor sopravvive alla UI; se il supervisor muore, la VM manuale è persa (stato dell'overlay non pulito). La scelta resta da fare con la prova del crash della UI.

Per **ADR-0012**: il canale QMP su socket Unix in directory privata ha funzionato in tutte le esecuzioni (circa 200 avvii); il file socket resta orfano dopo l'Emergency Stop e va rimosso dal supervisor (lo spike lo fa). Il test negativo da processo estraneo resta da eseguire.

Per **ADR-0017**: i limiti CPU/memoria del job sono compatibili con questo modello di processo (vedi `F0-08-limiti-risorse-misure.md`).
