//! Avvio di `lightvm-desktop`.
//!
//! Opzioni da riga di comando (spike e misure):
//! - `--smoke SECONDI`: chiude l'app dopo N secondi e stampa su stdout backend,
//!   versioni, dimensione finestra e numero di frame.
//! - `--renderer auto|glow|wgpu|warp|ELENCO`: `auto` (predefinito) prova glow,
//!   poi wgpu su adattatore hardware (DX12 su Windows), poi wgpu su
//!   rasterizzatore software (WARP). Un nome forza quel backend senza ripiego;
//!   un elenco separato da virgole (per esempio `glow,warp`) fissa l'ordine.
//! - `--static`: framebuffer statico, nessun repaint periodico (misura dell'idle).
//! - `--wgpu-adapter cpu|auto`: con `cpu` usa un adattatore wgpu di tipo CPU
//!   (WARP su DX12) al posto di quello hardware; da solo equivale a `--renderer warp`.
//! - `--glow-hw preferred|required|off`: accelerazione hardware richiesta a glutin
//!   (`off` su Windows fa fallire glow: serve a provare il ripiego).
//! - `--salta-controllo-librerie` (solo Linux): non esegue il controllo
//!   preventivo delle librerie caricate con `dlopen` (F1-14).

use std::cell::Cell;
use std::io::Write as _;
use std::rc::Rc;
use std::time::{Duration, Instant};

use lightvm_desktop::LightVmApp;
use lightvm_desktop::render::{FPS_CONSOLE_DEGRADATA, InfoRendering, Piano, Tentativo};

#[derive(Debug)]
struct Args {
    smoke: Option<Duration>,
    renderer: Option<String>,
    animate: bool,
    wgpu_cpu: bool,
    glow_hw: String,
    salta_controllo_librerie: bool,
    console: ConsoleArgs,
}

/// Parametri della console reale (F0-04). Nessuna configurazione QEMU: solo
/// i percorsi dei socket già creati da chi ha avviato QEMU.
#[derive(Debug, Default)]
struct ConsoleArgs {
    vnc_socket: Option<String>,
    vnc_impl: Option<String>,
    vnc_tls: Option<String>,
    tls_dir: Option<String>,
    qmp_socket: Option<String>,
    capture_dir: Option<String>,
    tmp_dir: Option<String>,
    layout: Option<String>,
    max_fps: Option<u32>,
    dump_fb: Option<String>,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        smoke: None,
        renderer: None,
        salta_controllo_librerie: false,
        animate: true,
        wgpu_cpu: false,
        glow_hw: "preferred".to_owned(),
        console: ConsoleArgs::default(),
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--smoke" => {
                let v = it.next().ok_or("--smoke richiede un numero di secondi")?;
                let secs: f64 = v.parse().map_err(|_| format!("secondi non validi: {v}"))?;
                if !(secs.is_finite() && secs > 0.0 && secs <= 3600.0) {
                    return Err(format!("secondi fuori intervallo (0, 3600]: {v}"));
                }
                args.smoke = Some(Duration::from_secs_f64(secs));
            }
            "--renderer" => {
                args.renderer = Some(
                    it.next()
                        .ok_or("--renderer richiede auto, glow, wgpu, warp o un elenco")?,
                );
            }
            "--salta-controllo-librerie" => args.salta_controllo_librerie = true,
            "--static" => args.animate = false,
            "--wgpu-adapter" => {
                let v = it.next().ok_or("--wgpu-adapter richiede cpu o auto")?;
                args.wgpu_cpu = match v.as_str() {
                    "cpu" => true,
                    "auto" => false,
                    _ => return Err(format!("adattatore sconosciuto: {v}")),
                };
            }
            "--glow-hw" => {
                let v = it
                    .next()
                    .ok_or("--glow-hw richiede preferred, required o off")?;
                if !matches!(v.as_str(), "preferred" | "required" | "off") {
                    return Err(format!("valore --glow-hw sconosciuto: {v}"));
                }
                args.glow_hw = v;
            }
            "--vnc-socket" => {
                args.console.vnc_socket =
                    Some(it.next().ok_or("--vnc-socket richiede un percorso")?)
            }
            "--vnc-impl" => {
                args.console.vnc_impl = Some(it.next().ok_or("--vnc-impl richiede socket2 o uds")?)
            }
            "--vnc-tls" => {
                args.console.vnc_tls = Some(it.next().ok_or("--vnc-tls richiede 127.0.0.1:PORTA")?)
            }
            "--tls-dir" => {
                args.console.tls_dir = Some(it.next().ok_or("--tls-dir richiede una directory")?)
            }
            "--qmp-socket" => {
                args.console.qmp_socket =
                    Some(it.next().ok_or("--qmp-socket richiede un percorso")?)
            }
            "--capture-dir" => {
                args.console.capture_dir =
                    Some(it.next().ok_or("--capture-dir richiede una directory")?)
            }
            "--tmp-dir" => {
                args.console.tmp_dir = Some(it.next().ok_or("--tmp-dir richiede una directory")?)
            }
            "--layout" => args.console.layout = Some(it.next().ok_or("--layout richiede us o it")?),
            "--max-fps" => {
                let v = it.next().ok_or("--max-fps richiede un numero")?;
                let n: u32 = v
                    .parse()
                    .map_err(|_| format!("--max-fps non valido: {v}"))?;
                if !(1..=120).contains(&n) {
                    return Err(format!("--max-fps fuori intervallo 1..=120: {v}"));
                }
                args.console.max_fps = Some(n);
            }
            "--dump-fb" => {
                args.console.dump_fb = Some(it.next().ok_or("--dump-fb richiede un percorso PNG")?)
            }
            "--help" | "-h" => {
                println!(
                    "uso: lightvm-desktop [--smoke SECONDI] [--renderer auto|glow|wgpu|warp|glow,warp...] [--static] \
                     [--wgpu-adapter cpu|auto] [--glow-hw preferred|required|off] \n                     [--vnc-socket S [--vnc-impl socket2|uds] | --vnc-tls 127.0.0.1:P --tls-dir D] \n                     --qmp-socket S [--capture-dir D] [--tmp-dir D] [--layout us|it] [--max-fps N] [--dump-fb PNG]"
                );
                std::process::exit(0);
            }
            other => return Err(format!("argomento sconosciuto: {other}")),
        }
    }
    Ok(args)
}

struct DesktopApp {
    ui: LightVmApp,
    started: Instant,
    first_frame: Option<Duration>,
    smoke: Option<Duration>,
    reported: bool,
    repaint_causes: std::collections::BTreeMap<String, u64>,
    last_frame: Option<Instant>,
    intervals: [u64; 4],
    dump_fb: Option<String>,
    filtro_appunti: lightvm_desktop::appunti::FiltroAppunti,
}

impl eframe::App for DesktopApp {
    /// F1-13: primo punto dell'app in cui l'input è visibile, prima che il
    /// contesto egui lo elabori. Nessun `Paste` prosegue oltre questo gancio.
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        let esito = self.filtro_appunti.filtra_raw_input(raw_input);
        self.ui.conta_appunti_scartati(esito.scartati);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let now = Instant::now();
        if let Some(prev) = self.last_frame.replace(now) {
            let ms = now.duration_since(prev).as_secs_f64() * 1000.0;
            let bucket = if ms < 2.0 {
                0
            } else if ms < 20.0 {
                1
            } else if ms < 90.0 {
                2
            } else {
                3
            };
            self.intervals[bucket] += 1;
        }
        if self.first_frame.is_none() {
            self.first_frame = Some(self.started.elapsed());
        }
        self.ui.ui(ui);
        if self.smoke.is_some() {
            for c in ui.ctx().repaint_causes() {
                *self.repaint_causes.entry(c.to_string()).or_default() += 1;
            }
        }

        let Some(limit) = self.smoke else { return };
        let ctx = ui.ctx().clone();
        let elapsed = self.started.elapsed();
        if elapsed >= limit {
            if !self.reported {
                self.reported = true;
                self.report(&ctx, elapsed);
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        } else {
            // Un solo risveglio alla scadenza: anche con `--static` lo smoke termina.
            ctx.request_repaint_after(limit - elapsed);
        }
    }
}

impl DesktopApp {
    fn report(&self, ctx: &egui::Context, elapsed: Duration) {
        let ppp = ctx.pixels_per_point();
        let rect = ctx.content_rect();
        let inner = ctx.input(|i| i.viewport().inner_rect);
        let r = &self.ui.rendering;
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "SMOKE esito=ok");
        let _ = writeln!(out, "SMOKE renderer={}", r.dettaglio);
        let falliti: Vec<String> = r
            .falliti
            .iter()
            .map(|(t, e)| format!("{}:{e}", t.nome()))
            .collect();
        let _ = writeln!(
            out,
            "SMOKE backend={} degradato={} forzato={} tentativi_falliti={} indicatore=\"{}\"",
            r.attivo.map_or("?", Tentativo::nome),
            r.degradato(),
            r.forzato,
            falliti.len(),
            r.etichetta(),
        );
        for f in &falliti {
            let _ = writeln!(out, "SMOKE tentativo_fallito {f}");
        }
        #[allow(unused_mut, reason = "i backend presenti dipendono dalle feature")]
        let mut backends = String::new();
        #[cfg(feature = "wgpu")]
        backends.push_str(concat!(" wgpu=", env!("LIGHTVM_WGPU_VERSION")));
        #[cfg(feature = "glow")]
        backends.push_str(concat!(" glow=", env!("LIGHTVM_GLOW_VERSION")));
        let _ = writeln!(
            out,
            "SMOKE versioni egui={} eframe={}{backends}",
            env!("LIGHTVM_EGUI_VERSION"),
            env!("LIGHTVM_EFRAME_VERSION"),
        );
        let _ = writeln!(
            out,
            "SMOKE finestra_punti={:.0}x{:.0} pixels_per_point={ppp:.2} finestra_px={:.0}x{:.0} inner_rect={inner:?}",
            rect.width(),
            rect.height(),
            rect.width() * ppp,
            rect.height() * ppp,
        );
        let _ = writeln!(
            out,
            "SMOKE frame={} aggiornamenti_fb={} durata_s={:.2} primo_frame_ms={} appunti_scartati={}",
            self.ui.frames(),
            self.ui.fb_updates(),
            elapsed.as_secs_f64(),
            self.first_frame.map_or(-1, |d| d.as_millis() as i64),
            self.ui.appunti_scartati(),
        );
        let _ = writeln!(
            out,
            "SMOKE intervalli_tra_frame <2ms={} 2-20ms={} 20-90ms={} >=90ms={}",
            self.intervals[0], self.intervals[1], self.intervals[2], self.intervals[3]
        );
        if let Some(live) = self.ui.live_console() {
            let s = &live.stats;
            let (w, h) = live.geometry();
            let _ = writeln!(
                out,
                "SMOKE console geometria={w}x{h} aggiornamenti_rfb={} caricamenti_texture={} pixel_caricati={} eventi_input={} ridimensionamenti={}",
                s.rfb_updates, s.texture_uploads, s.uploaded_pixels, s.input_events, s.resizes
            );
            if let Some(path) = &self.dump_fb {
                match live.framebuffer_snapshot().map(|fb| save_png(&fb, path)) {
                    Some(Ok(nb)) => {
                        let _ = writeln!(out, "SMOKE dump_fb={path} non_neri={nb:.2}%");
                    }
                    Some(Err(e)) => {
                        let _ = writeln!(out, "SMOKE dump_fb_errore={e}");
                    }
                    None => {
                        let _ = writeln!(out, "SMOKE dump_fb_errore=nessun framebuffer");
                    }
                }
            }
        }
        for (cause, n) in &self.repaint_causes {
            let _ = writeln!(out, "SMOKE causa_repaint n={n} {cause}");
        }
        let _ = out.flush();
    }
}

/// Descrizione dell'adattatore wgpu e indicazione "adattatore di tipo CPU".
#[cfg(feature = "wgpu")]
fn wgpu_info(rs: &eframe::egui_wgpu::RenderState) -> (String, bool) {
    let i = rs.adapter.get_info();
    (
        format!(
            "wgpu backend={:?} adapter=\"{}\" tipo={:?} driver=\"{}\" driver_info=\"{}\"",
            i.backend, i.name, i.device_type, i.driver, i.driver_info
        ),
        i.device_type == eframe::egui_wgpu::wgpu::DeviceType::Cpu,
    )
}

/// Versione GL dal contesto glow, senza `unsafe` (`HasContext::version`).
/// Il nome del renderer GL (`glGetString(GL_RENDERER)`) richiederebbe una
/// chiamata `unsafe` e non è riportato.
#[cfg(feature = "glow")]
fn glow_info(gl: &eframe::glow::Context) -> (String, bool) {
    use eframe::glow::HasContext as _;
    let v = gl.version();
    let software = ["llvmpipe", "softpipe", "GDI Generic", "SwiftShader"]
        .iter()
        .any(|s| v.vendor_info.contains(s));
    (
        format!(
            "glow versione_gl={}.{}{} driver=\"{}\"",
            v.major,
            v.minor,
            if v.is_embedded { " ES" } else { "" },
            v.vendor_info
        ),
        software,
    )
}

/// Opzioni eframe per un tentativo.
fn opzioni(t: Tentativo, args: &Args) -> eframe::NativeOptions {
    #[allow(unused_mut, reason = "i campi modificati dipendono dalle feature")]
    let mut options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("LightVM")
            .with_inner_size([1460.0, 860.0])
            .with_min_inner_size([640.0, 400.0]),
        // Lo stesso event loop serve a più tentativi: senza `run_and_return`
        // winit non permette di ricrearlo dopo un errore di inizializzazione.
        run_and_return: true,
        ..Default::default()
    };
    match t {
        #[cfg(feature = "glow")]
        Tentativo::Glow => {
            options.renderer = eframe::Renderer::Glow;
            options.glow_options.hardware_acceleration = match args.glow_hw.as_str() {
                "required" => eframe::egui_glow::HardwareAcceleration::Required,
                "off" => eframe::egui_glow::HardwareAcceleration::Off,
                _ => eframe::egui_glow::HardwareAcceleration::Preferred,
            };
        }
        #[cfg(feature = "wgpu")]
        Tentativo::WgpuHardware | Tentativo::WgpuSoftware => {
            use eframe::egui_wgpu::{WgpuSetup, wgpu};
            options.renderer = eframe::Renderer::Wgpu;
            let software = t == Tentativo::WgpuSoftware;
            if let WgpuSetup::CreateNew(create) = &mut options.wgpu_options.wgpu_setup {
                // ADR-0014: su Windows wgpu usa DX12 (hardware e WARP).
                if cfg!(windows) {
                    create.instance_descriptor.backends = wgpu::Backends::DX12;
                }
                create.native_adapter_selector =
                    Some(std::sync::Arc::new(move |adapters, _surface| {
                        for a in adapters {
                            let i = a.get_info();
                            eprintln!(
                                "wgpu adattatore disponibile: {:?} \"{}\" {:?}",
                                i.backend, i.name, i.device_type
                            );
                        }
                        adapters
                            .iter()
                            .find(|a| {
                                (a.get_info().device_type == wgpu::DeviceType::Cpu) == software
                            })
                            .cloned()
                            .ok_or_else(|| {
                                if software {
                                    "nessun adattatore wgpu di tipo CPU (WARP) disponibile"
                                        .to_owned()
                                } else {
                                    "nessun adattatore wgpu hardware disponibile".to_owned()
                                }
                            })
                    }));
            }
        }
        #[allow(unreachable_patterns, reason = "dipende dalle feature compilate")]
        _ => {}
    }
    let _ = &args.glow_hw;
    options
}

/// Testo di un panico catturato.
fn testo_panico(p: &(dyn std::any::Any + Send)) -> String {
    p.downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panico senza messaggio".to_owned())
}

/// Messaggio per un panico durante l'avvio del rendering o della finestra.
fn errore_da_panico(msg: &str) -> String {
    use lightvm_desktop::librerie::{Sessione, mancanti, messaggio, panico_da_libreria};
    if !(cfg!(target_os = "linux") || panico_da_libreria(msg)) {
        return format!("LightVM si è interrotto durante l'avvio della finestra: {msg}");
    }
    let sessione = Sessione::rileva(
        std::env::var("WAYLAND_DISPLAY").ok().as_deref(),
        std::env::var("DISPLAY").ok().as_deref(),
    );
    #[cfg(target_os = "linux")]
    let m = {
        let cache = std::fs::read("/etc/ld.so.cache").unwrap_or_default();
        mancanti(sessione, |f| {
            lightvm_desktop::librerie::presente_nel_sistema(&cache, f)
        })
    };
    #[cfg(not(target_os = "linux"))]
    let m = mancanti(sessione, |_| true);
    messaggio(sessione, &m, Some(msg))
}

fn main() -> std::process::ExitCode {
    let started = Instant::now();
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("errore: {e}");
            return std::process::ExitCode::from(2);
        }
    };
    let piano = match Piano::da_argomenti(args.renderer.as_deref(), args.wgpu_cpu) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("errore: {e}");
            return std::process::ExitCode::from(2);
        }
    };

    // F1-14: su Linux, prima di creare la finestra, controlla le librerie che
    // winit/glutin/wgpu aprono con `dlopen`.
    #[cfg(target_os = "linux")]
    if !args.salta_controllo_librerie
        && let Err(msg) = lightvm_desktop::librerie::controllo_preventivo()
    {
        eprintln!("errore: {msg}");
        println!("SMOKE esito=errore dettaglio=\"librerie di sistema mancanti\"");
        return std::process::ExitCode::from(3);
    }
    #[cfg(not(target_os = "linux"))]
    let _ = args.salta_controllo_librerie;

    let console = match console_config(&args.console) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("errore: {e}");
            return std::process::ExitCode::from(2);
        }
    };

    // I panici dovuti a librerie mancanti diventano un messaggio utilizzabile
    // (sotto): il gancio predefinito resta per tutti gli altri.
    let gancio_predefinito = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let msg = testo_panico(info.payload());
        if !lightvm_desktop::librerie::panico_da_libreria(&msg) {
            gancio_predefinito(info);
        }
    }));

    let mut falliti: Vec<(Tentativo, String)> = Vec::new();
    let n = piano.tentativi.len();
    for (indice, &t) in piano.tentativi.iter().enumerate() {
        let creata = Rc::new(Cell::new(false));
        let info_base = InfoRendering {
            attivo: Some(t),
            dettaglio: String::new(),
            adattatore_cpu: false,
            falliti: falliti.clone(),
            forzato: piano.forzato,
        };
        let console = console.clone();
        let dump_fb = args.console.dump_fb.clone();
        let animate = args.animate;
        let smoke = args.smoke;
        let creata_app = creata.clone();
        let options = opzioni(t, &args);
        let esito = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            eframe::run_native(
                "LightVM",
                options,
                Box::new(move |cc| {
                    creata_app.set(true);
                    let mut info = info_base;
                    #[cfg(feature = "wgpu")]
                    if let Some(rs) = &cc.wgpu_render_state {
                        (info.dettaglio, info.adattatore_cpu) = wgpu_info(rs);
                    }
                    #[cfg(feature = "glow")]
                    if let Some(gl) = &cc.gl {
                        (info.dettaglio, info.adattatore_cpu) = glow_info(gl);
                    }
                    let _ = cc;
                    let mut ui = match console {
                        Some(mut cfg) => {
                            if info.degradato() {
                                cfg.max_fps = cfg.max_fps.min(FPS_CONSOLE_DEGRADATA);
                            }
                            LightVmApp::with_console(cfg)
                        }
                        None => LightVmApp::new(animate),
                    };
                    eprintln!("rendering: {}", info.spiegazione().replace('\n', "; "));
                    ui.rendering = info;
                    Ok(Box::new(DesktopApp {
                        ui,
                        started,
                        first_frame: None,
                        smoke,
                        reported: false,
                        repaint_causes: Default::default(),
                        last_frame: None,
                        intervals: [0; 4],
                        dump_fb,
                        filtro_appunti: Default::default(),
                    }))
                }),
            )
        }));
        match esito {
            Ok(Ok(())) => return std::process::ExitCode::SUCCESS,
            Ok(Err(e)) if !creata.get() && indice + 1 < n => {
                let e = e.to_string();
                eprintln!(
                    "rendering: tentativo {} fallito ({e}); ripiego su {}",
                    t.nome(),
                    piano.tentativi[indice + 1].nome()
                );
                falliti.push((t, e));
            }
            Ok(Err(e)) => {
                falliti.push((t, e.to_string()));
                let elenco: Vec<String> = falliti
                    .iter()
                    .map(|(t, e)| format!("{}: {e}", t.nome()))
                    .collect();
                println!(
                    "SMOKE esito=errore dettaglio=\"{}\"",
                    elenco.join(" | ").replace('"', "'")
                );
                eprintln!(
                    "errore: nessun backend di rendering utilizzabile ({})",
                    elenco.join("; ")
                );
                return std::process::ExitCode::FAILURE;
            }
            Err(p) => {
                let msg = testo_panico(p.as_ref());
                eprintln!("errore: {}", errore_da_panico(&msg));
                println!(
                    "SMOKE esito=errore dettaglio=\"panico all'avvio: {}\"",
                    msg.replace('"', "'")
                );
                return std::process::ExitCode::from(3);
            }
        }
    }
    std::process::ExitCode::FAILURE
}

fn save_png(fb: &rfb_client::Framebuffer, path: &str) -> Result<f64, String> {
    let img = f0_console::image::RgbImage {
        width: u32::from(fb.width()),
        height: u32::from(fb.height()),
        data: fb.to_rgb(),
    };
    std::fs::write(path, f0_console::image::encode_png(&img)?).map_err(|e| e.to_string())?;
    Ok(img.non_black_percent())
}

/// Costruisce la configurazione della console dai soli percorsi forniti.
fn console_config(
    a: &ConsoleArgs,
) -> Result<Option<lightvm_desktop::console::ConsoleConfig>, String> {
    use lightvm_desktop::keymap::KeyboardLayout;
    use rfb_client::{TlsConfig, Transport, UnixImpl};
    use std::path::PathBuf;
    let vnc = match (&a.vnc_socket, &a.vnc_tls) {
        (None, None) => {
            if a.qmp_socket.is_some() {
                return Err("--qmp-socket richiede --vnc-socket o --vnc-tls".into());
            }
            return Ok(None);
        }
        (Some(_), Some(_)) => {
            return Err("usare --vnc-socket oppure --vnc-tls, non entrambi".into());
        }
        (Some(p), None) => Transport::Unix {
            path: PathBuf::from(p),
            imp: match a.vnc_impl.as_deref().unwrap_or("socket2") {
                "socket2" => UnixImpl::Socket2,
                "uds" => UnixImpl::UdsWindows,
                o => return Err(format!("--vnc-impl sconosciuto: {o}")),
            },
        },
        (None, Some(addr)) => {
            let dir = PathBuf::from(a.tls_dir.as_deref().ok_or("--vnc-tls richiede --tls-dir")?);
            Transport::VencryptTls {
                addr: addr.parse().map_err(|e| format!("--vnc-tls: {e}"))?,
                tls: TlsConfig {
                    ca_cert: dir.join("ca-cert.pem"),
                    client_cert: dir.join("client-cert.pem"),
                    client_key: dir.join("client-key.pem"),
                    server_name: "localhost".into(),
                },
            }
        }
    };
    let qmp = PathBuf::from(
        a.qmp_socket
            .as_deref()
            .ok_or("--qmp-socket obbligatorio con la console")?,
    );
    let capture = PathBuf::from(a.capture_dir.as_deref().unwrap_or("."));
    let tmp = a
        .tmp_dir
        .as_deref()
        .map(PathBuf::from)
        .or_else(|| qmp.parent().map(PathBuf::from))
        .unwrap_or_else(|| capture.clone());
    let layout = match a.layout.as_deref().unwrap_or("us") {
        "us" => KeyboardLayout::Us,
        "it" => KeyboardLayout::It,
        o => return Err(format!("--layout sconosciuto: {o}")),
    };
    Ok(Some(lightvm_desktop::console::ConsoleConfig {
        vnc,
        qmp_socket: qmp,
        capture_dir: capture,
        tmp_dir: tmp,
        layout,
        max_fps: a.max_fps.unwrap_or(30),
    }))
}
