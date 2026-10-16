# F6-02 — Embedded font notices in Windows packages

Date: 2026-10-17. This does not close release qualification.

The desktop embeds Departure Mono 1.500 and JetBrains Mono 2.304. Their
source assets and OFL-1.1 texts were already inventoried in ADR-0023, but
the Cargo licence collector did not include them in the Windows package.

`packaging/windows/embedded-fonts.ps1` now checks the two pinned asset
hashes, requires their complete copyright/licence texts and generates
`BUNDLED-ASSETS.json`. The package contains byte-for-byte copies under
`licenses/fonts/`, and `THIRD-PARTY-LICENSES.txt` includes both full notices.
The asset inventory complements the Cargo dependency SBOM.

Both the PowerShell installer and the Inno Setup file list include the
inventory and notices. The PowerShell uninstaller accepts only the two
known nested font paths, rejects reparse points at either ancestor and
removes empty directories without recursive deletion. Rebuilding a package
keeps previous staging directories instead of deleting the whole staging
tree, which may contain another agent's artifacts.

Verification on this host:

- Generated the inventory and exact notice copies in the fresh directory
  `E:\OpenVM-tests\packaging-fonts-20261017`; both hashes match ADR-0023.
- `packaging/windows/tests/install-uninstall-test.ps1`: all checks passed,
  including installation/manifest/removal of the new assets and preservation
  of the real user PATH. Negative tests preserve font files behind junctions
  at both `licenses` and `licenses/fonts`. Final log:
  `E:\OpenVM-tests\packaging-fonts-install-test2.log`.
- PowerShell parser check of the package builder passed.
- Built the actual experimental ZIP from an isolated checkout on E: at
  `29c9d153f3298ca545f43e69f458964e4533a2fb` with only the packaging changes
  described here overlaid. Default release features, static CRT, path
  remapping and no dev-tools. Build log:
  `E:\OpenVM-tests\package-20261017\build.log`.
- The generated CycloneDX SBOM has 257 components; licence policy passed.
  The release artifact scanner examined all four EXEs (including Tools)
  and reported no findings.
- Extracted that ZIP into a fresh private directory on E:, installed with
  `-NoShortcut` and without `-AddToPath`, verified both installed font notice
  hashes and successfully ran `--help` on all three product executables.
  Uninstallation removed all listed payload files and retained an extra
  user file; the real user PATH was unchanged. Log:
  `E:\OpenVM-tests\package-20261017\verify-package.log`.

Artifact:
`E:\OpenVM-tests\package-20261017\dist\LightVM-0.0.0-alpha.20261017-windows-x64.zip`
(19,433,199 bytes).
SHA-256: `cfac788fcc069036823fb9bead0e4ac86d026c931aa5c27224bdc9e26650aea7`.
The source checkout and exact packaging overlay are retained under
`E:\OpenVM-tests\package-20261017\source`; BUILDINFO marks the overlay dirty.

This is an unsigned experimental ZIP, not a supported release. Inno Setup
was deliberately skipped; the PowerShell ZIP installer is the tested path.
Clean-machine installation, external QEMU/swtpm runtime source/licence
closure, signing and independent review remain separate work. QEMU, swtpm,
firmware and guest images are not bundled. Windows 11 runtime work is owned
by Claude; no diagnostic QEMU bundle was changed or packaged here, and his
uncommitted Windows work is absent from this isolated package.
