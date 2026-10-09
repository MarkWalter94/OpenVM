# F0-04 — Console nativa con client RFB, input e screenshot sul canale protetto

- Data di esecuzione (UTC): 2026-10-09, dalle 11:11 alle 12:34
- Esecutore: agente di sviluppo (sessione Claude Code), su richiesta del responsabile del progetto
- Passo della roadmap: sezione 15, passo 3; sezione 10 (F0, "console nativa con client RFB sul canale protetto o il suo ripiego, input, screenshot"); sezioni 6.4, 7.2 (punti 3 e 6), 7.4 (righe "IPC/console", "Console", "Host"), 13
- ADR collegati: ADR-0003, ADR-0013, ADR-0014 (non modificati)
- Esito complessivo: **parziale** — console RFB, input QMP e screenshot verificati sul guest reale con trasporto socket Unix; trasporto VeNCrypt/TLS verificato con un limite sull'arresto; input da tastiera fisica attraverso winit non eseguito; prova con un secondo utente non eseguita.

Esiti ammessi: **verificato**, **fallito**, **non eseguito** (sezione 17). Log grezzi: `lab/logs/f0-04-evidence.txt`, `lab/logs/f0-04-evidence-2.txt`, `lab/logs/f0-04-verifiche-finali.txt`, `lab/logs/f0-04-dipendenze-ricerca.txt`, `lab/logs/f0-04-ui-*.stdout.txt`. Immagini in `lab/vms/f0-04/`.

## Scopo

Dimostrare su questo host il percorso console della sezione 6.4: client RFB in Rust, eseguito nel processo della UI, in sola lettura, su un canale protetto; framebuffer disegnato come texture egui; tastiera e mouse inoltrati **solo** via QMP; screenshot via QMP `screendump`; misure della sezione 13 (latenza screenshot, frequenza di aggiornamento, CPU e memoria della UI).

Non dimostra: comportamento su Linux, in RDP o su altri host; credenziale del display emessa dal supervisor per esecuzione (il supervisor non esiste ancora); revoca di sessione; isolamento rispetto a un secondo utente locale; percorso scancode fisico → winit → egui con una tastiera reale.

## Ambiente

| Voce | Valore |
| --- | --- |
| Host | HP ZBook Fury 16 G9, Intel Core i7-12800HX (24 processori logici), 31,6 GiB RAM, volume E: NTFS (come F0-01/F0-02) |
| OS e build | Windows 11 Pro 10.0.26200 |
| Acceleratore | WHPX; `query-accelerators` → `{"enabled":"whpx","present":["qtest","tcg","whpx"]}`; nessun fallback TCG configurato |
| Toolchain | rustc 1.99.0 (b940084d7 2026-09-28), cargo 1.99.0, target MSVC con ambiente di Build Tools 2022 caricato da un wrapper `vcvars64.bat` nella scratchpad della sessione (stesso metodo di F0-01, nessuna modifica al sistema) |
| QEMU | 11.1.0 portabile, `lab/qemu/qemu-11.1.0/qemu-system-x86_64.exe`, SHA-256 `47d57a6072e0bb3bd98f87926eb129eb1736dfe818c67b3b81ef7ce4edd0b3cd` (ricalcolato, coincide con F0-02) |
| Firmware | `edk2-x86_64-code.fd` SHA-256 `33090cc0…6dbb2a` (pflash sola lettura); copia per esecuzione di `edk2-i386-vars.fd` SHA-256 `5d2ac383…be5d1e` in `lab/vms/f0-04/priv/` |
| Guest | `ubuntu-26.04.1-desktop-amd64.iso`, SHA-256 `601e30fb…bda1f` (ricalcolato), file `.verified` presente; sessione live, nessuna installazione su disco |
| Configurazione QEMU | `-machine q35 -accel whpx -cpu qemu64 -smp 2 -m 4096 -nodefaults -no-user-config -nic none -device virtio-vga -device virtio-tablet-pci -device virtio-keyboard-pci -display none`; VNC e QMP vedi Procedura |
| GPU | NVIDIA RTX A2000 8GB Laptop GPU, driver 32.0.15.9658 |
| Sessione grafica | console locale. **Durante gran parte delle prove la sessione Windows era bloccata**: `LogonUI.exe` in esecuzione e una cattura della finestra dell'app ha restituito la schermata di blocco (cattura eliminata). Sbloccata dall'utente prima delle 12:22 (`LogonUI: 0` nel log). Ogni misura riporta lo stato |
| Altre condizioni | altri agenti eseguivano in parallelo propri QEMU (es. `-name lightvm-f0-06-c-desktop-21`) e compilazioni nello stesso `target/` |

### Crate aggiunti (versioni esatte, nessuna sorgente git)

Comando: `cargo tree -p rfb-client -e normal`, `cargo tree -p f0-console -e normal --depth 1`; `grep -c 'source = "git' Cargo.lock` → `0`.

| Crate | Versione | Licenza | Usato da | Motivo |
| --- | --- | --- | --- | --- |
| rustls | 0.23.45 (`default-features = false`, `ring`, `std`, `tls12`) | Apache-2.0 OR ISC OR MIT | rfb-client | VeNCrypt X.509 con certificato client |
| ring | 0.17.14 | Apache-2.0 AND ISC | rfb-client (via rustls) | provider crittografico; aws-lc-rs richiederebbe CMake/NASM |
| rustls-pki-types | 1.15.1 | MIT OR Apache-2.0 | rfb-client (via rustls) | lettura PEM |
| rustls-webpki | 0.103.15 | ISC | via rustls | verifica certificati |
| socket2 | 0.6.5 (`all`) | MIT OR Apache-2.0 | rfb-client, f0-console | AF_UNIX su Windows, scelto |
| uds_windows | 1.2.1 | MIT | rfb-client, f0-console | AF_UNIX alternativo, provato |
| flate2 | 1.1.10 (`rust_backend`, miniz_oxide 0.9.1) | MIT OR Apache-2.0 | rfb-client | flusso zlib di ZRLE |
| png | 0.18.1 | MIT OR Apache-2.0 | f0-console | PNG in memoria |
| serde_json | 1.0.151 | MIT OR Apache-2.0 | f0-console | QMP |

Transitive nuove nel lockfile: `zeroize 1.9.1`, `untrusted 0.9.0`, `subtle 2.6.1` (BSD-3-Clause), `getrandom 0.2.17`, `memoffset 0.9.1`, `tempfile 3.27.0` (via uds_windows). Il `Cargo.lock` di radice è stato aggiornato da cargo per queste dipendenze; il `Cargo.toml` di radice non è stato modificato (dipendenze dichiarate nei manifest dei crate).

## Codice prodotto

| Percorso | Contenuto |
| --- | --- |
| `crates/rfb-client/` | libreria `#![forbid(unsafe_code)]`: handshake 3.8, sicurezza, ServerInit, SetPixelFormat BGRX, SetEncodings (ZRLE, Hextile, CopyRect, Raw, DesktopSize −223), ciclo di FramebufferUpdateRequest incrementali con limite di frequenza, ridimensionamento, `Session` su thread con callback e framebuffer condiviso (`Arc<Mutex<Framebuffer>>`) |
| `crates/rfb-client/tests/sola_lettura.rs` | test negativo sulla sola lettura |
| `spikes/f0-console/` | libreria `f0_console` (QMP minimo tipizzato, PPM→PNG, digitazione US, percentili) e binario `f0-console` (sonde, misure, azioni QMP) |
| `apps/desktop/src/console.rs`, `keymap.rs` | console reale nella UI: texture egui aggiornata per rettangoli (`TextureHandle::set_partial`), stato connessione/geometria, input → QMP, pulsante Screenshot |
| `apps/desktop/src/main.rs` | opzioni `--vnc-socket`, `--vnc-impl`, `--vnc-tls`/`--tls-dir`, `--qmp-socket`, `--capture-dir`, `--tmp-dir`, `--layout us|it`, `--max-fps`, `--dump-fb` |
| `apps/desktop/tests/kittest_ui.rs`, `console_reale.rs` | test UI headless; prova d'integrazione contro QEMU reale (ignorata per default) |
| `lab/scripts/f0-04-boot.ps1`, `f0-04-stop.ps1`, `f0-04-measure-ui.ps1` | avvio/arresto QEMU (derivato da `boot-iso.ps1`), misura CPU/memoria della UI |

## Procedura

1. Avvio QEMU modalità socket Unix: `powershell -File lab/scripts/f0-04-boot.ps1 -Mode unix`. Lo script crea `lab/vms/f0-04/priv` con `icacls priv /inheritance:r /grant:r "<utente>:(OI)(CI)F"`, copia le variabili UEFI, avvia QEMU con
   `-vnc unix:E:/OpenVM/lab/vms/f0-04/priv/vnc.sock -qmp unix:E:/OpenVM/lab/vms/f0-04/priv/qmp.sock,server=on,wait=off` e registra il PID.
2. Modalità TLS (secondo trasporto, senza media): `f0-04-boot.ps1 -Mode tls` →
   `-object tls-creds-x509,id=tls0,dir=E:/OpenVM/lab/vms/tls,endpoint=server,verify-peer=on -vnc 127.0.0.1:4,tls-creds=tls0` (certificati di prova di F0-02). Ascolto osservato: `127.0.0.1:5904` soltanto.
3. Sonde e azioni con `target/release/f0-console.exe` (`probe-unix`, `rfb-snapshot`, `rfb-fps`, `rfb-idle`, `session-stop`, `qmp … move|click|key|type|taps|screendump`, `bench-screendump`, `wait-desktop`, `diff`).
4. App: `lightvm-desktop.exe --smoke N --vnc-socket <priv>/vnc.sock --qmp-socket <priv>/qmp.sock --capture-dir lab/vms/f0-04 --tmp-dir <priv> --dump-fb <png>`; build wgpu (`target/release`, eseguita con `WGPU_BACKEND=dx12`) e glow (`--no-default-features --features glow --target-dir target/glow`).
5. Misure UI: `lab/scripts/f0-04-measure-ui.ps1` (avvio con `--smoke 75`, `TotalProcessorTime` a 10 s e 70 s, `WorkingSet64`/`PrivateMemorySize64` ogni 5 s).
6. Arresto: `f0-04-stop.ps1` (QMP `quit`, poi eventuale `Stop-Process`), rimozione dei socket orfani; verifica dei processi con `Win32_Process` filtrando la riga di comando `f0-04`.

## Risultati

### 1. Trasporto socket Unix: socket2 contro uds_windows

| Voce | Esito | Evidenza |
| --- | --- | --- |
| Connessione AF_UNIX con `socket2` (`Domain::UNIX`, `SockAddr::unix`) | verificato | `PROBE impl=socket2 connesso=si byte=12 … prima_riga="RFB 003.008"`; QMP: greeting `{"QMP": {"version": {"qemu": {"micro": 0, "minor": 1, "major": 11} …` |
| Connessione AF_UNIX con `uds_windows` | verificato | `PROBE impl=uds connesso=si byte=12 … "RFB 003.008"`; stesso greeting QMP |
| Handshake RFB completo con entrambe | verificato | `RFB connesso canale=PrivateUnixSocket sicurezza="None su socket Unix privato" handshake_ms=5 server_init=1280x800 bpp=32 depth=24 nome="QEMU (f0-04)"`; PNG identici (33 223 byte) per le due implementazioni |
| Arresto di una sessione bloccata in lettura | verificato dopo correzione (vedi 8) | `SESSION stop_ms=93/94` con entrambe le implementazioni, evento `Disconnected { error: None }` |

Scelta: **socket2** come predefinito. Le due implementazioni si sono comportate in modo identico in tutte le prove (connessione, handshake, stessi limiti su `shutdown` e timeout); socket2 serve anche il TCP di VeNCrypt, ha meno dipendenze (uds_windows trascina `tempfile` e `memoffset`) e una base d'uso più ampia. uds_windows resta selezionabile (`UnixImpl::UdsWindows`, `--vnc-impl uds`) per confronto.

### 2. Client RFB

| Voce | Esito | Evidenza |
| --- | --- | --- |
| Versione 3.8; rifiuto di versioni inferiori | verificato (reale + test) | banner `RFB 003.008`; test `versione_precedente_alla_3_8_rifiutata` |
| `None` accettato solo su socket Unix | verificato | handshake reale su socket Unix; su TCP il client richiede VeNCrypt X509None |
| `None` su TCP in chiaro rifiutato | verificato (test con flusso simulato) | test `none_su_tcp_in_chiaro_rifiutato`: errore `InsecureChannel`, il client ha scritto solo `RFB 003.008\n`. Nessun endpoint QEMU TCP in chiaro è stato avviato (regola del compito) |
| ClientInit condiviso, ServerInit | verificato | `server_init=1280x800 bpp=32 depth=24 nome="QEMU (f0-04)"` |
| SetPixelFormat BGRX, decodifica corretta | verificato | GRUB e desktop resi con colori corretti (`rfb-early-socket2.png`, `desktop-0.png`) |
| Encoding usato da QEMU | verificato | QEMU ha risposto sempre con ZRLE: `rects_by_encoding: [0, 0, 0, 1, 0]`, in animazione `zrle=8361` rettangoli in 30 s. Hextile, CopyRect e Raw sono coperti solo da test unitari (`hextile_sfondo_e_subrect`, `copy_rect_con_sovrapposizione`, `zrle_solido_palette_e_rle_su_due_rettangoli`) |
| DesktopSize | verificato solo con test | `sessione_simulata_scrive_solo_tipi_0_2_3` (ridimensionamento 4×2 → 8×3 e richiesta completa successiva). Nessun cambio di risoluzione del guest è stato provocato sul guest reale |
| Estensioni non annunciate rifiutate | verificato (test) | `encoding_non_annunciato_rifiutato` (Cursor −239 → `Unsupported`) |
| ServerCutText scartato, mai consegnato | verificato (test) | contatore `cut_text_discarded: 1` nel test; nessun codice che scriva negli appunti |
| Integrità dopo aggiornamenti incrementali | verificato | app 45 s durante un'animazione a durata limitata: `aggiornamenti_rfb=204 caricamenti_texture=4514`, poi `DIFF … pixel_diversi=0` tra framebuffer dell'app e `screendump` |
| Aggiornamento dopo 20 s senza traffico | verificato | `IDLE modo=bloccante tasto_a_ms=20000 aggiornamenti_prima=1 pausa_max…=19578 aggiornamenti_dopo=8 primo_dopo_ms=Some(20270)` |

### 3. Console nell'app (punto 4a)

| Voce | Esito | Evidenza |
| --- | --- | --- |
| Framebuffer reale visibile come texture, wgpu DX12 | verificato | `SMOKE console geometria=1280x800 aggiornamenti_rfb=1 caricamenti_texture=2 …`; `SMOKE dump_fb=…app-fb-wgpu.png non_neri=100.00%`; `DIFF … pixel_diversi=0` contro `screendump` |
| Idem, glow | verificato | `renderer=glow … versione_gl="3.3.0 NVIDIA 596.58"`; `DIFF … pixel_diversi=0` |
| Idem, trasporto TLS | verificato | `--vnc-tls 127.0.0.1:5904`: `DIFF … pixel_diversi=0` |
| Aggiornamento solo dei rettangoli cambiati | verificato | `set_partial` per rettangolo; con 908 aggiornamenti: `caricamenti_texture=21166 pixel_caricati=20948752` (circa 23 000 pixel per aggiornamento invece di 1 024 000) |
| Indicatore di stato e geometria | implementato, verificato solo nell'albero AccessKit | testo "RFB: connesso (…)", "Geometria: 1280×800", "Input QMP", "Layout guest"; nessuna cattura dello schermo valida (sessione bloccata) |
| Prima frame / chiusura dell'app | verificato | `primo_frame_ms` 401–872 (wgpu DX12), 250–664 (glow); uscita con codice 0 dopo lo smoke su socket Unix |

Osservazione: il numero di frame è circa il doppio degli aggiornamenti (`frame=2115` per 908 aggiornamenti; `<2ms=872` intervalli nella prima serie). Ogni `request_repaint()` dal thread RFB produce due passaggi UI; il costo è incluso nelle misure di CPU.

### 4. Input e screenshot via QMP (punti 4b, 4c)

| Voce | Esito | Evidenza |
| --- | --- | --- |
| Tastiera: frecce nel menu GRUB | verificato | `key down` → selezione su "Ubuntu (safe graphics)" (`grub-1-dopo-down.png`); `key up` → ritorno |
| Mouse assoluto: clic su un elemento della finestra di benvenuto | verificato | `click 662 326` → selezione e lingua cambiate in "Deutsch", titolo "Willkommen bei Ubuntu" (`mouse-1-dopo-click.png`); `click 658 422` → ritorno a English. `query-mice`: `QEMU Virtio Tablet` `absolute:true current:true` |
| Combinazione Ctrl+Alt+T | verificato | terminale aperto entro 8 s (`kbd-0-terminale.png`) |
| Digitazione layout US | verificato | `echo "LightVM F0-04: tastiera_US ok [1+2=3] ~/path?" \| tr a-z A-Z` → output `LIGHTVM F0-04: TASTIERA_US OK [1+2=3] ~/PATH?` (`kbd-1-testo.png`) |
| Layout IT dichiarato | verificato | guest con `gsettings … sources "[('xkb', 'it')]"`; qcode `bracket_left,apostrophe,semicolon,backslash,less,equal,minus,grave_accent,slash,bracket_right` → `èàòù < ì ' \ - +` (`kbd-2-layout-it.png`), come la tabella `keymap::base_char`. Layout US ripristinato e verificato (`kbd-3-ritorno-us.png`) |
| UI → tabella tasti → QMP → guest (senza winit) | verificato | `cargo test -p lightvm-desktop --test console_reale -- --ignored`: eventi egui con `physical_key`, clic AccessKit sull'immagine per il focus → nel guest `echo UI-kittest` e output `UI-kittest` (`ui-screenshot-001.png`); Shift lasciato premuto e rilasciato dalla UI alla perdita di focus (`eventi_input prima=40 dopo=41`) |
| Pulsante "Screenshot" | verificato (via AccessKit) | `SCREENSHOT screenshot 1280x800 in 448 ms: …ui-screenshot-001.png` |
| Tastiera fisica reale → winit → egui → QMP | **non eseguito** | la sessione era bloccata e un'iniezione `SendInput` avrebbe agito sul desktop dell'utente; nessun evento OS sintetico inviato |
| Puntatore reale → QMP | osservato, effetto non verificato | dopo lo sblocco, il passaggio del puntatore dell'utente sulla finestra ha prodotto `eventi_input=151` e `160` nello smoke; nessuno screendump di controllo |
| Puntatore UI → QMP | verificato solo con test unitario | `puntatore_scalato_e_pulsanti` (x=1279 su 1280 → 32767; pulsante sinistro giù/su) |

Scala degli assi: `scale_abs(px, size) = px * 32767 / (size - 1)`, stessa formula di `qemu_input_scale_axis` usata dal server VNC di QEMU; test `scala_assi_come_qemu`.

#### Tasti coperti (tabella `apps/desktop/src/keymap.rs`, identica per US e IT)

Coperti: A–Z, 0–9, F1–F12, Esc, Tab, Backspace, Invio, Spazio, Ins, Canc, Home, Fine, PgSu, PgGiù, frecce, `- = [ ] \ ; ' \` , . /` (posizioni US; in IT producono `' ì è + ù ò à \ , . -`), tasto ISO `<>` (`less`), Shift/Ctrl/Alt/Super sinistri e destri, `+` del tastierino (`kp_add`). Il test `ogni_tasto_egui_e_mappato_o_dichiarato_non_coperto` verifica che ogni voce di `egui::Key::ALL` sia mappata o dichiarata non coperta.

Non coperti: F13–F35; Bloc Maiusc, Bloc Num, Bloc Scorr, Stamp, Pausa, Menu (egui-winit 0.36.2 non li converte in `egui::Key`); tasti dedicati Copia/Taglia/Incolla e BrowserBack; tastierino distinto (egui-winit fonde Numpad0–9 con le cifre, NumpadEnter con Invio, NumpadSubtract con `-`, NumpadDivide con `/`); caratteri Unicode generali (nessun guest agent, sezione 6.4).

Comportamenti di egui-winit 0.36.2 rilevanti (letti nel sorgente `egui-winit/src/lib.rs`, `on_keyboard_input`): Ctrl+C/X/V non producono `Event::Key` ma `Copy`/`Cut`/`Paste`; la UI ricostruisce la pressione di C/X/V e **scarta** il testo di `Paste` (test `incolla_non_inoltra_il_testo`), ma egui-winit legge comunque gli appunti host alla pressione di Ctrl+V. Su Windows AltGr arriva normalmente come Ctrl sinistro + Alt destro: comportamento non verificato in questa prova.

### 5. Trasporto VeNCrypt X.509 con rustls

| Voce | Esito | Evidenza |
| --- | --- | --- |
| Handshake con certificato client valido | verificato | `sicurezza="VeNCrypt X509None, Some(TLSv1_3) Some(TLS13_AES_256_GCM_SHA384)" handshake_ms=14 server_init=1280x800`; UEFI Shell visibile |
| Certificato client firmato da una CA estranea | verificato (nessun framebuffer) | `ERRORE I/O … (os error 10060) exit=1 tempo_ms=10076`: QEMU non invia SecurityResult né ServerInit; il client esce per il timeout di handshake di 10 s |
| Nome server errato | verificato | `TLS: invalid peer certificate: certificate not valid for name "altro.example"` |
| Indirizzo non di loopback | verificato (lato client) | `indirizzo 192.0.2.1:5904 non di loopback: rifiutato` |
| Aggiornamento incrementale su TLS | verificato | `IDLE … aggiornamenti_prima=0 … aggiornamenti_dopo=1 primo_dopo_ms=Some(11035)` dopo tasti `a,b` nella UEFI Shell |
| Arresto della sessione su TLS | **fallito** | `SESSION stop_ms=2004/2006/2001` e `nessun evento entro 15 s dallo stop`: il thread resta bloccato in lettura e viene abbandonato dopo 2 s |
| Chiusura lato QEMU | anomalia | `Get-NetTCPConnection -LocalPort 5904`: 11 connessioni in `CloseWait` dopo le prove, poi 7, poi 2 circa un minuto dopo. QEMU non chiude subito le connessioni TLS terminate dal client, comprese quelle con certificato rifiutato |

### 6. Test negativi (sezione 7.4)

| Voce | Esito | Evidenza |
| --- | --- | --- |
| Il client non può inviare input, appunti o power control | verificato | `tests/sola_lettura.rs`: `ClientMessage` ha tre varianti con `match` esaustivo (tipi 0, 2, 3); `CLIENT_MESSAGE_TYPES_SENT == [0, 2, 3]`; ricerca nel codice senza commenti di `keyevent`, `key_event`, `pointerevent`, `pointer_event`, `clientcuttext`, `client_cut_text`, `clipboard`, `button_mask`, `keysym`, `qemuextended`, `power` → 0 occorrenze. Test `sessione_simulata_scrive_solo_tipi_0_2_3`: tutti i byte scritti dal client dopo ClientInit sono messaggi 0, 2, 3 |
| Ricerca testuale documentata | verificato | `rg -n -i "keyevent\|key_event\|pointerevent\|pointer_event\|clientcuttext\|client_cut_text\|clipboard\|keysym\|button_mask" crates/rfb-client/src` → una sola riga, un commento (`// Scartato: nessuna clipboard …`); ricerca di `vec![4|5|6`, `push(4|5|6)`, `write_all(&[4|5|6` → nessuna occorrenza |
| Processo a integrità bassa dello stesso utente verso i socket privati | verificato | copia di `f0-console.exe` con `icacls … /setintegritylevel Low` (`whoami /groups` della copia: `Livello obbligatorio basso S-1-16-4096`): `probe-unix` su `vnc.sock` e `qmp.sock` con socket2 e uds_windows, `rfb-snapshot` e `qmp status` → `os error 10013` (WSAEACCES). Controllo a integrità media nello stesso momento: connesso, `QMP status=running`. Primo tentativo in `lab/vms/f0-04/lowil` fallito per `icacls … Accesso negato` (directory senza WRITE_OWNER): quei processi erano a integrità media e non contano |
| Secondo utente senza accesso alla directory privata | **non eseguito** | nessun secondo account disponibile e nessuna elevazione. Serve un account locale non amministratore e l'esecuzione di `f0-console probe-unix` con le sue credenziali (`runas /user:…`) verso `priv\vnc.sock` e `priv\qmp.sock`. ACL osservata: `priv` → solo `OMEGAGROUP\MOrtali:(OI)(CI)(F)`; socket → `MOrtali:(I)(F)` |
| La UI non espone campi che finiscano nella riga di comando QEMU | verificato | `nessun_campo_di_testo_in_alcuna_sezione` e `nessun_campo_di_testo_con_console_reale_configurata`: 0 nodi `TextInput`/`MultilineTextInput`/`SearchInput`/`SpinButton` in Dashboard, Console, Settings; l'unico comando della console è "Screenshot". I percorsi arrivano solo da riga di comando; nessun argomento QEMU è costruito dall'app. Il nome del file di `screendump` è generato dal thread QMP nella directory passata all'avvio |
| Secondo client QMP mentre l'app è collegata | osservato | `secondo client: tempo totale 30078 ms` con `os error 10060`: QEMU serve un solo client QMP alla volta, il secondo non riceve il greeting finché il primo resta collegato |

### 7. Misure (sezione 13)

**Screenshot `screendump` end-to-end** (QMP `screendump` PPM in `priv/` → lettura e analisi PPM → PNG in memoria, compressione `Fast` del crate png), 1280×800:

| Condizione | n | p50 | p95 | max | errori / timeout |
| --- | --- | --- | --- | --- | --- |
| desktop con terminale fermo | 120 | 17,6 ms | 19,2 ms | 20,5 ms | 0 / 0 |
| animazione intensa nel guest | 100 | 16,4 ms | 19,3 ms | 24,3 ms | 0 / 0 |

Fasi (120 campioni): QMP 8,8/10,0/10,8 ms; PPM 3,2/3,7/4,1 ms; PNG 5,5/6,2/6,7 ms (p50/p95/max). Obiettivo sezione 13 (p95 ≤ 500 ms a 1280×720): misurato sotto l'obiettivo, a 1280×800. Timeout: il client QMP usa 30 s di timeout di lettura; nessuno è scattato. Il pulsante della UI ha registrato 419 ms e 448 ms (build debug del test, include scrittura del PNG su disco).

**Frequenza di aggiornamento del framebuffer** (`f0-console rfb-fps`, codice definitivo):

| Animazione nel guest | limite richieste | aggiornamenti/s | intervallo p50 / p95 / max |
| --- | --- | --- | --- |
| nessuna (desktop fermo) | 60/s | 1 aggiornamento in 50,7 s | — |
| `date` ogni 50 ms nel terminale | 60/s | 12,09 | 109,3 / 132,4 / 1146,1 ms |
| idem | 30/s | 12,46 | 109,9 / 132,4 / 142,2 ms |
| `date` senza pausa | 60/s | 32,18 | 30,9 / 32,3 / 45,3 ms |
| idem | 30/s | 26,19 | 31,1 / 111,8 / 114,3 ms |

Il tetto di circa 32 aggiornamenti/s coincide con l'intervallo di 30 ms del server VNC di QEMU (dedotto dalla misura, non verificato nel sorgente QEMU). Con limite 30/s il p95 sale a 112 ms. In una serie la UI ha elaborato 1945 aggiornamenti in 75 s (circa 26/s) con la stessa animazione dichiarata; una misura di controllo app/CLI nelle stesse condizioni ha dato 371 aggiornamenti in 30 s (app) e 12,53/s (CLI): la differenza dipendeva dallo stato del terminale nel guest, non dalla UI.

**CPU e memoria del processo UI** (60 s, console attiva, 1460×860, `f0-04-measure-ui.ps1`):

| Backend | Guest | Sessione host | Aggiornamenti RFB in 75 s | CPU (% di un core) | Working set fine / max | Private bytes fine / max | Picco WS |
| --- | --- | --- | --- | --- | --- | --- | --- |
| wgpu DX12 | animazione | bloccata | 1945 | 9,04 | 190,6 / 192,4 MiB | 369,6 / 372,0 MiB | 198,3 MiB |
| glow | animazione | sbloccata | 908 | 9,91 | 93,8 / 94,5 MiB | 96,4 / 97,6 MiB | 95,8 MiB |
| wgpu DX12 | fermo | bloccata | 2 | 0,05 | 190,7 / 190,8 MiB | 369,2 / 369,4 MiB | 198,2 MiB |
| wgpu DX12 | fermo | sbloccata, puntatore utente sulla finestra (151 eventi) | 7 | 1,58 | 197,7 / 197,7 MiB | 369,8 / 369,9 MiB | 199,6 MiB |
| glow | fermo | sbloccata | 3 | 0,18 | 91,8 / 91,8 MiB | 88,4 / 88,5 MiB | 95,5 MiB |

CPU host (24 processori logici): 0,38 % (wgpu, animazione), 0,41 % (glow, animazione). Le misure glow sono state ripetute con un binario ricompilato dopo la correzione del punto 8; le serie precedenti (glow con il binario non aggiornato e una serie "anim" che in realtà era a guest fermo) restano nei log e non sono usate.

Confronto con il budget della sezione 13 (memoria manager ≤ 250 MiB, UI + supervisor; non modificato): il supervisor non esiste ancora, quindi il confronto copre solo la UI. Con wgpu DX12 il working set (191–198 MiB) è sotto il budget, i private bytes (369–372 MiB) lo superano; con glow entrambe le metriche (88–98 MiB) sono sotto. Le due metriche non vanno confrontate sotto la stessa etichetta (sezione 13).

### 8. Problemi trovati e corretti durante la prova

1. **Arresto della sessione su AF_UNIX Windows.** Misure con `session-stop` e `probe-unix`:
   - `shutdown(Both)` su un duplicato ottenuto con `try_clone` non sblocca una `recv` in corso sull'originale (`SESSION stop BLOCCATO oltre 5 s`, entrambe le implementazioni);
   - `SO_RCVTIMEO` impostato sul duplicato non vale per l'originale (`PROBE lettura ancora bloccata dopo 3 s`); sull'originale funziona (`seconda_lettura … Err(TimedOut) ms=3002`);
   - con timeout brevi sull'originale e letture ripetute, dopo alcuni timeout il flusso **smette di consegnare dati senza errore** (`IDLE modo=interrompibile … aggiornamenti_prima=13 … aggiornamenti_dopo=0`), mentre in modalità bloccante l'aggiornamento arriva. È coerente con l'avvertenza di Winsock sullo stato indeterminato dopo un timeout di `SO_RCVTIMEO`;
   - anche `shutdown` sull'handle originale condiviso non sblocca la `recv` (`stop_ms=2001/2003`).
   Soluzione adottata: un solo handle condiviso (`Arc`) per lettura, scrittura e chiusura; letture bloccanti; timeout di 10 s solo durante l'handshake (se scatta, la connessione viene comunque chiusa); `Session::stop` invia una FramebufferUpdateRequest non incrementale 1×1 (tipo 3, già ammesso) per provocare una risposta del server, poi `shutdown(Write)`, e attende al massimo 2 s. Esito: `stop_ms=93/94` su socket Unix; su TLS non funziona (punto 5).
2. **Prima versione dell'app bloccata alla chiusura** per la stessa causa (processo terminato con `Stop-Process`); risolto dal punto 1.
3. `cargo fmt --all` formatta l'intero workspace: controllato con `find -newer` che abbia modificato solo i file di questo passo.

## Limiti

- Un solo host, sessione locale; nessuna prova in RDP, su Linux, su host senza GPU.
- Gran parte delle prove con la sessione Windows bloccata: il rendering avveniva (frame contati) ma la finestra non era visibile; le misure "bloccata" e "sbloccata" non sono equivalenti.
- Input da tastiera fisica attraverso winit non provato; il percorso UI è stato provato da `egui::Event` in poi.
- Hextile, CopyRect, Raw e DesktopSize non sono mai stati inviati da QEMU in queste prove (QEMU ha scelto ZRLE e la risoluzione del guest non è cambiata): coperti solo da test con dati costruiti.
- Su TLS l'arresto di una sessione abbandona il thread lettore; QEMU lascia connessioni in `CloseWait` per decine di secondi.
- QMP accetta un solo client alla volta: con l'app collegata, il QMP non è disponibile per altri strumenti.
- Ogni `request_repaint` produce due frame UI.
- La credenziale del display per esecuzione e VM, la revoca e il supervisor non esistono ancora: la protezione del canale qui è l'ACL della directory privata (socket Unix) o il certificato client di prova (TLS).
- Il secondo utente locale non è stato provato; la prova con integrità bassa dimostra il rifiuto a un processo dello stesso utente con etichetta Low, non a un altro utente.
- Misure di CPU su 60 s con un campione per configurazione.

## Azioni richieste

1. Utente: fornire un secondo account locale non amministratore per la prova "IPC/console" con un altro utente (sezione 7.4), oppure decidere di registrarla come limite.
2. Prova con tastiera fisica: eseguire `lightvm-desktop --vnc-socket … --qmp-socket …` con sessione sbloccata e digitare nel terminale del guest, includendo AltGr con layout IT, Ctrl+C/V e tastierino; registrare l'esito.
3. F1 (`crates/qemu-backend`, supervisor): un solo proprietario della connessione QMP che multiplexa input e screenshot (QEMU serve un client alla volta).
4. Decidere come trattare il trasporto TLS dati i punti 5 e 8 (thread non arrestabile, `CloseWait` lato QEMU), per esempio limitandolo a ripiego con `connections=` e timeout del supervisor, da provare.
5. Valutare la sostituzione o la configurazione del percorso appunti di egui-winit (lettura degli appunti host su Ctrl+V) prima di F2.
6. Provocare un cambio di risoluzione del guest (DesktopSize reale) e un caso con Hextile/CopyRect lato QEMU.
7. Aggiornare l'indice di `docs/verification/README.md` e `docs/STATUS.md` (file non di competenza di questo passo).

## Decisioni proposte

Proposte; nessun ADR è stato modificato.

**ADR-0003 (console RFB interno):** confermare il client interno: con 2 072 righe (`wc -l` su `src` e `tests`, test unitari inclusi), il sottoinsieme (3.8, ZRLE/Hextile/CopyRect/Raw/DesktopSize) funziona sul guest reale, è privo di `unsafe` e la sola lettura è verificata da test. Registrare come requisiti: arresto tramite risposta provocata dal server su Windows (punto 8), rifiuto degli encoding non annunciati, scarto di ServerCutText. Il ripiego `screendump` non è necessario su questo host, ma la sua latenza (p95 19,2 ms) lo rende praticabile come modalità degradata.

**ADR-0013 (trasporto):** su Windows con QEMU 11.1.0 scegliere **VNC su socket Unix in directory privata** come trasporto primario (verificato; rifiuto a un processo a integrità bassa verificato; prova con secondo utente da completare). VeNCrypt X.509 funziona ma ha due limiti osservati (arresto non possibile lato client senza abbandonare il thread, `CloseWait` lato QEMU): proporlo solo come alternativa documentata, non come predefinito. `None` su TCP resta rifiutato dal client.

**ADR-0014 (egui/eframe e backend):** la console reale conferma le differenze di F0-01: glow 88–98 MiB (working set e private bytes), wgpu DX12 191–198 MiB di working set e circa 370 MiB di private bytes, CPU simile con animazione (9–10 % di un core). Se il budget di 250 MiB viene misurato sui private bytes, wgpu non lo rispetta già con la sola UI. Proposta: decisione dell'utente sul backend predefinito con questa evidenza; in ogni caso mantenere entrambe le build compilabili.

**Collocazione del client RFB nei crate:** proporre `crates/rfb-client` come crate di libreria separato, usato dalla UI (`apps/desktop`), invece di collocarlo in `crates/qemu-backend` come indica la sezione 6.2. Motivi: la sezione 6.4 esegue il client nel processo della UI, mentre `qemu-backend` appartiene al supervisor (processi, QMP, Job Object) e porterebbe nella UI dipendenze e capacità di controllo che la UI non deve avere; un crate a parte rende verificabile in isolamento la proprietà di sola lettura (nessuna dipendenza da QMP, test `sola_lettura.rs`); dipendenze e licenze (rustls, ring, flate2) restano inventariabili separatamente (ADR-0023). `qemu-backend` resterebbe responsabile di avviare QEMU con `-vnc unix:…` nella directory privata ed emettere la credenziale; input e `screendump` restano nel bridge QMP del supervisor. La modifica della sezione 6.2 richiede una decisione registrata in un ADR.
