# ADR-0008 — Approved media registry and read-only data medium

- Status: Accepted; operational details of the data medium recorded on 2026-10-10 (section "Data medium details")
- Date: 2026-10-09
- Owner: to be assigned

## Context

A path selected by the user can be replaced after approval (symlink, junction, file swap). The guest needs a channel to receive the software under test without shared folders (roadmap, sections 4 and 6.3).

## Decision

- The approved media registry keeps a managed read-only copy of each ISO, with identity and hash; QEMU receives only media from the registry.
- Creation and opening protected against symlinks, junctions and substitutions between check and use, with OS primitives and controlled directories.
- In addition to the installation medium, a VM can mount, **with the VM off**, a second read-only ISO data medium coming from the registry. It is the only way files enter the guest in the MVP: not writable by the guest, not manageable by the agent.
- No MCP tool mounts, replaces or removes media (section 9).
- No automatic mounting of guest filesystems on the host; no automatic image catalog; ISOs introduced by the user through the native picker.
- The MCP client supplies IDs, not paths (section 7.2, point 5).

## Details to be closed in F0

Section 17 lists the "approved data medium" among the F0 decisions. Still to be settled: the tool and format with which the user produces the data ISO, size limits, type of emulated device (read-only CD-ROM) and verification that the guest cannot write to it. These details must be recorded in this ADR (additional section) or in a later linked ADR, without changing the decision above.

## Data medium details (2026-10-10, closure of P-7)

Implementation data (section 17), without changing the decision above:

| Item | Choice | Where |
| --- | --- | --- |
| Format | **ISO 9660** optical image (including hybrids with UDF or Joliet/Rock Ridge) or **UDF**: `CD001` or `BEA01` signature in the first volume descriptor (offset 32769). Other files are rejected at inspection (`invalid_argument`) | `lightvm-service/src/adapters/catalog.rs` (`is_optical_image`, `ManagedMediaInspector`) |
| Tool | In the MVP the user produces the data ISO with a standard tool outside LightVM (for example `oscdimg` from the Windows ADK or any ISO 9660/UDF generator); LightVM makes a managed copy of it and verifies its signature, size and hash. Generating the ISO from a folder inside LightVM is a possible extension (to be evaluated with ADR-0023 for licenses), not a requirement | — |
| Size | Installation ISO ≤ 16 GiB, data ISO ≤ 8 GiB (dual-layer DVD); above that → `resource_limit` at registration. Managed copies count toward the storage budget (ADR-0017) | `lightvm-proto` (`MAX_INSTALL_MEDIA_BYTES`, `MAX_DATA_MEDIA_BYTES`), `vm-core` (`register_media`) |
| Emulated device | IDE CD-ROM (`ide-cd`) on the q35 AHCI controller, never bootable for the data medium (no `bootindex`); `raw` node with `read-only=on` on both the format and the file | `qemu-backend/src/cmdline.rs` (`push_cdrom`) |
| Guest cannot write | Three layers: a CD-ROM does not accept writes from the guest; the QEMU block is opened read-only; the managed copy is read-only and its hash is re-verified by the core before every start (`verify_media`): a modification prevents startup | `vm-core/src/media.rs` |
| Mounting | Only with the VM off (VM configuration, `data_media`); no MCP tool changes it | `vm-core` (`update_vm_config` requires the VM to be off) |

Evidence: `vm-core` `scenarios::register_media_rejects_oversized_images`; `lightvm-service` `adapters::catalog::tests::iso_9660_or_udf_signature`; the reference Ubuntu ISO has the `CD001` signature. Writing from the guest with real QEMU: **verified impossible** on 2026-10-10 (`docs/verification/F1-11-second-user-and-data-disc.md`: `touch`, remount and `dd` failed with "Read-only file system", ISO hash unchanged).

## Alternatives considered

- Shared folder (virtio-fs, 9p, SMB): excluded (section 4).
- Writable data disk: excluded; the medium is read-only.
- Media management by the agent: excluded (section 9).

## Consequences

- The test fixture (ADR-0019) is distributed via the data medium by F3.
- The media registry is part of the storage budget (ADR-0017).

## Required evidence

- "File" negative test from section 7.4 (ISO replaced after approval) and "Host" (path traversal, UNC, device path, symlink/junction): from F1.
- Identity and checksum of the guest ISO: `docs/verification/F0-03-guest-media.md` (verified).
- Details of the approved data medium: recorded on 2026-10-10 (section above, P-7 closed); test of writing from the guest with real QEMU verified in `F1-11`.

## References

Roadmap, sections 4, 6.3, 7.2, 7.4, 9, 17.
