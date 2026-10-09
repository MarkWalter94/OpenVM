# F0-08 — Limiti OS di CPU, memoria e storage; prima misura dei consumi (Windows)

- Data di esecuzione (UTC): 2026-10-09
- Esecutore: agente di sviluppo (spike `spikes/f0-supervisor`, sottocomandi `limits-probe`, `cpu-cap`, `mem-limit`)
- Passo della roadmap: sezione 10 F0 ("identificare i limiti OS disponibili per CPU, memoria e storage e registrare una prima misura dei consumi"); sezione 7.2 punto 8 e paragrafi successivi; sezione 13
- ADR collegati: ADR-0017, ADR-0002, ADR-0009
- Esito complessivo: **parziale** — limiti CPU e memoria del Job Object verificati con effetto reale su QEMU sotto WHPX; quota storage rigida non disponibile senza amministratore; misure su host condiviso con altri agenti; parte Linux in `F0-07`

## Scopo

1. Stabilire quali limiti del Job Object si possono applicare all'albero QEMU **senza elevazione** e quali no.
2. Verificare l'effetto reale (non la sola metrica) di `JOB_OBJECT_LIMIT_PROCESS_MEMORY`/`JOB_OBJECT_LIMIT_JOB_MEMORY` e dell'hard cap CPU (`JOBOBJECT_CPU_RATE_CONTROL_INFORMATION`) su QEMU con guest attivo, inclusa l'attribuzione del tempo dei vCPU WHPX al processo QEMU.
3. Registrare la capacità reale del laboratorio e una prima misura di CPU, memoria e spazio di una sessione.

Non dimostra: limiti su Linux (cgroup v2, `F0-07`), controllo del tasso di I/O e di rete (impostati ma effetto non misurato), prove sotto pressione di riempimento del volume o flood di screenshot/log, metriche del manager UI/supervisor (la UI non esiste ancora come processo misurabile).

## Ambiente

| Voce | Valore |
| --- | --- |
| Host | vedi tabella "Capacità del laboratorio" |
| OS e build | Windows 11 Pro 10.0.26200; utente non amministratore |
| Acceleratore | WHPX; Hyper-V e VBS/HVCI attivi (`F0-02`, 1.2) |
| Toolchain | Rust 1.99.0 MSVC; `windows-sys` 0.61.2 |
| QEMU | 11.1.0 portabile (`F0-02`, 2.2) |
| Guest | `ubuntu-26.04.1-desktop-amd64.iso` live, `-cpu qemu64 -smp 2 -m 4096`, firmware OVMF in pflash |
| Altre condizioni | Host condiviso: in parallelo erano attivi i QEMU dell'agente F0-04 e (in parte della giornata) WSL (`vmmemWSL` con circa 6 GiB di working set alla prima rilevazione). La RAM libera rilevata era 8,1 GiB su 31,6. Le misure CPU sono per processo/job e non dipendono dagli altri carichi, ma i tempi di boot sì |

### Capacità del laboratorio

Fonte: `lab/logs/f0-08-host-capacity.txt` (CIM, `Get-PhysicalDisk`, senza elevazione).

| Voce | Valore |
| --- | --- |
| OS | Microsoft Windows 11 Pro 10.0.26200, build 26200 |
| CPU | 12th Gen Intel Core i7-12800HX, 16 core / 24 processori logici |
| RAM | 31,6 GiB fisica; limite di commit 63,6 GiB (file di paging `C:\pagefile.sys` 32 768 MiB) |
| Volume del laboratorio | E: NTFS, 1 863,0 GiB totali, 744,2 GiB liberi, su Samsung SSD 970 EVO Plus 2 TB (NVMe) |
| Altri volumi | C: NTFS 952,8 GiB (308,8 liberi); D: NTFS 931,5 GiB (515,0 liberi); tutti su SSD NVMe |
| Confronto con il riferimento della sezione 13 | supera 4 core/8 thread, 16 GiB e 100 GiB liberi |

## Procedura

```text
target\release\f0-supervisor.exe limits-probe                                        → lab/logs/f0-08-limits-probe.log
target\release\f0-supervisor.exe cpu-cap --rate 0    --seconds 240                   → f0-08-cpu-rate0.log
target\release\f0-supervisor.exe cpu-cap --rate 2500 --seconds 150                   → f0-08-cpu-rate2500.log
target\release\f0-supervisor.exe cpu-cap --rate 500  --seconds 150                   → f0-08-cpu-rate500.log
target\release\f0-supervisor.exe cpu-cap --rate 200  --seconds 150                   → f0-08-cpu-rate200.log
target\release\f0-supervisor.exe mem-limit --kind process --limit-mb 1024 --seconds 60   → f0-08-mem-process-1024.log
target\release\f0-supervisor.exe mem-limit --kind process --limit-mb 4300 --seconds 150  → f0-08-mem-process-4300.log
target\release\f0-supervisor.exe mem-limit --kind job     --limit-mb 4300 --seconds 150  → f0-08-mem-job-4300.log
target\release\f0-supervisor.exe mem-limit --kind process --limit-mb 6144 --seconds 150  → f0-08-mem-process-6144.log
```

- `--rate` è il valore `CpuRate` di `JOBOBJECT_CPU_RATE_CONTROL_INFORMATION` con `JOB_OBJECT_CPU_RATE_CONTROL_ENABLE | JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP`, in centesimi di punto percentuale **dei cicli dell'intero sistema** (tutti i 24 processori logici): 2500 = 25 % del sistema = 6 processori logici; 500 = 5 % = 1,2 processori; 200 = 2 % = 0,48 processori. Applicato al job prima dello spawn (QEMU nasce già nel job, `F0-06`).
- La CPU è campionata ogni secondo da `JobObjectBasicAccountingInformation` (`TotalUserTime + TotalKernelTime` del job, cioè QEMU più il suo `conhost.exe`) e confrontata con `GetProcessTimes` del solo QEMU; la memoria con `K32GetProcessMemoryInfo` (working set, picco, `PrivateUsage`) e con `PeakProcessMemoryUsed`/`PeakJobMemoryUsed` del job.
- Ogni prova avvia il boot della ISO (carico CPU reale del guest: firmware, kernel, decompressione, avvio del desktop) e, nella prova senza cap da 240 s, prosegue sul desktop live inattivo.

## Risultati

| Voce | Esito | Evidenza |
| --- | --- | --- |
| `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, senza breakaway | verificato | `limit_flags=0x2000`; vedi `F0-06` |
| `JOB_OBJECT_LIMIT_PROCESS_MEMORY` e `JOB_OBJECT_LIMIT_JOB_MEMORY` senza elevazione | verificato | impostati (`limit_flags=8960` = 0x2300), letti indietro 1 GiB / 2 GiB |
| Hard cap CPU senza elevazione | verificato | `SetInformationJobObject(JobObjectCpuRateControlInformation)` riuscito |
| Effetto del limite di memoria sotto la RAM guest | verificato | QEMU esce subito: `cannot set up guest memory 'pc.ram'`, exit 1, notifica `PROCESS_MEMORY_LIMIT` |
| Effetto del limite di memoria tra avvio e regime | verificato (comportamento sfavorevole) | QEMU termina dopo circa 34 s con **0xC0000005** (access violation), notifiche `*_MEMORY_LIMIT` + `ABNORMAL_EXIT_PROCESS` |
| Tempo dei vCPU WHPX attribuito al processo QEMU | verificato | fino a 3,09 processori logici consumati dal job durante il boot; cumulato job = cumulato QEMU |
| Hard cap CPU 25 % con guest attivo | verificato, ma **non vincolante** su questo host | massimo osservato 11,6 % del sistema: QEMU con 2 vCPU non raggiunge il 25 % di 24 processori logici |
| Hard cap CPU che vincola davvero (5 %, 2 %) | verificato | p95 5,5 % e 2,4 % del sistema; boot rallentato in modo visibile |
| Limite working set (`JOB_OBJECT_LIMIT_WORKINGSET`) | **non applicabile senza elevazione** | errore 1314 "Il privilegio richiesto non appartiene al client" |
| Classe di priorità realtime sul job | **non applicabile senza elevazione** | errore 1314 |
| Controllo del tasso di I/O del job | impostato, effetto non misurato | `SetIoRateControlInformationJobObject` restituisce successo (10 MB/s) |
| Controllo del tasso di rete del job | impostato, effetto non misurato (il guest non ha NIC) | `JobObjectNetRateControlInformation` riuscito |
| Quota storage rigida NTFS | **non verificabile senza elevazione** | `fsutil quota query E:` → "Errore 5: Accesso negato." |
| Prima misura dei consumi | verificato (host condiviso) | tabelle seguenti |

### Limiti di memoria

| Prova | Private bytes all'avvio | Esito osservato | Notifiche del job |
| --- | --- | --- | --- |
| `PROCESS_MEMORY` 1024 MiB, guest 4096 MiB | — | QEMU esce dopo 0,2 s, exit 1, stderr `cannot set up guest memory 'pc.ram'` | `NEW_PROCESS` ×2, `PROCESS_MEMORY_LIMIT`, `EXIT_PROCESS` ×2, `ACTIVE_PROCESS_ZERO` |
| `PROCESS_MEMORY` 4300 MiB | 4 215 MiB | QEMU si avvia, il boot procede, poi QEMU termina dopo 34,3 s con codice 3221225477 (0xC0000005) senza messaggio su stderr | `PROCESS_MEMORY_LIMIT`, `ABNORMAL_EXIT_PROCESS`, `EXIT_PROCESS` (conhost), `ACTIVE_PROCESS_ZERO` |
| `JOB_MEMORY` 4300 MiB | 4 215 MiB | identico: uscita dopo 34,5 s, 0xC0000005 | `JOB_MEMORY_LIMIT`, `ABNORMAL_EXIT_PROCESS`, … |
| `PROCESS_MEMORY` 6144 MiB | 4 214 MiB | nessun effetto per 150 s; private 4 536 MiB, picco job 4 537 MiB; Emergency Stop finale in 602 ms | solo `NEW_PROCESS` ×2 |

Osservazioni:

- Con `-m 4096` QEMU impegna subito circa 4 215 MiB di private bytes (la RAM guest è allocata con commit all'avvio) e sale a circa 4 536 MiB durante il boot. Il limite va quindi dimensionato su RAM guest più un margine misurato (qui circa 440 MiB).
- Un limite troppo basso all'avvio produce un errore pulito e diagnosticabile; un limite superato **a regime** produce un crash di QEMU (access violation) invece di un errore gestito. Il supervisor può comunque riconoscere la causa dalla notifica `PROCESS_MEMORY_LIMIT`/`JOB_MEMORY_LIMIT` ricevuta sulla porta di completamento del job prima di `ABNORMAL_EXIT_PROCESS`.
- Working set e private bytes non coincidono: a desktop raggiunto il working set è circa 4 100 MiB (la RAM guest diventa residente), all'inizio del boot circa 118 MiB con private già oltre 4 200 MiB.

### Hard cap CPU e attribuzione dei vCPU

Percentuali per campione da 1 s. "% sistema" = tempo CPU del job / (tempo reale × 24 processori logici); "% di un core" = tempo CPU / tempo reale.

| Cap | Durata | % sistema p50 / p95 / max | % di un core p50 / p95 / max | Schermo a fine prova (pixel non neri) |
| --- | --- | --- | --- | --- |
| nessuno | 240 s (boot + desktop) | 4,6 / 8,9 / 12,9 | 109,3 / 212,4 / 309,3 | desktop (99,9 %) |
| 25 % (2500) | 150 s | 4,9 / 9,1 / 11,6 | 118,7 / 218,7 / 277,9 | 36,6 % (desktop in caricamento) |
| 5 % (500) | 149 s | 4,0 / 5,5 / 8,8 | 96,9 / 131,2 / 210,9 | 1,7 % (boot non concluso) |
| 2 % (200) | 150 s | 1,8 / 2,4 / 5,9 | 43,7 / 57,8 / 140,6 | 3,6 % (fase iniziale/GRUB) |

- **Attribuzione:** il tempo CPU cumulato del job coincide con quello del solo QEMU (per esempio 198,5 s contro 198,5 s a t = 150 s senza cap), quindi `conhost.exe` è trascurabile; il job arriva a 3,09 processori logici in un secondo, oltre le 2 vCPU, quindi include vCPU e thread di I/O/emulazione. Su questo host (Hyper-V con VBS) il tempo di esecuzione del guest sotto WHPX **è** contabilizzato sui thread vCPU del processo QEMU e soggetto al cap del job; nella rilevazione dei processi `vmmem*` fatta con una VM WHPX a desktop (`f0-08-host-capacity.txt`) non compare un processo `vmmem` per la VM WHPX; il `vmmemWSL` osservato in precedenza appartiene a WSL.
- **Effetto reale:** con cap 2 % il working set resta a circa 115 MiB per tutti i 150 s (il guest non arriva oltre le prime fasi), con cap 5 % a 150 s il boot non è concluso, mentre senza cap il desktop è presente a 240 s e la CPU scende sotto il 10 % di un core da circa t = 200 s. Il cap rallenta il guest, non lo termina.
- **Superamenti nel campione da 1 s:** il massimo supera il cap (8,8 % con cap 5 %, 5,9 % con cap 2 %) mentre il p95 resta vicino; la regolazione del sistema avviene su intervalli di scheduling e non garantisce il rispetto in ogni secondo. Va dichiarato come controllo "medio", non istantaneo.
- **Cap 25 %:** la richiesta "CPU misurata sotto il 25 %" è soddisfatta (massimo 11,6 %), ma lo è anche senza cap (massimo 12,9 %): su un host a 24 processori logici un cap al 25 % del sistema non limita una VM a 2 vCPU. Un limite significativo va espresso in processori logici e convertito: per "al massimo N vCPU equivalenti" `CpuRate = N × 10000 / processori_logici`.

### Prima misura dei consumi

Fonti: `f0-08-cpu-rate0.log`, `f0-08-mem-process-6144.log`, `f0-06-scenario-c-desktop.log`, `f0-05-*`.

| Misura | Valore | Note |
| --- | --- | --- |
| CPU QEMU durante il boot (0–180 s, senza cap) | p50 1,09 processori logici, picchi 2–3,1 | 4,6 % p50 del sistema |
| CPU QEMU con desktop live inattivo (t = 200–240 s) | 0–6,2 % di un core (0,00–0,26 % del sistema) | campioni a 5 s; il desktop mostra la finestra "Welcome to Ubuntu" |
| CPU cumulata per arrivare al desktop | circa 290 s di CPU per circa 200 s di boot | variabile con il carico dell'host |
| Private bytes QEMU | 4 215 MiB all'avvio, 4 536 MiB a regime | guest 4 096 MiB |
| Working set QEMU | 118 MiB all'avvio, circa 4 100 MiB a desktop | |
| Overlay dopo una sessione live senza scritture | 197 120 byte (invariato rispetto alla creazione) | `F0-05`, 5 cicli |
| Overlay dopo una sessione con `dd` del guest interrotto | 327 483 392 e 1 132 920 832 byte su disco | `F0-05`, 8.1 |
| Vars OVMF per esecuzione | 540 672 byte | |
| Log di una sessione QEMU | stderr circa 389 byte, stdout 0 byte | più il log dello spike (pochi KiB) |
| Screendump | 3 072 016 byte per PPM 1280×800 (circa 360 KiB convertito in PNG) | dimensione fissa per risoluzione |
| Socket QMP | file di 0 byte, orfano se QEMU è terminato forzatamente | rimosso dallo spike |

## Limiti

- Host condiviso con altri agenti: tempi di boot e latenze dipendono dal carico; le percentuali CPU per job no.
- Campionamento a 1 s: non osserva il comportamento del cap su intervalli più brevi.
- Una sola configurazione guest (2 vCPU, 4 GiB) e un solo carico (boot e desktop inattivo); nessun carico sintetico sostenuto nel guest.
- I/O rate e net rate solo impostati; nessuna misura dell'effetto.
- Quota NTFS non interrogabile senza amministratore; nessuna prova sotto pressione di riempimento del volume, né di flood di screenshot/log (richiesta da ADR-0017, criterio 4): **non eseguito**.
- Il crash di QEMU per limite di memoria a regime è osservato su 2 prove; non è stata studiata la causa interna (allocazione fallita non gestita in QEMU o in WHPX).

## Azioni richieste

All'utente (come amministratore, facoltative, nessuna è stata eseguita):

1. Per valutare una quota rigida dello storage: `fsutil quota query E:` e, se si vuole provarla, abilitare le quote NTFS su un volume di prova (`fsutil quota track E:` / `fsutil quota enforce E:`), poi ripetere le prove di riempimento. Le quote NTFS sono per utente e per volume, non per directory: non distinguono LightVM da altri programmi dello stesso utente.
2. Per provare `JOB_OBJECT_LIMIT_WORKINGSET` serve il privilegio richiesto dall'errore 1314 (concessione di criteri di sicurezza locali o esecuzione elevata): da valutare solo se il working set diventasse un controllo richiesto; non lo si propone per il MVP.

Backlog (`docs/RELEASE_PLAN.md`): prove sotto pressione di storage e flood di artefatti; misura dell'effetto del rate I/O; misure di CPU/memoria di UI e supervisor quando esistono come processi.

## Decisioni proposte

Per **ADR-0017**:

1. **CPU (Windows):** hard cap del Job Object (`JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP`) applicato al job prima dello spawn, espresso dalla policy in processori logici equivalenti e convertito con `CpuRate = N × 10000 / processori_logici` (arrotondato per difetto, minimo 1). Dichiararlo come limite medio su intervalli di scheduling, non istantaneo; vincolante anche per i vCPU WHPX su questo host (da riverificare per ogni host abilitato).
2. **Memoria (Windows):** `JOB_OBJECT_LIMIT_JOB_MEMORY` sull'intero job (copre QEMU ed eventuali discendenti), dimensionato come RAM guest + margine misurato (proposta iniziale: RAM guest + 512 MiB per questa build e configurazione), con verifica all'avvio che il commit iniziale sia sotto il limite. Il superamento a regime causa il crash di QEMU: il supervisor lo classifica come `resource_limit` usando la notifica `JOB_MEMORY_LIMIT` della porta di completamento.
3. **Working set e priorità:** non usati (richiedono privilegi).
4. **Storage:** nessuna quota rigida disponibile senza amministratore e le quote NTFS non isolano LightVM: profilo base con budget applicativo, monitoraggio e arresto, rischio di superamento dichiarato; una policy che richiede quota rigida non parte su questo host. Contabilità per sessione: overlay (da 192 KiB a dimensione virtuale), vars 540 672 byte, log, screendump (3 MiB per PPM 1280×800 prima della conversione), file `.partial`/`trash/` del journal (`F0-05`, 9).
5. Soglie, frequenza di controllo e riserva restano da fissare dopo le prove sotto pressione (non eseguite).
