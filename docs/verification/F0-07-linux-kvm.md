# F0-07 — Linux/KVM: inventario, portabilità della build e meccanismi di processo

- Data di esecuzione (UTC): 2026-10-09
- Esecutore: agente di sviluppo (ruolo: ingegnere sistemi Linux/Rust), sessione su host Windows
- Passo della roadmap: sezione 15, passo 5 ("Smoke test Linux/KVM e controlli risorse"); sezione 10, F0 (paragrafo "Laboratorio Linux assente")
- ADR collegati: ADR-0015, ADR-0016, ADR-0014, ADR-0017, ADR-0009
- Esito complessivo: **parziale**. Smoke test Linux/KVM su host fisico: **non eseguito** (nessun host Linux fisico disponibile). Portabilità della build e meccanismi di processo in WSL2: eseguiti, con la validità limitata dichiarata qui sotto.

## Scopo

Stabilire quale ambiente Linux è disponibile su questa macchina e usarlo **solo** entro il limite fissato dalla roadmap (sezione 10, F0): "WSL2, macchine virtuali annidate o runner senza accelerazione verificabile non sostituiscono l'host fisico per questa prova; servono soltanto per build e test indipendenti dall'acceleratore".

Ogni prova di questo file ha una delle tre validità seguenti, indicate a ogni risultato:

| Etichetta | Significato |
| --- | --- |
| **[build]** | Portabilità della build e dei test headless su Linux. Non certifica l'host Linux. |
| **[processi]** | Meccanismi del kernel Linux per il ciclo di vita dei processi (ADR-0015, ammesso da ADR-0015 "in un ambiente Linux non accelerato, purché il file dichiari che la prova riguarda solo il ciclo di vita dei processi"). Il figlio di prova è `sleep`, non QEMU. |
| **[annidato]** | Qualunque cosa riguardi KVM dentro WSL2: virtualizzazione annidata, **NON valida per la matrice** (roadmap, "Laboratorio Linux assente"). |

Che cosa questo file **non** dimostra: boot accelerato KVM, funzionamento di QEMU su Linux, comportamento dei limiti cgroup su un processo QEMU con KVM, rendering eframe con GPU su Linux, supporto di Ubuntu LTS come host. La cella Ubuntu della matrice (sezione 5) resta **non verificato**.

## Ambiente

| Voce | Valore |
| --- | --- |
| Host fisico (modello, CPU, RAM, storage) | HP ZBook Fury 16 G9, Intel Core i7-12800HX (16 core, 24 processori logici), 31,6 GiB RAM (dati da F0-01); volume `E:` NTFS locale |
| OS host e build | Windows 11 Pro 10.0.26200 (build 26200.9457 secondo `wsl --version`) |
| Hypervisor host | `HypervisorPresent=True` (`Win32_ComputerSystem`); Docker Desktop 4.92.0 installato (distribuzione `docker-desktop`, ferma, non usata) |
| WSL | WSL 2.7.8.0, kernel `6.18.33.1-microsoft-standard-WSL2`, WSLg 1.0.73.2, MSRDC 1.2.6676, Direct3D 1.611.1, DXCore 10.0.26100.1 |
| `.wslconfig` utente | **assente** (`Test-Path $env:USERPROFILE\.wslconfig` → `False`): impostazioni predefinite, quindi nessuna impostazione esplicita di `nestedVirtualization` |
| Distribuzioni WSL | `Ubuntu` (predefinita, versione 2), `docker-desktop` (versione 2) |
| Distribuzione usata | Ubuntu 26.04 LTS "Resolute Raccoon", glibc 2.43, systemd 259 attivo (`/etc/wsl.conf`: `[boot] systemd=true`) |
| Risorse visibili nella VM WSL | 24 CPU, 15 791 MiB RAM, 4 GiB swap; `/` 1007 GiB (955 GiB liberi) |
| Utente nella distro | `mortali` (uid 1000), gruppi `adm cdrom sudo dip plugdev users`; **non** nel gruppo `kvm`; `sudo -n true` → "interactive authentication is required" (nessun sudo senza password) |
| Acceleratore e stato | **[annidato]** `/dev/kvm` presente (`crw-rw---- root kvm 10,232`), moduli `kvm_intel` e `kvm` caricati, `kvm_intel.nested=Y`, flag CPU `vmx` e `hypervisor` visibili. `/dev/kvm` **non accessibile** all'utente (non nel gruppo `kvm`). Virtualizzazione annidata: non valida per la matrice |
| cgroup | cgroup v2 unificato (`nsdelegate`); controller radice `cpuset cpu io memory hugetlb pids rdma`; `user@1000.service` delegato con `cpu memory pids` |
| Toolchain Rust | rustup 1.29.1, rustc 1.99.0 (b940084d7 2026-09-28), cargo 1.99.0 (5f94df478 2026-08-27), host `x86_64-unknown-linux-gnu`, selezionata da `rust-toolchain.toml` |
| Driver C / linker | **non standard**: zig 0.14.1 (`zig cc -target x86_64-linux-gnu.2.39`) tramite wrapper `lab/wsl/bin/cc`; vedi "Installazioni eseguite" |
| Binario QEMU | **assente** nella distro (`command -v qemu-system-x86_64` vuoto; `apt-cache policy qemu-system-x86`: Installed `(none)`, Candidate `1:10.2.1+ds-1ubuntu3`). Non installato, per regola del compito |
| Firmware / guest | non applicabile (nessun boot eseguito) |
| Sessione grafica | WSLg: `WAYLAND_DISPLAY=wayland-0`, `DISPLAY=:0`; `/dev/dxg` presente; Mesa 26.0.3, nessuna ICD Vulkan "dozen" installata |
| Altre condizioni | sorgenti compilati direttamente da `/mnt/e/OpenVM` (9p), `CARGO_TARGET_DIR=/mnt/e/OpenVM/lab/wsl/target` separata dal `target/` Windows; altri agenti modificavano in parallelo `apps/desktop`, `crates/rfb-client` e `spikes/*` |

### Installazioni eseguite (solo profilo utente WSL, nessun sudo)

| Componente | Dove | Provenienza e verifica | Motivo |
| --- | --- | --- | --- |
| rustup 1.29.1 | `~/.rustup` (617 MiB), `~/.cargo` (278 MiB, include registro crate) | `https://sh.rustup.rs`, SHA-256 dello script scaricato `7d0ea0f8eba7fa1ebfe998091cd7ec4501e33ec5ca6b884eb4d894d7da5170af`; opzioni `-y --default-toolchain none --profile minimal --no-modify-path` | nessun Rust nella distro |
| toolchain 1.99.0-x86_64-unknown-linux-gnu | `~/.rustup/toolchains` | installata automaticamente da rustup alla prima invocazione in `/mnt/e/OpenVM` (`rust-toolchain.toml`) | versione fissata dal workspace |
| zig 0.14.1 | `~/.local/opt/zig-x86_64-linux-0.14.1` (340 MiB) + cache `~/.cache/zig` (50 MiB) | `https://ziglang.org/download/0.14.1/zig-x86_64-linux-0.14.1.tar.xz`, SHA-256 `24aeeec8af16c381934a6cd7d95c807a8cb2cf7df9fa40d359aa884195c4716c`, uguale al valore pubblicato in `https://ziglang.org/download/index.json` | la distro non ha `gcc`/`cc`, `libc6-dev` (`crt1.o`, `libc.so`, header) né `pkg-config`; senza sudo non si possono installare `build-essential` e affini. `zig cc` fornisce driver C e stub glibc senza privilegi |

Nessun pacchetto apt, nessuna distribuzione WSL, nessuna feature Windows installata. Per rimuovere: `rm -rf ~/.rustup ~/.cargo ~/.local/opt/zig-x86_64-linux-0.14.1 ~/.cache/zig` nella distro.

## Procedura

Tutti gli script sono in `lab/wsl/` (directory ignorata da git); i log grezzi in `lab/logs/f0-07-*`. Comandi lanciati da Git Bash con `MSYS_NO_PATHCONV=1 wsl -d Ubuntu -- ...`.

1. Inventario Windows: `wsl --status`, `wsl -l -v`, `wsl --version`, `Test-Path $env:USERPROFILE\.wslconfig`, `(Get-CimInstance Win32_ComputerSystem).HypervisorPresent`.
2. Inventario distro: `lab/wsl/inventory.sh`, `lab/wsl/inventory2.sh` (kernel, `/dev/kvm`, gruppi, sudo, cgroup, systemd, toolchain, librerie, WSLg, `systemd-run --user --scope`). Log: `lab/logs/f0-07-inventory.txt`, `f0-07-inventory2.txt`, `f0-07-kvm-check.txt`.
3. Installazioni utente: `lab/wsl/install-user-tools.sh` (log `f0-07-install-user-tools.txt`).
4. Ambiente di build: `lab/wsl/env.sh`:
   ```sh
   export PATH="$HOME/.cargo/bin:/mnt/e/OpenVM/lab/wsl/bin:$PATH"
   export CC=/mnt/e/OpenVM/lab/wsl/bin/cc CXX=/mnt/e/OpenVM/lab/wsl/bin/c++ AR=ar
   export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=/mnt/e/OpenVM/lab/wsl/bin/cc
   export CARGO_TARGET_DIR=/mnt/e/OpenVM/lab/wsl/target
   export RUSTUP_AUTO_INSTALL=0
   ```
   Il wrapper `lab/wsl/bin/cc` esegue `zig cc -target x86_64-linux-gnu.2.39` scartando `--target=*`, `-Wl,--no-undefined-version` e `-Wl,-O1` (quest'ultimo produceva l'avviso del linker "ignoring deprecated linker optimization setting '1'").
5. Build e test **[build]**: `lab/wsl/build-desktop.sh` (`cargo build -p lightvm-desktop`), `lab/wsl/build-all.sh` (`cargo build --release -p lightvm-desktop`, `cargo build --release -p f0-linux-procs`, `cargo clippy -p f0-linux-procs --all-targets -- -D warnings`, `cargo test --workspace`), poi `lab/wsl/run.sh cargo test --workspace` ripetuto.
6. Smoke eframe sotto WSLg **[build]**: `lab/wsl/smoke.sh <binario> [opzioni]` avvia `lightvm-desktop --smoke 16`, legge `VmRSS`/`RssAnon`/`RssFile` da `/proc/<pid>/status` a 3 s e la CPU (`utime+stime` da `/proc/<pid>/stat`) tra 3 e 13 s; `lab/wsl/primo-frame.sh` esegue 20 avvii a caldo con `--smoke 0.5`.
7. Spike `spikes/f0-linux-procs` **[processi]**:
   ```sh
   f0-linux-procs processi --samples 30
   f0-linux-procs cgroup --samples 30                                   # dal cgroup di avvio di `wsl --`
   systemd-run --user --scope -p Delegate=yes --quiet -- f0-linux-procs cgroup --samples 30
   ```
8. Verifica su Windows che lo spike non rompa il workspace: `cargo check -p f0-linux-procs` e `cargo clippy -p f0-linux-procs -- -D warnings` (target dir temporanea in `lab/wsl/`, poi rimossa); `cargo +stable-x86_64-pc-windows-gnu run -p f0-linux-procs` stampa `f0-linux-procs: non applicabile su questo sistema operativo (spike solo Linux)` ed esce normalmente.

## Risultati

### 1. Inventario

| Voce | Esito | Evidenza |
| --- | --- | --- |
| Distribuzione WSL presente | verificato | `Ubuntu` (WSL2, predefinita), `docker-desktop` |
| Host Linux fisico con KVM | **non eseguito** (non disponibile) | nessun host Linux fisico in questa sessione |
| `/dev/kvm` nella distro | verificato **[annidato]** | presente, `root:kvm 0660`, `kvm_intel.nested=Y`; utente non nel gruppo `kvm` → non apribile |
| QEMU nella distro | non presente | `qemu-system-x86_64` assente |
| Probe `-accel kvm -S` + `query-accelerators` | **non eseguito** | QEMU assente (non installato per regola) e `/dev/kvm` non accessibile all'utente. Anche se eseguito, sarebbe stato "WSL2 annidato, NON valido per la matrice" |
| systemd e delega cgroup utente | verificato | systemd 259 PID 1; `systemd-run --user --scope -p MemoryMax=64M -p CPUQuota=20% true` → `SCOPE_OK` |

### 2. Build e test su Linux **[build]**

| Voce | Esito | Evidenza |
| --- | --- | --- |
| `cargo build -p lightvm-desktop` (debug) | verificato, con linker zig | `Finished dev profile ... in 2m 50s`, `EXIT=0 durata_s=171` (log `f0-07-build-desktop.txt`) |
| `cargo build --release -p lightvm-desktop` | verificato, con linker zig | `EXIT=0 durata_s=231`; binario 134 019 080 byte, SHA-256 `f0368ddd3b6bfdcd44a9b02590bccb0155a2e3b3ffd71c5ebdd2a92484e90ca2`, 2026-10-09 11:12:40 UTC |
| Librerie collegate staticamente nel binario | osservato | `ldd`: solo `libc.so.6`, `libm.so.6`, `ld-linux-x86-64.so.2`; X11, Wayland, xkbcommon, EGL e Vulkan sono caricati con `dlopen` a runtime, per questo la build non richiede pacchetti `-dev` |
| `cargo test --workspace`, primo tentativo (11:12 UTC) | **fallito**, causa esterna | `apps/desktop/src/console.rs`: `unresolved import rfb_client`, `cannot find module or crate f0_console`: modifica in corso di un altro agente su `apps/desktop`, non un problema di portabilità |
| `cargo test --workspace --exclude lightvm-desktop` | verificato | tutti i test superati (f0_console 6, rfb_client 10, `sola_lettura` 3, altri 0 test) |
| `cargo test --workspace`, secondo tentativo (completato 11:21 UTC) | verificato | `EXIT=0`, nessun warning; inclusi `lightvm_desktop` lib 5 test e `kittest_ui` 4 test (egui_kittest headless con AccessKit) |
| `cargo clippy -p f0-linux-procs --all-targets -- -D warnings` (Linux) | verificato | `EXIT=0` |
| `cargo check`/`clippy -p f0-linux-procs` (Windows) | verificato | nessun warning; esecuzione su Windows: messaggio "non applicabile" |

Validità: la build è riuscita con un driver C non standard (zig). La stessa build con `build-essential` di Ubuntu **non è stata eseguita**; vedi Azioni richieste.

### 3. Smoke eframe sotto WSLg **[build]**

Binario release del punto 2. Metriche Linux, **non confrontabili** con working set/private bytes di Windows (sezione 13): `VmRSS` = memoria residente totale; `RssAnon` = residente anonima (la più vicina ai private bytes); `RssFile` = pagine di file mappate residenti.

| Configurazione | Adattatore riportato | Primo frame (ms) | VmRSS / RssAnon / RssFile a 3 s | CPU 3–13 s (% di un core) | Frame / aggiornamenti fb in 16 s |
| --- | --- | --- | --- | --- | --- |
| wgpu predefinito, Wayland | `backend=Vulkan adapter="llvmpipe (LLVM 21.1.8, 256 bits)" tipo=Cpu driver_info="Mesa 26.0.3-1ubuntu1 (LLVM 21.1.8)"` | 366 | 173,8 / 46,2 / 96,2 MiB | 56,8% | 126 / 125 |
| wgpu, `--static` | stesso | 149 | 150,9 / 37,9 / 93,4 MiB | 0,80% | 6 / 1 |
| wgpu, `WGPU_BACKEND=gl` | `backend=Gl adapter="llvmpipe (LLVM 21.1.8, 256 bits)" tipo=Cpu driver_info="4.5 (Core Profile) Mesa 26.0.3-1ubuntu1"` | 265 | 165,0 / 65,4 / 89,8 MiB | 52,6% | 139 / 136 |
| wgpu, X11 (`env -u WAYLAND_DISPLAY`) | — | — | — | — | **fallito**: panico `xkbcommon-dl-0.4.2/src/x11.rs:59:28: Library libxkbcommon-x11.so could not be loaded.`, uscita 101 |
| Build debug, wgpu predefinito | llvmpipe Vulkan | 509 | 201,0 / 53,9 / 115,7 MiB | 50,7% | 156 / 155 |

Primo frame, 20 avvii a caldo (wgpu predefinito, release, `--smoke 0.5`): p50 251 ms, p95 424 ms, max 499 ms (log `f0-07-primo-frame-20.txt`).

Osservazioni:
- Adattatori disponibili (`--wgpu-adapter cpu` li elenca): solo `Vulkan "llvmpipe"` e `Gl "llvmpipe"`, entrambi di tipo Cpu. L'accelerazione GPU di WSLg (Mesa d3d12 su `/dev/dxg`) non è stata raggiunta: stderr `libEGL warning: failed to get driver name for fd -1`, `MESA: error: ZINK: failed to choose pdev`, `egl: failed to create dri2 screen`. Causa non indagata oltre. In pratica è una prova di **rendering software su Linux**, utile per la voce "host senza accelerazione grafica" di F0, non per le prestazioni con GPU.
- Con animazione a 10 Hz llvmpipe non mantiene il ritmo: 125 aggiornamenti in 16 s (circa 7,8/s) contro 152 su Windows/NVIDIA (F0-01). CPU circa 53–57% di un core, contro il 7% su Windows con GPU e 786% con WARP (F0-01). Il costo della composizione WSLg/RDP sul lato Windows non è incluso.
- Su X11 manca `libxkbcommon-x11.so` (pacchetto Ubuntu `libxkbcommon-x11-0`): requisito runtime da dichiarare nel packaging Linux. L'app va in panico invece di dare un errore utilizzabile: da correggere entro F1 ("errori utilizzabili dall'utente").

### 4. Spike `spikes/f0-linux-procs` **[processi]**

Kernel `6.18.33.1-microsoft-standard-WSL2`. Binario release del workspace, SHA-256 `6e19036c4ad76c3177ecefe846f7474816ed64f531b389ef4f4e8f0486867eb0`, esecuzione 2026-10-09 11:21 UTC (log `lab/logs/f0-07-procs-run2.txt`). 30 campioni per serie, timeout 5 s per campione. Tempo misurato con `Instant` (CLOCK_MONOTONIC) dall'invio del segnale fino a quando i pidfd di **tutti** i processi osservati diventano leggibili (uscita del processo). Figlio di prova: `sleep 1000` (QEMU assente).

| Scenario | Meccanismo | Esito | p50 / p95 / max (ms) | Timeout / errori / orfani |
| --- | --- | --- | --- | --- |
| a. SIGKILL al supervisor, fine del figlio diretto | `PR_SET_PDEATHSIG(SIGKILL)` in `pre_exec` + controllo `getppid()` | verificato | 0,700 / 1,142 / 3,820 | 0 / 0 / 0 |
| a2. Stesso, figlio che crea un nipote (`sh -c 'sleep 1000 & ...; exec sleep 1000'`) | PDEATHSIG non ereditato dopo `fork` | verificato il **limite** | figlio diretto terminato = sì; **nipote sopravvissuto = sì** (poi ucciso dallo spike) | — |
| b. Segnale e raccolta via pidfd | `pidfd_open`, `pidfd_send_signal(SIGKILL)`, `waitid(P_PIDFD, WEXITED)` | verificato | 0,080 / 0,161 / 0,191 | 0 / 0 / 0 |
| b. Identità senza riuso del PID | `si_code=CLD_KILLED`, `si_status=SIGKILL`; `pidfd_send_signal(fd, 0)` dopo la raccolta; `pidfd_open` su PID già raccolto | verificato | 30/30 `CLD_KILLED`+`SIGKILL`; 30/30 `ESRCH` sul pidfd vecchio; `pidfd_open` su PID raccolto → `ESRCH` | — |
| c. Arresto di gruppo | figlio capogruppo (`process_group(0)`) con 2 nipoti, `kill(-pgid, SIGKILL)` | verificato | 0,215 / 0,313 / 0,315 | 0 / 0 / 0 |
| c2. Nipote che esegue `setsid` | uscita dal gruppo e dalla sessione | verificato il **limite** | membri `pgid=8294,sid=8078`, `pgid=8295,sid=8295` (fuggito), `pgid=8294,sid=8078`; **1 sopravvissuto** a `kill(-pgid)` (poi ucciso dallo spike) | — |
| Crash della UI | il supervisor sorveglia la UI con un pidfd; alla morte (SIGKILL) invia SIGTERM alla VM, attende 2 s, poi SIGKILL, ed esce | verificato | 0,629 / 1,769 / 1,916 (fino alla fine di VM **e** supervisor) | 0 / 0 / 0 |
| Controllo finale | processi `sleep 1000` dell'utente rimasti | verificato | 0 | — |

cgroup v2 (stessa esecuzione):

| Scenario | Esito | Evidenza |
| --- | --- | --- |
| d. Sotto-cgroup dal cgroup di avvio di `wsl --` (`/init.scope`) | **non disponibile** | `mkdir /sys/fs/cgroup/init.scope/supervisor: Permission denied (os error 13)` |
| d. Sotto-cgroup in uno scope utente delegato (`systemd-run --user --scope -p Delegate=yes`) | verificato | base `/user.slice/user-1000.slice/user@1000.service/app.slice/run-p8389-i24777.scope`, controller `cpu memory pids`; il supervisor si sposta in una foglia `supervisor/` (regola "nessun processo interno") e abilita `+cpu +memory +pids` |
| d. `memory.max=128 MiB`, `memory.swap.max=0`, processo che alloca 512 MiB | verificato | processo terminato con segnale 9, `memory.events oom_kill=1`, `memory.peak=134217728` |
| d. `cpu.max="20000 100000"` (0,2 CPU), 4 thread occupati per 3 s | verificato | `core_medi=0.206`, `cpu_usata_s=0.623` su 3,031 s, `nr_throttled=31`, `throttled_usec=11424258` |
| d. Controllo senza limite (`cpu.max="max 100000"`) | verificato | `core_medi=3.965`, `nr_throttled=0` |
| d. `pids.max=4` | verificato | 3 `sleep` su 8 avviati (la shell conta come 1), `sh: 0: Cannot fork`, `pids.events max=1` |
| d. `cgroup.kill` su albero con nipote `setsid` (3 membri) — fine di tutti i membri (pidfd) | verificato | 30 campioni: p50 0,203 / p95 0,420 / max 0,511 ms; timeout 0, errori 0, orfani 0 |
| d. `cgroup.kill` — `populated 0` in `cgroup.events` | verificato | 30 campioni: p50 0,213 / p95 0,434 / max 0,526 ms; timeout 0 |

Una prima esecuzione completa (2026-10-09 11:08 UTC, stessa sorgente compilata fuori dal workspace in `lab/wsl/standalone/` mentre un altro membro `spikes/*` non era ancora caricabile; log `f0-07-procs-processi.txt`, `f0-07-procs-cgroup.txt`) ha dato valori dello stesso ordine: a 0,409/0,809/3,317; b 0,072/0,165/0,187; c 0,215/0,457/0,563; crash UI 0,557/2,870/3,047; `cgroup.kill` 0,239/0,549/0,641 ms; nessun timeout, errore o orfano.

Nota sul metodo: in una versione intermedia la lettura di `cgroup.events` aspettava `POLLPRI` con timeout di 10 ms e restituiva circa 10,6 ms per campione; con timeout di 1 ms e la misura separata sui pidfd il valore scende sotto 1 ms. La misura di riferimento è quella sui pidfd.

## Limiti

- **Nessun host Linux fisico.** Tutto è stato eseguito in WSL2 (VM Hyper-V su Windows). Lo smoke test Linux/KVM della sezione 10 F0 e della cella Ubuntu (sezione 5) non è stato eseguito.
- **KVM annidato non provato e comunque non valido.** `/dev/kvm` esiste ma non è accessibile all'utente; QEMU non è installato. Nessun numero KVM in questo file.
- **Toolchain non standard.** La build Linux usa zig 0.14.1 come driver C/linker (stub glibc 2.39) invece di gcc/binutils di Ubuntu. Il binario risultante non collega `libgcc_s`. Una build con `build-essential` può differire (linker, unwinding, flag) e va ripetuta.
- **Rendering solo software.** Sotto WSLg l'app ha usato llvmpipe; WSLg passa da un compositor Weston e RDP verso Windows: latenze e CPU non sono rappresentative né di un desktop Linux con GPU né di una sessione X11/Wayland nativa. Il percorso X11 è fallito per una libreria mancante.
- **Figlio di prova `sleep`, non QEMU.** QEMU è multi-thread, alloca la RAM del guest, usa KVM e può avere processi ausiliari. In particolare non sono verificati: contabilizzazione della memoria del guest e delle allocazioni KVM nel cgroup, effetto di `cpu.max` sui vCPU thread KVM, tempi di terminazione di un QEMU con guest in esecuzione (Emergency Stop p95 ≤ 2 s della sezione 13), comportamento di SIGTERM su QEMU.
- **Arresto controllato simulato.** Nello scenario "crash della UI" l'arresto controllato è SIGTERM → attesa 2 s → SIGKILL; con QEMU l'arresto controllato passerebbe da QMP (`system_powerdown`) e non è provato. Il ricollegamento di una nuova UI al supervisor (ADR-0015, parte B, seconda alternativa) **non è stato provato**: richiede l'IPC autenticato.
- **PDEATHSIG è legato al thread.** Per `prctl(2)` il segnale parte alla morte del *thread* che ha creato il figlio, non del processo. Lo spike crea i figli dal thread principale; un supervisor che lanciasse QEMU da un thread di un pool (per esempio un runtime asincrono) potrebbe uccidere la VM alla terminazione di quel thread. Comportamento documentato, non riprodotto qui.
- **Delega cgroup dipendente da systemd.** Il sotto-cgroup è stato creato solo dentro uno scope utente con `Delegate=yes` ottenuto con `systemd-run --user`. Senza sessione systemd utente (distribuzioni senza systemd, container, alcuni runner CI) il meccanismo non è disponibile. Non provato: creazione dello scope via D-Bus (`StartTransientUnit`) dal supervisor stesso.
- **Riuso del PID non forzato.** È verificato che il pidfd restituisce `ESRCH` dopo la raccolta del processo; un vero riuso dello stesso numero di PID non è stato provocato (con `pid_max` elevato richiederebbe moltissimi processi). La garanzia deriva dalla semantica documentata di pidfd.
- **Campioni.** 30 campioni per serie (il compito chiedeva almeno 20); la sezione 13 chiede 100 campioni per le latenze di stop e revoca su hardware dichiarato: queste misure su `sleep` in WSL2 non sostituiscono quella misura.
- **Concorrenza con altri agenti.** `apps/desktop` è cambiato durante la prova: il binario release smoke (11:12 UTC) precede le modifiche che hanno fatto fallire il primo `cargo test`; il secondo `cargo test --workspace` (11:21 UTC) riguarda lo stato successivo. L'aggiunta del membro `spikes/f0-linux-procs` registra il pacchetto in `Cargo.lock` (dipendenza `libc 0.2.190` già presente nel lockfile).

## Azioni richieste

Azioni dell'utente (nessuna eseguita da questa prova):

1. **Host Linux fisico con KVM** (Ubuntu LTS x86_64, sezione 5) per chiudere la cella Ubuntu della matrice e, in seguito, R2a. Requisiti minimi della prova: accesso a `/dev/kvm` per l'utente non privilegiato, QEMU con provenienza verificata, guest e firmware con checksum come per Windows (F0-02/F0-03), sessione grafica locale con GPU. Se non disponibile in F0, registrare l'esito "verificato solo Windows" (vedi Decisioni proposte).
2. Nella distro WSL, per una build Linux con toolchain standard: `sudo apt install build-essential pkg-config` (eventualmente `libxkbcommon-x11-0` per provare il percorso X11). Dopo l'installazione ripetere `lab/wsl/build-all.sh` senza `CC`/linker zig (rimuovere le righe relative da `lab/wsl/env.sh`).
3. Facoltativo e **non valido per la matrice**: per un probe KVM annidato in WSL2, `sudo usermod -aG kvm mortali` e `sudo apt install qemu-system-x86` (oppure usare il binario QEMU con provenienza verificata scelto in ADR-0010), poi `-accel kvm -S` + `query-accelerators` via QMP su socket Unix. Serve solo a scoprire problemi di integrazione, non a qualificare l'host.
4. Aggiornare l'indice di `docs/verification/README.md` con questo file e lo stato in `docs/STATUS.md`/`docs/COMPATIBILITY.md` (file fuori dal perimetro di questa prova).

Attività tecniche da riportare nel backlog (`docs/RELEASE_PLAN.md`):

5. Dichiarare nel packaging Linux le dipendenze runtime caricate con `dlopen` (almeno `libxkbcommon0`, `libxkbcommon-x11-0` per X11, `libwayland-client0`, `libEGL`/`libGL` o `libvulkan1` con Mesa) e trasformare il panico di `xkbcommon-dl` in un errore utilizzabile (F1, "diagnostica con errori utilizzabili").
6. Ripetere lo spike `f0-linux-procs` sull'host fisico con QEMU reale come figlio (ADR-0015), misurando 100 campioni di Emergency Stop con guest in esecuzione e i limiti cgroup sul processo QEMU con KVM (ADR-0017).
7. Verificare la creazione dello scope delegato senza `systemd-run` esterno (D-Bus) e il comportamento senza systemd utente.

## Decisioni proposte

Proposte per gli ADR collegati; non sono decisioni finché gli ADR non sono aggiornati.

### ADR-0015, parte A (meccanismo Linux)

Le prove **[processi]** indicano che nessun meccanismo da solo soddisfa i quattro criteri dell'ADR; la combinazione seguente li copre tutti in WSL2 con `sleep`:

1. **Ogni QEMU con `PR_SET_PDEATHSIG(SIGKILL)`** impostato in `pre_exec` con controllo `getppid()` contro la corsa "padre già morto". Copre il criterio 1 per il figlio diretto anche con `SIGKILL` al supervisor (p95 1,1 ms, nessun orfano su 30). Vincolo: i processi QEMU vanno creati da un thread che vive quanto il supervisor (thread dedicato allo spawn), non da thread di pool.
2. **Identità tramite pidfd** (`pidfd_open`, `pidfd_send_signal`, `waitid(P_PIDFD)`, `poll` per la fine del processo): criterio 2 verificato (30/30 `ESRCH` dopo la raccolta). Requisiti kernel: `pidfd_open` ≥ 5.3, `P_PIDFD` ≥ 5.4.
3. **Un cgroup v2 per VM** dentro uno scope utente systemd con `Delegate=yes`, con `memory.max`, `cpu.max`, `pids.max` e **`cgroup.kill`** (kernel ≥ 5.14) per l'Emergency Stop e per il recupero dopo un crash del supervisor: è l'unico meccanismo provato che termina anche i discendenti fuggiti con `setsid` (casi a2 e c2 falliscono con PDEATHSIG e `kill(-pgid)`). Funziona senza privilegi (criterio 3) ed è lo stesso strumento dei limiti di ADR-0017 (criterio 4).
4. **Gruppo di processi** per VM come ripiego quando la delega cgroup non è disponibile: utile per `kill(-pgid)`, ma non contiene discendenti che cambiano gruppo.

Conseguenza per la sezione 7.2 punto 8: dove la delega cgroup non è disponibile, i limiti CPU/memoria sull'albero QEMU mancano; per la regola "un controllo obbligatorio assente blocca AI Mode" l'AI Mode su quell'host resta disabilitato, mentre la gestione manuale può usare PDEATHSIG + gruppo di processi dichiarandone il limite. Da confermare in ADR-0017 e ADR-0009.

Da decidere ancora: come il supervisor ottiene lo scope delegato (avvio da parte della UI con `systemd-run --user --scope -p Delegate=yes`, oppure chiamata D-Bus `StartTransientUnit` dal supervisor) e se ogni VM ha un proprio scope systemd oppure un sotto-cgroup dello scope del supervisor (quest'ultimo è ciò che è stato provato).

### ADR-0015, parte B (VM manuali dopo crash della UI)

Provato solo l'**arresto controllato**: il supervisor rileva la morte della UI con un pidfd (indipendente dal PID, nessun polling) e arresta la VM; p95 1,8 ms fino alla fine di VM e supervisor con `sleep` e SIGTERM. Proposta: arresto controllato come comportamento iniziale (con QMP `system_powerdown` e timeout prima di SIGTERM/SIGKILL, da provare con QEMU), perché verificabile senza IPC; il ricollegamento resta un'alternativa da provare quando esiste l'IPC autenticato. La scelta finale spetta al responsabile dell'ADR, coerentemente con F0-06 su Windows.

### ADR-0016 (esito del laboratorio Linux)

- Su questa macchina **non esiste un host Linux fisico con KVM**: c'è soltanto WSL2 con KVM annidato, che la roadmap esclude esplicitamente come prova.
- **Giudizio:** F0 **può** chiudersi con l'esito "verificato solo Windows" secondo il paragrafo "Laboratorio Linux assente", **a condizione che**: (a) l'utente confermi che in F0 non è disponibile un host Linux fisico con KVM (in caso contrario la prova va fatta e l'esito sarebbe "Windows e Linux verificati" o un blocco esplicito); (b) Windows superi da solo tutti i controlli del criterio di uscita di F0 (F0-02…F0-06, fuori da questo file e qui non giudicati); (c) l'esito sia registrato in `docs/STATUS.md` e `docs/COMPATIBILITY.md`, con Windows unico target del MVP e la cella Ubuntu "non verificato". Questo file non chiude F0: fornisce la parte Linux ammessa (build e test indipendenti dall'acceleratore), come richiesto dalle conseguenze di ADR-0016.
- **Prove che restano pendenti** con quell'esito (rinviate a R2a, che non si chiude senza di esse):
  1. smoke test reale su host Linux fisico con KVM: QEMU con provenienza verificata, boot accelerato del guest, QMP privato su socket Unix, console e input, arresto forzato;
  2. ADR-0015 sull'host fisico con QEMU reale: crash della UI, crash del supervisor, Emergency Stop con 100 campioni, nessun orfano;
  3. limiti cgroup applicati a QEMU con KVM (memoria del guest e allocazioni del kernel, vCPU, `cgroup.kill`);
  4. build eframe con toolchain standard Ubuntu e smoke con GPU reale, Wayland e X11 nativi;
  5. pacchetti Linux e policy host (sezione 5, righe Ubuntu e Debian/Fedora).

## Proposta: CI minima per F1

Roadmap sezione 10, F1: "CI Windows/Linux; build macOS su runner adeguato entro F2". Sezione 13: a ogni modifica unit test, validazione schema, lint e build; boot accelerato, QMP, console e ripristino su runner fisici controllati. Il provider non è scelto qui.

| Job | Runner | Contenuto | Note |
| --- | --- | --- | --- |
| `lint` | Linux x86_64 qualsiasi | `cargo fmt --all --check`; `cargo clippy --workspace --all-targets --locked -- -D warnings` | senza acceleratore |
| `build-test-linux` | Ubuntu LTS x86_64, senza acceleratore | `apt install build-essential pkg-config`; `cargo build --workspace --locked`; `cargo test --workspace --locked` (inclusi test headless egui_kittest) | toolchain standard, non zig |
| `build-test-windows` | Windows 11 x86_64, senza acceleratore | ambiente MSVC esplicito (Build Tools, vedi F0-01 §4.1); `cargo build --workspace --locked`; `cargo test --workspace --locked` | il problema `LNK1104 msvcrt.lib` di F0-01 va riprodotto o escluso sul runner |
| `clippy-features` | Linux e Windows | clippy con `--no-default-features --features glow` per `lightvm-desktop` | mantiene compilabile il backend alternativo |
| `processi-linux` | Ubuntu LTS, senza acceleratore | `f0-linux-procs processi` (poi i test del supervisor reale): PDEATHSIG, pidfd, gruppi; parte cgroup solo se il runner offre una sessione systemd utente con delega, altrimenti registrata come "non eseguito" | indipendente dall'acceleratore |
| `processi-windows` | Windows, senza acceleratore | test Job Object del supervisor (F0-06) | indipendente dall'acceleratore |
| `smoke-render-software` | Linux con display virtuale (per esempio Weston headless o Xvfb + Mesa llvmpipe) e Windows con WARP (`--wgpu-adapter cpu`) | `lightvm-desktop --smoke 5`, inclusi Wayland e X11 su Linux | verifica il ripiego software e le dipendenze runtime (`libxkbcommon-x11-0`); non misura prestazioni |
| `acceleratore-assente` | Linux senza `/dev/kvm`, Windows senza WHPX | il rilevamento capacità deve produrre l'errore chiaro richiesto dal criterio di uscita F1, senza ripiego TCG | sfrutta proprio l'assenza di acceleratore |
| `licenze-dipendenze` | Linux | inventario licenze e advisory delle dipendenze (strumento da scegliere in ADR-0023) | chiusura attività F0 non bloccante entro F1 |
| `build-macos` | macOS Apple Silicon | `cargo build --workspace --locked` | entro F2; solo assunzioni di portabilità, nessun supporto dichiarato; se manca, rischio in `STATUS.md` |
| `lab-fisico-windows` / `lab-fisico-linux` | runner fisici controllati (Windows/WHPX; Linux/KVM quando disponibile) | probe acceleratore, boot accelerato, QMP, console, 20 cicli start/stop senza orfani, Emergency Stop | senza credenziali di produzione; non attivati automaticamente da PR non fidate; unici job che possono produrre prove di boot |

Non possono girare su runner senza accelerazione verificabile, né essere sostituiti con TCG: boot del guest, misure di Emergency Stop su QEMU reale, limiti di risorse sull'albero QEMU con acceleratore, console reale. Test di protocollo QMP/RFB contro server simulati sono ammessi (sezione 10 F1) e vanno registrati come mock.

## File prodotti

- `docs/verification/F0-07-linux-kvm.md` (questo file)
- `spikes/f0-linux-procs/Cargo.toml`, `spikes/f0-linux-procs/src/main.rs`, `spikes/f0-linux-procs/src/linux.rs`
- `lab/wsl/` (ignorata da git): `env.sh`, `bin/cc`, `bin/c++`, `inventory.sh`, `inventory2.sh`, `install-user-tools.sh`, `build-desktop.sh`, `build-all.sh`, `run.sh`, `smoke.sh`, `primo-frame.sh`, `standalone/Cargo.toml`, `target/`
- `lab/logs/f0-07-*` (ignorati da git): inventari, installazioni, build, test, smoke, misure dello spike
