# LightVM user guide (alpha)

This guide takes you from a Windows 11 PC to an Ubuntu virtual machine (VM) installed with LightVM, and back to it after you close and reopen the app. It covers the **experimental alpha** (roadmap phase F2): read [Known limitations](#15-known-limitations-of-the-alpha) before you start.

Contents:

1. [Requirements](#1-requirements)
2. [Install QEMU 11.1.0](#2-install-qemu-1110)
3. [Install LightVM](#3-install-lightvm)
4. [First start and QEMU setup](#4-first-start-and-qemu-setup)
5. [Download the Ubuntu LTS installation image](#5-download-the-ubuntu-lts-installation-image)
6. [Create the VM with the wizard](#6-create-the-vm-with-the-wizard)
7. [Install Ubuntu in the VM](#7-install-ubuntu-in-the-vm)
8. [After the installation](#8-after-the-installation)
9. [Everyday use: stop, Emergency Stop, delete](#9-everyday-use-stop-emergency-stop-delete)
10. [Baselines: save and restore the VM's disk](#10-baselines-save-and-restore-the-vms-disk)
11. [Snapshots: named points of the VM's disk](#11-snapshots-named-points-of-the-vms-disk)
12. [Agent sessions](#12-agent-sessions)
13. [Where your data lives](#13-where-your-data-lives)
14. [Uninstall](#14-uninstall)
15. [Known limitations of the alpha](#15-known-limitations-of-the-alpha)
16. [Troubleshooting](#16-troubleshooting)

---

## 1. Requirements

| Item | Requirement |
| --- | --- |
| Operating system | Windows 11, 64-bit (x64). Windows on ARM and Windows 10 are not supported. |
| CPU | x86-64 with hardware virtualization (Intel VT-x or AMD-V) **enabled in the firmware (BIOS/UEFI)** |
| Windows feature | **Windows Hypervisor Platform** enabled (QEMU uses it through its `whpx` accelerator) |
| Memory | 16 GiB recommended (the project's reference host); the Ubuntu VM uses 4 GiB by default |
| Disk | SSD recommended; about 6 GiB for the Ubuntu image plus the VM disk (32 GiB by default; Ubuntu Desktop needs at least 25 GB) |
| QEMU | QEMU **11.1.0** for Windows, installed separately (section 2). LightVM does not include QEMU and does not work with other versions. |
| Account | A normal user account. LightVM itself needs no administrator rights; enabling the Windows feature and some QEMU install options do. |

### Check that virtualization is enabled

1. Open **Task Manager** (Ctrl+Shift+Esc) → **Performance** → **CPU**.
2. Under the graph, **Virtualization** must read **Enabled**. If it reads *Disabled*, enable Intel VT-x / AMD-V (sometimes called *SVM Mode* or *Intel Virtualization Technology*) in your PC's firmware setup; the steps depend on the manufacturer.

### Enable Windows Hypervisor Platform

1. Press Start, type **Turn Windows features on or off** and open it (administrator rights required).
2. Tick **Windows Hypervisor Platform** and press **OK**.
3. Restart Windows when asked.

Equivalent command, in a PowerShell window opened **as administrator**:

```powershell
Enable-WindowsOptionalFeature -Online -FeatureName HypervisorPlatform
```

To check the state (also as administrator): `Get-WindowsOptionalFeature -Online -FeatureName HypervisorPlatform` → `State : Enabled`.

Note: on the project's test PC the feature was reported as *Disabled* while the accelerator worked anyway (Hyper-V and virtualization-based security were active). LightVM does not trust the reported state: it asks QEMU which accelerators really work and shows the result on the Dashboard (**Acceleration**, in the supervisor card at the top). LightVM never falls back to slow software emulation (TCG): if the accelerator does not work, VMs do not start and you get an explicit error.

---

## 2. Install QEMU 11.1.0

LightVM drives QEMU 11.1.0 for Windows from the builds page the QEMU project links to (`https://qemu.weilnetz.de/w64/`, by Stefan Weil). The project approved one exact file (ADR-0010):

| Item | Value |
| --- | --- |
| File | `qemu-w64-setup-20260811.exe` (QEMU 11.1.0, `v11.1.0-12130-ge470268ff4`, 206 615 928 bytes) |
| Download | `https://qemu.weilnetz.de/w64/qemu-w64-setup-20260811.exe` (the winget manifest points to the same file under `https://qemu.weilnetz.de/w64/2026/`) |
| Installer SHA-256 | `f98a8aeb5f7faea9765b6dee28316c266cd179d80354a2fed8e50176f9a2e59f` |
| Installer SHA-512 | `5bcf9eed634e8575a37b74f445af41a2fe4106da512d0c30c368301d4c105037fdfab40a5287367a28a957624cddebbc8c07e16c88ab6634f554cdf3d16bf543` (also published by the site as `qemu-w64-setup-20260811.sha512`) |

After installing, LightVM checks these files itself and refuses any other build:

| File in the QEMU folder | SHA-256 |
| --- | --- |
| `qemu-system-x86_64.exe` | `47d57a6072e0bb3bd98f87926eb129eb1736dfe818c67b3b81ef7ce4edd0b3cd` |
| `qemu-img.exe` | `9d930582d0d574bb106d89982c547688e10b8f356192d302cc3d0ae4e8674534` |
| `share\edk2-x86_64-code.fd` (UEFI firmware) | `33090cc07675baa5190d9f1e84bf5176b33bcbfa9bacac522961150cdb6dbb2a` |
| `share\edk2-i386-vars.fd` (UEFI variables template) | `5d2ac383371b408398accee7ec27c8c09ea5b74a0de0ceea6513388b15be5d1e` |

**Code signature:** the QEMU binaries carry an Authenticode signature whose certificate **expired in 2023**. Windows will report the signature as not valid. This is known (ADR-0010): provenance relies on the checksums above, so check them.

### 2.1 Download and verify

In PowerShell (no administrator rights needed):

```powershell
$dl = "$env:USERPROFILE\Downloads"
Invoke-WebRequest https://qemu.weilnetz.de/w64/qemu-w64-setup-20260811.exe -OutFile "$dl\qemu-w64-setup-20260811.exe"
(Get-FileHash "$dl\qemu-w64-setup-20260811.exe" -Algorithm SHA256).Hash
```

The printed value must be `F98A8AEB5F7FAEA9765B6DEE28316C266CD179D80354A2FED8E50176F9A2E59F` (PowerShell prints it in upper case). **If it differs, delete the file and do not use it.**

### 2.2 Install: choose one option

**Option A — per-user extraction, no administrator rights (the method the project verified).** Requires [7-Zip](https://www.7-zip.org/). The installer is extracted, not run:

```powershell
& "C:\Program Files\7-Zip\7z.exe" x "$env:USERPROFILE\Downloads\qemu-w64-setup-20260811.exe" -o"$env:LOCALAPPDATA\Programs\qemu-11.1.0"
```

The QEMU folder is then `%LOCALAPPDATA%\Programs\qemu-11.1.0` (it contains `qemu-system-x86_64.exe` and the `share` folder). The extraction also creates a `$PLUGINSDIR` folder that is not used and can stay.

**Option B — run the installer (administrator rights, UAC prompt).** Double-click the verified `qemu-w64-setup-20260811.exe` and accept the defaults. The QEMU folder is `C:\Program Files\qemu`.

**Option C — winget (administrator rights).** Installs the same file (same SHA-256):

```powershell
winget install --id SoftwareFreedomConservancy.QEMU --exact --version 11.1.0
```

The QEMU folder is `C:\Program Files\qemu`. Do not let winget upgrade QEMU later: LightVM accepts only 11.1.0.

### 2.3 Verify the installed files

```powershell
$q = "$env:LOCALAPPDATA\Programs\qemu-11.1.0"   # or "C:\Program Files\qemu"
Get-FileHash "$q\qemu-system-x86_64.exe", "$q\qemu-img.exe", "$q\share\edk2-x86_64-code.fd", "$q\share\edk2-i386-vars.fd" -Algorithm SHA256 |
    Format-Table Hash, Path -AutoSize
```

Compare with the table above. LightVM repeats this check at every supervisor start (**Settings** → **QEMU distribution**).

LightVM finds QEMU by itself when no folder is chosen in **Settings**: it uses the first of these folders that contains `qemu-system-x86_64.exe`:

1. `%LOCALAPPDATA%\Programs\qemu-11.1.0` (option A);
2. `C:\Program Files\qemu` (options B and C);
3. any folder listed in the `PATH` environment variable.

**Settings** → **QEMU distribution** then shows the folder followed by *(found automatically)*. A folder found this way gets the same hash check as a chosen one: a QEMU that is not the approved 11.1.0 is refused when the supervisor starts.

---

## 3. Install LightVM

The alpha is distributed as a zip archive, `LightVM-<version>-windows-x64.zip`, with its checksum file `LightVM-<version>-windows-x64.zip.sha256`. It installs **for the current user only**, in `%LOCALAPPDATA%\Programs\LightVM`, without administrator rights and without changing the `PATH`.

**The LightVM binaries are not code-signed** (ADR-0020: no signing certificate is available to the project). Verify the checksum before running anything.

1. Download both files into the same folder and verify:

   ```powershell
   cd "$env:USERPROFILE\Downloads"
   $zip = Get-Item .\LightVM-*-windows-x64.zip
   (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
   Get-Content "$($zip.FullName).sha256"
   ```

   The two values must be identical. If not, delete the download.

2. Extract and install:

   ```powershell
   Expand-Archive $zip -DestinationPath "$env:TEMP\LightVM-package" -Force
   cd (Get-ChildItem "$env:TEMP\LightVM-package" -Directory | Select-Object -First 1).FullName
   powershell -NoProfile -ExecutionPolicy Bypass -File .\install.ps1
   ```

   `-ExecutionPolicy Bypass` applies only to that one PowerShell process: the downloaded script is not signed. `install.ps1` checks every file of the package against `SHA256SUMS.txt`, copies them into `%LOCALAPPDATA%\Programs\LightVM`, creates the Start Menu shortcut **LightVM** and writes `install-manifest.json` (used by the uninstaller).

   Options: `-Prefix <folder>` installs elsewhere; `-NoShortcut` skips the shortcut; `-AddToPath` adds the folder to your user `PATH` (off by default; the other entries are kept exactly as they are, including ones such as `%USERPROFILE%\bin`; new terminals see the change); `-Force` replaces an existing installation.

3. The package contains: `lightvm-desktop.exe` (the app), `lightvm-service.exe` (the supervisor, started by the app; the two files must stay in the same folder), `lightvm-mcp.exe` (the MCP adapter that an AI client starts for agent sessions, section 12; it does nothing on its own), `README.md`, `USER_GUIDE.md` (this guide), `LICENSE.txt`, `THIRD-PARTY-LICENSES.txt`, `SBOM.cdx.json` (software bill of materials: every component built into the three programs, with versions, licences and hashes, CycloneDX format), `BUILDINFO.txt` (commit and toolchain of the build), `SHA256SUMS.txt`, `install.ps1`, `uninstall.ps1`. The package has no test or measurement options: the programs are built without them and the build checks that none is left.

If a `LightVM-<version>-windows-x64-setup.exe` (Inno Setup, per user) is published next to the zip, it installs the same files into the same folder with a normal wizard; verify its `.sha256` the same way. It is equally unsigned.

---

## 4. First start and QEMU setup

1. Start **LightVM** from the Start Menu.
   - On the first run, Windows SmartScreen may show *Windows protected your PC* because the app is not signed. If, and only if, you verified the checksum in section 3, choose **More info** → **Run anyway**.
   - LightVM opens without a console window. Its diagnostic messages go to log files in `%LOCALAPPDATA%\LightVM\logs` (see [Troubleshooting](#16-troubleshooting)).
2. LightVM opens on the **Dashboard**. If you installed QEMU with option A, B or C of section 2, LightVM has already found it (section 2.3). Otherwise the Dashboard shows the notice *QEMU was not found: VMs cannot start. Choose the QEMU folder in Settings → QEMU distribution, then restart the supervisor.*, every VM's **Start** button is disabled with the same reason, and **Open Settings** under the notice leads to step 3.
3. Open **Settings** (left navigation). In the **Supervisor** section:
   - **QEMU distribution** shows the folder in use. *(found automatically)* after it means LightVM found it by itself: nothing to do. *not set: VMs cannot be started* means it found none: **Browse…** → select the QEMU folder from section 2 (the one that contains `qemu-system-x86_64.exe`). A folder chosen here takes precedence over the one found automatically.
   - **VM catalog** shows where VMs are stored (default `%LOCALAPPDATA%\LightVM\catalog`). Leave the default unless you need another drive. **Browse…** and **Default** only switch LightVM to another catalog folder: VMs already created stay in the old one. To take them along use **Move catalog…** (section 13.1).
   - Press **Apply and restart the supervisor** (or **Start the supervisor** if it is not running).
4. Back on the **Dashboard**, the supervisor card at the top (**Supervisor · PID …**) shows the connection state, **Backend**, **Acceleration** and **If the UI closes**. **Acceleration** must read `whpx`; *not detected* means VMs cannot use the hardware accelerator. **Settings** → **Host capacity** → **Hardware acceleration** says the same. If it does not work, see [Troubleshooting](#16-troubleshooting).

Also in **Settings**: **Appearance** → **Theme** (**Dark**, the default, or **Light**); **Console** → **Guest keyboard layout** and the list of **Console shortcuts**; **If LightVM closes unexpectedly** (what happens to running VMs if the app crashes; the default, **Terminate the VMs immediately**, stops them).

---

## 5. Download the Ubuntu LTS installation image

LightVM's reference guest is **Ubuntu 26.04.1 LTS Desktop, amd64** (pinned in F0, see `docs/verification/F0-03-guest-media.md`).

| Item | Value |
| --- | --- |
| Official page | `https://releases.ubuntu.com/26.04.1/` |
| File | `ubuntu-26.04.1-desktop-amd64.iso` |
| SHA-256 | `601e30fbf5d97759367c632e2c33630665039b7e2158fd068403da3ccf1bda1f` |
| Checksum list | `https://releases.ubuntu.com/26.04.1/SHA256SUMS` (signed: `SHA256SUMS.gpg`, Ubuntu CD image key `843938DF228D22F7B3742BC0D94AA3F0EFE21092`) |

```powershell
cd "$env:USERPROFILE\Downloads"
Invoke-WebRequest https://releases.ubuntu.com/26.04.1/ubuntu-26.04.1-desktop-amd64.iso -OutFile ubuntu-26.04.1-desktop-amd64.iso
(Get-FileHash .\ubuntu-26.04.1-desktop-amd64.iso -Algorithm SHA256).Hash.ToLower()
```

The value must be `601e30fbf5d97759367c632e2c33630665039b7e2158fd068403da3ccf1bda1f`. A browser download works as well; verify the hash in the same way. Optional, if you have GnuPG: verify `SHA256SUMS` with `SHA256SUMS.gpg` as described on the Ubuntu website ("How to verify your Ubuntu download").

Other Ubuntu versions may work but are not tested by the project.

---

## 6. Create the VM with the wizard

1. On the **Dashboard**, press **New VM**. The wizard opens; move between steps with **Next** and **Back**, or leave with **Cancel**.
2. **Name**: type a name for the VM, for example `ubuntu`. This is the only text field in LightVM (the import of a VMware VM, section 6.3, has the same name field); the name is used only inside LightVM. The wizard says when the name is not valid or already used.
3. **Installation media**: press **Import ISO…** and select `ubuntu-26.04.1-desktop-amd64.iso` in the file picker. LightVM uses the image **where it is**: it does not copy it (no disk space is used twice), it only checks it and records its SHA-256 and location. **Keep the file in that place**: LightVM never modifies, moves or deletes it, and checks it again before every start (see section 9.1 for what happens if it changes or disappears). Reading a 6 GiB image for its SHA-256 takes a minute or so. Images imported earlier are listed and can be chosen again for other VMs.
4. **System**: LightVM looks inside the image you chose and preselects the system it detects. Check it and change it if it is wrong. If it does not recognize the image, nothing is preselected: choose the system yourself; **Next** stays disabled until you do (*Choose the system to continue*). **Ubuntu LTS (x86-64)** is the reference guest; **Other Linux (x86-64)** is listed but not tested by the project. Windows systems are described in [section 6.1](#61-create-a-windows-vm).
5. **Data media** (optional): keep **No data media**. (A data ISO, imported with **Import data ISO…**, is attached to the VM as a read-only CD; it is not needed for the installation.)
6. **Resources**: **Virtual processors**, **Memory** and **Disk**. The defaults (2 vCPUs, 4 GiB, 32 GiB disk) are the standard template for Ubuntu LTS, shown as *Template: Ubuntu LTS — 2 vCPU, 4 GiB RAM, 32 GiB disk*, and suit Ubuntu Desktop; if you change them, **Reset to template** puts them back. The wizard does not offer more than the host can give. The disk file grows on the host only as the guest writes data.
7. **Network**: only **Offline: no network adapter** is available in this alpha (NAT and bridged networking are shown as not available).
8. **Summary**: check the values (for Ubuntu the firmware is always **UEFI**) and press **Create**.

The new VM appears on the Dashboard as **Stopped**, with system, CPU, memory, virtual disk size and space used on the host.

### 6.1 Create a Windows VM

LightVM can also create VMs for **Windows XP, Vista, 7, 8.1 and 10**, with the same wizard. **This is experimental: the project has started Windows Setup from Vista 32-bit and Windows 10 images, but has not yet completed a Windows installation** (`docs/COMPATIBILITY.md`, "Windows guest profiles"). **Windows 11 x64 is experimental on Windows hosts** with the optional TPM/Secure Boot package (section 6.4); installed Windows 11 remains unqualified. Windows 11 x86 and Windows 8 are not supported; Windows 8 is replaced by 8.1. On a Mac with Apple Silicon only Windows 11 ARM64 is offered (section 6.2).

What you need:

- **Your own Windows installation image** (ISO) and **a valid licence (product key)**. LightVM does not include Windows, does not provide keys and does not handle activation: that is your responsibility, under the terms of your licence.
- Enough disk space for the system: the wizard suggests a size for each Windows version and does not accept less than its minimum.

Steps (only what differs from section 6):

1. **Installation media**: import the Windows ISO with **Import ISO…**.
2. **System**: LightVM reads the image and shows what it found, for example *Detected: Windows 7 (64-bit, build 7601)*. If it shows *Not recognized: choose the system*, nothing is selected and you must pick the system in the list before going on; if the suggestion is wrong, pick the right one. The detection is only a suggestion; your choice is what counts. An image of Windows 8 or Windows 11 x86 is not recognized. Windows 11 x64 media are recognized and can be selected on Windows hosts (section 6.4). An image for another architecture (for example Windows 11 ARM64 on a Windows PC) is shown as *Detected: … — not supported on this host* and nothing is selected. **Windows Vista 64-bit does not start on a Windows PC** (its setup stops with a blue screen under the Windows hypervisor): its image is shown as *not supported on this host*, the wizard explains why and does not go on with it; use the 32-bit Vista image.
3. **Firmware** (Windows 8.1 and 10 only): choose **UEFI** or **Legacy BIOS (SeaBIOS)**. The wizard preselects **UEFI** for 64-bit images and **Legacy BIOS** for 32-bit images; keep the preselection unless you know you need the other one. Windows XP, Vista and 7 always use Legacy BIOS. Choose before installing: a system installed with one firmware does not boot with the other.
4. **Resources**: when you choose the system, the wizard fills in the standard template for that system and for the architecture of the image (32-bit or 64-bit; see the table below), unless you already changed the values. The step names the template, for example *Template: Windows 7 64-bit — 2 vCPU, 4 GiB RAM, 40 GiB disk*. You can change the values within what the host can give, and **Reset to template** puts them back. For 32-bit Windows the memory stops at 4 GiB, the most a 32-bit Windows can use.
5. **Network**: **Offline** only, as for every VM. Networking for Windows VMs is planned as separate work.
6. **Summary**: shows the system, the firmware (*UEFI* or *Legacy BIOS (SeaBIOS)*) and a one-line hardware summary such as *AHCI disk, standard VGA, USB tablet*. Press **Create**.

**Standard templates.** The starting resources the wizard proposes for each system (from `GuestOs::template` in `crates/lightvm-proto`). When the image architecture is not detected, the template is named without it (for example *Template: Windows 7*) and Windows XP uses the 32-bit values, the other systems the 64-bit ones. The wizard never goes below the minimum of the system or above what the host can give.

| System | Architecture | vCPU | Memory | Disk |
|---|---|---|---|---|
| Ubuntu LTS | x86-64 | 2 | 4 GiB | 32 GiB |
| Other Linux | x86-64 | 2 | 2 GiB | 20 GiB |
| Windows XP | 32-bit | 1 | 1 GiB | 16 GiB |
| Windows XP | 64-bit | 1 | 2 GiB | 20 GiB |
| Windows Vista | 32-bit and 64-bit (64-bit not on Windows PCs) | 2 | 2 GiB | 40 GiB |
| Windows 7 | 32-bit | 2 | 2 GiB | 32 GiB |
| Windows 7 | 64-bit | 2 | 4 GiB | 40 GiB |
| Windows 8.1 | 32-bit | 2 | 2 GiB | 32 GiB |
| Windows 8.1 | 64-bit | 2 | 4 GiB | 50 GiB |
| Windows 10 | 32-bit | 2 | 2 GiB | 40 GiB |
| Windows 10 | 64-bit | 2 | 4 GiB | 64 GiB |

During and after the installation:

- With **UEFI**, Windows images show *Press any key to boot from CD or DVD…* for a few seconds at start. While the virtual disk is still empty, LightVM presses the key for you during the first seconds, so setup starts even if the console is not open yet. Once Windows is installed it no longer does: if the installation media is still attached, the message times out and the VM boots from its disk.

- The VM has a standard disk controller, a standard graphics card, a **USB tablet** as the mouse and a **PS/2 keyboard**. Windows has its own drivers for all of them, so **no extra driver disc is needed** and you do not need to install guest tools. The mouse pointer follows your mouse in the console without being "captured".
- Set the **Guest keyboard layout** in the console toolbar (**US** or **IT**) to the layout you choose in Windows setup, as for Ubuntu (section 7).
- **No network**: skip the network steps of Windows setup (choose to set it up later, or a local/offline account where setup offers one). Windows Update and online activation are not available inside the VM; enter your product key when setup asks or later, following your licence's activation options.
- After the installation, remove the installation media as described in section 8, step 2, so the VM boots from its disk.
- Baselines work as for Ubuntu (section 10). A VM with Legacy BIOS has no firmware settings to save, so its baselines contain only the disk.

**Security note.** Windows XP, Vista, 7 and 8.1 no longer receive security updates, and ordinary support for Windows 10 ended on 14 October 2025. Keeping these VMs offline removes the network as a way in or out, but the systems themselves stay unpatched: only bring in software you trust, through the read-only data disc.


### 6.2 Windows 11 ARM64 on a Mac

On a Mac with Apple Silicon (M1 and later) the wizard offers only **Windows 11 (ARM64)**: the Mac's hypervisor runs only ARM64 systems, so the x86 systems of section 6.1 and Ubuntu x86-64 are not in the list (an x86 image is shown as *not supported on this host*). **This is experimental and not verified** (`docs/COMPATIBILITY.md`, "Windows 11 ARM64 on macOS Apple Silicon"; ADR-0028). **Starting the VM is not available yet**: you can create it, but the start waits for the macOS backend, which is still being built.

What you need:

- **Your own Windows 11 ARM64 installation image** (ISO, for example `Win11_25H2_EnglishInternational_Arm64.iso` from Microsoft) and **a valid licence**. LightVM does not include Windows, does not provide keys and does not handle activation; Microsoft names Parallels Desktop and VMware Fusion as the authorised ways to run Windows 11 on Apple Silicon, and LightVM claims no such authorisation. Setup uses *I don't have a product key*; no requirement check is bypassed.
- At least **4 GiB of memory and 32 GiB of disk** for the VM (Microsoft lists 64 GB; setup accepts 32 GB, R2b-01); the template is 4 vCPU, 6 GiB RAM, 64 GiB disk. The disk file grows only with use.

What the wizard shows: after you import the ISO, the System step reads *Detected: Windows 11, ARM64, build 26200 — …* and preselects **Windows 11 (ARM64)**. The firmware is fixed to **UEFI with Secure Boot**, and the hardware summary is *NVMe disk, ramfb display, USB keyboard and tablet, TPM 2.0, UEFI with Secure Boot*. The system and the firmware cannot be changed after creation.

- **TPM 2.0 and Secure Boot** are provided by LightVM: an emulated TPM 2.0 per VM and a firmware built by the project with Secure Boot on and the Microsoft keys, so Windows setup passes its requirement check without workarounds. The TPM state belongs to the VM; baselines are planned to save and restore it together with the disk and the firmware settings (ADR-0028), so secrets sealed to the TPM (BitLocker, Windows Hello) follow the baseline they were created in.
- **Offline only**, as for every VM: during setup choose to continue without internet / with a local account where setup offers it. If an edition of setup cannot complete offline, it cannot be installed in LightVM.

### 6.3 Importing a VMware VM

A VM you already have in **VMware Workstation or VMware Player** (a folder with a `.vmx` file and `.vmdk` disks) can be brought into LightVM without reinstalling the guest. LightVM **converts** the VMware disk into a new disk of its own catalog: the VMware files are only read, **never modified**, and the VM keeps working in VMware as before. The converted VM is independent: later changes in VMware do not reach it, and changes in LightVM do not reach VMware. **This is experimental: importing a real VMware VM and starting it to the desktop has not been verified yet** (ADR-0030).

**Before you start**, in VMware:

- **Shut the VM down** from inside the guest (Start → Shut down), not *Suspend*. A **suspended** VM is refused (*resume it in VMware and shut it down*), and so is a VM that is **running or locked** (VMware keeps `.lck` folders next to the files while the VM is open: close the VM, or VMware, first) and a VM that was **not shut down cleanly** (power it on in VMware and shut it down from the guest).
- **Encrypted** VMs are refused: remove the encryption in VMware first (VM Settings → Options → Access Control).
- Check the guest type in VMware (VM Settings → Options → General): LightVM maps it to its own systems (Windows XP, Vista, 7, 8.1, 10, Linux 64-bit). An **unknown or unsupported guest type** is refused, for example Windows 11, Windows Server or a 32-bit Linux. **Windows Vista 64-bit** VMs are refused on a Windows PC, as in section 6.1.
- If the disk is on an **LSI Logic** SCSI controller and the guest is Windows, LightVM prepares the converted disk itself (it enables the guest's own driver of the new controller): see [LSI Logic disks](#lsi-logic-disks) below for the conditions and for the manual preparation when it is not available.
- Have enough free space on the drive of the LightVM catalog: about the space the VMware disk files use (the preview shows it).

**Steps:**

1. On the **Dashboard**, press **Import VMware VM** and select the `.vmx` file of the VM in the file picker.
2. LightVM examines the VM (*Examining "….vmx" and its disks…*): it reads the `.vmx` and the descriptors of the disks, follows the chain of snapshot files and checks it with `qemu-img`. Nothing is written yet. If the VM cannot be imported, the page says why (*This VM cannot be imported: …*); fix it in VMware and press **Examine again**, or **Choose another .vmx…**.
3. The preview shows:
   - **Warnings** first, if any (see below);
   - **VMware VM**: file, VMware name, guest type, the disk file with its slot and controller (for example *scsi0:0 on VMware Paravirtual SCSI*), its virtual size and the space it uses, and how many **snapshot levels** will be flattened;
   - **New LightVM VM**: the **Name** (proposed from the VMware name; change it if you like, it must be valid and not already used), the system, firmware, virtual processors (with the VMware layout of cores per socket, kept because Windows client editions use at most one or two processor sockets), memory, disk and disk controller; **Network** is *Offline* as for every VM;
   - **Display adapter** (Windows VMs only): **Standard VGA** works with every Windows; **VMware SVGA II** uses the VMware Tools display driver already installed in the guest (the page says when the `.vmx` shows that VMware Tools were installed). If the screen stays black or low-resolution with one choice, the setting is fixed at import: import the VM again with the other one;
   - **Imported** and **Not imported** lists, and the space needed and free on the catalog drive. **Import** stays disabled while the name is not valid or the space is not enough.
4. Press **Import**. The page shows the phase (*Checking the VMware files…*, *Converting the disk…*, *Checking the converted disk and creating the VM…*) and a progress bar. Converting a 60 GiB disk takes several minutes. Meanwhile LightVM keeps the VMware disk files open read-only, so VMware cannot start the VM until the import ends.
5. When it ends (*Imported: "…" is in the catalog with its converted disk*), press **Go to the VM**: the VM is on the Dashboard, **Stopped**. Start it as any other VM (section 7 for the console).

**Cancel import** stops the conversion at any time: the partial disk is removed and no VM is created (*Import cancelled*). The Emergency Stop, closing LightVM and an interruption of the computer also stop it; after a crash LightVM removes the partial disk the next time it starts. Nothing in the VMware folder is ever touched.

**What is imported:** the guest system (from the VMware guest type), the virtual processors and memory (within what this PC can give; the preview warns when a value had to be lowered or raised), the firmware (**Legacy BIOS** or **UEFI**), and the **current state of the main disk**, with all its snapshot levels merged into one disk. The disk keeps its controller type, so the guest finds it with the driver it already has:

| VMware disk controller | Controller in LightVM |
| --- | --- |
| SATA | SATA (AHCI) |
| IDE | IDE (the VM uses the older PC chipset that has an IDE controller) |
| NVMe | NVMe (UEFI VMs); for legacy BIOS VMs the standard controller of the system, because the legacy firmware cannot boot from NVMe (see below) |
| SCSI, VMware Paravirtual (PVSCSI) | VMware Paravirtual SCSI (the guest keeps its VMware PVSCSI driver) |
| SCSI, LSI Logic SAS | LSI Logic SAS 1068 |
| SCSI, LSI Logic (parallel) | the standard controller of the system: SATA (AHCI) for Vista and later, IDE for XP, virtio for Linux; see below |
| SCSI, BusLogic | as LSI Logic |

**What is not imported:** the snapshots themselves (only the current state is converted: the VMware snapshot tree stays in VMware), a suspended state, the network adapters (LightVM VMs are offline), sound, 3D acceleration, USB controllers and devices, serial and parallel ports, printers, floppy drives, hard disks other than the main one, and CD/DVD drives (an ISO the VM used can be imported as a data medium, section 9.1, and attached in **Edit**). **Shared folders, USB devices, clipboard and drag and drop** are not taken from the `.vmx`: they will come later through LightVM's own guest tools (*LightVM Tools*), not through VMware Tools. VMware Tools stay installed in the guest but their host features do nothing in LightVM; you can leave them or uninstall them.

**UEFI VMs:** the firmware starts with new settings, so the boot entries VMware had are not carried over; the firmware starts the default boot loader on the disk, which Windows and the usual Linux distributions install. If an UEFI Linux VM does not boot, its boot loader is only registered in the old entries: reinstall it with the `--removable` option (for GRUB) from VMware before importing. Windows Vista and 7 VMs that use UEFI in VMware are refused (they need a legacy video path the LightVM firmware does not provide).

#### LSI Logic disks

QEMU, which runs the VMs, cannot emulate the **LSI Logic parallel SCSI** controller that VMware uses by default for many Windows VMs. LightVM attaches such a disk to the standard controller of the system (SATA AHCI for Windows Vista and later, IDE for XP), and Windows starts only from a disk controller whose driver it loads at boot; otherwise it stops with the blue screen **STOP 0x0000007B (INACCESSIBLE_BOOT_DEVICE)**. The same replacement and preparation applies to a **NVMe** disk of a **legacy BIOS** VM: the legacy firmware cannot boot from NVMe, so the disk moves to the standard controller (UEFI VMs keep NVMe).

**LightVM prepares the disk for you, in the converted copy only.** The VMware VM is never changed. During the import, after the conversion, LightVM enables the **guest's own** driver of the new controller so that it starts at boot — nothing is copied or installed, and no administrator rights are needed on your PC. The page shows the phase *Enabling the Windows storage driver in the converted disk…* and, when it succeeds, the notice *LightVM sets the guest's own Windows driver of the new controller to start at boot, in the converted disk only: the VMware VM is not changed*. The preparation covers Windows Vista, 7, 8.1 and 10 (SATA AHCI) and Windows XP (IDE), and it is verified: if the check of the prepared disk fails, the import stops and nothing is created.

Two conditions, in VMware, before you import:

- **Shut the guest down cleanly** from inside Windows (Start → Shut down), not *Suspend*. A **hibernated** guest, or a guest shut down with **Fast Startup**, cannot be prepared: the import stops and says so (*Windows in the VMware VM is hibernated or was shut down with Fast Startup… start the VM in VMware, shut it down completely (Shift + Shut down, or `shutdown /s /t 0`) and import it again*). The same happens if the guest **was not shut down cleanly**.
- The driver of the new controller must be **installed in the guest** (all the systems listed above install it by default). If it is not, the import stops naming the driver, and you can prepare the guest in VMware with the manual ways below and import it again.

After the import, the first start may take a little longer than usual (Windows reconfigures its storage drivers for the new hardware), and **Windows may ask to be activated again**: its hardware changed.

If the preparation is not available on this installation (the storage helper is not installed, or your guest is a combination it does not cover), the preview keeps the warning *Windows may not start: storage driver preparation needed* and the VM's **Edit** panel repeats it. Prepare the VM **in VMware, before importing**, while it still runs on LSI Logic. Use one of these two ways.

*Way 1: enable the SATA (AHCI) driver for boot in the registry.* In the Windows guest running in VMware, as an administrator:

1. Open **regedit**.
2. Windows Vista and 7: go to `HKEY_LOCAL_MACHINE\SYSTEM\CurrentControlSet\Services\msahci` and set **Start** to `0`. If the keys `pciide` and `iaStorV` exist under `Services`, set their **Start** to `0` as well.
3. Windows 8.1 and 10: go to `HKEY_LOCAL_MACHINE\SYSTEM\CurrentControlSet\Services\storahci`, set **Start** to `0`, and if it has a subkey **StartOverride**, delete it (or set its value `0` to `0`).
4. Shut Windows down cleanly from the Start menu (not Suspend), then import.

*Way 2: let Windows install the driver itself.* In VMware, add a small second disk (1 GB) on a **SATA** controller to the VM (VM Settings → Add → Hard Disk → SATA), start Windows, let it find the new disk and install its driver, restart once, shut down, then remove that small disk in VMware and import. The second disk is not imported in any case.

A VM whose disk is already on SATA, PVSCSI, LSI Logic SAS, IDE or NVMe (with UEFI) needs no preparation. Linux VMs usually need no preparation: standard distributions load the driver of the new controller at boot (not verified by the project).

---

### 6.4 Windows 11 x64 on Windows (experimental)

The wizard recognizes Windows 11 x64 media and offers **Windows 11 (64-bit)**
on a Windows host. It uses UEFI with Secure Boot and a private emulated TPM
2.0; it requires at least two vCPUs, 4 GiB RAM and a 64 GiB disk. Your host's
physical TPM is not used.

Starting this profile requires the optional Windows 11 support package. In
development it must be explicitly hash-approved and placed under `win11/`
in the configured QEMU directory, or selected using the supervisor's
`--win11-dir` option. Build/provenance and developer commands are in
`docs/verification/R6-05-windows11-x64-swtpm.md` §13. A missing or changed
package refuses start; other guests use the ordinary QEMU installation.

Windows PE has reported TPM 2.0 and Secure Boot enabled, and a test TPM value
survived a cold restart. **A complete Windows 11 installation does not work
yet**: in the project's test, Setup stopped at its first restart with a
virtual CPU error under the Windows hypervisor. Baselines and snapshots of a
Windows 11 VM save and restore the TPM state together with disk and firmware
settings; take them with the VM shut down (a snapshot of a running Windows 11
VM is refused).
The project's WHPX-compatible firmware does not use SMM: Secure Boot checks
boot signatures, but a guest kernel can rewrite its firmware variable store.

## 7. Install Ubuntu in the VM

1. On the Dashboard, press **Start** on the VM. The state goes to **Starting**, then **Running**.
2. Press **Console** on the VM (or open the **Console** page in the navigation and press **Open console**). The guest screen appears inside LightVM once the console shows **Connected**; click on it to capture keyboard and mouse for the VM, as in VMware (a coloured border shows the capture). That first click only captures: it does not reach the guest. While captured, the Windows pointer stays inside the LightVM window and is hidden over the guest screen (the guest draws its own), and every key goes to the VM. Before capture nothing reaches the VM, not even the pointer position.
   - **Ctrl+Alt** (press both and let go, with no other key) releases keyboard and mouse. Switching to another window releases them too; click the console again to capture them. Only the **left** Alt counts: AltGr (used to type `@`, `#`, `[` on an Italian keyboard) never releases, and neither does Ctrl+Alt followed by another key.
   - **Ctrl+Alt+Esc** also releases keyboard and mouse, and leaves full screen.
   - **Ctrl+Alt+Enter** switches full screen on and off; the **Full screen** / **Exit full screen** button in the console toolbar does the same. Neither shortcut reaches the guest.
   - **Screenshot** saves the current guest screen as a PNG file (you choose where).
   - **Guest keyboard layout** (toolbar, **US** or **IT**; also in **Settings** → **Console**): set it to the layout configured inside the guest.
   - Clicks on LightVM's own menus and dialogs never reach the guest, even when they are drawn over the guest screen.
   - Clipboard sharing is **off by default**. Optional experimental Tools
     integration is described in section 9.2.
   - If the display connection drops while the VM keeps running, the console reconnects by itself after a couple of seconds.
3. Ubuntu boots from the installation image. Choose **Try or Install Ubuntu** in the boot menu (or wait), then **Install Ubuntu** in the installer.
4. Practical tips:
   - **Keyboard layout**: choose in the Ubuntu installer the same layout as your physical keyboard (for example *Italian* or *English (US)*), and select the same one (**IT** or **US**) in the console toolbar. LightVM sends keys by their physical position, so the layouts must match. Italian and English (US) are the layouts the alpha targets.
   - **"Erase disk and install Ubuntu"** erases only the **virtual disk** of this VM. Your Windows disks are not visible to the VM.
   - **No network**: skip the network and update steps; the installation works offline from the image. Do not select options that download third-party software or updates.
   - Create your Ubuntu user and password when asked. LightVM does not see or store them.
5. The installation takes several minutes. At the end Ubuntu asks to **Restart now**.

---

## 8. After the installation

1. When Ubuntu asks to remove the installation medium, press **Enter** in the console.
2. **The installation image stays attached and the VM tries it first.** In this alpha the installation ISO keeps priority in the boot order while it is attached to the VM, so after the restart the Ubuntu boot menu of the image may appear again. Two ways to boot the installed system:
   - in the boot menu of the image, choose **Boot from next volume** (if listed) to continue to the virtual disk; **do not** choose *Try or Install Ubuntu* again;
   - or stop the VM: once a VM that still has its installation media has run at least once, the Dashboard shows under it *Installation finished? Remove the installation media so the VM boots from its disk.* with a **Remove installation media…** button. Press it and confirm with **Remove media**, then **Start** again. (The same is possible with **Edit** → **Installation media** → **Remove** → **Save settings**.) The ISO stays in the registry and can be attached again from **Edit**.
3. Log in to the installed Ubuntu.
4. **Persistence check (the alpha's goal):** shut down Ubuntu from its own menu (or **Stop** in LightVM), close LightVM, start it again from the Start Menu: the Dashboard lists the VM, **Stopped**. Press **Start** and Ubuntu boots from the disk with your files.

---

## 9. Everyday use: stop, Emergency Stop, delete

| Control | What it does | When to use |
| --- | --- | --- |
| **Stop** | Orderly shutdown: the guest is asked to power off (like pressing a PC's power button once) and LightVM waits for it. A guest that does not stop within the supervisor's timeout is terminated. | Normal way to turn a VM off. Saving work inside the guest first is still a good idea. |
| **More** → **Force stop…** | Terminates the VM at once, like pulling the plug. Unsaved data in the guest is lost; the guest's file system may need a check at the next boot. | Only when the guest does not respond to **Stop**. |
| **More** → **Reset…** | Restarts the guest at once, like a PC's reset button. | Guest frozen. |
| **Emergency Stop** | Button at the top right of the Dashboard. Immediately terminates **all** VMs and revokes the agent sessions. | Anything looks wrong and you want everything to stop now. |
| **More** → **Delete…** | Removes the VM and its disk after a confirmation. Only possible while the VM is stopped (otherwise: *Stop the VM to delete it*). **Cannot be undone.** Imported installation images stay in the registry (to remove them see section 9.1). | VM no longer needed. |

Resources and execution limits (**Virtual processors**, **Memory (RAM)**, **Disk**, **Maximum CPU**, **Memory margin**, **Storage budget**) and the network (section 9.2) are changed with **Edit** → **Save settings**, only while the VM is **Stopped**; the disk can only grow.

Closing LightVM while VMs are running opens a dialog: **Stop them and quit** shuts the VMs down in an orderly way (terminating those that do not stop within the timeout) and then closes LightVM; **Cancel** keeps everything running. VMs are not left running in the background. If the VMs stop by themselves while the dialog is open, LightVM closes without asking further. If the supervisor has already stopped or lost its connection, LightVM closes at once: the VMs it was running follow the **If LightVM closes unexpectedly** setting.

### 9.1 Imported ISOs: where they are, and deleting them

An ISO imported in the wizard (installation and data media) is **used where it is**: LightVM records its location and SHA-256 and never copies, modifies, moves or deletes the file. **Settings** → **Installation media** lists them: name, **Installation** or **Data**, the file's location (*… · used where it is, not copied*), size, the system LightVM detected in it, when it was imported and which VMs use it (*used by "web-test"* or *not used by any VM*). Images imported by an earlier version of LightVM were copied into the catalog: they show *Read-only copy in the LightVM catalog* and keep working as before.

Keep each file in its place. While a VM runs, LightVM holds the file open so that no program can change, rename or delete it. Between runs it can, so every start reads the file again and compares its SHA-256 (the VM's row shows *Verifying installation media… N %*; for a large ISO this takes a moment at every start):

- **The file changed** (for example overwritten by a newer download with the same name): the start is refused with *the ISO file "…" changed since it was selected: import it again to use it*. Import the same file again with **Import ISO…**: LightVM updates the existing entry, so the VMs that use it start again.
- **The file was moved, renamed or deleted, or its drive is not connected**: the start is refused with *the ISO file "…" is no longer where it was selected*, and the list shows it with **File missing** and *File not found at …: put it back there, or import it again in the New VM wizard*. Put it back (or connect the drive) and it is usable again; an ISO imported again from a new place is a new entry, to choose in the VM's **Edit** → **Installation media**.
- Files on network shares, files reached through links or junctions, files inside the LightVM catalog folder, and files whose path contains a character QEMU cannot accept (a comma, `=`, a quote…) are refused at import with the reason; rename or move the file and import it again.

To remove an image from the list, press **Delete…** next to it and confirm with **Delete medium**. For an image used in place only the entry is removed: **the ISO file stays where it is, untouched** (the dialog says so). For a copy made by an earlier version, LightVM removes its copy from the catalog and frees that space; the original file is not touched. You can import either again later. **Cannot be undone.**

An image that a VM uses as installation or data media cannot be deleted: **Delete…** is disabled and its tooltip names the VMs. Remove it first with **Edit** → **Installation media** (or **Data media**) → **Remove** → **Save settings**, or with **Remove installation media…** on the Dashboard, or delete the VM. If the supervisor still refuses (for example a VM started using it from another window meanwhile), the dialog stays open and says why: *The medium was not deleted: it is used by 1 VM: "…"*, or *a running VM or another operation holds it open*. Agent sessions can never delete media. If LightVM is interrupted during a deletion, it finishes or undoes it the next time it starts.

### 9.2 Network: Offline, NAT, endpoint allowlist

A VM has one of three network modes, chosen in **Edit** → **Network** → **Mode** while the VM is **Stopped** (a running VM never changes network):

| Mode | What the guest reaches | Agent sessions |
| --- | --- | --- |
| **Offline (no network adapter)** — the default | Nothing: the VM has no network adapter. | Same. |
| **NAT (manual use only)** | This PC, the local network and the Internet, like VMware NAT. The guest gets its address automatically (DHCP); this PC is `10.0.2.2` inside the guest. | **Never.** While an agent session uses the VM, the VM starts with the endpoints granted to the session, or with no network. A session cannot be approved while the VM runs with NAT: stop it first. |
| **Endpoint allowlist** | Only the endpoints you list (below), each one TCP service. Nothing else on this PC, the local network or the Internet; no UDP (except the name lookups of the listed names), no IPv6. | Only the endpoints you tick when you approve the session; none ticked: no network. |

The guest needs a driver for the network adapter: Linux and Windows Vista and later find it on their own (Intel PRO/1000, `e1000`); Windows XP uses a Realtek RTL8139. Windows 11 on ARM (Mac) VMs can only be offline.

**Endpoints.** In **Edit** → **Network** (mode **Endpoint allowlist**, or **NAT** if you want to grant endpoints to agent sessions) press **Add endpoint** and fill in:

- **Name**: the name the guest uses, for example `sql.lan` or `wcfhost` (letters, digits, `-` and dots; no `*`). Inside the guest it also works as `wcfhost.lightvm.internal`.
- **Host**: the real computer the service runs on, as seen from this PC: `127.0.0.1` for a service on this PC, or a name or address on your network (for example `sqlserver.corp.example`).
- **Port**: the TCP port of the service (1–65535); the guest uses the same port.

LightVM gives each endpoint an address inside the guest (shown as *guest 10.0.2.100*): the guest connects to `Name:Port` or to that address and port, and LightVM opens a connection to `Host:Port` for each connection of the guest. The host name is looked up when the VM starts; the address used is in the audit log. **Save settings** stays disabled while an endpoint is incomplete or invalid; the panel says why.

What to know:

- **An allowed endpoint receives whatever the guest sends to it.** List only the services the guest needs, with an account that can do only what the test needs (a database server, for example, may offer features that reach files or the network).
- **SQL Server must listen on a fixed TCP port** (for example 1433, set in SQL Server Configuration Manager → TCP/IP → IP All → TCP Port). A **named instance** found through the SQL Server Browser (UDP 1434) is **not reachable**: only TCP is carried. In the connection string use `sql.lan,1433` (or the instance's fixed port).
- A WCF service on this PC must listen on an address the connection can reach, for example `net.tcp://localhost:8733/...` or `http://+:8733/...`, and the guest uses `net.tcp://wcfhost:8733/...`.
- Windows inside the guest may show the network as *Unidentified network* / *No Internet access*: that is expected (there is no Internet).
- When an agent session ends, a VM that was running for it keeps **no** network until it is stopped and started again: a session never widens the network of a running VM.

---

### 9.1 File transfer with LightVM Tools (experimental)

Enable **Tools** and **File transfer** in the VM settings before starting it,
and install/run the guest Tools from the generated Tools medium. In the
console, expand **Files · Guest Inbox / Outbox**:

- **Send file…** selects a host file to send. On Windows and macOS this opens
  a native picker. You can also drop up to 16 local files onto the console.
  Files arrive in the guest's `Documents/LightVM Tools/inbox` folder.
- To receive a file, put it directly in `Documents/LightVM Tools/outbox`
  inside the guest, press **Refresh Outbox**, then **Receive…** and choose a
  new host file name (native save dialog on Windows and macOS).
- On a Windows host, **Prepare drag…** downloads one selected Outbox file
  to a private temporary cache. Drag the resulting row into a host folder
  to copy it there. Keep LightVM open until the target finishes copying;
  **Clear prepared copy** removes the cache. This uses the Outbox row,
  rather than a drag gesture inside a guest application.
- Progress and **Cancel transfer** appear during the operation. Transfers
  continue while you visit another LightVM page. Stopping/restarting the VM,
  losing the supervisor or handing input to an agent cancels the operation.

Only regular files up to 4 GiB each are supported; folders and recursive
copies are not available. Existing files are never overwritten. A completed
file keeps its name; an interrupted local download removes its temporary
file. Files already completed in a batch are kept. If completion is
interrupted, check the destination before retrying.

Sending verifies the SHA-256 in the guest before publication. Receiving
checks chunk bounds, offsets and the listed size; it does not have a guest
checksum manifest or a stable snapshot of a changing Outbox file. Finish
writing a guest file before receiving it. Automated tests cover these controls;
real Ubuntu live and Windows PE tests are recorded in
`docs/verification/R6-07-lightvm-tools.md`. Installed guest qualification and
an actual drag into Explorer remain open.

### 9.2 Clipboard with LightVM Tools (experimental)

Enable **Allow clipboard text and images** in VM settings. The console's
text and image buttons perform one explicit copy in the indicated direction
while its window is focused. Images are bounded to 4,194,304 pixels and
8192 pixels per side. Linux guests additionally need Pillow and
`wl-clipboard` or `xclip` in their graphical session; unavailable formats or
backends produce an error.

**Synchronize clipboard text while the guest screen has focus** is off by
default and requires a fresh opt-in for a new run. While checked, it exchanges
text only when the console captures input and no other Tools operation holds
the channel. Focus loss pauses synchronization and discards pending guest
text. A new host copy takes priority over a pending guest reply. Images use
the explicit buttons. Agent sessions still need separate clipboard approval.

## 10. Baselines: save and restore the VM's disk

A **baseline** is a read-only copy of the VM's disk and UEFI variables, taken **with the VM off**. It is not a snapshot of a running VM: no memory (RAM) state is saved, and after a restore the guest starts with a normal boot. Typical use: install Ubuntu, shut it down, create a baseline "clean install"; later, bring the VM back to exactly that state in a few seconds.

Open it from the Dashboard: **More** → **Baselines…** on the VM's row. (If the item is disabled, the supervisor has no QEMU disk image tool: check the QEMU folder in **Settings**.) The page shows:

- the VM's **current state**: *Standalone disk* (no baseline was ever restored) or *Running on baseline "…"*, with the space written since that restore;
- the list of baselines, most recent first, each with its creation time (UTC), the space it takes on the host and its badges:

| Badge | Meaning |
| --- | --- |
| **Current** | The VM's current state is built on this baseline (an overlay of it): it cannot be deleted. |
| **Verified** | Taken after a clean shutdown of the guest (or before the VM ever ran). |
| **Not verified** | Taken after a forced stop, an Emergency Stop, a crash or a supervisor restart, with your explicit consent: the disk may hold incomplete writes. It stays *Not verified* for good. |
| **Protected** | Belongs to an agent session (later phases): it cannot be deleted. |
| **In use** | The VM's current run uses it. |

The same facts appear in one line under the VM's row on the Dashboard (for example *On baseline "Baseline 1 · 2026-10-11 09:12 UTC" · 300 MiB of changes since the restore · 2 baselines (7.0 GiB)*).

**Create a baseline.** Stop the VM, ideally by shutting the guest down from its own menu. Press **Create baseline…**: the dialog shows the name LightVM gives it (*Baseline N · date and time UTC*; there is nothing to type) and **Create baseline** starts the copy. LightVM copies the whole disk and checks and hashes the copy: for a large disk this takes **several minutes**. The page shows *Creating the baseline…* with the elapsed time; meanwhile the VM cannot be started or changed (its **Start** button is disabled and its row shows *Creating baseline*). There is no time limit and no Cancel button: only the **Emergency Stop** (top right) cancels the operation, and then nothing is changed. The button is disabled if the VM never ran (it has no disk yet: start it once and install the system first).

If the VM was **not shut down cleanly** last time (forced stop, Emergency Stop, crash, supervisor restart), LightVM refuses at first and explains why. The safer way is to start the VM, shut it down from the guest and try again. If you still want the baseline (for example a live session that ignores the power button), tick *I understand: the baseline is marked Not verified* and press **Create unverified baseline**.

**Restore a baseline.** With the VM stopped, press **Restore…** on a baseline and confirm with **Discard and restore**. The VM's current state (everything written to its disk and UEFI variables since the last restore, or since the installation if you never restored) is **discarded** and replaced by a fresh copy of the baseline; this cannot be undone. To keep the current state, create a baseline of it first. LightVM verifies the baseline's integrity before using it (a moment for a large disk); the next **Start** boots the guest from it with a normal boot. Every start on a baseline verifies it again first: the VM's row shows *Verifying baseline… N %*, then *Launching…* (a start with installation media attached shows *Verifying installation media… N %* the same way). Changes made after a restore persist across runs until the next restore. Other baselines are kept.

**Delete a baseline.** Press **Delete…** and confirm with **Delete baseline**: the files are removed from the host, the VM's current state does not change. The **Current**, **In use** and **Protected** baselines cannot be deleted (the button is disabled and its tooltip says why): restore another baseline first. Deleting the VM deletes its baselines too.

If an operation is refused, the page says why in plain words, for example:

| Message (beginning) | What to do |
| --- | --- |
| *the VM was not shut down cleanly last time* | Shut it down from the guest and retry, or give the consent (above). |
| *the VM is busy* | A start, a stop or another baseline operation is in progress, or a disk is open in another program: wait and retry. |
| *not enough free space on the catalog drive* / *it would exceed the VM's storage budget* | Free space on the drive, delete older baselines, or raise **Storage budget** in **Edit**. LightVM keeps 1 GiB free in reserve. |
| *its files were changed or are missing on the host* | The baseline was altered outside LightVM: LightVM will not use it. Restore another baseline or delete this one. A VM whose current baseline is altered does not start. |
| *cancelled by the Emergency Stop* | Nothing was changed; start the operation again if you want. |

---

## 11. Snapshots: named points of the VM's disk

A **snapshot** is a named point of the VM's **disk**, as in VMware: take one any time (the VM off or running), revert to it, delete it, and the other snapshots are kept. The snapshots of a VM form a **tree**: the parent of a snapshot is the snapshot its disk state descended from (the last one taken or reverted to), and the page shows where you are now.

**A snapshot never saves memory (RAM).** It holds the disk state, a copy of the UEFI variables (when the VM has UEFI firmware) and the VM's configuration at that moment. Reverting to a snapshot restarts the guest from the saved disk state with a **normal boot**: it is not a return to the exact instant of the take. A snapshot taken **while the VM runs** is like a **power cut at that instant**: the disk is saved exactly as the guest left it (crash-consistent; a guest program that had data not yet written to disk loses it), and a thumbnail of the screen is kept with the snapshot.

Open it from the Dashboard: **More** → **Snapshots…** on the VM's row. (If the item is disabled, the supervisor has no QEMU disk image tool: check the QEMU folder in **Settings**.) The page shows:

- a summary: the **VM state**, **You are here** (*the state descends from "…"*, or — when no snapshot was ever taken or reverted to), the number of snapshots with the space of their own files, and **Memory: never saved**;
- the list of snapshots, oldest first, each with its tree indent (↳, *taken on "…"* on hover), its name and badges:

| Badge | Meaning |
| --- | --- |
| **You are here** | The VM's current state descends from this snapshot (it is the last one taken or reverted to). |
| **While running** | Taken while the VM ran: like a power cut at that instant, the disk is crash-consistent. |
| **Clean** | Taken with the VM off, after an observed clean shutdown of the guest (or before any run). |
| **Firmware saved** | A copy of the UEFI variables is kept with the snapshot. |

Each row also shows its creation time (UTC), its description, the VM's disk size and the space the snapshot's own files take. The same facts appear in one line under the VM's row on the Dashboard (for example *2 snapshots · you are here: "Snapshot 1 · 2026-10-15 10:12 UTC"*).

**Take a snapshot.** Press **Take snapshot…**: the dialog shows the name LightVM gives it (*Snapshot N · date and time UTC*) and an optional description; **Take snapshot** starts the operation. With the VM off, LightVM saves the disk state exactly as it is, with a copy of the UEFI variables. With the VM running, the disk is snapshotted exactly as the guest left it (as above), with a thumbnail of the screen. The page shows *Taking the snapshot…* with the elapsed time; meanwhile the VM cannot be started or changed (its **Start** button is disabled and its row shows *Taking snapshot*). There is no time limit and no Cancel button: only the **Emergency Stop** (top right) cancels the operation, and then nothing is changed.

**Revert to a snapshot.** A revert needs the VM **off**. With the VM stopped, press **Revert to…** on a snapshot and confirm with **Discard and revert**: the current state is **discarded** — everything written to the disk after the snapshot, and the UEFI variables changed after it, are lost — and replaced by the saved state; other snapshots are kept. **This cannot be undone.** To keep the current state, take a snapshot of it first. The next **Start** boots the guest from the saved disk state with a normal boot.

If the VM is **running** when you press **Revert to…**, the dialog says *The VM is running, so first:* and the confirm button reads **Shut down and revert**. Tick **Shut down "…" normally first (a revert needs the VM off)** — LightVM never forces the VM off behind your back — and, if you want to keep the current state, **Before that, take a snapshot of the current state ("Snapshot N · …")**. Then the page shows the flow: the snapshot of the current state is taken, the VM is shut down normally, and the revert is applied. If the snapshot of the current state fails, the VM keeps running and the revert does not happen.

**Delete a snapshot.** Press **Delete…** and confirm with **Delete snapshot**: it is removed from the VM's disk; the other snapshots keep their state, and the snapshot the VM's current state descends from stays usable. **This cannot be undone.**

**Rename a snapshot.** **Rename…** changes only the name and the description: nothing changes on the disk.

During a snapshot operation the VM's row and the page show *Taking snapshot*, *Reverting snapshot* or *Deleting snapshot*. If an operation is refused, the page says why in plain words (for example *the VM is running, it must be stopped* for a revert, *not enough free space on the catalog drive*, *it would exceed the VM's storage budget*).

Snapshots are a manual feature: agent sessions never take, revert or delete them, and none happens while an agent session uses the VM. Deleting the VM deletes its snapshots too.

---

## 12. Agent sessions

An **agent session** lets an AI agent (for example an MCP client running on this PC) look at and use **one** VM, for a limited time, with the actions you allow. Nothing happens without you: the agent can only *ask*, and you approve or deny in LightVM. Open the **Agents** page from the side bar.

> LightVM limits what an agent can do **through LightVM**. It does not limit what the agent's own app can already do on this computer (files, network, other programs).

**1. Agent access.** The switch **Allow agents to request sessions** is **off every time LightVM starts**. While it is off, agents cannot even ask. Turn it on only when you want to use an agent. Turning it off again denies the requests still waiting and ends the active sessions at once (the VMs keep running).

**2. Pair a client.** With agent access on, press **Pair a client…** → **Write pairing file**. LightVM writes a pairing credential, valid for **10 minutes**, to `%LOCALAPPDATA%\LightVM\pairing\pairing.json`, a folder that only your Windows user account can read. The LightVM MCP adapter reads it from there. In the AI client, add a local (stdio) MCP server whose command is `lightvm-mcp.exe` in the installation folder (by default `%LOCALAPPDATA%\Programs\LightVM\lightvm-mcp.exe`), with no arguments: the credential is never passed on the command line. The credential is never shown on screen, never copied to the clipboard and never written to the logs, and on its own it gives **no access to any VM**. The file is removed when you turn agent access off, after an Emergency Stop and when LightVM closes; a new pairing replaces the previous one.

**3. A request arrives.** The Agents page (and a blue bar at the top of every other page, with **Review**) shows the request: the client's name and its stated purpose (exactly as the agent wrote them), what it asks for, how long the request stays valid (5 minutes), and a large **verification code** such as `482-913`. The agent shows the same code on its side. **If the codes differ, press Deny**: someone else is asking.

**4. Approve or deny.** **Deny** tells the agent no. **Review and approve…** opens the choices (there is nothing to type):

| Choice | What it means |
| --- | --- |
| **Virtual machine** | Exactly one VM. Every other VM stays out of the agent's reach. If the agent asked for one VM, it is preselected. |
| **What it may do** | Tick only what is needed: see the VM in its list, read its state, see the screen (screenshots), type and use the mouse, start it, stop or force-stop it, reset it. Stop and reset are marked **Can lose unsaved work** and are never ticked for you. Anything not ticked is refused by the supervisor. |
| **Network** | Shown when the VM has network endpoints (section 9.2): tick the ones the agent may reach inside the guest. None is ticked for you; with none ticked the VM runs **without network** for the session. If the agent asked for some, the request says which. A VM running with NAT must be stopped first. |
| **Keyboard and mouse** | **Give keyboard and mouse to the agent now**: only one of you types at a time (see below). You can also hand them over later. |
| **Duration** | 15 min, 1 hour, 4 hours or 8 hours; the session then ends by itself. |
| **Limits** | Small, Standard or Large: how many actions, input events and screenshots the agent may use in the session. |
| **If the agent stops answering** | 10, 5 or 3 seconds. An agent that stops checking in for this long loses the session **and its VM is stopped**. |

Then tick **The agent shows the code …** (after comparing it) and press **Approve session**. If LightVM answers that another session is active, end that one first: one session at a time.

**5. While a session is active**, an orange bar at the top of **every page** (also over the full-screen console) says *Agent session active on <VM> · <client> · keyboard and mouse: agent/you · time left*, with:

- **Pause** / **Resume**: while paused the agent can still look (status, screenshots) but every action it tries is refused; the VM keeps running.
- **Take control** / **Give control**: keyboard and mouse belong either to you or to the agent, never both. While the agent has them, the console shows *Input controlled by the agent …* and nothing you type or click is sent to the VM; **Take control** gives them back at once (keys the agent was holding are released).
- **Revoke…**: ends the session at once after a confirmation; the agent's connection is closed and it needs a new approval to come back. Actions it already sent may still complete. The VM keeps running.
- **Emergency Stop**: stops **all** VMs and ends every session immediately. It stays clickable even while a dialog is open.
- **Details**: the session page with the stated objective, the state and its controls, the session report (see below) and the session events this app saw while it was open (request, start, pause, keyboard handover, end). Every action of the agent is also recorded in the audit log (section 13).

**Giving an agent its task.** The adapter carries its own reference documentation: its `help` tool describes how sessions work (request, verification code, approval), every tool with its parameters, key names, error codes and the limits below, so the agent does not need any other documentation. It only describes and never gives the agent instructions: what to do is decided by your prompt alone. With Claude Code, for example, register the adapter once:

```
claude mcp add lightvm -- "%LOCALAPPDATA%\Programs\LightVM\lightvm-mcp.exe"
```

(write the full path if your shell does not expand `%LOCALAPPDATA%`), pair the client (step 2) and give a prompt such as: *"Use the lightvm MCP server (its help tool documents how it works). Task: … (the VM, what to do, how to check it, the guest login if needed)."* Before handing over a task, keep in mind that shared folders and clipboard are off unless you enable Tools and grant the corresponding session capabilities, and there is **no network** unless you grant it network endpoints (the agent's `vm_status` then lists their names, addresses and ports): without Tools grants, the agent works through the screen, the mouse and the keyboard, typing any code or file content into the guest. It types with the **US keyboard layout**, so the guest should use the US layout; and tick only the capabilities the task needs.

**6. The AI Workspace.** The Agents page is titled **AI Workspace**. From the top:

- **Current session**: the session running now, with the client, the VM, the time left, the **objective** the agent stated, **So far** (actions, input events and screenshots used against the limits) and the last five actions recorded by LightVM. **Show the agent's latest screenshot** shows the last screenshot the agent took (re-read every few seconds while shown); **Hide** removes it. It is never shown unless you press the button, and never saved by the app. Below are the same controls as the bar at the top.
- **Session requests** and **Agent access**, as described above.
- **Session history**: every session report kept by LightVM, newest first (also after LightVM restarts), with the client, the VM, start and end time in UTC, the reason it ended, the number of actions and screenshots, and the **outcome** with where it comes from. **Report** opens it.

**7. Session reports and outcomes.** LightVM (not the agent) writes a report for every approved session: the objective, the VM and what was allowed, the limits, the policy revision, the versions of LightVM and of the MCP protocol, the backend and accelerator, the baseline the VM started from, every call the agent made (refused ones included), the results of its tools, its screenshots by SHA-256, errors and how the session ended. Text the agent typed into the VM is never stored: only its length and SHA-256.

The **outcome** is one of **Passed**, **Failed**, **Inconclusive**, **Cancelled**, **Timed out** and **Infrastructure error**, always with its source:

| Source | Meaning |
| --- | --- |
| *pending: session running* | The session is still live; the outcome reads inconclusive until it ends. |
| *from how the session ended* | Default: ended by the agent, disconnected or baseline restored → Inconclusive; revoked, Emergency Stop or LightVM closed → Cancelled; time limit or lease expired → Timed out; the VM or the supervisor stopped → Infrastructure error. Finishing the conversation never gives Passed. |
| *by the automatic verifier* | The result of **Run automatic check…** (below). |
| *your verdict* | The verdict you set, recorded as decided by *local user* with the time. |

The report page also shows the **Agent's declaration**: what the agent says about its own result (for example *The agent claims: passed* and its summary). It is clearly marked as a claim: **it never sets the outcome**.

On the report page:

- **Set verdict…**: choose *Passed*, *Failed* or *Inconclusive* and press **Record verdict**. Your verdict replaces the current outcome; earlier verdicts stay in the **Verdict history**. There is nothing to type.
- **Run automatic check…**: for LightVM test VMs that show the test fixture screen, checks the agent's last stored screenshot for the reference blocks and the marker (choose whether the marker should be on screen or not). The result replaces the outcome and is attributed to the verifier. On any other screen the result is failed or inconclusive.
- **Actions recorded**: the events in order, 50 at a time (**Previous** / **Next**). A stored screenshot has a **Show** button: the screenshot is read only then, checked against its SHA-256 (an altered file is not shown), kept in memory and never saved by the app; **Hide** removes it.

**8. Export.** **Export…** → choose **Markdown** (to read) or **JSON** (the canonical report, for tools) and, if you want, **Also save the stored screenshots in a folder next to it** (screenshots can show anything that was on the guest screen, passwords included) → **Choose where to save…** opens the Windows save dialog. Only you choose where the file goes; agents can never export. If a file with that name already exists, LightVM asks before replacing it (**Replace the file**). When it is done, LightVM shows where it wrote the report, its size and SHA-256, and how many screenshots it saved in the folder `<name>-screenshots`.

**9. Retention and deletion.** Reports and their screenshots are kept in the VM catalog (`sessions\<session>\` inside the catalog folder) until you delete them. At most 300 screenshots (512 MiB) are stored per session; later ones keep only their hash (*not stored*). The 100 most recent reports are kept; older ended ones are removed automatically. **Delete report…** (ended sessions only) removes the report **and its stored screenshots** after a confirmation; copies you exported are not touched. Screenshots cannot be deleted on their own: delete the report, or export it first if you need to keep a copy.

**If LightVM itself stops responding** while an agent works, run in a terminal `& "$env:LOCALAPPDATA\Programs\LightVM\lightvm-service.exe" --emergency-stop --catalog "$env:LOCALAPPDATA\LightVM\catalog"` (PowerShell; use your catalog folder if you moved it): it stops every VM and agent session of that catalog without going through the app.

---

## 13. Where your data lives

| What | Where |
| --- | --- |
| Program files | `%LOCALAPPDATA%\Programs\LightVM` (or the `-Prefix` you chose) |
| VM catalog: database, VM disks, UEFI variables, baselines, copies of images imported by earlier versions (ISOs imported now stay where you keep them) | `%LOCALAPPDATA%\LightVM\catalog` (or the folder chosen in **Settings** → **VM catalog**) |
| App settings | `%LOCALAPPDATA%\LightVM\ui-settings.json` |
| QEMU used by the supervisor | The folder chosen in **Settings** → **QEMU distribution**; if none is chosen, the one LightVM finds by itself (`%LOCALAPPDATA%\Programs\qemu-11.1.0`, `C:\Program Files\qemu` or a folder in `PATH`, shown as *(found automatically)*, section 2.3) |
| Audit log of the supervisor | `%LOCALAPPDATA%\LightVM\audit.jsonl` |
| Pairing file for an agent client (only after **Pair a client…**, removed when agent access is turned off, after an Emergency Stop and when LightVM closes) | `%LOCALAPPDATA%\LightVM\pairing\pairing.json` |
| Diagnostic logs | `%LOCALAPPDATA%\LightVM\logs\lightvm-desktop.log` (the app) and `lightvm-service.log` (the supervisor); each is rotated at 1 MiB, keeping three older copies (`*.1.log` … `*.3.log`) |

`%LOCALAPPDATA%` is usually `C:\Users\<you>\AppData\Local`. Do not edit or move files in the catalog by hand while LightVM is running; LightVM keeps a journal to recover from interruptions, but manual changes are not supported. To back up VMs, close LightVM and copy the whole catalog folder.

The catalog folder holds everything about your VMs: `catalog.sqlite3` (the database), `media\` (copies of images imported by earlier versions; ISOs imported now are used where they are and are not in the catalog), `vms\<VM id>\` (disk `primary.qcow2`, baselines in `base\`, overlays in `overlays\`, UEFI variables in `firmware\`, TPM state in `tpm\`, snapshots in `snapshots\` — their UEFI variables copy and thumbnail; the snapshot's disk state is inside the disk file itself), `runs\`, `sessions\` (agent session reports) and `tmp\`.

### 13.1 Move the VMs to another folder or drive

**Settings** → **Supervisor** → **VM catalog** → **Move catalog…** moves the whole catalog, with every VM, disk, baseline, image copy and session report, and makes LightVM use the new folder. ISOs used where they are (section 9.1) are outside the catalog: they are not moved or copied, and the VMs keep using them from their place.

1. Stop every VM. The button is disabled while a VM runs (hover over it for the reason), while the supervisor is restarting, when a changed catalog folder has not been applied yet, and when LightVM was started from the command line with `--catalog`.
2. Choose the destination folder. An empty (or new) folder receives the catalog directly; a folder that already contains files gets a `catalog` subfolder, so nothing of yours is mixed with it.
3. Read the confirmation and press **Move catalog**. Tick **Keep the original folder** to leave a full copy in the old place (you delete it yourself later).
4. LightVM stops the supervisor and moves the catalog, showing the progress. On the same drive the folder is simply renamed. On another drive every file is copied, read back and checked (size and SHA-256), and the new catalog is opened, before the old folder is deleted. **Cancel** stops the move.
5. LightVM saves the new folder in its settings, starts the supervisor again and shows the outcome.

If the move fails or you cancel it, the partial copy is removed and the catalog stays, unchanged, where it was. The move is refused when the destination is inside the catalog (or contains it), is not empty, or lacks free space for the catalog plus 1 GiB, and when the catalog has an operation still to be resolved. The new folder gets the permissions of the folder that contains it: choose a place only your user account can read (on Windows, inside your user profile or a folder whose permissions you restricted).

Without the app (for example with LightVM started with `--catalog`), close LightVM and run `lightvm-service.exe --move-catalog --catalog <current folder> --to <new folder>` (add `--keep-source` to keep the original); then start LightVM with the new folder.

---

## 14. Uninstall

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File "$env:LOCALAPPDATA\Programs\LightVM\uninstall.ps1"
```

Close LightVM first (and stop its VMs). The uninstaller removes exactly the files listed in `install-manifest.json` (plain file names inside the installation folder only), the Start Menu shortcut (only from your Start Menu **Programs** folder) and the `PATH` entry if `-AddToPath` was used (the other entries are left untouched). **Your VMs, catalog, settings and logs in `%LOCALAPPDATA%\LightVM` are kept.**

To delete them as well (all VMs and disks, irreversible): add `-RemoveUserData` (asks for confirmation). A catalog moved elsewhere in **Settings** is never removed by the script. QEMU is uninstalled separately (delete the extracted folder, or use *Installed apps* for options B and C).

If you used the setup `.exe`, uninstall from **Settings → Apps → Installed apps → LightVM**; it also keeps your VMs and settings.

---

## 15. Known limitations of the alpha

- **Experimental software.** Phase F2 "alpha manuale"; not a release. Feasibility items of phase F0 are still open (physical keyboard test P-4, second-user test on Windows P-5, see `docs/STATUS.md`).
- **Windows 11 x64 only**, tested on a single PC (`docs/COMPATIBILITY.md`). Linux and macOS are not supported (the macOS build in CI only checks portability).
- **Not code-signed** (ADR-0020): SmartScreen warnings are expected; rely on the checksums.
- **QEMU not included** and only 11.1.0 accepted; its own code signature is expired.
- **Network**: Offline by default; **NAT** for manual use and an **endpoint allowlist** (section 9.2). The allowlist has been verified with a Linux guest only; reaching a WCF service and SQL Server from a Windows guest is not verified yet. Only TCP is carried through the allowlist.
- Clipboard and shared folders are **off by default**. Optional experimental
  Tools access requires VM settings and separate session grants; approved
  read-only data discs remain available without Tools.
- **Keyboard**: Italian and English (US) layouts are the targets; other layouts and IME input are not verified.
- **CPU model**: the VM uses a generic CPU model, not the host's (`-cpu host` does not boot on the test PC), so some guest software may run slower or detect fewer CPU features.
- **One reference guest**: Ubuntu 26.04.1 LTS Desktop. Other guests are not tested. Windows XP, Vista, 7, 8.1 and 10 VMs can be created (section 6.1); the project has started Windows Setup from Vista 32-bit and Windows 10 images but has not completed an installation yet; Windows Vista 64-bit does not run on Windows PCs; Windows 8 and 11 are not supported.
- **Importing VMware VMs** (section 6.3) is not verified with real VMware VMs yet; Windows VMs on LSI Logic SCSI disks need a manual preparation in VMware before the import.
- Baselines are taken **with the VM off** only (section 10). Snapshots (section 11) can be taken with the VM running, but a snapshot **never saves memory**: it holds the disk state only, and a snapshot taken while the VM runs is crash-consistent, like a power cut at that instant. Baseline labels are generated by LightVM and cannot be renamed. Only one baseline or snapshot operation at a time across all VMs.
- Agent sessions (section 12): the MCP adapter `lightvm-mcp.exe` is included, but it has been verified only with a scripted test client: no real AI client (LLM) has been run against it yet, so client setup details may change. One agent session at a time; sessions never survive a restart of LightVM.
- No console window: diagnostic messages are only in the log files (section 13).
- Remote Desktop (RDP) sessions are not verified.

---

## 16. Troubleshooting

| Symptom | Cause and fix |
| --- | --- |
| **QEMU distribution** shows *not set: VMs cannot be started* | LightVM found no QEMU in the usual folders (section 2.3). Select the QEMU folder in **Settings** and restart the supervisor (section 4). |
| Dashboard notice *QEMU was not found: VMs cannot start* and **Start** disabled | The supervisor was started without a QEMU folder. **Open Settings** → **QEMU distribution** → **Browse…**, then restart the supervisor. If LightVM was started from the command line with `--catalog` but without `--qemu-dir`, the notice says so instead: start LightVM again adding `--qemu-dir` with the QEMU folder. |
| Supervisor error about the QEMU hash or version | The folder does not contain the approved QEMU 11.1.0 files. Verify with section 2.3; reinstall from the verified installer. |
| **Acceleration** reports WHPX not available / VM does not start with an accelerator error | Virtualization disabled in the firmware, or Windows Hypervisor Platform off (section 1). Restart Windows after enabling it. LightVM never uses software emulation instead. |
| *Windows protected your PC* | Unsigned app (expected). Verify the zip checksum, then **More info** → **Run anyway**. |
| `install.ps1` cannot be loaded / "running scripts is disabled" | Run it as shown, with `powershell -NoProfile -ExecutionPolicy Bypass -File .\install.ps1`. |
| `install.ps1`: *Checksum mismatch* | Damaged download or extraction: download the zip again and verify it. |
| Wrong characters when typing in the VM | The Ubuntu keyboard layout differs from your physical keyboard: change it in Ubuntu (**Settings → Keyboard**). |
| The Ubuntu installer starts again after installation | The installation image is still attached: see section 8, step 2. |

Diagnostic messages (start of the supervisor, rendering backend, errors) are written to `%LOCALAPPDATA%\LightVM\logs\lightvm-desktop.log`, and the supervisor's own messages to `lightvm-service.log` in the same folder; older copies are `*.1.log` … `*.3.log`. They contain no passwords, tokens, typed keys or guest contents. The supervisor's actions are recorded in `%LOCALAPPDATA%\LightVM\audit.jsonl` (no passwords or guest contents). Running `lightvm-desktop.exe` from a terminal with its output redirected (for example `cmd /c lightvm-desktop.exe > out.txt 2>&1`) also captures the messages there.
