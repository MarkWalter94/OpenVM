# ADR-0017 — Limiti OS e budget storage

- Stato: Accettata in parte (2026-10-10): **limiti per VM** impostati dall'utente e applicati dal supervisor su Windows; soglie host, contabilità completa dello storage, prove sotto pressione e Linux ancora aperti (F3, R2a)
- Data: 2026-10-09; decisione parziale: 2026-10-10
- Responsabile: responsabile del progetto

## Contesto

Il profilo AI richiede limiti CPU/memoria applicati all'albero processi, oltre alla configurazione virtuale del guest, e un budget complessivo di storage con riserva host e natura del controllo dichiarata (roadmap, sezione 7.2, punto 8). vCPU e RAM del guest non sono limiti del processo QEMU; la dimensione virtuale del disco non limita overlay, log e screenshot. In F0 vanno provati i limiti effettivi e fissati soglie, frequenza massima di controllo, riserva e limiti degli artefatti.

## Decisione

Da prendere. Va fissato, per Windows e Linux:

- meccanismo CPU e memoria sull'albero QEMU, con effetti dell'acceleratore osservati;
- natura del controllo storage: quota rigida OS (se disponibile) oppure budget applicativo con monitoraggio e arresto, con rischio di superamento dichiarato;
- soglie, frequenza massima di controllo, riserva per manifest ed esito minimo, limiti per screenshot e log;
- livello di confinamento addizionale disponibile (sezione 7.2, penultimi paragrafi), distinguendo configurazione guest, controllo delle API e sandbox del processo host.

Criteri:

1. Il limite è osservato sul processo QEMU e sui discendenti, non dedotto da una metrica.
2. Un controllo obbligatorio assente blocca AI Mode; un controllo addizionale assente è dichiarato come limite.
3. Una policy che richiede quota rigida non parte senza quella capacità.
4. Prove sotto pressione (riempimento, flood di screenshot/log).

### Stato delle prove al riesame di F0

Meccanismi **identificati** su Windows (`F0-08`), proposti e non ancora decisi perché mancano soglie e prove sotto pressione (criterio 4):

1. CPU: hard cap del Job Object (`JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP`) applicato prima dello spawn, espresso in processori logici equivalenti (`CpuRate = N × 10000 / processori_logici`). Vincolante anche per i vCPU WHPX su questo host (tempo dei vCPU contabilizzato sul processo QEMU); rispettato in media, non in ogni secondo (cap 5%: p95 5,5%, max 8,8% su campioni da 1 s).
2. Memoria: `JOB_OBJECT_LIMIT_JOB_MEMORY` sull'intero job, dimensionato come RAM guest più margine misurato (QEMU con `-m 4096`: 4 215 MiB di private bytes all'avvio, 4 536 MiB a regime). Un limite superato a regime fa terminare QEMU con access violation (0xC0000005): il supervisor classifica `resource_limit` dalla notifica `JOB_MEMORY_LIMIT` della porta di completamento.
3. Working set e priorità realtime del job: non applicabili senza elevazione (errore 1314); non usati.
4. Storage: nessuna quota rigida disponibile senza amministratore (`fsutil quota query` → accesso negato) e le quote NTFS sono per utente e volume, non per LightVM: profilo con budget applicativo, monitoraggio e arresto, rischio di superamento dichiarato; una policy che richiede quota rigida non parte su questo host. Contabilità per sessione: overlay, vars 540 672 byte, log, screendump (3 072 016 byte per PPM 1280×800), file `.partial` e `trash/` del journal.
5. Controlli del tasso di I/O e di rete del job: impostabili, effetto non misurato.

Linux: cgroup v2 delegato (`memory.max`, `cpu.max`, `pids.max`, `cgroup.kill`) verificato in WSL2 solo **[processi]** con figli di prova (`F0-07` §4); su QEMU con KVM rinviato a R2a.

Mancano per chiudere l'ADR: soglie, frequenza massima di controllo, riserva host, limiti per screenshot e log, prove sotto pressione (riempimento del volume, flood di screenshot e log), misura dell'effetto del tasso di I/O.

### Decisione del 2026-10-10 (responsabile del progetto)

**I limiti si impostano nelle impostazioni della VM**, si possono indicare **da riga di comando all'apertura di una VM** e, in F5, **da MCP**. Realizzazione e prove in `docs/verification/F1-10-limiti-vm-impostazioni.md`:

- `VmConfig.limits`: `cpu_max_percent` (5–100, predefinito 100), `memory_margin_mib` (256–16384, predefinito 1024), `storage_budget_gib` (opzionale). Validati dal core; mai applicati dal client.
- Windows: Job Object per esecuzione con hard cap CPU (`vcpus × percento / processori logici`) e memoria del job (RAM guest + margine), applicati prima dello spawn e riletti; memoria del job sotto la RAM → avvio rifiutato.
- Budget di storage: verificato a ogni avvio sui file della VM esclusi il disco principale; oltre il budget l'avvio è rifiutato (`resource_limit`). Contabilità completa e soglie host in F3.
- Modifica: solo a VM spenta, revisione attesa, disco solo in crescita, audit `vm_configure`; le sessioni agente non modificano le VM in F1. In F5 gli strumenti MCP potranno proporre limiti **solo più restrittivi** di quelli dell'utente (la policy dell'utente resta il tetto, sezione 7.2).
- Riga di comando: `lightvm-desktop --apri-vm NOME [--crea] [--vcpu N] [--ram MIB] [--disco GIB] [--cpu-max %] [--memoria-margine MIB] [--budget-disco GIB|nessuno]`, sullo stesso canale autenticato della UI.

Restano aperti per chiudere l'ADR: riserva dell'host, frequenza di controllo, limiti di screenshot e log, prove sotto pressione (riempimento del volume, flood), effetto misurato del tetto CPU con guest carico; parte Linux (cgroup v2) in R2a.

## Alternative considerate

Candidati da verificare:

- Windows: limiti CPU e memoria del Job Object (ADR-0002).
- Linux: controller cgroup v2 (`cpu.max`, `memory.max`), coerenti con ADR-0015.
- Storage: quote di progetto del filesystem (dove disponibili) contro budget applicativo con monitoraggio e arresto.

## Conseguenze

- La contabilità storage include baseline, overlay conservati, overhead QCOW2, firmware, registro media, log, screenshot e metadati (sezione 8).
- Nessuna promessa di protezione dal riempimento del volume causato da altri processi host.

## Prove richieste

- Limiti del Job Object, effetto reale su QEMU/WHPX e prima misura dei consumi: `docs/verification/F0-08-limiti-risorse-misure.md` (verificato in parte; prove sotto pressione non eseguite).
- Contabilità storage e journal: `docs/verification/F0-05-baseline-overlay-firmware.md` §9.
- cgroup v2 in ambiente non accelerato: `docs/verification/F0-07-linux-kvm.md` §4 (**[processi]**).
- Prove sotto pressione e soglie: backlog F1/F3 in `docs/RELEASE_PLAN.md`.

## Riferimenti

Roadmap, sezioni 7.2, 8, 10 (F0), 13, 14, 15 (passo 5), 17.
