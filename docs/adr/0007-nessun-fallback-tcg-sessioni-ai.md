# ADR-0007 — Nessun fallback TCG per le sessioni AI

- Stato: Accettata
- Data: 2026-10-09
- Responsabile: da assegnare

## Contesto

La documentazione QEMU distingue la virtualizzazione dall'emulazione TCG e non attribuisce a quest'ultima garanzie di isolamento (roadmap, sezione 3, vincolo 1). Un fallback silenzioso farebbe girare una sessione AI in un profilo non qualificato e falserebbe le misure.

## Decisione

- Le sessioni AI richiedono accelerazione hardware (WHPX su Windows, KVM su Linux) e una configurazione QEMU approvata.
- Nessun fallback silenzioso a TCG: se l'acceleratore manca o fallisce, l'avvio è rifiutato con errore esplicito.
- Il probe verifica l'acceleratore con la macchina e la configurazione effettivamente usate, non la sola presenza del binario.
- TCG non è usato per far risultare conclusa F0 (sezione 10, decisione di arresto).
- Un eventuale uso manuale sperimentale di TCG resta fuori dal profilo AI e dal piano MVP (sezione 5).

## Alternative considerate

- Fallback automatico a TCG con avviso: escluso (sezioni 3 e 7.2, punto 1).
- TCG ammesso per sessioni AI su host senza acceleratore: escluso.

## Conseguenze

- La diagnostica deve distinguere "acceleratore assente", "acceleratore presente ma rifiutato" e "configurazione non compatibile".
- Il backend deve passare l'acceleratore esplicitamente e verificare via QMP quello attivo.

## Prove richieste

- Probe WHPX reale: `docs/verification/F0-02-qemu-whpx-qmp.md` §4.1 (verificato, `query-accelerators` → `enabled: whpx`; nessuna prova F0 usa TCG, anche dove `-cpu host` è fallito).
- Criterio di uscita F1: errore chiaro con acceleratore assente.

## Riferimenti

Roadmap, sezioni 3, 5, 7.2 (punto 1), 10 (F0, F1).
