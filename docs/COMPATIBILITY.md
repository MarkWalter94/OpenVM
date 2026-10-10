# Compatibilità

Ultimo aggiornamento: 2026-10-09

Documento operativo previsto dalla sezione 17 di `docs/ROADMAP.md`: versioni e combinazioni **effettivamente provate**. **Non dichiara supporto senza evidenze.** Le combinazioni e gli impegni di release sono quelli della sezione 5; qui si registra solo l'esito delle prove.

Stato attuale: F0 in riesame (`docs/verification/F0-09-riesame-prove.md`); lavoro F1 avviato in parallelo, che non dichiara criteri di uscita prima della chiusura di F0. Una sola combinazione ha prove registrate: Windows 11 x86_64 → Linux x86_64 con WHPX, stato **sperimentale** (prove di fattibilità F0 superate su spike, con deviazioni; nessuna capacità di prodotto implementata). Tutte le altre celle sono "non verificato". L'esito "verificato solo Windows" è **proposto e in attesa della conferma dell'utente** (sezione "Esito di F0 sui target").

Due vocabolari distinti: le **celle della matrice** usano gli stati della sezione 5 (legenda sotto); le tabelle di **componenti e ambienti di prova** usano gli esiti dei file di verifica (`verificato`, `fallito`, `non eseguito`) più `non verificato` per ciò che non ha alcuna prova e `sperimentale` per prove positive parziali con limiti noti. Un esito "verificato" su un componente non promuove alcuna cella della matrice.

## Legenda degli stati

Stati ammessi (sezione 5):

| Stato | Significato |
| --- | --- |
| non verificato | Nessuna prova registrata, oppure prova non eseguita. Stato iniziale di ogni cella. |
| sperimentale | Prova registrata con esito positivo parziale; limiti noti; non pubblicizzabile come supportato. |
| supportato | Tutte le prove richieste per quella combinazione e modalità superate e archiviate in `docs/verification/`. |
| bloccato | Prova eseguita e fallita su un requisito obbligatorio, senza alternativa ammessa; motivo registrato. |

"Certificazione" indica una qualificazione interna basata sulle prove pubblicate, non una certificazione esterna (sezione 5). Una cella si aggiorna solo con un rimando a un file di verifica. Nessuna cella si considera supportata per analogia con un'altra (sezione 12, R2d).

## Matrice delle combinazioni

Colonne richieste dalla sezione 5. Il tipo di macchina va registrato in forma versionata (per esempio `pc-q35-<versione>`).

| Combinazione (sez. 5) | Stato | OS / build host | CPU | Binario e versione QEMU | Acceleratore | Tipo macchina | Firmware | Guest | Console | Snapshot | Controlli di sicurezza | Limiti | Ultimo test superato |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Windows 11 x86_64 → Linux x86_64 (WHPX) | sperimentale | Windows 11 Pro 10.0.26200 (build 26200); Hyper-V e VBS/HVCI attivi | Intel Core i7-12800HX, 16 core / 24 processori logici (F0); Intel Core i9-14900K, 32 processori logici, stessa build di Windows (F1-03, F1-09) | QEMU 11.1.0 (qemu.weilnetz.de, estrazione portabile), `qemu-system-x86_64.exe` SHA-256 `47d57a60…d0b3cd` (ADR-0010) | WHPX (`query-accelerators` → `enabled: whpx`); nessun TCG | `pc-q35-11.1`; CPU `qemu64`, 2 vCPU, 4 GiB | OVMF `edk2-x86_64-code.fd` (SHA-256 `33090cc0…6dbb2a`) in pflash sola lettura + copia per esecuzione di `edk2-i386-vars.fd` (SHA-256 `5d2ac383…be5d1e`); Secure Boot disattivo | Ubuntu 26.04.1 LTS desktop amd64, SHA-256 `601e30fb…1bda1f`, sessione live | RFB su socket Unix privato (client interno, sola lettura); screenshot QMP p95 19,2 ms a 1280×800 | baseline a VM spenta con overlay e vars per esecuzione: 5 cicli con base invariata (fattibilità) | QMP e VNC su socket Unix con ACL solo-utente; rifiuto a integrità bassa; nessuna NIC; Job Object; vedi `SECURITY.md` | `-cpu host` non avvia OVMF; boot solo da ISO live (installazione su disco non provata); arresto ACPI non concluso dal live; lock immagini QEMU inefficace (esclusione nel supervisor); firma Authenticode QEMU scaduta; arresto VeNCrypt fallito; secondo utente e tastiera fisica non provati; alla perdita della UI la VM non è arrestata in modo ordinato entro il timeout del supervisor (`F1-09`) | 2026-10-10: `F1-03` (20 cicli start/stop reali, VM nel firmware), `F1-09` (UI → supervisor, crash della UI); 2026-10-09: `F0-02`, `F0-04`, `F0-05`, `F0-06` (prove di fattibilità, non di prodotto) |
| Ubuntu LTS x86_64 → Linux x86_64 (KVM) | non verificato | non verificato | non verificato | non verificato | non verificato | non verificato | non verificato | non verificato | non verificato | non verificato | non verificato | nessun host Linux fisico in F0; prove WSL2 non valide per questa cella (`F0-07`) | nessuno |
| Debian/Fedora x86_64 → Linux x86_64 (KVM) | non verificato | non verificato | non verificato | non verificato | non verificato | non verificato | non verificato | non verificato | non verificato | non verificato | non verificato | non verificato | nessuno |
| macOS Apple Silicon → Linux ARM64 (HVF) | non verificato | non verificato | non verificato | non verificato | non verificato | non verificato | non verificato | non verificato | non verificato | non verificato | non verificato | non verificato | nessuno |
| macOS Intel → Linux x86_64 (HVF) | non verificato | non verificato | non verificato | non verificato | non verificato | non verificato | non verificato | non verificato | non verificato | non verificato | non verificato | non verificato | nessuno |
| Windows/Linux x86_64 → Windows x86_64 (WHPX/KVM) | non verificato | non verificato | non verificato | non verificato | non verificato | non verificato | non verificato | non verificato | non verificato | non verificato | non verificato | non verificato | nessuno |
| Host ARM64 → guest x86_64 (TCG) | non verificato | non verificato | non verificato | non verificato | non verificato | non verificato | non verificato | non verificato | non verificato | non verificato | non verificato | non verificato | nessuno |
| Hardware Apple compatibile → macOS | non verificato | non verificato | non verificato | non verificato | non verificato | non verificato | non verificato | non verificato | non verificato | non verificato | non verificato | non verificato | nessuno |
| Windows ARM64 → guest ARM64 | non verificato | non verificato | non verificato | non verificato | non verificato | non verificato | non verificato | non verificato | non verificato | non verificato | non verificato | non verificato | nessuno |

Note:

- La colonna "Console" indica la modalità effettiva: RFB su canale privato, RFB con VeNCrypt X.509, oppure ripiego screendump (ADR-0003, ADR-0013).
- La colonna "Snapshot" indica il modello effettivo: nell'MVP soltanto baseline a VM spenta con overlay (ADR-0005).
- La colonna "Controlli di sicurezza" rimanda alla tabella dei controlli in `docs/SECURITY.md` con lo stato per quell'host.
- La riga TCG resta fuori dal profilo AI in ogni caso (ADR-0007).
- La riga Ubuntu resta "non verificato" se F0 si chiude con esito "verificato solo Windows" (sezione 10 F0; ADR-0016). WSL2, VM annidate e runner senza accelerazione verificabile non la aggiornano.
- Lo stato "sperimentale" della riga Windows riflette prove di fattibilità F0 superate con limiti noti; non è pubblicizzabile come supportato e non implica alcuna modalità della matrice seguente.

## Matrice delle modalità

Le modalità sono tracciate separatamente: il supporto manuale di un host non implica le modalità AI (sezione 5).

| Combinazione | Manuale | Baseline | AI offline | Internet filtrato | Guest agent |
| --- | --- | --- | --- | --- | --- |
| Windows 11 x86_64 → Linux x86_64 (WHPX) | non verificato | non verificato | non verificato | non verificato | non verificato |
| Ubuntu LTS x86_64 → Linux x86_64 (KVM) | non verificato | non verificato | non verificato | non verificato | non verificato |
| Debian/Fedora x86_64 → Linux x86_64 (KVM) | non verificato | non verificato | non verificato | non verificato | non verificato |
| macOS Apple Silicon → Linux ARM64 (HVF) | non verificato | non verificato | non verificato | non verificato | non verificato |
| macOS Intel → Linux x86_64 (HVF) | non verificato | non verificato | non verificato | non verificato | non verificato |
| Windows/Linux x86_64 → Windows x86_64 (WHPX/KVM) | non verificato | non verificato | non verificato | non verificato | non verificato |

Internet filtrato appartiene a R1 e guest agent a R4: le relative celle restano "non verificato" per tutto l'MVP.

Per Windows 11 → Linux le modalità restano "non verificato" perché nessuna è implementata nel prodotto. Le prove di fattibilità F0 coprono soltanto i presupposti tecnici: gestione manuale (boot, console, input: `F0-02`, `F0-04`) e baseline (`F0-05`); per AI offline mancano sessioni, bridge e test negativi (F4–F5).

## Rendering della UI per host

Richiesto dal rischio "rendering eframe assente su host senza accelerazione grafica" (sezione 14; ADR-0014).

| Host | Backend (wgpu/glow) | Versione egui/eframe | Sessione locale | Sessione remota (RDP o equivalente) | Fallback software | Stato | Evidenza |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Windows 11 x86_64 | glow predefinito (OpenGL 3.3); wgpu DX12 selezionabile; WARP via wgpu come modalità degradata (ADR-0014) | egui/eframe 0.36.2 | verificato con glow, wgpu Vulkan/DX12/GL (GPU NVIDIA) | non eseguito (limite dichiarato) | wgpu su WARP verificato: CPU circa 7,9 processori logici con animazione a 10 Hz, circa 5,5 aggiornamenti/s; glow senza accelerazione fallisce (`egui_glow requires opengl 2.0+`) | sperimentale | `F0-01` §5–§6; `F0-04` Risultati §3, §7 |
| Ubuntu LTS x86_64 | non verificato | non verificato | non verificato | non verificato | non verificato | non verificato | solo WSL2/WSLg con llvmpipe (`F0-07` §3), non valido per questa riga; percorso X11 in panico per `libxkbcommon-x11.so` mancante; librerie da dichiarare nel pacchetto in `F1-07` §6; smoke software in CI definito ma non eseguito (`F1-08`) |

## Console: modalità e latenza

Se si usa il ripiego screendump, la latenza di aggiornamento va misurata e dichiarata qui (sezione 6.4), con il metodo della sezione 13.

| Host | Modalità console | Campioni | p50 | p95 | Massimo | Timeout / errori | Stato | Evidenza |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Windows 11 x86_64 | RFB su socket Unix privato (primario); ripiego screendump misurato ma non necessario | cattura screendump end-to-end: 120 (guest fermo) / 100 (animazione) | 17,6 / 16,4 ms | 19,2 / 19,3 ms | 20,5 / 24,3 ms | 0 / 0 | sperimentale | `F0-04` Risultati §7 (misura a 1280×800; consegna via IPC del ripiego non ancora esistente) |
| Ubuntu LTS x86_64 | non verificato | — | — | — | — | — | non verificato | — |

## Componenti esterni provati

| Componente | Versione | Provenienza | SHA-256 | Host | Stato | Evidenza |
| --- | --- | --- | --- | --- | --- | --- |
| QEMU (`qemu-system-x86_64`, `qemu-img`, `qemu-io`) | 11.1.0 (`v11.1.0-12130-ge470268ff4`) | `https://qemu.weilnetz.de/w64/qemu-w64-setup-20260811.exe` (SHA-512 pubblicato verificato; SHA-256 coincidente con il manifest winget); firma Authenticode scaduta | `qemu-system-x86_64.exe` `47d57a6072e0bb3bd98f87926eb129eb1736dfe818c67b3b81ef7ce4edd0b3cd`; `qemu-img.exe` `9d930582d0d574bb106d89982c547688e10b8f356192d302cc3d0ae4e8674534`; `qemu-io.exe` `2b4bce81239a3982d3e274a1409251a8c67b26876ff07e5ce921017e2d42da9a` | Windows 11 | sperimentale | `F0-02` §2–§3; ADR-0010 |
| Firmware UEFI (OVMF/edk2, dalla build QEMU) | `edk2-x86_64-code.fd`; template `edk2-i386-vars.fd` | `share/` della build QEMU 11.1.0 | codice `33090cc07675baa5190d9f1e84bf5176b33bcbfa9bacac522961150cdb6dbb2a`; vars `5d2ac383371b408398accee7ec27c8c09ea5b74a0de0ceea6513388b15be5d1e` | Windows 11 | sperimentale | `F0-02` §2.3, §9; `F0-05` §6; ADR-0011 |
| Guest Ubuntu LTS (ISO) | 26.04.1 LTS desktop amd64 | `releases.ubuntu.com/26.04.1/`, `SHA256SUMS` firmato dalla chiave `8439 38DF 228D 22F7 B374 2BC0 D94A A3F0 EFE2 1092` | `601e30fbf5d97759367c632e2c33630665039b7e2158fd068403da3ccf1bda1f` | Windows 11 (boot live sotto WHPX) | sperimentale | `F0-03`; `F0-02` §9; ADR-0011 |

## SDK MCP e client MCP provati

Configurazioni di esempio solo per client realmente provati, con versione e data (sezione 9; ADR-0021). **Nessun client MCP reale provato.**

SDK scelto (ADR-0021): `rmcp` 3.5.1, trasporto stdio, negoziazione limitata alla revisione 2025-11-25. La prova usa un client JSON-RPC scritto per il test e tool finti: dimostra il comportamento del server, non l'interoperabilità con i client.

| Prova | Revisione MCP | OS host | Esito | Evidenza |
| --- | --- | --- | --- | --- |
| Server stdio `rmcp` 3.5.1: `initialize` → `tools/list` → `tools/call`, risultati strutturati, errori operativi e di protocollo distinti, versioni diverse dalla baseline non accettate | 2025-11-25 | Windows 11 Pro 10.0.26200 | verificato (client di prova, tool finti) | `F1-06` |
| Stessa prova | 2025-11-25 | Linux in WSL2 (**[build]**) | verificato (client di prova, tool finti) | `F1-06` |
| Contenuto immagine nei risultati | 2025-11-25 | — | non eseguito | — |

| Client | Versione | OS host | Versione SDK LightVM | Data verifica | Esito | Evidenza |
| --- | --- | --- | --- | --- | --- | --- |
| — | — | — | `rmcp` 3.5.1 | — | non verificato | — (F5) |

## Ambienti di build e test (fuori matrice)

Ambienti senza acceleratore verificabile: servono a build e test indipendenti dall'acceleratore e **non aggiornano alcuna cella della matrice** (sezione 10 F0; `docs/TESTING.md` §6).

| Ambiente | Uso | Esito | Evidenza |
| --- | --- | --- | --- |
| Windows 11 x86_64, Build Tools 2022 con ambiente MSVC esplicito, Rust 1.99.0 | build e test del workspace | verificato (con ambiente MSVC caricato; senza, `LNK1104`) | `F0-01` §4.1; `F1-04` |
| Windows 11 x86_64, Visual Studio Community 2022 (MSVC 14.44, componente x64 aggiunto), Rust 1.99.0 | build, clippy e test del workspace; prove reali con WHPX | verificato (prima dell'aggiunta del componente: `LNK1104 msvcrt.lib`) | `F1-03` |
| Linux in WSL2 (Ubuntu, kernel 6.18, linker zig) | build e test; processi (pidfd, gruppi, cgroup delegato) | verificato **[build]**/**[processi]** | `F0-07`; `F1-04`; `F1-06`; `F1-08` §3 |
| Runner GitHub `windows-latest`, `ubuntu-latest` | CI F1 (`.github/workflows/ci.yml`) | non eseguito (repository non pubblicato) | `F1-08` |
| Runner GitHub `macos-latest` | solo `cargo check`, nessun supporto dichiarato | non eseguito | `F1-08` |
| Ubuntu LTS fisico | smoke rendering e librerie `dlopen` del pacchetto Linux | non verificato | `F1-07` §6 (elenco derivato dalle feature, non provato a runtime) |

## Esito di F0 sui target

**Proposto: "verificato solo Windows"**, in attesa della conferma dell'utente (ADR-0016; `docs/verification/F0-09-riesame-prove.md` §6). Con questo esito Windows è l'unico target del MVP, la riga Ubuntu resta "non verificato" e la qualificazione Linux è rinviata a R2a. Valori ammessi (sezione 10 F0): Windows e Linux verificati; verificato solo Windows; Linux primo target; bloccato.
