# F0-09 — Riesame delle prove di F0: decisione di prosecuzione e backlog F1

- Data di esecuzione (UTC): 2026-10-09
- Esecutore: architetto responsabile del riesame (agente Claude Code)
- Passo della roadmap: sezione 15, passo 6 ("Riesame delle prove: decisione di prosecuzione e backlog F1"); sezione 10, F0 (criterio di uscita, decisione di arresto o cambio target, laboratorio Linux assente); sezioni 5, 13, 17
- ADR collegati: tutti; chiusi o aggiornati in questo passo: ADR-0010, 0011, 0012, 0013, 0014, 0016, 0018, 0019, 0024, 0025
- Esito complessivo: **parziale**. Criterio di uscita di F0 soddisfatto con deviazioni pubblicate per boot, console/input, canali, ripristino, supervisione, limiti risorse e fixture; F0 **non è ancora chiusa** perché restano i pendenti elencati nella sezione 3, tra cui la conferma dell'utente sull'esito "verificato solo Windows".

## Scopo

Riesaminare i file di verifica F0-01…F0-08, valutare ogni elemento del criterio di uscita di F0 (sezione 10), pubblicare deviazioni e pendenti, confrontare le misure con gli obiettivi della sezione 13 e proporre la decisione di prosecuzione con il backlog F1.

Questo file **non** esegue nuove prove: ogni esito riportato rimanda a un file di verifica e alla sua sezione. Non dichiara nulla "supportato" (sezione 5) e non modifica gli obiettivi della sezione 13.

## Fonti riesaminate

| File | Passo | Esito complessivo dichiarato nel file | Stato del file al riesame |
| --- | --- | --- | --- |
| `F0-01-toolchain-eframe.md` | 1 | (per voce) build Windows verificata con ambiente MSVC esplicito; glow senza accelerazione fallito; RDP e Linux non eseguiti | completo |
| `F0-02-qemu-whpx-qmp.md` | 2 (+ parte del 4) | (per voce) WHPX, QMP su socket Unix, NIC assente, arresto forzato verificati; lock immagini e `-cpu host` falliti | completo |
| `F0-03-guest-media.md` | 1–2 | verificato (integrità e firma del media); boot non eseguito in quel file | completo |
| `F0-04-console-rfb-input.md` | 3 | parziale | completo |
| `F0-05-baseline-overlay-firmware.md` | 4 | parziale | completo |
| `F0-06-supervisore-processi.md` | 4 | parziale | completo (la serie di 100 campioni di Emergency Stop durante il boot è stata completata durante il riesame e integrata qui) |
| `F0-07-linux-kvm.md` | 5 | parziale; smoke Linux/KVM su host fisico non eseguito | completo |
| `F0-08-limiti-risorse-misure.md` | 5 | parziale | completo |

Host di tutte le prove Windows: HP ZBook Fury 16 G9, Intel Core i7-12800HX (16 core / 24 processori logici), 31,6 GiB RAM, Windows 11 Pro 10.0.26200, volume E: NTFS su SSD NVMe (ADR-0018). Nessun risultato si estende ad altri host (sezione 5).

## 1. Criterio di uscita di F0: esito per elemento

Criterio (sezione 10, F0): boot accelerato e console/input dimostrati sul primo host; canali di controllo protetti; percorso di ripristino e supervisione fattibili; limiti risorse identificati e fixture definita; prove Windows e Linux, oppure blocco esplicito del target fallito, oppure "verificato solo Windows".

| # | Elemento | Esito | Evidenza (file, sezione) | Limiti |
| --- | --- | --- | --- | --- |
| E1 | Boot accelerato sul primo host | **verificato con deviazione** | `F0-02` §4.1 (`query-accelerators` → `enabled: whpx`, nessun TCG), §9.2 (desktop live Ubuntu 26.04.1 tra 105 e 120 s); `F0-04` Ambiente; `F0-05` §6; `F0-06` §2c | Solo con `-cpu qemu64`: `-cpu host` e `-cpu max` bloccano OVMF prima del display (`F0-02` §9.1, causa non determinata). Solo sessione live da ISO: installazione su disco non eseguita. Un solo host |
| E2 | Console sul primo host | **verificato** | `F0-04` Risultati §1–3: client RFB 3.8 interno nel processo UI, socket Unix in directory privata, `DIFF … pixel_diversi=0` fra framebuffer dell'app e `screendump` con wgpu DX12, glow e trasporto TLS | Hextile, CopyRect, Raw e DesktopSize solo con test su dati costruiti (QEMU ha usato sempre ZRLE); nessun cambio di risoluzione reale; prove con sessione Windows in gran parte bloccata |
| E3 | Input sul primo host | **verificato con deviazione** | `F0-04` §4: tastiera via QMP (GRUB, Ctrl+Alt+T, digitazione layout US e IT), mouse assoluto (`virtio-tablet`, cambio lingua "Deutsch"), percorso UI → tabella tasti → QMP → guest da `egui::Event` (test `console_reale`) | Tastiera fisica → winit → egui → QMP **non eseguito** (sessione bloccata, nessun `SendInput` sintetico); AltGr non verificato; egui-winit legge gli appunti host su Ctrl+V (testo scartato dalla UI) |
| E4 | Screenshot | **verificato** | `F0-04` §7: `screendump` end-to-end p95 19,2 ms (120 campioni) e 19,3 ms (100 campioni con animazione), 0 errori | Misurato a 1280×800 (risoluzione del guest), non 1280×720 |
| E5 | Canale QMP protetto | **verificato con limiti** | `F0-02` §4.3 (AF_UNIX con ACL solo-utente in `priv/`), §4.2 (named pipe scartata); `F0-04` §6 (processo a integrità bassa dello stesso utente rifiutato con `os error 10013`); `F0-06` §1 (circa 200 avvii con QMP su socket Unix) | Test con secondo utente locale **non eseguito**; test negativo da processo estraneo di `F0-06` non eseguito; QEMU serve un solo client QMP alla volta (`F0-04` §6) |
| E6 | Canale display protetto | **verificato con limiti** | `F0-04` §1 e §6 (VNC su socket Unix privato, rifiuto a integrità bassa); §5 (VeNCrypt X.509 con rustls: rifiuto di CA estranea, nome errato, indirizzo non di loopback) | Credenziale display per esecuzione emessa dal supervisor non realizzata (il supervisor di prodotto non esiste): la protezione provata è l'ACL della directory privata; su TLS l'arresto della sessione fallisce (`F0-04` §5); secondo utente non provato |
| E7 | Percorso di ripristino fattibile | **verificato** (fattibilità) | `F0-05` §6: 5 cicli baseline → overlay + vars per esecuzione → boot → stop → ripristino; hash di base e vars della baseline invariati; vars dell'esecuzione modificato dal firmware e ricreato; §8 crash durante scrittura e durante `qemu-img convert` con base invariata | 5 cicli, non i 30 di F3; guest live; overlay in uso scrivibile da terzi dello stesso utente (`F0-05` §7.3); un file `.partial` interrotto supera `qemu-img check` (`F0-05` §8.2) |
| E8 | Supervisione fattibile | **verificato con pendente** | `F0-06` §1 (QEMU nel Job Object dalla creazione, `KILL_ON_JOB_CLOSE`, nessun breakaway, ambiente con solo `SystemRoot`), §2a (morte del supervisor → fine di QEMU, p50 118,9 / 110,4 ms, max 143,9 ms, 10 cicli, 0 residui), §2b, §2c (Emergency Stop: p95 640,2 ms su 30 campioni a desktop; p95 273,3 ms su 100 campioni durante il boot), §3 (identità handle + tempo di creazione) | Crash della UI su Windows **non eseguito** (la UI non avvia ancora il supervisor); serie con desktop raggiunto di 30 campioni, non 100; arresto gentile ACPI mai concluso dal live (`F0-06` §2d, `F0-02` §9.2) |
| E9 | Limiti risorse identificati | **verificato** (identificazione) | `F0-08` Risultati: hard cap CPU e limiti memoria del Job Object senza elevazione con effetto reale su QEMU/WHPX; working set e priorità non applicabili senza elevazione; nessuna quota storage rigida senza amministratore; `F0-07` §4 (cgroup v2 delegato in WSL2, solo **[processi]**) | Soglie e frequenze non fissate; prove sotto pressione di storage e flood di artefatti non eseguite; limiti cgroup su QEMU con KVM non provati (nessun host fisico) |
| E10 | Fixture definita | **verificato** (definizione) | ADR-0019 (Accettata in questo passo), basata su `F0-04` §4 e `F0-05` §8.1 | Realizzazione entro F3 (sezione 9); calibrazione dei pixel di controllo del passo mouse da fissare alla realizzazione |
| E11 | Prove Windows e Linux, blocco esplicito o "verificato solo Windows" | **proposto "verificato solo Windows"**, in attesa della conferma dell'utente | `F0-07` (nessun host Linux fisico; WSL2 usato solo per build e meccanismi di processo, come ammesso dalla sezione 10); ADR-0016 | Senza conferma dell'utente l'esito non è registrato; la cella Ubuntu resta "non verificato" e le prove Linux sono rinviate a R2a |

### Attività bloccanti di F0 (sezione 10) e copertura

| Attività | Esito | Evidenza | Nota |
| --- | --- | --- | --- |
| Toolchain Rust, build minima eframe su Windows e Linux | verificato con limiti | `F0-01` §4; `F0-07` §2 | Windows richiede l'ambiente di Build Tools 2022 caricato esplicitamente (`LNK1104` altrimenti); Linux in WSL2 con driver C zig, non con `build-essential` |
| Backend di rendering (wgpu o glow) | verificato | `F0-01` §5; `F0-04` §3, §7 | decisione in ADR-0014 |
| Comportamento senza accelerazione grafica, per esempio RDP | verificato in parte | `F0-01` §6 (WARP verificato con CPU circa 7,9 processori logici; glow senza accelerazione fallisce); `F0-07` §3 (llvmpipe in WSLg) | sessione RDP reale **non eseguita** |
| QEMU con provenienza verificata; binario, versione, firmware, guest con checksum | verificato con limite | `F0-02` §2; `F0-03` | firma Authenticode dei binari QEMU non valida (certificato scaduto) |
| Probe senza credenziali utente nel guest | verificato | `F0-02` §4.1; `F0-06` §1 (ambiente di QEMU ridotto a `SystemRoot`) | |
| Avvio del guest su Windows/WHPX con configurazione documentata | verificato con deviazione | `F0-02` §9 | `-cpu qemu64` |
| Avvio su Linux/KVM se il laboratorio è disponibile | non eseguito (laboratorio assente) | `F0-07` | |
| QMP privato, console nativa, input, screenshot, arresto forzato | verificato con limiti | `F0-02` §4, §7; `F0-04` | E3, E5, E6 |
| Ciclo base immutabile / overlay / ripristino con firmware persistente | verificato | `F0-05` §6 | |
| Modello di processo: QEMU figlio del supervisor, terminazione OS, crash UI, crash supervisor | verificato con pendente | `F0-06` | crash della UI non eseguito su Windows |
| Limiti OS per CPU, memoria, storage; prima misura dei consumi | verificato con limiti | `F0-08` | |
| Threat model, restrizioni MVP, matrice iniziale, fixture | verificato (documenti) | `docs/SECURITY.md`, `docs/COMPATIBILITY.md`, ADR-0019 | |
| Prima versione di `ARCHITECTURE.md`, `SECURITY.md` e ADR critici | verificato (documenti) | `docs/ARCHITECTURE.md`, `docs/SECURITY.md`, `docs/adr/` | aggiornati in questo passo |

## 2. Deviazioni pubblicate

Voci verificate con un esito diverso da quello richiesto o atteso. Nessuna deviazione modifica un requisito della roadmap; ciascuna ha causa (nota o dichiarata non determinata) e intervento previsto.

| ID | Deviazione | Causa | Intervento previsto | Evidenza |
| --- | --- | --- | --- | --- |
| D-1 | Boot WHPX riuscito con `-cpu qemu64`, non con `-cpu host` (né `-cpu max`) | Non determinata; ipotesi: funzionalità CPUID esposte da `host`/`max` non gestite da WHPX con VBS/HVCI attivi | Profilo di riferimento con `qemu64` registrato in ADR-0011 e in `COMPATIBILITY.md`; in F1 boot completo con `Skylake-Client-v4` o `Haswell-v4` (arrivano a GRUB) e ripetizione di `-cpu host` dopo la verifica della feature `HypervisorPlatform` (azione utente U-1) | `F0-02` §9.1–9.2 |
| D-2 | Lock delle immagini di QEMU inefficace su Windows | QEMU 11.1.0 apre base e overlay con `FILE_SHARE_WRITE` | Esclusione garantita dal supervisor (ADR-0025): baseline aperta con `FILE_SHARE_READ`, apertura esclusiva come rilevatore, nessuna dipendenza dal rifiuto di `qemu-img` | `F0-02` §8.3; `F0-05` §7 |
| D-3 | Overlay in uso non proteggibile dalla scrittura di un altro processo dello stesso utente | La share mode non distingue i processi; QEMU stesso concede `FILE_SHARE_WRITE` | Limite dichiarato in `SECURITY.md` (coerente con la sezione 7.1, fuori perimetro); in F1 prova di directory per esecuzione con ACL esplicita e di identità di processo distinta per QEMU | `F0-05` §7.3, §7.5 |
| D-4 | Firma Authenticode dei binari QEMU non valida (certificato scaduto il 12/10/2023) | Certificato del firmatario della build Windows scaduto | Provenienza basata su HTTPS, SHA-512 pubblicato e SHA-256 coincidente con il manifest winget indipendente; rischio dichiarato in ADR-0010; verifica aggiuntiva (rebuild riproducibile o fonte alternativa) prima di F6 | `F0-02` §2.2 |
| D-5 | Arresto della sessione RFB su VeNCrypt/TLS fallito; connessioni in `CloseWait` lato QEMU | Lettura bloccante non interrompibile su TLS; QEMU non chiude subito le connessioni TLS terminate | VeNCrypt solo alternativa documentata (ADR-0013), non predefinita; in F2 prova con `connections=` e timeout del supervisor | `F0-04` §5 |
| D-6 | Feature `HypervisorPlatform` riportata *Disabled* via CIM, ma WHPX funzionante | Non determinata; DISM non interrogabile senza elevazione | Azione utente U-1; nessuna deduzione per altri host; la diagnostica F1 non deve basarsi sul solo stato CIM della feature | `F0-02` §1.1, §1.3 |
| D-7 | Arresto gentile ACPI (`system_powerdown`) mai concluso dal live Ubuntu | Comportamento del live (evento `POWERDOWN` emesso, nessun `SHUTDOWN`) | Scalata `system_powerdown` → `quit` → `TerminateJobObject` sempre presente, timeout configurabile; ripetizione con guest installato | `F0-02` §9.2; `F0-06` §2d |
| D-8 | QMP su named pipe creata da QEMU inadatto (blocco all'avvio, client singolo senza riconnessione, stallo in `prelaunch`, occupazione da parte del primo processo) | Comportamento della chardev `pipe` di QEMU 11.1.0 su Windows | Scartato in ADR-0012 a favore del socket Unix | `F0-02` §4.2 |
| D-9 | QEMU serve un solo client QMP alla volta | Comportamento del monitor QMP | Un solo proprietario della connessione QMP nel supervisor, che multiplexa input, screenshot ed eventi (backlog F1) | `F0-04` §6 |
| D-10 | Build MSVC fallisce senza ambiente esplicito (`LNK1104 msvcrt.lib`) | rustc sceglie un'installazione VS priva delle librerie CRT | Ambiente "Developer" esplicito in sviluppo e in CI; decisione utente sulla correzione dell'installazione locale (U-9) | `F0-01` §4.1 |
| D-11 | glow non ha ripiego software su Windows; WARP (via wgpu) consuma circa 7,9 processori logici e regge circa 5,5 aggiornamenti/s a 10 Hz | OpenGL senza driver offre solo GDI 1.1; rasterizzazione software | glow predefinito con ripiego wgpu/WARP come modalità degradata dichiarata (ADR-0014); frequenza console ridotta in quella modalità | `F0-01` §5–6 |
| D-12 | Memoria UI con wgpu DX12: private bytes 369–372 MiB, oltre l'obiettivo di 250 MiB già con la sola UI | Allocazioni del backend wgpu/driver DX12 (causa interna non isolata) | glow predefinito (88–98 MiB); wgpu selezionabile con deviazione dichiarata; analisi delle allocazioni wgpu in F2 | `F0-01` §5; `F0-04` §7 |
| D-13 | egui-winit legge gli appunti host alla pressione di Ctrl+V | Comportamento di egui-winit 0.36.2 (`on_keyboard_input`) | Il testo è già scartato dalla UI; blocco della lettura degli appunti host nel backlog F1, prima di F2 | `F0-04` §4 |
| D-14 | Limite di memoria del job superato a regime: QEMU termina con access violation invece di un errore gestito | Allocazione fallita non gestita (causa interna non studiata) | Limite dimensionato su RAM guest + margine misurato; classificazione `resource_limit` dalla notifica `JOB_MEMORY_LIMIT` (ADR-0017, Proposta) | `F0-08` Limiti di memoria |
| D-15 | Hard cap CPU del job rispettato in media, non in ogni secondo | Regolazione su intervalli di scheduling | Dichiararlo come limite medio; espressione in processori logici equivalenti | `F0-08` Hard cap CPU |
| D-16 | Nessuna quota storage rigida senza amministratore; quote NTFS per utente e volume | Privilegi; natura delle quote NTFS | Budget applicativo con monitoraggio e arresto, rischio di superamento dichiarato (ADR-0017, Proposta) | `F0-08` Risultati |
| D-17 | Su Linux (WSLg) percorso X11 in panico per `libxkbcommon-x11.so` mancante | Libreria caricata con `dlopen` assente | Dipendenza nel packaging Linux; panico trasformato in errore utilizzabile (backlog F1) | `F0-07` §3 |
| D-18 | Emergency Stop con desktop raggiunto misurato su 30 campioni, non sui 100 della sezione 13 (la serie durante il boot ha 100 campioni) | Ogni campione richiede il boot completo fino al desktop su host condiviso | Serie da 100 campioni con desktop o guest installato nelle misure formali (F1/F4) | `F0-06` §2c |

## 3. Pendenti

### 3.1 Pendenti per la chiusura formale di F0

| ID | Pendente | Elemento | Chi | Evidenza attesa |
| --- | --- | --- | --- | --- |
| P-1 | Conferma che in F0 non è disponibile un host Linux fisico con KVM (esito "verificato solo Windows") | E11 | utente (U-5) | registrazione in `docs/STATUS.md`; ADR-0016 diventa efficace |
| P-2 | Crash della UI su Windows con UI che avvia il supervisor come processo separato; decisione ADR-0015 parte B | E8 | sviluppo | sezione aggiunta a `F0-06` o nuovo file F0 |
| P-3 | Serie di 100 campioni di Emergency Stop durante il boot | E8 | sviluppo | **chiuso**: `F0-06` §2c, p50 142,7 ms, p95 273,3 ms, max 354,5 ms, 0 errori, 0 residui |
| P-4 | Tastiera fisica → winit → egui → QMP con sessione sbloccata, inclusi AltGr (layout IT), Ctrl+C/V, tastierino | E3 | utente (U-4) con sviluppo | sezione aggiunta a `F0-04` o nuovo file F0 |
| P-5 | Test negativo con secondo utente locale verso `qmp.sock` e `vnc.sock`, oppure decisione dell'utente di registrarlo come limite | E5, E6 | utente (U-3) | sezione aggiunta a `F0-04`/`F0-06` oppure limite registrato in `SECURITY.md` |
| P-6 | Registrazione della disponibilità di firma del codice e revisione indipendente (decisione da chiudere in F0, sezione 17) | — | utente (U-7) | `docs/STATUS.md`; ADR-0020 |
| P-7 | Dettagli del supporto dati approvato (strumento, formato, limiti, dispositivo, verifica di sola lettura) — decisione da chiudere in F0 (sezione 17) | — | sviluppo | sezione aggiuntiva di ADR-0008 |

### 3.2 Pendenti dichiarati come limite, con rimando

| ID | Pendente | Rimando |
| --- | --- | --- |
| L-1 | Sessione RDP reale e host senza GPU (comportamento eframe) | Limite dichiarato in `COMPATIBILITY.md`; azione utente facoltativa U-8; prova entro F2 |
| L-2 | Installazione di Ubuntu su disco e boot dal disco sotto WHPX | Backlog F1/F2; ADR-0011 criterio 2 |
| L-3 | Avvii UI a freddo (20 campioni), CPU manager in idle su 5 minuti, memoria UI + supervisor | Backlog F1/F2 (misure con processi di prodotto) |
| L-4 | Soglie storage, prove sotto pressione e flood di screenshot/log | ADR-0017 (Proposta); backlog F1/F3 |
| L-5 | Tutte le prove Linux su host fisico (KVM, QEMU reale, cgroup su QEMU, eframe con GPU, packaging) | R2a (ADR-0016) |
| L-6 | Hextile, CopyRect, Raw e DesktopSize inviati da un QEMU reale | Backlog F2 |

## 4. Confronto con gli obiettivi della sezione 13

Gli obiettivi non sono modificati. Le misure F0 sono prime misure su spike, non le misure formali di release.

| Misura | Obiettivo (sez. 13) | Misura F0 | Campioni | Esito rispetto all'obiettivo | Evidenza |
| --- | --- | --- | --- | --- | --- |
| Screenshot guest | p95 ≤ 500 ms a 1280×720 | p50 17,6 ms, p95 19,2 ms, max 20,5 ms (guest fermo); p50 16,4, p95 19,3, max 24,3 ms (animazione); 0 errori, 0 timeout | 120 + 100 | entro l'obiettivo, a 1280×800 | `F0-04` §7 |
| Memoria UI — glow | ≤ 250 MiB (UI + supervisor) | working set 91,8–94,5 MiB; private bytes 88,4–97,6 MiB (console attiva) | 1 serie da 60 s per condizione | solo UI: entro l'obiettivo con entrambe le metriche; supervisor non misurato | `F0-04` §7; `F0-01` §5 |
| Memoria UI — wgpu DX12 | ≤ 250 MiB (UI + supervisor) | working set 190,6–198,3 MiB; private bytes 369,2–372,0 MiB | 1 serie da 60 s per condizione | **deviazione D-12** con la metrica proposta (private bytes); working set entro l'obiettivo | `F0-04` §7 |
| Memoria UI — wgpu WARP | ≤ 250 MiB | working set 124,8 MiB; private bytes 376,3 MiB (spike senza console) | 1 | **deviazione** con la metrica proposta; modalità degradata | `F0-01` §5 |
| Avvio UI (caldo) | p95 ≤ 3 s | primo frame p50/p95: glow 294/326 ms, wgpu Vulkan 705/919 ms (20 avvii), wgpu DX12 501/538 ms (10 avvii); processo completo con `--smoke 0.5` p95 glow 653 ms, wgpu 1333 ms | 20 (10 per DX12) | entro l'obiettivo sul campione a caldo | `F0-01` §5 |
| Avvio UI (freddo) | p95 ≤ 3 s | non misurato | — | **assenza di misura** (pendente L-3) | `F0-01` §5 |
| Processi orfani | zero | 0 residui: 5 cicli di kill (`F0-02` §7), 5 cicli baseline e prove di crash storage (`F0-05`), 10 morti del supervisor, 6 kill esterni di QEMU, 130 Emergency Stop, 10 arresti con scalata (`F0-06`) | vedi colonna precedente | zero sui cicli eseguiti; scenario "crash app" non eseguito (P-2) | `F0-02`, `F0-05`, `F0-06` |
| Emergency Stop | p95 ≤ 2 s per fine processo QEMU | desktop: p50 394,3 ms, p95 640,2 ms, max 652,0 ms; boot: p50 142,7 ms, p95 273,3 ms, max 354,5 ms; 0 errori, 0 timeout, 0 residui | 30 (desktop) + 100 (boot) | entro l'obiettivo; serie a desktop con meno di 100 campioni (D-18) | `F0-06` §2c |
| CPU manager in idle | media < 1% di un core | indicazioni su 10–60 s: wgpu `--static` 0,16%, glow `--static` 0,47% (`F0-01`); console a guest fermo: wgpu 0,05%, glow 0,18% (`F0-04`) | finestre da 10 e 60 s | **assenza della misura formale** (5 minuti, UI + supervisor) | `F0-01` §5; `F0-04` §7 |
| Revoca | p95 ≤ 250 ms | non misurabile (sessioni da F4) | — | non eseguito | — |
| Spazio temporaneo | nessuna crescita non contabilizzata | contabilità per sessione registrata (overlay, vars 540 672 byte, log, screendump 3 072 016 byte per PPM); socket QMP orfano dopo arresto forzato, rimosso dallo spike | — | prima contabilità; serie di sessioni e pulizia dopo riavvio non eseguite | `F0-08` Prima misura; `F0-05` §9 |

Latenza console del ripiego screendump (sezione 6.4, nessun obiettivo numerico): il ripiego non è necessario sul primo host; la sua latenza misurata (p95 19,2 ms per la cattura, più la consegna via IPC non ancora esistente) è riportata in `COMPATIBILITY.md`.

## 5. Metrica di memoria proposta

Proposta: per l'obiettivo "Memoria manager ≤ 250 MiB" usare i **private bytes** su Windows (`PrivateMemorySize64`, cioè `PrivateUsage`), sommati su UI e supervisor; su Linux la metrica equivalente proposta è la memoria anonima privata `RssAnon + VmSwap` da `/proc/<pid>/status`, riportata con la propria etichetta. Il working set (Windows) e `VmRSS` (Linux) si pubblicano accanto, senza confonderli sotto la stessa etichetta (sezione 13).

Motivazione:

1. **I private bytes sono la memoria che il processo impegna nel sistema** (commit), non riducibile dal sistema operativo con il trimming del working set; il working set dipende dallo stato dell'host (pressione di memoria, finestra minimizzata) e quindi non è riproducibile tra esecuzioni.
2. **Il working set nasconde allocazioni reali del backend**: con wgpu DX12 il working set (circa 191–198 MiB) rientrerebbe nell'obiettivo mentre l'impegno effettivo è circa 370 MiB (`F0-04` §7). Usare il working set farebbe risultare raggiunto l'obiettivo su una metrica favorevole, contro la regola della sezione 13.
3. **Additività**: la somma dei private bytes di UI e supervisor non conta due volte le pagine condivise (DLL, mappature), che il working set include in ciascun processo.
4. Con glow le due metriche coincidono quasi (88–98 MiB), quindi la scelta non penalizza il backend proposto; penalizza correttamente le configurazioni che impegnano più memoria.

Conseguenza: ADR-0014 sceglie glow come backend predefinito su Windows; wgpu DX12 e WARP restano disponibili con la deviazione D-12 dichiarata.

## 6. Decisione di prosecuzione proposta

**Proposta: Windows 11 x86_64 resta primo target (e unico target del MVP con l'esito "verificato solo Windows").** Nessun controllo è fallito senza un'alternativa ammessa: i fallimenti osservati (`-cpu host`, lock immagini, arresto TLS, glow senza accelerazione) hanno un'alternativa verificata entro le opzioni della roadmap (modello CPU nominale, esclusione nel supervisor, socket Unix, ripiego wgpu/WARP). Non si applica il cambio target verso Linux (sezione 10: Linux non dispone del laboratorio) né il blocco. Nessun risultato usa TCG.

Verificato con deviazione sul primo host (sezione 2): D-1 `-cpu qemu64` invece di `host`; D-2 lock immagini QEMU inefficace, sostituito dall'esclusione del supervisor; D-4 firma Authenticode QEMU scaduta; D-5 arresto TLS fallito; D-6 `HypervisorPlatform` "Disabled" via CIM con WHPX funzionante; D-7 arresto ACPI non concluso dal live; D-8/D-9 limiti di QMP (named pipe scartata, client singolo); D-10 ambiente MSVC esplicito; D-11/D-12 rendering (glow senza ripiego software, memoria wgpu); D-13 appunti host letti da egui-winit; D-14–D-16 limiti risorse; D-18 campioni di Emergency Stop.

Pendente: P-1, P-2, P-4…P-7 per la chiusura formale (P-3 chiuso durante il riesame); L-1…L-6 dichiarati come limiti con rimando.

**Condizione di chiusura di F0:** F0 si chiude quando P-1, P-2 e P-4…P-7 hanno un esito registrato (verificato, fallito con conseguenza applicata, oppure limite deciso dall'utente dove la tabella 3.1 lo ammette). Fino ad allora `docs/STATUS.md` riporta F0 "in riesame". La sezione 10 stabilisce che ogni fase parte dopo il criterio di uscita della precedente: il lavoro F1 avviato in parallelo non può essere usato per dichiarare un criterio di uscita F1 prima della chiusura di F0, e il suo stato va riconciliato con questo riesame.

Se un pendente fallisce:

| Pendente | Conseguenza in caso di fallimento |
| --- | --- |
| P-1 (host Linux disponibile) | Lo smoke test Linux/KVM va eseguito in F0; l'esito diventa "Windows e Linux verificati" o un blocco esplicito per Linux |
| P-2 (crash UI) | Se il supervisor non sopravvive o non arresta le VM in modo controllato, ADR-0015 parte B resta aperta e F0 non si chiude sull'elemento E8 |
| P-4 (tastiera fisica) | La mappatura winit → qcode va corretta prima di stabilizzare la UI (sezione 6.4) |
| P-5 (secondo utente) | Se un altro utente si collega ai socket, ADR-0012/0013 tornano a Proposta e si prova il socket creato dal supervisor e passato per descrittore, o VeNCrypt con certificato per esecuzione |

## 7. ADR: esito del riesame

| ADR | Stato dopo il riesame | Decisione o motivo |
| --- | --- | --- |
| 0010 | Accettata | QEMU 11.1.0 da qemu.weilnetz.de, estrazione portabile, checksum; firma scaduta come rischio; winget alternativa con elevazione |
| 0011 | Accettata | Ubuntu 26.04.1 LTS desktop amd64; OVMF edk2 in pflash, vars copiato per esecuzione; `pc-q35-11.1`; `-cpu qemu64`; Secure Boot disattivo |
| 0012 | Accettata | QMP su socket Unix in directory privata; named pipe scartata; un solo client QMP posseduto dal supervisor |
| 0013 | Accettata | VNC su socket Unix primario; VeNCrypt alternativa documentata con il problema dell'arresto; ripiego screendump fattibile |
| 0014 | Accettata | egui/eframe 0.36.2; glow predefinito su Windows, wgpu DX12 selezionabile, WARP modalità degradata |
| 0016 | Accettata (condizionata alla conferma dell'utente) | "Verificato solo Windows"; cella Ubuntu "non verificato"; prove Linux in R2a |
| 0018 | Accettata | Host di sviluppo come primo host di misura Windows |
| 0019 | Accettata | Fixture definita (sezione 9: realizzazione entro F3) |
| 0024 (nuovo) | Accettata | Crate `rfb-client` separato usato dalla UI; `qemu-backend` resta nel supervisor |
| 0025 (nuovo) | Accettata | Esclusione dei dischi garantita dal supervisor, non dal lock di QEMU |
| 0015 | Proposta | Parte A Linux con prove **[processi]** in WSL2; parte B in attesa del crash della UI su Windows (P-2) |
| 0017 | Proposta | Meccanismi Windows identificati; soglie, prove sotto pressione e parte Linux su host fisico mancanti |
| 0020 | Proposta | Richiede la dichiarazione dell'utente (P-6) |
| 0021, 0022, 0023 | Proposta | Chiudibili entro F1 (sezione 17); 0022 con evidenze su egui_kittest |
| 0001, 0002, 0003, 0004, 0005, 0007, 0008, 0009 | Accettata (invariata) | Sezioni "Prove" aggiornate con i rimandi F0; 0002: crash della UI pendente |

## 8. Azioni che richiedono l'utente

Raccolte da tutti i file F0. Nessuna è stata eseguita da questo riesame.

| ID | Azione | Origine | Effetto |
| --- | --- | --- | --- |
| U-1 | In PowerShell **come amministratore**: `Get-WindowsOptionalFeature -Online -FeatureName HypervisorPlatform`; se disabilitata, valutare `Enable-WindowsOptionalFeature -Online -FeatureName HypervisorPlatform` (riavvio), poi ripetere `F0-02` §4.1 e §9.1 | `F0-02` Azioni richieste | Chiarisce D-6; può cambiare l'esito di `-cpu host` (D-1). Non bloccante |
| U-2 | Disporre di un host Linux fisico Ubuntu LTS x86_64 con KVM (utente non privilegiato con accesso a `/dev/kvm`, sessione grafica locale con GPU) | `F0-07` Azioni 1 | Se disponibile in F0, lo smoke Linux diventa obbligatorio in F0; altrimenti R2a |
| U-3 | Creare un secondo account locale non amministratore ed eseguire con le sue credenziali (`runas /user:…`) `f0-console probe-unix` verso `priv\vnc.sock` e `priv\qmp.sock`; in alternativa decidere di registrare la prova come limite | `F0-02` Azioni 3; `F0-04` Azioni 1; `F0-05` Limiti; `F0-06` Azioni | Chiude P-5 |
| U-4 | Con sessione Windows sbloccata, usare la tastiera fisica in `lightvm-desktop --vnc-socket … --qmp-socket …` digitando nel terminale del guest, inclusi AltGr con layout IT, Ctrl+C/V e tastierino | `F0-04` Azioni 2 | Chiude P-4 |
| U-5 | Confermare che in F0 non è disponibile un host Linux fisico con KVM e accettare l'esito "verificato solo Windows" | `F0-07` Decisioni proposte, ADR-0016 | Chiude P-1 |
| U-6 | Nella distro WSL: `sudo apt install build-essential pkg-config` (eventualmente `libxkbcommon-x11-0`), poi ripetere `lab/wsl/build-all.sh` senza il driver zig | `F0-07` Azioni 2 | Build Linux con toolchain standard; non bloccante per F0 |
| U-7 | Dichiarare chi detiene le credenziali di firma Windows e chi esegue la revisione indipendente, oppure registrarne l'indisponibilità | ADR-0020; sezione 14 | Chiude P-6 |
| U-8 | Facoltativo: aprire una sessione RDP verso l'host ed eseguire lo smoke di `lightvm-desktop` (adattatore automatico e `--wgpu-adapter cpu`) | `F0-01` §6 | Riduce il limite L-1 |
| U-9 | Decidere la correzione dell'ambiente MSVC locale (componente con librerie in VS 2026, oppure uso sistematico del Developer Prompt di Build Tools 2022) | `F0-01` §4.1 | Rimuove D-10 in sviluppo |
| U-10 | Facoltativo, come amministratore: `fsutil quota query E:` ed eventuale prova di quote NTFS su un volume di prova | `F0-08` Azioni 1 | Informazione per ADR-0017; le quote NTFS non isolano LightVM |
| U-11 | Facoltativo e **non valido per la matrice**: `sudo usermod -aG kvm mortali` e QEMU nella distro WSL per un probe KVM annidato | `F0-07` Azioni 3 | Solo integrazione; nessun effetto sulla cella Ubuntu |

## 9. Backlog F1

Il backlog F1 derivato dalla sezione 10 F1 e dalle azioni dei file F0 è in `docs/RELEASE_PLAN.md`, sezione "Backlog F1".

## Limiti del riesame

- Il riesame si basa sui file di verifica presenti al momento della lettura; `F0-06` è stato completato durante il riesame (serie di 100 campioni) e i suoi valori finali sono integrati.
- Le prove sono state eseguite su un host condiviso con altri agenti (QEMU paralleli, WSL): tempi di boot e latenze ne risentono (`F0-06`, `F0-08`).
- Nessuna misura di questo file è nuova: ogni valore è copiato dal file indicato.

## Decisioni proposte

Registrate negli ADR elencati nella sezione 7 e in `docs/STATUS.md`. La decisione di prosecuzione della sezione 6 diventa efficace con la conferma dell'utente (U-5) e la chiusura dei pendenti P-2 e P-4…P-7.
