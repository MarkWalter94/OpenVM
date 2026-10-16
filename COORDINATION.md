# Coordinamento Codex / Claude — 2026-10-17

Il proprietario ha assegnato Windows 11 a Claude e ha chiesto a Codex di
organizzare il lavoro restante senza sovrapposizioni. Questo file è il
punto di passaggio condiviso; non attesta una conferma già ricevuta da Claude.

## Claude: Windows 11

- Installazione, riavvii WHPX, XSAVE, patch e bundle diagnostici QEMU.
- `packaging/qemu-win11/`, `crates/qemu-backend/src/vcpu_fault.rs`,
  `crates/qemu-backend/tests/qmp_mock.rs`.
- `crates/lightvm-service/tests/windows11_real.rs` e fixture Windows in
  `crates/lightvm-service/tests/common/`.
- Evidenze Windows: `R6-05`, `R2c-01`, parti Windows di STATUS/compatibilità.
- Codex non avvia altre prove Windows 11 e non modifica questi file.

Passaggio già disponibile: checkpoint TPM implementati nel commit `29c9d15`.
La prova reale Windows PE ha verificato creazione NV → modifica → ripristino
snapshot → modifica → ripristino baseline, sempre con il valore originale.
Log: `E:\OpenVM-tests\win11\checkpoint-real2.log` (158,95 s, passato).
Artefatti: `E:\OpenVM-tests\lab\w11\b31c5cf10a37\vms\01a14519-3dd1-74d2-9462-fdd1310b2693`.
Il precedente test Codex di installazione/ripresa è terminato; non è lasciata
una VM Codex Windows 11 attiva. Disco conservato in
`E:\OpenVM-tests\lab\win11-real\installed-01a1450c-537e-717b-832a-650f7377c392`.

## Codex: rete filtrata, Tools e distribuzione

1. Completare le prove R1 attraverso il prodotto: avvio del servizio,
   risoluzione, grant agente, traffico guest reale e revoca.
2. Qualificare i Tools su Linux installato e il flusso applicazione/log/
   ripristino, senza utilizzare o modificare i dischi Windows di Claude.
3. Preparare inventario e pacchetto di distribuzione, distinguendo i blocchi
   esterni: firma e revisione indipendente non disponibili.
4. USB: avanzamento del software possibile, prove fisiche ancora sospese per
   istruzione del proprietario. Nessuna installazione UsbDk o cattura hardware.

Il file `crates/guest-net/tests/real_guest.rs` e le evidenze R1 risultano già
modificati da Claude: Codex li considera condivisi in sola lettura e userà
un nuovo test per l'integrazione attraverso il prodotto. Nessun reset, stash
o commit globale delle modifiche dell'altro agente.

Prova Codex: `crates/lightvm-service/tests/common/filtered_product.rs`,
con un solo collegamento aggiunto in `tests/f4_real.rs`; fixture guest
`common/filtered_probe.py`. VM Linux: 2 vCPU / 6144 MiB, cataloghi separati
in `E:\OpenVM-tests\lab\r1-product`. Prova finale PASSATA (273,88 s), log
`E:\OpenVM-tests\r1-product-final.log`, evidenze `5180ba24db77`.
HTTPS con grant, blocchi di rete anche dopo modifica route, revoca TLS,
build applicazione Python e rientro di artefatto/log/checksum verificati.
Dettagli e limiti: `docs/verification/R1-02-product-path.md`.
Packaging Codex: `packaging/windows/build-package.ps1` e
`packaging/windows/embedded-fonts.ps1` (licenze complete dei font incorporati).
Test installazione/disinstallazione e protezione junction PASSATI:
`E:\OpenVM-tests\packaging-fonts-install-test2.log`.
Build sperimentale non firmata PASSATA in `E:\OpenVM-tests\package-20261017`,
da copia del commit `29c9d15` con sole correzioni packaging Codex. Non include
le modifiche Windows 11 ancora in lavorazione; nessuna build nella checkout
condivisa. Linux installato/ripristino e firma/revisione restano da qualificare.
ZIP: `dist/LightVM-0.0.0-alpha.20261017-windows-x64.zip`, SHA-256
`cfac788fcc069036823fb9bead0e4ac86d026c931aa5c27224bdc9e26650aea7`.
SBOM 257 componenti, scansione artefatti senza rilievi. Installazione reale
in prefisso privato su E:, tre CLI `--help` e disinstallazione PASSATI;
PATH e file utente preservati. Evidenza `docs/verification/F6-02-embedded-font-notices.md`.

Tutti i dischi, build, temporanei e artefatti di prova devono restare su E:.
Le modifiche a file condivisi (`STATUS.md`, manifest Cargo, schema) richiedono
prima il controllo del diff corrente e interventi circoscritti.
