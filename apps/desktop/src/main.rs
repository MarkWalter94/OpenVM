//! Avvio di `lightvm-desktop` (spike F0, passo 1).
//!
//! Opzioni da riga di comando (solo per lo spike e le misure di F0):
//! - `--smoke SECONDI`: chiude l'app dopo N secondi e stampa su stdout backend,
//!   versioni, dimensione finestra e numero di frame.
//! - `--renderer wgpu|glow`: sceglie il backend se la build li include entrambi.
//! - `--static`: framebuffer statico, nessun repaint periodico (misura dell'idle).
//! - `--wgpu-adapter cpu|auto`: con `cpu` forza un adattatore wgpu di tipo CPU
//!   (rasterizzatore software, per esempio WARP su DX12); fallisce se assente.
//! - `--glow-hw preferred|required|off`: accelerazione hardware richiesta a glutin.

use std::io::Write as _;
use std::time::{Duration, Instant};

use lightvm_desktop::LightVmApp;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RendererChoice {
    Wgpu,
    Glow,
}

#[derive(Debug)]
struct Args {
    smoke: Option<Duration>,
    renderer: Option<RendererChoice>,
    animate: bool,
    wgpu_cpu: bool,
    glow_hw: String,
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
                let v = it.next().ok_or("--renderer richiede wgpu o glow")?;
                args.renderer = Some(match v.as_str() {
                    "wgpu" => RendererChoice::Wgpu,
                    "glow" => RendererChoice::Glow,
                    _ => return Err(format!("renderer sconosciuto: {v}")),
                });
            }
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
                    "uso: lightvm-desktop [--smoke SECONDI] [--renderer wgpu|glow] [--static] \
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
}

impl eframe::App for DesktopApp {
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
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "SMOKE esito=ok");
        let _ = writeln!(out, "SMOKE renderer={}", self.ui.renderer_info);
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
            "SMOKE frame={} aggiornamenti_fb={} durata_s={:.2} primo_frame_ms={}",
            self.ui.frames(),
            self.ui.fb_updates(),
            elapsed.as_secs_f64(),
            self.first_frame.map_or(-1, |d| d.as_millis() as i64),
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

#[cfg(feature = "wgpu")]
fn wgpu_info(rs: &eframe::egui_wgpu::RenderState) -> String {
    let i = rs.adapter.get_info();
    format!(
        "wgpu backend={:?} adapter=\"{}\" tipo={:?} driver=\"{}\" driver_info=\"{}\"",
        i.backend, i.name, i.device_type, i.driver, i.driver_info
    )
}

#[cfg(feature = "glow")]
#[expect(
    unsafe_code,
    reason = "glow espone le query di stringhe GL solo come fn unsafe"
)]
fn glow_info(gl: &eframe::glow::Context) -> String {
    use eframe::glow::HasContext as _;
    // SAFETY: il contesto GL è corrente sul thread della UI durante la creazione
    // dell'app (eframe lo rende corrente prima di invocare l'app creator) e le
    // costanti passate sono enumerazioni valide per glGetString.
    let (vendor, renderer, version) = unsafe {
        (
            gl.get_parameter_string(eframe::glow::VENDOR),
            gl.get_parameter_string(eframe::glow::RENDERER),
            gl.get_parameter_string(eframe::glow::VERSION),
        )
    };
    format!("glow vendor=\"{vendor}\" renderer=\"{renderer}\" versione_gl=\"{version}\"")
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

    #[allow(unused_mut, reason = "i campi modificati dipendono dalle feature")]
    let mut options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("LightVM")
            .with_inner_size([1460.0, 860.0])
            .with_min_inner_size([640.0, 400.0]),
        ..Default::default()
    };

    #[cfg(all(feature = "wgpu", feature = "glow"))]
    {
        options.renderer = match args.renderer.unwrap_or(RendererChoice::Wgpu) {
            RendererChoice::Wgpu => eframe::Renderer::Wgpu,
            RendererChoice::Glow => eframe::Renderer::Glow,
        };
    }
    #[cfg(not(all(feature = "wgpu", feature = "glow")))]
    if let Some(r) = args.renderer {
        let compiled = if cfg!(feature = "wgpu") {
            RendererChoice::Wgpu
        } else {
            RendererChoice::Glow
        };
        if r != compiled {
            eprintln!("errore: renderer {r:?} non incluso in questa build (feature cargo)");
            return std::process::ExitCode::from(2);
        }
    }

    #[cfg(feature = "wgpu")]
    if args.wgpu_cpu {
        use eframe::egui_wgpu::{WgpuSetup, wgpu};
        if let WgpuSetup::CreateNew(create) = &mut options.wgpu_options.wgpu_setup {
            create.native_adapter_selector = Some(std::sync::Arc::new(|adapters, _surface| {
                for a in adapters {
                    let i = a.get_info();
                    eprintln!(
                        "wgpu adattatore disponibile: {:?} \"{}\" {:?}",
                        i.backend, i.name, i.device_type
                    );
                }
                adapters
                    .iter()
                    .find(|a| a.get_info().device_type == wgpu::DeviceType::Cpu)
                    .cloned()
                    .ok_or_else(|| "nessun adattatore wgpu di tipo CPU disponibile".to_owned())
            }));
        }
    }
    #[cfg(not(feature = "wgpu"))]
    if args.wgpu_cpu {
        eprintln!("errore: --wgpu-adapter richiede la feature wgpu");
        return std::process::ExitCode::from(2);
    }

    #[cfg(feature = "glow")]
    {
        options.glow_options.hardware_acceleration = match args.glow_hw.as_str() {
            "required" => eframe::egui_glow::HardwareAcceleration::Required,
            "off" => eframe::egui_glow::HardwareAcceleration::Off,
            _ => eframe::egui_glow::HardwareAcceleration::Preferred,
        };
    }
    #[cfg(not(feature = "glow"))]
    let _ = &args.glow_hw;

    let console = match console_config(&args.console) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("errore: {e}");
            return std::process::ExitCode::from(2);
        }
    };
    let dump_fb = args.console.dump_fb.clone();
    let animate = args.animate;
    let smoke = args.smoke;
    let result = eframe::run_native(
        "LightVM",
        options,
        Box::new(move |cc| {
            #[allow(unused_mut, reason = "renderer_info dipende dalle feature")]
            let mut ui = match console {
                Some(cfg) => LightVmApp::with_console(cfg),
                None => LightVmApp::new(animate),
            };
            #[cfg(feature = "wgpu")]
            if let Some(rs) = &cc.wgpu_render_state {
                ui.renderer_info = wgpu_info(rs);
            }
            #[cfg(feature = "glow")]
            if let Some(gl) = &cc.gl {
                ui.renderer_info = glow_info(gl);
            }
            let _ = cc;
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
            }))
        }),
    );

    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            println!("SMOKE esito=errore dettaglio=\"{e}\"");
            eprintln!("errore eframe: {e}");
            std::process::ExitCode::FAILURE
        }
    }
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
