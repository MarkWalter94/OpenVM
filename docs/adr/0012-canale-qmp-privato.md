# ADR-0012 — Canale QMP privato

- Stato: Accettata (Windows); parte Linux rinviata a R2a con ADR-0016
- Data: 2026-10-09
- Responsabile: da assegnare

## Contesto

QMP è un'interfaccia privilegiata. Deve essere interno al supervisor, senza endpoint TCP non autenticato, tramite pipe ereditate o canali privati OS verificati sul target (roadmap, sezioni 3 e 6.3; sezione 7.2, punto 6).

## Decisione

Su Windows con la build di ADR-0010:

1. **QMP su socket Unix (AF_UNIX)** con `-qmp unix:<dir_privata>/qmp.sock,server=on,wait=off`.
2. La directory privata è creata dal supervisor **per esecuzione**, con ereditarietà rimossa e accesso concesso solo all'utente corrente (equivalente di `icacls <dir> /inheritance:r /grant:r "<utente>:(OI)(CI)F"`); il socket eredita quell'ACL. Nome e percorso sono decisi dal supervisor e mai ricevuti da UI o client MCP.
3. **Un solo client QMP, posseduto dal supervisor**: il supervisor si collega subito dopo lo spawn e mantiene la connessione per tutta l'esecuzione; input, screenshot, eventi e stato passano da quella connessione (QEMU serve un client alla volta). Nessun altro componente riceve il percorso del socket.
4. Il file socket è rimosso dal supervisor dopo l'uscita di QEMU (resta orfano dopo arresto forzato) e la directory è ricreata a ogni esecuzione.
5. L'Emergency Stop non dipende da QMP (ADR-0009).
6. **Named pipe creata da QEMU (`-qmp pipe:`) scartata.**

Verifica dei criteri:

1. Nessun processo diverso dal supervisor si connette: **soddisfatto in parte**. Un processo dello stesso utente a integrità bassa è rifiutato (`os error 10013`); la connessione singola del supervisor impedisce a un secondo client di ricevere il greeting. Il test con un **secondo utente locale non è stato eseguito** (azione utente U-3 in `F0-09`). Un processo dello stesso utente a integrità media può raggiungere il socket se il supervisor non è collegato: rischio coerente con la sezione 7.1 (separazione assoluta dallo stesso utente fuori perimetro) e mitigato dalla connessione immediata del supervisor.
2. Supporto verificato nella build: soddisfatto (circa 200 avvii in F0-06; funziona anche in `prelaunch` con `-S`; riconnessione possibile; OOB annunciato).
3. Nessun nome o percorso controllabile da UI o MCP: soddisfatto per costruzione (percorso generato dal supervisor).
4. Nessuna eredità verso altri figli né variabili d'ambiente: soddisfatto per costruzione (`PROC_THREAD_ATTRIBUTE_HANDLE_LIST` con i soli handle standard, ambiente ridotto a `SystemRoot`; F0-06).
5. Disconnessione e timeout senza bloccare l'Emergency Stop: soddisfatto (Emergency Stop con `TerminateJobObject`, indipendente da QMP).

Se il test con il secondo utente fallisce, questo ADR torna a Proposta e si prova l'alternativa con socket creato dal supervisor e passato a QEMU per descrittore.

## Alternative considerate

- Named pipe creata da QEMU: scartata. Avvio bloccato finché un client non si collega, greeting senza OOB, stallo dei comandi in `prelaunch` con `-S`, un solo client senza riconnessione, il primo processo che apre la pipe la occupa, ACL non impostabile da opzione.
- Named pipe o socket creati dal supervisor e passati per handle/descrittore: non provati; restano l'alternativa se il criterio 1 fallisce.
- TCP su loopback senza autenticazione: escluso.
- Linux: socket Unix in directory privata o descrittore ereditato, da provare su host fisico (R2a).

## Conseguenze

- `qemu-backend` implementa un client QMP asincrono unico per esecuzione, con multiplexing di comandi ed eventi, timeout e riconnessione (backlog F1).
- Il client QMP su Windows usa AF_UNIX tramite `socket2` (verificato in F0-04; `uds_windows` provato come alternativa).
- La UI non riceve mai il percorso del socket QMP.

## Prove richieste

- QMP su socket Unix, named pipe, permessi osservati: `docs/verification/F0-02-qemu-whpx-qmp.md` §4.2–§4.3 (verificato; named pipe con limiti gravi).
- Rifiuto a integrità bassa e secondo client: `docs/verification/F0-04-console-rfb-input.md` Risultati §6 (verificato).
- Uso ripetuto con il supervisor e rimozione del socket orfano: `docs/verification/F0-06-supervisore-processi.md` (verificato).
- Test con secondo utente locale: non eseguito (pendente P-5 in `F0-09`).
- Linux: rinvio a R2a.

## Riferimenti

Roadmap, sezioni 3, 6.3, 7.1, 7.2, 7.4, 10 (F0), 17. Riesame: `docs/verification/F0-09-riesame-prove.md`.
