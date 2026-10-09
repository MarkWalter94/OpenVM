# ADR-0023 — Dipendenze e licenze

- Stato: Proposta
- Data: 2026-10-09
- Responsabile: da assegnare
- Da chiudere entro: F1 (avviata in F0, attività non bloccante); verifica finale in F6

## Contesto

Il codice originale di LightVM è distribuito con licenza MIT; le licenze dei componenti terzi vanno inventariate e la compatibilità della distribuzione verificata in F0/F6 (roadmap, sezione 6.0). Versioni fissate e lockfile (sezione 6.3). F6 richiede SBOM, inventario licenze e gestione vulnerabilità. Una versione vulnerabile non si congela per rispettare il testo (sezione 17).

## Decisione

Da prendere. Va fissato:

- politica delle licenze ammesse per le dipendenze collegate nel binario;
- strumenti di controllo di licenze, advisory e duplicati, eseguiti in CI;
- formato e strumento dell'SBOM;
- trattamento dei componenti esterni non redistribuiti (QEMU, firmware, guest): documentati nella guida, non inclusi nell'installer MVP;
- criterio di accettazione di nuove dipendenze (manutenzione, superficie, necessità), in particolare per librerie VNC/RFB (ADR-0003).

Criteri:

1. Ogni dipendenza ha licenza nota e compatibile con la distribuzione MIT del binario.
2. Advisory noti controllati a ogni modifica.
3. Inventario riproducibile dal lockfile.

## Alternative considerate

Candidati da verificare: `cargo-deny` per licenze e advisory, `cargo-audit` per advisory, `cargo-about` o `cargo-cyclonedx` per inventario e SBOM. La scelta degli strumenti non sostituisce la revisione umana delle licenze non standard.

## Conseguenze

- Una dipendenza con licenza incompatibile o non manutenuta viene sostituita o il codice reso interno.
- La gestione vulnerabilità in `docs/SECURITY.md` si appoggia a questi controlli.

## Prove richieste

- Primo inventario sul workspace creato in F0: file di verifica dedicato (nome da assegnare).

## Riferimenti

Roadmap, sezioni 6.0, 6.3, 6.4, 10 (F0, F1, F6), 14, 17.
