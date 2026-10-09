# ADR-0003 — Console con client RFB interno sul canale protetto e ripiego screendump

- Stato: Accettata (scelta iniziale; la sezione 6.4 ne richiede la validazione in F0)
- Data: 2026-10-09
- Responsabile: da assegnare

## Contesto

La console deve mostrare il guest dentro l'app nativa senza WebView, senza gateway WebSocket e senza endpoint VNC privi di autenticazione del client. La roadmap fissa la direzione e il ripiego (sezione 6.4).

## Decisione

- Client RFB (VNC) scritto in Rust, eseguito nel processo della UI, che riceve da QEMU solo aggiornamenti del framebuffer e li disegna come texture egui.
- Sottoinsieme RFB: handshake, formato pixel, Raw, CopyRect e un encoding compresso; estensioni non previste rifiutate. Dipendenze VNC di terzi solo se manutenute e inventariate (ADR-0023), altrimenti client interno.
- Display in sola lettura per costruzione: il client non implementa invio di input, clipboard o power control. L'input dell'utente passa dal bridge autorizzato tramite comandi QMP, come quello degli agenti.
- Credenziale del display emessa dal supervisor per esecuzione e VM, distinta da QMP e IPC amministrativo, revocata con la sessione.
- Canale protetto scelto in ADR-0013 (socket privato OS oppure VeNCrypt X.509 con certificato client).
- **Ripiego:** se sul primo host nessuna protezione del canale è verificabile, framebuffer ricostruito dal backend con QMP `screendump` a frequenza limitata e consegnato alla UI via IPC, con gli stessi controlli di autorizzazione e revoca; latenza misurata e dichiarata in `docs/COMPATIBILITY.md`.
- Mai un endpoint VNC senza autenticazione del client. Se anche il ripiego fallisce, il target è bloccato.
- Tastiera e mouse letti da winit con scancode fisici e mappati sui qcode QEMU per layout dichiarati; tasti rilasciati su perdita di focus, disconnessione o cambio proprietario dell'input; nessuna lettura automatica della clipboard host.

## Alternative considerate

- Client VNC web in WebView: escluso (ADR-0001).
- VNC con password tradizionale o solo bind su localhost: esclusi, non sono autorizzazione (sezione 6.4).
- Display SPICE o altri protocolli: non previsti dalla roadmap.

## Conseguenze

- Il percorso console completo va prototipato prima di stabilizzare la UI (sezione 6.4) e prima di una costruzione estesa della dashboard (sezione 15).
- Il ripiego comporta latenza maggiore e carico QMP da contabilizzare.
- La proprietà "sola lettura" richiede un test negativo (sezione 7.4, riga "Console").

## Prove richieste

- Console, input e screenshot sul canale protetto: `docs/verification/F0-04-console-rfb-input.md` (parziale: console RFB, input QMP e screenshot verificati su socket Unix; tastiera fisica attraverso winit non eseguita). La scelta iniziale è validata sul primo host; il ripiego screendump non è necessario ma è misurato (p95 19,2 ms). Trasporto in ADR-0013, collocazione del client in ADR-0024.
- Test negativo di sola lettura del client RFB: `crates/rfb-client/tests/sola_lettura.rs` (verificato in simulazione/test, `F0-04` §6). Test negativo sul canale con un altro utente: pendente (P-5 in `docs/verification/F0-09-riesame-prove.md`); test completi entro F2/F4.

## Riferimenti

Roadmap, sezioni 6.4, 7.4, 10 (F0), 14, 15.
