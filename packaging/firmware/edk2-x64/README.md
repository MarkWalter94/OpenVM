# OVMF X64 with Secure Boot, without SMM (ADR-0035)

The Secure Boot OVMF shipped with QEMU requires SMM, which WHPX does not
provide. This recipe builds `OvmfPkg/OvmfPkgX64.dsc` from
edk2-stable202408 (`b158dad150bf02879668f72ce306445250838201`, the edk2 QEMU
11.1 ships prebuilt and the version of ADR-0028) with:

`-b RELEASE -t VS2019 -D SECURE_BOOT_ENABLE=TRUE -D SMM_REQUIRE=FALSE -D TPM2_ENABLE=TRUE -D TPM2_CONFIG_ENABLE=TRUE -D TPM1_ENABLE=FALSE -D BUILD_SHELL=FALSE`

and enrols the Secure Boot keys with virt-firmware 26.9 (wheel SHA-256
`64d0078e6dd0c82a1df3a97c3cc8a149340068095cf8cfb6beb02005e3194160`):
PK generated at build time by `gen-pk.py` (private key discarded),
KEK = Microsoft Corporation KEK CA 2011 + Microsoft Corporation KEK 2K CA 2023
(+ the PK certificate, as virt-firmware does), db = Microsoft Windows
Production PCA 2011 + Windows UEFI CA 2023 (profile `win11`), no third-party
UEFI CA. dbx is the empty placeholder of the template.

Local patch: `0001-OvmfPkgX64-IntrinsicLib-for-DisplayEngineDxe-MSVC.patch`
links `CryptoPkg/Library/IntrinsicLib` into `DisplayEngineDxe` only: MSVC
14.44 lowers a structure copy there to a `memcpy` call that edk2 does not
otherwise provide (`LNK2001: memcpy`).

Toolchain: Visual Studio 2022 Build Tools (MSVC 14.44, edk2 tag `VS2019`),
NASM (MSYS2 `mingw-w64-ucrt-x86_64-nasm`), ACPICA `iasl` 20260408
(`iasl-win-20260408.zip`, SHA-256
`121f5e4f30b1df63d09052294e4a605d4dee2dfb9599fa24af4ac6015df02b70`), Python 3
with `cryptography` (MSYS2 UCRT64 Python 3.14.8).

```
powershell -File build.ps1 -Work <dir> -Out <dir> -Python <python.exe> -NasmDir <dir\> -IaslDir <dir\>
```

**Status: not yet completed end to end.** On the project host, all modules
compile and link, but the corporate antivirus removes the freshly built
BaseTools host tool `BaseTools\Bin\Win32\LzmaCompress.exe` seconds after it
is linked, so the firmware volumes cannot be compressed. The build must run
on a machine where that tool is allowed (or after an exclusion granted by the
host administrator), and the resulting files are pinned by SHA-256 in
ADR-0035 when released. The key enrolment step was verified separately
(R6-05).

`collect-licenses.ps1 -Edk2 <edk2-tree> -Out <new-dir>` copies the licence
files of edk2 and of every submodule initialised by `build.ps1` and writes
`INVENTORY.tsv` (component, path, commit, URL). The corresponding source is
that edk2 commit, those submodule commits and `0001-*.patch`. Its output is
one of the `-Licenses` inputs of `../../qemu-win11/assemble.ps1`.

Declared limit (ADR-0035): without SMM the variable store is writable by code
running in the guest kernel; Secure Boot protects against unsigned boot
components, not against a guest already compromised at ring 0.
