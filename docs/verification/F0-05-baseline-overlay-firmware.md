# F0-05 — Baseline, overlay and firmware with the VM off; disk exclusion; storage crashes

- Execution date (UTC): 2026-10-09
- Executed by: development agent (spikes `spikes/f0-storage`, `spikes/f0-supervisor`)
- Roadmap step: section 15, step 4 of F0 ("baseline/overlay/firmware"); section 8; section 10 F0 ("immutable base/overlay/restore cycle with persistent firmware"); anticipates the crash tests of F3
- Related ADRs: ADR-0005, ADR-0002, ADR-0009, ADR-0017
- Overall outcome: **partial** (see results table)

## Purpose

Demonstrate on the first host, with real QEMU under WHPX:

1. the cycle immutable baseline → QCOW2 overlay plus writable copy of the firmware (OVMF vars) for the run → boot → stop → restore, with the baseline hash unchanged (section 8, points 3–6);
2. a disk exclusion mechanism that does **not** depend on QEMU image locking, which failed on Windows in `F0-02-qemu-whpx-qmp.md` (8.3);
3. the behaviour of storage after a kill during a write (overlay written by the guest, `qemu-img convert` interrupted);
4. a proposal for a journal and reconciliation (document only).

It does not demonstrate: installation of Ubuntu to disk (the guest is the live ISO), application consistency of the guest, the 30 cycles of the F3 exit criterion, protection against malicious processes of the same user or against the administrator.

## Environment

| Item | Value |
| --- | --- |
| Host | Intel Core i7-12800HX (16 cores / 24 threads), 31.6 GiB RAM, NTFS volume E: on Samsung SSD 970 EVO Plus 2 TB (NVMe), about 744 GiB free |
| OS and build | Windows 11 Pro 10.0.26200; non-administrator user, no elevation |
| Accelerator | WHPX (`query-accelerators` → `enabled: whpx`, verified at every start of the spike) |
| Toolchain | Rust 1.99.0 MSVC (Build Tools 2022 environment, procedure from `F0-01`), `windows-sys` 0.61.2, `uds_windows` 1.2.1, `sha2` 0.10.9, `serde_json` 1.0.151 |
| QEMU | 11.1.0 portable, `lab/qemu/qemu-11.1.0/` (SHA-256 of the binaries in `F0-02`, 2.2) |
| Firmware | `share/edk2-x86_64-code.fd` in read-only pflash (SHA-256 `33090cc0…6dbb2a`); NVRAM template `share/edk2-i386-vars.fd` (SHA-256 `5d2ac383…be5d1e`) |
| Guest | `ubuntu-26.04.1-desktop-amd64.iso`, SHA-256 `601e30fb…1bda1f` (verification in `F0-03`) |
| VM configuration | `-machine q35,pflash0=fwcode,pflash1=fwvars -accel whpx -cpu qemu64 -smp 2 -m 4096 -nodefaults -no-user-config -nic none`, ISO on `ide-cd`, overlay on `virtio-blk-pci` via `-blockdev driver=file` + `driver=qcow2`, `virtio-vga`, `-display none`, QMP over Unix socket in a private directory |
| Session | local, headless for QEMU |
| Other conditions | Other agents were running QEMU (F0-04 console) and WSL in parallel on the same host: the process counts distinguish the QEMU instances of this spike (command line with `-name lightvm-f0-05…`/`f0-06…`/`f0-08…`) from the host total |

## Procedure

Build (in a `cmd` with `vcvars64.bat` of Build Tools 2022, as in `F0-01`):

```text
cargo build --release -p f0-supervisor -p f0-storage
```

Commands run (logs in `lab/logs/`):

```text
target\release\f0-storage.exe   baseline-cycles --cycles 5 --boot-s 60   → f0-05-baseline-cycles.log
target\release\f0-supervisor.exe exclusion   → f0-05-exclusion-run1.log and f0-05-exclusion.log (two runs, identical outcomes)
target\release\f0-storage.exe   guest-write-crash --boot-s 170 --grow-mib 256    → f0-05-guest-write-crash-1.log
target\release\f0-storage.exe   guest-write-crash --boot-s 170 --grow-mib 1024   → f0-05-guest-write-crash-2.log
target\release\f0-storage.exe   guest-write-crash --boot-s 240 --grow-mib 1024   → f0-05-guest-write-crash-3.log
target\release\f0-storage.exe   convert-crash                            → f0-05-convert-crash.log
```

QEMU is always started by the spike with `CreateProcessW` and structured arguments (no shell), environment reduced to `SystemRoot`, inheritance of the three standard handles only and assignment to a Job Object at creation (details in `F0-06-process-supervisor.md`). The paths passed to QEMU are absolute and rejected if they contain commas (grammar of the `-blockdev` options).

### Layout of the baseline cycle

```text
lab/vms/f0-05/bl/
  baseline/base.qcow2      qemu-img create -f qcow2 base.qcow2 20G
  baseline/vars.fd         copy of share/edk2-i386-vars.fd ("baseline firmware")
  baseline/manifest.json   hashes of base, vars and firmware code
  run-<n>/overlay.qcow2    qemu-img create -f qcow2 -b ..\baseline\base.qcow2 -F qcow2 overlay.qcow2
  run-<n>/vars.fd          copy of baseline/vars.fd for the run
```

Throughout the series the spike keeps `baseline/base.qcow2` and `baseline/vars.fd` open with `OpenOptions::read(true).share_mode(FILE_SHARE_READ)`.

For each cycle: pre-boot hash of the run's overlay and vars; boot of the ISO with the run's overlay and vars for 60 s; `screendump` and `query-block`; stop with QMP `quit` (no guest data to save); post-stop hashes of base, baseline vars, overlay and run vars; `qemu-img check`; demonstrative write with the VM off with `qemu-io -c "write -P 0x5a 0 1M"` and check with `qemu-io -r -c "read -P 0x5a 0 1M"`; restore = deletion of the run's overlay and vars and new creation from the baseline, then `qemu-io -r -c "read -P 0 0 1M"` on the new overlay and comparison of the hash of the new vars with that of the baseline.

## Results

| Item | Outcome | Evidence |
| --- | --- | --- |
| 6a. Baseline (20G base + vars) with hash manifest | verified | `baseline/manifest.json`; base `221c49a7…ef338`, vars `5d2ac383…be5d1e` |
| 6b. Boot with the run's overlay and vars (5 cycles) | verified | overlay attached `ro=false` with backing `..\baseline\base.qcow2`; boot up to Plymouth at 60 s; `quit` in 250–270 ms, 0 leftovers |
| 6c. Hashes of base and baseline vars unchanged | verified | unchanged after each of the 5 cycles and at the end of the series |
| 6d. Run vars modified by the firmware | verified (observed fact) | changed in all 5 cycles, 6 839 bytes different out of 540 672, same resulting hash `39048a8f…` |
| 6e. Overlay modified by the live guest | not modified (observed fact) | hash and size (197 120 bytes) unchanged after 60 s of boot |
| 6f. Write to the overlay with the VM off and base unchanged | verified | `qemu-io` writes/reads back 1 MiB `0x5a`; base hash unchanged |
| 6g. Restore = new overlay + new vars from the baseline | verified | new overlay reads zeros like the base and is byte-for-byte identical to the pre-boot overlay; new vars = baseline hash |
| 7a. Share mode with which QEMU opens the images | verified (probe) | base and overlay opened by QEMU with `FILE_SHARE_WRITE`; explains the locking failure (`F0-02` 8.3) |
| 7b. Base protected by a supervisor handle with `FILE_SHARE_READ` | **verified** | QEMU reads the base; `qemu-io` write, `qemu-img resize`/`snapshot`/`commit`, rename and delete rejected (error 32); hash unchanged; holds whether opened before or after QEMU |
| 7c. Overlay reserved to QEMU alone via share mode | **failed** (not achievable) | share READ before QEMU prevents QEMU from starting; share READ after QEMU is rejected; share READ\|WRITE lets anyone write; only blocking of rename/delete is achieved |
| 7d. Detection of a pre-existing writer | verified | supervisor open rejected (error 32) while `qemu-io` keeps the base open |
| 7e. Baseline vars protected | verified | copy allowed, write rejected |
| 8.1 Kill of QEMU during a guest write to the overlay | verified (2 successful tests out of 3 attempts) | base unchanged in all cases; overlay: clean `check` in one case, 110 leaked clusters (harmless) in the other; 1 attempt without a write (input sent before the desktop) |
| 8.2 Kill of `qemu-img convert` | verified for the source; partial file **not recognizable** by `qemu-img check` | 5 kills, source unchanged, `check` of the partial files "No errors" |
| 9. Journal and reconciliation | proposal (document only) | section 9 |

## 6. Baseline/overlay/firmware cycle

Log: `lab/logs/f0-05-baseline-cycles.log`. Excerpt of cycle 1 (the other 4 are identical except for the times):

```text
baseline: sha256 base=221c49a70cfd327c299bb22fdaeeb3510e9da4acd70618e5c97b052141cef338 vars=5d2ac383371b408398accee7ec27c8c09ea5b74a0de0ceea6513388b15be5d1e
== ciclo 1
  qemu-img create overlay (backing ..\baseline\base.qcow2): (Some(0), "")
  pre-boot: overlay=e30d024c3288c762 vars_esecuzione=5d2ac383371b4083 (uguale alla baseline: true)
  dopo 60s: schermo=Some((1280, 800, 1.57…, 3072016)) disco=Some("ro=false backing=\"..\\baseline\\base.qcow2\"")
  quit: risposta=true uscita=Some(0) in 250 ms
  post-stop: base invariata=true vars_baseline invariato=true overlay cambiato=false (e30d024c3288c762) vars_esecuzione cambiato=true (39048a8f20a1e09e) overlay_byte=197120
  vars: 6839 byte diversi su 540672
  qemu-img check overlay: (Some(0), "No errors were found on the image.  Image end offset: 262144")
  qemu-io write 1 MiB 0x5a nell'overlay: wrote 1048576/1048576 bytes at offset 0
  qemu-io -r read -P 0x5a: read 1048576/1048576 bytes at offset 0
  base invariata dopo la scrittura nell'overlay=true
  ripristino: overlay nuovo legge zeri come la base: read 1048576/1048576 …; vars nuovo = baseline: true
  ripristino: overlay nuovo identico all'overlay pre-boot: true
…
RIEPILOGO: base finale invariata=true vars baseline invariato=true stop (ms): n=5 min=250.6 p50=262.3 p95=270.1 max=270.1 media=260.2
residui_spike=Some(0) qemu_totali_host=0
```

Facts reported:

- **Base and baseline vars:** identical hashes before the series, after each cycle and at the end; the files were kept open by the spike with `FILE_SHARE_READ` (section 7).
- **Run vars:** OVMF modifies them at every first boot (6 839 bytes different, presumably boot variables created for the CD-ROM; the content was not decoded). The resulting hash is the same in all 5 cycles, a sign of deterministic writes for this configuration. The firmware state therefore really is part of the persistent state and must be restored together with the overlay, as section 8 requires.
- **Run overlay:** the live session, without installation, does not write to the virtio disk: hash and size unchanged. Neither QEMU opening it for writing nor `quit` modified the qcow2 header. Writing was demonstrated with the VM off using `qemu-io`; writing by the guest is in test 8.1.
- **Restore:** deleting the run's overlay and vars and creating new ones from the baseline brings back a state identical to the pre-boot one (hash of the new overlay equal to that of the first overlay, `e30d024c…`; new vars equal to the baseline).
- **Boot state:** at 60 s the screen is at the boot logo (about 1.6 % non-black pixels): the cycle does not reach the desktop; for this test it is not necessary.

**Outcome: verified** for 5 cycles (not the 30 of the F3 criterion).

## 7. Disk exclusion via share mode (without QEMU locking)

Log: `lab/logs/f0-05-exclusion.log` (second run; the first, `f0-05-exclusion-run1.log`, gave the same outcomes). Directory `lab/vms/f0-05/excl/`: `base.qcow2` 20 GiB with 1 MiB written as `0x11`, `overlay.qcow2` with backing `base.qcow2`, `run-vars.fd`. QEMU started with overlay and vars in pflash, without ISO (the firmware finds no bootable disk and keeps running), state verified with `query-named-block-nodes`.

### 7.1 Control without protection (T0) and QEMU share mode

QEMU opens the base as backing with `ro=true` and the overlay with `ro=false`. With QEMU alive and no spike handle:

| Test open while QEMU is running | `base.qcow2` | `overlay.qcow2` |
| --- | --- | --- |
| access R, share READ\|WRITE\|DELETE | succeeded | succeeded |
| access RW, share READ\|WRITE\|DELETE | **succeeded** | **succeeded** |
| access R, share READ | succeeded | error 32 (file in use) |
| access R, share READ\|WRITE | succeeded | succeeded |

Deduction from the tests: QEMU 11.1.0 on Windows opens both the base (for reading) and the overlay (for reading/writing) with a share mode that **includes `FILE_SHARE_WRITE`**; it does not include `FILE_SHARE_DELETE` (deleting the overlay in use fails, 7.2). This explains the locking failure in `F0-02`: any process of the same user can open the images in use for writing. Confirmation: `qemu-io -c "write -P 0xee 0 64k" base.qcow2` → `wrote 65536/65536`, base SHA-256 changed from `2abdebbf…` to `109dc3c9…`.

### 7.2 Base opened by the supervisor with `FILE_SHARE_READ` (T1: before QEMU; T1b: after)

```rust
let guard = OpenOptions::new().read(true).share_mode(FILE_SHARE_READ).open(&base)?;
```

| Operation with QEMU running and spike handle open | Outcome |
| --- | --- |
| QEMU start with overlay (the base is opened by QEMU for reading) | succeeded; nodes `base.qcow2 ro=true`, `overlay.qcow2 ro=false` |
| `qemu-img info base.qcow2` | succeeded |
| `qemu-io -r -c "read -P 0x11 0 64k" base.qcow2` | succeeded |
| `qemu-io -c "write -P 0xee 0 64k" base.qcow2` | **rejected**: `Could not open 'base.qcow2': … Il file è utilizzato da un altro processo.` (exit 1) |
| `qemu-img resize base.qcow2 +1G` | **rejected** (exit 1, same error) |
| `qemu-img snapshot -c s1 base.qcow2` | **rejected** (exit 1) |
| `qemu-img commit overlay.qcow2` (would write to the base) | **rejected**: `Could not reopen 'base.qcow2'` (exit 1) |
| `qemu-img check base.qcow2` | succeeded (read-only) |
| Rename of the base | **rejected** (error 32) |
| Deletion of the base | **rejected** (error 32) |
| RW open with share READ\|WRITE\|DELETE from another handle | **rejected** (error 32) |
| Setting the read-only attribute | **succeeded**: the share mode does not protect attributes (`FILE_WRITE_ATTRIBUTES`) |
| QEMU after all attempts | still running |
| Base SHA-256 before/after T1 | `2abdebbf8d18dcb80ff1ef7905460ad3cb6c82f5ce4adb36032d2fabb61615fa` / identical |

T1b (handle opened **after** QEMU starts): the spike's open with share READ succeeds, because QEMU holds the base for reading only; a write with `qemu-io` is then rejected in the same way. Both sequences work for the base.

### 7.3 Overlay: the share mode cannot allow "QEMU only"

| Sequence | Outcome |
| --- | --- |
| T2a: spike opens the overlay (R, share READ\|WRITE) **before** QEMU | QEMU starts; `qemu-io` writes to the overlay (exit 0), `qemu-img resize overlay +1G` succeeds; deletion fails (error 32) |
| T2b: spike opens the overlay (R, share READ) **before** QEMU | **QEMU does not start**: exit 1, `-blockdev driver=file,node-name=ovlfile,…: Could not open '…overlay.qcow2': … Il file è utilizzato da un altro processo.` |
| T3a: spike opens the overlay (R, share READ\|WRITE) **after** QEMU | open succeeded; `qemu-io` writes, `qemu-img resize` succeeds; deletion rejected |
| T3b: spike opens the overlay (R, share READ) **after** QEMU | spike open **rejected** (error 32): QEMU already has write access |
| Without spike handle (T1) | write and resize of the overlay in use succeeded; deletion rejected by QEMU's share mode |

The share mode is a property of the open, not of the process: it cannot distinguish QEMU from another process. Since QEMU needs to write the overlay and itself opens it with `FILE_SHARE_WRITE`, no combination of supervisor opens (before or after) prevents a third process of the same user from writing the overlay in use. Only protection against rename/delete is achieved (absence of `FILE_SHARE_DELETE`, already guaranteed by QEMU's own open and, independently of the QEMU build, by a supervisor handle without `FILE_SHARE_DELETE`).

### 7.4 Pre-existing writer (T4) and baseline vars (T5)

- T4: interactive `qemu-io -f qcow2 base.qcow2` (base opened for reading/writing) started first; the spike's open with share READ **fails** with error 32. After `qemu-io` is closed the open succeeds. The supervisor can therefore use the exclusive open as a detector: if it fails, someone already has the baseline open for writing and the VM must not start. Note: `qemu-io` with stdin on a pipe did not process `quit`; in the first run it was terminated manually (`Stop-Process` on the spike's child PID), in the second the spike terminates it by itself after 2 s.
- T5: with `baseline-vars.fd` opened by the spike (R, share READ), the copy for the run succeeds (540 672 bytes) and opening the baseline vars for writing is rejected (error 32).

### 7.5 Limits of the exclusion

- It holds as long as the supervisor is alive and holds the handle; when it dies (or closes the handle) the protection ends. With the Job Object QEMU dies too (`F0-06`), so no QEMU is left running on an unprotected base, but the base remains writable by other processes between one run and the next if the supervisor is not running: the read-only ACL/attribute and hash verification before every use are also needed.
- The same user can terminate the supervisor and change attributes and ACLs of their own files; the administrator can bypass everything. It is a protection against mistakes and concurrent tools, not a security boundary (consistent with section 7.1).
- Handles already open before the supervisor: detected only if they have write access (T4); a pre-existing reader with a share without READ would make the supervisor's open fail in a similar way (not tested).
- Memory mapping or volume-level access (`\\.\E:`, administrator) were not tested.
- The overlay in use is not protected against writes by third parties (7.3).

## 8. Storage crashes

### 8.1 Kill of QEMU during a guest write to the overlay

Logs: `lab/logs/f0-05-guest-write-crash-1.log`, `-2.log`, `-3.log`; screenshots in `lab/vms/f0-05/priv/` (`f0-05-gw-*.ppm`, PNG conversion of those from the first attempt).

Procedure (`f0-storage guest-write-crash`): 20 GiB base with 64 MiB written as `0x11` (SHA-256 `1be91d9b…d2d310`), kept open by the spike with `FILE_SHARE_READ`; new overlay and vars; boot of the live session; `send-key` `ctrl-alt-t` via QMP to open a terminal; typing via `send-key` (US layout) of:

```text
sudo dd if=/dev/urandom of=/dev/vda bs=1M count=8000 oflag=direct status=progress
```

`/dev/vda` is the `virtio-blk` disk attached to the test overlay (no real disk is attached to the VM). Every 250 ms the spike checks the size of the overlay on the host and, once the threshold is exceeded, performs the Emergency Stop (`TerminateJobObject`) while `dd` is running. Then `qemu-img check`, `qemu-img info`, base hash and `qemu-io -r -c "read -P 0x11 0 64M"` on the base.

| Attempt | Wait before input | Write outcome | Stop | `qemu-img check` of the overlay after the kill | Base |
| --- | --- | --- | --- | --- | --- |
| 1 | 170 s (desktop, 99.9 %) | overlay from 197 120 to 327 483 392 bytes in 6.3 s; screenshot: terminal with `sudo dd …` running | 431 ms | exit 0, "No errors", image end offset 327 680; `info`: disk size 47.3 MiB, file length 312 MiB | hash unchanged, 64 MiB `0x11` read back |
| 2 | 170 s (screen still booting, 1.5 %) | **no write**: input sent before the desktop and lost; overlay unchanged | Emergency Stop at the end of the wait | exit 0, "No errors" | hash unchanged |
| 3 | 240 s (desktop, 99.9 %) | overlay from 197 120 to 1 132 920 832 bytes in 9.6 s | 536 ms | **exit 3**: "110 leaked clusters were found on the image. This means waste of disk space, but no harm to data."; 16 244 clusters allocated | hash unchanged, 64 MiB `0x11` read back |

Facts: the base never changed; the interrupted overlay remains openable and consistent for `qemu-img`, but the data written after the last metadata update is lost (attempt 1: the written clusters are not referenced) or leaked clusters remain (attempt 3). Consistent with section 8: after a forced stop the overlay is not a clean state and must not be promoted to baseline. **Outcome: verified** (2 effective tests); sending input before the desktop must be avoided or detected (attempt 2 is reported as failed for the write part).

### 8.2 Kill of `qemu-img convert`

Log: `lab/logs/f0-05-convert-crash.log`. Source `src.qcow2` of 8 GiB virtual with 2 GiB written (8 blocks of 256 MiB with patterns `0x21`–`0x28`), 2 148 073 472 bytes on disk, SHA-256 `f4044150…7cc3a`. Full reference convert: 1 902 ms, `qemu-img compare` → `Images are identical.`

Five kills (`TerminateProcess` via `Child::kill`) at increasing fractions of the reference time, destination with suffix `.partial`:

| # | Kill after (ms) | Bytes of the partial file | `qemu-img check` | `qemu-img compare` with the source | Source unchanged |
| --- | --- | --- | --- | --- | --- |
| 1 | 95 | 67 436 544 | No errors (image end offset 327 680) | Content mismatch at offset 0 | yes |
| 2 | 285 | 304 414 720 | No errors (image end offset 327 680) | Content mismatch at offset 0 | yes |
| 3 | 570 | 598 081 536 | No errors, 6.25 % allocated | mismatch at 536 870 912 | yes |
| 4 | 950 | 981 860 352 | No errors, 6.25 % allocated | mismatch at 536 870 912 | yes |
| 5 | 1 426 | 1 174 863 872 | No errors, 12.50 % allocated | mismatch at 1 073 741 824 | yes |

**Outcome: verified** for the source (identical hash after every kill). Relevant fact: **the partial file passes `qemu-img check` without errors** and `qemu-img info` describes it as a valid 8 GiB qcow2 with `corrupt: false`; only `compare` reveals the difference. An interrupted image is therefore not recognizable from its content: it must be recognized by a temporary name (`.partial`) and the journal, and promoted with an atomic rename only after verified completion (see 9).

## 9. Journal and reconciliation (proposal, not implemented)

Problem (section 6.3): operations that touch the DB and the filesystem (baseline creation, new overlay, restore, deletion, checkpoint materialization) are not atomic together; a crash halfway through must lead to a safe state without touching the baseline.

Principles:

1. **Intent before effect.** Every operation writes to the DB a `pending` row with all the paths it will create, modify or delete, before touching the filesystem; the SQL commit of the intent precedes the first file action.
2. **New files only with temporary names** in the VM's managed directory (`<op_id>.partial`), created with `CREATE_NEW` (no overwrite, no following of reparse points), then `FlushFileBuffers` and atomic rename (`MoveFileExW` with `MOVEFILE_REPLACE_EXISTING` only where intended, `MOVEFILE_WRITE_THROUGH`) to the final name.
3. **Two-step deletions:** rename into `trash/<op_id>/…` (atomic on the same volume), commit of the `done` state, then physical removal; reconciliation completes any leftover removals.
4. **The baseline is never a write destination:** the only way to change a baseline is to create a new one (section 8); the hash recorded in the manifest is verified before every use and after every reconciliation.
5. **Reconciliation at supervisor startup, before accepting commands**, under the single instance of the catalog.

Minimal schema (SQLite):

```sql
CREATE TABLE journal (
  op_id        TEXT PRIMARY KEY,          -- UUID of the operation (operation_id)
  vm_id        TEXT NOT NULL,
  run_id       TEXT,                      -- run it refers to, if any
  kind         TEXT NOT NULL,             -- 'create_baseline' | 'new_overlay' | 'restore' | 'delete_baseline' | 'materialize'
  state        TEXT NOT NULL,             -- 'pending' | 'fs_done' | 'done' | 'rolled_back' | 'failed'
  created_at   TEXT NOT NULL,
  updated_at   TEXT NOT NULL,
  error        TEXT
);
CREATE TABLE journal_file (
  op_id        TEXT NOT NULL REFERENCES journal(op_id),
  role         TEXT NOT NULL,             -- 'create' | 'replace' | 'delete' | 'read_only_input'
  temp_path    TEXT,                      -- .partial path or in trash/
  final_path   TEXT NOT NULL,             -- relative to the managed root of the catalog
  expected_sha256 TEXT,                   -- for inputs (baseline, vars) and for verified results
  PRIMARY KEY (op_id, final_path)
);
```

Reconciliation rules:

| State found | Action |
| --- | --- |
| `pending`, temporary files present, no final file | remove the `.partial` files (even if `qemu-img check` declares them valid: 8.2), state `rolled_back` |
| `pending`, final file present and hash/`qemu-img check` consistent with the expected | complete (`done`); otherwise move the final file to quarantine and report |
| `fs_done` | update the catalog rows and mark `done` (idempotent) |
| deletion with files in `trash/<op_id>` | complete the physical removal |
| `read_only_input` input with a hash different from the manifest | block the VM (`state_conflict`), no automatic repair of the baseline |
| file in the managed directory referenced by neither catalog nor journal | list it as an orphan; removal only after confirmation or beyond an age threshold, never for baseline files |

Restore (section 8) becomes: `restore` intent with `delete` of the run's overlay and vars and `create` of the new ones from `read_only_input` = baseline + baseline vars; new `run_id`; requests with the old `run_id` are rejected. Before executing, the per-VM queue checks that the VM is `Stopped` and that the exclusive open (7.2) of the baseline succeeds.

## Limits

- A single host, a single QEMU build, live guest without installation: the live session does not write to the disk on its own; the write to the overlay was obtained with `qemu-io` with the VM off and, in test 8.1, with a command typed in the guest.
- 5 cycles, not the 30 required by the F3 exit criterion.
- Stops with QMP `quit` (equivalent to a forced stop for the guest): the consistency of a guest filesystem is not evaluated.
- Exclusion via share mode protects the baseline only while the supervisor is alive and is not a security boundary against the same user (7.5); the overlay in use remains writable by third parties.
- Multi-user and ACL tests not executed (no second account).
- After the tests, the 1.1 GB overlay of test 8.1 and the files of `lab/vms/f0-05/cv/` were removed for space; the logs with hashes, sizes and outputs remain.

## Required actions

1. Decide how to protect the overlay in use (see Proposed decisions, point 3): per-run directory with an explicit ACL created by the supervisor, or a distinct process identity for QEMU (restricted token/AppContainer), to be tested in F1; or accept and declare the limit.
2. Carry over into the F1/F3 backlog (`docs/RELEASE_PLAN.md`): journal (9), exclusive open of the baseline, `.partial` names with atomic rename, baseline hash verification before every start, 30 cycles with a real installation to disk.
3. No administrator action required by this test.

## Proposed decisions

For **ADR-0005** (baseline with the VM off):

1. A baseline comprises `base.qcow2` **and** the copy of the OVMF vars; restore recreates both (the run's overlay and vars) from the baseline, never reusing those of the previous run.
2. Fixed relative backing file (`..\baseline\base.qcow2`, explicit format `-F qcow2`), verified with `qemu-img info --backing-chain` before every start; no chain beyond two levels.
3. Exclusion: the supervisor opens the baseline and baseline vars with `FILE_SHARE_READ` (read-only access) for the whole lifetime of the catalog or at least for every run and storage operation; a failure of the open blocks the start (`vm_busy`/`state_conflict`). QEMU locking is not used as a guarantee. For the overlay: supervisor handle without `FILE_SHARE_DELETE` (prevents rename/delete) and per-run directory with a restricted ACL; declare as a limit that a process of the same user can write the overlay in use.
4. `qemu-img` operations on the baseline (creation, materialization) write to `.partial` and promote with an atomic rename after `qemu-img check` **and** comparison (`compare`) or hash, because `check` alone does not detect an interrupted convert.
5. Journal and reconciliation according to the schema in section 9.

For **ADR-0017**: the storage budget must count overlays, per-run vars (540 672 bytes each), `.partial` files and `trash/` until reconciliation.
