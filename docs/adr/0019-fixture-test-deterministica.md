# ADR-0019 — Fixture di test deterministica

- Stato: Accettata (definizione); realizzazione entro F3 (roadmap, sezione 9)
- Data: 2026-10-09
- Responsabile: da assegnare

## Contesto

In F0 va definita una fixture grafica deterministica da eseguire realmente nel guest: applicazione, sequenza di azioni, risultato atteso verificabile; non un mock di QEMU. La fixture è realizzata e distribuita tramite il supporto dati approvato entro F3, prima dei test di regressione del bridge e del rollback (roadmap, sezione 9). Il criterio di uscita di F5 richiede dieci esecuzioni con ripristino senza errori infrastrutturali.

La definizione usa soltanto ciò che `F0-04` e `F0-05` hanno dimostrato funzionare nel guest di riferimento (ADR-0011) in sessione live, senza installazione: apertura del terminale con Ctrl+Alt+T, digitazione con layout US, clic assoluto nella finestra di benvenuto con cambio di lingua osservabile, `sudo` senza password del live e scrittura del guest sul disco `virtio-blk` collegato all'overlay.

## Decisione

### Ambiente e stato iniziale

- Guest: ISO di ADR-0011 (SHA-256 `601e30fb…1bda1f`) avviata in sessione live; profilo VM di ADR-0011 (`pc-q35-11.1`, `-cpu qemu64`, 2 vCPU, 4 GiB, `-nodefaults -nic none`, `virtio-vga`, `virtio-tablet-pci`, `virtio-keyboard-pci`).
- Baseline della fixture: disco dati `base.qcow2` **vuoto** (tutti zeri) collegato come `virtio-blk` (nel guest `/dev/vda`) e copia del vars OVMF, secondo ADR-0005. Ogni esecuzione usa un overlay nuovo e una copia nuova del vars.
- Supporto dati approvato (ADR-0008) con etichetta di volume `LIGHTVM_FX` contenente lo script `fx.sh` descritto sotto. Fino alla chiusura dei dettagli del supporto dati, le stesse istruzioni si digitano direttamente nel terminale (variante digitata, quella dimostrata in `F0-04`).
- Applicazioni usate: finestra "Welcome to Ubuntu" del live, GNOME Terminal, `bash`, `sudo`, `head`, `dd` del guest. Nessuna rete, nessun guest agent.

### Script della fixture (`fx.sh`)

Solo caratteri ASCII digitabili con il layout US dichiarato. Comportamento:

- `fx.sh mostra`: nasconde il cursore (`\033[?25l`), pulisce lo schermo e stampa con sequenze ANSI a colori 24 bit una fascia di quattro blocchi pieni, ciascuno largo 12 colonne e alto 3 righe, affiancati su una stessa riga del terminale:
  - B1 = RGB (1, 254, 3), B2 = RGB (251, 2, 249), B3 = RGB (4, 5, 250): colori fissi di riferimento;
  - B4 = colore calcolato da un comando con output noto: RGB (`$((6*7))`, `$((200+13))`, `$((3**4))`) = (42, 213, 81);
  - B5, sotto la fascia, largo 36 colonne e alto 3 righe: **stato del disco**. Se i primi 11 byte di `/dev/vda` letti con `sudo head -c 11 /dev/vda` sono `LIGHTVM-FX1`, B5 = RGB (250, 250, 6) ("marcatore presente"); altrimenti B5 = RGB (6, 250, 250) ("marcatore assente").
- `fx.sh scrivi`: `printf 'LIGHTVM-FX1' | sudo dd of=/dev/vda bs=512 count=1 conv=fsync status=none`, poi `fx.sh mostra`.

### Sequenza di azioni

Espressa con i tool del catalogo v0.2 (screenshot, mouse, tastiera). Coordinate per il display 1280×800 osservato in `F0-04`.

| Passo | Azione | Attesa |
| --- | --- | --- |
| A0 | Boot dalla baseline | Pronto quando due screenshot consecutivi a 5 s di distanza sono identici e hanno almeno il 99% di pixel non neri; timeout 300 s |
| A1 | Clic sinistro in (662, 326), voce "Deutsch" dell'elenco lingue | Screenshot dopo 2 s |
| A2 | Clic sinistro in (658, 422), voce "English" | Screenshot dopo 2 s |
| A3 | Tasti `ctrl-alt-t` | Terminale aperto: screenshot diverso da quello di A2 entro 15 s |
| A4 | Digitazione di `sh /media/ubuntu/LIGHTVM_FX/fx.sh mostra` (o del corpo equivalente nella variante digitata), Invio | Screenshot dopo che due catture consecutive a 1 s sono identiche; timeout 20 s |
| A5 | Digitazione di `sh /media/ubuntu/LIGHTVM_FX/fx.sh scrivi`, Invio | Come A4 |
| A6 | Arresto della VM con la scalata ordinaria (ADR-0009, `F0-06` §2d) | Fine del processo osservata |
| A7 | Nuovo avvio **senza** ripristino, poi A3 e A4 | Come sopra |
| A8 | Ripristino dalla baseline (nuovo overlay e nuovo vars), avvio, poi A3 e A4 | Come sopra |

### Risultato atteso e verifica

Il verificatore è un programma deterministico eseguito dal backend sugli screenshot PPM a 24 bit prodotti da `screendump` (nessun ridimensionamento, nessuna compressione con perdita). Non usa OCR, confronto approssimato di immagini o giudizio di un modello.

| Controllo | Dopo | Criterio |
| --- | --- | --- |
| V1 mouse | A1 | Il pixel di controllo della riga "Deutsch" ha esattamente il colore di selezione registrato nella calibrazione e quello della riga "English" non lo ha |
| V2 mouse | A2 | Condizione opposta a V1 |
| V3 tastiera e output noto | A4 | Per ciascuno di B1…B4 esiste un insieme connesso di pixel di quel colore esatto il cui riquadro di contorno è pieno (numero di pixel = area), con area di almeno 12×3 celle alla dimensione minima di cella dichiarata in calibrazione; i quattro riquadri sono alla stessa altezza e in ordine B1, B2, B3, B4 da sinistra a destra; B5 è "marcatore assente" |
| V4 scrittura | A5 | V3 con B5 "marcatore presente" |
| V5 persistenza | A7 | V3 con B5 "marcatore presente" |
| V6 ripristino | A8 | V3 con B5 "marcatore assente" |

Calibrazione, eseguita una volta per hash della ISO e risoluzione e registrata nel manifest della fixture: coordinate e colore dei pixel di controllo di V1/V2 ricavati dagli screenshot di riferimento; dimensione minima di cella del terminale; verifica che i sei colori della fixture (B1…B4 e i due valori di B5) non compaiano in nessun pixel degli screenshot di riferimento senza fixture.

Esiti, fissati prima dell'esecuzione:

| Esito | Condizione |
| --- | --- |
| `passed` | V1…V6 soddisfatti |
| `failed` | Infrastruttura funzionante (screenshot validi, QMP e processo regolari) e almeno un controllo V non soddisfatto |
| `timed_out` | Un'attesa della sequenza supera il proprio timeout |
| `infrastructure_error` | Errore di QMP, screenshot non valido, uscita inattesa di QEMU, errore di storage o di ripristino |
| `inconclusive` | Calibrazione assente o non valida per l'hash e la risoluzione correnti; colori della fixture presenti negli screenshot di riferimento |
| `cancelled` | Arresto richiesto dall'utente, revoca o Emergency Stop durante l'esecuzione |

L'effetto persistente che il ripristino deve annullare è il marcatore `LIGHTVM-FX1` nel primo settore di `/dev/vda`, cioè nell'overlay dell'esecuzione: la baseline resta tutta a zero (verificabile anche con l'hash della baseline, ADR-0005).

Verifica dei criteri:

1. Offline, senza rete e senza guest agent: soddisfatto (profilo `-nic none`, nessun agente).
2. Indipendente da animazioni, orari, notifiche: il verificatore cerca colori esatti non presenti nel desktop di riferimento; il cursore del terminale è nascosto; l'orologio e le notifiche non toccano i colori della fixture; le attese usano la stabilità tra catture, non tempi fissi.
3. Verifica automatica senza LLM: soddisfatto.
4. Effetto persistente sul disco annullato dal ripristino: soddisfatto (V5, V6).
5. Input limitato al layout US dichiarato e ad ASCII: soddisfatto.

## Alternative considerate

- Editor di testo preinstallato con verifica del testo visualizzato: richiederebbe OCR o confronto approssimato di regioni; escluso.
- Piccola applicazione dedicata distribuita sul supporto dati: comportamento controllato, ma richiede build e distribuzione di un eseguibile nel guest; non necessaria, perché uno script `sh` con sequenze ANSI dà un risultato pixel-deterministico.
- File nella home come effetto persistente: nel live la home è in RAM e non persiste; il primo settore del disco dati sì.
- Baseline con Ubuntu installato su disco: non ancora provata (ADR-0011, criterio 2); la fixture usa il live con disco dati vuoto, già dimostrato in `F0-05`.

## Conseguenze

- La fixture diventa parte dei test di regressione di F3–F6 ed è il carico del criterio di uscita F5.
- Un cambio di ISO, di risoluzione o di modello CPU richiede una nuova calibrazione e una nuova validazione.
- Il passo mouse dipende dalla finestra di benvenuto del live: con una baseline installata va sostituito con un'azione mouse equivalente e l'ADR va aggiornato.
- Il percorso `/media/ubuntu/LIGHTVM_FX` dipende dal montaggio automatico del secondo CD-ROM nel live, non ancora provato; è legato ai dettagli del supporto dati ancora aperti in ADR-0008.

## Limiti

- Elementi non ancora provati nel guest: stampa dei blocchi a colori 24 bit in GNOME Terminal, montaggio automatico del supporto dati, lettura di `/dev/vda` con `head`. Sono provati: Ctrl+Alt+T, digitazione US, clic in (662, 326) e (658, 422) con cambio di lingua, `sudo dd` su `/dev/vda` (`F0-04` §4; `F0-05` §8.1).
- Le coordinate valgono per 1280×800 e per l'ISO di ADR-0011; il colore di selezione e i pixel di controllo sono fissati alla calibrazione.
- In `F0-05` §8.1 un input inviato prima del desktop è andato perso: il criterio di prontezza di A0 serve a evitarlo e va validato alla realizzazione.

## Prove richieste

- Definizione: basata su `docs/verification/F0-04-console-rfb-input.md` Risultati §4 e `docs/verification/F0-05-baseline-overlay-firmware.md` §8.1.
- Realizzazione e dieci esecuzioni reali con ripristino: file di verifica dedicato entro F3 (nome da assegnare nella fase in cui viene eseguito).

## Riferimenti

Roadmap, sezioni 4, 6.4, 8, 9, 10 (F0, F3, F5), 17. Riesame: `docs/verification/F0-09-riesame-prove.md`.
