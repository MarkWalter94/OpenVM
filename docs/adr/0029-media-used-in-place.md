# ADR-0029 — Installation and data ISOs used in place, without a managed copy

- Status: Accepted
- Date: 2026-10-14
- Owner: project owner (decision); implementation recorded below

## Context

ADR-0008 kept a **managed read-only copy** of every approved ISO in the catalog (`media/<media_id>.iso`), so that a path selected by the user cannot later authorize a different file. In practice every ISO takes its disk space twice (a Windows or Ubuntu ISO is 5–7 GB), and importing it means copying it.

The project owner decided: *"when I select an ISO, do not copy it — we waste disk space for nothing. Use the file the user linked, do not copy it out."* This decision takes precedence over the copy rule of ADR-0008; ROADMAP 6.3 states it (the registry uses the ISO at the path chosen by the user, re-verified before every start), with `docs/ROADMAP.sha256` pinned to that text. The other requirements of that rule are kept: identity and hash, a selected path does not authorize whatever replaces the file, protection against symlinks, junctions and substitutions between check and use.

## Decision

New imports (installation and data media) **reference the user's file in place**; nothing is copied.

**Import** (`vm_storage::Catalog::reserve_media_import` → `MediaImportReservation::open_and_hash` on the import worker thread → `finish_media_import`):

1. the reservation chooses the identifier and records nothing (no journal intent, no temporary file): an interrupted import leaves nothing behind;
2. the file is opened **once**, without following a reparse point on the last component and with `FILE_SHARE_READ` only (nobody can write, rename or delete it while it is examined); on the handle: regular file, size limit (16 GiB installation, 8 GiB data), and the supervisor's checks (`check_import_handle`: link or reparse point refused, local volume only — network shares and mapped network drives refused as before, removable and optical drives accepted as before; Windows: the final path of the handle must be the requested path, so no link, junction, mount point, `subst` drive or 8.3 name on the way);
3. the path recorded is the **absolute path the system resolved** for that handle (Windows: final path of the handle; elsewhere `canonicalize`). It is refused when it lies inside the catalog folder, and when QEMU's option syntax could misread it (`,`, `=`, `:` after the drive, quotes and the other characters of `qemu_backend::validate::qemu_path_value`): the user renames the file or its folder;
4. SHA-256 and the ISO 9660 / UDF signature are read in one pass **through the same handle**, and the guest detection (ADR-0027) reads through it too, before the handle is closed;
5. the registry row (`storage = 'referenced'`, `external_path`, size, hash, detected guest) is written in one transaction. The same file selected again with the same kind updates its row (new hash and size, status `approved`): the identifier the VMs use does not change, which is how a changed file is approved again.

**Every start** (`MediaGuard::open_external`, `ManagedMediaInspector`): the file is opened the same way (`FILE_SHARE_READ` only, last component not followed), every component of its path (each directory and the file) is checked not to be a link or reparse point, the import checks are repeated on the handle (volume, final path, size, signature), the size and SHA-256 are recomputed **through that handle** and compared by the core (`vm_core::media::verify_media`), and QEMU receives that same path (read-only CD-ROM node, unchanged) while the handle is held until the run is released. A mismatch refuses the start: *the ISO file "name" changed since it was selected: import it again to use it*. A file no longer found refuses the start (*… is no longer where it was selected …*), and the medium is marked `missing`; the registry listing looks the file up again and shows it as missing or available. Public messages name the file, never its path; the full path is shown only to the `ui` role (`MediaRecord.location`), never to agent sessions.

**The user's file is never modified**: the catalog does not write, move, rename or delete it. Deleting such a medium (Settings → Installation media) removes its registry row only; the UI says the file is kept. A catalog move does not copy it and keeps the reference.

**Compatibility.** Schema 8 (`0008_media_in_place.sql`) rebuilds `media` with `storage` (`managed` | `referenced`), `managed_path` (now nullable) and `external_path`, one row per file and kind; the migration follows the existing rules (backup before migrating, downgrade refused, refused with open runs) and runs with foreign keys off, checked with `foreign_key_check` afterwards. Media imported before keep their managed copy: verified, used and deleted (copy removed through the journal) exactly as before; unfinished `import_media` intents of an older version still reconcile.

## Alternatives considered

- Keep the managed copy (ADR-0008): rejected by the owner (disk space and import time).
- Hard link instead of a copy: only on the same NTFS volume, and a hard link shares the content with the user's file, so it protects nothing a reference does not; not chosen.
- Copy-on-write clone (ReFS block cloning, APFS clones): not available on NTFS, the usual Windows file system; not chosen.
- Pass QEMU an inherited file descriptor instead of a path: not available for block nodes on Windows; not chosen.

## Consequences

Residual risks, recorded in `docs/SECURITY.md`:

- **The file can change between runs** (same user, any program): detected by the SHA-256 at the next start, which is refused until the user imports the file again.
- **The file can be moved, renamed or deleted, or its drive disconnected**: the VM cannot use it (start refused, medium shown as missing) until it is back or imported again; an ISO imported again from a new place is a new medium to choose in the VM's settings.
- **Hashing at every start costs time** (about a minute for a 6 GiB ISO on a typical SSD, more on a removable drive); this was already the case for managed copies.
- The protection during a run relies on the Windows share mode of the guard handle. **Without share modes (macOS, Linux)** another process of the same user can write the file during a run (declared limit, as for the other guarded files, R2a).
- The final-path check limits paths to the real location; a medium on a removable drive whose letter changes is reported missing.
- QEMU now receives a path chosen by the user (after the checks above) instead of a catalog path: its form is validated against the QEMU option grammar at import and again at start.

## Required evidence

- `vm-storage` `tests/media_registry.rs`: in-place import (no file under `media/`, path recorded, nothing left behind), same file selected again, changed file, missing file and its return, guard on a file used in place (links on the path, pinned file), deletion keeps the user's file, managed copies of older catalogs (verification, deletion, interrupted deletion).
- `vm-storage` `tests/migrations.rs`: upgrade of a schema 7 catalog with a managed copy attached to a VM.
- `vm-storage` `tests/catalog_move.rs`: catalog move with a medium used in place.
- `lightvm-service` `adapters::catalog` tests and `tests/f2.rs`: import through the supervisor, start-time hash mismatch and missing file with their messages, refusals (inside the catalog, path QEMU cannot take).
- With real QEMU and a real ISO through the app: **not verified** (to be added to a verification file).

## References

Roadmap, sections 6.3, 7.4 ("File" and "Host" rows); ADR-0008 (amended by this ADR), ADR-0017 (storage budget: files used in place are outside the catalog and not counted), ADR-0025 (guard share modes), ADR-0027 (guest detection).
