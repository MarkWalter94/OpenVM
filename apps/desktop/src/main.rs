#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! Startup of `lightvm-desktop`.
//!
//! Command-line options. Test and measurement switches, only in builds with
//! the `dev-tools` feature (tests, CI smoke, measurements; never in the
//! distributed package, F6-01 §6):
//! - `--smoke SECONDS`: closes the app after N seconds and prints backend,
//!   versions, window size and frame count on stdout. It implies
//!   `--simulated-console`.
//! - `--simulated-console`: without an open VM console, the Console page
//!   shows a framebuffer generated in memory (measurements; never product
//!   data).
//! - `--static`: static simulated framebuffer, no periodic repaint (idle
//!   measurement).
//! - `--no-supervisor`: the supervisor is not started even if the automatic
//!   start of the Settings page is on (smoke and measurements; a user turns
//!   the automatic start off in Settings instead).
//!
//! Options of every build:
//! - `--renderer auto|glow|wgpu|warp|LIST`: `auto` (default) tries glow, then
//!   wgpu on a hardware adapter (DX12 on Windows), then wgpu on a software
//!   rasterizer (WARP). A single name forces that backend without fallback; a
//!   comma-separated list (for example `glow,warp`) sets the order.
//! - `--wgpu-adapter cpu|auto`: with `cpu` uses a CPU-type wgpu adapter (WARP
//!   on DX12) instead of the hardware one; on its own it is equivalent to
//!   `--renderer warp`.
//! - `--glow-hw preferred|required|off`: hardware acceleration requested from
//!   glutin (`off` on Windows makes glow fail: used to test the fallback).
//! - `--skip-library-check` (Linux only): does not run the preflight check of
//!   the libraries loaded with `dlopen` (F1-14).
//! - `--catalog DIR` (F1-11): starts `lightvm-service` as a child process on
//!   the given catalog and shows the service's VMs in the Dashboard;
//!   `--service-exe PATH` (default: next to this executable), `--qemu-dir DIR`,
//!   `--runtime-dir DIR` and `--audit-log FILE` are passed to the service.
//! - `--settings FILE`: UI settings file (default in the user profile, see
//!   `settings.rs`), written by the Settings page. Without `--catalog` the
//!   supervisor starts with the settings of the Settings page, if automatic
//!   start is on (default).
//! - `--open-vm NAME [--create]` (ADR-0017): at startup opens and starts the
//!   given VM (with `--create` it creates it if it does not exist, without
//!   installation media); with `--vcpu N`, `--ram MIB`, `--disk GIB`,
//!   `--cpu-max PERCENT`, `--memory-margin MIB`, `--disk-budget GIB|none` it
//!   first changes its resources and limits (saved in the VM settings; only
//!   while the VM is stopped).
//!
//! Release builds use the Windows GUI subsystem (no console window). The
//! diagnostics are also appended to `%LOCALAPPDATA%\LightVM\logs\lightvm-desktop.log`
//! (and the supervisor's to `lightvm-service.log`), rotated at 1 MiB, see
//! `diag.rs`. Output (smoke report, `--help`, diagnostics) still reaches redirected handles: pipe
//! it (`| Out-File`), use `cmd /c ... > file` or `Start-Process -Wait
//! -RedirectStandardOutput`. PowerShell's `& exe ... > file` neither waits
//! for a GUI program nor captures its output, and in an interactive console
//! without redirection nothing is printed. Debug builds keep the console.
//!
//! The console of the product path is opened from the Dashboard with a
//! per-run display ticket of the supervisor (ADR-0013); the F0 options that
//! connected the UI directly to QEMU's VNC and QMP sockets were removed in F2.

use std::cell::Cell;
use std::io::Write as _;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lightvm_desktop::LightVmApp;
use lightvm_desktop::render::{Attempt, DEGRADED_CONSOLE_FPS, Plan, RenderingInfo};
use lightvm_desktop::service::{Service, ServiceConfig};
use lightvm_desktop::settings::UiSettings;
use lightvm_proto::ipc::ParentLossPolicy;

#[derive(Debug)]
struct Args {
    smoke: Option<Duration>,
    renderer: Option<String>,
    animate: bool,
    #[cfg(feature = "dev-tools")]
    simulated_console: bool,
    wgpu_cpu: bool,
    glow_hw: String,
    skip_library_check: bool,
    service: ServiceArgs,
}

/// Parameters of the supervisor started by the UI (F1-11).
#[derive(Debug, Default)]
struct ServiceArgs {
    catalog: Option<String>,
    executable: Option<String>,
    qemu_dir: Option<String>,
    runtime_dir: Option<String>,
    audit_log: Option<String>,
    settings: Option<String>,
    /// `--no-supervisor` (`dev-tools` only): do not start the supervisor
    /// (smoke and measurements).
    no_supervisor: bool,
    /// `--open-vm`: VM to open at startup.
    open_vm: Option<String>,
    /// Changes to resources and limits for `--open-vm`.
    changes: lightvm_desktop::vm_settings::VmChanges,
    /// `--create`: create the `--open-vm` VM if it does not exist.
    create: bool,
}

/// Usage of the options of every build.
const USAGE: &str = "usage: lightvm-desktop [--renderer auto|glow|wgpu|warp|glow,warp...] \
     [--wgpu-adapter cpu|auto] [--glow-hw preferred|required|off] [--skip-library-check]\n    \
     [--catalog DIR [--service-exe PATH] [--qemu-dir DIR] [--runtime-dir DIR] [--audit-log FILE]] [--settings FILE]\n    \
     [--open-vm NAME [--create] [--vcpu N] [--ram MIB] [--disk GIB] [--cpu-max %] [--memory-margin MIB] [--disk-budget GIB|none]]";

/// Test and measurement switches (`dev-tools` builds only).
#[cfg(feature = "dev-tools")]
const DEV_USAGE: &str = "\n    [--smoke SECONDS] [--simulated-console] [--static] [--no-supervisor]   (dev-tools build)";
#[cfg(not(feature = "dev-tools"))]
const DEV_USAGE: &str = "";

fn parse_number<T: std::str::FromStr>(option: &str, v: Option<String>) -> Result<T, String> {
    let v = v.ok_or_else(|| format!("{option} requires a value"))?;
    v.parse()
        .map_err(|_| format!("{option}: invalid value: {v}"))
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        smoke: None,
        renderer: None,
        skip_library_check: false,
        animate: true,
        #[cfg(feature = "dev-tools")]
        simulated_console: false,
        wgpu_cpu: false,
        glow_hw: "preferred".to_owned(),
        service: ServiceArgs::default(),
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            #[cfg(feature = "dev-tools")]
            "--smoke" => {
                let v = it.next().ok_or("--smoke requires a number of seconds")?;
                let secs: f64 = v.parse().map_err(|_| format!("invalid seconds: {v}"))?;
                if !(secs.is_finite() && secs > 0.0 && secs <= 3600.0) {
                    return Err(format!("seconds out of range (0, 3600]: {v}"));
                }
                args.smoke = Some(Duration::from_secs_f64(secs));
            }
            #[cfg(feature = "dev-tools")]
            "--simulated-console" => args.simulated_console = true,
            "--renderer" => {
                args.renderer = Some(
                    it.next()
                        .ok_or("--renderer requires auto, glow, wgpu, warp or a list")?,
                );
            }
            "--skip-library-check" => args.skip_library_check = true,
            #[cfg(feature = "dev-tools")]
            "--static" => args.animate = false,
            "--wgpu-adapter" => {
                let v = it.next().ok_or("--wgpu-adapter requires cpu or auto")?;
                args.wgpu_cpu = match v.as_str() {
                    "cpu" => true,
                    "auto" => false,
                    _ => return Err(format!("unknown adapter: {v}")),
                };
            }
            "--glow-hw" => {
                let v = it
                    .next()
                    .ok_or("--glow-hw requires preferred, required or off")?;
                if !matches!(v.as_str(), "preferred" | "required" | "off") {
                    return Err(format!("unknown --glow-hw value: {v}"));
                }
                args.glow_hw = v;
            }
            "--catalog" => {
                args.service.catalog = Some(it.next().ok_or("--catalog requires a directory")?)
            }
            "--service-exe" => {
                args.service.executable = Some(it.next().ok_or("--service-exe requires a path")?)
            }
            "--qemu-dir" => {
                args.service.qemu_dir = Some(it.next().ok_or("--qemu-dir requires a directory")?)
            }
            "--runtime-dir" => {
                args.service.runtime_dir =
                    Some(it.next().ok_or("--runtime-dir requires a directory")?)
            }
            #[cfg(feature = "dev-tools")]
            "--no-supervisor" => args.service.no_supervisor = true,
            "--open-vm" => {
                args.service.open_vm = Some(it.next().ok_or("--open-vm requires the VM name")?)
            }
            "--create" => args.service.create = true,
            "--vcpu" => args.service.changes.vcpus = Some(parse_number("--vcpu", it.next())?),
            "--ram" => args.service.changes.memory_mib = Some(parse_number("--ram", it.next())?),
            "--disk" => args.service.changes.disk_gib = Some(parse_number("--disk", it.next())?),
            "--cpu-max" => {
                args.service.changes.cpu_max_percent = Some(parse_number("--cpu-max", it.next())?)
            }
            "--memory-margin" => {
                args.service.changes.memory_margin_mib =
                    Some(parse_number("--memory-margin", it.next())?)
            }
            "--disk-budget" => {
                let v = it.next().ok_or("--disk-budget requires GiB or none")?;
                args.service.changes.storage_budget_gib = Some(if v == "none" {
                    None
                } else {
                    Some(parse_number("--disk-budget", Some(v))?)
                });
            }
            "--settings" => {
                args.service.settings = Some(it.next().ok_or("--settings requires a file")?)
            }
            "--audit-log" => {
                args.service.audit_log = Some(it.next().ok_or("--audit-log requires a file")?)
            }
            "--help" | "-h" => {
                println!("{USAGE}{DEV_USAGE}");
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {other}")),
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
    clipboard_filter: lightvm_desktop::clipboard::ClipboardFilter,
}

impl eframe::App for DesktopApp {
    /// F1-13: first point of the app where the input is visible, before the
    /// egui context processes it. No `Paste` goes past this hook.
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        let outcome = self.clipboard_filter.filter_raw_input(raw_input);
        self.ui.count_discarded_pastes(outcome.discarded);
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
                if let Some((code, forced)) = self.ui.stop_service() {
                    println!("SMOKE service_stop code={code:?} forced={forced}");
                }
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        } else {
            // A single wake-up at the deadline: the smoke test ends even with `--static`.
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
        let _ = writeln!(out, "SMOKE outcome=ok");
        let _ = writeln!(out, "SMOKE renderer={}", r.detail);
        let failed: Vec<String> = r
            .failed
            .iter()
            .map(|(t, e)| format!("{}:{e}", t.name()))
            .collect();
        let _ = writeln!(
            out,
            "SMOKE backend={} degraded={} forced={} failed_attempts={} indicator=\"{}\"",
            r.active.map_or("?", Attempt::name),
            r.degraded(),
            r.forced,
            failed.len(),
            r.label(),
        );
        for f in &failed {
            let _ = writeln!(out, "SMOKE failed_attempt {f}");
        }
        #[allow(unused_mut, reason = "the backends present depend on the features")]
        let mut backends = String::new();
        #[cfg(feature = "wgpu")]
        backends.push_str(concat!(" wgpu=", env!("LIGHTVM_WGPU_VERSION")));
        #[cfg(feature = "glow")]
        backends.push_str(concat!(" glow=", env!("LIGHTVM_GLOW_VERSION")));
        let _ = writeln!(
            out,
            "SMOKE versions egui={} eframe={}{backends}",
            env!("LIGHTVM_EGUI_VERSION"),
            env!("LIGHTVM_EFRAME_VERSION"),
        );
        let _ = writeln!(
            out,
            "SMOKE window_points={:.0}x{:.0} pixels_per_point={ppp:.2} window_px={:.0}x{:.0} inner_rect={inner:?}",
            rect.width(),
            rect.height(),
            rect.width() * ppp,
            rect.height() * ppp,
        );
        let _ = writeln!(
            out,
            "SMOKE frame={} fb_updates={} duration_s={:.2} first_frame_ms={} discarded_pastes={}",
            self.ui.frames(),
            self.ui.fb_updates(),
            elapsed.as_secs_f64(),
            self.first_frame.map_or(-1, |d| d.as_millis() as i64),
            self.ui.discarded_pastes(),
        );
        let _ = writeln!(
            out,
            "SMOKE frame_intervals <2ms={} 2-20ms={} 20-90ms={} >=90ms={}",
            self.intervals[0], self.intervals[1], self.intervals[2], self.intervals[3]
        );
        if let Some(live) = self.ui.live_console() {
            let s = &live.stats;
            let (w, h) = live.geometry();
            let _ = writeln!(
                out,
                "SMOKE console vm=\"{}\" geometry={w}x{h} link=\"{}\" rfb_updates={} texture_uploads={} uploaded_pixels={} input_events={} resizes={}",
                live.target().vm_name,
                live.link().describe().0,
                s.rfb_updates,
                s.texture_uploads,
                s.uploaded_pixels,
                s.input_events,
                s.resizes
            );
        }
        if let Some(srv) = self.ui.service() {
            let st = srv.state();
            let _ = writeln!(
                out,
                "SMOKE service pid={} connected={} backend={} accelerator={} vms={} running_vms={} states={} last_outcome=\"{}\" connection=\"{}\"",
                srv.pid(),
                st.connected,
                st.backend.as_deref().unwrap_or("-"),
                st.accelerator.as_deref().unwrap_or("-"),
                st.vms.len(),
                st.vms
                    .iter()
                    .filter(|v| v.state == lightvm_proto::VmState::Running)
                    .count(),
                st.vms
                    .iter()
                    .map(|v| format!("{}:{}", v.name.as_str(), v.state.as_str()))
                    .collect::<Vec<_>>()
                    .join(","),
                st.last_outcome.as_deref().unwrap_or("-").replace('"', "'"),
                st.connection.replace('"', "'"),
            );
        }
        for (cause, n) in &self.repaint_causes {
            let _ = writeln!(out, "SMOKE repaint_cause n={n} {cause}");
        }
        let _ = out.flush();
    }
}

/// Description of the wgpu adapter and "CPU-type adapter" indication.
#[cfg(feature = "wgpu")]
fn wgpu_info(rs: &eframe::egui_wgpu::RenderState) -> (String, bool) {
    let i = rs.adapter.get_info();
    (
        format!(
            "wgpu backend={:?} adapter=\"{}\" type={:?} driver=\"{}\" driver_info=\"{}\"",
            i.backend, i.name, i.device_type, i.driver, i.driver_info
        ),
        i.device_type == eframe::egui_wgpu::wgpu::DeviceType::Cpu,
    )
}

/// GL version from the glow context, without `unsafe` (`HasContext::version`).
/// The GL renderer name (`glGetString(GL_RENDERER)`) would require an
/// `unsafe` call and is not reported.
#[cfg(feature = "glow")]
fn glow_info(gl: &eframe::glow::Context) -> (String, bool) {
    use eframe::glow::HasContext as _;
    let v = gl.version();
    let software = ["llvmpipe", "softpipe", "GDI Generic", "SwiftShader"]
        .iter()
        .any(|s| v.vendor_info.contains(s));
    (
        format!(
            "glow gl_version={}.{}{} driver=\"{}\"",
            v.major,
            v.minor,
            if v.is_embedded { " ES" } else { "" },
            v.vendor_info
        ),
        software,
    )
}

/// eframe options for an attempt.
fn options(t: Attempt, args: &Args) -> eframe::NativeOptions {
    #[allow(unused_mut, reason = "the modified fields depend on the features")]
    let mut options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("LightVM")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([880.0, 560.0]),
        // The same event loop serves several attempts: without
        // `run_and_return` winit does not allow recreating it after an
        // initialisation error.
        run_and_return: true,
        ..Default::default()
    };
    match t {
        #[cfg(feature = "glow")]
        Attempt::Glow => {
            options.renderer = eframe::Renderer::Glow;
            options.glow_options.hardware_acceleration = match args.glow_hw.as_str() {
                "required" => eframe::egui_glow::HardwareAcceleration::Required,
                "off" => eframe::egui_glow::HardwareAcceleration::Off,
                _ => eframe::egui_glow::HardwareAcceleration::Preferred,
            };
        }
        #[cfg(feature = "wgpu")]
        Attempt::WgpuHardware | Attempt::WgpuSoftware => {
            use eframe::egui_wgpu::{WgpuSetup, wgpu};
            options.renderer = eframe::Renderer::Wgpu;
            let software = t == Attempt::WgpuSoftware;
            if let WgpuSetup::CreateNew(create) = &mut options.wgpu_options.wgpu_setup {
                // ADR-0014: on Windows wgpu uses DX12 (hardware and WARP).
                if cfg!(windows) {
                    create.instance_descriptor.backends = wgpu::Backends::DX12;
                }
                // D-12: the default hint (`Performance`) preallocates large
                // memory blocks; for a UI with a single console texture the
                // footprint matters (target 250 MiB, sec. 13).
                let base = create.device_descriptor.clone();
                create.device_descriptor = Arc::new(move |adapter| wgpu::DeviceDescriptor {
                    memory_hints: wgpu::MemoryHints::MemoryUsage,
                    ..base(adapter)
                });
                create.native_adapter_selector = Some(Arc::new(move |adapters, _surface| {
                    for a in adapters {
                        let i = a.get_info();
                        lightvm_desktop::diag::log(&format!(
                            "wgpu adapter available: {:?} \"{}\" {:?}",
                            i.backend, i.name, i.device_type
                        ));
                    }
                    adapters
                        .iter()
                        .find(|a| (a.get_info().device_type == wgpu::DeviceType::Cpu) == software)
                        .cloned()
                        .ok_or_else(|| {
                            if software {
                                "no CPU-type (WARP) wgpu adapter available".to_owned()
                            } else {
                                "no hardware wgpu adapter available".to_owned()
                            }
                        })
                }));
            }
        }
        #[allow(unreachable_patterns, reason = "depends on the compiled features")]
        _ => {}
    }
    let _ = &args.glow_hw;
    options
}

/// Text of a caught panic.
fn panic_text(p: &(dyn std::any::Any + Send)) -> String {
    p.downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panic without message".to_owned())
}

/// Message for a panic during the start of the rendering or of the window.
fn error_from_panic(msg: &str) -> String {
    use lightvm_desktop::libraries::{Session, is_library_panic, message, missing};
    if !(cfg!(target_os = "linux") || is_library_panic(msg)) {
        return format!("LightVM stopped while starting the window: {msg}");
    }
    let session = Session::detect(
        std::env::var("WAYLAND_DISPLAY").ok().as_deref(),
        std::env::var("DISPLAY").ok().as_deref(),
    );
    #[cfg(target_os = "linux")]
    let m = {
        let cache = std::fs::read("/etc/ld.so.cache").unwrap_or_default();
        missing(session, |f| {
            lightvm_desktop::libraries::present_on_system(&cache, f)
        })
    };
    #[cfg(not(target_os = "linux"))]
    let m = missing(session, |_| true);
    message(session, &m, Some(msg))
}

fn main() -> std::process::ExitCode {
    let started = Instant::now();
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            lightvm_desktop::diag::log(&format!("error: {e}"));
            return std::process::ExitCode::from(2);
        }
    };
    // Diagnostic log: in release builds (GUI subsystem) stderr is usually
    // not visible, so the diagnostics also go to a rotated file.
    if let Some(dir) = lightvm_desktop::diag::default_dir() {
        match lightvm_desktop::diag::init(&dir) {
            Ok(path) => lightvm_desktop::diag::log(&format!(
                "LightVM {} starting; diagnostic log {}",
                env!("CARGO_PKG_VERSION"),
                path.display()
            )),
            Err(e) => eprintln!("diagnostic log not available in {}: {e}", dir.display()),
        }
    }
    // Settings before the rendering plan: the saved preference applies if
    // `--renderer` is not given. Without `--settings`, the file of earlier
    // versions is read if the new one does not exist yet (saving always
    // writes the new one).
    let settings_path = args
        .service
        .settings
        .as_ref()
        .map(PathBuf::from)
        .or_else(lightvm_desktop::settings::default_path);
    let legacy_path = if args.service.settings.is_none() {
        lightvm_desktop::settings::legacy_default_path()
    } else {
        None
    };
    let (settings, settings_error) = match (&settings_path, &legacy_path) {
        (Some(p), Some(l)) => UiSettings::load_or_legacy(p, l),
        (Some(p), None) => UiSettings::load(p),
        (None, _) => (UiSettings::default(), None),
    };
    if let Some(e) = &settings_error {
        lightvm_desktop::diag::log(&format!("settings: {e}"));
    }
    let renderer = args
        .renderer
        .clone()
        .or_else(|| Some(settings.rendering.as_argument().to_owned()));
    let plan = match Plan::from_args(renderer.as_deref(), args.wgpu_cpu) {
        Ok(p) => p,
        Err(e) => {
            lightvm_desktop::diag::log(&format!("error: {e}"));
            return std::process::ExitCode::from(2);
        }
    };

    // F1-14: on Linux, before creating the window, check the libraries that
    // winit/glutin/wgpu open with `dlopen`.
    #[cfg(target_os = "linux")]
    if !args.skip_library_check
        && let Err(msg) = lightvm_desktop::libraries::preflight_check()
    {
        lightvm_desktop::diag::log(&format!("error: {msg}"));
        println!("SMOKE outcome=error detail=\"missing system libraries\"");
        return std::process::ExitCode::from(3);
    }
    #[cfg(not(target_os = "linux"))]
    let _ = args.skip_library_check;

    if args.service.open_vm.is_none() && (!args.service.changes.is_empty() || args.service.create) {
        lightvm_desktop::diag::log(
            "error: --create, --vcpu, --ram, --disk, --cpu-max, --memory-margin and --disk-budget require --open-vm",
        );
        return std::process::ExitCode::from(2);
    }
    #[cfg(feature = "dev-tools")]
    if args.service.open_vm.is_some() && args.service.no_supervisor {
        lightvm_desktop::diag::log(
            "error: --open-vm requires the supervisor (incompatible with --no-supervisor)",
        );
        return std::process::ExitCode::from(2);
    }
    let service = match service_config(&args.service, settings.ui_loss_policy) {
        Ok(c) => c,
        Err(e) => {
            lightvm_desktop::diag::log(&format!("error: {e}"));
            return std::process::ExitCode::from(2);
        }
    };

    // `--smoke` measures the simulated framebuffer, as before the flag existed.
    #[cfg(feature = "dev-tools")]
    let simulated_console = args.simulated_console || args.smoke.is_some();

    // Panics due to missing libraries become a usable message (below): the
    // default hook stays for all the others.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let msg = panic_text(info.payload());
        let at = info
            .location()
            .map(|l| format!(" at {}:{}", l.file(), l.line()))
            .unwrap_or_default();
        lightvm_desktop::diag::log(&format!("panic{at}: {msg}"));
        if !lightvm_desktop::libraries::is_library_panic(&msg) {
            default_hook(info);
        }
    }));

    let mut failed: Vec<(Attempt, String)> = Vec::new();
    let n = plan.attempts.len();
    for (index, &t) in plan.attempts.iter().enumerate() {
        let created = Rc::new(Cell::new(false));
        let info_base = RenderingInfo {
            active: Some(t),
            detail: String::new(),
            cpu_adapter: false,
            failed: failed.clone(),
            forced: plan.forced,
        };
        let service = service.clone();
        let settings_path = settings_path.clone();
        let settings_error = settings_error.clone();
        let settings = settings.clone();
        let service_exe = args.service.executable.as_ref().map(PathBuf::from);
        let no_service = args.service.no_supervisor;
        let open_vm = args.service.open_vm.clone();
        let changes = args.service.changes;
        let create = args.service.create;
        let animate = args.animate;
        let smoke = args.smoke;
        let created_app = created.clone();
        let options = options(t, &args);
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            eframe::run_native(
                "LightVM",
                options,
                Box::new(move |cc| {
                    created_app.set(true);
                    let mut info = info_base;
                    #[cfg(feature = "wgpu")]
                    if let Some(rs) = &cc.wgpu_render_state {
                        (info.detail, info.cpu_adapter) = wgpu_info(rs);
                    }
                    #[cfg(feature = "glow")]
                    if let Some(gl) = &cc.gl {
                        (info.detail, info.cpu_adapter) = glow_info(gl);
                    }
                    let mut ui = LightVmApp::new(animate);
                    if info.degraded() {
                        ui.set_console_max_fps(DEGRADED_CONSOLE_FPS);
                    }
                    #[cfg(feature = "dev-tools")]
                    {
                        ui.set_simulated_console(simulated_console);
                        if simulated_console {
                            // The simulated framebuffer is what is being measured:
                            // open on the Console page, as before the Dashboard
                            // became the start page.
                            ui.set_section(lightvm_desktop::Section::Console);
                        }
                    }
                    lightvm_desktop::diag::log(&format!(
                        "rendering: {}",
                        info.explanation().replace('\n', "; ")
                    ));
                    ui.rendering = info;
                    let start_from_settings = settings.start_supervisor;
                    ui.set_settings(settings, settings_path, settings_error);
                    ui.set_service_exe(service_exe);
                    ui.set_folder_picker(lightvm_desktop::native_folder_picker());
                    ui.set_file_picker(lightvm_desktop::native_file_picker());
                    ui.set_vmx_picker(lightvm_desktop::native_vmx_picker());
                    ui.set_save_picker(lightvm_desktop::native_save_picker());
                    ui.set_report_picker(lightvm_desktop::native_report_picker());
                    // Style and theme before the first frame (no frame with the
                    // default egui style).
                    lightvm_desktop::theme::install(&cc.egui_ctx);
                    cc.egui_ctx.set_theme(egui::Theme::from(ui.theme()));
                    // The supervisor starts only once the window exists: failed
                    // rendering attempts leave no services running. With
                    // `--catalog` the command line applies, otherwise the
                    // Settings page (automatic start by default).
                    if let Some(cfg) = &service {
                        ui.set_command_line_config(cfg.clone());
                        let ctx = cc.egui_ctx.clone();
                        match Service::start(cfg, Arc::new(move || ctx.request_repaint())) {
                            Ok(s) => {
                                lightvm_desktop::diag::log(&format!(
                                    "service: lightvm-service started (PID {})",
                                    s.pid()
                                ));
                                ui.set_service(s, cfg);
                            }
                            Err(e) => {
                                lightvm_desktop::diag::log(&format!("service: {e}"));
                                ui.set_service_error(e.to_string());
                            }
                        }
                    } else if (start_from_settings || open_vm.is_some()) && !no_service {
                        ui.start_service(&cc.egui_ctx);
                        if let Some(s) = ui.service() {
                            lightvm_desktop::diag::log(&format!(
                                "service: lightvm-service started (PID {})",
                                s.pid()
                            ));
                        }
                    }
                    if let Some(name) = open_vm {
                        ui.open_vm(name, changes, create);
                    }
                    Ok(Box::new(DesktopApp {
                        ui,
                        started,
                        first_frame: None,
                        smoke,
                        reported: false,
                        repaint_causes: Default::default(),
                        last_frame: None,
                        intervals: [0; 4],
                        clipboard_filter: Default::default(),
                    }))
                }),
            )
        }));
        match outcome {
            Ok(Ok(())) => return std::process::ExitCode::SUCCESS,
            Ok(Err(e)) if !created.get() && index + 1 < n => {
                let e = e.to_string();
                lightvm_desktop::diag::log(&format!(
                    "rendering: attempt {} failed ({e}); falling back to {}",
                    t.name(),
                    plan.attempts[index + 1].name()
                ));
                failed.push((t, e));
            }
            Ok(Err(e)) => {
                failed.push((t, e.to_string()));
                let list: Vec<String> = failed
                    .iter()
                    .map(|(t, e)| format!("{}: {e}", t.name()))
                    .collect();
                println!(
                    "SMOKE outcome=error detail=\"{}\"",
                    list.join(" | ").replace('"', "'")
                );
                lightvm_desktop::diag::log(&format!(
                    "error: no usable rendering backend ({})",
                    list.join("; ")
                ));
                return std::process::ExitCode::FAILURE;
            }
            Err(p) => {
                let msg = panic_text(p.as_ref());
                lightvm_desktop::diag::log(&format!("error: {}", error_from_panic(&msg)));
                println!(
                    "SMOKE outcome=error detail=\"panic at startup: {}\"",
                    msg.replace('"', "'")
                );
                return std::process::ExitCode::from(3);
            }
        }
    }
    std::process::ExitCode::FAILURE
}

/// Supervisor configuration, if requested with `--catalog`.
fn service_config(
    a: &ServiceArgs,
    policy: ParentLossPolicy,
) -> Result<Option<ServiceConfig>, String> {
    let Some(catalog) = &a.catalog else {
        if a.executable.is_some()
            || a.qemu_dir.is_some()
            || a.runtime_dir.is_some()
            || a.audit_log.is_some()
        {
            return Err(
                "--service-exe, --qemu-dir, --runtime-dir and --audit-log require --catalog".into(),
            );
        }
        return Ok(None);
    };
    let executable = match &a.executable {
        Some(p) => PathBuf::from(p),
        None => ServiceConfig::executable_next_to_ui()
            .ok_or("path of the UI executable not available: use --service-exe")?,
    };
    Ok(Some(ServiceConfig {
        executable,
        catalog: PathBuf::from(catalog),
        qemu_dir: a.qemu_dir.as_ref().map(PathBuf::from),
        runtime_dir: a.runtime_dir.as_ref().map(PathBuf::from),
        audit_log: a.audit_log.as_ref().map(PathBuf::from),
        policy,
        pairing_dir: None,
    }))
}
