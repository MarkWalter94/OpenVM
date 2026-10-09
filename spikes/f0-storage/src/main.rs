//! Spike F0-05: ciclo baseline/overlay/firmware a VM spenta, esclusione dei
//! dischi e prove di crash sullo storage. Log in `lab/logs/f0-05-*`.

#[cfg(not(windows))]
fn main() {
    eprintln!("f0-storage: spike solo Windows");
}

#[cfg(windows)]
fn main() {
    if let Err(e) = imp::run() {
        eprintln!("errore: {e}");
        std::process::exit(2);
    }
}

#[cfg(windows)]
mod imp {
    use std::fs::{File, OpenOptions};
    use std::io::{self, Read};
    use std::os::windows::fs::OpenOptionsExt;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    use f0_supervisor::qmp::Qmp;
    use f0_supervisor::win::ConsoleMode;
    use f0_supervisor::{
        EnvProfile, Job, JobLimits, Log, Vm, VmConfig, ppm_nonblack, qemu_img, qemu_io,
        qemu_residuals, stats,
    };
    use serde_json::json;
    use sha2::{Digest, Sha256};

    const ROOT: &str = r"E:\OpenVM\lab\vms\f0-05";
    const LOGS: &str = r"E:\OpenVM\lab\logs";
    const PRIV: &str = r"E:\OpenVM\lab\vms\f0-05\priv";
    const FILE_SHARE_READ: u32 = 1;

    pub(crate) fn run() -> io::Result<()> {
        let argv: Vec<String> = std::env::args().collect();
        std::fs::create_dir_all(PRIV)?;
        let arg = |k: &str, d: u64| -> u64 {
            argv.iter()
                .position(|a| a == k)
                .and_then(|i| argv.get(i + 1))
                .and_then(|v| v.parse().ok())
                .unwrap_or(d)
        };
        match argv.get(1).map(String::as_str) {
            Some("baseline-cycles") => baseline_cycles(arg("--cycles", 5), arg("--boot-s", 60)),
            Some("guest-write-crash") => {
                guest_write_crash(arg("--boot-s", 135), arg("--grow-mib", 256))
            }
            Some("convert-crash") => convert_crash(),
            _ => {
                eprintln!("uso: f0-storage <baseline-cycles|guest-write-crash|convert-crash>");
                Ok(())
            }
        }
    }

    fn sha256(p: &Path) -> io::Result<String> {
        let mut f = File::open(p)?;
        let mut h = Sha256::new();
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            h.update(&buf[..n]);
        }
        Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
    }

    fn short(h: &str) -> &str {
        &h[..16.min(h.len())]
    }

    fn tool(dir: &Path, exe: &Path, args: &[&str]) -> (Option<i32>, String) {
        match Command::new(exe).args(args).current_dir(dir).output() {
            Ok(o) => (
                o.status.code(),
                format!(
                    "{} {}",
                    String::from_utf8_lossy(&o.stdout).trim(),
                    String::from_utf8_lossy(&o.stderr).trim()
                )
                .replace(['\r', '\n'], " ")
                .trim()
                .to_string(),
            ),
            Err(e) => (None, format!("spawn fallito: {e}")),
        }
    }

    fn guard(p: &Path) -> io::Result<File> {
        OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(p)
    }

    fn start_vm(tag: &str, overlay: &Path, vars: &Path) -> io::Result<(Vm, Qmp, PathBuf)> {
        let sock = Path::new(PRIV).join(format!("{tag}.sock"));
        let cfg = VmConfig {
            tag: tag.into(),
            mem_mb: 4096,
            smp: 2,
            iso: true,
            overlay: Some(overlay.to_path_buf()),
            vars: Some(vars.to_path_buf()),
            qmp_sock: sock.clone(),
            paused: false,
            env: EnvProfile::SystemRootOnly,
            console: ConsoleMode::NoWindow,
        };
        let mut job = Job::create(JobLimits::default())?;
        job.attach_port()?;
        let vm = Vm::start(&cfg, job, Path::new(LOGS))?;
        let q = Qmp::connect(&sock, Duration::from_secs(10))?;
        Ok((vm, q, sock))
    }

    fn screendump(q: &mut Qmp, tag: &str, keep: bool) -> Option<(u32, u32, f64, u64)> {
        let shot = Path::new(PRIV).join(format!("{tag}.ppm"));
        q.execute("screendump", Some(json!({"filename": shot.to_str()})))
            .ok()?;
        let r = ppm_nonblack(&shot).ok();
        let size = std::fs::metadata(&shot).map(|m| m.len()).unwrap_or(0);
        if !keep {
            let _ = std::fs::remove_file(&shot);
        }
        r.map(|(w, h, p)| (w, h, p, size))
    }

    /// Arresto ordinato per questa prova: `quit` (nessun guest da salvare)
    /// con ripiego su Emergency Stop.
    fn stop_vm(vm: &Vm, q: &mut Qmp, log: &mut Log) -> io::Result<()> {
        let t = Instant::now();
        let r = q.execute("quit", None);
        let code = vm.wait(Duration::from_secs(10))?;
        log.line(&format!(
            "  quit: risposta={:?} uscita={code:?} in {:.0} ms",
            r.is_ok(),
            t.elapsed().as_secs_f64() * 1000.0
        ));
        if code.is_none() {
            let (p, _) = vm.emergency_stop(Duration::from_secs(10))?;
            log.line(&format!("  quit senza uscita: Emergency Stop in {p:.0} ms"));
        }
        Ok(())
    }

    fn new_run(
        dir: &Path,
        base_rel: &str,
        bvars: &Path,
        log: &mut Log,
    ) -> io::Result<(PathBuf, PathBuf)> {
        std::fs::create_dir_all(dir)?;
        let ovl = dir.join("overlay.qcow2");
        let vars = dir.join("vars.fd");
        let r = tool(
            dir,
            &qemu_img(),
            &[
                "create",
                "-q",
                "-f",
                "qcow2",
                "-b",
                base_rel,
                "-F",
                "qcow2",
                "overlay.qcow2",
            ],
        );
        log.line(&format!(
            "  qemu-img create overlay (backing {base_rel}): {r:?}"
        ));
        std::fs::copy(bvars, &vars)?;
        Ok((ovl, vars))
    }

    // -----------------------------------------------------------------------
    // Ciclo baseline/overlay/firmware.
    // -----------------------------------------------------------------------
    fn baseline_cycles(cycles: u64, boot_s: u64) -> io::Result<()> {
        let mut log = Log::create(&Path::new(LOGS).join("f0-05-baseline-cycles.log"))?;
        let root = Path::new(ROOT).join("bl");
        let _ = std::fs::remove_dir_all(&root);
        let bdir = root.join("baseline");
        std::fs::create_dir_all(&bdir)?;
        let base = bdir.join("base.qcow2");
        let bvars = bdir.join("vars.fd");
        let r = tool(
            &bdir,
            &qemu_img(),
            &["create", "-q", "-f", "qcow2", "base.qcow2", "20G"],
        );
        log.line(&format!("baseline: qemu-img create base.qcow2 20G: {r:?}"));
        std::fs::copy(f0_supervisor::fw_vars_template(), &bvars)?;
        let hb = sha256(&base)?;
        let hv = sha256(&bvars)?;
        let manifest = json!({
            "versione": 1,
            "base": {"file": "baseline/base.qcow2", "sha256": hb, "byte": std::fs::metadata(&base)?.len()},
            "firmware_vars": {"file": "baseline/vars.fd", "sha256": hv, "origine": "share/edk2-i386-vars.fd"},
            "firmware_code": {"file": f0_supervisor::fw_code().to_string_lossy(), "sha256": sha256(&f0_supervisor::fw_code())?},
        });
        std::fs::write(
            bdir.join("manifest.json"),
            serde_json::to_string_pretty(&manifest)?,
        )?;
        log.line(&format!("baseline: sha256 base={hb} vars={hv}"));
        // Il supervisor tiene aperti i file della baseline con FILE_SHARE_READ
        // per tutta la serie: nessuno può scriverli, rinominarli o cancellarli.
        let gb = guard(&base)?;
        let gv = guard(&bvars)?;
        let mut stop_ms = Vec::new();
        for i in 1..=cycles {
            log.line(&format!("== ciclo {i}"));
            let rdir = root.join(format!("run-{i}"));
            let (ovl, vars) = new_run(&rdir, r"..\baseline\base.qcow2", &bvars, &mut log)?;
            let (_, chain) = tool(
                &rdir,
                &qemu_img(),
                &["info", "--backing-chain", "overlay.qcow2"],
            );
            log.line(&format!(
                "  catena: {}",
                chain
                    .split_whitespace()
                    .filter(|w| w.contains(".qcow2"))
                    .collect::<Vec<_>>()
                    .join(" ")
            ));
            let ho0 = sha256(&ovl)?;
            let hr0 = sha256(&vars)?;
            log.line(&format!(
                "  pre-boot: overlay={} vars_esecuzione={} (uguale alla baseline: {})",
                short(&ho0),
                short(&hr0),
                hr0 == hv
            ));
            let tag = format!("f0-05-bl-{i}");
            let (vm, mut q, sock) = start_vm(&tag, &ovl, &vars)?;
            std::thread::sleep(Duration::from_secs(boot_s));
            let shot = screendump(&mut q, &tag, i == 1);
            let blk = q.execute("query-block", None).ok();
            let disk = blk
                .as_ref()
                .and_then(|b| b.as_array())
                .and_then(|a| a.iter().find(|d| d["inserted"]["node-name"] == "ovl"))
                .map(|d| {
                    format!(
                        "ro={} backing={}",
                        d["inserted"]["ro"], d["inserted"]["image"]["backing-filename"]
                    )
                });
            log.line(&format!(
                "  dopo {boot_s}s: schermo={shot:?} disco={disk:?}"
            ));
            let t = Instant::now();
            stop_vm(&vm, &mut q, &mut log)?;
            stop_ms.push(t.elapsed().as_secs_f64() * 1000.0);
            drop(q);
            drop(vm);
            let _ = std::fs::remove_file(&sock);
            let hb1 = sha256(&base)?;
            let hv1 = sha256(&bvars)?;
            let ho1 = sha256(&ovl)?;
            let hr1 = sha256(&vars)?;
            log.line(&format!(
                "  post-stop: base invariata={} vars_baseline invariato={} overlay cambiato={} ({}) vars_esecuzione cambiato={} ({}) overlay_byte={}",
                hb1 == hb,
                hv1 == hv,
                ho1 != ho0,
                short(&ho1),
                hr1 != hr0,
                short(&hr1),
                std::fs::metadata(&ovl)?.len()
            ));
            if hr1 != hr0 {
                let a = std::fs::read(&bvars)?;
                let b = std::fs::read(&vars)?;
                let diff = a.iter().zip(&b).filter(|(x, y)| x != y).count();
                log.line(&format!("  vars: {diff} byte diversi su {}", a.len()));
            }
            let r = tool(&rdir, &qemu_img(), &["check", "overlay.qcow2"]);
            log.line(&format!("  qemu-img check overlay: {r:?}"));
            // Scrittura dimostrativa a VM spenta (il guest live non scrive sul disco).
            let r = tool(
                &rdir,
                &qemu_io(),
                &["-f", "qcow2", "-c", "write -P 0x5a 0 1M", "overlay.qcow2"],
            );
            log.line(&format!("  qemu-io write 1 MiB 0x5a nell'overlay: {r:?}"));
            let r = tool(
                &rdir,
                &qemu_io(),
                &[
                    "-r",
                    "-f",
                    "qcow2",
                    "-c",
                    "read -P 0x5a 0 1M",
                    "overlay.qcow2",
                ],
            );
            log.line(&format!("  qemu-io -r read -P 0x5a: {r:?}"));
            log.line(&format!(
                "  base invariata dopo la scrittura nell'overlay={}",
                sha256(&base)? == hb
            ));
            // Ripristino: scarta overlay e vars dell'esecuzione, ricrea dalla baseline.
            std::fs::remove_file(&ovl)?;
            std::fs::remove_file(&vars)?;
            let (ovl2, vars2) = new_run(&rdir, r"..\baseline\base.qcow2", &bvars, &mut log)?;
            let r = tool(
                &rdir,
                &qemu_io(),
                &["-r", "-f", "qcow2", "-c", "read -P 0 0 1M", "overlay.qcow2"],
            );
            log.line(&format!(
                "  ripristino: overlay nuovo legge zeri come la base: {r:?}; vars nuovo = baseline: {}",
                sha256(&vars2)? == hv
            ));
            log.line(&format!(
                "  ripristino: overlay nuovo identico all'overlay pre-boot: {}",
                sha256(&ovl2)? == ho0
            ));
        }
        drop(gb);
        drop(gv);
        log.line(&format!(
            "RIEPILOGO: base finale invariata={} vars baseline invariato={} stop (ms): {}",
            sha256(&base)? == hb,
            sha256(&bvars)? == hv,
            stats(&stop_ms).map(|s| s.to_string()).unwrap_or_default()
        ));
        let (t, m) = qemu_residuals("lightvm-f0-05");
        log.line(&format!("residui_spike={m:?} qemu_totali_host={t}"));
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Kill di QEMU mentre il guest scrive nell'overlay.
    // -----------------------------------------------------------------------
    fn guest_write_crash(boot_s: u64, grow_mib: u64) -> io::Result<()> {
        let mut log = Log::create(&Path::new(LOGS).join("f0-05-guest-write-crash.log"))?;
        let root = Path::new(ROOT).join("gw");
        let _ = std::fs::remove_dir_all(&root);
        let bdir = root.join("baseline");
        std::fs::create_dir_all(&bdir)?;
        let base = bdir.join("base.qcow2");
        let bvars = bdir.join("vars.fd");
        tool(
            &bdir,
            &qemu_img(),
            &["create", "-q", "-f", "qcow2", "base.qcow2", "20G"],
        );
        // Contenuto riconoscibile nella base per verificare che resti intatta.
        tool(
            &bdir,
            &qemu_io(),
            &["-f", "qcow2", "-c", "write -P 0x11 0 64M", "base.qcow2"],
        );
        std::fs::copy(f0_supervisor::fw_vars_template(), &bvars)?;
        let hb = sha256(&base)?;
        log.line(&format!("base 20G con 64 MiB 0x11: sha256={hb}"));
        let gb = guard(&base)?;
        let rdir = root.join("run");
        let (ovl, vars) = new_run(&rdir, r"..\baseline\base.qcow2", &bvars, &mut log)?;
        let tag = "f0-05-gw";
        let (vm, mut q, sock) = start_vm(tag, &ovl, &vars)?;
        std::thread::sleep(Duration::from_secs(boot_s));
        log.line(&format!(
            "desktop? schermo={:?}",
            screendump(&mut q, &format!("{tag}-1"), true)
        ));
        // Apre un terminale nel live (Ctrl+Alt+T) e lancia dd sul disco di prova.
        q.send_keys(&["ctrl", "alt", "t"])?;
        std::thread::sleep(Duration::from_secs(8));
        log.line(&format!(
            "dopo Ctrl+Alt+T schermo={:?}",
            screendump(&mut q, &format!("{tag}-2"), true)
        ));
        // /dev/vda è il disco virtio-blk di prova collegato all'overlay (nessun disco reale).
        let cmd =
            "sudo dd if=/dev/urandom of=/dev/vda bs=1M count=8000 oflag=direct status=progress\n";
        q.type_text(cmd)?;
        log.line(&format!("digitato: {}", cmd.trim()));
        let size0 = std::fs::metadata(&ovl)?.len();
        let t0 = Instant::now();
        let mut killed = false;
        while t0.elapsed() < Duration::from_secs(90) {
            std::thread::sleep(Duration::from_millis(250));
            let s = std::fs::metadata(&ovl)?.len();
            if s > size0 + (grow_mib << 20) {
                log.line(&format!(
                    "overlay cresciuto da {size0} a {s} byte in {:.1}s: Emergency Stop durante la scrittura",
                    t0.elapsed().as_secs_f64()
                ));
                let _ = screendump(&mut q, &format!("{tag}-3"), true);
                let (p, j) = vm.emergency_stop(Duration::from_secs(10))?;
                log.line(&format!(
                    "Emergency Stop: ms_uscita_qemu={p:.1} ms_job_vuoto={j:.1}"
                ));
                killed = true;
                break;
            }
        }
        if !killed {
            log.line(&format!(
                "overlay non cresciuto oltre {grow_mib} MiB entro 90 s (dimensione {} byte): schermo={:?}",
                std::fs::metadata(&ovl)?.len(),
                screendump(&mut q, &format!("{tag}-3"), true)
            ));
            vm.emergency_stop(Duration::from_secs(10))?;
        }
        drop(q);
        drop(vm);
        drop(gb);
        let _ = std::fs::remove_file(&sock);
        let r = tool(&rdir, &qemu_img(), &["check", "overlay.qcow2"]);
        log.line(&format!("qemu-img check overlay dopo il kill: {r:?}"));
        let r = tool(&rdir, &qemu_img(), &["info", "overlay.qcow2"]);
        log.line(&format!("qemu-img info overlay: {r:?}"));
        log.line(&format!("base invariata={}", sha256(&base)? == hb));
        let r = tool(
            &bdir,
            &qemu_io(),
            &[
                "-r",
                "-f",
                "qcow2",
                "-c",
                "read -P 0x11 0 64M",
                "base.qcow2",
            ],
        );
        log.line(&format!("contenuto base 0x11 verificato: {r:?}"));
        let (t, m) = qemu_residuals("lightvm-f0-05");
        log.line(&format!("residui_spike={m:?} qemu_totali_host={t}"));
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Kill di qemu-img durante convert.
    // -----------------------------------------------------------------------
    fn convert_crash() -> io::Result<()> {
        let mut log = Log::create(&Path::new(LOGS).join("f0-05-convert-crash.log"))?;
        let dir = Path::new(ROOT).join("cv");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;
        tool(
            &dir,
            &qemu_img(),
            &["create", "-q", "-f", "qcow2", "src.qcow2", "8G"],
        );
        for k in 0..8u32 {
            let c = format!("write -P 0x{:02x} {}M 256M", 0x21 + k, k * 256);
            let r = tool(&dir, &qemu_io(), &["-f", "qcow2", "-c", &c, "src.qcow2"]);
            log.line(&format!("src: {c}: exit={:?}", r.0));
        }
        let src = dir.join("src.qcow2");
        let hs = sha256(&src)?;
        log.line(&format!(
            "src: 8G virtuali, 2 GiB scritti, {} byte su disco, sha256={hs}",
            std::fs::metadata(&src)?.len()
        ));
        // Riferimento: convert completo.
        let t = Instant::now();
        let r = tool(
            &dir,
            &qemu_img(),
            &["convert", "-O", "qcow2", "src.qcow2", "full.qcow2"],
        );
        let full_ms = t.elapsed().as_secs_f64() * 1000.0;
        log.line(&format!("convert completo: {r:?} in {full_ms:.0} ms"));
        let r = tool(&dir, &qemu_img(), &["compare", "src.qcow2", "full.qcow2"]);
        log.line(&format!("compare src/full: {r:?}"));
        let _ = std::fs::remove_file(dir.join("full.qcow2"));
        for (i, frac) in [0.05f64, 0.15, 0.3, 0.5, 0.75].iter().enumerate() {
            let delay = (full_ms * frac).max(20.0) as u64;
            let name = format!("dst-{}.qcow2.partial", i + 1);
            let mut ch = Command::new(qemu_img())
                .args(["convert", "-O", "qcow2", "src.qcow2", &name])
                .current_dir(&dir)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()?;
            std::thread::sleep(Duration::from_millis(delay));
            let still = ch.try_wait()?.is_none();
            ch.kill()?;
            let st = ch.wait()?;
            let dst = dir.join(&name);
            let size = std::fs::metadata(&dst).map(|m| m.len()).ok();
            log.line(&format!(
                "[{}] kill dopo {delay} ms (vivo al kill={still}) uscita={:?} file={name} byte={size:?}",
                i + 1,
                st.code()
            ));
            let r = tool(&dir, &qemu_img(), &["check", "-f", "qcow2", &name]);
            log.line(&format!("  qemu-img check: {r:?}"));
            let r = tool(&dir, &qemu_img(), &["info", "-f", "qcow2", &name]);
            log.line(&format!(
                "  qemu-img info: {}",
                r.1.chars().take(300).collect::<String>()
            ));
            let r = tool(
                &dir,
                &qemu_img(),
                &["compare", "-f", "qcow2", "-F", "qcow2", "src.qcow2", &name],
            );
            log.line(&format!("  compare con src: {r:?}"));
            log.line(&format!("  src invariata={}", sha256(&src)? == hs));
        }
        Ok(())
    }
}
