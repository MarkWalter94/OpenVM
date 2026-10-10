# F0-04 — Native console with RFB client, input and screenshots on the protected channel

- Execution date (UTC): 2026-10-09, from 11:11 to 12:34
- Executed by: development agent (Claude Code session), at the request of the project owner
- Roadmap step: section 15, step 3; section 10 (F0, "native console with RFB client on the protected channel or its fallback, input, screenshots"); sections 6.4, 7.2 (points 3 and 6), 7.4 (rows "IPC/console", "Console", "Host"), 13
- Related ADRs: ADR-0003, ADR-0013, ADR-0014 (not modified)
- Overall outcome: **partial** — RFB console, QMP input and screenshots verified on the real guest with the Unix socket transport; VeNCrypt/TLS transport verified with a limit on shutdown; input from a physical keyboard through winit not executed; test with a second user not executed.

Allowed outcomes: **verified**, **failed**, **not executed** (section 17). Raw logs: `lab/logs/f0-04-evidence.txt`, `lab/logs/f0-04-evidence-2.txt`, `lab/logs/f0-04-verifiche-finali.txt`, `lab/logs/f0-04-dipendenze-ricerca.txt`, `lab/logs/f0-04-ui-*.stdout.txt`. Images in `lab/vms/f0-04/`.

## Purpose

Demonstrate on this host the console path of section 6.4: RFB client in Rust, running in the UI process, read-only, on a protected channel; framebuffer drawn as an egui texture; keyboard and mouse forwarded **only** via QMP; screenshots via QMP `screendump`; section 13 measurements (screenshot latency, update rate, UI CPU and memory).

It does not demonstrate: behaviour on Linux, in RDP or on other hosts; per-run display credential issued by the supervisor (the supervisor does not exist yet); session revocation; isolation from a second local user; the physical scancode → winit → egui path with a real keyboard.

## Environment

| Item | Value |
| --- | --- |
| Host | HP ZBook Fury 16 G9, Intel Core i7-12800HX (24 logical processors), 31.6 GiB RAM, NTFS volume E: (as in F0-01/F0-02) |
| OS and build | Windows 11 Pro 10.0.26200 |
| Accelerator | WHPX; `query-accelerators` → `{"enabled":"whpx","present":["qtest","tcg","whpx"]}`; no TCG fallback configured |
| Toolchain | rustc 1.99.0 (b940084d7 2026-09-28), cargo 1.99.0, MSVC target with the Build Tools 2022 environment loaded by a `vcvars64.bat` wrapper in the session scratchpad (same method as F0-01, no changes to the system) |
| QEMU | 11.1.0 portable, `lab/qemu/qemu-11.1.0/qemu-system-x86_64.exe`, SHA-256 `47d57a6072e0bb3bd98f87926eb129eb1736dfe818c67b3b81ef7ce4edd0b3cd` (recomputed, matches F0-02) |
| Firmware | `edk2-x86_64-code.fd` SHA-256 `33090cc0…6dbb2a` (read-only pflash); per-run copy of `edk2-i386-vars.fd` SHA-256 `5d2ac383…be5d1e` in `lab/vms/f0-04/priv/` |
| Guest | `ubuntu-26.04.1-desktop-amd64.iso`, SHA-256 `601e30fb…bda1f` (recomputed), `.verified` file present; live session, no installation to disk |
| QEMU configuration | `-machine q35 -accel whpx -cpu qemu64 -smp 2 -m 4096 -nodefaults -no-user-config -nic none -device virtio-vga -device virtio-tablet-pci -device virtio-keyboard-pci -display none`; VNC and QMP see Procedure |
| GPU | NVIDIA RTX A2000 8GB Laptop GPU, driver 32.0.15.9658 |
| Graphics session | local console. **During most of the tests the Windows session was locked**: `LogonUI.exe` running, and a capture of the app window returned the lock screen (capture deleted). Unlocked by the user before 12:22 (`LogonUI: 0` in the log). Every measurement reports the state |
| Other conditions | other agents were running their own QEMU instances in parallel (e.g. `-name lightvm-f0-06-c-desktop-21`) and builds in the same `target/` |

### Crates added (exact versions, no git sources)

Command: `cargo tree -p rfb-client -e normal`, `cargo tree -p f0-console -e normal --depth 1`; `grep -c 'source = "git' Cargo.lock` → `0`.

| Crate | Version | Licence | Used by | Reason |
| --- | --- | --- | --- | --- |
| rustls | 0.23.45 (`default-features = false`, `ring`, `std`, `tls12`) | Apache-2.0 OR ISC OR MIT | rfb-client | VeNCrypt X.509 with client certificate |
| ring | 0.17.14 | Apache-2.0 AND ISC | rfb-client (via rustls) | crypto provider; aws-lc-rs would require CMake/NASM |
| rustls-pki-types | 1.15.1 | MIT OR Apache-2.0 | rfb-client (via rustls) | PEM reading |
| rustls-webpki | 0.103.15 | ISC | via rustls | certificate verification |
| socket2 | 0.6.5 (`all`) | MIT OR Apache-2.0 | rfb-client, f0-console | AF_UNIX on Windows, chosen |
| uds_windows | 1.2.1 | MIT | rfb-client, f0-console | alternative AF_UNIX, tested |
| flate2 | 1.1.10 (`rust_backend`, miniz_oxide 0.9.1) | MIT OR Apache-2.0 | rfb-client | ZRLE zlib stream |
| png | 0.18.1 | MIT OR Apache-2.0 | f0-console | in-memory PNG |
| serde_json | 1.0.151 | MIT OR Apache-2.0 | f0-console | QMP |

New transitive dependencies in the lockfile: `zeroize 1.9.1`, `untrusted 0.9.0`, `subtle 2.6.1` (BSD-3-Clause), `getrandom 0.2.17`, `memoffset 0.9.1`, `tempfile 3.27.0` (via uds_windows). The root `Cargo.lock` was updated by cargo for these dependencies; the root `Cargo.toml` was not modified (dependencies declared in the crate manifests).

## Code produced

| Path | Content |
| --- | --- |
| `crates/rfb-client/` | `#![forbid(unsafe_code)]` library: 3.8 handshake, security, ServerInit, SetPixelFormat BGRX, SetEncodings (ZRLE, Hextile, CopyRect, Raw, DesktopSize −223), loop of incremental FramebufferUpdateRequests with a rate limit, resize, `Session` on a thread with callback and shared framebuffer (`Arc<Mutex<Framebuffer>>`) |
| `crates/rfb-client/tests/read_only.rs` | negative test on read-only behaviour |
| `spikes/f0-console/` | `f0_console` library (minimal typed QMP, PPM→PNG, US typing, percentiles) and `f0-console` binary (probes, measurements, QMP actions) |
| `apps/desktop/src/console.rs`, `keymap.rs` | real console in the UI: egui texture updated by rectangles (`TextureHandle::set_partial`), connection/geometry state, input → QMP, Screenshot button |
| `apps/desktop/src/main.rs` | options `--vnc-socket`, `--vnc-impl`, `--vnc-tls`/`--tls-dir`, `--qmp-socket`, `--capture-dir`, `--tmp-dir`, `--layout us|it`, `--max-fps`, `--dump-fb` |
| `apps/desktop/tests/kittest_ui.rs`, `real_console.rs` | headless UI tests; integration test against real QEMU (ignored by default) |
| `lab/scripts/f0-04-boot.ps1`, `f0-04-stop.ps1`, `f0-04-measure-ui.ps1` | QEMU start/stop (derived from `boot-iso.ps1`), UI CPU/memory measurement |

## Procedure

1. Start QEMU in Unix socket mode: `powershell -File lab/scripts/f0-04-boot.ps1 -Mode unix`. The script creates `lab/vms/f0-04/priv` with `icacls priv /inheritance:r /grant:r "<user>:(OI)(CI)F"`, copies the UEFI variables, starts QEMU with
   `-vnc unix:E:/OpenVM/lab/vms/f0-04/priv/vnc.sock -qmp unix:E:/OpenVM/lab/vms/f0-04/priv/qmp.sock,server=on,wait=off` and records the PID.
2. TLS mode (second transport, without media): `f0-04-boot.ps1 -Mode tls` →
   `-object tls-creds-x509,id=tls0,dir=E:/OpenVM/lab/vms/tls,endpoint=server,verify-peer=on -vnc 127.0.0.1:4,tls-creds=tls0` (test certificates from F0-02). Listening observed: `127.0.0.1:5904` only.
3. Probes and actions with `target/release/f0-console.exe` (`probe-unix`, `rfb-snapshot`, `rfb-fps`, `rfb-idle`, `session-stop`, `qmp … move|click|key|type|taps|screendump`, `bench-screendump`, `wait-desktop`, `diff`).
4. App: `lightvm-desktop.exe --smoke N --vnc-socket <priv>/vnc.sock --qmp-socket <priv>/qmp.sock --capture-dir lab/vms/f0-04 --tmp-dir <priv> --dump-fb <png>`; wgpu build (`target/release`, run with `WGPU_BACKEND=dx12`) and glow build (`--no-default-features --features glow --target-dir target/glow`).
5. UI measurements: `lab/scripts/f0-04-measure-ui.ps1` (start with `--smoke 75`, `TotalProcessorTime` at 10 s and 70 s, `WorkingSet64`/`PrivateMemorySize64` every 5 s).
6. Shutdown: `f0-04-stop.ps1` (QMP `quit`, then `Stop-Process` if needed), removal of orphaned sockets; process check with `Win32_Process` filtering on the `f0-04` command line.

## Results

### 1. Unix socket transport: socket2 versus uds_windows

| Item | Outcome | Evidence |
| --- | --- | --- |
| AF_UNIX connection with `socket2` (`Domain::UNIX`, `SockAddr::unix`) | verified | `PROBE impl=socket2 connesso=si byte=12 … prima_riga="RFB 003.008"`; QMP: greeting `{"QMP": {"version": {"qemu": {"micro": 0, "minor": 1, "major": 11} …` |
| AF_UNIX connection with `uds_windows` | verified | `PROBE impl=uds connesso=si byte=12 … "RFB 003.008"`; same QMP greeting |
| Full RFB handshake with both | verified | `RFB connesso canale=PrivateUnixSocket sicurezza="None su socket Unix privato" handshake_ms=5 server_init=1280x800 bpp=32 depth=24 nome="QEMU (f0-04)"`; identical PNGs (33 223 bytes) for the two implementations |
| Stopping a session blocked on a read | verified after a fix (see 8) | `SESSION stop_ms=93/94` with both implementations, event `Disconnected { error: None }` |

Choice: **socket2** as the default. The two implementations behaved identically in all tests (connection, handshake, same limits on `shutdown` and timeouts); socket2 also serves the VeNCrypt TCP, has fewer dependencies (uds_windows pulls in `tempfile` and `memoffset`) and a wider user base. uds_windows remains selectable (`UnixImpl::UdsWindows`, `--vnc-impl uds`) for comparison.

### 2. RFB client

| Item | Outcome | Evidence |
| --- | --- | --- |
| Version 3.8; rejection of lower versions | verified (real + test) | banner `RFB 003.008`; test `versione_precedente_alla_3_8_rifiutata` |
| `None` accepted only on Unix socket | verified | real handshake on Unix socket; on TCP the client requires VeNCrypt X509None |
| `None` on plaintext TCP rejected | verified (test with simulated stream) | test `none_su_tcp_in_chiaro_rifiutato`: error `InsecureChannel`, the client only wrote `RFB 003.008\n`. No plaintext TCP QEMU endpoint was started (task rule) |
| Shared ClientInit, ServerInit | verified | `server_init=1280x800 bpp=32 depth=24 nome="QEMU (f0-04)"` |
| SetPixelFormat BGRX, correct decoding | verified | GRUB and desktop rendered with correct colours (`rfb-early-socket2.png`, `desktop-0.png`) |
| Encoding used by QEMU | verified | QEMU always replied with ZRLE: `rects_by_encoding: [0, 0, 0, 1, 0]`, in animation `zrle=8361` rectangles in 30 s. Hextile, CopyRect and Raw are covered only by unit tests (`hextile_sfondo_e_subrect`, `copy_rect_con_sovrapposizione`, `zrle_solido_palette_e_rle_su_due_rettangoli`) |
| DesktopSize | verified only with tests | `sessione_simulata_scrive_solo_tipi_0_2_3` (resize 4×2 → 8×3 and subsequent full request). No guest resolution change was triggered on the real guest |
| Unannounced extensions rejected | verified (test) | `encoding_non_annunciato_rifiutato` (Cursor −239 → `Unsupported`) |
| ServerCutText discarded, never delivered | verified (test) | counter `cut_text_discarded: 1` in the test; no code that writes to the clipboard |
| Integrity after incremental updates | verified | app 45 s during a time-limited animation: `aggiornamenti_rfb=204 caricamenti_texture=4514`, then `DIFF … pixel_diversi=0` between the app framebuffer and `screendump` |
| Update after 20 s without traffic | verified | `IDLE modo=bloccante tasto_a_ms=20000 aggiornamenti_prima=1 pausa_max…=19578 aggiornamenti_dopo=8 primo_dopo_ms=Some(20270)` |

### 3. Console in the app (point 4a)

| Item | Outcome | Evidence |
| --- | --- | --- |
| Real framebuffer visible as a texture, wgpu DX12 | verified | `SMOKE console geometria=1280x800 aggiornamenti_rfb=1 caricamenti_texture=2 …`; `SMOKE dump_fb=…app-fb-wgpu.png non_neri=100.00%`; `DIFF … pixel_diversi=0` against `screendump` |
| Same, glow | verified | `renderer=glow … versione_gl="3.3.0 NVIDIA 596.58"`; `DIFF … pixel_diversi=0` |
| Same, TLS transport | verified | `--vnc-tls 127.0.0.1:5904`: `DIFF … pixel_diversi=0` |
| Update of changed rectangles only | verified | `set_partial` per rectangle; with 908 updates: `caricamenti_texture=21166 pixel_caricati=20948752` (about 23 000 pixels per update instead of 1 024 000) |
| Status and geometry indicator | implemented, verified only in the AccessKit tree | text "RFB: connesso (…)", "Geometria: 1280×800", "Input QMP", "Layout guest"; no valid screen capture (session locked) |
| First frame / app shutdown | verified | `primo_frame_ms` 401–872 (wgpu DX12), 250–664 (glow); exit with code 0 after the smoke on Unix socket |

Observation: the number of frames is about twice the number of updates (`frame=2115` for 908 updates; `<2ms=872` intervals in the first series). Each `request_repaint()` from the RFB thread produces two UI passes; the cost is included in the CPU measurements.

### 4. Input and screenshots via QMP (points 4b, 4c)

| Item | Outcome | Evidence |
| --- | --- | --- |
| Keyboard: arrows in the GRUB menu | verified | `key down` → selection on "Ubuntu (safe graphics)" (`grub-1-dopo-down.png`); `key up` → back |
| Absolute mouse: click on an element of the welcome window | verified | `click 662 326` → selection and language changed to "Deutsch", title "Willkommen bei Ubuntu" (`mouse-1-dopo-click.png`); `click 658 422` → back to English. `query-mice`: `QEMU Virtio Tablet` `absolute:true current:true` |
| Ctrl+Alt+T combination | verified | terminal opened within 8 s (`kbd-0-terminale.png`) |
| Typing with US layout | verified | `echo "LightVM F0-04: tastiera_US ok [1+2=3] ~/path?" \| tr a-z A-Z` → output `LIGHTVM F0-04: TASTIERA_US OK [1+2=3] ~/PATH?` (`kbd-1-testo.png`) |
| Declared IT layout | verified | guest with `gsettings … sources "[('xkb', 'it')]"`; qcodes `bracket_left,apostrophe,semicolon,backslash,less,equal,minus,grave_accent,slash,bracket_right` → `èàòù < ì ' \ - +` (`kbd-2-layout-it.png`), as in the `keymap::base_char` table. US layout restored and verified (`kbd-3-ritorno-us.png`) |
| UI → key table → QMP → guest (without winit) | verified | `cargo test -p lightvm-desktop --test console_reale -- --ignored`: egui events with `physical_key`, AccessKit click on the image for focus → in the guest `echo UI-kittest` and output `UI-kittest` (`ui-screenshot-001.png`); Shift left pressed and released by the UI on focus loss (`eventi_input prima=40 dopo=41`) |
| "Screenshot" button | verified (via AccessKit) | `SCREENSHOT screenshot 1280x800 in 448 ms: …ui-screenshot-001.png` |
| Real physical keyboard → winit → egui → QMP | **not executed** | the session was locked and a `SendInput` injection would have acted on the user's desktop; no synthetic OS event sent |
| Real pointer → QMP | observed, effect not verified | after unlocking, the user's pointer passing over the window produced `eventi_input=151` and `160` in the smoke; no control screendump |
| UI pointer → QMP | verified only with a unit test | `puntatore_scalato_e_pulsanti` (x=1279 out of 1280 → 32767; left button down/up) |

Axis scaling: `scale_abs(px, size) = px * 32767 / (size - 1)`, same formula as `qemu_input_scale_axis` used by the QEMU VNC server; test `scala_assi_come_qemu`.

#### Keys covered (table `apps/desktop/src/keymap.rs`, identical for US and IT)

Covered: A–Z, 0–9, F1–F12, Esc, Tab, Backspace, Enter, Space, Ins, Del, Home, End, PgUp, PgDn, arrows, `- = [ ] \ ; ' \` , . /` (US positions; in IT they produce `' ì è + ù ò à \ , . -`), ISO `<>` key (`less`), left and right Shift/Ctrl/Alt/Super, keypad `+` (`kp_add`). The test `ogni_tasto_egui_e_mappato_o_dichiarato_non_coperto` checks that every entry of `egui::Key::ALL` is mapped or declared not covered.

Not covered: F13–F35; Caps Lock, Num Lock, Scroll Lock, Print Screen, Pause, Menu (egui-winit 0.36.2 does not convert them into `egui::Key`); dedicated Copy/Cut/Paste and BrowserBack keys; separate keypad (egui-winit merges Numpad0–9 with the digits, NumpadEnter with Enter, NumpadSubtract with `-`, NumpadDivide with `/`); general Unicode characters (no guest agent, section 6.4).

Relevant egui-winit 0.36.2 behaviours (read in the source `egui-winit/src/lib.rs`, `on_keyboard_input`): Ctrl+C/X/V do not produce `Event::Key` but `Copy`/`Cut`/`Paste`; the UI reconstructs the press of C/X/V and **discards** the text of `Paste` (test `incolla_non_inoltra_il_testo`), but egui-winit still reads the host clipboard when Ctrl+V is pressed. On Windows AltGr normally arrives as left Ctrl + right Alt: behaviour not verified in this test.

### 5. VeNCrypt X.509 transport with rustls

| Item | Outcome | Evidence |
| --- | --- | --- |
| Handshake with valid client certificate | verified | `sicurezza="VeNCrypt X509None, Some(TLSv1_3) Some(TLS13_AES_256_GCM_SHA384)" handshake_ms=14 server_init=1280x800`; UEFI Shell visible |
| Client certificate signed by a foreign CA | verified (no framebuffer) | `ERRORE I/O … (os error 10060) exit=1 tempo_ms=10076`: QEMU sends neither SecurityResult nor ServerInit; the client exits because of the 10 s handshake timeout |
| Wrong server name | verified | `TLS: invalid peer certificate: certificate not valid for name "altro.example"` |
| Non-loopback address | verified (client side) | `indirizzo 192.0.2.1:5904 non di loopback: rifiutato` |
| Incremental update over TLS | verified | `IDLE … aggiornamenti_prima=0 … aggiornamenti_dopo=1 primo_dopo_ms=Some(11035)` after keys `a,b` in the UEFI Shell |
| Session stop over TLS | **failed** | `SESSION stop_ms=2004/2006/2001` and `nessun evento entro 15 s dallo stop`: the thread stays blocked on a read and is abandoned after 2 s |
| Closing on the QEMU side | anomaly | `Get-NetTCPConnection -LocalPort 5904`: 11 connections in `CloseWait` after the tests, then 7, then 2 about a minute later. QEMU does not immediately close TLS connections terminated by the client, including those with a rejected certificate |

### 6. Negative tests (section 7.4)

| Item | Outcome | Evidence |
| --- | --- | --- |
| The client cannot send input, clipboard or power control | verified | `tests/read_only.rs`: `ClientMessage` has three variants with an exhaustive `match` (types 0, 2, 3); `CLIENT_MESSAGE_TYPES_SENT == [0, 2, 3]`; search in the code without comments for `keyevent`, `key_event`, `pointerevent`, `pointer_event`, `clientcuttext`, `client_cut_text`, `clipboard`, `button_mask`, `keysym`, `qemuextended`, `power` → 0 occurrences. Test `sessione_simulata_scrive_solo_tipi_0_2_3`: all bytes written by the client after ClientInit are messages 0, 2, 3 |
| Documented text search | verified | `rg -n -i "keyevent\|key_event\|pointerevent\|pointer_event\|clientcuttext\|client_cut_text\|clipboard\|keysym\|button_mask" crates/rfb-client/src` → a single line, a comment (`// Scartato: nessuna clipboard …`); search for `vec![4|5|6`, `push(4|5|6)`, `write_all(&[4|5|6` → no occurrences |
| Low-integrity process of the same user towards the private sockets | verified | copy of `f0-console.exe` with `icacls … /setintegritylevel Low` (`whoami /groups` of the copy: `Livello obbligatorio basso S-1-16-4096`): `probe-unix` on `vnc.sock` and `qmp.sock` with socket2 and uds_windows, `rfb-snapshot` and `qmp status` → `os error 10013` (WSAEACCES). Medium-integrity control at the same moment: connected, `QMP status=running`. First attempt in `lab/vms/f0-04/lowil` failed because of `icacls … Accesso negato` (directory without WRITE_OWNER): those processes were at medium integrity and do not count |
| Second user without access to the private directory | **not executed** | no second account available and no elevation. It requires a local non-administrator account and running `f0-console probe-unix` with its credentials (`runas /user:…`) towards `priv\vnc.sock` and `priv\qmp.sock`. Observed ACL: `priv` → only `OMEGAGROUP\MOrtali:(OI)(CI)(F)`; socket → `MOrtali:(I)(F)` |
| The UI exposes no fields that end up on the QEMU command line | verified | `nessun_campo_di_testo_in_alcuna_sezione` and `nessun_campo_di_testo_con_console_reale_configurata`: 0 `TextInput`/`MultilineTextInput`/`SearchInput`/`SpinButton` nodes in Dashboard, Console, Settings; the only console command is "Screenshot". Paths come only from the command line; no QEMU argument is built by the app. The `screendump` file name is generated by the QMP thread in the directory passed at startup |
| Second QMP client while the app is connected | observed | `secondo client: tempo totale 30078 ms` with `os error 10060`: QEMU serves only one QMP client at a time, the second does not receive the greeting while the first stays connected |

### 7. Measurements (section 13)

**End-to-end `screendump` screenshot** (QMP `screendump` PPM in `priv/` → PPM read and analysis → in-memory PNG, `Fast` compression of the png crate), 1280×800:

| Condition | n | p50 | p95 | max | errors / timeouts |
| --- | --- | --- | --- | --- | --- |
| desktop with idle terminal | 120 | 17.6 ms | 19.2 ms | 20.5 ms | 0 / 0 |
| intense animation in the guest | 100 | 16.4 ms | 19.3 ms | 24.3 ms | 0 / 0 |

Phases (120 samples): QMP 8.8/10.0/10.8 ms; PPM 3.2/3.7/4.1 ms; PNG 5.5/6.2/6.7 ms (p50/p95/max). Section 13 target (p95 ≤ 500 ms at 1280×720): measured below the target, at 1280×800. Timeouts: the QMP client uses a 30 s read timeout; none fired. The UI button recorded 419 ms and 448 ms (debug build of the test, includes writing the PNG to disk).

**Framebuffer update rate** (`f0-console rfb-fps`, final code):

| Animation in the guest | request limit | updates/s | interval p50 / p95 / max |
| --- | --- | --- | --- |
| none (idle desktop) | 60/s | 1 update in 50.7 s | — |
| `date` every 50 ms in the terminal | 60/s | 12.09 | 109.3 / 132.4 / 1146.1 ms |
| same | 30/s | 12.46 | 109.9 / 132.4 / 142.2 ms |
| `date` without pause | 60/s | 32.18 | 30.9 / 32.3 / 45.3 ms |
| same | 30/s | 26.19 | 31.1 / 111.8 / 114.3 ms |

The ceiling of about 32 updates/s matches the 30 ms interval of the QEMU VNC server (deduced from the measurement, not verified in the QEMU source). With a 30/s limit the p95 rises to 112 ms. In one series the UI processed 1945 updates in 75 s (about 26/s) with the same declared animation; an app/CLI control measurement under the same conditions gave 371 updates in 30 s (app) and 12.53/s (CLI): the difference depended on the state of the terminal in the guest, not on the UI.

**CPU and memory of the UI process** (60 s, console active, 1460×860, `f0-04-measure-ui.ps1`):

| Backend | Guest | Host session | RFB updates in 75 s | CPU (% of one core) | Working set end / max | Private bytes end / max | WS peak |
| --- | --- | --- | --- | --- | --- | --- | --- |
| wgpu DX12 | animation | locked | 1945 | 9.04 | 190.6 / 192.4 MiB | 369.6 / 372.0 MiB | 198.3 MiB |
| glow | animation | unlocked | 908 | 9.91 | 93.8 / 94.5 MiB | 96.4 / 97.6 MiB | 95.8 MiB |
| wgpu DX12 | idle | locked | 2 | 0.05 | 190.7 / 190.8 MiB | 369.2 / 369.4 MiB | 198.2 MiB |
| wgpu DX12 | idle | unlocked, user pointer over the window (151 events) | 7 | 1.58 | 197.7 / 197.7 MiB | 369.8 / 369.9 MiB | 199.6 MiB |
| glow | idle | unlocked | 3 | 0.18 | 91.8 / 91.8 MiB | 88.4 / 88.5 MiB | 95.5 MiB |

Host CPU (24 logical processors): 0.38 % (wgpu, animation), 0.41 % (glow, animation). The glow measurements were repeated with a binary rebuilt after the fix of point 8; the previous series (glow with the stale binary and an "anim" series that was actually with an idle guest) remain in the logs and are not used.

Comparison with the section 13 budget (manager memory ≤ 250 MiB, UI + supervisor; not modified): the supervisor does not exist yet, so the comparison covers only the UI. With wgpu DX12 the working set (191–198 MiB) is below the budget, the private bytes (369–372 MiB) exceed it; with glow both metrics (88–98 MiB) are below. The two metrics must not be compared under the same label (section 13).

### 8. Problems found and fixed during the test

1. **Session stop on Windows AF_UNIX.** Measurements with `session-stop` and `probe-unix`:
   - `shutdown(Both)` on a duplicate obtained with `try_clone` does not unblock a `recv` in progress on the original (`SESSION stop BLOCCATO oltre 5 s`, both implementations);
   - `SO_RCVTIMEO` set on the duplicate does not apply to the original (`PROBE lettura ancora bloccata dopo 3 s`); on the original it works (`seconda_lettura … Err(TimedOut) ms=3002`);
   - with short timeouts on the original and repeated reads, after a few timeouts the stream **stops delivering data without an error** (`IDLE modo=interrompibile … aggiornamenti_prima=13 … aggiornamenti_dopo=0`), while in blocking mode the update arrives. This is consistent with the Winsock warning about the indeterminate state after an `SO_RCVTIMEO` timeout;
   - `shutdown` on the shared original handle does not unblock the `recv` either (`stop_ms=2001/2003`).
   Solution adopted: a single shared handle (`Arc`) for reading, writing and closing; blocking reads; 10 s timeout only during the handshake (if it fires, the connection is closed anyway); `Session::stop` sends a non-incremental 1×1 FramebufferUpdateRequest (type 3, already allowed) to provoke a response from the server, then `shutdown(Write)`, and waits at most 2 s. Outcome: `stop_ms=93/94` on Unix socket; over TLS it does not work (point 5).
2. **First version of the app hung on close** for the same cause (process terminated with `Stop-Process`); solved by point 1.
3. `cargo fmt --all` formats the whole workspace: checked with `find -newer` that it modified only the files of this step.

## Limits

- A single host, local session; no tests in RDP, on Linux, on hosts without a GPU.
- Most tests with the Windows session locked: rendering took place (frames counted) but the window was not visible; the "locked" and "unlocked" measurements are not equivalent.
- Input from a physical keyboard through winit not tested; the UI path was tested from `egui::Event` onward.
- Hextile, CopyRect, Raw and DesktopSize were never sent by QEMU in these tests (QEMU chose ZRLE and the guest resolution did not change): covered only by tests with constructed data.
- Over TLS, stopping a session abandons the reader thread; QEMU leaves connections in `CloseWait` for tens of seconds.
- QMP accepts only one client at a time: with the app connected, QMP is not available to other tools.
- Each `request_repaint` produces two UI frames.
- The per-run, per-VM display credential, revocation and the supervisor do not exist yet: the channel protection here is the ACL of the private directory (Unix socket) or the test client certificate (TLS).
- The second local user was not tested; the low-integrity test demonstrates rejection of a process of the same user with a Low label, not of another user.
- CPU measurements over 60 s with one sample per configuration.

## Required actions

1. User: provide a second local non-administrator account for the "IPC/console" test with another user (section 7.4), or decide to record it as a limit.
2. Physical keyboard test: run `lightvm-desktop --vnc-socket … --qmp-socket …` with the session unlocked and type in the guest terminal, including AltGr with the IT layout, Ctrl+C/V and the keypad; record the outcome.
3. F1 (`crates/qemu-backend`, supervisor): a single owner of the QMP connection that multiplexes input and screenshots (QEMU serves one client at a time).
4. Decide how to treat the TLS transport given points 5 and 8 (non-stoppable thread, `CloseWait` on the QEMU side), for example restricting it to a fallback with `connections=` and supervisor timeouts, to be tested.
5. Consider replacing or configuring the egui-winit clipboard path (host clipboard read on Ctrl+V) before F2.
6. Trigger a guest resolution change (real DesktopSize) and a case with Hextile/CopyRect on the QEMU side.
7. Update the index of `docs/verification/README.md` and `docs/STATUS.md` (files not within the remit of this step).

## Proposed decisions

Proposals; no ADR was modified.

**ADR-0003 (internal RFB console):** confirm the internal client: with 2 072 lines (`wc -l` on `src` and `tests`, unit tests included), the subset (3.8, ZRLE/Hextile/CopyRect/Raw/DesktopSize) works on the real guest, is free of `unsafe` and read-only behaviour is verified by tests. Record as requirements: stop via a response provoked from the server on Windows (point 8), rejection of unannounced encodings, discarding of ServerCutText. The `screendump` fallback is not needed on this host, but its latency (p95 19.2 ms) makes it viable as a degraded mode.

**ADR-0013 (transport):** on Windows with QEMU 11.1.0 choose **VNC over Unix socket in a private directory** as the primary transport (verified; rejection of a low-integrity process verified; test with a second user to be completed). VeNCrypt X.509 works but has two observed limits (stop not possible on the client side without abandoning the thread, `CloseWait` on the QEMU side): propose it only as a documented alternative, not as the default. `None` over TCP remains rejected by the client.

**ADR-0014 (egui/eframe and backend):** the real console confirms the differences from F0-01: glow 88–98 MiB (working set and private bytes), wgpu DX12 191–198 MiB of working set and about 370 MiB of private bytes, similar CPU with animation (9–10 % of one core). If the 250 MiB budget is measured on private bytes, wgpu already fails it with the UI alone. Proposal: user decision on the default backend with this evidence; in any case keep both builds compilable.

**Placement of the RFB client among the crates:** propose `crates/rfb-client` as a separate library crate, used by the UI (`apps/desktop`), instead of placing it in `crates/qemu-backend` as section 6.2 indicates. Reasons: section 6.4 runs the client in the UI process, whereas `qemu-backend` belongs to the supervisor (processes, QMP, Job Object) and would bring into the UI dependencies and control capabilities the UI must not have; a separate crate makes the read-only property verifiable in isolation (no dependency on QMP, test `read_only.rs`); dependencies and licences (rustls, ring, flate2) remain separately inventoriable (ADR-0023). `qemu-backend` would remain responsible for starting QEMU with `-vnc unix:…` in the private directory and issuing the credential; input and `screendump` remain in the supervisor's QMP bridge. The change to section 6.2 requires a decision recorded in an ADR.
