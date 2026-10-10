# F0-02 — Real QEMU on Windows: WHPX probe, QMP, display, shutdown and base/overlay

Phase F0, step 2 of section 15 of `docs/ROADMAP.md` (real QEMU on Windows, WHPX probe, QMP and stop), plus the part of step 4 concerning base/overlay with `qemu-img`. The tests were run on 9 October 2026 between 10:00 and 10:55 UTC. Raw logs are in `lab/logs/` (outside the repository); the essential outputs are reported here.

Allowed outcomes: **verified**, **failed**, **not executed** (section 17). An outcome holds only for the host, build and configuration indicated.

## Summary of outcomes

| Item | Outcome | Short note |
| --- | --- | --- |
| 1. Host virtualization state | verified (detection) | Hyper-V and VBS/HVCI active; `HypervisorPlatform` reported as *Disabled* but WHPX works anyway (see 1.3) |
| 2. QEMU installation with verified provenance | verified, with a limit | QEMU 11.1.0, portable extraction, source SHA-512 and winget manifest SHA-256 match; Authenticode signature not valid (expired certificate) |
| 3. Binary capabilities | verified | `whpx` present, q35 up to `pc-q35-11.1`, `tls-creds-x509`, `virtio-tablet`, `virtio-vga`, `qemu-io` included |
| 4a. WHPX probe without media | verified | `query-accelerators` → `enabled: whpx`; no WHPX error on stderr |
| 4b. QMP over named pipe | verified with serious limits | Works; blocks startup until connection, a single client, no reconnection, hangs in `prelaunch` state with `-S` |
| 4c. QMP over Unix socket (AF_UNIX) | verified | Works also with `-S`, reconnection possible, OOB advertised; permissions inherited from the directory |
| 4d. `screendump` and `input-send-event` | verified | Valid PPM; absolute events accepted with `virtio-tablet-pci` |
| 5. No NIC with `-nodefaults` / `-nic none` | verified | Without `-nodefaults` QEMU creates `e1000e` + `user` network |
| 6a. VNC over Unix socket | verified | Endpoint open, banner `RFB 003.008` |
| 6b. Loopback VNC with VeNCrypt X.509 and client certificate | verified with a limit | With certificate: ServerInit received; without certificate: no framebuffer, but connection left open |
| 7. Forced stop and orphans (5 cycles) | verified | 0 leftover processes; only child `conhost.exe`, terminated with QEMU; Unix socket orphaned after kill |
| 8a. qcow2 base/overlay with base hash unchanged | verified | Base SHA-256 identical before and after writing to the overlay |
| 8b. Image locking with QEMU running | **failed** | On Windows `qemu-img`/`qemu-io` do not detect the use: write and resize succeeded on images opened by QEMU |
| 9a. Real boot with `-cpu host` (required) | **failed** | OVMF hangs before initializing the display; same outcome with `-cpu max` |
| 9b. Real boot with named CPU model | verified | `-cpu qemu64`: GRUB, kernel, Plymouth and Ubuntu graphical installer reached under WHPX |
| 9c. Graceful shutdown with `system_powerdown` | failed within the allotted time | `POWERDOWN` event emitted, no guest `SHUTDOWN` within 60/120/180 s; closed with `quit` |

## Environment

| Element | Value |
| --- | --- |
| Host | Windows 11 Pro, build 26200 (`WinHvPlatform.dll` 10.0.26100.9278) |
| CPU | Intel Core i7-12800HX, 16 cores / 24 threads |
| RAM | 31.6 GiB |
| Storage | volume E:, over 700 GiB free |
| User | non-administrator; no elevation attempted |
| Shell | Git Bash, Windows PowerShell 5.1 |
| Clients used | Node 24.15.0 (named pipe, TCP/TLS), .NET SDK 10.0.401 (AF_UNIX), OpenSSL 3.2.1 from Git Bash (test certificates) |
| Guest | `ubuntu-26.04.1-desktop-amd64.iso`, SHA-256 `601e30fbf5d97759367c632e2c33630665039b7e2158fd068403da3ccf1bda1f` (GPG and checksum verification documented in `F0-03-guest-media.md`; hash recomputed here before use: matches) |

Test scripts written for this step (in `lab/scripts/`, they do not run anything downloaded): `qmp-client.mjs` (QMP over named pipe), `qmpunix/qmpunix.cs` (QMP and banner over AF_UNIX), `pipe-test.ps1`, `run-probe.ps1`, `kill-cycles.ps1`, `vnc-probe.mjs` (RFB/VeNCrypt probe, sends no input), `boot-iso.ps1` + `boot-pipe.mjs` (real boot), `fw-diag.ps1` (firmware/CPU diagnostics), `ppm-stats.mjs`, `ppm2png.mjs`.

## 1. Virtualization state

### 1.1 Optional features (CIM, without elevation)

Command: `Get-CimInstance Win32_OptionalFeature -Filter "Name='<name>'"` (InstallState: 1 = Enabled, 2 = Disabled).

| Feature | InstallState |
| --- | --- |
| `HypervisorPlatform` (Windows Hypervisor Platform) | 2 — Disabled |
| `Microsoft-Hyper-V-All` | 1 — Enabled |
| `Microsoft-Hyper-V` | 1 — Enabled |
| `VirtualMachinePlatform` | 1 — Enabled |
| `Containers-DisposableClientVM` (Windows Sandbox) | 2 — Disabled |
| `Microsoft-Windows-Subsystem-Linux` | 1 — Enabled |

`Get-WindowsOptionalFeature -Online -FeatureName HypervisorPlatform` → error "è necessaria l'esecuzione con privilegi elevati": the state cannot be confirmed with DISM without administrator rights.

### 1.2 Hypervisor, VBS and services

- `Win32_ComputerSystem.HypervisorPresent` = `True`; `Get-ComputerInfo` → `HyperVisorPresent : True`.
- `systeminfo` → "Requisiti Hyper-V: Rilevato hypervisor"; "Sicurezza basata sulla virtualizzazione: Stato: In esecuzione"; service configured and running: "Integrità del codice applicata da hypervisor" (HVCI); App Control for Business policy: set.
- `Win32_DeviceGuard`: `VirtualizationBasedSecurityStatus=2` (running), `SecurityServicesRunning={2}` (HVCI).
- Services/drivers: `vmcompute` Running, `vmms` Running, `HvHost` Running, `hvservice` Running, `vid` Running, `hvsocketcontrol` Running, `vmbus` Stopped. There is no service called `WinHvPlatform`; the library `C:\Windows\System32\WinHvPlatform.dll` (and `WinHvEmulation.dll`) is present.

### 1.3 Outcome

Full Hyper-V is active and the hypervisor is running with VBS/HVCI. The `HypervisorPlatform` feature is reported as *Disabled* via CIM, yet QEMU initializes WHPX and `query-accelerators` returns `"enabled": "whpx"` (section 4). The cause of the discrepancy was not determined: it is not inferred that the feature is irrelevant on other hosts. User action in the "Required actions" section.

## 2. QEMU installation with verified provenance

### 2.1 Chosen path

- `winget` available. `winget show --id SoftwareFreedomConservancy.QEMU --exact` → version 11.1.0, `Installer Type: nullsoft`, `Installer Url: https://qemu.weilnetz.de/w64/2026/qemu-w64-setup-20260811.exe`, `Installer SHA256: f98a8aeb5f7faea9765b6dee28316c266cd179d80354a2fed8e50176f9a2e59f`. The NSIS installer installs per machine in `Program Files` and requires elevation: `winget install` was **not** run so as not to trigger a UAC prompt.
- Path (b): download from `https://qemu.weilnetz.de/w64/` (official page of the QEMU Windows builds). The page lists "2026-08-11 : New QEMU installer (11.1.0)" as the latest stable release (the previous 11.1.0-rcN are release candidates). The directory is labelled by the site as "experimental QEMU for Windows".
- Portable extraction with 7-Zip (`C:\Program Files\7-Zip\7z.exe x`), without running the installer: "Everything is Ok", 3388 files, 1 256 078 336 bytes.

### 2.2 Provenance

| Field | Value |
| --- | --- |
| Version | QEMU 11.1.0 (`v11.1.0-12130-ge470268ff4`), file `VERSION` = `11.1.0` |
| URL | `https://qemu.weilnetz.de/w64/qemu-w64-setup-20260811.exe` |
| Published SHA-512 (`qemu-w64-setup-20260811.sha512`, same source) | `5bcf9eed634e8575a37b74f445af41a2fe4106da512d0c30c368301d4c105037fdfab40a5287367a28a957624cddebbc8c07e16c88ab6634f554cdf3d16bf543` |
| Computed SHA-512 | identical |
| Computed SHA-256 | `f98a8aeb5f7faea9765b6dee28316c266cd179d80354a2fed8e50176f9a2e59f`, identical to the winget manifest (repository independent of the site) |
| Size | 206 615 928 bytes |
| Download date | 2026-10-09 |
| Authenticode signature | **not valid**: signer "Universität Mannheim" (CA GEANT Code Signing CA 4), certificate valid from 12/09/2022 to 12/10/2023; `Get-AuthenticodeSignature` → `UnknownError`, "Un certificato richiesto non rientra nel suo periodo di validità". Same outcome on the extracted `qemu-system-x86_64.exe` |
| Installation path | `lab/qemu/qemu-11.1.0/` (installer kept in `lab/qemu/download/`) |
| Binary | `lab/qemu/qemu-11.1.0/qemu-system-x86_64.exe`, SHA-256 `47d57a6072e0bb3bd98f87926eb129eb1736dfe818c67b3b81ef7ce4edd0b3cd` |
| `qemu-img.exe` | SHA-256 `9d930582d0d574bb106d89982c547688e10b8f356192d302cc3d0ae4e8674534` |
| `qemu-io.exe` | SHA-256 `2b4bce81239a3982d3e274a1409251a8c67b26876ff07e5ce921017e2d42da9a` |

Limit: provenance relies on HTTPS, on the checksum published by the same source and on the match with the winget manifest; not on a valid code signature. The extraction also includes the NSIS directory `$PLUGINSDIR`, not used.

### 2.3 Bundled firmware (`share/`)

| File | Bytes | SHA-256 |
| --- | --- | --- |
| `edk2-x86_64-code.fd` | 3 653 632 | `33090cc07675baa5190d9f1e84bf5176b33bcbfa9bacac522961150cdb6dbb2a` |
| `edk2-x86_64-secure-code.fd` | 3 653 632 | `32807682a9e5c0e2d192ecb6077941d4be8bcfa6b9cd6e60830c3a9f48c0555a` |
| `edk2-i386-vars.fd` (NVRAM template) | 540 672 | `5d2ac383371b408398accee7ec27c8c09ea5b74a0de0ceea6513388b15be5d1e` |
| `edk2-i386-code.fd` | — | `c8161f6e8464778e13795297e87f5e83cf97c8987480eea1d80742f6c98c18a9` |
| `edk2-i386-secure-code.fd` | — | `2c5e36fc70b391532ceff5bc9b410c6e0fb29b486906e91577f5818368b0ce78` |
| `bios-256k.bin` (SeaBIOS) | — | `ae6f6aa973aaccc143f57aa960fb035fd9de4daee4ad0cd713322f8c259e7650` |

The descriptors `share/firmware/60-edk2-x86_64.json` ("UEFI firmware for x86_64", machines `pc-i440fx-*`, `pc-q35-*`) and `50-edk2-x86_64-secure.json` ("with Secure Boot and SMM", `requires-smm`, only `pc-q35-*`) both associate the `edk2-i386-vars.fd` template. EDK2 licences in `share/edk2-licenses.txt`. SeaBIOS shown on screen: `rel-1.17.0-0-gb52ca86e094d-prebuilt.qemu.org`.

## 3. Binary capabilities

Full outputs in `lab/logs/caps/`.

- `--version`: `QEMU emulator version 11.1.0 (v11.1.0-12130-ge470268ff4)`.
- `-accel help`: `tcg`, `whpx`.
- `-machine help` (q35): `q35` alias of `pc-q35-11.1`; available `pc-q35-11.1`, `11.0`, `10.2`, `10.1`, `10.0`, `9.2`, `9.1`, `9.0`, `8.2`; deprecated `8.1` … `5.2`.
- `-device help`: `usb-tablet`, `virtio-tablet-pci` (alias `virtio-tablet`), `virtio-keyboard-pci`, `virtio-mouse-pci`, `virtio-vga`, `virtio-vga-gl`, `virtio-gpu-pci`, `virtio-gpu-gl-pci`, `VGA`, `bochs-display`, `ramfb`.
- `-object help`: `tls-creds-x509`, `tls-creds-psk`, `tls-creds-anon`, `tls-cipher-suites`, `secret` (build with GnuTLS: `libgnutls-30.dll` present).
- `-vnc help`: options include `tls-creds`, `tls-authz`, `sasl`, `sasl-authz`, `password-secret`, `power-control`, `websocket`, `connections`, `share`.
- `-chardev help`: among others `pipe`, `socket`, `file`, `null`, `ringbuf`, `stdio`, `dbus`.
- `-display help`: `none`, `gtk`, `sdl`, `egl-headless`, `curses`, `spice-app`, `dbus`.
- `-cpu help`: `host` and `max` present, in addition to the named models.
- `qemu-img --version`: 11.1.0. Formats: `blkdebug blklogwrites blkverify bochs cloop compress copy-before-write copy-on-read dmg file ftp ftps host_device http https luks nbd nfs null-aio null-co parallels preallocate qcow qcow2 qed quorum raw replication snapshot-access ssh throttle vdi vhdx vmdk vpc vvfat`. Note: the build includes remote protocol drivers (http, ftp, nfs, ssh, nbd): section 6.3 requires a protocol allowlist and structured representations.
- `qemu-io.exe` is included (version 11.1.0).

## 4. WHPX probe without media and QMP channel

Common base: `qemu-system-x86_64 -machine q35 -accel whpx -m 1024 -display none -nodefaults -no-user-config` (plus `-S` where indicated).

### 4.1 WHPX initialization

Stderr in all WHPX runs with the default CPU model: only two warnings, no WHPX errors:

```text
warning: host doesn't support requested feature: CPUID[eax=80000001h].ECX.svm [bit 2]
warning: Ignoring request for interrupt vector 0
```

QMP responses (Unix socket, `-S`): `query-status` → `{"status": "prelaunch", "running": false}`; `query-kvm` → `{"enabled": false, "present": false}`; `query-accelerators` → `{"enabled": "whpx", "present": ["qtest", "tcg", "whpx"]}`; `query-version` → `{"qemu": {"major": 11, "minor": 1, "micro": 0}, "package": "v11.1.0-12130-ge470268ff4"}`. `query-current-machine` returns only `{"wakeup-suspend-support": true}` (no machine name in this version); the machine is the one requested on the command line. `query-pci` with `-nodefaults`: Host bridge, ISA bridge, SATA controller (AHCI integrated in q35), no NIC. **Outcome: verified**, without TCG.

### 4.2 QMP over named pipe

Commands tried: `-qmp pipe:lightvm-f0` and `-chardev pipe,id=qmp0,path=lightvm-f0b -mon chardev=qmp0,mode=control` (QEMU reports `-mon` as deprecated). QEMU creates `\\.\pipe\<name>`. Clients: Node `net.connect('\\\\.\\pipe\\...')` and PowerShell `System.IO.Pipes.NamedPipeClientStream`.

Observations:

1. **Startup blocked until connection.** Stderr empty until a client connects; the initialization warnings appear only after the connection. The process stays alive, waiting.
2. **Greeting without OOB:** `"capabilities": []` (on AF_UNIX it is `["oob"]`).
3. **Hang in `prelaunch`.** With `-S` the greeting and `qmp_capabilities` get a response, but the next command (`query-status`) gets no response within 15–25 s (two attempts, also with 500 ms between commands). Without `-S` (VM running) all commands respond: `query-status` → `running`, `query-accelerators` → `whpx`, `quit` → `SHUTDOWN` event `host-qmp-quit`.
4. **A single client.** With the first client connected, a second `NamedPipeClientStream.Connect(2000)` fails: "Periodo di timeout del semaforo scaduto".
5. **No reconnection.** After the first client disconnects, a new client cannot connect (timeout).
6. **The first process that opens the pipe takes it.** An attempt at `Get-Acl` on the pipe path opened the instance: QEMU continued startup as if a client had connected and the subsequent legitimate client could no longer connect. The ACL of the pipe created by QEMU was not read (Get-Acl does not support the path) and QEMU offers no option to set it.

**Outcome: verified with limits.** The channel works, but points 3–6 make it unsuitable as the supervisor's private QMP channel without a different mechanism (for example a pipe created and inherited by the supervisor, not tested here).

### 4.3 QMP over Unix socket (AF_UNIX)

Command: `-qmp unix:E:/OpenVM/lab/vms/priv/<name>.sock,server=on,wait=off`. QEMU accepts the option and creates the socket file. Node on Windows cannot connect (it interprets the path as a named pipe: `EACCES`); .NET client with `UnixDomainSocketEndPoint` succeeded.

- Works also with `-S` (`prelaunch`); all commands respond (about 40–120 ms for the whole sequence).
- Reconnection: a second client after the first one disconnects connects and receives a new greeting.
- The socket file is removed by QEMU after `quit`; it stays orphaned after a forced stop (section 7) or after an exit due to a startup error (observed with a `-bios` that could not be loaded). An orphaned socket file did not prevent a new start on the same path (test with the VNC socket).
- Permissions: the file inherits the directory ACL. In `lab/vms/` (ACL inherited from E:) it contains `Authenticated Users: Modify` and `Users: ReadAndExecute`. In `lab/vms/priv/`, created with `icacls priv /inheritance:r /grant:r "<user>:(OI)(CI)F"` without elevation, the socket has only `<current user>: FullControl`. Rejection of another user was not tested (no second account available).

**Outcome: verified.**

### 4.4 `screendump` and input

Configuration: base + `-nic none -device VGA -device virtio-tablet-pci` (and a `-device virtio-vga` variant), VM running without media for 6 s (SeaBIOS).

- `query-mice` → `QEMU Virtio Tablet` (`absolute: true`, `current: false`) and `QEMU PS/2 Mouse` (`current: true`): without a guest driver the tablet is not the current device.
- `screendump` to `lab/vms/priv/disp-vga.ppm` → `{}`; P6 file 720×400, 864 015 bytes (consistent with 720×400×3 + header), 1.84 % non-black pixels; visual content: "SeaBIOS … Booting from Hard Disk… No bootable device." Same result with `virtio-vga` (in legacy VGA mode).
- `input-send-event` with `abs` events x/y = 16384 → `{}`; click `btn left` down/up → `{}`; `send-key` `esc` → `{}`. The effect in the guest is not observable in this test (no operating system); a real guest is required in step 3.

**Outcome: verified** (acceptance of the commands and production of the file).

## 5. Default network isolation

Checked via QMP with `human-monitor-command "info network"` and `query-rx-filter`.

| Configuration | `info network` | `query-rx-filter` |
| --- | --- | --- |
| `-nodefaults` | `""` | `[]` |
| no `-nodefaults` | `hub 0` / `hub0port1: #net158: index=0,type=user,net=10.0.2.0,restrict=off` / `hub0port0: e1000e.0: … model=e1000e,macaddr=52:54:00:12:34:56` | `[]` |
| `-nic none` (without `-nodefaults`) | `""` | `[]` |
| `-nodefaults -nic none` | `""` | `[]` |

Without `-nodefaults` or `-nic none`, q35 gets an `e1000e` NIC connected to a `user` network without `restrict`. `query-rx-filter` returns `[]` even with the NIC present, so it is not a reliable indicator of the absence of a NIC: the indicator used is `info network` (plus `query-pci`). **Outcome: verified.** Proposal: use both options `-nodefaults -nic none` and check the absence of NICs after startup.

## 6. VNC and display channel

Base: `-nodefaults -nic none -device VGA`, VM running.

### 6.1 VNC over Unix socket

`-vnc unix:E:/OpenVM/lab/vms/priv/vnc.sock`: accepted, file created, AF_UNIX connection succeeded, banner `RFB 003.008\n`. Socket ACL: only the current user (inherited from `priv/`). After `Stop-Process -Force` the file remains. **Outcome: verified.** A full RFB client and authentication beyond the banner were not tested: security types on this endpoint not detected.

### 6.2 Loopback VNC with VeNCrypt X.509 and client certificate

Test certificates generated with OpenSSL 3.2.1 in `lab/vms/tls/` (RSA 3072 CA, server CN=localhost with SAN IP:127.0.0.1, client CN=lightvm-ui-f0; validity 30 days; `openssl verify` → OK). Command:

```text
-object tls-creds-x509,id=tls0,dir=E:/OpenVM/lab/vms/tls,endpoint=server,verify-peer=on
-vnc 127.0.0.1:0,tls-creds=tls0
```

Listening observed: `127.0.0.1:5900` only. Probe `vnc-probe.mjs` (RFB 3.8 handshake, VeNCrypt 0.2, TLS with Node):

| Client | Result |
| --- | --- |
| With client certificate signed by the CA | types offered `[19]` (VeNCrypt only), subtypes `[260]` (X509None), TLSv1.3 `TLS_AES_256_GCM_SHA384`, server verified (`authorized=true`), `SecurityResult: 0`, `ServerInit: 720x400 bpp=32 name="QEMU"` |
| Without client certificate (known CA) | TLS handshake completed on the client side, then no `SecurityResult` or `ServerInit`; the connection is **not** closed within 15 s (probe timeout). Repeated twice |
| Without certificate and without server verification | as above |
| With certificate, after a failed attempt | `SecurityResult: 0`, ServerInit received |
| Comparison: `-vnc 127.0.0.1:1` without TLS | types offered `[1]` (None): no authentication |

**Outcome: verified with a limit.** Without a client certificate the framebuffer is not delivered, but the connection stays open: a local process can occupy connections (availability risk; to be contained with `connections=` and timeouts on the supervisor side, to be tested). The only subtype offered is X509None: client authentication depends entirely on `verify-peer=on`. The per-run credential issued by the supervisor and its revocation were not tested.

## 7. Forced stop and orphan processes

Script `kill-cycles.ps1`: 5 cycles in sequence of start (`-accel whpx -S -nodefaults -nic none`, QMP AF_UNIX), 2 s wait, `Stop-Process -Force`, `WaitForExit`, count with `Get-Process` and `Win32_Process` (children by `ParentProcessId`).

```text
processi qemu-system-x86_64 prima dei cicli: 0
ciclo 1 … figli=1[conhost.exe] figli_vivi_dopo_kill=0 uscita_osservata_ms=69 residui_qemu=0 pid_ancora_presente=0 socket_orfano=True
ciclo 2 … figli=1[conhost.exe] figli_vivi_dopo_kill=0 uscita_osservata_ms=58 residui_qemu=0 pid_ancora_presente=0 socket_orfano=True
ciclo 3 … figli=1[conhost.exe] figli_vivi_dopo_kill=0 uscita_osservata_ms=60 residui_qemu=0 pid_ancora_presente=0 socket_orfano=True
ciclo 4 … figli=1[conhost.exe] figli_vivi_dopo_kill=0 uscita_osservata_ms=68 residui_qemu=0 pid_ancora_presente=0 socket_orfano=True
ciclo 5 … figli=1[conhost.exe] figli_vivi_dopo_kill=0 uscita_osservata_ms=63 residui_qemu=0 pid_ancora_presente=0 socket_orfano=True
processi qemu-system-x86_64 dopo i cicli: 0
```

(`ExitCode` not available through the object returned by `Start-Process` after `Stop-Process`.) Also after all the real boots of point 9 the count of `qemu-system-x86_64` processes returned to 0.

**Outcome: verified** for the sample of 5 cycles: no leftover process, the only child is `conhost.exe` (the process console) and it terminates with QEMU; the QMP socket file remains and must be removed by the supervisor. Not tested: Job Object, death of the parent process, supervisor crash; the Job Object requires native code and is deferred to the Rust spike of step 4 (not simulated).

## 8. `qemu-img`: base and overlay

Directory `lab/vms/disk/`.

### 8.1 Creation and chain

```text
qemu-img create -f qcow2 base.qcow2 20G
  → Formatting 'base.qcow2', fmt=qcow2 cluster_size=65536 … size=21474836480
qemu-img create -f qcow2 -b base.qcow2 -F qcow2 overlay.qcow2
  → … backing_file=base.qcow2 backing_fmt=qcow2 …
```

Sparse files: 197 120 bytes each on disk for 20 GiB virtual. `qemu-img info --backing-chain --output=json overlay.qcow2`: two elements, overlay with `"backing-filename": "base.qcow2"`, `"backing-filename-format": "qcow2"`, both `"corrupt": false`, `"dirty-flag": false`. `qemu-img check` on base and overlay: "No errors were found on the image."

### 8.2 Base hash after writing to the overlay

```text
sha256 base prima:  221c49a70cfd327c299bb22fdaeeb3510e9da4acd70618e5c97b052141cef338
qemu-io -f qcow2 -c "write -P 0xab 0 1M" overlay.qcow2 → wrote 1048576/1048576 bytes at offset 0
qemu-io -f qcow2 -c "read -P 0xab 0 1M" overlay.qcow2  → read 1048576/1048576 bytes (pattern verificato)
qemu-io -f qcow2 -c "read -P 0 0 1M" base.qcow2        → read 1048576/1048576 bytes (base ancora a zero)
sha256 base dopo:   221c49a70cfd327c299bb22fdaeeb3510e9da4acd70618e5c97b052141cef338  (sha256sum -c: OK)
qemu-img map overlay: 0–1 MiB depth 0 data=true; resto depth 1 zero=true
```

Repeated after recreating the disks (8.4): base SHA-256 unchanged. **Outcome: verified.** Note: the check read of the base was done with `qemu-io` without `-r` (opened for writing); the hash nevertheless stayed identical. In future tests use `-r`.

### 8.3 Locking with QEMU running

QEMU started paused with `-blockdev driver=file,node-name=f0,filename=…/overlay.qcow2 -blockdev driver=qcow2,node-name=d0,file=f0 -device virtio-blk-pci,drive=d0`; `query-named-block-nodes` confirms the base opened as backing `"ro": true`. With QEMU alive:

| Operation | Result |
| --- | --- |
| `qemu-img info overlay.qcow2` | succeeded, no warning |
| `qemu-img check overlay.qcow2` | "No errors were found" |
| `qemu-io -c "write -P 0xcd 2M 64k" overlay.qcow2` | **write succeeded** |
| `qemu-img resize overlay.qcow2 +1G` | **"Image resized."** |
| `qemu-io -c "write -P 0xee 0 64k" base.qcow2` | **write succeeded**; base SHA-256 changed to `f24dfb6a…` |

**Outcome: failed.** With this build on Windows neither `qemu-img` nor `qemu-io` refuse or warn when the image is in use by QEMU: image locking does not protect. After this test the disks were recreated.

### 8.4 Read-only attribute on the base (mitigation tested)

Base recreated (SHA-256 `221c49a7…`), `IsReadOnly = True` set by the user without elevation. QEMU started with the overlay: it starts (backing opened read-only). `qemu-io -c "write …" base.qcow2` → "Could not open 'base.qcow2': Accesso negato." Writing to the overlay in use still succeeded. Base hash unchanged after `quit` and after a new write to the overlay; `qemu-img check overlay.qcow2` → no errors. Limit: the attribute can be removed by the same user; it is a protection against mistakes, not a security boundary.

## 9. Real boot from the verified ISO

ISO: `lab/media/ubuntu-26.04.1-desktop-amd64.iso`, accompanied by `ubuntu-26.04.1-desktop-amd64.iso.verified` (`SHA256SUMS` checksum OK, valid GPG signature from key `843938DF228D22F7B3742BC0D94AA3F0EFE21092`, produced by the other agent). SHA-256 recomputed before boot: `601e30fb…a1f`, matches.

Configuration (`boot-iso.ps1`):

```text
qemu-system-x86_64 -name <tag> -machine q35,pflash0=fwcode,pflash1=fwvars -accel whpx
  -cpu <model> -smp 2 -m 4096 -nodefaults -no-user-config -nic none
  -blockdev driver=file,node-name=fwcode,filename=…/share/edk2-x86_64-code.fd,read-only=on
  -blockdev driver=file,node-name=fwvars,filename=lab/vms/boot/<tag>-vars.fd   (copy of edk2-i386-vars.fd)
  -blockdev driver=file,node-name=isofile,filename=…/ubuntu-26.04.1-desktop-amd64.iso,read-only=on
  -blockdev driver=raw,node-name=iso,file=isofile,read-only=on
  -device ide-cd,drive=iso,bus=ide.0,bootindex=0
  -device virtio-vga -device virtio-tablet-pci -device virtio-keyboard-pci
  -display none -vnc 127.0.0.1:0 -qmp pipe:lightvm-<tag>
```

The QMP client over named pipe (`boot-pipe.mjs`) stays connected for the whole test (constraint 4.2), queries `query-status` every 15 s, runs `screendump` at 90 s, then `system_powerdown` and, after the timeout, `quit`. Stderr saved in `lab/logs/<tag>.stderr.txt`, transcripts in `lab/logs/<tag>.run.txt`, screenshots in `lab/vms/boot/`.

### 9.1 `-cpu host` (required configuration) — failed

`boot-ubuntu` (virtio-vga): `query-accelerators` → `whpx`; `query-cpus-fast` → 2 vCPUs `host-x86_64-cpu`; state `running` for the whole test. `screendump` at 90.9 s: P6 640×480, 0.22 % non-black pixels, text "Display output is not active." `system_powerdown` → `POWERDOWN` event, no `SHUTDOWN` in 60 s; `quit` → `SHUTDOWN host-qmp-quit`. Total duration 152 s, 0 leftover processes. Stderr: only `Ignoring request for interrupt vector 0`.

Diagnostics (`boot-ubuntu-stdvga` with `-device VGA`, screenshots every 15 s for 180 s): all identical, "Guest has not initialized the display (yet)." `fw-diag.ps1` after 25 s of execution, `info registers`:

| Variant | RIP vCPU0 | Display after 25 s |
| --- | --- | --- |
| pflash, `-cpu host` | `000000007fec0710` (RAX=`fee00900`, RCX=`1b`) | not initialized |
| pflash, `-cpu max` | `000000007fec0710` (same registers) | not initialized |
| pflash, `-cpu host,-x2apic` | `000000007fec0710` | not initialized |
| pflash, `-cpu host,pmu=off` | `000000007fecb277` | not initialized |
| pflash, `-cpu host,-pdpe1gb,-hle,-rtm` | — | not initialized |
| pflash, `-cpu max,-x2apic,-pdpe1gb` | — | not initialized |
| pflash, default CPU / `-cpu qemu64` | advances | GRUB 2.14, menu "Try or Install Ubuntu" (1280×800) |
| pflash, `Skylake-Client-v4`, `Haswell-v4`, `Nehalem` | — | GRUB (1280×800, 3.57 % non-black) |
| SeaBIOS (no pflash), `-cpu host` | state `paused` | stderr `WHPX: Unexpected VP exit code 4` |
| `-bios edk2-x86_64-code.fd` | — | QEMU exits: `could not load PC BIOS` |

With `-cpu host` and `-cpu max` OVMF stalls before initializing the display; with named CPU models the firmware reaches GRUB. The cause was not determined (hypothesis to verify: CPUID features exposed by `host`/`max` not handled by WHPX on this host with VBS/HVCI). **Outcome: failed** for the required configuration; no TCG fallback used.

### 9.2 `-cpu qemu64` — verified (documented deviation)

`boot-ubuntu-qemu64` (virtio-vga, 90 s test requested):

- `query-accelerators` → `whpx`; vCPU `qemu64-x86_64-cpu` ×2; `RTC_CHANGE` events at 1.9 s and 2.9 s.
- Screenshots: 15 s and 30 s GRUB (3.57 % non-black); from 45 s to 90 s TianoCore logo + spinner + Ubuntu logo (Plymouth: kernel and userspace started), 46–61 colours sampled.
- `screendump` at 92.0 s: `boot-ubuntu-qemu64.ppm`, P6 1280×800, 3 072 016 bytes (= 1280×800×3 + 16), 17 992 non-black pixels (1.76 %), SHA-256 `b4571ce8613d5c542a7fbaef1e295b99875cd1e6b88d3d8b12cdedbc8925811a`: not all zero.
- `system_powerdown` at 92.0 s → `POWERDOWN`; event `DEVICE_TRAY_MOVED tray-open:true` at 184.9 s (the guest reacted by starting the shutdown and ejecting the medium); no `SHUTDOWN` within 120 s; `quit` at 212.0 s → `SHUTDOWN host-qmp-quit`. 0 leftover processes.

`boot-ubuntu-qemu64-long` (same configuration, 240 s): screenshot at 105 s still Plymouth; at 120 s live desktop with the window "Welcome to Ubuntu — Choose your language" (100 % non-black pixels, 789 colours sampled); unchanged until 240 s. `system_powerdown` at 241.7 s → `POWERDOWN`, no `SHUTDOWN` and no eject event within 180 s; `quit`. Total duration 423 s, 0 leftovers.

**Outcome: verified** as a WHPX-accelerated boot up to the graphical environment, **with CPU model `qemu64` and not `host`**. It is not a TCG result. Limits: slow startup (desktop between 105 and 120 s), CPU model with few extensions; graceful shutdown via ACPI not completed in the allotted time; no installation to disk; input in the guest not yet tested.

## Actions required from the user

No step of this verification was blocked by the lack of elevation. Recommended actions, to be run in PowerShell **as administrator**:

1. Confirm the state of the feature: `Get-WindowsOptionalFeature -Online -FeatureName HypervisorPlatform`.
2. If it is disabled, consider enabling it to align the host with the documented WHPX prerequisite: `Enable-WindowsOptionalFeature -Online -FeatureName HypervisorPlatform` (requires a reboot). After enabling it, repeat 4.1 and 9.1 to check whether the behaviour of `-cpu host` changes.
3. Optional, for the multi-user permission tests (4.3, 6.1): provide a second local non-administrator account.

## Limits of this verification

- A single host, a single QEMU build (11.1.0 from qemu.weilnetz.de); no result extends to other hosts or versions.
- Unexplained discrepancy between `HypervisorPlatform` *Disabled* and WHPX working.
- The code signature of the QEMU binaries is not valid; provenance relies on checksum and winget manifest.
- Socket permissions tested only as observed ACL, without an access attempt from a second user.
- The VNC rejection without a client certificate does not close the connection; behaviour with TLS 1.2 not tested.
- Input via QMP accepted but effect in the guest not observed; no full RFB client.
- Graceful shutdown not demonstrated; forced stop tested only with `Stop-Process`, without a Job Object.
- Single time measurements, not statistical samples (section 13 requires at least 100 samples for latencies).
- The PNG screenshots in `lab/vms/` are conversions of the PPMs done with `ppm2png.mjs` for visual inspection.

## Proposed decisions for the ADRs (not decided here)

1. **QEMU build.** Use the Windows build from qemu.weilnetz.de pinned by version and hash (11.1.0, installer SHA-512 above), extracted portably without the installer, with the hashes of binaries and firmware recorded in the manifest. Record the expired Authenticode signature as a risk and consider an additional verification (winget manifest, reproducible rebuild or alternative source) before F6.
2. **Private QMP channel.** Prefer AF_UNIX in a private supervisor directory with a user-only ACL (possibly with an explicit DACL created by the supervisor), file removed and recreated per run. Discard `-qmp pipe:` created by QEMU because of the limits in 4.2 (blocking at startup, single client without reconnection, stall in `prelaunch`, taken by the first process that opens the pipe). To be tested in the Rust spike: socket or pipe created by the supervisor and passed to QEMU by inherited handle (`-chardev socket,fd=` or equivalent on Windows), with the ACL verified from a second user.
3. **Display transport.** Primary candidate: VNC over AF_UNIX in the same private directory (endpoint and banner verified). Alternative: loopback with VeNCrypt X.509 and `verify-peer=on` (verified), plus a `connections=` limit and supervisor timeouts for unauthenticated connections. VNC without TLS over TCP offers `None` and remains excluded. `screendump` fallback already working via QMP.
4. **Firmware.** OVMF `edk2-x86_64-code.fd` in read-only pflash plus a per-run copy of `edk2-i386-vars.fd` (hashes recorded); SeaBIOS not suitable with WHPX on this host (exit code 4). Secure Boot/SMM not tested.
5. **CPU model.** `-cpu host` and `-cpu max` do not boot OVMF on this host: propose a verified named model (candidates `Skylake-Client-v4` or `Haswell-v4`, which reach GRUB; `qemu64` reaches the desktop) and repeat the full boot with the chosen model; record the deviation in `COMPATIBILITY.md`.
6. **Network profile.** `-nodefaults -nic none` mandatory plus post-start check with `info network`/`query-pci`; do not use `query-rx-filter` as proof of the absence of a NIC.
7. **Storage.** QEMU locking does not protect images on Windows: the supervisor must guarantee exclusion (access only through a per-VM queue, opening with restrictive sharing modes or its own lock, base with read-only attribute/ACL) and must not rely on `qemu-img` refusing.
