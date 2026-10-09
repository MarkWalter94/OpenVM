//! Driver dello spike F0-06/F0-08. Ogni sottocomando è una prova ripetibile;
//! i log vanno in `lab/logs/f0-06-*` e `lab/logs/f0-08-*`.

#[cfg(not(windows))]
fn main() {
    eprintln!("f0-supervisor: spike solo Windows");
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
    use std::collections::HashMap;
    use std::io::{self, BufRead, BufReader};
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    use f0_supervisor::win::{self, ConsoleMode};
    use f0_supervisor::{
        EnvProfile, IdentityCheck, Job, JobLimits, Log, Vm, VmConfig, qemu_residuals, stats,
    };
    use serde_json::json;

    const VMDIR: &str = r"E:\OpenVM\lab\vms\f0-06";
    const LOGS: &str = r"E:\OpenVM\lab\logs";
    const PRIV: &str = r"E:\OpenVM\lab\vms\f0-06\priv";
    const MARKER: &str = "lightvm-f0-0[568]";

    struct Args(HashMap<String, String>);
    impl Args {
        fn get<T: std::str::FromStr>(&self, k: &str, d: T) -> T {
            self.0.get(k).and_then(|v| v.parse().ok()).unwrap_or(d)
        }
        fn s(&self, k: &str, d: &str) -> String {
            self.0.get(k).cloned().unwrap_or_else(|| d.to_string())
        }
    }

    fn parse(args: &[String]) -> Args {
        let mut m = HashMap::new();
        let mut i = 0;
        while i < args.len() {
            if let Some(k) = args[i].strip_prefix("--") {
                let v = args.get(i + 1).cloned().unwrap_or_default();
                m.insert(k.to_string(), v);
                i += 2;
            } else {
                i += 1;
            }
        }
        Args(m)
    }

    pub(crate) fn run() -> io::Result<()> {
        let argv: Vec<String> = std::env::args().collect();
        let cmd = argv.get(1).cloned().unwrap_or_default();
        let a = parse(&argv[2.min(argv.len())..]);
        std::fs::create_dir_all(VMDIR)?;
        std::fs::create_dir_all(PRIV)?;
        match cmd.as_str() {
            "env-probe" => env_probe(),
            "limits-probe" => limits_probe(),
            "supervise" => supervise(&a),
            "scenario-a" => scenario_a(&a),
            "scenario-b" => scenario_b(&a),
            "scenario-c" => scenario_c(&a),
            "scenario-d" => scenario_d(&a),
            "pid-reuse" => pid_reuse(&a),
            "mem-limit" => mem_limit(&a),
            "cpu-cap" => cpu_cap(&a),
            "exclusion" => exclusion(&a),
            _ => {
                eprintln!(
                    "uso: f0-supervisor <env-probe|limits-probe|scenario-a|scenario-b|scenario-c|scenario-d|pid-reuse|mem-limit|cpu-cap|exclusion> [--opzione valore]"
                );
                Ok(())
            }
        }
    }

    fn vars_copy(tag: &str) -> io::Result<PathBuf> {
        let p = Path::new(VMDIR).join(format!("{tag}-vars.fd"));
        std::fs::copy(f0_supervisor::fw_vars_template(), &p)?;
        Ok(p)
    }

    fn cfg(tag: &str, iso: bool) -> io::Result<VmConfig> {
        Ok(VmConfig {
            tag: tag.to_string(),
            mem_mb: 4096,
            smp: 2,
            iso,
            overlay: None,
            vars: if iso { Some(vars_copy(tag)?) } else { None },
            qmp_sock: Path::new(PRIV).join(format!("{tag}.sock")),
            paused: false,
            env: EnvProfile::SystemRootOnly,
            console: ConsoleMode::NoWindow,
        })
    }

    fn new_job() -> io::Result<Job> {
        let mut j = Job::create(JobLimits::default())?;
        j.attach_port()?;
        Ok(j)
    }

    fn residual_line(log: &mut Log, ctx: &str) -> usize {
        // Breve attesa perché la lista processi rifletta l'uscita.
        std::thread::sleep(Duration::from_millis(200));
        let (total, mine) = qemu_residuals(MARKER);
        log.line(&format!(
            "{ctx}: residui_spike={} qemu_totali_host={total}",
            mine.map(|m| m.to_string()).unwrap_or_else(|| "?".into())
        ));
        mine.unwrap_or(usize::MAX)
    }

    fn cleanup_sock(cfg: &VmConfig) {
        let _ = std::fs::remove_file(&cfg.qmp_sock);
        if let Some(v) = &cfg.vars {
            let _ = std::fs::remove_file(v);
        }
    }

    // -----------------------------------------------------------------------
    // env-probe: ambiente minimo, console, appartenenza al job, figli.
    // -----------------------------------------------------------------------
    fn env_probe() -> io::Result<()> {
        let mut log = Log::create(&Path::new(LOGS).join("f0-06-env-probe.log"))?;
        log.line(&format!(
            "supervisor (questo processo) già in un job: {:?}",
            win::current_in_job()
        ));
        for env in [EnvProfile::Empty, EnvProfile::SystemRootOnly] {
            for console in [ConsoleMode::NoWindow, ConsoleMode::Detached] {
                let tag = format!("f0-06-env-{env:?}-{console:?}").to_lowercase();
                let mut c = cfg(&tag, false)?;
                c.env = env;
                c.console = console;
                c.paused = true;
                c.mem_mb = 512;
                let vm = Vm::start(&c, new_job()?, Path::new(LOGS))?;
                log.line(&format!(
                    "[{tag}] pid={} creation={} env={:?}",
                    vm.proc.pid,
                    vm.identity.creation,
                    f0_supervisor::env_for(env)
                ));
                let qmp = f0_supervisor::qmp::Qmp::connect(&c.qmp_sock, Duration::from_secs(8));
                match qmp {
                    Ok(mut q) => {
                        let acc = q.execute("query-accelerators", None);
                        let st = q.execute("query-status", None);
                        log.line(&format!("[{tag}] QMP ok accel={acc:?} status={st:?}"));
                    }
                    Err(e) => log.line(&format!("[{tag}] QMP fallito: {e}")),
                }
                let exited = vm.wait(Duration::from_millis(0))?;
                log.line(&format!("[{tag}] uscito_prima_dello_stop={exited:?}"));
                let inj = win::is_in_job(&vm.proc.process, Some(&vm.job))?;
                log.line(&format!(
                    "[{tag}] qemu_nel_job={inj} pid_nel_job={:?} figli={:?}",
                    vm.job.pids()?,
                    vm.children()?
                ));
                let ext = vm.job.extended()?;
                log.line(&format!(
                    "[{tag}] limit_flags=0x{:x} (KILL_ON_JOB_CLOSE=0x2000, BREAKAWAY_OK=0x800, SILENT_BREAKAWAY_OK=0x1000)",
                    ext.limit_flags
                ));
                if exited.is_none() {
                    let (tp, tj) = vm.emergency_stop(Duration::from_secs(10))?;
                    log.line(&format!(
                        "[{tag}] stop ms_processo={tp:.1} ms_job_vuoto={tj:.1}"
                    ));
                }
                let msgs = vm.job.drain_messages(100);
                log.line(&format!(
                    "[{tag}] messaggi_job={:?}",
                    msgs.iter().map(|m| (m.name(), m.value)).collect::<Vec<_>>()
                ));
                let stderr = std::fs::read_to_string(&vm.stderr).unwrap_or_default();
                log.line(&format!(
                    "[{tag}] stderr: {}",
                    stderr.trim().replace('\n', " | ")
                ));
                cleanup_sock(&c);
                residual_line(&mut log, &tag);
            }
        }
        Ok(())
    }

    fn limits_probe() -> io::Result<()> {
        let mut log = Log::create(&Path::new(LOGS).join("f0-08-limits-probe.log"))?;
        let job = Job::create(JobLimits::default())?;
        for (name, r) in job.probe_privileged_limits() {
            log.line(&format!("{name}: {r:?}"));
        }
        let j2 = Job::create(JobLimits {
            process_memory: Some(1 << 30),
            job_memory: Some(2 << 30),
        })?;
        log.line(&format!(
            "PROCESS_MEMORY+JOB_MEMORY impostati: {:?}",
            j2.extended()
        ));
        log.line(&format!(
            "CPU hard cap 25%: {:?}",
            j2.set_cpu_hard_cap(Some(2500))
        ));
        Ok(())
    }

    // -----------------------------------------------------------------------
    // supervise: processo supervisor figlio per lo scenario (a).
    // -----------------------------------------------------------------------
    fn supervise(a: &Args) -> io::Result<()> {
        let tag = a.s("tag", "f0-06-a");
        let after = a.get("abort-after-ms", 0u64);
        let c = cfg(&tag, true)?;
        let vm = Vm::start(&c, new_job()?, Path::new(LOGS))?;
        // conhost.exe viene creato poco dopo il processo: attesa fino a 3 s.
        let mut conhost = None;
        for _ in 0..60 {
            conhost = vm
                .children()?
                .into_iter()
                .find(|(_, n, _)| n.eq_ignore_ascii_case("conhost.exe"));
            if conhost.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        println!(
            "READY pid={} creation={} conhost={} conhost_in_job={}",
            vm.proc.pid,
            vm.identity.creation,
            conhost.as_ref().map(|c| c.0).unwrap_or(0),
            conhost.as_ref().and_then(|c| c.2).unwrap_or(false)
        );
        use std::io::Write;
        io::stdout().flush()?;
        if after > 0 {
            std::thread::sleep(Duration::from_millis(after));
            println!("ABORT");
            io::stdout().flush()?;
            std::process::abort();
        }
        // Modalità "kill esterno": resta in attesa finché non viene ucciso.
        loop {
            std::thread::sleep(Duration::from_secs(1));
        }
    }

    fn scenario_a(a: &Args) -> io::Result<()> {
        let cycles = a.get("cycles", 5u32);
        let mode = a.s("mode", "abort");
        let delay = a.get("delay-ms", 15000u64);
        let mut log = Log::create(&Path::new(LOGS).join(format!("f0-06-scenario-a-{mode}.log")))?;
        let me = std::env::current_exe()?;
        let mut samples = Vec::new();
        let mut failures = 0;
        for i in 1..=cycles {
            let tag = format!("f0-06-a-{mode}-{i}");
            let mut cmd = Command::new(&me);
            cmd.arg("supervise").arg("--tag").arg(&tag);
            if mode == "abort" {
                cmd.arg("--abort-after-ms").arg(delay.to_string());
            }
            let mut child = cmd.stdout(Stdio::piped()).stderr(Stdio::null()).spawn()?;
            let sup = win::open_process(child.id(), true)?;
            let mut rd = BufReader::new(child.stdout.take().expect("stdout"));
            let mut line = String::new();
            rd.read_line(&mut line)?;
            let kv: HashMap<&str, &str> = line
                .split_whitespace()
                .filter_map(|t| t.split_once('='))
                .collect();
            let qpid: u32 = kv.get("pid").and_then(|v| v.parse().ok()).unwrap_or(0);
            let qcre: u64 = kv.get("creation").and_then(|v| v.parse().ok()).unwrap_or(0);
            let chpid: u32 = kv.get("conhost").and_then(|v| v.parse().ok()).unwrap_or(0);
            let q = win::open_process(qpid, false)?;
            let (cre, _, _) = win::process_times(&q)?;
            let ch = if chpid != 0 {
                win::open_process(chpid, false).ok()
            } else {
                None
            };
            log.line(&format!(
                "[{tag}] supervisor pid={} {} identita_qemu_coincide={}",
                child.id(),
                line.trim(),
                cre == qcre
            ));
            if mode == "kill" {
                std::thread::sleep(Duration::from_millis(delay));
                log.line(&format!(
                    "[{tag}] TerminateProcess(supervisor) dall'esterno"
                ));
                win::terminate_process(&sup, 1)?;
            }
            let sup_code = win::wait_process(&sup, 60_000)?;
            let t0 = Instant::now();
            let q_code = win::wait_process(&q, 30_000)?;
            let dt = t0.elapsed().as_secs_f64() * 1000.0;
            let ch_code = ch.as_ref().map(|h| win::wait_process(h, 30_000));
            let ch_dt = t0.elapsed().as_secs_f64() * 1000.0;
            let _ = child.wait();
            log.line(&format!(
                "[{tag}] supervisor_uscita={sup_code:?} qemu_uscita={q_code:?} ms_da_morte_supervisor_a_uscita_qemu={dt:.2} conhost_uscita={ch_code:?} ms_conhost={ch_dt:.2}"
            ));
            if q_code.is_some() {
                samples.push(dt);
            } else {
                failures += 1;
            }
            let _ = std::fs::remove_file(Path::new(PRIV).join(format!("{tag}.sock")));
            let _ = std::fs::remove_file(Path::new(VMDIR).join(format!("{tag}-vars.fd")));
            residual_line(&mut log, &tag);
        }
        log.line(&format!(
            "RIEPILOGO scenario a ({mode}): {} fallimenti={failures}",
            stats(&samples).map(|s| s.to_string()).unwrap_or_default()
        ));
        Ok(())
    }

    // -----------------------------------------------------------------------
    // (b) QEMU ucciso dall'esterno: il supervisor lo rileva.
    // -----------------------------------------------------------------------
    fn scenario_b(a: &Args) -> io::Result<()> {
        let cycles = a.get("cycles", 5u32);
        let mut log = Log::create(&Path::new(LOGS).join("f0-06-scenario-b.log"))?;
        let mut det = Vec::new();
        for i in 1..=cycles {
            let tag = format!("f0-06-b-{i}");
            let c = cfg(&tag, true)?;
            let vm = Vm::start(&c, new_job()?, Path::new(LOGS))?;
            std::thread::sleep(Duration::from_secs(10));
            // Kill esterno: processo separato, come farebbe un utente o un altro programma.
            let killer = if i % 2 == 1 {
                vec![
                    "taskkill.exe".to_string(),
                    "/F".into(),
                    "/PID".into(),
                    vm.proc.pid.to_string(),
                ]
            } else {
                vec![
                    "powershell.exe".to_string(),
                    "-NoProfile".into(),
                    "-Command".into(),
                    format!("Stop-Process -Id {} -Force", vm.proc.pid),
                ]
            };
            let tk = Instant::now();
            let out = Command::new(&killer[0]).args(&killer[1..]).output()?;
            let t_killer = tk.elapsed().as_secs_f64() * 1000.0;
            let code = vm.wait(Duration::from_secs(30))?;
            let t_detect = tk.elapsed().as_secs_f64() * 1000.0;
            let msgs = vm.job.drain_messages(200);
            log.line(&format!(
                "[{tag}] killer={:?} esito_killer={:?} ms_killer={t_killer:.1} rilevato_dal_supervisor={code:?} (0x{:X}) ms_dall_avvio_kill={t_detect:.1} messaggi_job={:?} identita={:?}",
                killer[0],
                String::from_utf8_lossy(&out.stdout).trim(),
                code.unwrap_or(0),
                msgs.iter().map(|m| (m.name(), m.value)).collect::<Vec<_>>(),
                vm.identity.check_pid()
            ));
            det.push(t_detect);
            // Il job deve risultare vuoto (conhost incluso).
            let acc = vm.job.accounting()?;
            log.line(&format!(
                "[{tag}] job attivi={} totali={} terminati={}",
                acc.active_processes, acc.total_processes, acc.terminated_processes
            ));
            drop(vm);
            cleanup_sock(&c);
            residual_line(&mut log, &tag);
        }
        log.line(&format!(
            "RIEPILOGO scenario b (ms dall'avvio del killer alla rilevazione): {}",
            stats(&det).map(|s| s.to_string()).unwrap_or_default()
        ));
        Ok(())
    }

    // -----------------------------------------------------------------------
    // (c) Emergency Stop con guest in esecuzione.
    // -----------------------------------------------------------------------
    fn scenario_c(a: &Args) -> io::Result<()> {
        let samples = a.get("samples", 30u32);
        let wait_min = a.get("wait-min-s", 20u64);
        let wait_max = a.get("wait-max-s", 60u64);
        let label = a.s("label", "boot");
        let mut log = Log::create(&Path::new(LOGS).join(format!("f0-06-scenario-c-{label}.log")))?;
        let mut tp = Vec::new();
        let mut tj = Vec::new();
        let mut errors = 0;
        let mut residual_bad = 0;
        let mut seed: u64 = 0x2545_F491_4F6C_DD1D;
        for i in 1..=samples {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let wait = if wait_max > wait_min {
                wait_min + seed % (wait_max - wait_min + 1)
            } else {
                wait_min
            };
            let tag = format!("f0-06-c-{label}-{i}");
            let c = cfg(&tag, true)?;
            let vm = Vm::start(&c, new_job()?, Path::new(LOGS))?;
            let mut q = f0_supervisor::qmp::Qmp::connect(&c.qmp_sock, Duration::from_secs(10))?;
            std::thread::sleep(Duration::from_secs(wait));
            let st = q.execute("query-status", None)?;
            let shot = Path::new(PRIV).join(format!("{tag}.ppm"));
            let pct = q
                .execute("screendump", Some(json!({"filename": shot.to_str()})))
                .ok()
                .and_then(|_| f0_supervisor::ppm_nonblack(&shot).ok());
            let _ = std::fs::remove_file(&shot);
            let cpu_before = vm.job.accounting()?;
            match vm.emergency_stop(Duration::from_secs(10)) {
                Ok((p, j)) => {
                    tp.push(p);
                    tj.push(j);
                    log.line(&format!(
                        "[{tag}] attesa_s={wait} stato={} schermo={pct:?} cpu_job_s={:.1} ms_uscita_qemu={p:.2} ms_job_vuoto={j:.2} uscita={:?}",
                        st["status"],
                        (cpu_before.user_100ns + cpu_before.kernel_100ns) as f64 / 1e7,
                        vm.wait(Duration::ZERO)?
                    ));
                }
                Err(e) => {
                    errors += 1;
                    log.line(&format!("[{tag}] ERRORE stop: {e}"));
                }
            }
            drop(q);
            drop(vm);
            cleanup_sock(&c);
            if residual_line(&mut log, &tag) != 0 {
                residual_bad += 1;
            }
        }
        log.line(&format!(
            "RIEPILOGO scenario c [{label}] uscita QEMU (ms): {}",
            stats(&tp).map(|s| s.to_string()).unwrap_or_default()
        ));
        log.line(&format!(
            "RIEPILOGO scenario c [{label}] job vuoto (ms): {} errori={errors} cicli_con_residui={residual_bad}",
            stats(&tj).map(|s| s.to_string()).unwrap_or_default()
        ));
        Ok(())
    }

    // -----------------------------------------------------------------------
    // (d) Arresto gentile: system_powerdown → quit → TerminateJobObject.
    // -----------------------------------------------------------------------
    fn scenario_d(a: &Args) -> io::Result<()> {
        let cycles = a.get("cycles", 5u32);
        let boot = a.get("boot-wait-s", 130u64);
        let pd_t = a.get("powerdown-timeout-s", 45u64);
        let quit_t = a.get("quit-timeout-s", 5u64);
        let mut log = Log::create(&Path::new(LOGS).join("f0-06-scenario-d.log"))?;
        let mut total = Vec::new();
        for i in 1..=cycles {
            let tag = format!("f0-06-d-{i}");
            let c = cfg(&tag, true)?;
            let vm = Vm::start(&c, new_job()?, Path::new(LOGS))?;
            let mut q = f0_supervisor::qmp::Qmp::connect(&c.qmp_sock, Duration::from_secs(10))?;
            std::thread::sleep(Duration::from_secs(boot));
            let shot = Path::new(PRIV).join(format!("{tag}.ppm"));
            let pct = q
                .execute("screendump", Some(json!({"filename": shot.to_str()})))
                .ok()
                .and_then(|_| f0_supervisor::ppm_nonblack(&shot).ok());
            let _ = std::fs::remove_file(&shot);
            let t0 = Instant::now();
            let mut phase = "powerdown";
            q.execute("system_powerdown", None)?;
            let mut exited = None;
            let deadline = t0 + Duration::from_secs(pd_t);
            while Instant::now() < deadline {
                let _ = q.wait_event("SHUTDOWN", Duration::from_millis(500));
                if let Some(c) = vm.wait(Duration::ZERO)? {
                    exited = Some(c);
                    break;
                }
            }
            let t_pd = t0.elapsed().as_secs_f64() * 1000.0;
            let mut t_quit = 0.0;
            let mut t_kill = 0.0;
            if exited.is_none() {
                phase = "quit";
                let tq = Instant::now();
                let r = q.execute("quit", None);
                exited = vm.wait(Duration::from_secs(quit_t))?;
                t_quit = tq.elapsed().as_secs_f64() * 1000.0;
                log.line(&format!("[{tag}] quit risposta={r:?}"));
                if exited.is_none() {
                    phase = "terminate-job";
                    let tk = Instant::now();
                    let _ = vm.emergency_stop(Duration::from_secs(10))?;
                    exited = vm.wait(Duration::ZERO)?;
                    t_kill = tk.elapsed().as_secs_f64() * 1000.0;
                }
            }
            let tt = t0.elapsed().as_secs_f64() * 1000.0;
            total.push(tt);
            let evs: Vec<String> = q
                .events
                .iter()
                .map(|(t, v)| {
                    format!(
                        "{}@{:.1}s",
                        v["event"].as_str().unwrap_or("?"),
                        t.duration_since(vm.started).as_secs_f64()
                    )
                })
                .collect();
            log.line(&format!(
                "[{tag}] schermo_prima={pct:?} fase_conclusiva={phase} ms_powerdown={t_pd:.0} ms_quit={t_quit:.1} ms_terminate={t_kill:.1} ms_totale={tt:.0} uscita={exited:?} eventi={evs:?}"
            ));
            drop(q);
            drop(vm);
            cleanup_sock(&c);
            residual_line(&mut log, &tag);
        }
        log.line(&format!(
            "RIEPILOGO scenario d (ms totali dal comando all'uscita): {}",
            stats(&total).map(|s| s.to_string()).unwrap_or_default()
        ));
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Identità: un PID riusato non viene scambiato per il QEMU precedente.
    // -----------------------------------------------------------------------
    fn pid_reuse(a: &Args) -> io::Result<()> {
        let max = a.get("max-spawn", 20000u32);
        let mut log = Log::create(&Path::new(LOGS).join("f0-06-pid-reuse.log"))?;
        let tag = "f0-06-pid";
        let c = cfg(tag, false)?;
        let mut c = c;
        c.paused = true;
        c.mem_mb = 256;
        let vm = Vm::start(&c, new_job()?, Path::new(LOGS))?;
        let old = vm.identity;
        log.line(&format!("QEMU pid={} creation={}", old.pid, old.creation));
        vm.emergency_stop(Duration::from_secs(10))?;
        log.line(&format!(
            "dopo la morte, handle ancora aperto: check_pid={:?}",
            old.check_pid()
        ));
        // Fase 1: con l'handle aperto il PID non può essere riassegnato.
        let probe = |n: u32, log: &mut Log| -> io::Result<Option<(u32, std::process::Child)>> {
            let whoami = Path::new(&std::env::var("SystemRoot").unwrap_or(r"C:\Windows".into()))
                .join(r"System32\whoami.exe");
            for k in 1..=n {
                let mut ch = Command::new(&whoami)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()?;
                if ch.id() == old.pid {
                    return Ok(Some((k, ch)));
                }
                let _ = ch.wait();
                if k.is_multiple_of(5000) {
                    log.line(&format!("  ... {k} processi creati senza riuso"));
                }
            }
            Ok(None)
        };
        let n1 = 2000;
        let r1 = probe(n1, &mut log)?;
        log.line(&format!(
            "fase 1 (handle QEMU aperto): {n1} processi creati, riuso del PID {} = {}",
            old.pid,
            r1.is_some()
        ));
        // Fase 2: chiuso l'handle (e il job), il PID torna disponibile.
        drop(vm);
        cleanup_sock(&c);
        log.line(&format!("handle chiuso: check_pid={:?}", old.check_pid()));
        match probe(max, &mut log)? {
            Some((k, mut ch)) => {
                let chk = old.check_pid();
                log.line(&format!(
                    "fase 2: PID {} riusato dopo {k} processi; check_pid={chk:?} → scambio evitato={}",
                    old.pid,
                    matches!(chk, IdentityCheck::Reused { .. } | IdentityCheck::Absent)
                ));
                let _ = ch.wait();
            }
            None => log.line(&format!(
                "fase 2: PID {} non riusato entro {max} processi (test del riuso non eseguito)",
                old.pid
            )),
        }
        residual_line(&mut log, tag);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Limiti di memoria.
    // -----------------------------------------------------------------------
    fn mem_limit(a: &Args) -> io::Result<()> {
        let limit_mb = a.get("limit-mb", 1024usize);
        let kind = a.s("kind", "process");
        let secs = a.get("seconds", 90u64);
        let tag = format!("f0-08-mem-{kind}-{limit_mb}");
        let mut log = Log::create(&Path::new(LOGS).join(format!("{tag}.log")))?;
        let lim = limit_mb << 20;
        let limits = if kind == "job" {
            JobLimits {
                process_memory: None,
                job_memory: Some(lim),
            }
        } else {
            JobLimits {
                process_memory: Some(lim),
                job_memory: None,
            }
        };
        let mut job = Job::create(limits)?;
        job.attach_port()?;
        let c = cfg(&tag, true)?;
        let vm = Vm::start(&c, job, Path::new(LOGS))?;
        log.line(&format!(
            "limite {kind}={limit_mb} MiB, guest -m {} pid={}",
            c.mem_mb, vm.proc.pid
        ));
        let t0 = Instant::now();
        let mut exit = None;
        while t0.elapsed() < Duration::from_secs(secs) {
            if let Some(code) = vm.wait(Duration::from_millis(5000))? {
                exit = Some(code);
                break;
            }
            let (ws, _, pv) = win::process_memory(&vm.proc.process).unwrap_or((0, 0, 0));
            let ext = vm.job.extended()?;
            log.line(&format!(
                "t={:.0}s ws={} MiB private={} MiB picco_job={} MiB picco_proc={} MiB",
                t0.elapsed().as_secs_f64(),
                ws >> 20,
                pv >> 20,
                ext.peak_job_memory >> 20,
                ext.peak_process_memory >> 20
            ));
        }
        let msgs = vm.job.drain_messages(200);
        log.line(&format!(
            "uscita={exit:?} dopo {:.1}s messaggi_job={:?}",
            t0.elapsed().as_secs_f64(),
            msgs.iter().map(|m| (m.name(), m.value)).collect::<Vec<_>>()
        ));
        if exit.is_none() {
            let (p, _) = vm.emergency_stop(Duration::from_secs(10))?;
            log.line(&format!(
                "ancora vivo al termine: Emergency Stop in {p:.1} ms"
            ));
        }
        let stderr = std::fs::read_to_string(&vm.stderr).unwrap_or_default();
        log.line(&format!("stderr: {}", stderr.trim().replace('\n', " | ")));
        drop(vm);
        cleanup_sock(&c);
        residual_line(&mut log, &tag);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Cap CPU e misure di consumo.
    // -----------------------------------------------------------------------
    fn cpu_cap(a: &Args) -> io::Result<()> {
        let rate = a.get("rate", 0u32); // centesimi di %; 0 = nessun cap
        let secs = a.get("seconds", 150u64);
        let set_at = a.get("cap-at-s", 0u64);
        let label = a.s("label", &format!("rate{rate}"));
        let tag = format!("f0-08-cpu-{label}");
        let mut log = Log::create(&Path::new(LOGS).join(format!("{tag}.log")))?;
        let ncpu = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1) as f64;
        let job = Job::create(JobLimits::default())?;
        if rate > 0 && set_at == 0 {
            job.set_cpu_hard_cap(Some(rate))?;
        }
        let c = cfg(&tag, true)?;
        let vm = Vm::start(&c, job, Path::new(LOGS))?;
        let mut q = f0_supervisor::qmp::Qmp::connect(&c.qmp_sock, Duration::from_secs(10))?;
        log.line(&format!(
            "cap={} (centesimi di % dei cicli di sistema) da t={set_at}s, processori logici={ncpu}",
            rate
        ));
        let t0 = Instant::now();
        let mut last = (Instant::now(), 0u64, 0u64);
        let mut series = Vec::new();
        let mut capped = set_at == 0 && rate > 0;
        let mut sec = 0u64;
        while t0.elapsed() < Duration::from_secs(secs) {
            std::thread::sleep(Duration::from_secs(1));
            sec += 1;
            if !capped && rate > 0 && sec >= set_at {
                vm.job.set_cpu_hard_cap(Some(rate))?;
                capped = true;
                log.line(&format!("t={sec}s cap applicato"));
            }
            let acc = vm.job.accounting()?;
            let (_, k, u) = win::process_times(&vm.proc.process)?;
            let now = Instant::now();
            let wall = now.duration_since(last.0).as_secs_f64();
            let job_cpu = (acc.user_100ns + acc.kernel_100ns) as f64 / 1e7;
            let proc_cpu = (k + u) as f64 / 1e7;
            let d_job = job_cpu - last.1 as f64 / 1e7;
            let core_pct = 100.0 * d_job / wall;
            let sys_pct = core_pct / ncpu;
            let (ws, _, pv) = win::process_memory(&vm.proc.process)?;
            if sec.is_multiple_of(5) || sec <= 3 {
                log.line(&format!(
                    "t={sec}s cpu_job={core_pct:.1}% di un core ({sys_pct:.2}% del sistema) cpu_cumulata_job={job_cpu:.1}s cpu_cumulata_qemu={proc_cpu:.1}s ws={} MiB private={} MiB",
                    ws >> 20,
                    pv >> 20
                ));
            }
            series.push((sec, capped, sys_pct, core_pct));
            last = (now, acc.user_100ns + acc.kernel_100ns, 0);
        }
        let shot = Path::new(PRIV).join(format!("{tag}.ppm"));
        let pct = q
            .execute("screendump", Some(json!({"filename": shot.to_str()})))
            .ok()
            .and_then(|_| f0_supervisor::ppm_nonblack(&shot).ok());
        let shot_size = std::fs::metadata(&shot).map(|m| m.len()).unwrap_or(0);
        let _ = std::fs::remove_file(&shot);
        let capped_sys: Vec<f64> = series.iter().filter(|s| s.1).map(|s| s.2).collect();
        let free_sys: Vec<f64> = series.iter().filter(|s| !s.1).map(|s| s.2).collect();
        let capped_core: Vec<f64> = series.iter().filter(|s| s.1).map(|s| s.3).collect();
        let free_core: Vec<f64> = series.iter().filter(|s| !s.1).map(|s| s.3).collect();
        log.line(&format!(
            "RIEPILOGO [{label}] % sistema con cap: {} | senza cap: {}",
            stats(&capped_sys)
                .map(|s| s.to_string())
                .unwrap_or("-".into()),
            stats(&free_sys)
                .map(|s| s.to_string())
                .unwrap_or("-".into())
        ));
        log.line(&format!(
            "RIEPILOGO [{label}] % di un core con cap: {} | senza cap: {}",
            stats(&capped_core)
                .map(|s| s.to_string())
                .unwrap_or("-".into()),
            stats(&free_core)
                .map(|s| s.to_string())
                .unwrap_or("-".into())
        ));
        log.line(&format!(
            "schermo finale non nero={pct:?} dimensione_screendump={shot_size} byte"
        ));
        let (_, pk, pv) = win::process_memory(&vm.proc.process)?;
        log.line(&format!(
            "picco_ws={} MiB private_finale={} MiB",
            pk >> 20,
            pv >> 20
        ));
        drop(q);
        vm.emergency_stop(Duration::from_secs(10))?;
        drop(vm);
        cleanup_sock(&c);
        residual_line(&mut log, &tag);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Esclusione dei dischi tramite share mode (F0-05, punto 7).
    // -----------------------------------------------------------------------
    fn exclusion(_a: &Args) -> io::Result<()> {
        crate::excl::run(Path::new(LOGS))
    }
}

#[cfg(windows)]
mod excl;
