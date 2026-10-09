# ADR-0018 — Hardware di riferimento

- Stato: Accettata
- Data: 2026-10-09
- Responsabile: da assegnare

## Contesto

Le misure della sezione 13 usano un riferimento iniziale: host fisico con almeno 4 core/8 thread, 16 GiB RAM e SSD, guest Linux con 2 vCPU/4 GiB, display 1280×720, almeno 100 GiB liberi prima di installazione e baseline. F0 registra CPU, OS, storage e capacità reali del laboratorio.

## Decisione

L'host di sviluppo è il **primo host di misura Windows**. Caratteristiche registrate con comando e output nei file di verifica:

| Voce | Valore registrato | Riferimento sezione 13 | Evidenza |
| --- | --- | --- | --- |
| Macchina | HP ZBook Fury 16 G9 Mobile Workstation (portatile) | — | `F0-01` §2 |
| OS | Windows 11 Pro 10.0.26200, build 26200 | Windows 11 x86_64 | `F0-01` §2; `F0-08` |
| CPU | 12th Gen Intel Core i7-12800HX, 16 core / 24 processori logici (architettura ibrida) | ≥ 4 core / 8 thread | `F0-01` §2; `F0-08` |
| RAM | 31,6 GiB; limite di commit 63,6 GiB | ≥ 16 GiB | `F0-08` |
| Storage del laboratorio | E: NTFS su Samsung SSD 970 EVO Plus 2 TB (NVMe), 744,2 GiB liberi | SSD, ≥ 100 GiB liberi | `F0-08` |
| GPU | NVIDIA RTX A2000 8GB Laptop (driver 32.0.15.9658) e Intel UHD Graphics | — | `F0-01` §2 |
| Virtualizzazione | Hyper-V attivo, VBS/HVCI in esecuzione, WHPX funzionante (`query-accelerators` → `whpx`); feature `HypervisorPlatform` riportata *Disabled* via CIM | WHPX disponibile | `F0-02` §1, §4.1 |

L'host supera il riferimento; le misure si eseguono con la configurazione guest del riferimento (2 vCPU / 4 GiB) e la risoluzione effettiva del guest registrata (1280×800 osservata).

Verifica dei criteri:

1. Caratteristiche registrate con comando e output: soddisfatto.
2. Stato di virtualizzazione e WHPX verificato: soddisfatto, con la discrepanza CIM dichiarata.
3. Condizioni di misura dichiarate: **soddisfatto in parte**. Le prove F0 dichiarano sessione locale, host condiviso con altri agenti (QEMU paralleli, WSL) e stato della sessione Windows; profilo energetico, alimentazione e antivirus non sono stati registrati. Ogni misura formale (F1 e successive) li registra.

## Alternative considerate

- Host dedicato al laboratorio, separato dalla macchina di sviluppo: preferibile per misure stabili e per guest non fidati (sezione 13); non disponibile in F0.
- Host Linux fisico con KVM: non disponibile (ADR-0016).

## Conseguenze

- La CPU ibrida richiede misure normalizzate per host, con la nota dell'architettura; i confronti con altri host vanno fatti con cautela.
- È un solo host Intel: F6 richiede almeno due host fisici del target certificato, inclusi Intel e AMD se entrambi dichiarati (sezione 10 F6).
- È un portatile: alimentazione e profilo termico vanno registrati in ogni misura formale.
- Le misure F0 sono state eseguite con carichi paralleli di altri agenti: le latenze non sono misure di release.
- L'host esegue anche lo sviluppo: i runner con guest non fidati non devono contenere credenziali di produzione (sezione 13).

## Prove richieste

- `docs/verification/F0-01-toolchain-eframe.md` §2, `docs/verification/F0-02-qemu-whpx-qmp.md` §1, `docs/verification/F0-08-limiti-risorse-misure.md` "Capacità del laboratorio" (verificato).

## Riferimenti

Roadmap, sezioni 10 (F0, F6), 13, 14, 17. Riesame: `docs/verification/F0-09-riesame-prove.md`.
