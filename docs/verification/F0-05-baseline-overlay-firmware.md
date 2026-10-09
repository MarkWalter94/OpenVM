# F0-05 — Baseline, overlay e firmware a VM spenta; esclusione dei dischi; crash sullo storage

- Data di esecuzione (UTC): 2026-10-09
- Esecutore: agente di sviluppo (spike `spikes/f0-storage`, `spikes/f0-supervisor`)
- Passo della roadmap: sezione 15, passo 4 di F0 ("baseline/overlay/firmware"); sezione 8; sezione 10 F0 ("ciclo base immutabile/overlay/ripristino con firmware persistente"); anticipa le prove di crash di F3
- ADR collegati: ADR-0005, ADR-0002, ADR-0009, ADR-0017
- Esito complessivo: **parziale** (vedi tabella dei risultati)

## Scopo

Dimostrare sul primo host, con QEMU reale sotto WHPX:

1. il ciclo baseline immutabile → overlay QCOW2 più copia del firmware scrivibile (OVMF vars) per l'esecuzione → boot → stop → ripristino, con hash della baseline invariato (sezione 8, punti 3–6);
2. un meccanismo di esclusione dei dischi che **non** dipenda dal lock delle immagini di QEMU, fallito su Windows in `F0-02-qemu-whpx-qmp.md` (8.3);
3. il comportamento dello storage dopo un kill durante la scrittura (overlay scritto dal guest, `qemu-img convert` interrotto);
4. una proposta di journal e riconciliazione (solo documento).

Non dimostra: installazione di Ubuntu sul disco (il guest è il live ISO), consistenza applicativa del guest, i 30 cicli del criterio di uscita F3, protezione contro processi malevoli dello stesso utente o contro l'amministratore.

## Ambiente

| Voce | Valore |
| --- | --- |
| Host | Intel Core i7-12800HX (16 core / 24 thread), 31,6 GiB RAM, volume E: NTFS su Samsung SSD 970 EVO Plus 2 TB (NVMe), circa 744 GiB liberi |
| OS e build | Windows 11 Pro 10.0.26200; utente non amministratore, nessuna elevazione |
| Acceleratore | WHPX (`query-accelerators` → `enabled: whpx`, verificato a ogni avvio dello spike) |
| Toolchain | Rust 1.99.0 MSVC (ambiente Build Tools 2022, procedura di `F0-01`), `windows-sys` 0.61.2, `uds_windows` 1.2.1, `sha2` 0.10.9, `serde_json` 1.0.151 |
| QEMU | 11.1.0 portabile, `lab/qemu/qemu-11.1.0/` (SHA-256 dei binari in `F0-02`, 2.2) |
| Firmware | `share/edk2-x86_64-code.fd` in pflash sola lettura (SHA-256 `33090cc0…6dbb2a`); template NVRAM `share/edk2-i386-vars.fd` (SHA-256 `5d2ac383…be5d1e`) |
| Guest | `ubuntu-26.04.1-desktop-amd64.iso`, SHA-256 `601e30fb…1bda1f` (verifica in `F0-03`) |
| Configurazione VM | `-machine q35,pflash0=fwcode,pflash1=fwvars -accel whpx -cpu qemu64 -smp 2 -m 4096 -nodefaults -no-user-config -nic none`, ISO su `ide-cd`, overlay su `virtio-blk-pci` tramite `-blockdev driver=file` + `driver=qcow2`, `virtio-vga`, `-display none`, QMP su socket Unix in directory privata |
| Sessione | locale, headless per QEMU |
| Altre condizioni | Altri agenti eseguivano in parallelo QEMU (console F0-04) e WSL sullo stesso host: i conteggi dei processi distinguono i QEMU di questo spike (riga di comando con `-name lightvm-f0-05…`/`f0-06…`/`f0-08…`) dal totale dell'host |

## Procedura

Compilazione (in un `cmd` con `vcvars64.bat` di Build Tools 2022, come in `F0-01`):

```text
cargo build --release -p f0-supervisor -p f0-storage
```

Comandi eseguiti (log in `lab/logs/`):

```text
target\release\f0-storage.exe   baseline-cycles --cycles 5 --boot-s 60   → f0-05-baseline-cycles.log
target\release\f0-supervisor.exe exclusion   → f0-05-exclusion-run1.log e f0-05-exclusion.log (due esecuzioni, esiti identici)
target\release\f0-storage.exe   guest-write-crash --boot-s 170 --grow-mib 256    → f0-05-guest-write-crash-1.log
target\release\f0-storage.exe   guest-write-crash --boot-s 170 --grow-mib 1024   → f0-05-guest-write-crash-2.log
target\release\f0-storage.exe   guest-write-crash --boot-s 240 --grow-mib 1024   → f0-05-guest-write-crash-3.log
target\release\f0-storage.exe   convert-crash                            → f0-05-convert-crash.log
```

QEMU è sempre avviato dallo spike con `CreateProcessW` e argomenti strutturati (nessuna shell), ambiente ridotto a `SystemRoot`, eredità dei soli tre handle standard e assegnazione a un Job Object alla creazione (dettagli in `F0-06-supervisore-processi.md`). I percorsi passati a QEMU sono assoluti e rifiutati se contengono virgole (grammatica delle opzioni `-blockdev`).

### Layout del ciclo baseline

```text
lab/vms/f0-05/bl/
  baseline/base.qcow2      qemu-img create -f qcow2 base.qcow2 20G
  baseline/vars.fd         copia di share/edk2-i386-vars.fd ("firmware della baseline")
  baseline/manifest.json   hash di base, vars e codice firmware
  run-<n>/overlay.qcow2    qemu-img create -f qcow2 -b ..\baseline\base.qcow2 -F qcow2 overlay.qcow2
  run-<n>/vars.fd          copia di baseline/vars.fd per l'esecuzione
```

Durante tutta la serie lo spike tiene aperti `baseline/base.qcow2` e `baseline/vars.fd` con `OpenOptions::read(true).share_mode(FILE_SHARE_READ)`.

Per ogni ciclo: hash pre-boot di overlay e vars dell'esecuzione; boot dell'ISO con overlay e vars dell'esecuzione per 60 s; `screendump` e `query-block`; arresto con QMP `quit` (nessun dato guest da salvare); hash post-stop di base, vars della baseline, overlay e vars dell'esecuzione; `qemu-img check`; scrittura dimostrativa a VM spenta con `qemu-io -c "write -P 0x5a 0 1M"` e verifica con `qemu-io -r -c "read -P 0x5a 0 1M"`; ripristino = cancellazione di overlay e vars dell'esecuzione e nuova creazione dalla baseline, poi `qemu-io -r -c "read -P 0 0 1M"` sul nuovo overlay e confronto dell'hash del nuovo vars con quello della baseline.

## Risultati

| Voce | Esito | Evidenza |
| --- | --- | --- |
| 6a. Baseline (base 20G + vars) con manifest di hash | verificato | `baseline/manifest.json`; base `221c49a7…ef338`, vars `5d2ac383…be5d1e` |
| 6b. Boot con overlay e vars dell'esecuzione (5 cicli) | verificato | overlay collegato `ro=false` con backing `..\baseline\base.qcow2`; boot fino al Plymouth a 60 s; `quit` in 250–270 ms, 0 residui |
| 6c. Hash di base e vars della baseline invariati | verificato | invariati dopo ognuno dei 5 cicli e a fine serie |
| 6d. Vars dell'esecuzione modificato dal firmware | verificato (fatto osservato) | cambiato in tutti i 5 cicli, 6 839 byte diversi su 540 672, stesso hash risultante `39048a8f…` |
| 6e. Overlay modificato dal guest live | non modificato (fatto osservato) | hash e dimensione (197 120 byte) invariati dopo 60 s di boot |
| 6f. Scrittura nell'overlay a VM spenta e base invariata | verificato | `qemu-io` scrive/rilegge 1 MiB `0x5a`; hash base invariato |
| 6g. Ripristino = nuovo overlay + nuovo vars dalla baseline | verificato | nuovo overlay legge zeri come la base ed è identico byte per byte all'overlay pre-boot; nuovo vars = hash della baseline |
| 7a. Share mode con cui QEMU apre le immagini | verificato (sonda) | base e overlay aperti da QEMU con `FILE_SHARE_WRITE`; spiega il fallimento del lock (`F0-02` 8.3) |
| 7b. Base protetta da handle del supervisor con `FILE_SHARE_READ` | **verificato** | QEMU legge la base; `qemu-io` write, `qemu-img resize`/`snapshot`/`commit`, rinomina e cancellazione rifiutati (errore 32); hash invariato; vale aprendo prima o dopo QEMU |
| 7c. Overlay riservato al solo QEMU tramite share mode | **fallito** (non realizzabile) | share READ prima di QEMU impedisce l'avvio di QEMU; share READ dopo QEMU è rifiutata; share READ\|WRITE lascia scrivere chiunque; si ottiene solo il blocco di rinomina/cancellazione |
| 7d. Rilevazione di uno scrittore preesistente | verificato | apertura del supervisor rifiutata (errore 32) mentre `qemu-io` tiene aperta la base |
| 7e. Vars della baseline protetto | verificato | copia consentita, scrittura rifiutata |
| 8.1 Kill di QEMU durante scrittura del guest nell'overlay | verificato (2 prove riuscite su 3 tentativi) | base invariata in tutti i casi; overlay: `check` pulito in un caso, 110 cluster persi (innocui) nell'altro; 1 tentativo senza scrittura (input inviato prima del desktop) |
| 8.2 Kill di `qemu-img convert` | verificato per la sorgente; file parziale **non riconoscibile** da `qemu-img check` | 5 kill, sorgente invariata, `check` dei parziali "No errors" |
| 9. Journal e riconciliazione | proposta (solo documento) | sezione 9 |

## 6. Ciclo baseline/overlay/firmware

Log: `lab/logs/f0-05-baseline-cycles.log`. Estratto del ciclo 1 (gli altri 4 sono identici salvo i tempi):

```text
baseline: sha256 base=221c49a70cfd327c299bb22fdaeeb3510e9da4acd70618e5c97b052141cef338 vars=5d2ac383371b408398accee7ec27c8c09ea5b74a0de0ceea6513388b15be5d1e
== ciclo 1
  qemu-img create overlay (backing ..\baseline\base.qcow2): (Some(0), "")
  pre-boot: overlay=e30d024c3288c762 vars_esecuzione=5d2ac383371b4083 (uguale alla baseline: true)
  dopo 60s: schermo=Some((1280, 800, 1.57…, 3072016)) disco=Some("ro=false backing=\"..\\baseline\\base.qcow2\"")
  quit: risposta=true uscita=Some(0) in 250 ms
  post-stop: base invariata=true vars_baseline invariato=true overlay cambiato=false (e30d024c3288c762) vars_esecuzione cambiato=true (39048a8f20a1e09e) overlay_byte=197120
  vars: 6839 byte diversi su 540672
  qemu-img check overlay: (Some(0), "No errors were found on the image.  Image end offset: 262144")
  qemu-io write 1 MiB 0x5a nell'overlay: wrote 1048576/1048576 bytes at offset 0
  qemu-io -r read -P 0x5a: read 1048576/1048576 bytes at offset 0
  base invariata dopo la scrittura nell'overlay=true
  ripristino: overlay nuovo legge zeri come la base: read 1048576/1048576 …; vars nuovo = baseline: true
  ripristino: overlay nuovo identico all'overlay pre-boot: true
…
RIEPILOGO: base finale invariata=true vars baseline invariato=true stop (ms): n=5 min=250.6 p50=262.3 p95=270.1 max=270.1 media=260.2
residui_spike=Some(0) qemu_totali_host=0
```

Fatti riportati:

- **Base e vars della baseline:** hash identici prima della serie, dopo ogni ciclo e alla fine; i file erano tenuti aperti dallo spike con `FILE_SHARE_READ` (sezione 7).
- **Vars dell'esecuzione:** OVMF li modifica a ogni primo boot (6 839 byte diversi, presumibilmente variabili di boot create per il CD-ROM; il contenuto non è stato decodificato). L'hash risultante è lo stesso in tutti i 5 cicli, segno di scritture deterministiche per questa configurazione. Lo stato firmware partecipa quindi davvero allo stato persistente e va ripristinato insieme all'overlay, come richiede la sezione 8.
- **Overlay dell'esecuzione:** il live, senza installazione, non scrive sul disco virtio: hash e dimensione invariati. Anche l'apertura in scrittura da parte di QEMU e il `quit` non hanno modificato l'intestazione qcow2. La scrittura è stata dimostrata a VM spenta con `qemu-io`; la scrittura da parte del guest è nella prova 8.1.
- **Ripristino:** cancellare overlay e vars dell'esecuzione e crearne di nuovi dalla baseline riporta a uno stato identico a quello pre-boot (hash dell'overlay nuovo uguale a quello del primo overlay, `e30d024c…`; vars nuovo uguale alla baseline).
- **Stato del boot:** a 60 s lo schermo è al logo di avvio (circa 1,6 % di pixel non neri): il ciclo non arriva al desktop; per questa prova non è necessario.

**Esito: verificato** per 5 cicli (non i 30 del criterio F3).

## 7. Esclusione dei dischi tramite share mode (senza lock di QEMU)

Log: `lab/logs/f0-05-exclusion.log` (seconda esecuzione; la prima, `f0-05-exclusion-run1.log`, ha dato gli stessi esiti). Directory `lab/vms/f0-05/excl/`: `base.qcow2` 20 GiB con 1 MiB scritto a `0x11`, `overlay.qcow2` con backing `base.qcow2`, `run-vars.fd`. QEMU avviato con overlay e vars in pflash, senza ISO (il firmware non trova un disco avviabile e resta in esecuzione), stato verificato con `query-named-block-nodes`.

### 7.1 Controllo senza protezione (T0) e share mode di QEMU

QEMU apre la base come backing con `ro=true` e l'overlay con `ro=false`. Con QEMU vivo e nessun handle dello spike:

| Apertura di prova mentre QEMU è attivo | `base.qcow2` | `overlay.qcow2` |
| --- | --- | --- |
| accesso R, share READ\|WRITE\|DELETE | riuscita | riuscita |
| accesso RW, share READ\|WRITE\|DELETE | **riuscita** | **riuscita** |
| accesso R, share READ | riuscita | errore 32 (file in uso) |
| accesso R, share READ\|WRITE | riuscita | riuscita |

Deduzione dalle prove: QEMU 11.1.0 su Windows apre sia la base (in lettura) sia l'overlay (in lettura/scrittura) con una share mode che **include `FILE_SHARE_WRITE`**; non include `FILE_SHARE_DELETE` (la cancellazione dell'overlay in uso fallisce, 7.2). Questo spiega il fallimento del lock in `F0-02`: qualunque processo dello stesso utente può aprire in scrittura le immagini in uso. Conferma: `qemu-io -c "write -P 0xee 0 64k" base.qcow2` → `wrote 65536/65536`, SHA-256 della base cambiato da `2abdebbf…` a `109dc3c9…`.

### 7.2 Base aperta dal supervisor con `FILE_SHARE_READ` (T1: prima di QEMU; T1b: dopo)

```rust
let guard = OpenOptions::new().read(true).share_mode(FILE_SHARE_READ).open(&base)?;
```

| Operazione con QEMU attivo e handle dello spike aperto | Esito |
| --- | --- |
| Avvio di QEMU con overlay (la base è aperta da QEMU in lettura) | riuscito; nodi `base.qcow2 ro=true`, `overlay.qcow2 ro=false` |
| `qemu-img info base.qcow2` | riuscito |
| `qemu-io -r -c "read -P 0x11 0 64k" base.qcow2` | riuscito |
| `qemu-io -c "write -P 0xee 0 64k" base.qcow2` | **rifiutato**: `Could not open 'base.qcow2': … Il file è utilizzato da un altro processo.` (exit 1) |
| `qemu-img resize base.qcow2 +1G` | **rifiutato** (exit 1, stesso errore) |
| `qemu-img snapshot -c s1 base.qcow2` | **rifiutato** (exit 1) |
| `qemu-img commit overlay.qcow2` (scriverebbe nella base) | **rifiutato**: `Could not reopen 'base.qcow2'` (exit 1) |
| `qemu-img check base.qcow2` | riuscito (sola lettura) |
| Rinomina della base | **rifiutata** (errore 32) |
| Cancellazione della base | **rifiutata** (errore 32) |
| Apertura RW con share READ\|WRITE\|DELETE da un altro handle | **rifiutata** (errore 32) |
| Impostazione dell'attributo sola lettura | **riuscita**: la share mode non protegge gli attributi (`FILE_WRITE_ATTRIBUTES`) |
| QEMU dopo tutti i tentativi | ancora in esecuzione |
| SHA-256 della base prima/dopo T1 | `2abdebbf8d18dcb80ff1ef7905460ad3cb6c82f5ce4adb36032d2fabb61615fa` / identico |

T1b (handle aperto **dopo** l'avvio di QEMU): l'apertura dello spike con share READ riesce, perché QEMU tiene la base solo in lettura; `qemu-io` in scrittura è poi rifiutato allo stesso modo. Entrambe le sequenze funzionano per la base.

### 7.3 Overlay: la share mode non può ammettere "solo QEMU"

| Sequenza | Esito |
| --- | --- |
| T2a: spike apre l'overlay (R, share READ\|WRITE) **prima** di QEMU | QEMU si avvia; `qemu-io` scrive nell'overlay (exit 0), `qemu-img resize overlay +1G` riesce; la cancellazione fallisce (errore 32) |
| T2b: spike apre l'overlay (R, share READ) **prima** di QEMU | **QEMU non si avvia**: exit 1, `-blockdev driver=file,node-name=ovlfile,…: Could not open '…overlay.qcow2': … Il file è utilizzato da un altro processo.` |
| T3a: spike apre l'overlay (R, share READ\|WRITE) **dopo** QEMU | apertura riuscita; `qemu-io` scrive, `qemu-img resize` riesce; cancellazione rifiutata |
| T3b: spike apre l'overlay (R, share READ) **dopo** QEMU | apertura dello spike **rifiutata** (errore 32): QEMU ha già accesso in scrittura |
| Senza handle dello spike (T1) | scrittura e resize dell'overlay in uso riusciti; cancellazione rifiutata dalla share mode di QEMU |

La share mode è una proprietà dell'apertura, non del processo: non può distinguere QEMU da un altro processo. Poiché QEMU ha bisogno di scrivere l'overlay e lo apre a sua volta con `FILE_SHARE_WRITE`, nessuna combinazione di aperture del supervisor (prima o dopo) impedisce a un terzo processo dello stesso utente di scrivere l'overlay in uso. Si ottiene soltanto la protezione da rinomina/cancellazione (assenza di `FILE_SHARE_DELETE`, già garantita dalla stessa apertura di QEMU e, in modo indipendente dalla build QEMU, da un handle del supervisor senza `FILE_SHARE_DELETE`).

### 7.4 Scrittore preesistente (T4) e vars della baseline (T5)

- T4: `qemu-io -f qcow2 base.qcow2` interattivo (base aperta in lettura/scrittura) avviato prima; l'apertura dello spike con share READ **fallisce** con errore 32. Dopo la chiusura di `qemu-io` l'apertura riesce. Il supervisor può quindi usare l'apertura esclusiva come rilevatore: se fallisce, qualcuno ha già la baseline aperta in scrittura e la VM non deve partire. Nota: `qemu-io` con stdin in pipe non ha elaborato `quit`; nella prima esecuzione è stato terminato manualmente (`Stop-Process` sul PID figlio dello spike), nella seconda lo spike lo termina da sé dopo 2 s.
- T5: con `baseline-vars.fd` aperto dallo spike (R, share READ), la copia per l'esecuzione riesce (540 672 byte) e l'apertura in scrittura del vars della baseline è rifiutata (errore 32).

### 7.5 Limiti dell'esclusione

- Vale finché il supervisor è vivo e tiene l'handle; alla sua morte (o chiusura dell'handle) la protezione cessa. Con il Job Object anche QEMU muore (`F0-06`), quindi non resta un QEMU attivo su una base non protetta, ma la base resta scrivibile da altri processi tra un'esecuzione e l'altra se il supervisor non è in esecuzione: serve anche l'ACL/attributo di sola lettura e la verifica dell'hash prima di ogni uso.
- Lo stesso utente può terminare il supervisor, cambiare attributi e ACL dei propri file; l'amministratore può aggirare tutto. È una protezione contro errori e strumenti concorrenti, non un confine di sicurezza (coerente con la sezione 7.1).
- Handle già aperti prima del supervisor: rilevati solo se hanno accesso in scrittura (T4); un lettore preesistente con share senza READ farebbe fallire l'apertura del supervisor in modo analogo (non provato).
- La mappatura in memoria o l'accesso a livello di volume (`\\.\E:`, amministratore) non sono stati provati.
- L'overlay in uso non è protetto dalla scrittura di terzi (7.3).

## 8. Crash sullo storage

### 8.1 Kill di QEMU durante la scrittura del guest nell'overlay

Log: `lab/logs/f0-05-guest-write-crash-1.log`, `-2.log`, `-3.log`; schermate in `lab/vms/f0-05/priv/` (`f0-05-gw-*.ppm`, conversione PNG di quelle del primo tentativo).

Procedura (`f0-storage guest-write-crash`): base 20 GiB con 64 MiB scritti a `0x11` (SHA-256 `1be91d9b…d2d310`), tenuta aperta dallo spike con `FILE_SHARE_READ`; overlay e vars nuovi; boot del live; `send-key` `ctrl-alt-t` via QMP per aprire un terminale; digitazione via `send-key` (layout US) di:

```text
sudo dd if=/dev/urandom of=/dev/vda bs=1M count=8000 oflag=direct status=progress
```

`/dev/vda` è il disco `virtio-blk` collegato all'overlay di prova (nessun disco reale è collegato alla VM). Lo spike controlla ogni 250 ms la dimensione dell'overlay sull'host e, superata la soglia, esegue l'Emergency Stop (`TerminateJobObject`) mentre `dd` è in corso. Poi `qemu-img check`, `qemu-img info`, hash della base e `qemu-io -r -c "read -P 0x11 0 64M"` sulla base.

| Tentativo | Attesa prima dell'input | Esito della scrittura | Stop | `qemu-img check` overlay dopo il kill | Base |
| --- | --- | --- | --- | --- | --- |
| 1 | 170 s (desktop, 99,9 %) | overlay da 197 120 a 327 483 392 byte in 6,3 s; schermata: terminale con `sudo dd …` in esecuzione | 431 ms | exit 0, "No errors", image end offset 327 680; `info`: disk size 47,3 MiB, file length 312 MiB | hash invariato, 64 MiB `0x11` riletti |
| 2 | 170 s (schermo ancora all'avvio, 1,5 %) | **nessuna scrittura**: input inviato prima del desktop e perso; overlay invariato | Emergency Stop a fine attesa | exit 0, "No errors" | hash invariato |
| 3 | 240 s (desktop, 99,9 %) | overlay da 197 120 a 1 132 920 832 byte in 9,6 s | 536 ms | **exit 3**: "110 leaked clusters were found on the image. This means waste of disk space, but no harm to data."; 16 244 cluster allocati | hash invariato, 64 MiB `0x11` riletti |

Fatti: la base non è mai cambiata; l'overlay interrotto resta apribile e coerente per `qemu-img`, ma i dati scritti dopo l'ultimo aggiornamento dei metadati vanno persi (tentativo 1: i cluster scritti non risultano referenziati) oppure restano cluster persi (tentativo 3). Coerente con la sezione 8: dopo un arresto forzato l'overlay non è uno stato pulito e non va promosso a baseline. **Esito: verificato** (2 prove effettive); l'invio di input prima del desktop va evitato o rilevato (il tentativo 2 è riportato come fallito per la parte di scrittura).

### 8.2 Kill di `qemu-img convert`

Log: `lab/logs/f0-05-convert-crash.log`. Sorgente `src.qcow2` da 8 GiB virtuali con 2 GiB scritti (8 blocchi da 256 MiB con pattern `0x21`–`0x28`), 2 148 073 472 byte su disco, SHA-256 `f4044150…7cc3a`. Convert completo di riferimento: 1 902 ms, `qemu-img compare` → `Images are identical.`

Cinque kill (`TerminateProcess` tramite `Child::kill`) a frazioni crescenti del tempo di riferimento, destinazione con suffisso `.partial`:

| # | Kill dopo (ms) | Byte del file parziale | `qemu-img check` | `qemu-img compare` con la sorgente | Sorgente invariata |
| --- | --- | --- | --- | --- | --- |
| 1 | 95 | 67 436 544 | No errors (image end offset 327 680) | Content mismatch at offset 0 | sì |
| 2 | 285 | 304 414 720 | No errors (image end offset 327 680) | Content mismatch at offset 0 | sì |
| 3 | 570 | 598 081 536 | No errors, 6,25 % allocato | mismatch a 536 870 912 | sì |
| 4 | 950 | 981 860 352 | No errors, 6,25 % allocato | mismatch a 536 870 912 | sì |
| 5 | 1 426 | 1 174 863 872 | No errors, 12,50 % allocato | mismatch a 1 073 741 824 | sì |

**Esito: verificato** per la sorgente (hash identico dopo ogni kill). Fatto rilevante: **il file parziale supera `qemu-img check` senza errori** e `qemu-img info` lo descrive come qcow2 valido da 8 GiB con `corrupt: false`; solo `compare` rivela la differenza. Un'immagine interrotta non è quindi riconoscibile dal suo contenuto: va riconosciuta da nome temporaneo (`.partial`) e journal, e promossa con una rinomina atomica solo dopo il completamento verificato (vedi 9).

## 9. Journal e riconciliazione (proposta, non implementata)

Problema (sezione 6.3): operazioni che toccano DB e filesystem (creazione baseline, nuovo overlay, ripristino, eliminazione, materializzazione di checkpoint) non sono atomiche insieme; un crash a metà deve portare a uno stato sicuro senza toccare la baseline.

Principi:

1. **Intento prima dell'effetto.** Ogni operazione scrive nel DB una riga `pending` con tutti i percorsi che creerà, modificherà o cancellerà, prima di toccare il filesystem; il commit SQL dell'intento precede la prima azione su file.
2. **File nuovi solo con nomi temporanei** nella directory gestita della VM (`<op_id>.partial`), creati con `CREATE_NEW` (nessuna sovrascrittura, nessun inseguimento di reparse point), poi `FlushFileBuffers` e rinomina atomica (`MoveFileExW` con `MOVEFILE_REPLACE_EXISTING` solo dove previsto, `MOVEFILE_WRITE_THROUGH`) verso il nome definitivo.
3. **Cancellazioni in due tempi:** rinomina in `trash/<op_id>/…` (atomica sullo stesso volume), commit dello stato `done`, poi rimozione fisica; la riconciliazione completa le rimozioni rimaste.
4. **La baseline non è mai destinazione di scrittura:** l'unico modo di cambiare una baseline è crearne una nuova (sezione 8); l'hash registrato nel manifest è verificato prima di ogni uso e dopo ogni riconciliazione.
5. **Riconciliazione all'avvio del supervisor, prima di accettare comandi**, sotto l'istanza singola del catalogo.

Schema minimo (SQLite):

```sql
CREATE TABLE journal (
  op_id        TEXT PRIMARY KEY,          -- UUID dell'operazione (operation_id)
  vm_id        TEXT NOT NULL,
  run_id       TEXT,                      -- esecuzione a cui si riferisce, se esiste
  kind         TEXT NOT NULL,             -- 'create_baseline' | 'new_overlay' | 'restore' | 'delete_baseline' | 'materialize'
  state        TEXT NOT NULL,             -- 'pending' | 'fs_done' | 'done' | 'rolled_back' | 'failed'
  created_at   TEXT NOT NULL,
  updated_at   TEXT NOT NULL,
  error        TEXT
);
CREATE TABLE journal_file (
  op_id        TEXT NOT NULL REFERENCES journal(op_id),
  role         TEXT NOT NULL,             -- 'create' | 'replace' | 'delete' | 'read_only_input'
  temp_path    TEXT,                      -- percorso .partial o in trash/
  final_path   TEXT NOT NULL,             -- relativo alla radice gestita del catalogo
  expected_sha256 TEXT,                   -- per gli input (baseline, vars) e per i risultati verificati
  PRIMARY KEY (op_id, final_path)
);
```

Regole di riconciliazione:

| Stato trovato | Azione |
| --- | --- |
| `pending`, file temporanei presenti, nessun definitivo | rimuovere i `.partial` (anche se `qemu-img check` li dichiara validi: 8.2), stato `rolled_back` |
| `pending`, definitivo presente e hash/`qemu-img check` coerenti con l'atteso | completare (`done`); altrimenti spostare il definitivo in quarantena e segnalare |
| `fs_done` | aggiornare le righe di catalogo e segnare `done` (idempotente) |
| cancellazione con file in `trash/<op_id>` | completare la rimozione fisica |
| input `read_only_input` con hash diverso dal manifest | bloccare la VM (`state_conflict`), nessuna riparazione automatica della baseline |
| file nella directory gestita non referenziato da catalogo né journal | elencarlo come orfano; rimozione solo dopo conferma o oltre una soglia di età, mai per i file baseline |

Il ripristino (sezione 8) diventa: intento `restore` con `delete` di overlay e vars dell'esecuzione e `create` dei nuovi da `read_only_input` = baseline + vars della baseline; nuovo `run_id`; le richieste con il vecchio `run_id` sono rifiutate. Prima di eseguire, la coda per VM verifica che la VM sia `Stopped` e che l'apertura esclusiva (7.2) della baseline riesca.

## Limiti

- Un solo host, una sola build QEMU, guest live senza installazione: il live non scrive sul disco di sua iniziativa; la scrittura nell'overlay è stata ottenuta con `qemu-io` a VM spenta e, nella prova 8.1, con un comando digitato nel guest.
- 5 cicli, non i 30 richiesti dal criterio di uscita di F3.
- Arresti con QMP `quit` (equivalente a un arresto forzato per il guest): la consistenza di un filesystem guest non è valutata.
- L'esclusione via share mode protegge la baseline solo mentre il supervisor è vivo e contro lo stesso utente non è un confine di sicurezza (7.5); l'overlay in uso resta scrivibile da terzi.
- Prove multiutente e ACL non eseguite (nessun secondo account).
- Dopo le prove sono stati rimossi, per spazio, l'overlay da 1,1 GB della prova 8.1 e i file di `lab/vms/f0-05/cv/`; restano i log con hash, dimensioni e output.

## Azioni richieste

1. Decidere come proteggere l'overlay in uso (vedi Decisioni proposte, punto 3): directory per esecuzione con ACL esplicita creata dal supervisor, oppure un'identità di processo distinta per QEMU (token ristretto/AppContainer), da provare in F1; oppure accettare e dichiarare il limite.
2. Riportare nel backlog F1/F3 (`docs/RELEASE_PLAN.md`): journal (9), apertura esclusiva della baseline, nomi `.partial` con rinomina atomica, verifica hash della baseline prima di ogni avvio, 30 cicli con installazione reale su disco.
3. Nessuna azione di amministratore richiesta da questa prova.

## Decisioni proposte

Per **ADR-0005** (baseline a VM spenta):

1. Una baseline comprende `base.qcow2` **e** la copia del vars OVMF; il ripristino ricrea entrambi (overlay e vars dell'esecuzione) dalla baseline, mai riutilizzando quelli dell'esecuzione precedente.
2. Backing file relativo fisso (`..\baseline\base.qcow2`, formato esplicito `-F qcow2`), verificato con `qemu-img info --backing-chain` prima di ogni avvio; nessuna catena oltre due livelli.
3. Esclusione: il supervisor apre baseline e vars della baseline con `FILE_SHARE_READ` (accesso solo lettura) per tutta la vita del catalogo o almeno per ogni esecuzione e operazione storage; un fallimento dell'apertura blocca l'avvio (`vm_busy`/`state_conflict`). Il lock di QEMU non è usato come garanzia. Per l'overlay: handle del supervisor senza `FILE_SHARE_DELETE` (impedisce rinomina/cancellazione) e directory per esecuzione con ACL ristretta; dichiarare come limite che un processo dello stesso utente può scrivere l'overlay in uso.
4. Le operazioni `qemu-img` sulla baseline (creazione, materializzazione) scrivono su `.partial` e promuovono con rinomina atomica dopo `qemu-img check` **e** confronto (`compare`) o hash, perché `check` da solo non rileva un convert interrotto.
5. Journal e riconciliazione secondo lo schema della sezione 9.

Per **ADR-0017**: il budget storage deve contare overlay, vars per esecuzione (540 672 byte ciascuno), file `.partial` e `trash/` fino alla riconciliazione.
