# F2-03 — Experimental Windows installer, user guide and macOS CI probe

- Execution date (UTC): 2026-10-10
- Executed by: development agent "packaging" (Claude Code session), at the request of the project owner
- Roadmap step: section 10 F2 ("Installer sperimentale e primo test su macchina pulita"; exit criterion "un utente segue la guida…"); section 10 F1 ("build macOS su runner adeguato entro F2 … senza dichiarare supporto operativo")
- Related ADRs: ADR-0010 (QEMU build and source), ADR-0020 (signing not available), ADR-0023 (dependencies and licences), ADR-0011 (guest release)
- Overall outcome: **partial** — package build, per-user install, smoke start from the installed location and uninstall **verified** on the development host (zip + `install.ps1`, and Inno Setup `setup.exe`); clean-machine test **not executed**; macOS CI job **not executed** (YAML validated locally only); user guide written, end-to-end walkthrough by a user **not executed**

Allowed outcomes: **verified**, **failed**, **not executed** (section 17).

## Purpose

1. Produce an experimental per-user Windows package of LightVM without administrator rights, with checksums, licence notices and an uninstaller that keeps the user's VMs.
2. Prove on this host that the package installs, that the installed app starts, and that uninstalling leaves nothing behind.
3. Prepare the clean-machine test (Windows Sandbox) for the project owner.
4. Write `docs/USER_GUIDE.md`, the guide of the F2 exit criterion.
5. Add a non-blocking macOS portability probe to CI.

It does **not** demonstrate: installation on a clean machine; a VM started from the installed package (the smoke uses `--no-supervisor`); that a user can follow the guide to an installed Ubuntu (F2 exit criterion, to be run by the owner); code signing (none, ADR-0020); a macOS build.

## Environment

| Item | Value |
| --- | --- |
| Host | development PC of ADR-0018, Intel Core i9-14900K, NVIDIA GPU (driver 577.00) |
| OS and build | Windows 11 Pro 10.0.26300; Windows PowerShell 5.1.26100.9549 |
| Accelerator and state | not used (no VM started in this test) |
| Toolchain | Rust 1.99.0 / cargo 1.99.0 `x86_64-pc-windows-msvc` (rust-toolchain.toml), MSVC from Visual Studio 2022 Community via `vcvars64.bat` (located with `vswhere`) |
| Installer tools found | Inno Setup **6.5.1** (`C:\Program Files (x86)\Inno Setup 6\ISCC.exe`, already installed); `makensis` (w64devkit) present but not used; WiX not present. Nothing was installed for this test |
| Licence tools | `cargo-about`, `cargo-cyclonedx` not installed; `cargo-deny` not on `PATH` in this session: notices generated from `cargo metadata` + `cargo tree` + licence files in the cargo registry |
| YAML validation | Python 3.12, PyYAML 6.0.3 |
| Source built | commit `2a769fe3eb6a0550829afe0a4757e4e57e913fe2` (HEAD) in a temporary detached `git worktree`, plus the new packaging files and docs of this stream (uncommitted). The shared working tree did not compile at the time (other agents mid-change in `vm-storage`), so the package binaries are HEAD's code, **not** the F2 UI being written in parallel |
| QEMU / firmware / guest | not used |
| Windows Sandbox | not available on this host (`%WINDIR%\System32\WindowsSandbox.exe` absent: optional feature not enabled; enabling needs administrator rights, not done) |

## Deliverables

| File | Content |
| --- | --- |
| `packaging/windows/build-package.ps1` | Builds the two release binaries (`--locked`, static CRT, host paths remapped), generates `THIRD-PARTY-LICENSES.txt`, `BUILDINFO.txt`, `SHA256SUMS.txt`, the zip and its `.sha256`, the optional Inno Setup `setup.exe` and its `.sha256`, and the Windows Sandbox files in `dist/` |
| `packaging/windows/install.ps1` | Per-user installer: verifies the package against `SHA256SUMS.txt`, copies into `%LOCALAPPDATA%\Programs\LightVM` (parameter `-Prefix`), Start Menu shortcut (`-StartMenuDir`, `-NoShortcut`), user `PATH` only with `-AddToPath`, refuses a non-empty prefix without `-Force` and any prefix that is not a LightVM installation, refuses while LightVM runs from the prefix, writes `install-manifest.json` |
| `packaging/windows/uninstall.ps1` | Removes only the files listed in the manifest, the shortcut and the `PATH` entry it added, then the prefix if empty; keeps `%LOCALAPPDATA%\LightVM` unless `-RemoveUserData` (confirmation, or `-Force`) |
| `packaging/windows/lightvm.iss` | Inno Setup script: `PrivilegesRequired=lowest`, `{localappdata}\Programs\LightVM`, user Start Menu shortcut, no `PATH` change, uninstaller removes only installed files |
| `packaging/windows/sandbox/LightVM-sandbox.wsb.template`, `sandbox-test.ps1` | Clean-machine test in Windows Sandbox (dist mapped read-only, networking and clipboard disabled) |
| `docs/USER_GUIDE.md` | User guide (English) |
| `.github/workflows/ci.yml` | `macos-check` job pinned to `macos-14`, documented as portability probe |
| `README.md` | "Install (alpha)" and "Usage" sections |
| `.gitignore` | `/dist/` |

## Procedure

```powershell
# 1. Build (from a detached worktree of HEAD, see Environment)
git worktree add --detach <scratch>\pkgwt HEAD     # + copy of packaging/, docs/USER_GUIDE.md, README.md
powershell -NoProfile -ExecutionPolicy Bypass -File <scratch>\pkgwt\packaging\windows\build-package.ps1 `
    -TargetDir C:\Woarkareas\OpenVM\target\packaging
#   = cargo build --release --locked -p lightvm-desktop -p lightvm-service --bin lightvm-desktop --bin lightvm-service
#     with CARGO_ENCODED_RUSTFLAGS: -Ctarget-feature=+crt-static, --remap-path-prefix=<cargo home>=/cargo,
#     --remap-path-prefix=<repo>=/lightvm, --remap-path-prefix=<target>=/target, -Clink-arg=/PDBALTPATH:%_PDB%
#   then ISCC.exe lightvm.iss (Inno Setup 6.5.1)

# 2. Binary checks
dumpbin /dependents lightvm-desktop.exe lightvm-service.exe   # no vcruntime/msvcp/ucrt DLLs
# ASCII scan of both .exe for "Users\Marco", "Woarkareas", "claude" (build-host paths)

# 3. Zip package test (scratchpad test-package.ps1): temp root under %TEMP%
#    verify zip SHA-256 -> Expand-Archive -> install.ps1 -Prefix <tmp>\prefix\LightVM -StartMenuDir <tmp>\startmenu
#    -> install.ps1 again without -Force (must fail)
#    -> with LOCALAPPDATA=<tmp>\localappdata: <prefix>\lightvm-desktop.exe --smoke 3 --no-supervisor
#    -> <prefix>\uninstall.ps1 (no arguments: prefix taken from the script folder)
#    -> check prefix, shortcut, user PATH, leftover processes; delete temp root

# 4. setup.exe test (scratchpad test-setup.ps1)
#    setup.exe /VERYSILENT /SUPPRESSMSGBOXES /NORESTART /CURRENTUSER /NOICONS /DIR=<tmp>\LightVM /LOG=...
#    -> smoke as in 3 -> unins000.exe /VERYSILENT -> check folder and HKCU uninstall key

# 5. -RemoveUserData and foreign-prefix refusal
#    install.ps1 -Prefix <tmp>\p -NoShortcut; uninstall.ps1 -RemoveUserData -UserDataDir <tmp>\data -Force
#    install.ps1 -Prefix <tmp>\p2 -Force   (p2 contains an unrelated file)

# 6. CI YAML
python -c "import yaml; yaml.safe_load(open('.github/workflows/ci.yml', encoding='utf-8'))"
```

The real `%LOCALAPPDATA%\LightVM` and the real Start Menu were not touched by the zip test (temporary `-StartMenuDir`, `LOCALAPPDATA` redirected for the app run). The `setup.exe` test wrote and then removed the per-user uninstall key `HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\{6C1E7C52-2B7B-4C55-9E4E-6A1D3F0B8E21}_is1` and its user Start Menu shortcut.

## Results

### Artefacts (final build)

| Artefact | Bytes | SHA-256 |
| --- | --- | --- |
| `dist/LightVM-0.0.0-windows-x64.zip` | 10 780 798 | `17d40c92daabb07802da8d5fb75df3003e8261c6cec49b75cae166b3b6e23914` |
| `dist/LightVM-0.0.0-windows-x64-setup.exe` | 9 394 378 | `8fa1d1ebf3805823717a43136a629ee5980906c7cffb742f780b5831dcbef188` |
| `lightvm-desktop.exe` (in both) | | `1dda28a30d4aa71c07285c4124b8cc3cf3200c3a6db8dff4ebb38e920aef6bfe` |
| `lightvm-service.exe` (in both) | | `40c4784e506cfb6ed10782a98408b356824c820e44c7627436ac9d7b33e8f314` |

Version `0.0.0` is the workspace version in `Cargo.toml` (not owned by this stream); `-Version` overrides the file names (for example `0.1.0-alpha` at the v0.1 milestone). These artefacts were built from HEAD `2a769fe` and are **test artefacts**, not a release: rebuild after the F2 streams are committed. The documents inside the package (`USER_GUIDE.md`, `README.md`) are the versions of this stream at build time.

### Checks

| Item | Outcome | Evidence |
| --- | --- | --- |
| Release build `--locked` of both binaries with static CRT | verified | `Finished release profile`; `dumpbin /dependents`: only system DLLs (kernel32, user32, opengl32, dxgi, uiautomationcore, ws2_32, bcrypt…), no `vcruntime140.dll`/`msvcp140.dll`/`api-ms-win-crt-*` |
| No build-host paths in the binaries | verified | 0 occurrences of `Users\Marco`, `Woarkareas`, `claude` in both `.exe` (first build without remap had registry paths with the user name; second build still had 2 `OUT_DIR` paths of `glutin_*_sys`, fixed by remapping the target dir) |
| `THIRD-PARTY-LICENSES.txt` | verified (generation) | 199 crates linked for `x86_64-pc-windows-msvc` (`cargo tree -e normal`, workspace crates excluded); licence files copied from the registry sources, including `epaint_default_fonts` `OFL.txt`, `UFL.txt`, `Hack-Regular.txt`, emoji font licence and `ring`'s `LICENSE`; 17 crates ship no licence file (accesskit ×4, egui family ×8, clipboard-win, enum-map ×2, profiling, spirv): authors, repository and a standard-text appendix (Apache-2.0, MIT, BSL-1.0) are written instead |
| Zip checksum file matches | verified | `.sha256` = computed hash |
| `install.ps1` verifies the package and installs per user | verified | `Package checksums: OK`; 11 files in the prefix (10 payload + `install-manifest.json`); shortcut target = `<prefix>\lightvm-desktop.exe`; exit 0; no administrator rights (normal user session) |
| Re-install without `-Force` refused | verified | `The prefix … is not empty. Use -Force …`, exit 1 |
| Install over a non-LightVM folder refused even with `-Force` | verified | `… contains no LightVM installation (install-manifest.json missing): refusing to overwrite`, unrelated file kept |
| Smoke from the installed location (`--smoke 3 --no-supervisor`) | verified | exit 0 after 4 s; `SMOKE outcome=ok`, `renderer=glow gl_version=3.3`, `frame=37`, `first_frame_ms=130`; nothing written to the redirected `LOCALAPPDATA` |
| Uninstall leaves nothing | verified | prefix removed, parent folder empty, shortcut removed, user `PATH` unchanged, 0 LightVM processes from the test root, temp root deleted |
| Uninstall keeps user data by default | verified | message `Your VMs and settings were kept in …`; data folder untouched |
| `-RemoveUserData -Force` removes the given data folder | verified | with `-UserDataDir <tmp>\data`: `User data removed`, folder gone |
| `setup.exe` (Inno Setup) per-user silent install, smoke, uninstall | verified | `Get-AuthenticodeSignature` → `NotSigned`; setup exit 0; same 8 payload files + `unins000.*`; smoke exit 0, `SMOKE outcome=ok`; uninstaller exit 0; folder removed; HKCU uninstall key present after install, absent after uninstall |
| `-AddToPath` | not executed | would modify the real user `PATH`; code path reviewed only |
| Interactive install (double-click, SmartScreen prompt, Start Menu launch, interactive Inno wizard) | not executed | agent session without interactive desktop use |
| Clean-machine test | **not executed** | Windows Sandbox not enabled on this host; procedure below for the owner |
| VM start from the installed package with QEMU 11.1.0 | not executed | smoke runs without supervisor; the F2 UI was not in the built commit |
| macOS job on GitHub | **not executed** | CI has never run on GitHub (`F1-08`); YAML parsed with PyYAML: 10 jobs, `macos-check` with `runs-on: macos-14`, `continue-on-error: true`, steps unchanged in shape |
| Existing CI jobs unchanged | verified (diff) | `git diff .github/workflows/ci.yml` touches only the `macos-check` job and its comment |
| User guide labels match the UI source | verified for the labels listed below, at 2026-10-10 end of session | grep of `apps/desktop/src` (desktop stream still in progress) |

### User guide: labels checked against `apps/desktop/src`

Found in the source: **Dashboard**, **Console**, **Settings**, **New VM**, **Next**, **Back**, **Cancel**, **Create**, **Import ISO…**, **Import data ISO…**, **No data media**, **Ubuntu LTS**, **Other Linux**, **Offline: no network adapter**, **Start**, **Stop**, **Edit**, **More**, **Force stop…**, **Reset…**, **Delete…**, *Stop the VM to delete it*, **Emergency Stop**, **Backend**, **Acceleration**, **Full screen**, **Exit full screen**, **Screenshot**, **Guest keyboard layout** (**US**/**IT**), **Connected**, **Save settings**, **Stop them and quit**, **QEMU distribution**, **VM catalog**, **Browse…**, **Apply and restart the supervisor**, **Start the supervisor**, **Appearance**, **Theme**, **If LightVM closes unexpectedly**; wizard defaults 2 vCPUs / 4096 MiB / 32 GiB, firmware UEFI.

**Not confirmed** (absent from the source at the time of the check, or behaviour not observable): the confirmation dialogs of **Delete…**, **Force stop…** and **Reset…** (texts); a way to **detach the installation media** after installation (guide section 8 offers it conditionally); the text shown by **Acceleration** when WHPX works or fails; the console window opened with the app (the binary is a console-subsystem executable today); the Ubuntu boot menu entry **Boot from next volume** (Ubuntu's, not LightVM's; not observed in this session).

## Clean-machine test procedure (not executed)

Prerequisite (owner, administrator once): Windows 11 Pro/Enterprise, *Turn Windows features on or off* → **Windows Sandbox** → restart.

1. Build or copy the artefacts into `C:\Woarkareas\OpenVM\dist` (`build-package.ps1` writes `LightVM-sandbox.wsb` with the absolute `dist` path; for another location edit `<HostFolder>`).
2. Double-click `dist\LightVM-sandbox.wsb`. The sandbox maps `dist` read-only on its desktop, has no network and no clipboard, and at logon runs `sandbox-test.ps1`, which verifies the zip's SHA-256, installs with the default per-user prefix and Start Menu shortcut, runs `lightvm-desktop.exe --smoke 5 --no-supervisor` and writes `Desktop\LightVM-sandbox-report.txt`.
3. Manually in the sandbox: open the report (expected `smoke exit code: 0`, `SMOKE outcome=ok`; the renderer will probably be WARP or glow on the sandbox's virtual GPU); start **LightVM** from the Start Menu (expect a SmartScreen prompt for the unsigned binary; record it); open **Settings**: **QEMU distribution** must read *not set: VMs cannot be started*; the Dashboard must not show simulated VMs; close LightVM.
4. Optional: install the `setup.exe` from `dist` with its wizard, check *Installed apps*, uninstall from there.
5. Run `powershell -NoProfile -ExecutionPolicy Bypass -File "$env:LOCALAPPDATA\Programs\LightVM\uninstall.ps1"`; check that `%LOCALAPPDATA%\Programs\LightVM` and the Start Menu shortcut are gone and that `%LOCALAPPDATA%\LightVM` (if created by the run) is kept.
6. Record the report, screenshots and outcomes in a dated section appended to this file.

Limits of the sandbox: no nested virtualization (no WHPX) and no QEMU, so VM start, console and Ubuntu installation cannot be tested there. The full clean-machine run of the guide (QEMU install, Ubuntu installation, reopen) needs a second physical PC or a reinstalled one.

## Limits

- Package binaries come from HEAD `2a769fe`, not from the F2 code being written; rebuild after the F2 commit.
- Only one host, already used for development (Visual C++ runtime installed system-wide): the static CRT avoids that dependency by construction and `dumpbin` confirms it, but a clean-machine start was not observed.
- `lightvm-desktop.exe` is a console-subsystem executable: starting it from the shortcut also opens a console window (documented in the guide). Changing it belongs to `apps/desktop`.
- The release profile keeps debug info; PDBs are not shipped (`/PDBALTPATH:%_PDB%` keeps only the file name in the binary).
- The zip is reproducible only in structure (entry order, timestamps = commit time); binary reproducibility was not tested.
- `THIRD-PARTY-LICENSES.txt` is not an SBOM and not a legal review (F1-07 actions remain); the font licences of `epaint_default_fonts` still need human review (ADR-0023).
- No icon resource in the binaries: the shortcut shows the default executable icon.
- The macOS probe compiles `--workspace --all-targets` including spikes; failures are expected to be informative, not blocking.

## Required actions

1. Owner: run the clean-machine procedure above (Windows Sandbox) and, for the F2 exit criterion, the full guide on a clean PC; append the results here (backlog F2 installer item in `docs/RELEASE_PLAN.md`).
2. Desktop stream: provide a way to detach the installation ISO after installation, or change the boot order once the disk is installed (today `cmdline.rs` gives the install CD `bootindex=0` while attached); otherwise the guide's section 8 relies on the Ubuntu menu entry *Boot from next volume*. Confirm the delete/force-stop/reset dialog texts and update `docs/USER_GUIDE.md` if they differ.
3. Desktop stream: consider `windows_subsystem = "windows"` for release builds (keeping diagnostics in a log file) so the shortcut does not open a console window.
4. Coordinator: rebuild the package from the F2 commit with `-Version 0.1.0-alpha` when the milestone is tagged; record the hashes.
5. First CI run on GitHub: record the outcome of `macos-check` (risk in `docs/STATUS.md` if it fails or the runner is unavailable; macOS stays "not supported").

## Proposed decisions

- ADR-0020 consequence: the alpha is published, if at all, as an unsigned per-user package with SHA-256 files; the guide asks users to verify the checksum before bypassing SmartScreen.
- Keep the zip + `install.ps1` as the reference package (fully scriptable, tested with a temporary prefix) and the Inno Setup `setup.exe` as an optional convenience produced when Inno Setup is available on the build machine; choose one for F6 together with signing.

## 2026-10-11 — F6 package: MCP adapter, SBOM, release artefact check, no test switches

Executed by the desktop/packaging agent on the development host (Windows 11, MSVC, Inno Setup 6), working tree on `e10e75b` with uncommitted changes (BUILDINFO says so); not committed. The sections above are historical: the procedure steps that run `lightvm-desktop.exe --smoke … --no-supervisor` on the installed package no longer apply (see below).

Changes (`packaging/windows/`):

- `build-package.ps1`: builds and stages `lightvm-mcp.exe` too (licence tree with `-p lightvm-mcp`); generates `SBOM.cdx.json` (CycloneDX 1.5, `crates/lightvm-sbom`, offline, `SOURCE_DATE_EPOCH` = commit time, licence check against `deny.toml`); runs `scripts/check-release-artifacts.ps1` on the staging folder and stops on any finding; `BUILDINFO.txt` gets an `SBOM:` line and the build line names the three packages and "default features, no dev-tools".
- `lightvm.iss`: `lightvm-mcp.exe` and `SBOM.cdx.json` in `[Files]`. `install.ps1`: both in the payload (checked against `SHA256SUMS.txt`, listed in `install-manifest.json`, so `uninstall.ps1` removes them); install and uninstall refuse while `lightvm-mcp` runs from the prefix.
- `tests/install-uninstall-test.ps1`: fake package with the new files; checks that every package file is installed and listed in the manifest, and that a package without `lightvm-mcp.exe` is refused with nothing installed; fixed `Invoke-Script` so a failing child script is reported instead of aborting the test (Windows PowerShell turns stderr into an error under `Stop`).
- `sandbox/sandbox-test.ps1`: the package has no `--smoke` any more (the test switches exist only in `dev-tools` builds of the desktop, F6-01): the sandbox test starts the installed app with no option, checks after 10 s that it runs and that `lightvm-desktop.log` records the rendering backend (and the supervisor start), closes the window and records exit code and log lines.
- `docs/USER_GUIDE.md` §3: package contents with `lightvm-mcp.exe` and `SBOM.cdx.json`; §11: the AI client starts `lightvm-mcp.exe` from the installation folder (stdio, no arguments); limitations updated (adapter included, verified only with a scripted client).

Results:

| Check | Result |
| --- | --- |
| `powershell -NoProfile -ExecutionPolicy Bypass -File packaging\windows\build-package.ps1 -Version 0.1.0-alpha -TargetDir target\rel` | exit 0; 219 crates in `THIRD-PARTY-LICENSES.txt` (19 without licence file, standard texts appended); SBOM 247 components, licences satisfy `deny.toml`; release artefact check: 3 files scanned, **no findings**; zip and setup.exe produced |
| `packaging\windows\tests\install-uninstall-test.ps1` | **29/29 PASS**, exit 0; real user PATH unchanged |
| Installed package (zip SHA-256 verified, `install.ps1 -Prefix <tmp> -NoShortcut`, `LOCALAPPDATA` redirected to a temp folder) | 13 files installed incl. `lightvm-mcp.exe`, `SBOM.cdx.json`, manifest; `--help` lists no test switch; `--smoke 1`, `--simulated-console`, `--no-supervisor`, `--static` → exit 2 `unknown argument`; `lightvm-mcp.exe --version` → exit 0; timed start: running after 10 s, log `rendering: glow gl_version=3.3`, `service: lightvm-service started`; window closed → exit 0, supervisor `shutdown completed`, no process left from the prefix; `uninstall.ps1` → prefix removed, user data kept |

Artefacts (`dist\`, git-ignored):

| File | SHA-256 |
| --- | --- |
| `LightVM-0.1.0-alpha-windows-x64.zip` | `6b6eebd2321de4dcd05c715dda6ff3be613e15803c630905d095f40d4591003a` |
| `LightVM-0.1.0-alpha-windows-x64-setup.exe` | `57a25c421412196bf5d9744b63a0b4c59400bf5705ca661000ee539dc32f4dba` |
| `lightvm-desktop.exe` (21 871 104 bytes) | `21254f52c89f38869875023be8fcf9ea9dbe5c11f5380183bd43030792f5a6f1` |
| `lightvm-service.exe` (9 845 248 bytes) | `b7761f7eeb1f17df1fea28fe490ec022239fbafd5c032325b8693324ce1beaeb` |
| `lightvm-mcp.exe` (7 899 136 bytes) | `71aba328badd50b84b742d1b3b1fd70f0170f001f370c3d7cdeeda11a5a165ff` |
| `SBOM.cdx.json` | `63465e8392ae11abe6c907c2ca6ab57b35985f72d32532977b51077b0d4c53a8` |

Not executed: the setup.exe install/uninstall test and the Windows Sandbox run (unchanged required action 1); package built from an uncommitted tree, to be rebuilt from the commit. The crate version is still `0.0.0` (`lightvm-mcp --version`, the app's start line), only the file names carry `0.1.0-alpha`.
