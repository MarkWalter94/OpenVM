# LightVM — Analisi del prompt e roadmap esecutiva

Baseline di pianificazione: 9 ottobre 2026. Ambito di questo documento: pianificazione, non implementazione. Il documento fissa perimetro, dipendenze, controlli e criteri di uscita; non contiene stime di tempo, ore o risorse di sviluppo. Regole di congelamento nella sezione 17.

## 1. Decisione di prodotto

LightVM deve nascere come **gestore locale di VM per test ripetibili, con controllo delegabile ad agenti esterni e autorizzazioni applicate dal backend**.

La prima release deve dimostrare un percorso completo: installazione, VM Linux reale, console, baseline ripristinabile, sessione MCP autorizzata, test nel guest, evidenze, revoca e ripristino. Il primo agente può essere un client esterno: non occorre costruire contemporaneamente un motore LLM interno.

La roadmap fissa perimetro, dipendenze, controlli obbligatori e criteri di accettazione. Versioni selezionate, prove e stato vengono registrati nei documenti operativi senza modificare questa baseline. Le funzionalità con esito tecnico incerto hanno una decisione di prosecuzione esplicita, non una promessa implicita.

**Raccomandazione:** Windows 11 x86_64 come primo host di prodotto, coerente con l'ambiente attuale; Ubuntu LTS x86_64 come host di riferimento tecnico e secondo host verificato durante lo sviluppo. Prima release pubblica certificata su Windows solo se supera tutti i controlli obbligatori. Nessun ampliamento del perimetro prima del completamento dello scenario MVP.

## 2. Situazione effettivamente osservata

Alla ricognizione iniziale del 9 ottobre 2026, prima della creazione di questa documentazione:

- `E:\OpenVM` non conteneva file di progetto o repository visibili.
- Sono disponibili nel PATH Git, Rust, Cargo, Node e npm.
- I comandi versione restituiscono Rust 1.99.0, Cargo 1.99.0, Node 24.15.0 e Git 2.45.1.
- Rust/Cargo emettono anche avvisi di canonicalizzazione del percorso: da verificare nella prima build.
- `qemu-system-x86_64` e `qemu-img` non sono rilevabili nel PATH. Questo non dimostra che siano assenti da ogni directory del computer.
- Non sono stati verificati WHPX, firmware di virtualizzazione, toolchain C/C++, driver e backend grafici richiesti da eframe, compilazione della UI nativa o boot di una VM.

Il piano parte quindi da zero sulla base di quella ricognizione. Questi controlli non equivalgono alla verifica completa dell'ambiente di sviluppo e non descrivono gli eventuali file aggiunti successivamente.

## 3. Cosa correggere nel prompt

| Punto | Problema concreto | Correzione vincolante |
| --- | --- | --- |
| MVP | Comprende virtualizzazione, sicurezza, snapshot, MCP, autonomia e quattro famiglie di host | Separare alpha manuale, MVP con agente esterno e release successive |
| Sicurezza in Phase 2 | Processo QEMU, IPC, file e console sono già superfici di attacco in Phase 1 | Threat model e controlli minimi prima del primo boot integrato |
| Multipiattaforma in Phase 6 | Percorsi, processi, accelerazione, console e isolamento dipendono dall'host | Prove Windows/Linux iniziali; build degli altri target presto; certificazione progressiva |
| AI Mode | Mescola delega a un agente esterno e runtime AI interno | MCP nel primo MVP; runtime integrato dopo |
| QEMU rilevato | Un binario trovato non dimostra che la VM possa avviarsi | Probe dell'acceleratore, della macchina e della configurazione realmente usata |
| Snapshot | Non distingue disco, RAM, stato dispositivi e consistenza applicativa | Baseline persistente a VM spenta nell'MVP; snapshot a caldo fuori perimetro |
| Rete | NAT o impostazioni proxy nel guest non applicano da soli una policy | Offline nell'MVP; egress controllato e verificato in una release dedicata |
| macOS guest | Non è una semplice voce aggiuntiva del wizard QEMU | Ricerca separata, con verifica tecnica e delle condizioni applicabili |
| Leggerezza | Non definisce processi inclusi e carico di riferimento | Budget misurati distinti per UI, supervisor, QEMU e LLM |
| Risultato del test | Il giudizio dell'LLM può essere scambiato per una verifica | Separare esecuzione, dichiarazione dell'agente ed evidenze verificabili |
| Permessi dell'agente | Un tool come `vm.create(config)` può introdurre percorsi e opzioni host | Template e identificativi di risorse autorizzate, mai configurazione QEMU arbitraria |
| Struttura del repository | Nove crate iniziali favoriscono interfacce premature | Confini logici da subito; crate aggiunti quando servono davvero |
| Stack UI | Una WebView con gateway WebSocket e CSP solo per mostrare un framebuffer e alcuni form amplia la superficie di attacco | UI nativa Rust con egui/eframe; console come texture |

Tre vincoli tecnici cambiano il piano:

1. La documentazione QEMU distingue la virtualizzazione dall'emulazione TCG e non attribuisce a quest'ultima garanzie di isolamento. QMP è inoltre un'interfaccia privilegiata, capace di attivare accessi al filesystem host. Decisione LightVM: niente fallback TCG per sessioni AI e niente inoltro QMP generico ai client. La pagina consultata appartiene al ramo di sviluppo: verificare i dettagli sulla release effettivamente scelta. [QEMU Security](https://www.qemu.org/docs/master/system/security.html)
2. `qemu-img` non deve modificare immagini in uso. Decisione LightVM: operazioni di baseline e ripristino solo dopo arresto e rilascio dei file. [QEMU disk image utility](https://www.qemu.org/docs/master/tools/qemu-img.html)
3. MCP prevede stdio e Streamable HTTP; con stdio il client avvia il server come sottoprocesso. Decisione LightVM: adapter stdio nel primo MVP, collegato a un servizio locale autorizzato dall'applicazione. Il pulsante desktop abilita o revoca l'accesso; non pretende di gestire direttamente un processo appartenente al client. [MCP Transports, revisione 2025-11-25](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports)

## 4. Perimetro delle release

### Alpha manuale, v0.1

Gestione di una VM Linux reale: wizard, selezione ISO autorizzata dall'utente, disco QCOW2, installazione manuale del guest, console, input, arresto, persistenza e diagnostica. Rete disattivata. Nessuna sessione AI disponibile.

### MVP pubblico, v0.2

La v0.1 più baseline a VM spenta, overlay per sessione, screenshot, bridge autorizzato, MCP stdio, revoca, Emergency Stop, cronologia, report JSON/Markdown e installer verificato.

Limiti deliberati:

- Un solo guest di riferimento: Ubuntu LTS x86_64, con versione e checksum fissati in F0.
- Un disco guest principale QCOW2, firmware configurato e relativo stato persistente gestito.
- Una VM in esecuzione e una sessione agente alla volta; catalogo capace di contenere più VM.
- Una console principale, con proprietà dell'input assegnata all'utente oppure all'agente.
- Nessuna rete, cartella condivisa, clipboard, passthrough, guest agent o shell host.
- ISO introdotte dall'utente; nessun catalogo automatico di immagini.
- Oltre al supporto di installazione, una VM può montare a VM spenta un secondo supporto ISO dati in sola lettura, proveniente dal registro media approvato, per portare nel guest il software da testare. È l'unico canale di ingresso di file nel guest previsto dal MVP: non è una cartella condivisa, non è scrivibile dal guest e non è gestibile dall'agente.
- QEMU installato separatamente, con guida e diagnostica. L'installer LightVM non va presentato come autosufficiente.
- Nessuna creazione o eliminazione arbitraria di VM da MCP: l'MVP controlla VM già predisposte e autorizzate dall'utente.
- Nessun provider LLM obbligatorio o runtime AI residente nell'applicazione.

MCP è disabilitato per difetto a ogni avvio dell'applicazione e nessuna autorizzazione attiva sopravvive al riavvio. L'eventuale adapter avviato dal client rimane privo di accesso fino all'abilitazione e all'approvazione nell'app. L'accesso a un provider esterno riguarda il processo agente sull'host e non abilita la rete del guest offline.

Il download di Firefox da Internet non è un test di accettazione per questo MVP. Il test iniziale usa un'applicazione già presente nel guest o un carico di test locale fornito tramite il supporto di installazione o il supporto dati approvato.

### Dopo il MVP

Rete filtrata; ampliamento certificato degli host; guest Windows; agente integrato opzionale; guest agent; import di immagini esterne; import di VM VMware; integrazione host/guest in uso manuale (cartelle condivise, clipboard, trascinamento file, USB); automazione più ricca. Queste funzionalità hanno priorità e verifiche separate, descritte più avanti.

## 5. Compatibilità: cosa si può promettere

La disponibilità di KVM, WHPX o HVF non dimostra il funzionamento di ogni combinazione di architettura, macchina e guest. QEMU documenta acceleratori diversi per host diversi. La matrice LightVM deve descrivere combinazioni provate. [QEMU Introduction](https://www.qemu.org/docs/master/system/introduction.html)

| Host | Guest iniziale | Acceleratore previsto | Impegno |
| --- | --- | --- | --- |
| Windows 11 x86_64 | Linux x86_64 | WHPX | Primo target di prodotto; certificazione obbligatoria per v0.2 Windows |
| Ubuntu LTS x86_64 | Linux x86_64 | KVM | Smoke test reale in F0 se il laboratorio è disponibile, altrimenti in R2a; certificazione pubblica successiva |
| Debian/Fedora x86_64 | Linux x86_64 | KVM | Estensione Linux dopo pacchetti e policy host verificati |
| macOS Apple Silicon | Linux ARM64 | HVF | R2b: modalità manuale/baseline; AI e rete filtrata abilitate solo con rispettivi controlli verificati |
| macOS Intel | Linux x86_64 | HVF | Target distinto; dipende da versioni host e hardware ancora mantenibili |
| Windows/Linux x86_64 | Windows x86_64 | WHPX/KVM | Dopo MVP; firmware, driver e requisiti della versione Windows verificati |
| Host ARM64 | Guest x86_64 tramite emulazione | TCG | Fuori dal profilo AI; eventuale funzione manuale sperimentale futura |
| Hardware Apple compatibile | macOS | Da studiare | Nessun impegno di release |
| Windows ARM64 | Guest ARM64 | Da verificare | Fuori dal piano iniziale |

Per ogni combinazione registrare: OS/build host, CPU, binario e versione QEMU, acceleratore, tipo macchina versionato, firmware, guest, console, snapshot, controlli di sicurezza, limiti effettivi e ultimo test superato. Stati ammessi: non verificato, sperimentale, supportato, bloccato. Il termine certificazione in questa roadmap indica una qualificazione interna basata sulle prove pubblicate, non una certificazione esterna.

La matrice distingue sempre gestione manuale, baseline, AI offline, Internet filtrato e guest agent. Il supporto manuale di un host non implica che tutte le modalità AI siano disponibili. macOS può raggiungere il traguardo manuale senza possedere ancora gli stessi meccanismi di contenimento Windows/Linux; in tal caso le modalità che li richiedono restano disabilitate.

Windows 10 non è il target principale: il supporto ordinario è terminato il 14 ottobre 2025; le eventuali eccezioni ESU/LTSC vanno valutate per edizione e build. L'eseguibilità tecnica non equivale a un profilo AI supportato. [Microsoft: fine del supporto Windows 10](https://support.microsoft.com/en-us/windows/deployment/updates-lifecycle/windows-10-support-has-ended-on-october-14-2025)

Apple documenta il percorso Virtualization.framework per eseguire macOS su Apple Silicon. È un possibile backend futuro, non una capacità da dedurre automaticamente dalla presenza di HVF in QEMU. La licenza macOS pone condizioni su hardware e uso: verificare quella della versione distribuita dall'utente. Nessuna immagine macOS o Windows viene redistribuita dal progetto nel piano iniziale. [Apple Virtualization](https://developer.apple.com/documentation/virtualization/running-macos-in-a-virtual-machine-on-apple-silicon), [licenza macOS Tahoe](https://www.apple.com/legal/sla/docs/macOSTahoe.pdf)

## 6. Architettura da fissare prima dello sviluppo esteso

### 6.0 Stack e vincoli di prodotto

Confermare Rust su tutta l'applicazione: UI nativa con egui ed eframe (winit più backend di rendering wgpu o glow), AccessKit per l'accessibilità, egui_extras per tabelle e immagini, egui_commonmark per la vista Markdown dei report; SQLite, manifest JSON versionati, QEMU/QEMU-IMG e QMP. La console resa come texture nativa tiene WebView, gateway WebSocket e CSP fuori dal perimetro di sicurezza. Versioni di egui/eframe e dei backend fissate in F0. [egui](https://github.com/emilk/egui) Nessuna WebView, nessun runtime web e nessuna dipendenza applicativa da Tauri, Electron, Docker o Proxmox; nessun servizio cloud, telemetria o account obbligatorio. Codice originale distribuito open source con licenza MIT; licenze dei componenti terzi inventariate e compatibilità della distribuzione verificata in F0/F6. La licenza scelta per LightVM non sostituisce quelle delle dipendenze.

Interfaccia nativa senza motore HTML né interprete di script: il desktop guest è una texture disegnata dal client display interno; report, log e testo proveniente dal guest sono resi come testo o Markdown limitato, mai interpretati come markup attivo né come link apribili automaticamente. Repaint solo su evento o su aggiornamento del framebuffer, stato tramite eventi e diagnostica periodica limitata: nessun polling continuo aggressivo. Nessun processo di inferenza se l'agente integrato è disattivato. La UI apre file e cartelle soltanto tramite il selettore nativo del sistema e non espone comandi host generici.

### 6.1 Confini e responsabilità

```text
Desktop egui/eframe                    Client MCP esterno
       |                                      |
       | comandi tipizzati                  stdio
       |                                      |
       |                            Adapter lightvm-mcp
       |                                      |
       +--------> Supervisor Rust <--- IPC locale autenticato
                        |
                policy e sessioni
                        |
                coda operazioni per VM
                        |
           backend QEMU / storage / console
                        |
                processo QEMU dedicato
                        |
                       guest
```

Il supervisor è l'unica autorità su processi, sessioni, policy e file VM. La logica risiede in librerie Rust indipendenti dalla UI. L'adapter MCP non apre dischi e non esegue direttamente QEMU. La UI riceve lo stretto necessario per le schermate e non può lanciare comandi host generici.

Il supervisor parte con LightVM e termina dopo la chiusura controllata delle VM. Nell'MVP non è un servizio installato sempre attivo. La perdita del supervisor deve interrompere le sessioni e attivare una strategia verificata per terminare i processi posseduti. Il recupero dell'interfaccia o dell'applicazione non riattiva automaticamente una sessione agente. Istanza singola del supervisor per catalogo; processi riconosciuti con handle/identità e identificativo di esecuzione, mai soltanto con un PID riutilizzabile.

Il supervisor è un processo distinto dal processo dell'interfaccia eframe, avviato e sorvegliato da quest'ultima e raggiungibile soltanto tramite l'IPC locale autenticato usato anche dall'adapter MCP. I processi QEMU sono figli del supervisor, non della UI. Su Windows appartengono a un Job Object del supervisor con terminazione automatica alla chiusura dell'handle; su Linux si usa un meccanismo equivalente scelto in F0, per esempio gruppo di processi con segnale alla morte del genitore. Un crash della UI revoca le sessioni agente secondo la sezione 7.3; la sorte delle VM manuali rimaste senza interfaccia è decisa in F0 tra arresto controllato e ricollegamento della UI. La decisione e la prova dei due scenari di crash sono registrate in un ADR.

La UI nativa parla con il supervisor soltanto attraverso l'IPC tipizzato: nessun comando generico, nessuna stringa interpretata come percorso o opzione. La policy per VM e sessione è applicata nel core Rust e vale allo stesso modo per UI e MCP; la UI non è un livello fidato aggiuntivo.

### 6.2 Organizzazione iniziale

```text
apps/desktop/              # UI egui/eframe
crates/vm-core/            # modelli, stati, casi d'uso, policy e sessioni
crates/qemu-backend/       # processi, QMP, probe, console
crates/vm-storage/         # SQLite, manifest, baseline, journal
crates/lightvm-service/    # supervisor e IPC locale
crates/mcp-server/         # aggiunto in F5
crates/lightvm-proto/      # tipi IPC/MCP e JSON Schema generati
tests/{integration,security,e2e}/
docs/{adr,verification}/
```

`vm-network`, `ai-control-bridge`, `vm-security`, `agent-runtime` e `test-runner` possono diventare crate separati quando hanno responsabilità e dipendenze stabili. I loro confini logici devono essere chiari prima. Non si implementa un framework di plugin o un secondo backend nel MVP.

### 6.3 Regole di implementazione

- Spawn tramite argomenti strutturati, mai stringhe per una shell; percorso QEMU approvato e verificato dall'utente.
- Validare anche la grammatica delle opzioni QEMU: evitare concatenazioni di percorsi in stringhe `-drive` e altri campi interpretabili. Preferire rappresentazioni strutturate, formato disco esplicito e protocolli allowlist. Lo spawn senza shell non impedisce da solo l'iniezione nelle opzioni di QEMU.
- Il registro media approvato usa l'ISO nel percorso scelto dall'utente, senza copiarla nel catalogo, e ne conserva identità e hash, riverificati prima di ogni avvio; un percorso selezionato non autorizza qualunque file che lo sostituisca successivamente: un file cambiato o spostato fa rifiutare l'avvio. Proteggere creazione/apertura contro symlink, junction e sostituzioni tra controllo e uso, con primitive OS e directory controllate. Non montare automaticamente filesystem guest sull'host.
- Non ereditare indiscriminatamente variabili d'ambiente, handle e descrittori: in particolare, non trasferire credenziali provider o del client MCP a QEMU.
- QMP interno, senza endpoint TCP aperto privo di autenticazione. Pipe ereditate o canali privati OS, verificati sul target; nessun tool `qmp.execute`.
- Backend con API ridotta: probe, create, start, shutdown, force-stop, status, observe, input e baseline. Le capacità non disponibili producono errori espliciti.
- Macchina a stati, ad esempio `Stopped → Starting → Running → Stopping → Stopped`, più `Failed/Recovering`; lo stato osservato deriva da processo e QMP, non dal solo database.
- Una coda di mutazioni per VM; blocco delle operazioni incompatibili; identificativo operazione e controllo delle revisioni.
- Policy effettiva pari all'intersezione tra limiti globali, policy VM, grant di sessione e capacità host disponibili; nessun livello amplia i precedenti. Definire `run_id` per esecuzione e `operation_id` per effetto; invalidare richieste riferite a un'esecuzione precedente dopo riavvio/ripristino.
- Retry sicuri per operazioni idempotenti. Input GUI non ripetuti automaticamente dopo un timeout: un secondo click può avere conseguenze diverse.
- SQLite per metadati, file per dischi e artefatti. Journal e riconciliazione per operazioni che attraversano DB e filesystem: una transazione SQL non rende atomico uno spostamento di file.
- Tipi e JSON Schema generati da una fonte unica, con errori pubblici stabili: `permission_denied`, `session_expired`, `vm_busy`, `unsupported`, `resource_limit`, `state_conflict`.
- Versioni dipendenze fissate e lockfile; versione QEMU e firmware testati registrati, senza dipendenza automatica dai rami `master`.

### 6.4 Console

Scelta iniziale da validare in F0: client RFB (VNC) scritto in Rust ed eseguito nel processo della UI, che riceve soltanto aggiornamenti del framebuffer da QEMU e li disegna come texture egui. Nessun gateway WebSocket, nessun ticket per un browser, nessuna WebView. Il client implementa il sottoinsieme RFB necessario (handshake, formato pixel, Raw, CopyRect e un encoding compresso) e rifiuta estensioni non previste; dipendenze VNC di terze parti sono ammesse solo se manutenute e inventariate, altrimenti il client è interno.

Il collegamento UI-QEMU usa un canale OS privato supportato dalla build, socket Unix con permessi restrittivi oppure named pipe con ACL su Windows se la build QEMU scelta li supporta per il display; altrimenti loopback con VeNCrypt X.509 e autenticazione del certificato client gestita da rustls. Il solo bind su localhost non è un meccanismo di autorizzazione e una password VNC tradizionale non è sufficiente. La credenziale del display è emessa dal supervisor per esecuzione e VM, distinta da quella QMP e dall'IPC amministrativo, e revocata con la sessione. La compatibilità di questo percorso su Windows è un requisito di F0. [QEMU VNC security](https://www.qemu.org/docs/master/system/vnc-security.html)

**Ripiego previsto:** se sul primo host nessuna delle due protezioni del canale è verificabile, F0 prova la console senza RFB: framebuffer ricostruito dal backend tramite cattura QMP `screendump` a frequenza limitata e consegnato alla UI attraverso l'IPC. Il ripiego mantiene gli stessi controlli di autorizzazione e revoca e accetta una latenza di aggiornamento maggiore, misurata e dichiarata in `docs/COMPATIBILITY.md`. Se nemmeno il ripiego supera le prove, il target è bloccato secondo la regola di F0. In nessun caso si attiva un endpoint VNC privo di autenticazione del client.

Il display è in sola lettura per costruzione: il client RFB non implementa l'invio di eventi di input, clipboard o power control, e l'input dell'utente passa attraverso lo stesso bridge autorizzato usato dagli agenti, con comandi QMP. Questa proprietà è verificata con un test negativo, non soltanto dichiarata. Tastiera e mouse sono letti dagli eventi di winit con scancode fisici e mappati sui qcode QEMU per layout dichiarati. Prototipare questo percorso completo prima di stabilizzare la UI.

Il bridge interno per input tipizzato e cattura esiste già in F1/F2; F4 aggiunge le sessioni e l'accesso delegato. Gli screenshot sono prodotti dal backend anche con console UI chiusa, senza catturare il desktop host. Usando QMP `screendump`, soltanto il backend decide il file temporaneo in una directory privata e ne applica dimensione, validazione e pulizia. QMP offre comandi di input e cattura: nessun nome di file di questi comandi è esposto al client. [QEMU QMP Reference](https://www.qemu.org/docs/master/interop/qemu-qmp-ref.html)

Dimensioni, coordinate, tasti supportati, layout tastiera e limiti Unicode sono dichiarati; screenshot e input riportano esecuzione e geometria del display per rifiutare coordinate obsolete. Il testo Unicode generale senza guest agent non va promesso prima dei test su layout diversi. Rilascio dei tasti premuti in caso di perdita di focus, disconnessione o cambio proprietario dell'input; nessuna lettura automatica della clipboard host.

## 7. Sicurezza: criteri minimi e limiti dichiarati

### 7.1 Threat model

Considerare ostili: contenuto guest, output del test, client MCP con permessi insufficienti, parametri ricevuti, immagini importate e processi locali senza credenziale che tentano di raggiungere l'IPC o il canale del display.

Proteggere file e credenziali host, altre VM, policy, endpoint di controllo, disponibilità delle risorse e integrità dei report. Esplicitare il rischio residuo di vulnerabilità nell'hypervisor, nei dispositivi emulati e nelle dipendenze.

L'MVP non promette una sandbox per malware deliberatamente ostile, resistenza a un amministratore host compromesso o separazione assoluta da processi malevoli dello stesso utente. L'assenza di cartelle condivise limita i percorsi previsti; non dimostra che un processo QEMU compromesso sia confinato nel filesystem.

LightVM limita ciò che un agente può fare **attraverso LightVM**. Non può revocare shell, filesystem o strumenti esterni che il client agente possiede già. Per una sessione destinata a operare soltanto nella VM, il client deve essere configurato anche con strumenti e permessi coerenti.

### 7.2 Profilo richiesto per il MVP AI

1. Accelerazione hardware e configurazione QEMU approvate; nessun fallback silenzioso TCG.
2. Nessuna NIC guest, con esclusione esplicita delle periferiche di rete predefinite; nessun forwarding.
3. Nessuna condivisione host, clipboard, USB passthrough, disco fisico o dispositivo non necessario. Dispositivi HID virtuali allowlist, come un puntatore assoluto, sono ammessi: non equivalgono al passthrough di una periferica host.
4. Processi senza elevazione, ambiente ripulito e supervisione dell'intero albero processi.
5. Accesso a dischi, ISO e artefatti tramite registri interni; il client MCP fornisce ID, non percorsi host.
6. QMP privato, console protetta, IPC autenticato e permessi OS restrittivi.
7. Sessioni con scadenza, VM allowlist, azioni consentite, quote, revoca e proprietà dell'input.
8. Limiti CPU/memoria applicati all'albero processi per AI Mode, oltre alla configurazione virtuale del guest; budget complessivo di storage e riserva host con natura del controllo dichiarata.
9. Audit delle azioni e dei dinieghi, con minimizzazione di segreti e contenuti sensibili.
10. Stop e revoca indipendenti dal modello e dalla risposta del client.

Le eccezioni previste da R6 (rete su allowlist di endpoint, cartelle condivise, clipboard, trasferimento file, USB, ispezione dell'interfaccia guest) sono capability concesse esplicitamente nel grant e approvate nell'app: senza grant vale questo profilo; quelle concesse sono visibili nella sessione e registrate nel report.

I Job Objects Windows sono candidati per limiti e gestione del ciclo di vita dei processi: non sostituiscono una sandbox del filesystem. I controlli di accesso e sicurezza del processo vanno progettati separatamente. [Microsoft Job Objects](https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects)

In F0 decidere e provare il livello di confinamento addizionale concretamente disponibile su Windows e Linux. Distinguere sempre tra configurazione guest restrittiva, controllo delle API e sandbox del processo host. Un controllo obbligatorio assente blocca AI Mode; un controllo addizionale assente viene descritto come limite senza trasformarlo in una garanzia equivalente.

Il numero di vCPU non è un limite percentuale di CPU e la RAM assegnata al guest non è il limite dell'intero processo QEMU. La dimensione virtuale del disco non limita da sola lo spazio totale di overlay, snapshot, log e screenshot. Il piano deve contabilizzare tutti questi consumi, anche prima di accettare una nuova sessione.

Per CPU/memoria, F0 prova i limiti effettivi sul processo QEMU e sui suoi discendenti, inclusi gli effetti dell'acceleratore; una metrica apparentemente limitata non basta. Per storage distinguere quota rigida OS, se disponibile, da budget applicativo con monitoraggio e arresto. Il profilo base ammette quest'ultimo dichiarandone il rischio di superamento prima dello stop; una policy che richiede quota rigida non può partire senza tale capacità. Soglie, frequenza massima di controllo, riserva e limiti degli artefatti sono fissati in F0 e testati sotto pressione. Nessuna promessa di protezione dal riempimento del volume causato da altri processi host.

### 7.3 Autorizzazione e arresto

L'utente autorizza un insieme delimitato di operazioni per una durata definita. Ogni chiamata è verificata al ricevimento e immediatamente prima dell'inoltro al backend; il client non può ampliare i propri permessi.

Separare:

- **Pausa agente:** blocca l'inoltro di nuove mutazioni dell'agente; la VM continua a eseguire processi già avviati. Non equivale a sospendere o mettere in sicurezza il guest.
- **Revoca:** invalida la sessione, scarta operazioni non ancora inoltrate e chiude i canali autorizzati. Le operazioni già consegnate a QEMU possono ancora produrre effetti.
- **Emergency Stop:** revoca e richiede direttamente la terminazione dell'albero QEMU con primitive OS, senza attendere una risposta QMP o lo shutdown guest. Può perdere lo stato dell'overlay, ma non deve alterare la baseline; il completamento è dichiarato solo dopo aver osservato la fine dei processi.

Il kill switch ha un percorso prioritario nel supervisor, senza dipendere dalla coda QMP bloccata. Prevedere anche un comando locale di emergenza se la UI si blocca e un meccanismo OS di terminazione dei figli se il supervisor muore. Nessun comando dipende dal consenso dell'LLM. Il punto di revoca è quello oltre il quale il supervisor non inoltra nuove mutazioni; il report distingue coda scartata, richieste già inoltrate ed effetti osservati. Non si promette l'annullamento retroattivo di input o comandi già inviati.

Il grant ha scadenza e lease verificati dal supervisor, non dal client. Su perdita del client, scadenza, chiusura/crash UI o sospensione/ripresa host, le sessioni AI non proseguono indefinitamente: revoca e stop secondo il percorso di emergenza. Lease senza rinnovo valido non superiore a 10 secondi; al resume si controllano scadenze prima di riaccettare input. La ripresa dell'agente richiede una nuova autorizzazione. Questa regola riguarda le sessioni AI; la chiusura manuale dell'app gestisce separatamente il salvataggio del lavoro dell'utente.

### 7.4 Test negativi obbligatori

| Area | Tentativo | Esito richiesto |
| --- | --- | --- |
| Sessioni | Token assente, scaduto, revocato o riutilizzato dopo riavvio | Diniego senza effetto sulla VM |
| VM | Sessione autorizzata su A richiede screenshot o input su B | Diniego; nessun dato di B |
| Host | Parametri con path traversal, UNC, device path, symlink/junction o opzioni QEMU | Rifiuto; nessun accesso arbitrario |
| Rete | Guest tenta host, LAN, Internet e IPv6 in profilo offline | Nessun percorso di rete disponibile |
| IPC/console | Processo locale senza credenziale tenta accesso all'IPC o al canale del display | Connessione rifiutata |
| Policy | Testo guest chiede di disabilitare i controlli | Nessun cambiamento alle autorizzazioni |
| Revoca | Flood di input in coda durante Emergency Stop | Nessun nuovo inoltro dopo la revoca; richieste già inviate identificate; fine dei processi verificata |
| Storage | Ripristino concorrente al boot; eliminazione di base referenziata | Operazione impedita |
| Crash | Processo ucciso durante creazione, rollback o aggiornamento DB | Recupero in stato sicuro, baseline integra |
| Risorse | Richiesta VM sovradimensionata; flood screenshot/log | Rifiuto o contenimento entro i limiti dichiarati |
| Report | Log con HTML/script, sequenze terminale o link pericolosi | Visualizzazione come testo; nessun markup attivo né apertura automatica di link |
| Lifecycle | Client perso, UI chiusa, supervisor terminato, host ripreso da sospensione | Sessione revocata, niente riavvio autonomo e processi gestiti secondo policy |
| Console | Tentativo di inviare clipboard, power control o input sul canale RFB, fuori dal bridge | Il client non implementa quei messaggi e la credenziale del display non è disponibile ad altri processi; nessun percorso alternativo per mutare la VM |
| File | ISO sostituita dopo l'approvazione; opzioni QEMU nei metadati | Identità verificata e nessuna interpretazione come configurazione host |

Test negativi superati dimostrano le proprietà testate, non l'assenza di ogni possibile VM escape.

## 8. Snapshot e ripristino: modello del MVP

Usare il termine visibile **baseline a VM spenta**. Non presentarlo come salvataggio completo della RAM.

1. L'utente installa Ubuntu sul disco gestito da LightVM.
2. La VM viene arrestata correttamente; il supervisor attende l'uscita e il rilascio dei file.
3. Si registra una baseline immutabile del disco e della configurazione, includendo lo stato firmware scrivibile pertinente.
4. Ogni test usa un overlay QCOW2 con backing file interno fissato, più una copia dello stato firmware della baseline.
5. Alla chiusura si arresta la VM, si finalizzano le evidenze e si scarta o conserva l'overlay secondo la scelta autorizzata.
6. Il ripristino crea un nuovo overlay dalla baseline e ripristina anche gli altri stati persistenti gestiti; il guest riparte da un boot normale.

Un disco non è l'intero stato VM: UEFI NVRAM e, in futuro, stato TPM e dischi aggiuntivi devono partecipare al modello di consistenza. Nel MVP non si supportano snapshot RAM, snapshot live, migrazione, catene arbitrarie di backing file o commit automatico dell'overlay nella base.

La baseline rimane in sola lettura per le sessioni. Cancellazione solo se non referenziata; operazioni distruttive serializzate; manifest versionati e riconciliazione dopo crash. Il budget storage considera overlay conservati, overhead QCOW2 e copie dei metadati.

Una baseline persistente è un disco autonomo gestito, ottenuto a VM spenta; un nuovo checkpoint da overlay viene materializzato in un'immagine autonoma a VM spenta, senza catene crescenti. La baseline iniziale di una sessione non viene sostituita da un checkpoint creato dall'agente. Conservare più baseline è possibile entro quota; la cancellazione esplicita rifiuta quelle referenziate. L'arresto forzato produce uno stato non pulito: non promuoverlo automaticamente a baseline verificata. Anche uno shutdown osservato non certifica da solo la consistenza applicativa di ogni programma guest.

Lo stato firmware attivo e gli overlay sono proprietà della specifica esecuzione; lo stop è requisito anche per operazioni su NVRAM. Il ripristino chiude la sessione di controllo e invalida richieste pendenti della vecchia esecuzione. Conservazione o eliminazione dell'overlay e termine del report devono rimanere tracciabili anche se il disco si riempie; conservare una riserva per manifest ed esito minimo, senza promettere screenshot in caso di storage indisponibile.

Il ripristino riguarda soltanto lo stato VM gestito: non annulla chiamate a servizi esterni, addebiti dei provider o altri effetti già prodotti in rete nelle release successive. Evidenze e audit sull'host rimangono separati dai dischi ripristinati.

Import di QCOW2 esterni rinviato: backing chain, riferimenti a file o protocolli host e parsing dell'immagine richiedono una validazione separata. Per il primo MVP si creano soltanto dischi controllati da LightVM. L'import di VM VMware (R6) converte i VMDK in dischi gestiti con le regole di R4b.

## 9. MCP, sessioni e report

Baseline di protocollo: MCP 2025-11-25. Selezionare in F0 una release dell'SDK Rust ufficiale che la supporti e provarla con le versioni dei client prescelti; rifiutare negoziazioni non supportate. Non usare dipendenze dal ramo principale né presumere che la revisione scelta sia per sempre la più recente. [SDK Rust MCP](https://github.com/modelcontextprotocol/rust-sdk)

L'adapter stdio si collega al supervisor mediante named pipe con ACL su Windows o socket locale con permessi restrittivi su Unix. L'identità OS e una credenziale di sessione approvata nell'app concorrono all'autorizzazione. Non adottare OAuth HTTP come soluzione automatica per stdio: MCP tratta i due trasporti diversamente. [MCP Authorization](https://modelcontextprotocol.io/specification/2025-11-25/basic/authorization)

La prima connessione crea soltanto una richiesta di pairing con identità e codice verificabili nell'app. L'utente approva VM, strumenti, durata e quote; soltanto allora il supervisor emette un grant limitato. La credenziale di pairing non conferisce permessi VM da sola, non è riutilizzabile come grant globale ed è revocabile. Dati sensibili conservati nell'archivio credenziali OS; l'adapter legge soltanto ciò che gli occorre e non propaga segreti a QEMU. Un processo malevolo dello stesso utente resta fuori dalle garanzie assolute dichiarate nel threat model.

Nell'MVP:

- L'app crea la sessione, definisce obiettivo e limiti e autorizza VM e strumenti.
- L'adapter non avvia automaticamente una VM o riaccende l'accesso MCP se disabilitato.
- Esporre elenco filtrato, stato, avvio/arresto autorizzati, screenshot, mouse, tastiera, log e ciclo di test/baseline nei limiti della sessione.
- Nessun passthrough host, QMP generico, path host, cancellazione VM o modifica policy.
- Nessun tool monta, sostituisce o rimuove supporti: i media, incluso il supporto dati approvato, sono gestiti dall'utente nell'app a VM spenta.
- Le credenziali non compaiono negli argomenti di processo, nei report o negli esempi committati.
- Rate limit per identità/sessione, code limitate, timeout e limitazione del volume di dati.
- Comandi lunghi restituiscono uno stato tracciabile; il timeout non deve generare retry ciechi e duplicazione degli effetti.
- Screenshot restituiti come contenuto immagine MCP con metadati di VM/esecuzione/geometria e limiti di byte, non come percorso host che il client dovrebbe aprire. Risultati strutturati validati rispetto agli schemi; errori operativi distinti da errori del protocollo. `stdout` contiene solo MCP, diagnostica su `stderr` e comunque priva di segreti. [MCP Tools](https://modelcontextprotocol.io/specification/2025-11-25/server/tools)
- `vm.logs` nell'MVP comprende eventi LightVM e log QEMU autorizzati, con segreti e percorsi host sensibili rimossi, non lettura arbitraria di log dentro il guest. Osservazione e consultazione di artefatti sono soggette alle stesse regole di autorizzazione delle mutazioni.
- Configurazioni di esempio per almeno due client realmente provati, con versione e data della verifica.

Catalogo v0.2: `vm.list`, `vm.status`, `vm.start`, `vm.stop`, `vm.reset`, `vm.screenshot`, `vm.mouse.move`, `vm.mouse.click`, `vm.keyboard.type`, `vm.keyboard.press`, `vm.snapshot`, `vm.restore`, `vm.logs`, più consultazione operazioni/report per ID. Elenco e stato sono filtrati dal grant; reset e checkpoint richiedono capability esplicite. `vm.snapshot` e `vm.restore` rifiutano la VM accesa e non eseguono shutdown nascosti; il chiamante deve prima ottenere uno stop completato. La baseline iniziale protetta non è cancellabile o sostituibile da questi tool. `vm.create`, `vm.delete` e i tool guest restano assegnati a R4.

Per ogni sessione conservare `session_id`, `vm_id`, `run_id`, `baseline_id`, OS guest, manifest VM/baseline, revisione della policy applicata, versioni, obiettivo, eventi in ordine, timestamp UTC, risultati dei tool, screenshot con hash, errori, causa di terminazione, dichiarazione dell'agente e risultato motivato della verifica.

Stati del test: `passed`, `failed`, `inconclusive`, `cancelled`, `timed_out`, `infrastructure_error`. Il semplice completamento della conversazione non produce automaticamente `passed`. Il primo test dimostrativo usa un'applicazione reale nel guest con risultato osservabile e criteri fissati prima dell'esecuzione.

In F0 definire una fixture grafica deterministica da eseguire realmente nel guest, con applicazione scelta, sequenza di azioni e risultato atteso verificabile; non un mock di QEMU. La fixture è realizzata e distribuita tramite il supporto dati approvato entro F3, prima dei test di regressione del bridge e del rollback che la usano. In F5 aggiungere il client LLM reale e distinguere i suoi fallimenti dalla correttezza infrastrutturale: un fallimento del modello non va nascosto, ma non equivale automaticamente a un bug del manager. Obiettivi arbitrari senza un verificatore affidabile restano `inconclusive` o ricevono una valutazione umana esplicitamente attribuita.

Il report JSON è il dato canonico; Markdown è una vista esportata. L'export su filesystem host è controllato dall'utente e non accetta destinazioni arbitrarie da MCP. Screenshot e log hanno limiti di conservazione; non devono registrare automaticamente ogni testo digitato, soprattutto password.

Log di audit con tipo azione, ID, esito e ordine, separati dai payload sensibili. Screenshot possono contenere segreti visibili nel guest: offrire controlli espliciti di cattura/conservazione e cancellazione, senza promettere riconoscimento automatico affidabile delle password. Hash degli artefatti rilevano alterazioni rispetto al manifest, ma un audit locale modificabile dallo stesso utente non costituisce una prova forense resistente a manomissioni.

## 10. Roadmap con dipendenze e criteri di uscita

Le fasi sono sequenziali: ogni fase parte soltanto dopo il criterio di uscita della precedente.

| Fase | Dipende da | Risultato |
| --- | --- | --- |
| F0 — Fattibilità e decisioni | Hardware disponibile | Prove reali e architettura approvata |
| F1 — Fondazioni e controllo QEMU | F0 | Lifecycle headless e sicurezza iniziale |
| F2 — Alpha manuale | F1 | VM Linux usabile nella console desktop |
| F3 — Baseline e recupero | F2 | Test ripetibili e storage affidabile |
| F4 — Bridge e sessioni | F3 | Controllo autorizzato e revocabile |
| F5 — MCP e report | F4 | Scenario completo con agente esterno |
| F6 — Stabilizzazione pubblica | F5 | Installer, regressione e revisione sicurezza |

F0 realizza prove di fattibilità riutilizzabili dove adatte; F1–F6 le trasformano in implementazione di prodotto. Non si conta due volte lo sviluppo completo delle stesse funzioni.

### F0 — Fattibilità e decisioni

Attività bloccanti per l'uscita:

- Verificare toolchain Rust, build minima reale di eframe su Windows e Linux, backend di rendering (wgpu o glow) e comportamento su host senza accelerazione grafica, per esempio sessioni RDP.
- Installare/configurare QEMU nell'ambiente di sviluppo con provenienza verificata; fissare binario, versione, firmware e guest con checksum. Il probe non carica credenziali utente nel guest.
- Avviare il guest su Windows/WHPX con configurazione documentata; su Linux/KVM se il laboratorio è disponibile.
- Provare QMP privato, console nativa con client RFB sul canale protetto o il suo ripiego, input, screenshot e arresto forzato.
- Provare il ciclo base immutabile/overlay/ripristino con firmware persistente.
- Provare il modello di processo del supervisor: QEMU figlio del supervisor, terminazione OS dei figli, crash della UI e crash del supervisor.
- Identificare i limiti OS disponibili per CPU, memoria e storage e registrare una prima misura dei consumi.
- Definire threat model, restrizioni del MVP, matrice di supporto iniziale e fixture di test (applicazione, azioni, risultato atteso).
- Scrivere la prima versione di `ARCHITECTURE.md` e `SECURITY.md` e gli ADR delle decisioni critiche.

Attività avviate in F0 e completabili entro F1 senza violare i criteri di uscita:

- Selezione e prova dell'SDK MCP e dei client di riferimento.
- Inventario packaging e licenze delle dipendenze.
- `COMPATIBILITY.md` e `TESTING.md` completi; scelta del framework di test UI.
- Realizzazione della fixture nel guest, entro F3 come indicato nella sezione 9.

**Criterio di uscita:** boot accelerato e console/input dimostrati sul primo host; canali di controllo protetti; percorso di ripristino e supervisione fattibili; limiti risorse identificati e fixture definita. Sono necessarie prove Windows e Linux, oppure un risultato di blocco esplicito per il target fallito, oppure l'esito "verificato solo Windows" descritto sotto. Una lista di tecnologie installate non basta.

**Decisione di arresto o cambio target:** dopo F0, Windows rimane primo target se supera i controlli. Se fallisce, Linux può diventare primo target solo se supera gli stessi controlli e dispone del laboratorio necessario. Se nessuno li supera, il progetto resta bloccato su quel criterio. Hardware mancante significa verifica pendente. Non sostituire l'acceleratore con TCG per far risultare conclusa la fase; il cambio target previsto qui non riduce i criteri di sicurezza.

**Laboratorio Linux assente:** se in F0 non è disponibile un host Linux fisico con KVM, F0 può chiudersi con esito "verificato solo Windows", registrato in `docs/STATUS.md` e `docs/COMPATIBILITY.md`. In tal caso Windows resta l'unico target del MVP, la cella Ubuntu della matrice resta "non verificato" e la qualificazione Linux è rinviata a R2a, che non può concludersi senza quella prova. WSL2, macchine virtuali annidate o runner senza accelerazione verificabile non sostituiscono l'host fisico per questa prova; servono soltanto per build e test indipendenti dall'acceleratore.

### F1 — Fondazioni e controllo QEMU

Attività:

- Workspace, build riproducibile, lint, migrazioni SQLite e tipi condivisi.
- Rilevamento capacità e diagnostica con errori utilizzabili dall'utente.
- Modello VM, macchina a stati, validazione risorse e registro dei media approvati.
- Gestione processi, QMP asincrono, timeout, eventi e riconciliazione.
- API interne di input/cattura per la futura console; audit minimo di creazione, avvio, arresto e dinieghi già in questa fase.
- Profilo offline predefinito e comandi QEMU costruiti da opzioni controllate.
- Skeleton del supervisor e test headless senza dipendenza dalla UI.
- Chiusura delle attività F0 non bloccanti: SDK MCP selezionato e provato, inventario packaging/licenze, `COMPATIBILITY.md` e `TESTING.md` completi, framework di test UI scelto.
- CI Windows/Linux; build macOS su runner adeguato entro F2 per individuare assunzioni di portabilità, senza dichiarare supporto operativo. Se il runner manca, segnalare il rischio in `STATUS.md` e impedire l'etichetta di target verificato.

**Criterio di uscita:** creazione disco e almeno 20 cicli start/stop reali senza processi orfani; errore chiaro con acceleratore assente; nessuna NIC inattesa; rifiuto di percorsi e risorse non validi. Test sintetici e mock sono ammessi per errori/protocollo, ma non sostituiscono il boot reale.

### F2 — Alpha manuale

Attività:

- Dashboard, wizard, import ISO tramite selettore utente e gestione disco.
- Dashboard con OS, stato, CPU/RAM configurate, dimensione virtuale e spazio occupato; wizard nome/OS/media di installazione/supporto dati opzionale/CPU/RAM/disco/rete/riepilogo, con sola rete Offline selezionabile nel MVP.
- Installazione manuale Ubuntu, boot successivo dal disco e persistenza.
- Console come texture nel pannello UI, fullscreen, input con layout dichiarati, screenshot e stato connessione.
- Arresto gentile con timeout; arresto forzato distinto; reset esplicito.
- Dialoghi coerenti per eliminazione e chiusura app con VM attiva.
- Modifica della configurazione soltanto a VM spenta, entro le capacità disponibili; modifiche che invalidano la baseline richiedono una nuova baseline e non alterano quella esistente.
- Dark mode predefinita, light mode, layout ridimensionabile, controlli accessibili da tastiera e albero AccessKit verificato con uno screen reader per piattaforma; stato e azioni distinguibili senza basarsi solo sul colore. Scala DPI e multi-monitor verificati. Settings per percorsi QEMU, capacità, risorse e sicurezza; rete/MCP non disponibili chiaramente indicati.
- Installer sperimentale e primo test su macchina pulita.

**Criterio di uscita:** un utente segue la guida, installa Ubuntu, riapre LightVM e ritrova la VM installata. Tastiera italiana/inglese e coordinate dopo ridimensionamento verificate. Nessuna schermata con dati simulati nel percorso di prodotto.

**Milestone:** v0.1 alpha manuale.

### F3 — Baseline e recupero

Attività:

- Baseline offline, overlay, manifest e stato firmware.
- Lock, journal, controlli riferimenti e quote del catalogo.
- Ripristino dopo interruzione e pulizia prudente dei temporanei.
- Divieto di modificare dischi in uso e di cancellare basi referenziate.
- Estensione dell'audit alle operazioni storage e raccolta artefatti; schermata baseline con creazione, ripristino ed eliminazione autorizzata.

**Criterio di uscita:** 30 cicli di modifica nel guest e ripristino con ritorno allo stato iniziale; hash della base invariato; prove di crash nei punti critici; nessuna perdita del disco originale.

### F4 — Bridge e sessioni

Attività:

- Estensione delle API interne di F1/F2 a screenshot/mouse/tastiera/stato con VM, sessione e identificativo di esecuzione.
- Capability, scadenze, quote, allowlist e controllo esclusivo dell'input.
- Pausa, revoca, Emergency Stop e comando locale di emergenza.
- Audit completo e contenimento di input, output e concorrenza.
- Test negativi e misurazione delle latenze di stop.

**Criterio di uscita:** ogni chiamata protetta è verificata dal core; una sessione non controlla altre VM; revoca e stop funzionano anche con client non collaborante e coda piena. Input provati contro una VM reale.

### F5 — MCP e report

Attività:

- Adapter stdio, pairing/autorizzazione locale, handshake e schema tool.
- Collegamento di almeno due client compatibili, con versioni documentate.
- Schermata sessione con obiettivo, azioni, limiti e artefatti.
- AI Workspace e cronologia Sessions; indicatore persistente della VM controllata e Emergency Stop sempre raggiungibile durante la sessione. Permessi approvati nell'app, senza pulsanti guest capaci di alterare policy.
- Report JSON/Markdown e stati di esito distinti.
- Scenario completo: controllo di un'app nel guest, report, revoca e ripristino.

**Criterio di uscita:** fixture deterministica completata e ripristinata dieci volte senza errori infrastrutturali; almeno un percorso riuscito con ciascuno dei due client LLM reali e report corretti anche per fallimento, timeout e inconcludente. Registrare tutti i tentativi del campione, inclusi i fallimenti. Il campione non è una stima statistica della precisione del modello. Nessun runtime LLM integrato richiesto.

### F6 — Stabilizzazione pubblica

Attività:

- Installer finale, gestione dipendenze e disinstallazione che conserva VM/dati salvo eliminazione separata esplicita. Distribuzione Windows pubblica con firma del codice e checksum verificabili; equivalenti misure di provenienza nel target Linux alternativo. Budget/credenziali di firma mancanti bloccano la pubblicazione prevista, non autorizzano dichiarazioni di firma inesistenti.
- Test su almeno due host fisici del target certificato, includendo configurazioni Intel/AMD se entrambe dichiarate supportate.
- Regressione storage, sessioni, sicurezza e sospensione/ripresa dell'host.
- Recupero dopo crash, controlli su spazio esaurito e aggiornamento del database.
- Backup dei metadati prima della migrazione, rifiuto di downgrade incompatibili, aggiornamento solo con VM ferme e ripetizione dei probe dopo cambi QEMU/firmware. MVP con aggiornamento manuale: nessun auto-updater aggiunto implicitamente.
- Revisione indipendente mirata su boundary MCP, QMP, path, console e processi.
- SBOM, inventario licenze, istruzioni di aggiornamento QEMU e gestione vulnerabilità.
- Documentazione riproducibile e limitazioni visibili nel prodotto.
- Verificare che l'artefatto pubblico non includa endpoint di test, driver di automazione UI incorporato, credenziali o impostazioni di debug. Test UI strumentati e prove sull'installer reale hanno evidenze distinte.

**Criterio di uscita:** tutti i test bloccanti verdi sull'artefatto distribuito, nessuna vulnerabilità critica/alta nota non mitigata nel perimetro, prove archiviate e nessuna funzionalità pubblicizzata soltanto perché compila.

## 11. Definition of Done del MVP pubblico

Tutti i punti sono obbligatori sul target dichiarato supportato:

1. Installare LightVM su un host pulito seguendo prerequisiti documentati.
2. Rilevare QEMU oppure guidare l'utente alla configurazione; verificare accelerazione con un probe reale.
3. Selezionare una ISO Ubuntu valida e creare una VM offline.
4. Installare il guest e riavviarlo dal disco persistente.
5. Usare console, mouse e tastiera senza uscire dall'applicazione.
6. Arrestare correttamente la VM e creare una baseline.
7. Abilitare MCP e autorizzare una sessione su quella sola VM.
8. Collegare un client esterno e svolgere un test GUI reale e ripetibile.
9. Rifiutare l'accesso a un'altra VM, ai percorsi host e alle operazioni fuori policy.
10. Produrre eventi, screenshot e un report con un esito sostenuto dalle evidenze.
11. Dimostrare pausa, revoca ed Emergency Stop con un client che continua a inviare richieste.
12. Ripristinare la baseline dopo arresto e verificare il ritorno allo stato iniziale.
13. Chiudere e riaprire l'app senza sessioni riattivate, processi orfani o metadati incoerenti.
14. Superare i test negativi dichiarati, documentando esattamente ciò che dimostrano.

## 12. Roadmap successiva: ordine consigliato

| Traguardo | Condizione di rilascio |
| --- | --- |
| R1 — Internet filtrato | Percorso egress non aggirabile dal guest, prove IPv4/IPv6/DNS e blocco host/LAN |
| R2 — Portabilità certificata e Windows guest | Hardware disponibile, installer e suite reale per ogni combinazione |
| R3 — Agente integrato opzionale | Provider adapter, loop limitato, cancellazione e valutazione risultati |
| R4 — Guest agent e import avanzato | Dopo v1.0; tre incrementi distinti R4a–R4c, ciascuno con verifica propria |
| R5 — macOS guest | Solo studio iniziale; nuova decisione tecnica, legale e di prodotto prima di qualunque implementazione |
| R6 — Import VMware, snapshot, integrazione, rete, Windows 11 x64, ispezione UI | Conversione verificata su VM VMware reali; snapshot del disco provati su guest reali; integrazioni e rete concesse solo tramite grant; Windows 11 x64; ispezione UI |

L'ordine pianificato è F0–F6, R1, R2a–R2c, R3, poi R4. R6 procede in parallelo per decisione del proprietario e non è requisito di v1.0. La v1.0 richiede MVP, R1, R2a–R2c e R3; non si dichiara completa se una di queste qualificazioni è bloccata. R2d e R5 sono estensioni condizionali senza impegno di consegna dell'intera visione originaria.

### R1 — Internet filtrato

Scegliere un solo meccanismo controllato dal lato host: rete virtuale privata verso un gateway/proxy dedicato, senza percorso alternativo. Il guest non deve ottenere accesso esterno rimuovendo una propria impostazione proxy. Per il primo incremento supportare soltanto protocolli necessari, ad esempio HTTP/HTTPS, con policy chiuse per difetto.

Verificare destinazioni risolte e connessioni effettive, indirizzi locali e LAN anche se pubblici, loopback, link-local, reti private, IPv6, redirect, DNS rebinding, VPN, proxy di sistema e connessioni persistenti dopo revoca. Bloccare UDP e altri protocolli non implementati. Un crash del gateway deve chiudere la connettività.

Primo incremento limitato a policy di destinazione/porta, non controllo del contenuto HTTPS. Risoluzione DNS controllata dal gateway; nessuna uscita DNS alternativa dal guest. Con HTTPS tramite CONNECT non si afferma di vedere redirect o path cifrati: si verificano tutte le nuove destinazioni/tunnel e gli IP effettivi. Filtri su URL completi richiederebbero un progetto separato. Niente intercettazione TLS trasparente inserita implicitamente.

Il servizio di test per il primo caso browser deve essere raggiungibile attraverso il percorso consentito; `localhost` del guest non è quello dell'host. L'accesso a un server di sviluppo host richiede una futura eccezione Custom Network esplicitamente autorizzata e non è necessario per superare R1.

L'opzione QEMU `restrict=on` non costituisce da sola Internet filtrato: la documentazione dice che blocca il routing esterno ordinario, salvo forwarding espliciti. Non basta attivarla per soddisfare il requisito del prompt. [QEMU network options](https://www.qemu.org/docs/master/system/qemu-manpage.html)

L'accesso Internet autorizzato può comunque trasmettere i dati presenti nel guest alle destinazioni consentite. Una policy di rete non è una garanzia generale contro l'esfiltrazione. Custom Network resta una scelta manuale esplicita, incompatibile con il profilo restrittivo fino a verifiche dedicate.

### R2 — Portabilità

Incrementi vincolanti per v1.0:

- **R2a:** qualificare entrambi gli host Windows 11 e Ubuntu LTS x86_64 con guest Linux, gestione manuale, AI offline e Internet filtrato R1, completando quello non qualificato nel MVP. Se Windows era bloccato e lo rimane, R2a non è conclusa.
- **R2b:** macOS Apple Silicon con Linux ARM64 in modalità manuale offline, incluso firmware, console, baseline, installer firmato/notarizzato e matrice dei limiti effettivi. Hardware fisico obbligatorio. AI e Internet filtrato su macOS sono estensioni condizionali: restano disabilitate se mancano i controlli richiesti, senza bloccare il traguardo manuale e senza dichiarare parità funzionale.
- **R2c:** Windows guest x86_64 su Windows 11/WHPX e Ubuntu/KVM, con driver, stato persistente e recupero verificati.

**R2d, estensione condizionale fuori v1.0:** Debian/Fedora e macOS Intel su versioni mantenibili e hardware disponibile; richiedono un piano operativo e qualificazione per target. Windows 10 resta compatibilità legacy condizionata a edizione/build e manutenzione effettiva. Nessuna di queste celle si considera supportata per analogia con un altro sistema.

Un guest Windows introduce firmware, driver, licenza e, a seconda della versione, TPM/Secure Boot. Lo stato dei dispositivi aggiunti deve entrare nel ripristino. Nessun bypass dei requisiti viene assunto come funzione del prodotto.

### R3 — Agente integrato

Reimpiegare il bridge già verificato. Aggiungere pianificazione, osservazione/azione, checkpoint, limite azioni/durata, gestione errori, provider configurabili e report. Nessun ragionamento dell'LLM modifica le autorizzazioni.

Per v1.0 basta un provider vision/tool-use configurabile e verificato, con credenziale fornita dall'utente e conservata nell'archivio OS. Ulteriori provider, inferenza locale incorporata e download automatici di modelli non sono requisiti di questa release. Gestire esaurimento budget, rete/provider indisponibile, loop senza progresso, cancellazione e limite output; nessun retry illimitato. Usare confronti deterministici ove disponibili, senza affidare ogni esito al modello.

Distinguere prezzo stimato, consumi contabilizzati e quota realmente applicata dal provider. LightVM può limitare le proprie richieste; non può imporre un tetto di spesa globale a un client esterno. Screenshot e log inviati a provider remoti richiedono una scelta consapevole; il manager rimane utilizzabile offline e senza cloud.

### R4 — Guest agent

Tre incrementi dopo v1.0: **R4a guest agent**; **R4b import immagini/configurazioni e policy JSON**; **R4c provisioning MCP controllato**.

Canale dedicato con identità per istanza/sessione, riconnessione e rotazione dopo rollback. Nessuna cartella condivisa usata come scorciatoia. API file e comandi operano nel guest; trasferimenti host/guest avvengono solo attraverso artefatti autorizzati e limiti espliciti.

Il guest agent è un componente separato da aggiornare e testare. La presenza di QEMU Guest Agent non equivale automaticamente all'autenticazione e alle policy richieste da LightVM. Shell guest, privilegi amministrativi guest ed esecuzione di programmi devono avere capability distinte.

Catalogo R4a: `vm.guest.exec`, `vm.guest.read_file`, `vm.guest.write_file`, `vm.guest.processes`. Comandi/argomenti tipizzati, working directory guest, timeout, limiti output/file e terminazione dei processi guest avviati; nessun campo viene reinterpretato come percorso o comando host.

R4b importa soltanto formati dichiarati, con limiti di parsing/decompressione, backing chain risolta in un'area di staging controllata, nessun accesso a protocolli remoti o dischi host e normalizzazione in dischi gestiti. Export/import JSON è versionato, privo di segreti e grant attivi; importare una policy non la rende autorizzata senza applicazione esplicita dell'utente. Report JSON/Markdown restano già disponibili in F5.

R4c abilita `vm.create` da template e media approvati con quote di catalogo e risorse; `vm.delete` soltanto per VM eleggibili, entro un permesso distruttivo esplicito, senza cancellare file arbitrari. La VM appena creata non acquisisce automaticamente tutti i permessi di controllo. Restano esclusi dalla baseline multi-VM concorrenti, snapshot RAM/live, migrazione, GPU passthrough, USB passthrough nelle sessioni agente (in uso manuale vedi R6), bundle automatico QEMU e cataloghi di immagini: non sono dipendenze nascoste di v1.0 o R4.

### R6 — Import VMware, snapshot, integrazione host/guest, rete, Windows 11 x64 e ispezione UI

**Import VMware.** L'utente sceglie il file `.vmx` di una VM VMware spenta; LightVM ricava profilo guest, CPU, RAM, firmware e controller del disco e converte lo stato corrente del disco (catena di snapshot inclusa) in un disco QCOW2 gestito. La VM VMware non viene mai modificata. `.vmx` e descrittori VMDK sono input non fidato: chiavi note, limiti di parsing, estensioni e parent solo nella cartella della VM, nessun valore passato a QEMU. VM sospese, bloccate o cifrate sono rifiutate. Controller: PVSCSI, LSI SAS, SATA, IDE e NVMe mappati su dispositivi QEMU equivalenti; per LSI Logic, che QEMU non emula, LightVM abilita offline nella copia convertita il driver di archiviazione del controller di destinazione.

**Snapshot in uso manuale.** Più snapshot con nome per VM, organizzati ad albero, presi a VM spenta (consistenti) o accesa (solo disco, come un'interruzione di corrente in quell'istante; con LightVM Tools i file system del guest vengono prima scaricati su disco), ripristino a VM spenta ed eliminazione di uno qualsiasi senza perdere gli altri. Lo snapshot contiene lo stato del disco, lo stato firmware scrivibile e la configurazione; non contiene la RAM, che con WHPX non può essere salvata. Gli snapshot non sono esposti agli agenti né usati durante una sessione agente; uno snapshot a VM spenta può diventare baseline secondo le regole della sezione 8. L'import VMware può convertire anche gli snapshot della VM, senza i loro stati di memoria.

**Integrazione host/guest.** Cartelle condivise (lettura o lettura/scrittura), clipboard, trascinamento di file nei due sensi e collegamento di dispositivi USB dell'host, tramite un componente guest LightVM Tools installato dall'utente da un supporto generato dall'app. Ogni funzione è disattivata per difetto e abilitata per singola VM. In una sessione agente ogni funzione è una capability da concedere esplicitamente nel grant: ciò che non è concesso viene scollegato prima della sessione, ciò che è concesso viene scollegato alla revoca, alla scadenza e con l'Emergency Stop. Il canale guest non offre percorsi verso host, LAN o Internet; i percorsi condivisi seguono le regole di 6.3 e 7.4.

**Rete.** Oltre a Offline, che resta il profilo predefinito: NAT completo solo in uso manuale; rete su allowlist di endpoint (solo coppie host:porta TCP approvate dall'utente, mappate su indirizzi virtuali del guest, senza instradamento, DNS libero, UDP, LAN o Internet) utilizzabile nelle sessioni agente come capability concessa nel grant. È l'eccezione Custom Network di R1 resa esplicita; R1 resta il traguardo dell'Internet filtrato.

**Windows 11 x64.** Profilo con OVMF Secure Boot e TPM 2.0 emulato (`swtpm` per Windows, stato per VM incluso in baseline e snapshot), senza aggiramento dei requisiti di installazione.

**Ispezione dell'interfaccia guest.** Tramite LightVM Tools, l'albero di accessibilità della finestra in primo piano (UI Automation su Windows, AT-SPI su Linux) e azioni sugli elementi, come capability distinte concesse nel grant; i dati del guest restano non fidati. Il criterio di uscita di F5 sui client reali si esegue con Claude Code e con un modello multimodale aperto tramite un client MCP.

**Criterio di uscita:** VM VMware reali (Windows con PVSCSI, LSI Logic e SATA; Linux) importate e avviate fino al desktop; rete su allowlist provata con i test negativi di 7.4 e su un caso reale (client Windows verso servizi sull'host); Windows 11 x64 installato con Secure Boot e TPM; albero UI di un'applicazione WPF restituito a un client reale; snapshot presi a VM spenta e accesa, ripristinati ed eliminati su un guest Windows e uno Linux; ogni integrazione provata su Windows Vista, Windows 10/11 e Linux; test negativi di 7.4 ripetuti con le integrazioni abilitate; sessione agente rifiutata finché un'integrazione resta collegata.

## 13. Verifica e budget di prestazione

Obiettivi progettuali congelati, da misurare in F0 e a ogni release; non sono risultati ottenuti. Non sono garanzie temporali del sistema operativo né criteri per ridurre i controlli di sicurezza. Sono bloccanti: assenza delle misure, processi orfani, crescita non contabilizzata e violazione dei limiti funzionali/autorizzativi. Uno scostamento dagli obiettivi numerici di prestazione va pubblicato con causa e intervento pianificato, senza cambiare i valori di questa tabella o dichiararli raggiunti.

Riferimento iniziale: host fisico con almeno 4 core/8 thread, 16 GiB RAM e SSD, guest Linux con 2 vCPU/4 GiB, display 1280×720, almeno 100 GiB liberi prima di installazione e baseline. F0 registra CPU/OS/storage e capacità reali del laboratorio. La configurazione è una base di misura, non il requisito minimo di ogni futuro host.

| Misura | Obiettivo preliminare | Metodo |
| --- | --- | --- |
| CPU manager in idle | Media <1% di un core logico | 5 minuti, nessuna VM/sessione, metriche normalizzate per host |
| Memoria manager | <=250 MiB sul riferimento iniziale | Somma dei processi UI e supervisor; QEMU/LLM esclusi e riportati separatamente |
| Avvio UI | p95 <=3 s | 20 avvii su SSD, distinguendo freddo/caldo |
| Screenshot guest | p95 <=500 ms a 1280×720 | Pipeline completa, guest locale sul riferimento |
| Revoca | p95 <=250 ms | Dall'azione locale al blocco di nuovi inoltri nel supervisor; non annulla input già consegnati |
| Emergency Stop | p95 <=2 s per fine processo QEMU | Su carico e hardware dichiarati; timeout/fallimenti visibili |
| Processi orfani | Zero | Cicli normali, crash app, crash supervisor e timeout |
| Spazio temporaneo | Nessuna crescita non contabilizzata | Serie di sessioni e pulizia dopo riavvio |

Non fissare una dimensione installer unica includendo indifferentemente QEMU e firmware: riportare componenti inclusi, download necessari e spazio VM separatamente. Backend di rendering diversi per OS richiedono confronti per OS, senza nascondere i processi figli.

Per latenze screenshot/revoca/stop usare almeno 100 campioni e riportare p50/p95/massimo, timeout ed errori; niente esclusione dei fallimenti dal report. Gli avvii UI restano un campione di 20 per classe freddo/caldo. Misure memoria distinguono working set/private bytes o metriche equivalenti per OS: non confrontare numeri definiti diversamente sotto la stessa etichetta.

Pipeline di verifica:

- A ogni modifica: unit test significativi, validazione schema, lint e build dei target configurati.
- Test di proprietà sulle transizioni VM e sul registro storage; input avversari e fuzzing mirato dei parser esposti, evitando test che ripetano soltanto l'implementazione.
- Su runner fisici controllati: probe, boot accelerato, QMP, console e ripristino.
- Prima di ogni release: installazione pulita, upgrade, regressione E2E e test negativi sull'artefatto finale.
- Prima di abilitare un host: suite reale su quell'host, non soltanto cross-compilazione.

Runner che eseguono guest e immagini non fidati non devono contenere credenziali di produzione. Le PR non fidate non accedono automaticamente al laboratorio con segreti. Ogni verifica registra comando/procedura, ambiente, risultato e limiti in `docs/verification/`.

Il framework di test UI è scelto e provato per ogni target: per egui il candidato è egui_kittest, che pilota l'interfaccia attraverso l'albero AccessKit e produce istantanee confrontabili, affiancato dai test headless del core. Un test della UI con un supervisor simulato non verifica IPC, rendering nativo o accelerazione: i percorsi reali sono coperti dalle prove sui runner fisici. Eventuali ganci di automazione esistono solo negli artefatti di test.

## 14. Rischi principali e risposta prevista

| Rischio | Probabilità iniziale | Impatto | Risposta e responsabile |
| --- | --- | --- | --- |
| WHPX/configurazione QEMU incompatibile | Media | Blocca il primo host | Probe F0; owner sistemi; target Linux se manca un percorso verificabile |
| Client RFB interno o canale display non verificabile sul primo host | Media | Blocca la console | Spike F0 con ripiego screendump; owner desktop |
| Mappatura tastiera scancode/qcode e IME incompleti | Media | Blocca uso e automazione | Layout dichiarati, test F2 italiano/inglese; owner desktop |
| Rendering eframe assente su host senza accelerazione grafica | Bassa | Blocca la UI su alcuni host | Fallback software verificato in F0; limite dichiarato in COMPATIBILITY |
| Corruzione o perdita di baseline | Media | Critico | Stato offline, journal, lock e crash test; owner storage |
| Accesso host tramite API troppo generiche | Media | Critico | ID e template, nessun passthrough, review indipendente; owner sicurezza |
| Falso isolamento di rete | Alta se si usa solo NAT | Critico | Offline fino a R1 verificata; owner sistemi/rete |
| Divergenza dei client MCP | Media | Alto | Versioni fissate e due client reali; owner integrazione |
| Limiti di risorse inefficaci | Media | Alto | Contabilità totale e enforcement OS; blocco capacità assenti |
| Espansione multipiattaforma prematura | Alta | Perimetro non certificabile | Matrice e certificazione per combinazione; owner prodotto |
| Esiti AI non attendibili | Alta | Report inutilizzabili | Criteri predefiniti, prove e stato inconcludente; owner test |
| Dipendenze vulnerabili o non redistribuibili | Media | Alto | Inventario, aggiornamenti e review distribuzione; owner release |
| Hardware, firma o revisione indipendente indisponibili | Media | Blocca prove/pubblicazione | Verificare la disponibilità in F0; non considerare il silenzio una verifica superata |

## 15. Ordine di esecuzione di F0

| Passo | Lavoro | Evidenza prodotta |
| --- | --- | --- |
| 1 | Congelare scenario, minacce, target e dipendenze; verificare toolchain | ADR iniziali, matrice e build minima |
| 2 | QEMU reale su Windows, probe WHPX, QMP e stop | Log di boot e configurazione riproducibile |
| 3 | Console nativa con client RFB, input e screenshot sul canale protetto | Registrazione della prova e limiti osservati |
| 4 | Baseline/overlay/firmware e supervisione processi | Ripristino verificato, prove di interruzione |
| 5 | Smoke test Linux/KVM e controlli risorse | Confronto tra i due host |
| 6 | Riesame delle prove | Decisione di prosecuzione e backlog F1 |

Non si avvia una costruzione estesa della dashboard prima di conoscere l'esito di QEMU, console e isolamento sul primo host.

## 16. Regole da riportare nel prompt operativo

Per trasformare il testo originale in istruzioni eseguibili:

1. Chiedere inizialmente di completare **F0**, non di implementare tutta la prima macrofase del prompt.
2. Allegare il perimetro v0.1/v0.2 e la matrice di compatibilità di questo documento.
3. Richiedere sicurezza e diagnostica dal primo percorso di avvio.
4. Escludere esplicitamente runtime LLM, rete Internet, guest macOS e snapshot live dal MVP.
5. Ammettere mock e fixture soltanto nei test, mantenendo il boot reale come requisito di accettazione.
6. Separare stato implementato, verificato in simulazione/test, verificato su hardware e non verificato.
7. Per ogni fase richiedere prove, problemi aperti, limiti e decisione di prosecuzione.
8. Non accettare una dichiarazione di completamento se manca un criterio di uscita obbligatorio.
9. Non modificare `ROADMAP.md` per aggiornare stato o versioni: usare gli artefatti operativi definiti nella sezione 17.

**Prima consegna di sviluppo prevista:** fattibilità Windows/Linux documentata, architecture/threat model, decisione su console e canali privati, prova reale di boot/controllo/ripristino e backlog F1. Da questa base può partire una realizzazione controllabile e verificabile.

## 17. Congelamento, decisioni residue e registrazione delle prove

Questa baseline non viene modificata durante l'esecuzione. I requisiti di sicurezza, le esclusioni, i criteri di uscita e le condizioni dei rami alternativi restano invariati. Un limite tecnico può bloccare un traguardo; non autorizza a cancellarlo, simularlo o trasformarlo retroattivamente in requisito facoltativo.

| Artefatto operativo | Contenuto aggiornabile | Vincolo |
| --- | --- | --- |
| `docs/STATUS.md` | Fase corrente, impedimenti, avanzamento | Non modifica la Definition of Done |
| `docs/RELEASE_PLAN.md` | Backlog, responsabili e ordine di esecuzione | Non amplia o riduce il perimetro congelato |
| `docs/COMPATIBILITY.md` | Versioni e combinazioni effettivamente provate | Non dichiara supporto senza evidenze |
| `docs/adr/` | Scelte tecniche lasciate aperte, motivazioni e alternative | Nessuna deroga implicita ai controlli obbligatori |
| `docs/verification/` | Procedure, risultati, benchmark e manifest degli artefatti | Separa chiaramente verificato, fallito e non eseguito |
| `docs/SECURITY.md` | Implementazione dei controlli e rischi residui aggiornati | Non attenua il threat model per far passare i test |

Decisioni da chiudere in F0 entro le alternative già previste: build QEMU e fonte; release guest e firmware; canale QMP privato; trasporto console protetto e suo ripiego; versioni di egui/eframe e backend di rendering; modello di processo del supervisor e meccanismo OS di terminazione dei figli; esito del laboratorio Linux; limiti OS e budget storage; hardware di riferimento; definizione della fixture di test; supporto dati approvato; responsabilità di firma/review. Chiudibili entro F1 se non bloccano F0: SDK/client MCP compatibili con 2025-11-25; framework di test UI; dipendenze/licenze. Registrare soluzione, proprietario, prova e conseguenza in caso di fallimento. Sono dati di implementazione, non un rinvio della definizione del prodotto.

Nessuna verifica non eseguita viene sostituita da un'affermazione di compatibilità. Una vulnerabilità sopravvenuta richiede aggiornamento delle dipendenze, nuova verifica o disabilitazione della capacità interessata secondo questi stessi criteri. Non si congela una versione vulnerabile per rispettare il testo.

Questo documento non costituisce collaudo del software: QEMU, console, isolamento e performance devono ancora essere dimostrati in F0–F6. `docs/ROADMAP.sha256` identifica i byte congelati di questo documento; è un controllo di integrità, non una firma digitale né un blocco tecnico delle modifiche.
