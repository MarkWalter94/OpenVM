# ADR-0005 — Baseline a VM spenta con overlay per sessione

- Stato: Accettata
- Data: 2026-10-09
- Responsabile: da assegnare

## Contesto

Gli snapshot a caldo coinvolgono RAM, stato dei dispositivi e consistenza applicativa; `qemu-img` non deve modificare immagini in uso (roadmap, sezioni 3 e 8). L'MVP ha bisogno di un ritorno ripetibile allo stato iniziale.

## Decisione

- Termine visibile: **baseline a VM spenta**; mai presentata come salvataggio della RAM.
- Baseline immutabile di disco e configurazione, incluso lo stato firmware scrivibile pertinente (UEFI NVRAM), registrata solo dopo arresto e rilascio dei file.
- Ogni test usa un overlay QCOW2 con backing file interno fissato e una copia dello stato firmware della baseline.
- Il ripristino crea un nuovo overlay e ripristina gli altri stati persistenti gestiti; il guest riparte con un boot normale; le richieste della vecchia esecuzione sono invalidate.
- Nuovi checkpoint materializzati in immagini autonome a VM spenta, senza catene crescenti; la baseline iniziale di una sessione non è sostituibile dall'agente.
- Esclusi nell'MVP: snapshot RAM, snapshot live, migrazione, catene arbitrarie di backing file, commit automatico dell'overlay nella base, import di QCOW2 esterni.
- Un arresto forzato non produce automaticamente una baseline verificata.

## Alternative considerate

- Snapshot interni QCOW2 con stato RAM (`savevm`): esclusi (sezione 8).
- Snapshot live con blockdev: esclusi dall'MVP (sezioni 4 e 8).
- Copia completa del disco a ogni test: non prevista; il modello è overlay su base immutabile.

## Conseguenze

- Lock, journal, controllo dei riferimenti e riconciliazione dopo crash (F3).
- Il budget storage deve contare overlay conservati, overhead QCOW2 e metadati (ADR-0017).
- `vm.snapshot` e `vm.restore` rifiutano la VM accesa e non eseguono shutdown nascosti (sezione 9).

## Prove richieste

- Ciclo base/overlay/ripristino con firmware persistente e prove di interruzione: `docs/verification/F0-05-baseline-overlay-firmware.md` §6, §8 (verificato, 5 cicli; il vars dell'esecuzione è modificato dal firmware e ricreato dalla baseline; base invariata dopo kill durante la scrittura del guest e durante `qemu-img convert`). Proposta di journal e riconciliazione in `F0-05` §9. Esclusione dei dischi in ADR-0025.
- Criterio di uscita F3: 30 cicli, hash della base invariato, prove di crash.

## Riferimenti

Roadmap, sezioni 3, 4, 8, 9, 10 (F0, F3).
