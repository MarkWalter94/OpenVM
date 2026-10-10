# F1-11 — Second user on the IPC endpoint (P-5, Linux part in a VM) and read-only data medium

- Execution date (UTC): 2026-10-10, from about 11:15 to 11:25
- Executed by: development agent (Claude Code session), on the direction of the project owner ("U-3 lo fai facendo una VM")
- Roadmap step: section 7.4 (rows "Sessions" and "IPC/console": another local user does not reach the endpoint), 4 and 6.3 (read-only data medium); F0 pending items **P-5** (Linux part) and **P-7** (write test from the guest)
- Related ADRs: ADR-0002, ADR-0008, ADR-0016 (decision not modified)
- Overall outcome: **partial** — Linux part of P-5 **verified in an Ubuntu VM on Windows** (validity **[processes]**: nested VM, ADR-0016); Windows part (named pipe with ACL and a second Windows account) **not executed**; writing the data medium from the guest **verified impossible**

Allowed outcomes: **verified**, **failed**, **not executed** (section 17). Raw logs in `lab/logs/` (outside the repository): `p5-linux-vm.txt`, `p5-linux-vm-2.txt`, `f1-03/s1791631100127-p5-*.png`, `f1-03/s1791631282989-p5-*.png`.

## Purpose

1. P-5 (Linux): with the service's IPC transport (`lightvm-service::transport`, Unix socket), a second local user must not reach the endpoint: neither with the permissions set by the service, nor with permissions deliberately loosened (`SO_PEERCRED` check).
2. ADR-0008: the guest cannot write the data medium mounted as a read-only CD-ROM.

It does not demonstrate: the Windows part of P-5 (pipe with security descriptor, different SID), the complete service on Linux (catalog, QEMU, cgroup: R2a), a physical Linux host.

## Environment

| Item | Value |
| --- | --- |
| Host | Windows 11 Pro 10.0.26200, i9-14900K, QEMU 11.1.0 with WHPX (as in `F1-03`) |
| VM | Ubuntu 26.04.1 desktop live (ISO SHA-256 `601e30fb…1bda1f`), `-cpu Skylake-Client-v4`, 2 vCPUs, 4096 MiB, data ISO on the second IDE CD-ROM (`ide.1`, `read-only=on`) |
| Tool | `lightvm-peer-check` (`crates/lightvm-service/src/bin/lightvm-peer-check.rs`), same `transport` module as the service, built from Windows for `x86_64-unknown-linux-musl` (static, linked with `rust-lld`; `rustup target add` in this session) |
| Data ISO | `lab/media/p5-dati.iso` generated with `pycdlib` 1.14.0 (ISO 9660 level 3 + Rock Ridge): `prova.sh` (`crates/lightvm-service/scripts/p5-guest-test.sh`) and `lvmpeer`; signature `CD001`; SHA-256 `0c5ee3904d591a22fa989545598ef742e64ec184014555f0dc9ea5c64dcab6f3` (second run) |
| Users in the guest | `ubuntu` (uid 1000, passwordless sudo of the live session), `altro` (uid 1002, created by the script) |

## Procedure

```text
set CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld
cargo build --release -p lightvm-service --no-default-features --bin lightvm-peer-check --target x86_64-unknown-linux-musl
python -I make_p5_iso.py lab\media\p5-dati.iso        (script in the session scratchpad)
set LIGHTVM_QEMU_DIR=<workspace>/lab/qemu/qemu-11.1.0
cargo test -p qemu-backend --test real_qemu -- --ignored --nocapture reale_p5
```

The test `reale_p5_secondo_utente_linux_in_vm` starts the VM, waits for the desktop (130 s), opens a terminal with Ctrl+Alt+T and types via QMP (`input-send-event`, US layout) `sudo mount -o ro /dev/sr1 /mnt && bash /mnt/prova.sh`; after 20 s it saves the screenshot of the outcome, which is read to record the result. At the end: Emergency Stop and check that there are no orphan processes.

## Results

| Item | Outcome | Evidence (screenshot `…-p5-esito.png`) |
| --- | --- | --- |
| Permissions set by the service | verified | `drwx------ ubuntu /tmp/lvm-ipc`, `srw------- ubuntu …/svc-<casuale>.sock` |
| Same user | verified | `CLIENT CONNESSO`; server `ACCETTATO identita=uid=1000` |
| **Other user with the service's permissions** | verified | `CLIENT ERRORE Permission denied (os error 13)` |
| **Other user with deliberately loosened permissions** (`chmod 777` on directory and socket) | verified | `CLIENT CHIUSO DAL SERVER`; server `RIFIUTATO UID del peer 1002 diverso da 1000` |
| Data ISO mounted and used in the guest (ISO 9660 + Rock Ridge format, execute permissions preserved) | verified | the script and the tool were run from `/mnt` |
| **Writing to the data medium from the guest** (ADR-0008) | verified impossible | `touch: cannot touch '/mnt/scrittura': Read-only file system`; remount for writing failed; `dd: failed to open '/dev/sr1': Read-only file system`; SHA-256 of the ISO on the host identical before and after |
| No orphan process after the test | verified | `assert_no_orphan` passed, 2 runs |
| Windows part of P-5 (second Windows account on the named pipe) | not executed | no second account and no Windows image for a VM; the SID check is in the code (`transport/windows.rs`) and the low-integrity rejection is verified in `F1-04` |

## Limits

- Nested VM on Windows: valid for the operating system's process and permission mechanisms **[processes]**, not for the Ubuntu cell of the matrix (ADR-0016, section 10).
- The service's transport was tested, not the complete service on Linux (process containment on Linux is not yet implemented).
- One sample per case (two complete runs).

## Required actions

1. Windows part of P-5: a second non-administrator Windows account on the host (created by the owner: an agent does not create accounts) or a licensed Windows VM; alternatively record it as a limit.
2. Repeat the Linux part on the physical Linux server in R2a with the complete service.

## Proposed decisions

No new decision: the results confirm ADR-0002 (local IPC authenticated for the user only) and the data medium details of ADR-0008.
