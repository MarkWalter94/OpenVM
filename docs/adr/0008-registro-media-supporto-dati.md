# ADR-0008 — Registro media approvato e supporto dati in sola lettura

- Stato: Accettata; dettagli operativi del supporto dati registrati il 2026-10-10 (sezione "Dettagli del supporto dati")
- Data: 2026-10-09
- Responsabile: da assegnare

## Contesto

Un percorso selezionato dall'utente può essere sostituito dopo l'approvazione (symlink, junction, scambio di file). Il guest ha bisogno di un canale per ricevere il software da testare senza cartelle condivise (roadmap, sezioni 4 e 6.3).

## Decisione

- Il registro media approvato conserva una copia gestita in sola lettura di ogni ISO, con identità e hash; QEMU riceve solo media del registro.
- Creazione e apertura protette contro symlink, junction e sostituzioni tra controllo e uso, con primitive OS e directory controllate.
- Oltre al supporto di installazione, una VM può montare **a VM spenta** un secondo supporto ISO dati in sola lettura, proveniente dal registro. È l'unico ingresso di file nel guest nell'MVP: non scrivibile dal guest, non gestibile dall'agente.
- Nessun tool MCP monta, sostituisce o rimuove supporti (sezione 9).
- Nessun montaggio automatico di filesystem guest sull'host; nessun catalogo automatico di immagini; ISO introdotte dall'utente tramite selettore nativo.
- Il client MCP fornisce ID, non percorsi (sezione 7.2, punto 5).

## Dettagli da chiudere in F0

La sezione 17 elenca il "supporto dati approvato" tra le decisioni di F0. Restano da fissare: strumento e formato con cui l'utente produce l'ISO dati, limiti di dimensione, tipo di dispositivo emulato (CD-ROM in sola lettura) e verifica che il guest non possa scriverlo. Questi dettagli vanno registrati in questo ADR (sezione aggiuntiva) o in un ADR successivo collegato, senza cambiare la decisione sopra.

## Dettagli del supporto dati (2026-10-10, chiusura di P-7)

Dati di implementazione (sezione 17), senza cambiare la decisione sopra:

| Punto | Scelta | Dove |
| --- | --- | --- |
| Formato | Immagine ottica **ISO 9660** (anche ibrida con UDF o Joliet/Rock Ridge) oppure **UDF**: firma `CD001` o `BEA01` nel primo descrittore di volume (offset 32769). Altri file sono rifiutati all'ispezione (`invalid_argument`) | `lightvm-service/src/adapters/catalog.rs` (`is_optical_image`, `ManagedMediaInspector`) |
| Strumento | Nell'MVP l'utente produce l'ISO dati con uno strumento standard fuori da LightVM (per esempio `oscdimg` del Windows ADK o qualunque generatore ISO 9660/UDF); LightVM ne fa una copia gestita, ne verifica firma, dimensione e hash. Generare l'ISO da una cartella dentro LightVM è un'estensione possibile (da valutare con ADR-0023 per le licenze), non un requisito | — |
| Dimensione | ISO di installazione ≤ 16 GiB, ISO dati ≤ 8 GiB (DVD a doppio strato); oltre → `resource_limit` alla registrazione. Le copie gestite contano nel budget di storage (ADR-0017) | `lightvm-proto` (`MAX_INSTALL_MEDIA_BYTES`, `MAX_DATA_MEDIA_BYTES`), `vm-core` (`register_media`) |
| Dispositivo emulato | CD-ROM IDE (`ide-cd`) sul controller AHCI di q35, mai avviabile per il supporto dati (nessun `bootindex`); nodo `raw` con `read-only=on` sia sul formato sia sul file | `qemu-backend/src/cmdline.rs` (`push_cdrom`) |
| Guest non può scrivere | Tre livelli: un CD-ROM non accetta scritture dal guest; il blocco QEMU è aperto in sola lettura; la copia gestita è in sola lettura e il suo hash è riverificato dal core prima di ogni avvio (`verify_media`): una modifica impedisce l'avvio | `vm-core/src/media.rs` |
| Montaggio | Solo a VM spenta (configurazione della VM, `data_media`); nessun tool MCP lo cambia | `vm-core` (`update_vm_config` richiede VM spenta) |

Prove: `vm-core` `scenarios::register_media_rejects_oversized_images`; `lightvm-service` `adapters::catalog::tests::firma_iso_9660_o_udf`; la ISO Ubuntu di riferimento ha la firma `CD001`. Scrittura dal guest con QEMU reale: **verificata impossibile** il 2026-10-10 (`docs/verification/F1-11-secondo-utente-e-supporto-dati.md`: `touch`, remount e `dd` falliti con "Read-only file system", hash dell'ISO invariato).

## Alternative considerate

- Cartella condivisa (virtio-fs, 9p, SMB): esclusa (sezione 4).
- Disco dati scrivibile: escluso; il supporto è in sola lettura.
- Gestione media da parte dell'agente: esclusa (sezione 9).

## Conseguenze

- La fixture di test (ADR-0019) viene distribuita tramite il supporto dati entro F3.
- Il registro media fa parte del budget storage (ADR-0017).

## Prove richieste

- Test negativo "File" della sezione 7.4 (ISO sostituita dopo l'approvazione) e "Host" (path traversal, UNC, device path, symlink/junction): da F1.
- Identità e checksum della ISO guest: `docs/verification/F0-03-guest-media.md` (verificato).
- Dettagli del supporto dati approvato: registrati il 2026-10-10 (sezione sopra, P-7 chiuso); prova di scrittura dal guest con QEMU reale verificata in `F1-11`.

## Riferimenti

Roadmap, sezioni 4, 6.3, 7.2, 7.4, 9, 17.
