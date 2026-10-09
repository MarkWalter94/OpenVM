# LightVM

LightVM è un gestore locale di macchine virtuali per test ripetibili, basato su QEMU.
Offre una UI desktop nativa in Rust (egui/eframe) con console integrata e baseline ripristinabili a VM spenta.
Il controllo può essere delegato ad agenti esterni tramite MCP (stdio), con autorizzazioni applicate dal backend.
Ogni sessione agente è limitata a VM, strumenti, durata e quote approvate dall'utente, ed è revocabile in ogni momento.
Primo host di prodotto: Windows 11 x86_64 con guest Ubuntu LTS; nessun servizio cloud, telemetria o account obbligatorio.

## Stato

**F0 — Fattibilità e decisioni: in riesame.** Le prove di fattibilità sul primo host (Windows 11 con WHPX, guest Ubuntu 26.04.1 LTS) hanno dimostrato boot accelerato, console RFB nativa, input, canali di controllo su socket privati, ripristino da baseline e supervisione dei processi, con deviazioni pubblicate (per esempio modello CPU `qemu64` invece di `host`). Esito proposto: "verificato solo Windows", in attesa della conferma dell'utente e di alcuni pendenti; F0 non è ancora chiusa. Nessuna funzionalità di prodotto è implementata o verificata. Riesame in [`docs/verification/F0-09-riesame-prove.md`](docs/verification/F0-09-riesame-prove.md), stato in [`docs/STATUS.md`](docs/STATUS.md).

## Documenti

| Documento | Contenuto |
| --- | --- |
| [`docs/ROADMAP.md`](docs/ROADMAP.md) | Riferimento congelato: perimetro, controlli obbligatori, fasi e criteri di uscita. Non si modifica durante l'esecuzione; integrità in `docs/ROADMAP.sha256`. |
| [`docs/STATUS.md`](docs/STATUS.md) | Fase corrente, avanzamento, impedimenti |
| [`docs/RELEASE_PLAN.md`](docs/RELEASE_PLAN.md) | Backlog, ordine di esecuzione, decisioni aperte |
| [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) | Componenti, confini, modello di processo, console |
| [`docs/SECURITY.md`](docs/SECURITY.md) | Threat model, controlli, test negativi, rischi residui |
| [`docs/COMPATIBILITY.md`](docs/COMPATIBILITY.md) | Matrice delle combinazioni effettivamente provate |
| [`docs/TESTING.md`](docs/TESTING.md) | Pipeline di verifica e misure di prestazione |
| [`docs/adr/`](docs/adr/README.md) | Decisioni architetturali (accettate e proposte) |
| [`docs/verification/`](docs/verification/README.md) | Prove eseguite |

## Come leggere `docs/verification/`

Ogni file `F<fase>-<nn>-<argomento>.md` documenta una prova: scopo, ambiente, procedura con i comandi esatti, risultati e limiti. Ogni voce è marcata `verificato`, `fallito` o `non eseguito`. Solo un file di verifica può dimostrare che qualcosa funziona: un'affermazione in altri documenti senza rimando a una verifica non vale come prova.

## Licenza

Codice originale distribuito con licenza MIT: vedi [`LICENSE`](LICENSE). QEMU, firmware, immagini guest e dipendenze di terzi hanno licenze proprie, che la licenza di LightVM non sostituisce.
