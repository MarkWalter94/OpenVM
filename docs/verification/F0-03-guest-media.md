# F0-03 — Reference guest: release choice and media verification

- Execution date (UTC): 2026-10-09 (downloads and checks between 10:04:49Z and 10:14:11Z)
- Executed by: release/security agent (Claude Code)
- Roadmap step: section 4 (MVP, "a single reference guest: Ubuntu LTS x86_64, with version and checksum pinned in F0"), section 10 F0 ("pin binary, version, firmware and guest with checksums"), section 17 (decision "guest and firmware release")
- Related ADRs: ADR on guest and firmware release (to be written; see "Proposed decisions")
- Overall outcome: **verified** (integrity and signature of the media). Guest boot: **not executed** (out of scope of this test).

## Purpose

Choose and obtain the MVP reference guest with verified provenance: Ubuntu LTS amd64 desktop image, version and SHA-256 pinned, signature of the checksum file verified with the official Ubuntu image key.

This test does **not** demonstrate: that the ISO boots in QEMU/WHPX, that the content is free of vulnerabilities or malicious code, nor any compatibility of the guest with LightVM. The ISO file was neither executed nor mounted.

## Environment

| Item | Value |
| --- | --- |
| Host | development workstation, Windows 11 Pro |
| OS and build | Windows 10.0.26200.9457 (output of `ver`); Git Bash shell `MINGW64_NT-10.0-26200 3.4.10` |
| Accelerator and state | not relevant for this test |
| Tools | `curl 8.7.1 (x86_64-w64-mingw32) ... Schannel`; `gpg (GnuPG) 2.4.5`, `libgcrypt 1.9.4`; `sha256sum (GNU coreutils) 8.32` |
| QEMU binary | not used |
| Firmware | not used |
| Guest / media | `ubuntu-26.04.1-desktop-amd64.iso`, SHA-256 `601e30fbf5d97759367c632e2c33630665039b7e2158fd068403da3ccf1bda1f` |
| Graphics session | not relevant |
| Other conditions | Dedicated GPG keyring `lab/media/gnupg` (variable `GNUPGHOME`), separate from the user keyring |

## LTS releases observed and rationale for the choice

Command:

```bash
curl -s https://releases.ubuntu.com/ -o lab/media/evidence/releases-index.html
sed 's/<[^>]*>/ /g' lab/media/evidence/releases-index.html | grep -oE "Ubuntu [0-9.]+( LTS)? \([A-Za-z ]+\)" | sort -u
```

Output:

```text
Ubuntu 14.04.6 LTS (Trusty Tahr)
Ubuntu 16.04.7 LTS (Xenial Xerus)
Ubuntu 18.04.6 LTS (Bionic Beaver)
Ubuntu 20.04.6 LTS (Focal Fossa)
Ubuntu 22.04.5 LTS (Jammy Jellyfish)
Ubuntu 24.04.3 LTS (Noble Numbat)
Ubuntu 24.04.5 LTS (Noble Numbat)
Ubuntu 25.10 (Questing Quokka)
Ubuntu 26.04.1 LTS (Resolute Raccoon)
Ubuntu 26.10 (Stonking Stingray)
```

Directories present in the index (`href` excerpt): `14.04.6/ 16.04.7/ 18.04.6/ 20.04.6/ 22.04.5/ 24.04.2/ 24.04.3/ 24.04.4/ 24.04.5/ 24.04.5.1/ 25.10/ 26.04/ 26.04.1/ 26.10/` plus aliases by code name. 25.10 and 26.10 are not LTS (no "LTS" label in the title).

Listing of the directory `https://releases.ubuntu.com/26.04.1/` (excerpt of the desktop images and checksums; `26.04/` returns the same listing):

```text
SHA256SUMS                            2026-08-27 22:43   588
SHA256SUMS.gpg                        2026-08-27 22:43   833
ubuntu-26.04-desktop-amd64.iso        2026-04-23 02:25   6.1G
ubuntu-26.04.1-desktop-amd64.iso      2026-08-26 06:38   6.0G
ubuntu-26.04.1-live-server-amd64.iso  2026-08-26 05:14   2.7G
```

Criterion applied: latest point release of the most recent LTS that has at least one published point release. The most recent LTS is 26.04 (Resolute Raccoon) and it has point release 26.04.1 published (ISO dated 2026-08-26). No 26.04.2 is present in the index. **Choice: Ubuntu 26.04.1 LTS, amd64 desktop image** (the MVP requires a graphical guest; the server image is excluded). The previous LTS (24.04.x) is not needed.

## Procedure

All commands run in `E:\OpenVM\lab\media` (Git Bash).

1. Checksums, signature and HTTP headers of the ISO:

```bash
B=https://releases.ubuntu.com/26.04.1
curl -L --fail --retry 5 --retry-delay 10 -o SHA256SUMS     $B/SHA256SUMS
curl -L --fail --retry 5 --retry-delay 10 -o SHA256SUMS.gpg $B/SHA256SUMS.gpg
curl -sIL $B/ubuntu-26.04.1-desktop-amd64.iso
```

Relevant headers: `HTTP/1.1 200 OK`, `Content-Length: 6482409472`, `Last-Modified: Wed, 26 Aug 2026 06:38:13 GMT`, `ETag: "18261b800-659ed76a65f40"`. No redirect.

Content of `SHA256SUMS`:

```text
487f87faaf547ea30e0aba4d5b53346292571256b25333a978db1692bcee9dd2 *ubuntu-26.04-desktop-amd64.iso
dec49008a71f6098d0bcfc822021f4d042d5f2db279e4d75bdd981304f1ca5d9 *ubuntu-26.04-live-server-amd64.iso
96c7f5fb28a7fe28245331f9bfbe4375f18dd29a4850116ad3c4f60f6700c55c *ubuntu-26.04-wsl-amd64.wsl
601e30fbf5d97759367c632e2c33630665039b7e2158fd068403da3ccf1bda1f *ubuntu-26.04.1-desktop-amd64.iso
cc8a95cde20f6ced61a322420de00f10cc3c90ced545daa46cb9c1a117f1d927 *ubuntu-26.04.1-live-server-amd64.iso
48d56724b5c8e60f24893e83e73bbb58c60b3ca22fba3da977075420acd54104 *ubuntu-26.04.1-wsl-amd64.wsl
```

2. Download of the ISO in the background, with resume:

```bash
curl -L --fail --retry 5 --retry-delay 10 -C - -s -S \
  -o ubuntu-26.04.1-desktop-amd64.iso \
  https://releases.ubuntu.com/26.04.1/ubuntu-26.04.1-desktop-amd64.iso
```

Outcome: `exit=0` (log `evidence/curl-iso.log`), no error reported, no resume needed. Final size (`stat -c %s`): **6482409472 bytes**, equal to the `Content-Length`.

3. Identification of the signing key:

```bash
gpg --list-packets SHA256SUMS.gpg
```

```text
:signature packet: algo 1, keyid D94AA3F0EFE21092
	version 4, created 1787870589, md5len 0, sigclass 0x00
	digest algo 10, begin of digest 24 61
	hashed subpkt 33 len 21 (issuer fpr v4 843938DF228D22F7B3742BC0D94AA3F0EFE21092)
	hashed subpkt 2 len 4 (sig created 2026-08-27)
	subpkt 16 len 8 (issuer key ID D94AA3F0EFE21092)
	data: [4096 bits]
```

The file contains a single signature (RSA 4096, digest algo 10 = SHA-512). Before the import, `gpg --verify` reported `Can't check signature: No public key`.

4. Import of the key from the official keyserver, into a dedicated keyring:

```bash
export GNUPGHOME=/e/OpenVM/lab/media/gnupg
gpg --keyserver hkps://keyserver.ubuntu.com --recv-keys 843938DF228D22F7B3742BC0D94AA3F0EFE21092
gpg --list-keys --with-fingerprint --with-subkey-fingerprints --keyid-format long
```

```text
gpg: key D94AA3F0EFE21092: public key "Ubuntu CD Image Automatic Signing Key (2012) <cdimage@ubuntu.com>" imported
gpg: Total number processed: 1
gpg:               imported: 1
pub   rsa4096/D94AA3F0EFE21092 2012-05-11 [SC]
      Key fingerprint = 8439 38DF 228D 22F7 B374  2BC0 D94A A3F0 EFE2 1092
uid                 [ unknown] Ubuntu CD Image Automatic Signing Key (2012) <cdimage@ubuntu.com>
```

5. Comparison with the official Ubuntu documentation: `curl -sL https://ubuntu.com/tutorials/how-to-verify-ubuntu` (effective URL unchanged, HTTP 200; copy in `lab/media/evidence/tutorial-verify.html`, SHA-256 `0d729e63cf9f896e430c412b6837362a96c756accbc6a926116f9afb19b8399c`). Fingerprints given on the page:

```text
pub   dsa1024/46181433FBB75451 2004-12-30 [SC]
      Key fingerprint = C598 6B4F 1257 FFA8 6632  CBA7 4618 1433 FBB7 5451
pub   rsa4096/D94AA3F0EFE21092 2012-05-11 [SC]
      Key fingerprint = 8439 38DF 228D 22F7 B374  2BC0 D94A A3F0 EFE2 1092
```

The key that signed `SHA256SUMS` (D94AA3F0EFE21092) matches character by character the fingerprint of the 2012 key given in the tutorial. The 2004 DSA key (46181433FBB75451) mentioned in the tutorial is not used by this signature and was not imported.

6. Checksum verification (only the relevant line):

```bash
grep ' \*ubuntu-26.04.1-desktop-amd64.iso$' SHA256SUMS > evidence/SHA256SUMS.desktop-26.04.1
sha256sum -c evidence/SHA256SUMS.desktop-26.04.1
```

```text
ubuntu-26.04.1-desktop-amd64.iso: OK
exit=0
```

7. Signature verification:

```bash
GNUPGHOME=/e/OpenVM/lab/media/gnupg gpg --keyid-format long --verify SHA256SUMS.gpg SHA256SUMS
```

```text
gpg: Signature made Fri Aug 28 00:43:09 2026
gpg:                using RSA key 843938DF228D22F7B3742BC0D94AA3F0EFE21092
gpg: Good signature from "Ubuntu CD Image Automatic Signing Key (2012) <cdimage@ubuntu.com>" [unknown]
gpg: WARNING: This key is not certified with a trusted signature!
gpg:          There is no indication that the signature belongs to the owner.
Primary key fingerprint: 8439 38DF 228D 22F7 B374  2BC0 D94A A3F0 EFE2 1092
exit=0
```

The time `Fri Aug 28 00:43:09 2026` is in local time (CEST); the signature packet reports `sig created 2026-08-27` (UTC).

**Trust warning explained.** `Good signature` means that `SHA256SUMS` was signed with the private key corresponding to the imported public key and has not been altered. The warning `not certified with a trusted signature` and the `[unknown]` marker only indicate that no trusted key in the local keyring has certified this key: there is no path in the Web of Trust. It is not a verification error. The link between key and owner was established out of band, by comparing the full fingerprint with the official `ubuntu.com` page served over HTTPS (step 5). No local trust was assigned to the key (`--edit-key trust` not run), so as not to mask this distinction.

8. Attestation: with both checks successful, `lab/media/ubuntu-26.04.1-desktop-amd64.iso.verified` was created (UTC date, SHA-256, size, key fingerprint, URL) and `lab/media/MANIFEST.md` was completed.

## Results

| Item | Outcome | Evidence |
| --- | --- | --- |
| Release choice (point release criterion) | verified | `releases.ubuntu.com` index observed on 2026-10-09: 26.04.1 LTS is the latest point release of the most recent LTS |
| Complete ISO download | verified | `curl` exit 0; 6482409472 bytes = `Content-Length` |
| ISO SHA-256 against `SHA256SUMS` | verified | `ubuntu-26.04.1-desktop-amd64.iso: OK` |
| GPG signature of `SHA256SUMS` | verified | `Good signature`, key `8439 38DF 228D 22F7 B374 2BC0 D94A A3F0 EFE2 1092` |
| Key fingerprint against official documentation | verified | matches https://ubuntu.com/tutorials/how-to-verify-ubuntu |
| Presence of the UEFI/Secure Boot chain in the media | verified on the published listing, not on the ISO | see Firmware section |
| Guest boot in QEMU/WHPX | not executed | out of scope (F0-02 / later tests) |

Pinned artifacts:

| Artifact | SHA-256 | Size |
| --- | --- | --- |
| `ubuntu-26.04.1-desktop-amd64.iso` | `601e30fbf5d97759367c632e2c33630665039b7e2158fd068403da3ccf1bda1f` | 6482409472 bytes |
| `SHA256SUMS` | `4adde9f7e7760a624db5821ad75679a76b9cde1e0d3e9cb2117cfe7d8043fa43` | 588 bytes |
| `SHA256SUMS.gpg` | `055292b53dffbc5387c1691e8c2357df0b4db88e7d7fd4c1b36fadf068479c5d` | 833 bytes |

## Required firmware and reference VM profile

To avoid mounting the ISO, the content listing that Ubuntu publishes next to the image was downloaded: `https://releases.ubuntu.com/26.04.1/ubuntu-26.04.1-desktop-amd64.list`, 951 lines. This file is not covered by `SHA256SUMS`, so it is only indicative. Relevant lines:

```text
/EFI/boot/bootx64.efi
/EFI/boot/grubx64.efi
/EFI/boot/mmx64.efi
/pool/main/s/shim-signed/shim-signed_1.59+15.8-0ubuntu2_amd64.deb
/boot/grub/x86_64-efi/...
/boot/grub/i386-pc/eltorito.img
```

- Firmware planned for the reference: **UEFI x86_64** (in QEMU, OVMF/edk2). The media contains the fallback EFI loader (`bootx64.efi`, the signed shim in Ubuntu), GRUB EFI and MokManager (`mmx64.efi`): the Secure Boot boot chain is present.
- **Secure Boot is not required** for Ubuntu: the same shim/GRUB chain also boots with Secure Boot disabled. It is however supported, so in the future an OVMF variant with Secure Boot enabled can also be tested. This note is based on the file listing and on Ubuntu's documented behaviour, not on an actual boot.
- The media also contains `i386-pc/eltorito.img` (legacy BIOS boot). The roadmap includes the UEFI NVRAM in the consistency model (section 8), so legacy BIOS is not proposed as the reference.
- UEFI implies one NVRAM variables file per VM, to be included in baseline, overlay and restore (roadmap, section 8).
- Minimum VM profile recommended by the roadmap for the reference (section 13): **2 vCPUs, 4 GiB of RAM, 1280×720 display**, on a reference host with at least 4 cores/8 threads, 16 GiB of RAM, SSD and at least 100 GiB free.

## Limits

- The GPG verification demonstrates the integrity and provenance of `SHA256SUMS` with respect to the Ubuntu image signing key and, through the SHA-256, those of the ISO. It does not demonstrate that the content is safe, free of vulnerabilities or suitable for the product.
- Trust in the key relies on the keyserver `keyserver.ubuntu.com` and on the HTTPS page `ubuntu.com`, i.e. on the same publisher's infrastructure. There is no independent attestation through the Web of Trust.
- The key D94AA3F0EFE21092 dates from 2012 (RSA 4096) and the listing shows no expiry date. The revocation status is the one returned by the keyserver at import time: no revocation shown.
- `releases.ubuntu.com/26.04/` and `/26.04.1/` show the same content. A future 26.04.2 or a republication would change `SHA256SUMS` in that directory: the reference stays pinned to the hash given above, not to the URL.
- The ISO was not booted. Firmware and compatibility with QEMU/WHPX remain to be tested.
- A first `gpg --verify`, run before setting `GNUPGHOME`, created the user's empty `~/.gnupg` directory (`gpg: directory '/c/Users/mortali/.gnupg' created`). No key was imported into that keyring.
- The ISO in `lab/media/` is not yet a managed read-only copy in the sense of the approved media registry (section 6.3): it is only a lab artifact.

## Required actions

- Guest boot test with QEMU/WHPX and pinned OVMF firmware (F0-02 or a later file), recording the hash of the firmware used.
- Record guest and firmware release in the dedicated ADR, and in `docs/COMPATIBILITY.md` only after the boot.
- Before each release, check whether a new point release or a security advisory has come out that makes it necessary to update the reference (section 17: a vulnerable version is not frozen).

## Proposed decisions

Proposals for the ADR "guest and firmware release" (they are not decisions until the ADR is written):

1. MVP reference guest: **Ubuntu 26.04.1 LTS desktop amd64**, file `ubuntu-26.04.1-desktop-amd64.iso`, SHA-256 `601e30fbf5d97759367c632e2c33630665039b7e2158fd068403da3ccf1bda1f`, 6482409472 bytes.
2. The identity of the media is pinned by the hash, not by the URL or the file name. A change of hash requires an update of the ADR with a new verification.
3. Accepted chain of trust: `SHA256SUMS` signed by the key `8439 38DF 228D 22F7 B374 2BC0 D94A A3F0 EFE2 1092` (Ubuntu CD Image Automatic Signing Key 2012). The fingerprint must be pinned in the repository and compared at every new acquisition, using a dedicated keyring.
4. Firmware: UEFI x86_64 (OVMF/edk2, version and hash to be pinned with F0-02), with per-VM NVRAM included in the restore model. Secure Boot is not required for the reference; the variant with Secure Boot enabled is an optional test.
5. Reference VM profile: 2 vCPUs, 4 GiB, 1280×720 display (roadmap, section 13).
6. Update criterion: adopt a new point release of the same LTS only after a new verification (checksum, signature, boot). Moving to a new LTS is evaluated only after its first point release.
7. Rejected alternative: 24.04.x LTS. It is available, but it is not the most recent LTS with a point release.
