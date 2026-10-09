# ADR-0013 — Trasporto console protetto e ripiego

- Stato: Accettata (Windows); parte Linux rinviata a R2a con ADR-0016
- Data: 2026-10-09
- Responsabile: da assegnare

## Contesto

ADR-0003 fissa il client RFB interno e il ripiego screendump. Resta da scegliere il canale UI–QEMU per il display. La compatibilità di questo percorso su Windows è un requisito di F0 (roadmap, sezione 6.4). Il solo bind su localhost e la password VNC tradizionale non sono sufficienti.

## Decisione

Su Windows con la build di ADR-0010, nell'ordine di preferenza della roadmap:

1. **Primario: VNC su socket Unix** (`-vnc unix:<dir_privata>/vnc.sock`) nella directory privata per esecuzione creata dal supervisor (stessa regola di ACL di ADR-0012). Sul socket privato il client accetta il tipo di sicurezza `None` soltanto perché il canale è un socket Unix; su TCP lo rifiuta.
2. **Alternativa documentata: loopback con VeNCrypt X.509** (`tls-creds-x509` con `verify-peer=on`, sottotipo X509None) e certificato client gestito con rustls. Non è predefinita per due limiti osservati: l'arresto di una sessione TLS non interrompe la lettura bloccante (il thread viene abbandonato dopo 2 s) e QEMU lascia le connessioni in `CloseWait` per decine di secondi, incluse quelle con certificato rifiutato. Prima di usarla serve una prova con `connections=` e timeout del supervisor.
3. **Ripiego `screendump`**: confermato fattibile dalle misure (cattura end-to-end p95 19,2 ms a 1280×800, 120 campioni, 0 errori). Non è necessario sul primo host; resta la modalità degradata se il canale RFB non è disponibile.

Il client usa `socket2` per AF_UNIX su Windows; arresto della sessione su socket Unix con un solo handle condiviso, letture bloccanti e richiesta di aggiornamento 1×1 per provocare una risposta del server (stop misurato 93–94 ms).

**Credenziale del display per esecuzione (sezione 6.4): obbligo non ancora realizzato.** In F0 la protezione provata è l'ACL della directory privata e il rifiuto a un processo a integrità bassa. Con un socket Unix e UI e supervisor sotto lo stesso utente, l'ACL non distingue la credenziale del display da quella QMP: la distinzione oggi deriva dal fatto che solo il supervisor conosce e occupa il socket QMP (ADR-0012). Entro F2 il supervisor deve emettere per ogni esecuzione e VM una credenziale del display distinta da QMP e IPC, consegnata alla UI e revocata con la sessione, con prova. Candidati da provare: directory del display separata da quella QMP con etichetta di integrità che consenta l'accesso a una UI eseguita a integrità ridotta; oppure VeNCrypt con certificato client emesso per esecuzione, dopo la soluzione del problema di arresto. Se nessun candidato supera la prova, questo ADR torna a Proposta.

Verifica dei criteri:

- Processo locale senza credenziale non ottiene il framebuffer: soddisfatto per un processo a integrità bassa (socket Unix) e per un client con certificato di CA estranea (VeNCrypt); secondo utente non provato (`F0-09`, P-5).
- Credenziale per esecuzione, distinta, revocabile: **non soddisfatto in F0** (obbligo sopra).
- Funzionamento con la build di ADR-0010 sull'host: soddisfatto per entrambi i trasporti.
- Ripiego: latenza e carico della cattura misurati con il metodo della sezione 13; consegna via IPC non ancora esistente.

## Alternative considerate

- Named pipe con ACL per il display: la build non offre VNC su named pipe; non provata.
- VNC su TCP senza TLS: QEMU offre il tipo `None`, nessuna autenticazione; escluso e rifiutato dal client.
- VNC con password, gateway WebSocket: esclusi (sezione 6.4).

## Conseguenze

- `qemu-backend` avvia QEMU con `-vnc unix:` nella directory privata e consegna alla UI soltanto il percorso del socket display (mai quello QMP).
- La riga Windows di `docs/COMPATIBILITY.md` registra la modalità "RFB su socket Unix privato".
- Con VeNCrypt: CA per esecuzione, emissione e revoca dei certificati dal supervisor, chiavi private solo nella directory privata.
- La scelta può differire su Linux; la riga Ubuntu resta "non verificato".

## Prove richieste

- Console, trasporti, test negativi e misure: `docs/verification/F0-04-console-rfb-input.md` Risultati §1, §5, §6, §7 (verificato con i limiti indicati).
- Endpoint VNC su socket Unix e VeNCrypt con QEMU reale: `docs/verification/F0-02-qemu-whpx-qmp.md` §6 (verificato).
- Credenziale del display per esecuzione e revoca: da eseguire entro F2.
- Secondo utente: pendente P-5 in `docs/verification/F0-09-riesame-prove.md`.

## Riferimenti

Roadmap, sezioni 6.4, 7.1, 7.2 (punto 6), 7.4, 10 (F0, F2), 13, 14, 17.
