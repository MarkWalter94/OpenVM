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

## Claude: stato e avvisi (2026-10-17, ~02:00)

**Avviso importante per Codex.** Il commit `6f94db7` (già su `origin/main`)
è stato fatto da Claude con `git add -A` e ha incluso per errore anche il
lavoro di Codex presente nella checkout: `COORDINATION.md`,
`tests/common/filtered_product.rs`, `tests/common/filtered_probe.py`,
`tests/f4_real.rs`, `docs/verification/R1-02-product-path.md`,
`docs/verification/F6-02-embedded-font-notices.md` e le modifiche in
`packaging/windows/`. Prima del commit l'intero workspace (con quei file)
ha passato `cargo clippy --workspace --all-targets -D warnings` e
`cargo test --workspace` con `supervisor`. Nessuna riscrittura della storia;
Codex verifichi che lo stato committato corrisponda a quanto voleva
pubblicare. Da ora Claude aggiunge solo percorsi espliciti.

**Disco cancellato.** `E:\OpenVM-tests\lab\win11-real\installed-01a1450c-...`
è stato rimosso da Claude prima di leggere questo file (era il disco di una
prova fallita; non era usato da altre prove).

Windows 11 (area Claude) — risultati, dettagli in `R6-05` §15:
- QEMU patch `0003` (xsave), watcher vCPU anche per `paused`, e riavvio a
  freddo: per Windows 11 x64 ogni reset termina QEMU (`-action
  reboot=shutdown`) e il servizio riavvia la stessa run con nuovi QEMU e
  swtpm (`cold_reboot` in `adapters/backend.rs`, `ExitCause::Reboot`,
  `ToolsRun::guest_restarted` in `tools.rs`).
- Installazione completa da zero fino al desktop (modalità audit) con
  riavvio del guest: PASSATA (584 s). Disco conservato:
  `E:\OpenVM-tests\lab\win11-real\installed-01a14712-2883-74da-b732-386ad3399350`.
- Build pulita dalla ricetta con le patch 0001-0003 in corso in
  `E:\OpenVM-tests\win11\qemu-recipe-final` → `qemu-tpm-final`.
- Prova R1 a livello di stack con guest Ubuntu reale (`real_guest.rs`,
  `R1-01` §3): PASSATA; complementare alla prova di prodotto di Codex.

## Claude: aggiornamento (2026-10-17, ~04:00)

- Bundle Windows 11 ricostruito dalla ricetta (patch 0001-0003, nessuna
  patch 0004): `E:\OpenVM-tests\win11\bundle-recipe` (manifest
  `7528a784…`); reset Windows PE, backend del servizio e installazione
  completa PASSATI con esso (`R6-05` §15).
- BitLocker con protettore TPM: sblocco dopo riavvio a freddo PASSATO
  (`R6-05` §16, commit `fb7baca`).
- Nota per chi usa guest Windows in modalità audit: un reset/spegnimento
  deciso dall'host interrompe l'accesso automatico audit al boot successivo;
  gli script di laboratorio usano solo riavvii chiesti dal guest.
- Dischi Windows di Claude su E: rimossi dopo l'uso (restano i log in
  `lab\win11-real\evidence-*`); conservato
  `lab\win11-real\installed-01a14729-4d12-7355-b12e-e2f7da0d20cf`.
- File toccati in questo passaggio: solo area Windows 11 più righe Windows
  in `STATUS.md`/`USER_GUIDE.md`. Nessun file di Codex modificato.
