# ADR-0011 — Release guest e firmware

- Stato: Accettata
- Data: 2026-10-09
- Responsabile: da assegnare

## Contesto

L'MVP ha un solo guest di riferimento: Ubuntu LTS x86_64, con versione e checksum fissati in F0 (roadmap, sezione 4). Il modello di baseline include lo stato firmware scrivibile (UEFI NVRAM) (sezione 8). Il test dimostrativo è grafico (sezione 9), quindi il guest deve avere un ambiente desktop.

## Decisione

**Guest di riferimento**

| Voce | Valore |
| --- | --- |
| Release | Ubuntu 26.04.1 LTS (Resolute Raccoon), variante desktop amd64 |
| File | `ubuntu-26.04.1-desktop-amd64.iso`, 6 482 409 472 byte |
| SHA-256 | `601e30fbf5d97759367c632e2c33630665039b7e2158fd068403da3ccf1bda1f` |
| Catena di fiducia | `SHA256SUMS` firmato dalla chiave `8439 38DF 228D 22F7 B374 2BC0 D94A A3F0 EFE2 1092` (Ubuntu CD Image Automatic Signing Key 2012), impronta confrontata con la pagina ufficiale ubuntu.com; keyring dedicato |
| Identità | fissata dall'hash, non dall'URL o dal nome del file |

**Firmware**

| Voce | Valore |
| --- | --- |
| Tipo | UEFI x86_64, OVMF/edk2 fornito con la build QEMU di ADR-0010 |
| Codice | `share/edk2-x86_64-code.fd`, 3 653 632 byte, SHA-256 `33090cc07675baa5190d9f1e84bf5176b33bcbfa9bacac522961150cdb6dbb2a`, in `pflash0` con `read-only=on` |
| Template variabili | `share/edk2-i386-vars.fd`, 540 672 byte, SHA-256 `5d2ac383371b408398accee7ec27c8c09ea5b74a0de0ceea6513388b15be5d1e` |
| Variabili per baseline ed esecuzione | la baseline contiene una copia del template; ogni esecuzione usa una **copia del vars della baseline**, in `pflash1`, ricreata a ogni ripristino (ADR-0005) |
| Secure Boot | disattivo nel profilo di riferimento: non necessario per Ubuntu (catena shim/GRUB presente); la variante `edk2-x86_64-secure-code.fd` con SMM non è stata provata ed è un test facoltativo |
| BIOS legacy | escluso: SeaBIOS con `-cpu host` sotto WHPX va in pausa con `WHPX: Unexpected VP exit code 4`, e il modello di consistenza richiede la NVRAM UEFI |

**Profilo VM di riferimento**

- Tipo macchina: `pc-q35-11.1` (le prove hanno usato l'alias `q35`, che in questa build risolve a `pc-q35-11.1`; il prodotto usa il nome versionato).
- CPU: **`-cpu qemu64`**, 2 vCPU; RAM 4 GiB (sezione 13).
- Dispositivi: `-nodefaults -no-user-config -nic none`, `virtio-vga`, `virtio-tablet-pci`, `virtio-keyboard-pci`, ISO su `ide-cd` in sola lettura, disco su `virtio-blk-pci` tramite `-blockdev` con formato esplicito.
- Risoluzione osservata del guest: 1280×800.

**Deviazione dichiarata (CPU):** `-cpu host` e `-cpu max` bloccano OVMF prima dell'inizializzazione del display su questo host; i modelli nominali `Skylake-Client-v4`, `Haswell-v4` e `Nehalem` arrivano a GRUB, `qemu64` arriva al desktop. La causa non è determinata. Il profilo usa `qemu64` finché un modello con più estensioni non supera un boot completo; la scelta va ripetuta dopo l'azione utente sulla feature `HypervisorPlatform` (`F0-09`, U-1).

Verifica dei criteri:

1. Release LTS supportata con checksum e firma verificabili: soddisfatto.
2. Installazione e boot dal disco sotto WHPX: **soddisfatto in parte**. Il boot dal media fino al desktop live è verificato; l'installazione su disco e il boot dal disco non sono stati eseguiti (backlog F1/F2). Se falliscono, questo ADR torna a Proposta.
3. Firmware con stato persistente gestibile come file separato: soddisfatto (vars per esecuzione modificato dal firmware e ripristinato dalla baseline).
4. Ambiente grafico adatto alla fixture senza rete: soddisfatto (ADR-0019).

## Alternative considerate

- Ubuntu 24.04.x LTS: disponibile, ma non è la LTS più recente con una point release.
- Variante server con ambiente grafico aggiunto: richiede pacchetti scaricati, incompatibile con il profilo offline.
- BIOS legacy (SeaBIOS): escluso (vedi sopra).
- Firmware da pacchetto separato: non necessario; il firmware della build QEMU ha versione e hash registrati.
- `-cpu host`: richiesto in prima istanza, fallito su questo host (deviazione).

## Conseguenze

- La matrice in `docs/COMPATIBILITY.md` usa esattamente questi valori nelle colonne guest, firmware e tipo macchina.
- Un cambio di release guest, firmware o modello CPU richiede nuove prove e una nuova baseline.
- Una nuova point release della stessa LTS si adotta solo dopo checksum, firma e boot verificati; una nuova LTS si valuta dopo la sua prima point release. Un avviso di sicurezza rilevante impone l'aggiornamento (sezione 17).
- Le prestazioni del guest con `qemu64` (poche estensioni CPU, desktop live tra 105 e 120 s) non sono rappresentative di un modello CPU completo.

## Prove richieste

- Procura e verifica della ISO: `docs/verification/F0-03-guest-media.md` (verificato).
- Boot con firmware sotto WHPX e diagnostica del modello CPU: `docs/verification/F0-02-qemu-whpx-qmp.md` §9 (verificato con `qemu64`; fallito con `host`).
- Persistenza e ripristino della NVRAM nella baseline: `docs/verification/F0-05-baseline-overlay-firmware.md` §6 (verificato, 5 cicli).
- Installazione su disco e boot dal disco: da eseguire (backlog F1/F2 in `docs/RELEASE_PLAN.md`).

## Riferimenti

Roadmap, sezioni 4, 8, 9, 10 (F0), 13, 17. Riesame: `docs/verification/F0-09-riesame-prove.md`.
