# LightVM user guide (alpha)

This guide takes you from a Windows 11 PC to an Ubuntu virtual machine (VM) installed with LightVM, and back to it after you close and reopen the app. It covers the **experimental alpha** (roadmap phase F2): read [Known limitations](#14-known-limitations-of-the-alpha) before you start.

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
11. [Agent sessions](#11-agent-sessions)
12. [Where your data lives](#12-where-your-data-lives)
13. [Uninstall](#13-uninstall)
14. [Known limitations of the alpha](#14-known-limitations-of-the-alpha)
15. [Troubleshooting](#15-troubleshooting)

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

3. The package contains: `lightvm-desktop.exe` (the app), `lightvm-service.exe` (the supervisor, started by the app; the two files must stay in the same folder), `lightvm-mcp.exe` (the MCP adapter that an AI client starts for agent sessions, section 11; it does nothing on its own), `README.md`, `USER_GUIDE.md` (this guide), `LICENSE.txt`, `THIRD-PARTY-LICENSES.txt`, `SBOM.cdx.json` (software bill of materials: every component built into the three programs, with versions, licences and hashes, CycloneDX format), `BUILDINFO.txt` (commit and toolchain of the build), `SHA256SUMS.txt`, `install.ps1`, `uninstall.ps1`. The package has no test or measurement options: the programs are built without them and the build checks that none is left.

If a `LightVM-<version>-windows-x64-setup.exe` (Inno Setup, per user) is published next to the zip, it installs the same files into the same folder with a normal wizard; verify its `.sha256` the same way. It is equally unsigned.

---

## 4. First start and QEMU setup

1. Start **LightVM** from the Start Menu.
   - On the first run, Windows SmartScreen may show *Windows protected your PC* because the app is not signed. If, and only if, you verified the checksum in section 3, choose **More info** → **Run anyway**.
   - LightVM opens without a console window. Its diagnostic messages go to log files in `%LOCALAPPDATA%\LightVM\logs` (see [Troubleshooting](#14-troubleshooting)).
2. LightVM opens on the **Dashboard**. If you installed QEMU with option A, B or C of section 2, LightVM has already found it (section 2.3). Otherwise the Dashboard shows the notice *QEMU was not found: VMs cannot start. Choose the QEMU folder in Settings → QEMU distribution, then restart the supervisor.*, every VM's **Start** button is disabled with the same reason, and **Open Settings** under the notice leads to step 3.
3. Open **Settings** (left navigation). In the **Supervisor** section:
   - **QEMU distribution** shows the folder in use. *(found automatically)* after it means LightVM found it by itself: nothing to do. *not set: VMs cannot be started* means it found none: **Browse…** → select the QEMU folder from section 2 (the one that contains `qemu-system-x86_64.exe`). A folder chosen here takes precedence over the one found automatically.
   - **VM catalog** shows where VMs are stored (default `%LOCALAPPDATA%\LightVM\catalog`). Leave the default unless you need another drive. **Browse…** and **Default** only switch LightVM to another catalog folder: VMs already created stay in the old one. To take them along use **Move catalog…** (section 12.1).
   - Press **Apply and restart the supervisor** (or **Start the supervisor** if it is not running).
4. Back on the **Dashboard**, the supervisor card at the top (**Supervisor · PID …**) shows the connection state, **Backend**, **Acceleration** and **If the UI closes**. **Acceleration** must read `whpx`; *not detected* means VMs cannot use the hardware accelerator. **Settings** → **Host capacity** → **Hardware acceleration** says the same. If it does not work, see [Troubleshooting](#14-troubleshooting).

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
2. **Name**: type a name for the VM, for example `ubuntu`. This is the only text field in LightVM; the name is used only inside LightVM. The wizard says when the name is not valid or already used.
3. **Installation media**: press **Import ISO…** and select `ubuntu-26.04.1-desktop-amd64.iso` in the file picker. LightVM **copies** the image into its catalog and records its SHA-256; the original file is not modified and you can delete it afterwards. Importing a 6 GiB image takes a while. Images imported earlier are listed and can be chosen again for other VMs.
4. **System**: LightVM looks inside the image you chose and preselects the system it detects. Check it and change it if it is wrong. If it does not recognize the image, nothing is preselected: choose the system yourself; **Next** stays disabled until you do (*Choose the system to continue*). **Ubuntu LTS (x86-64)** is the reference guest; **Other Linux (x86-64)** is listed but not tested by the project. Windows systems are described in [section 6.1](#61-create-a-windows-vm).
5. **Data media** (optional): keep **No data media**. (A data ISO, imported with **Import data ISO…**, is attached to the VM as a read-only CD; it is not needed for the installation.)
6. **Resources**: **Virtual processors**, **Memory** and **Disk**. The defaults (2 vCPUs, 4 GiB, 32 GiB disk) are the standard template for Ubuntu LTS, shown as *Template: Ubuntu LTS — 2 vCPU, 4 GiB RAM, 32 GiB disk*, and suit Ubuntu Desktop; if you change them, **Reset to template** puts them back. The wizard does not offer more than the host can give. The disk file grows on the host only as the guest writes data.
7. **Network**: only **Offline: no network adapter** is available in this alpha (NAT and bridged networking are shown as not available).
8. **Summary**: check the values (for Ubuntu the firmware is always **UEFI**) and press **Create**.

The new VM appears on the Dashboard as **Stopped**, with system, CPU, memory, virtual disk size and space used on the host.

### 6.1 Create a Windows VM

LightVM can also create VMs for **Windows XP, Vista, 7, 8.1 and 10**, with the same wizard. **This is experimental: the project has started Windows Setup from Vista 32-bit and Windows 10 images, but has not yet completed a Windows installation** (`docs/COMPATIBILITY.md`, "Windows guest profiles"). **Windows 11 (x86/x64) and Windows 8 are not supported** on Windows and Linux hosts: Windows 11 needs TPM and Secure Boot, which LightVM does not provide there and does not work around; Windows 8 is replaced by 8.1. On a Mac with Apple Silicon only Windows 11 ARM64 is offered (section 6.2).

What you need:

- **Your own Windows installation image** (ISO) and **a valid licence (product key)**. LightVM does not include Windows, does not provide keys and does not handle activation: that is your responsibility, under the terms of your licence.
- Enough disk space for the system: the wizard suggests a size for each Windows version and does not accept less than its minimum.

Steps (only what differs from section 6):

1. **Installation media**: import the Windows ISO with **Import ISO…**.
2. **System**: LightVM reads the image and shows what it found, for example *Detected: Windows 7 (64-bit, build 7601)*. If it shows *Not recognized: choose the system*, nothing is selected and you must pick the system in the list before going on; if the suggestion is wrong, pick the right one. The detection is only a suggestion; your choice is what counts. An image of Windows 8 or of Windows 11 for x86/x64 is not recognized, and those systems are not in the list. An image for another architecture (for example Windows 11 ARM64 on a Windows PC) is shown as *Detected: … — not supported on this host* and nothing is selected. **Windows Vista 64-bit does not start on a Windows PC** (its setup stops with a blue screen under the Windows hypervisor): its image is shown as *not supported on this host*, the wizard explains why and does not go on with it; use the 32-bit Vista image.
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

---

## 7. Install Ubuntu in the VM

1. On the Dashboard, press **Start** on the VM. The state goes to **Starting**, then **Running**.
2. Press **Console** on the VM (or open the **Console** page in the navigation and press **Open console**). The guest screen appears inside LightVM once the console shows **Connected**; click on it to capture keyboard and mouse for the VM, as in VMware (a coloured border shows the capture). That first click only captures: it does not reach the guest. While captured, the Windows pointer stays inside the LightVM window and is hidden over the guest screen (the guest draws its own), and every key goes to the VM. Before capture nothing reaches the VM, not even the pointer position.
   - **Ctrl+Alt** (press both and let go, with no other key) releases keyboard and mouse. Switching to another window releases them too; click the console again to capture them. Only the **left** Alt counts: AltGr (used to type `@`, `#`, `[` on an Italian keyboard) never releases, and neither does Ctrl+Alt followed by another key.
   - **Ctrl+Alt+Esc** also releases keyboard and mouse, and leaves full screen.
   - **Ctrl+Alt+Enter** switches full screen on and off; the **Full screen** / **Exit full screen** button in the console toolbar does the same. Neither shortcut reaches the guest.
   - **Screenshot** saves the current guest screen as a PNG file (you choose where).
   - **Guest keyboard layout** (toolbar, **US** or **IT**; also in **Settings** → **Console**): set it to the layout configured inside the guest.
   - Clicks on LightVM's own menus and dialogs never reach the guest, even when they are drawn over the guest screen.
   - There is **no clipboard** between Windows and the VM (by design in the MVP).
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
| **More** → **Delete…** | Removes the VM and its disk after a confirmation. Only possible while the VM is stopped (otherwise: *Stop the VM to delete it*). **Cannot be undone.** Imported installation images stay in the catalog (to remove them see section 9.1). | VM no longer needed. |

Resources and execution limits (**Virtual processors**, **Memory (RAM)**, **Disk**, **Maximum CPU**, **Memory margin**, **Storage budget**) are changed with **Edit** → **Save settings**, only while the VM is **Stopped**; the disk can only grow.

Closing LightVM while VMs are running opens a dialog: **Stop them and quit** shuts the VMs down in an orderly way (terminating those that do not stop within the timeout) and then closes LightVM; **Cancel** keeps everything running. VMs are not left running in the background. If the VMs stop by themselves while the dialog is open, LightVM closes without asking further. If the supervisor has already stopped or lost its connection, LightVM closes at once: the VMs it was running follow the **If LightVM closes unexpectedly** setting.

### 9.1 Delete imported ISOs

Every ISO imported in the wizard (installation and data media) is a read-only copy in the catalog, so it takes as much disk space as the original. **Settings** → **Installation media** lists them: name, **Installation** or **Data**, size, the system LightVM detected in it, when it was imported and which VMs use it (*used by "web-test"* or *not used by any VM*).

To free the space, press **Delete…** next to an image and confirm with **Delete medium**. LightVM removes its copy from the catalog; the original file you imported it from is not touched, and you can import it again later. **Cannot be undone.**

An image that a VM uses as installation or data media cannot be deleted: **Delete…** is disabled and its tooltip names the VMs. Remove it first with **Edit** → **Installation media** (or **Data media**) → **Remove** → **Save settings**, or with **Remove installation media…** on the Dashboard, or delete the VM. If the supervisor still refuses (for example a VM started using it from another window meanwhile), the dialog stays open and says why: *The medium was not deleted: it is used by 1 VM: "…"*, or *a running VM or another operation holds it open*. Agent sessions can never delete media. If LightVM is interrupted during a deletion, it finishes or undoes it the next time it starts.

---

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

## 11. Agent sessions

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
- **Details**: the session page with the stated objective, the state and its controls, the session report (see below) and the session events this app saw while it was open (request, start, pause, keyboard handover, end). Every action of the agent is also recorded in the audit log (section 12).

**Giving an agent its task.** The adapter carries its own reference documentation: its `help` tool describes how sessions work (request, verification code, approval), every tool with its parameters, key names, error codes and the limits below, so the agent does not need any other documentation. It only describes and never gives the agent instructions: what to do is decided by your prompt alone. With Claude Code, for example, register the adapter once:

```
claude mcp add lightvm -- "%LOCALAPPDATA%\Programs\LightVM\lightvm-mcp.exe"
```

(write the full path if your shell does not expand `%LOCALAPPDATA%`), pair the client (step 2) and give a prompt such as: *"Use the lightvm MCP server (its help tool documents how it works). Task: … (the VM, what to do, how to check it, the guest login if needed)."* Before handing over a task, keep in mind that the VM has **no network, no shared folders and no clipboard**: the agent works only through the screen, the mouse and the keyboard, typing any code or file content into the guest. It types with the **US keyboard layout**, so the guest should use the US layout; and tick only the capabilities the task needs.

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

## 12. Where your data lives

| What | Where |
| --- | --- |
| Program files | `%LOCALAPPDATA%\Programs\LightVM` (or the `-Prefix` you chose) |
| VM catalog: database, VM disks, UEFI variables, baselines, imported images | `%LOCALAPPDATA%\LightVM\catalog` (or the folder chosen in **Settings** → **VM catalog**) |
| App settings | `%LOCALAPPDATA%\LightVM\ui-settings.json` |
| QEMU used by the supervisor | The folder chosen in **Settings** → **QEMU distribution**; if none is chosen, the one LightVM finds by itself (`%LOCALAPPDATA%\Programs\qemu-11.1.0`, `C:\Program Files\qemu` or a folder in `PATH`, shown as *(found automatically)*, section 2.3) |
| Audit log of the supervisor | `%LOCALAPPDATA%\LightVM\audit.jsonl` |
| Pairing file for an agent client (only after **Pair a client…**, removed when agent access is turned off, after an Emergency Stop and when LightVM closes) | `%LOCALAPPDATA%\LightVM\pairing\pairing.json` |
| Diagnostic logs | `%LOCALAPPDATA%\LightVM\logs\lightvm-desktop.log` (the app) and `lightvm-service.log` (the supervisor); each is rotated at 1 MiB, keeping three older copies (`*.1.log` … `*.3.log`) |

`%LOCALAPPDATA%` is usually `C:\Users\<you>\AppData\Local`. Do not edit or move files in the catalog by hand while LightVM is running; LightVM keeps a journal to recover from interruptions, but manual changes are not supported. To back up VMs, close LightVM and copy the whole catalog folder.

The catalog folder holds everything about your VMs: `catalog.sqlite3` (the database), `media\` (imported installation images), `vms\<VM id>\` (disk `primary.qcow2`, baselines in `base\`, overlays in `overlays\`, UEFI variables in `firmware\`, TPM state in `tpm\`), `runs\`, `sessions\` (agent session reports) and `tmp\`.

### 12.1 Move the VMs to another folder or drive

**Settings** → **Supervisor** → **VM catalog** → **Move catalog…** moves the whole catalog, with every VM, disk, baseline, imported image and session report, and makes LightVM use the new folder.

1. Stop every VM. The button is disabled while a VM runs (hover over it for the reason), while the supervisor is restarting, when a changed catalog folder has not been applied yet, and when LightVM was started from the command line with `--catalog`.
2. Choose the destination folder. An empty (or new) folder receives the catalog directly; a folder that already contains files gets a `catalog` subfolder, so nothing of yours is mixed with it.
3. Read the confirmation and press **Move catalog**. Tick **Keep the original folder** to leave a full copy in the old place (you delete it yourself later).
4. LightVM stops the supervisor and moves the catalog, showing the progress. On the same drive the folder is simply renamed. On another drive every file is copied, read back and checked (size and SHA-256), and the new catalog is opened, before the old folder is deleted. **Cancel** stops the move.
5. LightVM saves the new folder in its settings, starts the supervisor again and shows the outcome.

If the move fails or you cancel it, the partial copy is removed and the catalog stays, unchanged, where it was. The move is refused when the destination is inside the catalog (or contains it), is not empty, or lacks free space for the catalog plus 1 GiB, and when the catalog has an operation still to be resolved. The new folder gets the permissions of the folder that contains it: choose a place only your user account can read (on Windows, inside your user profile or a folder whose permissions you restricted).

Without the app (for example with LightVM started with `--catalog`), close LightVM and run `lightvm-service.exe --move-catalog --catalog <current folder> --to <new folder>` (add `--keep-source` to keep the original); then start LightVM with the new folder.

---

## 13. Uninstall

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File "$env:LOCALAPPDATA\Programs\LightVM\uninstall.ps1"
```

Close LightVM first (and stop its VMs). The uninstaller removes exactly the files listed in `install-manifest.json` (plain file names inside the installation folder only), the Start Menu shortcut (only from your Start Menu **Programs** folder) and the `PATH` entry if `-AddToPath` was used (the other entries are left untouched). **Your VMs, catalog, settings and logs in `%LOCALAPPDATA%\LightVM` are kept.**

To delete them as well (all VMs and disks, irreversible): add `-RemoveUserData` (asks for confirmation). A catalog moved elsewhere in **Settings** is never removed by the script. QEMU is uninstalled separately (delete the extracted folder, or use *Installed apps* for options B and C).

If you used the setup `.exe`, uninstall from **Settings → Apps → Installed apps → LightVM**; it also keeps your VMs and settings.

---

## 14. Known limitations of the alpha

- **Experimental software.** Phase F2 "alpha manuale"; not a release. Feasibility items of phase F0 are still open (physical keyboard test P-4, second-user test on Windows P-5, see `docs/STATUS.md`).
- **Windows 11 x64 only**, tested on a single PC (`docs/COMPATIBILITY.md`). Linux and macOS are not supported (the macOS build in CI only checks portability).
- **Not code-signed** (ADR-0020): SmartScreen warnings are expected; rely on the checksums.
- **QEMU not included** and only 11.1.0 accepted; its own code signature is expired.
- **No network** in the VM (Offline only). No updates or software downloads inside the guest.
- **No clipboard** and **no shared folders** between Windows and the VM. Files go into the VM only through an approved read-only data disc.
- **Keyboard**: Italian and English (US) layouts are the targets; other layouts and IME input are not verified.
- **CPU model**: the VM uses a generic CPU model, not the host's (`-cpu host` does not boot on the test PC), so some guest software may run slower or detect fewer CPU features.
- **One reference guest**: Ubuntu 26.04.1 LTS Desktop. Other guests are not tested. Windows XP, Vista, 7, 8.1 and 10 VMs can be created (section 6.1); the project has started Windows Setup from Vista 32-bit and Windows 10 images but has not completed an installation yet; Windows Vista 64-bit does not run on Windows PCs; Windows 8 and 11 are not supported.
- Baselines are taken **with the VM off** only: no snapshot of a running VM, no RAM state (section 10). Baseline labels are generated by LightVM and cannot be renamed. Only one baseline operation at a time across all VMs.
- Agent sessions (section 11): the MCP adapter `lightvm-mcp.exe` is included, but it has been verified only with a scripted test client: no real AI client (LLM) has been run against it yet, so client setup details may change. One agent session at a time; sessions never survive a restart of LightVM.
- No console window: diagnostic messages are only in the log files (section 12).
- Remote Desktop (RDP) sessions are not verified.

---

## 15. Troubleshooting

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
