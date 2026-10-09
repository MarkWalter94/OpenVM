# F0-02 — QEMU reale su Windows: probe WHPX, QMP, display, arresto e base/overlay

Fase F0, passo 2 della sezione 15 di `docs/ROADMAP.md` (QEMU reale su Windows, probe WHPX, QMP e stop), più la parte del passo 4 relativa a base/overlay con `qemu-img`. Le prove sono state eseguite il 9 ottobre 2026 tra le 10:00 e le 10:55 UTC. I log grezzi sono in `lab/logs/` (fuori dal repository); qui sono riportati gli output essenziali.

Esiti ammessi: **verificato**, **fallito**, **non eseguito** (sezione 17). Un esito vale soltanto per l'host, la build e la configurazione indicati.

## Riepilogo degli esiti

| Voce | Esito | Nota sintetica |
| --- | --- | --- |
| 1. Stato virtualizzazione host | verificato (rilevazione) | Hyper-V e VBS/HVCI attivi; `HypervisorPlatform` riportata come *Disabled* ma WHPX funziona comunque (vedi 1.3) |
| 2. Installazione QEMU con provenienza verificata | verificato, con un limite | QEMU 11.1.0, estrazione portabile, SHA-512 della fonte e SHA-256 del manifest winget coincidono; firma Authenticode non valida (certificato scaduto) |
| 3. Capacità del binario | verificato | `whpx` presente, q35 fino a `pc-q35-11.1`, `tls-creds-x509`, `virtio-tablet`, `virtio-vga`, `qemu-io` incluso |
| 4a. Probe WHPX senza media | verificato | `query-accelerators` → `enabled: whpx`; nessun errore WHPX su stderr |
| 4b. QMP su named pipe | verificato con limiti gravi | Funziona; blocca l'avvio fino alla connessione, un solo client, nessuna riconnessione, si blocca in stato `prelaunch` con `-S` |
| 4c. QMP su socket Unix (AF_UNIX) | verificato | Funziona anche con `-S`, riconnessione possibile, OOB annunciato; permessi ereditati dalla directory |
| 4d. `screendump` e `input-send-event` | verificato | PPM valido; eventi assoluti accettati con `virtio-tablet-pci` |
| 5. Nessuna NIC con `-nodefaults` / `-nic none` | verificato | Senza `-nodefaults` QEMU crea `e1000e` + rete `user` |
| 6a. VNC su socket Unix | verificato | Endpoint aperto, banner `RFB 003.008` |
| 6b. VNC loopback con VeNCrypt X.509 e certificato client | verificato con un limite | Con certificato: ServerInit ricevuto; senza certificato: nessun framebuffer, ma connessione lasciata aperta |
| 7. Arresto forzato e orfani (5 cicli) | verificato | 0 processi residui; unico figlio `conhost.exe`, terminato con QEMU; socket Unix orfano dopo kill |
| 8a. Base/overlay qcow2 con hash base invariato | verificato | SHA-256 della base identico prima e dopo scrittura nell'overlay |
| 8b. Lock immagini con QEMU attivo | **fallito** | Su Windows `qemu-img`/`qemu-io` non rilevano l'uso: scrittura e resize riusciti su immagini aperte da QEMU |
| 9a. Boot reale con `-cpu host` (richiesto) | **fallito** | OVMF si blocca prima di inizializzare il display; stesso esito con `-cpu max` |
| 9b. Boot reale con modello CPU nominale | verificato | `-cpu qemu64`: GRUB, kernel, Plymouth e installer grafico Ubuntu raggiunti sotto WHPX |
| 9c. Arresto gentile con `system_powerdown` | fallito nel tempo concesso | Evento `POWERDOWN` emesso, nessun `SHUTDOWN` guest entro 60/120/180 s; chiusura con `quit` |

## Ambiente

| Elemento | Valore |
| --- | --- |
| Host | Windows 11 Pro, build 26200 (`WinHvPlatform.dll` 10.0.26100.9278) |
| CPU | Intel Core i7-12800HX, 16 core / 24 thread |
| RAM | 31,6 GiB |
| Storage | volume E:, oltre 700 GiB liberi |
| Utente | non amministratore; nessuna elevazione tentata |
| Shell | Git Bash, Windows PowerShell 5.1 |
| Client usati | Node 24.15.0 (named pipe, TCP/TLS), .NET SDK 10.0.401 (AF_UNIX), OpenSSL 3.2.1 di Git Bash (certificati di prova) |
| Guest | `ubuntu-26.04.1-desktop-amd64.iso`, SHA-256 `601e30fbf5d97759367c632e2c33630665039b7e2158fd068403da3ccf1bda1f` (verifica GPG e checksum documentata in `F0-03-guest-media.md`; hash ricalcolato qui prima dell'uso: coincide) |

Script di prova scritti per questo passo (in `lab/scripts/`, non eseguono nulla di scaricato): `qmp-client.mjs` (QMP su named pipe), `qmpunix/qmpunix.cs` (QMP e banner su AF_UNIX), `pipe-test.ps1`, `run-probe.ps1`, `kill-cycles.ps1`, `vnc-probe.mjs` (sonda RFB/VeNCrypt, non invia input), `boot-iso.ps1` + `boot-pipe.mjs` (boot reale), `fw-diag.ps1` (diagnostica firmware/CPU), `ppm-stats.mjs`, `ppm2png.mjs`.

## 1. Stato della virtualizzazione

### 1.1 Feature opzionali (CIM, senza elevazione)

Comando: `Get-CimInstance Win32_OptionalFeature -Filter "Name='<nome>'"` (InstallState: 1 = Enabled, 2 = Disabled).

| Feature | InstallState |
| --- | --- |
| `HypervisorPlatform` (Windows Hypervisor Platform) | 2 — Disabled |
| `Microsoft-Hyper-V-All` | 1 — Enabled |
| `Microsoft-Hyper-V` | 1 — Enabled |
| `VirtualMachinePlatform` | 1 — Enabled |
| `Containers-DisposableClientVM` (Windows Sandbox) | 2 — Disabled |
| `Microsoft-Windows-Subsystem-Linux` | 1 — Enabled |

`Get-WindowsOptionalFeature -Online -FeatureName HypervisorPlatform` → errore "è necessaria l'esecuzione con privilegi elevati": lo stato non è confermabile con DISM senza amministratore.

### 1.2 Hypervisor, VBS e servizi

- `Win32_ComputerSystem.HypervisorPresent` = `True`; `Get-ComputerInfo` → `HyperVisorPresent : True`.
- `systeminfo` → "Requisiti Hyper-V: Rilevato hypervisor"; "Sicurezza basata sulla virtualizzazione: Stato: In esecuzione"; servizio configurato ed in esecuzione: "Integrità del codice applicata da hypervisor" (HVCI); App Control for Business policy: imposta.
- `Win32_DeviceGuard`: `VirtualizationBasedSecurityStatus=2` (in esecuzione), `SecurityServicesRunning={2}` (HVCI).
- Servizi/driver: `vmcompute` Running, `vmms` Running, `HvHost` Running, `hvservice` Running, `vid` Running, `hvsocketcontrol` Running, `vmbus` Stopped. Non esiste un servizio chiamato `WinHvPlatform`; la libreria `C:\Windows\System32\WinHvPlatform.dll` (e `WinHvEmulation.dll`) è presente.

### 1.3 Esito

Hyper-V completo è attivo e l'hypervisor è in esecuzione con VBS/HVCI. La feature `HypervisorPlatform` risulta *Disabled* via CIM, eppure QEMU inizializza WHPX e `query-accelerators` restituisce `"enabled": "whpx"` (sezione 4). La causa della discrepanza non è stata determinata: non si deduce che la feature sia irrilevante su altri host. Azione utente nella sezione "Azioni richieste".

## 2. Installazione di QEMU con provenienza verificata

### 2.1 Percorso scelto

- `winget` disponibile. `winget show --id SoftwareFreedomConservancy.QEMU --exact` → versione 11.1.0, `Installer Type: nullsoft`, `Installer Url: https://qemu.weilnetz.de/w64/2026/qemu-w64-setup-20260811.exe`, `Installer SHA256: f98a8aeb5f7faea9765b6dee28316c266cd179d80354a2fed8e50176f9a2e59f`. L'installer NSIS installa per macchina in `Program Files` e richiede elevazione: `winget install` **non** è stato eseguito per non provocare una richiesta UAC.
- Percorso (b): download da `https://qemu.weilnetz.de/w64/` (pagina ufficiale dei build Windows di QEMU). La pagina elenca "2026-08-11 : New QEMU installer (11.1.0)" come ultima release stabile (le precedenti 11.1.0-rcN sono release candidate). La directory è etichettata dal sito come "experimental QEMU for Windows".
- Estrazione portabile con 7-Zip (`C:\Program Files\7-Zip\7z.exe x`), senza eseguire l'installer: "Everything is Ok", 3388 file, 1 256 078 336 byte.

### 2.2 Provenienza

| Campo | Valore |
| --- | --- |
| Versione | QEMU 11.1.0 (`v11.1.0-12130-ge470268ff4`), file `VERSION` = `11.1.0` |
| URL | `https://qemu.weilnetz.de/w64/qemu-w64-setup-20260811.exe` |
| SHA-512 pubblicato (`qemu-w64-setup-20260811.sha512`, stessa fonte) | `5bcf9eed634e8575a37b74f445af41a2fe4106da512d0c30c368301d4c105037fdfab40a5287367a28a957624cddebbc8c07e16c88ab6634f554cdf3d16bf543` |
| SHA-512 calcolato | identico |
| SHA-256 calcolato | `f98a8aeb5f7faea9765b6dee28316c266cd179d80354a2fed8e50176f9a2e59f`, identico al manifest winget (repository indipendente dal sito) |
| Dimensione | 206 615 928 byte |
| Data download | 2026-10-09 |
| Firma Authenticode | **non valida**: firmatario "Universität Mannheim" (CA GEANT Code Signing CA 4), certificato valido dal 12/09/2022 al 12/10/2023; `Get-AuthenticodeSignature` → `UnknownError`, "Un certificato richiesto non rientra nel suo periodo di validità". Stesso esito su `qemu-system-x86_64.exe` estratto |
| Percorso installazione | `lab/qemu/qemu-11.1.0/` (installer conservato in `lab/qemu/download/`) |
| Binario | `lab/qemu/qemu-11.1.0/qemu-system-x86_64.exe`, SHA-256 `47d57a6072e0bb3bd98f87926eb129eb1736dfe818c67b3b81ef7ce4edd0b3cd` |
| `qemu-img.exe` | SHA-256 `9d930582d0d574bb106d89982c547688e10b8f356192d302cc3d0ae4e8674534` |
| `qemu-io.exe` | SHA-256 `2b4bce81239a3982d3e274a1409251a8c67b26876ff07e5ce921017e2d42da9a` |

Limite: la provenienza si basa su HTTPS, sul checksum pubblicato dalla stessa fonte e sulla coincidenza con il manifest winget; non su una firma del codice valida. L'estrazione include anche la directory NSIS `$PLUGINSDIR`, non usata.

### 2.3 Firmware forniti (`share/`)

| File | Byte | SHA-256 |
| --- | --- | --- |
| `edk2-x86_64-code.fd` | 3 653 632 | `33090cc07675baa5190d9f1e84bf5176b33bcbfa9bacac522961150cdb6dbb2a` |
| `edk2-x86_64-secure-code.fd` | 3 653 632 | `32807682a9e5c0e2d192ecb6077941d4be8bcfa6b9cd6e60830c3a9f48c0555a` |
| `edk2-i386-vars.fd` (template NVRAM) | 540 672 | `5d2ac383371b408398accee7ec27c8c09ea5b74a0de0ceea6513388b15be5d1e` |
| `edk2-i386-code.fd` | — | `c8161f6e8464778e13795297e87f5e83cf97c8987480eea1d80742f6c98c18a9` |
| `edk2-i386-secure-code.fd` | — | `2c5e36fc70b391532ceff5bc9b410c6e0fb29b486906e91577f5818368b0ce78` |
| `bios-256k.bin` (SeaBIOS) | — | `ae6f6aa973aaccc143f57aa960fb035fd9de4daee4ad0cd713322f8c259e7650` |

I descrittori `share/firmware/60-edk2-x86_64.json` ("UEFI firmware for x86_64", macchine `pc-i440fx-*`, `pc-q35-*`) e `50-edk2-x86_64-secure.json` ("with Secure Boot and SMM", `requires-smm`, solo `pc-q35-*`) associano entrambi il template `edk2-i386-vars.fd`. Licenze EDK2 in `share/edk2-licenses.txt`. SeaBIOS riportato a schermo: `rel-1.17.0-0-gb52ca86e094d-prebuilt.qemu.org`.

## 3. Capacità del binario

Output completi in `lab/logs/caps/`.

- `--version`: `QEMU emulator version 11.1.0 (v11.1.0-12130-ge470268ff4)`.
- `-accel help`: `tcg`, `whpx`.
- `-machine help` (q35): `q35` alias di `pc-q35-11.1`; disponibili `pc-q35-11.1`, `11.0`, `10.2`, `10.1`, `10.0`, `9.2`, `9.1`, `9.0`, `8.2`; deprecate `8.1` … `5.2`.
- `-device help`: `usb-tablet`, `virtio-tablet-pci` (alias `virtio-tablet`), `virtio-keyboard-pci`, `virtio-mouse-pci`, `virtio-vga`, `virtio-vga-gl`, `virtio-gpu-pci`, `virtio-gpu-gl-pci`, `VGA`, `bochs-display`, `ramfb`.
- `-object help`: `tls-creds-x509`, `tls-creds-psk`, `tls-creds-anon`, `tls-cipher-suites`, `secret` (build con GnuTLS: `libgnutls-30.dll` presente).
- `-vnc help`: opzioni incluse `tls-creds`, `tls-authz`, `sasl`, `sasl-authz`, `password-secret`, `power-control`, `websocket`, `connections`, `share`.
- `-chardev help`: tra gli altri `pipe`, `socket`, `file`, `null`, `ringbuf`, `stdio`, `dbus`.
- `-display help`: `none`, `gtk`, `sdl`, `egl-headless`, `curses`, `spice-app`, `dbus`.
- `-cpu help`: presenti `host` e `max`, oltre ai modelli nominali.
- `qemu-img --version`: 11.1.0. Formati: `blkdebug blklogwrites blkverify bochs cloop compress copy-before-write copy-on-read dmg file ftp ftps host_device http https luks nbd nfs null-aio null-co parallels preallocate qcow qcow2 qed quorum raw replication snapshot-access ssh throttle vdi vhdx vmdk vpc vvfat`. Nota: la build include driver di protocollo remoti (http, ftp, nfs, ssh, nbd): la sezione 6.3 richiede allowlist di protocolli e rappresentazioni strutturate.
- `qemu-io.exe` è incluso (versione 11.1.0).

## 4. Probe WHPX senza media e canale QMP

Base comune: `qemu-system-x86_64 -machine q35 -accel whpx -m 1024 -display none -nodefaults -no-user-config` (più `-S` dove indicato).

### 4.1 Inizializzazione WHPX

Stderr in tutte le esecuzioni WHPX con modello CPU predefinito: soltanto due avvisi, nessun errore WHPX:

```text
warning: host doesn't support requested feature: CPUID[eax=80000001h].ECX.svm [bit 2]
warning: Ignoring request for interrupt vector 0
```

Risposte QMP (socket Unix, `-S`): `query-status` → `{"status": "prelaunch", "running": false}`; `query-kvm` → `{"enabled": false, "present": false}`; `query-accelerators` → `{"enabled": "whpx", "present": ["qtest", "tcg", "whpx"]}`; `query-version` → `{"qemu": {"major": 11, "minor": 1, "micro": 0}, "package": "v11.1.0-12130-ge470268ff4"}`. `query-current-machine` restituisce soltanto `{"wakeup-suspend-support": true}` (nessun nome macchina in questa versione); la macchina è quella richiesta sulla riga di comando. `query-pci` con `-nodefaults`: Host bridge, ISA bridge, SATA controller (AHCI integrato in q35), nessuna NIC. **Esito: verificato**, senza TCG.

### 4.2 QMP su named pipe

Comandi provati: `-qmp pipe:lightvm-f0` e `-chardev pipe,id=qmp0,path=lightvm-f0b -mon chardev=qmp0,mode=control` (QEMU segnala `-mon` deprecato). QEMU crea `\\.\pipe\<nome>`. Client: Node `net.connect('\\\\.\\pipe\\...')` e PowerShell `System.IO.Pipes.NamedPipeClientStream`.

Osservazioni:

1. **Avvio bloccato fino alla connessione.** Stderr vuoto finché nessun client si collega; gli avvisi di inizializzazione compaiono solo dopo la connessione. Il processo resta vivo in attesa.
2. **Greeting senza OOB:** `"capabilities": []` (su AF_UNIX è `["oob"]`).
3. **Blocco in `prelaunch`.** Con `-S` il greeting e `qmp_capabilities` ricevono risposta, ma il comando successivo (`query-status`) non riceve risposta entro 15–25 s (due prove, anche con 500 ms tra i comandi). Senza `-S` (VM in esecuzione) tutti i comandi rispondono: `query-status` → `running`, `query-accelerators` → `whpx`, `quit` → evento `SHUTDOWN` `host-qmp-quit`.
4. **Un solo client.** Con il primo client collegato, un secondo `NamedPipeClientStream.Connect(2000)` fallisce: "Periodo di timeout del semaforo scaduto".
5. **Nessuna riconnessione.** Dopo la disconnessione del primo client un nuovo client non riesce a collegarsi (timeout).
6. **Il primo processo che apre la pipe la occupa.** Un tentativo di `Get-Acl` sul percorso della pipe ha aperto l'istanza: QEMU ha proseguito l'avvio come se un client fosse collegato e il client legittimo successivo non ha più potuto connettersi. L'ACL della pipe creata da QEMU non è stata letta (Get-Acl non supporta il percorso) e QEMU non offre un'opzione per impostarla.

**Esito: verificato con limiti.** Il canale funziona, ma i punti 3–6 lo rendono inadatto come canale QMP privato del supervisor senza un meccanismo diverso (per esempio pipe creata ed ereditata dal supervisor, non provata qui).

### 4.3 QMP su socket Unix (AF_UNIX)

Comando: `-qmp unix:E:/OpenVM/lab/vms/priv/<nome>.sock,server=on,wait=off`. QEMU accetta l'opzione e crea il file socket. Node su Windows non può collegarsi (interpreta il percorso come named pipe: `EACCES`); client .NET con `UnixDomainSocketEndPoint` riuscito.

- Funziona anche con `-S` (`prelaunch`); tutti i comandi rispondono (circa 40–120 ms per l'intera sequenza).
- Riconnessione: un secondo client dopo la disconnessione del primo si collega e riceve un nuovo greeting.
- Il file socket viene rimosso da QEMU dopo `quit`; resta orfano dopo un arresto forzato (sezione 7) o dopo un'uscita per errore di avvio (osservato con `-bios` non caricabile). Un file socket orfano non ha impedito un nuovo avvio sullo stesso percorso (prova con il socket VNC).
- Permessi: il file eredita l'ACL della directory. In `lab/vms/` (ACL ereditata da E:) contiene `Authenticated Users: Modify` e `Users: ReadAndExecute`. In `lab/vms/priv/`, creata con `icacls priv /inheritance:r /grant:r "<utente>:(OI)(CI)F"` senza elevazione, il socket ha soltanto `<utente corrente>: FullControl`. Il rifiuto di un altro utente non è stato provato (nessun secondo account disponibile).

**Esito: verificato.**

### 4.4 `screendump` e input

Configurazione: base + `-nic none -device VGA -device virtio-tablet-pci` (e variante `-device virtio-vga`), VM in esecuzione senza media per 6 s (SeaBIOS).

- `query-mice` → `QEMU Virtio Tablet` (`absolute: true`, `current: false`) e `QEMU PS/2 Mouse` (`current: true`): senza driver guest il tablet non è il dispositivo corrente.
- `screendump` verso `lab/vms/priv/disp-vga.ppm` → `{}`; file P6 720×400, 864 015 byte (coerente con 720×400×3 + intestazione), 1,84 % di pixel non neri; contenuto visivo: "SeaBIOS … Booting from Hard Disk… No bootable device." Stesso risultato con `virtio-vga` (in modalità VGA legacy).
- `input-send-event` con eventi `abs` x/y = 16384 → `{}`; clic `btn left` down/up → `{}`; `send-key` `esc` → `{}`. L'effetto nel guest non è osservabile in questa prova (nessun sistema operativo); è richiesto un guest reale nel passo 3.

**Esito: verificato** (accettazione dei comandi e produzione del file).

## 5. Isolamento di rete predefinito

Verifica via QMP con `human-monitor-command "info network"` e `query-rx-filter`.

| Configurazione | `info network` | `query-rx-filter` |
| --- | --- | --- |
| `-nodefaults` | `""` | `[]` |
| nessun `-nodefaults` | `hub 0` / `hub0port1: #net158: index=0,type=user,net=10.0.2.0,restrict=off` / `hub0port0: e1000e.0: … model=e1000e,macaddr=52:54:00:12:34:56` | `[]` |
| `-nic none` (senza `-nodefaults`) | `""` | `[]` |
| `-nodefaults -nic none` | `""` | `[]` |

Senza `-nodefaults` né `-nic none`, q35 riceve una NIC `e1000e` collegata a rete `user` senza `restrict`. `query-rx-filter` restituisce `[]` anche con la NIC presente, quindi non è un indicatore affidabile di assenza di NIC: l'indicatore usato è `info network` (più `query-pci`). **Esito: verificato.** Proposta: usare entrambe le opzioni `-nodefaults -nic none` e verificare l'assenza di NIC dopo l'avvio.

## 6. VNC e canale display

Base: `-nodefaults -nic none -device VGA`, VM in esecuzione.

### 6.1 VNC su socket Unix

`-vnc unix:E:/OpenVM/lab/vms/priv/vnc.sock`: accettato, file creato, connessione AF_UNIX riuscita, banner `RFB 003.008\n`. ACL del socket: soltanto l'utente corrente (ereditata da `priv/`). Dopo `Stop-Process -Force` il file resta. **Esito: verificato.** Non è stato provato un client RFB completo né l'autenticazione oltre al banner: tipi di sicurezza su questo endpoint non rilevati.

### 6.2 VNC loopback con VeNCrypt X.509 e certificato client

Certificati di prova generati con OpenSSL 3.2.1 in `lab/vms/tls/` (CA RSA 3072, server CN=localhost con SAN IP:127.0.0.1, client CN=lightvm-ui-f0; validità 30 giorni; `openssl verify` → OK). Comando:

```text
-object tls-creds-x509,id=tls0,dir=E:/OpenVM/lab/vms/tls,endpoint=server,verify-peer=on
-vnc 127.0.0.1:0,tls-creds=tls0
```

Ascolto osservato: `127.0.0.1:5900` soltanto. Sonda `vnc-probe.mjs` (handshake RFB 3.8, VeNCrypt 0.2, TLS con Node):

| Client | Risultato |
| --- | --- |
| Con certificato client firmato dalla CA | tipi offerti `[19]` (solo VeNCrypt), sottotipi `[260]` (X509None), TLSv1.3 `TLS_AES_256_GCM_SHA384`, server verificato (`authorized=true`), `SecurityResult: 0`, `ServerInit: 720x400 bpp=32 name="QEMU"` |
| Senza certificato client (CA nota) | handshake TLS completato lato client, poi nessun `SecurityResult` né `ServerInit`; la connessione **non** viene chiusa entro 15 s (timeout della sonda). Ripetuto due volte |
| Senza certificato e senza verifica del server | come sopra |
| Con certificato, dopo un tentativo fallito | `SecurityResult: 0`, ServerInit ricevuto |
| Confronto: `-vnc 127.0.0.1:1` senza TLS | tipi offerti `[1]` (None): nessuna autenticazione |

**Esito: verificato con un limite.** Senza certificato client il framebuffer non viene consegnato, ma la connessione resta aperta: un processo locale può occupare connessioni (rischio di disponibilità; da contenere con `connections=` e timeout lato supervisor, da provare). Il sottotipo offerto è solo X509None: l'autenticazione del client dipende interamente da `verify-peer=on`. La credenziale per esecuzione emessa dal supervisor e la sua revoca non sono state provate.

## 7. Arresto forzato e processi orfani

Script `kill-cycles.ps1`: 5 cicli in sequenza di avvio (`-accel whpx -S -nodefaults -nic none`, QMP AF_UNIX), attesa 2 s, `Stop-Process -Force`, `WaitForExit`, conteggio con `Get-Process` e `Win32_Process` (figli per `ParentProcessId`).

```text
processi qemu-system-x86_64 prima dei cicli: 0
ciclo 1 … figli=1[conhost.exe] figli_vivi_dopo_kill=0 uscita_osservata_ms=69 residui_qemu=0 pid_ancora_presente=0 socket_orfano=True
ciclo 2 … figli=1[conhost.exe] figli_vivi_dopo_kill=0 uscita_osservata_ms=58 residui_qemu=0 pid_ancora_presente=0 socket_orfano=True
ciclo 3 … figli=1[conhost.exe] figli_vivi_dopo_kill=0 uscita_osservata_ms=60 residui_qemu=0 pid_ancora_presente=0 socket_orfano=True
ciclo 4 … figli=1[conhost.exe] figli_vivi_dopo_kill=0 uscita_osservata_ms=68 residui_qemu=0 pid_ancora_presente=0 socket_orfano=True
ciclo 5 … figli=1[conhost.exe] figli_vivi_dopo_kill=0 uscita_osservata_ms=63 residui_qemu=0 pid_ancora_presente=0 socket_orfano=True
processi qemu-system-x86_64 dopo i cicli: 0
```

(`ExitCode` non disponibile tramite l'oggetto restituito da `Start-Process` dopo `Stop-Process`.) Anche dopo tutti i boot reali del punto 9 il conteggio dei processi `qemu-system-x86_64` è tornato a 0.

**Esito: verificato** per il campione di 5 cicli: nessun processo residuo, l'unico figlio è `conhost.exe` (console del processo) e termina con QEMU; il file socket QMP resta e va rimosso dal supervisor. Non provato: Job Object, morte del processo padre, crash del supervisor; il Job Object richiede codice nativo ed è rinviato allo spike Rust del passo 4 (non simulato).

## 8. `qemu-img`: base e overlay

Directory `lab/vms/disk/`.

### 8.1 Creazione e catena

```text
qemu-img create -f qcow2 base.qcow2 20G
  → Formatting 'base.qcow2', fmt=qcow2 cluster_size=65536 … size=21474836480
qemu-img create -f qcow2 -b base.qcow2 -F qcow2 overlay.qcow2
  → … backing_file=base.qcow2 backing_fmt=qcow2 …
```

File sparsi: 197 120 byte ciascuno su disco per 20 GiB virtuali. `qemu-img info --backing-chain --output=json overlay.qcow2`: due elementi, overlay con `"backing-filename": "base.qcow2"`, `"backing-filename-format": "qcow2"`, entrambi `"corrupt": false`, `"dirty-flag": false`. `qemu-img check` su base e overlay: "No errors were found on the image."

### 8.2 Hash della base dopo scrittura nell'overlay

```text
sha256 base prima:  221c49a70cfd327c299bb22fdaeeb3510e9da4acd70618e5c97b052141cef338
qemu-io -f qcow2 -c "write -P 0xab 0 1M" overlay.qcow2 → wrote 1048576/1048576 bytes at offset 0
qemu-io -f qcow2 -c "read -P 0xab 0 1M" overlay.qcow2  → read 1048576/1048576 bytes (pattern verificato)
qemu-io -f qcow2 -c "read -P 0 0 1M" base.qcow2        → read 1048576/1048576 bytes (base ancora a zero)
sha256 base dopo:   221c49a70cfd327c299bb22fdaeeb3510e9da4acd70618e5c97b052141cef338  (sha256sum -c: OK)
qemu-img map overlay: 0–1 MiB depth 0 data=true; resto depth 1 zero=true
```

Ripetuto dopo la ricreazione dei dischi (8.4): SHA-256 della base invariato. **Esito: verificato.** Nota: la lettura di controllo della base è stata fatta con `qemu-io` senza `-r` (apertura in scrittura); l'hash è comunque rimasto identico. Nelle prove future usare `-r`.

### 8.3 Lock con QEMU attivo

QEMU avviato in pausa con `-blockdev driver=file,node-name=f0,filename=…/overlay.qcow2 -blockdev driver=qcow2,node-name=d0,file=f0 -device virtio-blk-pci,drive=d0`; `query-named-block-nodes` conferma la base aperta come backing `"ro": true`. Con QEMU vivo:

| Operazione | Risultato |
| --- | --- |
| `qemu-img info overlay.qcow2` | riuscita, nessun avviso |
| `qemu-img check overlay.qcow2` | "No errors were found" |
| `qemu-io -c "write -P 0xcd 2M 64k" overlay.qcow2` | **scrittura riuscita** |
| `qemu-img resize overlay.qcow2 +1G` | **"Image resized."** |
| `qemu-io -c "write -P 0xee 0 64k" base.qcow2` | **scrittura riuscita**; SHA-256 della base cambiato in `f24dfb6a…` |

**Esito: fallito.** Con questa build su Windows né `qemu-img` né `qemu-io` rifiutano o avvisano quando l'immagine è in uso da QEMU: il lock delle immagini non protegge. Dopo questa prova i dischi sono stati ricreati.

### 8.4 Attributo di sola lettura sulla base (mitigazione provata)

Base ricreata (SHA-256 `221c49a7…`), `IsReadOnly = True` impostato dall'utente senza elevazione. QEMU avviato con l'overlay: si avvia (backing aperto in sola lettura). `qemu-io -c "write …" base.qcow2` → "Could not open 'base.qcow2': Accesso negato." Scrittura nell'overlay in uso ancora riuscita. Hash della base invariato dopo `quit` e dopo una nuova scrittura nell'overlay; `qemu-img check overlay.qcow2` → nessun errore. Limite: l'attributo è rimovibile dallo stesso utente; è una protezione contro errori, non un confine di sicurezza.

## 9. Boot reale dalla ISO verificata

ISO: `lab/media/ubuntu-26.04.1-desktop-amd64.iso`, accompagnata da `ubuntu-26.04.1-desktop-amd64.iso.verified` (checksum `SHA256SUMS` OK, firma GPG valida della chiave `843938DF228D22F7B3742BC0D94AA3F0EFE21092`, prodotti dall'altro agente). SHA-256 ricalcolato prima del boot: `601e30fb…a1f`, coincide.

Configurazione (`boot-iso.ps1`):

```text
qemu-system-x86_64 -name <tag> -machine q35,pflash0=fwcode,pflash1=fwvars -accel whpx
  -cpu <modello> -smp 2 -m 4096 -nodefaults -no-user-config -nic none
  -blockdev driver=file,node-name=fwcode,filename=…/share/edk2-x86_64-code.fd,read-only=on
  -blockdev driver=file,node-name=fwvars,filename=lab/vms/boot/<tag>-vars.fd   (copia di edk2-i386-vars.fd)
  -blockdev driver=file,node-name=isofile,filename=…/ubuntu-26.04.1-desktop-amd64.iso,read-only=on
  -blockdev driver=raw,node-name=iso,file=isofile,read-only=on
  -device ide-cd,drive=iso,bus=ide.0,bootindex=0
  -device virtio-vga -device virtio-tablet-pci -device virtio-keyboard-pci
  -display none -vnc 127.0.0.1:0 -qmp pipe:lightvm-<tag>
```

Il client QMP su named pipe (`boot-pipe.mjs`) resta collegato per tutta la prova (vincolo 4.2), interroga `query-status` ogni 15 s, esegue `screendump` a 90 s, poi `system_powerdown` e, dopo il timeout, `quit`. Stderr salvati in `lab/logs/<tag>.stderr.txt`, trascrizioni in `lab/logs/<tag>.run.txt`, schermate in `lab/vms/boot/`.

### 9.1 `-cpu host` (configurazione richiesta) — fallito

`boot-ubuntu` (virtio-vga): `query-accelerators` → `whpx`; `query-cpus-fast` → 2 vCPU `host-x86_64-cpu`; stato `running` per tutta la prova. `screendump` a 90,9 s: P6 640×480, 0,22 % pixel non neri, testo "Display output is not active." `system_powerdown` → evento `POWERDOWN`, nessun `SHUTDOWN` in 60 s; `quit` → `SHUTDOWN host-qmp-quit`. Durata totale 152 s, 0 processi residui. Stderr: solo `Ignoring request for interrupt vector 0`.

Diagnostica (`boot-ubuntu-stdvga` con `-device VGA`, schermate ogni 15 s per 180 s): tutte identiche, "Guest has not initialized the display (yet)." `fw-diag.ps1` dopo 25 s di esecuzione, `info registers`:

| Variante | RIP vCPU0 | Display dopo 25 s |
| --- | --- | --- |
| pflash, `-cpu host` | `000000007fec0710` (RAX=`fee00900`, RCX=`1b`) | non inizializzato |
| pflash, `-cpu max` | `000000007fec0710` (stessi registri) | non inizializzato |
| pflash, `-cpu host,-x2apic` | `000000007fec0710` | non inizializzato |
| pflash, `-cpu host,pmu=off` | `000000007fecb277` | non inizializzato |
| pflash, `-cpu host,-pdpe1gb,-hle,-rtm` | — | non inizializzato |
| pflash, `-cpu max,-x2apic,-pdpe1gb` | — | non inizializzato |
| pflash, CPU predefinita / `-cpu qemu64` | avanza | GRUB 2.14, menu "Try or Install Ubuntu" (1280×800) |
| pflash, `Skylake-Client-v4`, `Haswell-v4`, `Nehalem` | — | GRUB (1280×800, 3,57 % non neri) |
| SeaBIOS (nessun pflash), `-cpu host` | stato `paused` | stderr `WHPX: Unexpected VP exit code 4` |
| `-bios edk2-x86_64-code.fd` | — | QEMU esce: `could not load PC BIOS` |

Con `-cpu host` e `-cpu max` OVMF resta fermo prima di inizializzare il display; con modelli CPU nominali il firmware arriva a GRUB. La causa non è stata determinata (ipotesi da verificare: funzionalità CPUID esposte da `host`/`max` non gestite da WHPX su questo host con VBS/HVCI). **Esito: fallito** per la configurazione richiesta; nessun ripiego TCG usato.

### 9.2 `-cpu qemu64` — verificato (deviazione documentata)

`boot-ubuntu-qemu64` (virtio-vga, prova da 90 s richiesta):

- `query-accelerators` → `whpx`; vCPU `qemu64-x86_64-cpu` ×2; eventi `RTC_CHANGE` a 1,9 s e 2,9 s.
- Schermate: 15 s e 30 s GRUB (3,57 % non neri); da 45 s a 90 s logo TianoCore + spinner + logo Ubuntu (Plymouth: kernel e userspace avviati), 46–61 colori campionati.
- `screendump` a 92,0 s: `boot-ubuntu-qemu64.ppm`, P6 1280×800, 3 072 016 byte (= 1280×800×3 + 16), 17 992 pixel non neri (1,76 %), SHA-256 `b4571ce8613d5c542a7fbaef1e295b99875cd1e6b88d3d8b12cdedbc8925811a`: non tutto zero.
- `system_powerdown` a 92,0 s → `POWERDOWN`; evento `DEVICE_TRAY_MOVED tray-open:true` a 184,9 s (il guest ha reagito avviando lo spegnimento ed espellendo il supporto); nessun `SHUTDOWN` entro 120 s; `quit` a 212,0 s → `SHUTDOWN host-qmp-quit`. 0 processi residui.

`boot-ubuntu-qemu64-long` (stessa configurazione, 240 s): schermata a 105 s ancora Plymouth; a 120 s desktop live con finestra "Welcome to Ubuntu — Choose your language" (100 % pixel non neri, 789 colori campionati); invariata fino a 240 s. `system_powerdown` a 241,7 s → `POWERDOWN`, nessun `SHUTDOWN` e nessun evento di espulsione entro 180 s; `quit`. Durata totale 423 s, 0 residui.

**Esito: verificato** come boot accelerato WHPX fino all'ambiente grafico, **con modello CPU `qemu64` e non `host`**. Non è un risultato TCG. Limiti: avvio lento (desktop tra 105 e 120 s), modello CPU con poche estensioni; arresto gentile via ACPI non completato nel tempo concesso; nessuna installazione su disco; input nel guest non ancora provato.

## Azioni richieste all'utente

Nessun passaggio di questa verifica è stato bloccato dalla mancanza di elevazione. Azioni consigliate, da eseguire in PowerShell **come amministratore**:

1. Confermare lo stato della feature: `Get-WindowsOptionalFeature -Online -FeatureName HypervisorPlatform`.
2. Se risulta disabilitata, valutare l'abilitazione per allineare l'host al prerequisito documentato di WHPX: `Enable-WindowsOptionalFeature -Online -FeatureName HypervisorPlatform` (richiede riavvio). Dopo l'eventuale abilitazione ripetere 4.1 e 9.1 per verificare se cambia il comportamento di `-cpu host`.
3. Facoltativo, per le prove multiutente dei permessi (4.3, 6.1): disporre di un secondo account locale non amministratore.

## Limiti di questa verifica

- Un solo host, una sola build QEMU (11.1.0 di qemu.weilnetz.de); nessun risultato si estende ad altri host o versioni.
- Discrepanza non spiegata tra `HypervisorPlatform` *Disabled* e WHPX funzionante.
- La firma del codice dei binari QEMU non è valida; la provenienza si basa su checksum e manifest winget.
- Permessi dei socket provati solo come ACL osservata, senza un tentativo di accesso da un secondo utente.
- Il rifiuto VNC senza certificato client non chiude la connessione; comportamento con TLS 1.2 non provato.
- Input via QMP accettato ma effetto nel guest non osservato; nessun client RFB completo.
- Arresto gentile non dimostrato; arresto forzato provato solo con `Stop-Process`, senza Job Object.
- Misure di tempo singole, non campioni statistici (sezione 13 richiede almeno 100 campioni per le latenze).
- Le schermate PNG in `lab/vms/` sono conversioni dei PPM eseguite con `ppm2png.mjs` per ispezione visiva.

## Decisioni proposte per gli ADR (non decise qui)

1. **Build QEMU.** Usare la build Windows di qemu.weilnetz.de fissata per versione e hash (11.1.0, SHA-512 dell'installer sopra), estratta in modo portabile senza installer, con hash dei binari e del firmware registrati nel manifest. Registrare come rischio la firma Authenticode scaduta e valutare una verifica aggiuntiva (manifest winget, rebuild riproducibile o fonte alternativa) prima di F6.
2. **Canale QMP privato.** Preferire AF_UNIX in una directory privata del supervisor con ACL solo-utente (eventualmente con DACL esplicita creata dal supervisor), file rimosso e ricreato per esecuzione. Scartare `-qmp pipe:` creato da QEMU per i limiti di 4.2 (blocco all'avvio, client singolo senza riconnessione, stallo in `prelaunch`, occupazione da parte del primo processo che apre la pipe). Da provare nello spike Rust: socket o pipe creati dal supervisor e passati a QEMU per handle ereditato (`-chardev socket,fd=` o equivalente su Windows), con ACL verificata da un secondo utente.
3. **Trasporto display.** Candidato primario: VNC su AF_UNIX nella stessa directory privata (endpoint e banner verificati). Alternativa: loopback con VeNCrypt X.509 e `verify-peer=on` (verificato), più limite `connections=` e timeout del supervisor per le connessioni non autenticate. Il VNC senza TLS su TCP offre `None` e resta escluso. Ripiego `screendump` già funzionante via QMP.
4. **Firmware.** OVMF `edk2-x86_64-code.fd` in pflash di sola lettura più copia per esecuzione di `edk2-i386-vars.fd` (hash registrati); SeaBIOS non adatto con WHPX su questo host (exit code 4). Secure Boot/SMM non provati.
5. **Modello CPU.** `-cpu host` e `-cpu max` non avviano OVMF su questo host: proporre un modello nominale verificato (candidati `Skylake-Client-v4` o `Haswell-v4`, che arrivano a GRUB; `qemu64` arriva al desktop) e ripetere il boot completo col modello scelto; registrare la deviazione in `COMPATIBILITY.md`.
6. **Profilo di rete.** `-nodefaults -nic none` obbligatori più verifica post-avvio con `info network`/`query-pci`; non usare `query-rx-filter` come prova di assenza di NIC.
7. **Storage.** Il lock di QEMU non protegge le immagini su Windows: il supervisor deve garantire l'esclusione (accesso solo tramite coda per VM, apertura con modalità di condivisione restrittive o lock proprio, base con attributo/ACL di sola lettura) e non deve contare sul rifiuto di `qemu-img`.
