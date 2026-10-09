# Sicurezza

Ultimo aggiornamento: 2026-10-09 (riesame di F0)

Documento operativo previsto dalla sezione 17 di `docs/ROADMAP.md`: implementazione dei controlli e rischi residui aggiornati. **Non attenua il threat model per far passare i test.** I requisiti sono nella sezione 7 della roadmap; qui si registrano la loro traduzione operativa e lo stato. Nessun controllo è implementato nel prodotto: tutti sono "pianificato". Dove uno spike F0 ne ha dimostrato la fattibilità sul primo host (Windows 11), lo stato riporta "pianificato, fattibilità provata in F0-xx": è una prova di fattibilità, non un controllo implementato o verificato nel prodotto (riesame in `docs/verification/F0-09-riesame-prove.md`).

## 1. Threat model operativo

### 1.1 Asset da proteggere

| Asset | Esempi concreti in LightVM |
| --- | --- |
| File e credenziali host | File dell'utente, archivio credenziali OS, credenziali provider del client agente, chiavi di firma |
| Altre VM | Dischi, baseline, firmware e artefatti di VM diverse da quella autorizzata |
| Policy e grant | Policy globale e per VM, grant di sessione, credenziali di pairing |
| Endpoint di controllo | IPC del supervisor, canale QMP, canale display |
| Disponibilità delle risorse | CPU, memoria e storage dell'host; spazio di riserva per manifest ed esiti |
| Integrità dei report | Report JSON, audit, hash degli artefatti, screenshot |
| Baseline | Disco e stato firmware immutabili della baseline |

### 1.2 Attaccanti e input considerati ostili (sezione 7.1)

| Sorgente ostile | Superficie raggiunta | Controlli principali |
| --- | --- | --- |
| Contenuto del guest | Dispositivi emulati di QEMU, framebuffer, log, testo visualizzato | Profilo offline, dispositivi minimi, rendering come testo, client RFB a sottoinsieme ristretto |
| Output del test | Report, log, screenshot | Rendering come testo o Markdown limitato, limiti di dimensione, nessun link aperto automaticamente |
| Client MCP con permessi insufficienti | Adapter MCP, IPC | Grant limitato, verifica al ricevimento e prima dell'inoltro, rate limit |
| Parametri ricevuti | Validazione nel core, generazione comandi QEMU | ID invece di percorsi, grammatica delle opzioni QEMU controllata |
| Immagini importate | Registro media, QEMU | Copia gestita in sola lettura con hash; nessun import di QCOW2 esterni nell'MVP |
| Processo locale senza credenziale | IPC, QMP, canale display | Canali privati OS, ACL/permessi, credenziali per esecuzione |
| Testo nel guest che istruisce l'agente | Agente esterno | Nessun ragionamento dell'LLM modifica la policy; policy nel core |

### 1.3 Fuori perimetro dichiarato (sezione 7.1)

- Sandbox per malware deliberatamente ostile.
- Resistenza a un amministratore host compromesso.
- Separazione assoluta da processi malevoli dello stesso utente.
- Confinamento nel filesystem di un processo QEMU compromesso: l'assenza di cartelle condivise limita i percorsi previsti, non dimostra il confinamento.
- Revoca di shell, filesystem o strumenti che il client agente possiede già fuori da LightVM.
- Prova forense resistente a manomissioni: l'audit locale è modificabile dallo stesso utente (sezione 9).
- Protezione dal riempimento del volume causato da altri processi host (sezione 7.2).

### 1.4 Confinamento addizionale

Stato al riesame di F0: Windows provato con gli spike F0, Linux rinviato a R2a (sezione 7.2; ADR-0017, ADR-0016). Per ogni host si registra separatamente: configurazione guest restrittiva, controllo delle API, sandbox del processo host. Un controllo obbligatorio assente blocca AI Mode; un controllo addizionale assente è un limite dichiarato, non una garanzia equivalente.

| Livello | Windows | Linux |
| --- | --- | --- |
| Configurazione guest restrittiva | pianificato, fattibilità provata in F0-02 (`-nodefaults -nic none`, dispositivi HID virtio) | pianificato |
| Controllo delle API (supervisor, policy) | pianificato | pianificato |
| Sandbox del processo host QEMU | non disponibile in F0: QEMU gira con l'identità dell'utente, senza elevazione, ambiente ridotto e handle limitati (F0-06); token ristretto o AppContainer da provare in F1 (ADR-0025). Limite dichiarato | non provato (R2a) |

## 2. Controlli del profilo MVP AI (sezione 7.2)

| # | Controllo | Stato | Fase di implementazione | Prova prevista |
| --- | --- | --- | --- | --- |
| 1 | Accelerazione hardware e configurazione QEMU approvate; nessun fallback silenzioso a TCG | pianificato, fattibilità provata in F0-02 (probe WHPX reale, nessun TCG in tutte le prove F0) | F0 probe; F1 diagnostica e rifiuto | Criterio F1 "errore chiaro con acceleratore assente" (ADR-0007) |
| 2 | Nessuna NIC guest, periferiche di rete predefinite escluse, nessun forwarding | pianificato, fattibilità provata in F0-02 §5 (`-nodefaults -nic none`; verifica con `info network`, non con `query-rx-filter`) | F1 (profilo offline predefinito) | Criterio F1 "nessuna NIC inattesa"; test negativo Rete (ADR-0004) |
| 3 | Nessuna condivisione host, clipboard, USB passthrough, disco fisico o dispositivo non necessario; solo HID virtuali allowlist | pianificato, fattibilità provata in F0-02 e F0-04 (solo `virtio-tablet`/`virtio-keyboard`; client RFB senza clipboard). Rischio aperto: egui-winit legge gli appunti host su Ctrl+V (testo scartato dalla UI; backlog F1-13) | F1 | Confronto tra configurazione generata e dispositivi osservati via QMP; test negativo Console |
| 4 | Processi senza elevazione, ambiente ripulito, supervisione dell'intero albero processi | pianificato, fattibilità provata in F0-06 (Job Object dalla creazione, `KILL_ON_JOB_CLOSE`, ambiente con solo `SystemRoot`, handle limitati, morte del supervisor senza orfani); crash della UI non provato | F0 prova del modello; F1 implementazione | Criterio F1 "20 cicli senza orfani" (ADR-0002, ADR-0015) |
| 5 | Dischi, ISO e artefatti tramite registri interni; il client fornisce ID, non percorsi | pianificato; esclusione della baseline tramite share mode provata in F0-05 (ADR-0025) | F1 registro media; F4–F5 sessioni e MCP | Test negativi Host e File (ADR-0008) |
| 6 | QMP privato, console protetta, IPC autenticato, permessi OS restrittivi | pianificato, fattibilità provata in F0-02, F0-04 e F0-06 (socket Unix in directory privata con ACL solo-utente; rifiuto a integrità bassa); secondo utente e credenziale del display per esecuzione non provati; IPC da F1 | F0 prova; F1 QMP/IPC; F2 console | Test negativo IPC/console (ADR-0012, ADR-0013) |
| 7 | Sessioni con scadenza, VM allowlist, azioni consentite, quote, revoca, proprietà dell'input | pianificato | F4 | Test negativi Sessioni e VM; criterio di uscita F4 (ADR-0006) |
| 8 | Limiti CPU/memoria sull'albero processi; budget storage e riserva host con natura del controllo dichiarata | pianificato, fattibilità provata in F0-08 (hard cap CPU e limite di memoria del job con effetto reale su QEMU/WHPX; nessuna quota storage rigida senza amministratore: budget applicativo) | F0 identificazione; F3 quote catalogo; F4 applicazione alle sessioni | Test negativo Risorse; prove sotto pressione (ADR-0017) |
| 9 | Audit di azioni e dinieghi, minimizzazione di segreti e contenuti sensibili | pianificato | F1 minimo; F3 storage; F4 completo | Revisione dei log prodotti dai test negativi; assenza di segreti nei report |
| 10 | Stop e revoca indipendenti dal modello e dalla risposta del client | pianificato, fattibilità provata in F0-06 per l'Emergency Stop (`TerminateJobObject`, indipendente da QMP: p95 640,2 ms su 30 campioni a desktop, 273,3 ms su 100 campioni durante il boot); revoca da F4 | F0 prova terminazione; F4 implementazione | Test negativi Revoca e Lifecycle; misure revoca ed Emergency Stop della sezione 13 (ADR-0009) |

Stati usati in questa tabella: `pianificato`, `implementato`, `verificato in simulazione/test`, `verificato su hardware`, `assente (blocca AI Mode)`. Ogni passaggio di stato rimanda a un file in `docs/verification/`.

## 3. Test negativi obbligatori (sezione 7.4)

Checklist da completare con rimando al file di verifica sul prodotto. Nessuna voce è eseguita sul prodotto; le note indicano le prove di fattibilità F0 pertinenti. Superare questi test dimostra le proprietà testate, non l'assenza di ogni possibile VM escape.

- [ ] **Sessioni** — token assente, scaduto, revocato o riutilizzato dopo riavvio: diniego senza effetto sulla VM.
- [ ] **VM** — sessione su A chiede screenshot o input su B: diniego, nessun dato di B.
- [ ] **Host** — path traversal, UNC, device path, symlink/junction, opzioni QEMU nei parametri: rifiuto.
- [ ] **Rete** — guest verso host, LAN, Internet, IPv6 in profilo offline: nessun percorso.
- [ ] **IPC/console** — processo locale senza credenziale verso IPC o canale display: connessione rifiutata. *F0-04 §6: processo a integrità bassa dello stesso utente rifiutato sui socket QMP e VNC; secondo utente non provato (P-5).*
- [ ] **Policy** — testo nel guest chiede di disabilitare i controlli: autorizzazioni invariate.
- [ ] **Revoca** — flood di input durante Emergency Stop: nessun inoltro dopo la revoca, richieste già inviate identificate, fine dei processi verificata.
- [ ] **Storage** — ripristino concorrente al boot; eliminazione di base referenziata: operazione impedita. *F0-05 §7: con l'handle del supervisor in `FILE_SHARE_READ` scrittura, rinomina e cancellazione della base sono rifiutate.*
- [ ] **Crash** — processo ucciso durante creazione, rollback o aggiornamento DB: stato sicuro, baseline integra. *F0-05 §8: base invariata dopo kill di QEMU in scrittura e di `qemu-img convert`; il file parziale supera `qemu-img check`.*
- [ ] **Risorse** — VM sovradimensionata; flood di screenshot/log: rifiuto o contenimento.
- [ ] **Report** — log con HTML/script, sequenze terminale, link pericolosi: solo testo, nessun markup attivo, nessuna apertura automatica.
- [ ] **Lifecycle** — client perso, UI chiusa, supervisor terminato, host ripreso da sospensione: sessione revocata, nessun riavvio autonomo, processi gestiti secondo policy. *F0-06 §2a: supervisor terminato → QEMU terminato dal sistema operativo; UI chiusa e sospensione non provate.*
- [ ] **Console** — clipboard, power control o input sul canale RFB fuori dal bridge: messaggi non implementati, credenziale display non disponibile ad altri processi. *F0-04 §6: `rfb-client` invia solo i messaggi 0, 2, 3 (test `sola_lettura.rs`, verificato in simulazione/test); credenziale per esecuzione non ancora realizzata.*
- [ ] **File** — ISO sostituita dopo l'approvazione; opzioni QEMU nei metadati: identità verificata, nessuna interpretazione come configurazione host.

## 4. Rischi residui dichiarati

| Rischio residuo | Motivo | Mitigazione disponibile |
| --- | --- | --- |
| Vulnerabilità di QEMU, dei dispositivi emulati o dell'hypervisor (VM escape) | Codice di terzi esposto al guest | Dispositivi minimi, nessuna rete, aggiornamento QEMU, nessuna promessa di sandbox |
| Vulnerabilità nelle dipendenze Rust | Codice di terzi | Inventario, advisory in CI, aggiornamento o disabilitazione (ADR-0023) |
| Processo malevolo dello stesso utente | Stessa identità OS | Canali privati e credenziali per esecuzione riducono ma non annullano il rischio |
| Strumenti già posseduti dal client agente | Fuori dal controllo di LightVM | Documentare la configurazione del client coerente con la sessione |
| Effetti di input già consegnati a QEMU dopo la revoca | Non annullabili retroattivamente | Report che distingue coda scartata, richieste inoltrate ed effetti osservati |
| Superamento del budget storage prima dello stop (se non c'è quota rigida) | Controllo applicativo periodico | Soglie, riserva, frequenza massima di controllo dichiarate (ADR-0017) |
| Segreti visibili negli screenshot | Contenuto del guest | Controlli espliciti di cattura, conservazione e cancellazione; nessun riconoscimento automatico promesso |
| Audit modificabile localmente | Stesso utente | Hash degli artefatti rilevano alterazioni rispetto al manifest, non manomissioni coordinate |
| Overlay in uso scrivibile da un altro processo dello stesso utente | QEMU apre le immagini con `FILE_SHARE_WRITE`; la share mode non distingue i processi (`F0-05` §7.3) | Baseline protetta dall'handle del supervisor; overlay in directory per esecuzione con ACL ristretta; identità distinta per QEMU da provare in F1 (ADR-0025) |
| Lock delle immagini di QEMU inefficace su Windows | Comportamento della build 11.1.0 (`F0-02` §8.3) | Esclusione garantita dal supervisor, mai dal rifiuto di `qemu-img` (ADR-0025) |
| Credenziale del display non distinta da quella QMP a livello di ACL | UI e supervisor girano con lo stesso utente; i socket hanno la stessa ACL | Solo il supervisor conosce e occupa il socket QMP (client singolo); credenziale per esecuzione da realizzare entro F2 (ADR-0013) |
| Firma Authenticode dei binari QEMU scaduta | Certificato del firmatario della build scaduto (`F0-02` §2.2) | Hash dei binari approvati verificati prima dell'uso; checksum e manifest winget; verifica aggiuntiva prima di F6 (ADR-0010) |
| Driver di protocollo remoti inclusi nella build QEMU (`http`, `ssh`, `nbd`, …) | Build di terze parti | Protocolli in allowlist e rappresentazioni strutturate (sezione 6.3; backlog F1-08) |
| Lettura degli appunti host da parte di egui-winit su Ctrl+V | Comportamento della libreria (`F0-04` §4) | Testo scartato dalla UI; blocco del percorso prima di F2 (backlog F1-13) |
| Limite di memoria superato a regime: crash di QEMU invece di errore gestito | Comportamento osservato (`F0-08`) | Classificazione `resource_limit` dalla notifica del job; overlay non promosso (ADR-0017) |

## 5. Gestione delle vulnerabilità (processo minimo)

1. **Segnalazione.** Canale privato di segnalazione da definire alla creazione del repository pubblico; fino ad allora le segnalazioni vanno al responsabile sicurezza (da assegnare). Le segnalazioni non si discutono in issue pubbliche prima della correzione.
2. **Valutazione.** Classificazione per gravità e per capacità interessata (QMP, IPC, console, MCP, path, processi, storage).
3. **Dipendenze.** Advisory controllati a ogni modifica (ADR-0023); per QEMU e firmware, monitoraggio degli avvisi di sicurezza della versione registrata in `docs/COMPATIBILITY.md`.
4. **Risposta** (sezione 17): aggiornamento della dipendenza, nuova verifica, oppure disabilitazione della capacità interessata. Non si mantiene una versione vulnerabile per rispettare il testo della roadmap.
5. **Registrazione.** Ogni intervento aggiorna questo documento, la matrice e, se serve, un file di verifica.
6. **Rilascio.** Nessuna vulnerabilità critica o alta nota e non mitigata nel perimetro al rilascio (criterio di uscita F6).

## 6. Regole sui segreti

- Credenziali nell'archivio credenziali OS; mai negli argomenti di processo, nei report, nei log o negli esempi committati (sezione 9).
- Nessuna credenziale provider o del client MCP trasmessa a QEMU; ambiente dei processi figli ripulito (sezione 6.3).
- Runner con guest o immagini non fidati senza credenziali di produzione (sezione 13).
- Il file `.gitignore` esclude chiavi e file `.env`; non sostituisce la revisione prima del commit.
