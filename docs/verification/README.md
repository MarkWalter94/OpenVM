# Verifiche registrate

Questa directory contiene le prove eseguite su LightVM: procedure, risultati, benchmark e manifest degli artefatti. È l'unico luogo in cui uno stato "verificato" può essere dimostrato. Riferimenti: `docs/ROADMAP.md`, sezioni 13 e 17.

## Regole

1. **Ogni verifica registra** comando o procedura, ambiente, risultato e limiti (sezione 13). Una prova non riproducibile dal testo del file non vale come verifica.
2. **Tre esiti distinti, mai fusi** (sezione 17): `verificato`, `fallito`, `non eseguito`. Una voce non eseguita non diventa verificata per analogia, per deduzione o perché "compila".
3. **Ambito esplicito.** Il file dichiara host, build, versioni e configurazione su cui vale l'esito. Un risultato su un host non si estende ad altri host, acceleratori, macchine o guest (sezione 5).
4. **Mock e boot reale separati.** Un test con supervisor o QEMU simulato è registrato come tale e non sostituisce la prova su hardware (sezioni 10 F1, 13, 16 punto 5).
5. **Fallimenti inclusi.** Campioni di misura, tentativi con client LLM e prove ripetute riportano anche errori, timeout e tentativi falliti (sezioni 10 F5 e 13).
6. **Niente segreti.** Nessuna credenziale, token, chiave privata o percorso personale sensibile nei log riportati; i log grezzi restano in `lab/`, che è fuori dal repository.
7. **Artefatti identificati.** ISO, binari QEMU, firmware e build citati sono identificati con nome, versione, provenienza e hash (SHA-256).
8. **Un file non si riscrive per cambiare esito.** Una nuova prova produce un nuovo file oppure una sezione datata aggiunta in coda; l'esito precedente rimane leggibile.
9. **Le decisioni restano negli ADR.** Il file di verifica può proporre una decisione; la decisione si registra in `docs/adr/`, lo stato in `docs/STATUS.md`, la matrice in `docs/COMPATIBILITY.md`.

## Convenzione dei nomi

```text
F<fase>-<nn>-<argomento>.md
```

- `<fase>`: numero della fase della sezione 10 (`0`–`6`); per i traguardi successivi `R1`, `R2a`, ecc.
- `<nn>`: progressivo a due cifre all'interno della fase.
- `<argomento>`: parole minuscole separate da trattino, senza spazi né accenti.

Esempi: `F0-01-toolchain-eframe.md`, `F0-02-qemu-whpx-qmp.md`, `F1-04-cicli-start-stop.md`.

## Indice delle verifiche

| File | Passo / fase | Esito dichiarato | Stato del file |
| --- | --- | --- | --- |
| `F0-01-toolchain-eframe.md` | F0, passo 1 (sezione 15) | per voce: build Windows verificata con ambiente MSVC esplicito; glow senza accelerazione fallito; RDP e Linux non eseguiti in questo file | completo |
| `F0-02-qemu-whpx-qmp.md` | F0, passo 2 (e base/overlay del passo 4) | per voce: WHPX, QMP su socket Unix, NIC, arresto forzato verificati; lock immagini e `-cpu host` falliti | completo |
| `F0-03-guest-media.md` | F0, passi 1–2 (release guest, checksum) | verificato | completo |
| `F0-04-console-rfb-input.md` | F0, passo 3 | parziale | completo |
| `F0-05-baseline-overlay-firmware.md` | F0, passo 4 (baseline, overlay, firmware, esclusione dischi) | parziale | completo |
| `F0-06-supervisore-processi.md` | F0, passo 4 (supervisione processi) | parziale (crash della UI non eseguito) | completo |
| `F0-07-linux-kvm.md` | F0, passo 5 (Linux/KVM) | parziale; smoke su host fisico non eseguito | completo |
| `F0-08-limiti-risorse-misure.md` | F0, passo 5 (limiti risorse e consumi) | parziale | completo |
| `F0-09-riesame-prove.md` | F0, passo 6 (riesame, decisione di prosecuzione) | parziale; F0 non ancora chiusa | completo |
| `F1-01-proto-core.md` | F1 (backlog F1-01, F1-04 come modello, F1-10) | parziale: tutte le voci verificate solo con test sintetici | completo |
| `F1-02-storage.md` | F1 (backlog F1-01 migrazioni, F1-04 registro media, F1-09 parte storage) | parziale: verificato con test sintetici su Windows; QEMU reale e Linux non eseguiti | completo |
| `F1-04-service-ipc-ci.md` | F1 (backlog F1-10, F1-11; integrazione del composition root) | parziale: IPC, istanza singola, revoca, morte del genitore verificati (Windows, WSL2); end-to-end con QEMU reale, 2 esecuzioni; secondo utente, perdita del genitore con QEMU reale e CI su GitHub non eseguiti | completo (sezione datata in coda) |
| `F1-06-mcp-sdk.md` | F1 (backlog F1-19, SDK MCP) | parziale: SDK `rmcp` provato su stdio; nessun client MCP reale | completo |
| `F1-07-licenze-packaging.md` | F1 (backlog F1-19 licenze, F1-14 packaging) | verificato per inventario e `cargo deny`; SBOM e librerie Linux a runtime non eseguiti | completo |
| `F1-08-ci.md` | F1 (backlog F1-02, F1-15) | parziale: workflow validato in locale, non eseguito su GitHub; delega cgroup provata in WSL2 | completo |
| `F1-03-qemu-backend-reale.md` | F1 (backlog F1-03, F1-05…F1-08, F1-12) | parziale: probe, rifiuti, NIC, 20 cicli start/stop senza orfani e disco del servizio verificati su hardware (Windows, WHPX); VM nel firmware; non vale come criterio di uscita finché F0 è aperta | completo |
| `F1-09-ui-supervisore-crash-ui.md` | F1 (backlog F1-11); pendente F0 P-2 | parziale: UI → supervisor figlio → IPC verificato; crash della UI senza orfani verificato, arresto ordinato della VM fallito (timeout del supervisor); Linux non eseguito | completo |

File non ancora scritto: `F1-05` (`apps/desktop`: appunti, backend di rendering, istantanee, misure; flusso interrotto nel commit `aa04ccd`). I test automatici relativi passano (`cargo test --workspace`), ma prova manuale degli appunti, prova in RDP e misure F1-18 restano `non eseguito`.

## Template

Copiare il blocco seguente in un nuovo file e compilarlo. Le sezioni sono obbligatorie; se una sezione non si applica, scriverlo esplicitamente.

```markdown
# F<fase>-<nn> — <titolo>

- Data di esecuzione (UTC): AAAA-MM-GG
- Esecutore: <nome o ruolo>
- Passo della roadmap: sezione <n>, <fase/passo>
- ADR collegati: ADR-NNNN, ...
- Esito complessivo: verificato | fallito | parziale | non eseguito

## Scopo

Che cosa si vuole dimostrare e quale criterio della roadmap (sezione e voce) la prova alimenta.
Che cosa questa prova NON dimostra.

## Ambiente

| Voce | Valore |
| --- | --- |
| Host (modello, CPU, RAM, storage) | |
| OS e build | |
| Acceleratore e stato | |
| Toolchain / versioni rilevanti | |
| Binario QEMU (versione, provenienza, SHA-256) | |
| Firmware (file, versione, SHA-256) | |
| Guest / media (nome, versione, SHA-256) | |
| Sessione grafica (locale, RDP, headless) | |
| Altre condizioni (antivirus, criteri di gruppo, ecc.) | |

## Procedura

Passi numerati, con i comandi esatti (blocchi di codice) e i file di configurazione usati.
Nessun segreto nei comandi riportati.

## Risultati

| Voce | Esito | Evidenza |
| --- | --- | --- |
| <controllo 1> | verificato / fallito / non eseguito | estratto di log, hash, screenshot, misura |

Per le misure: numero di campioni, p50, p95, massimo, timeout ed errori (sezione 13).

## Limiti

Condizioni non coperte, differenze dall'ambiente di riferimento, anomalie osservate non spiegate.

## Azioni richieste

Attività che la prova rende necessarie, con rimando al backlog in `docs/RELEASE_PLAN.md`.

## Decisioni proposte

Proposte per gli ADR collegati (non sono decisioni finché l'ADR non è aggiornato).
```
