//! Driver of the F0-06/F0-08 spike. Every subcommand is a repeatable test;
//! logs go to `lab/logs/f0-06-*` and `lab/logs/f0-08-*`.

#[cfg(not(windows))]
fn main() {
    eprintln!("f0-supervisor: Windows-only spike");
}

#[cfg(windows)]
fn main() {
    if let Err(e) = imp::run() {
        eprintln!("error: {e}");
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
                    "usage: f0-supervisor <env-probe|limits-probe|scenario-a|scenario-b|scenario-c|scenario-d|pid-reuse|mem-limit|cpu-cap|exclusion> [--option value]"
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
        // Short wait so that the process list reflects the exit.
        std::thread::sleep(Duration::from_millis(200));
        let (total, mine) = qemu_residuals(MARKER);
        log.line(&format!(
            "{ctx}: spike_residuals={} host_qemu_total={total}",
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
    // env-probe: minimal environment, console, job membership, children.
    // -----------------------------------------------------------------------
    fn env_probe() -> io::Result<()> {
        let mut log = Log::create(&Path::new(LOGS).join("f0-06-env-probe.log"))?;
        log.line(&format!(
            "supervisor (this process) already in a job: {:?}",
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
                    Err(e) => log.line(&format!("[{tag}] QMP failed: {e}")),
                }
                let exited = vm.wait(Duration::from_millis(0))?;
                log.line(&format!("[{tag}] exited_before_stop={exited:?}"));
                let inj = win::is_in_job(&vm.proc.process, Some(&vm.job))?;
                log.line(&format!(
                    "[{tag}] qemu_in_job={inj} pids_in_job={:?} children={:?}",
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
                        "[{tag}] stop process_ms={tp:.1} job_empty_ms={tj:.1}"
                    ));
                }
                let msgs = vm.job.drain_messages(100);
                log.line(&format!(
                    "[{tag}] job_messages={:?}",
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
            "PROCESS_MEMORY+JOB_MEMORY set: {:?}",
            j2.extended()
        ));
        log.line(&format!(
            "CPU hard cap 25%: {:?}",
            j2.set_cpu_hard_cap(Some(2500))
        ));
        Ok(())
    }

    // -----------------------------------------------------------------------
    // supervise: child supervisor process for scenario (a).
    // -----------------------------------------------------------------------
    fn supervise(a: &Args) -> io::Result<()> {
        let tag = a.s("tag", "f0-06-a");
        let after = a.get("abort-after-ms", 0u64);
        let c = cfg(&tag, true)?;
        let vm = Vm::start(&c, new_job()?, Path::new(LOGS))?;
        // conhost.exe is created shortly after the process: wait up to 3 s.
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
        // "External kill" mode: keeps waiting until it is killed.
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
                "[{tag}] supervisor pid={} {} qemu_identity_matches={}",
                child.id(),
                line.trim(),
                cre == qcre
            ));
            if mode == "kill" {
                std::thread::sleep(Duration::from_millis(delay));
                log.line(&format!(
                    "[{tag}] TerminateProcess(supervisor) from outside"
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
                "[{tag}] supervisor_exit={sup_code:?} qemu_exit={q_code:?} ms_from_supervisor_death_to_qemu_exit={dt:.2} conhost_exit={ch_code:?} conhost_ms={ch_dt:.2}"
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
            "SUMMARY scenario a ({mode}): {} failures={failures}",
            stats(&samples).map(|s| s.to_string()).unwrap_or_default()
        ));
        Ok(())
    }

    // -----------------------------------------------------------------------
    // (b) QEMU killed from outside: the supervisor detects it.
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
            // External kill: separate process, as a user or another program would do.
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
                "[{tag}] killer={:?} killer_outcome={:?} killer_ms={t_killer:.1} detected_by_supervisor={code:?} (0x{:X}) ms_since_kill_start={t_detect:.1} job_messages={:?} identity={:?}",
                killer[0],
                String::from_utf8_lossy(&out.stdout).trim(),
                code.unwrap_or(0),
                msgs.iter().map(|m| (m.name(), m.value)).collect::<Vec<_>>(),
                vm.identity.check_pid()
            ));
            det.push(t_detect);
            // The job must turn out empty (conhost included).
            let acc = vm.job.accounting()?;
            log.line(&format!(
                "[{tag}] job active={} total={} terminated={}",
                acc.active_processes, acc.total_processes, acc.terminated_processes
            ));
            drop(vm);
            cleanup_sock(&c);
            residual_line(&mut log, &tag);
        }
        log.line(&format!(
            "SUMMARY scenario b (ms from killer start to detection): {}",
            stats(&det).map(|s| s.to_string()).unwrap_or_default()
        ));
        Ok(())
    }

    // -----------------------------------------------------------------------
    // (c) Emergency Stop with a running guest.
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
                        "[{tag}] wait_s={wait} status={} screen={pct:?} job_cpu_s={:.1} qemu_exit_ms={p:.2} job_empty_ms={j:.2} exit={:?}",
                        st["status"],
                        (cpu_before.user_100ns + cpu_before.kernel_100ns) as f64 / 1e7,
                        vm.wait(Duration::ZERO)?
                    ));
                }
                Err(e) => {
                    errors += 1;
                    log.line(&format!("[{tag}] ERROR stop: {e}"));
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
            "SUMMARY scenario c [{label}] QEMU exit (ms): {}",
            stats(&tp).map(|s| s.to_string()).unwrap_or_default()
        ));
        log.line(&format!(
            "SUMMARY scenario c [{label}] job empty (ms): {} errors={errors} cycles_with_residuals={residual_bad}",
            stats(&tj).map(|s| s.to_string()).unwrap_or_default()
        ));
        Ok(())
    }

    // -----------------------------------------------------------------------
    // (d) Graceful shutdown: system_powerdown → quit → TerminateJobObject.
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
                log.line(&format!("[{tag}] quit response={r:?}"));
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
                "[{tag}] screen_before={pct:?} final_phase={phase} powerdown_ms={t_pd:.0} quit_ms={t_quit:.1} terminate_ms={t_kill:.1} total_ms={tt:.0} exit={exited:?} events={evs:?}"
            ));
            drop(q);
            drop(vm);
            cleanup_sock(&c);
            residual_line(&mut log, &tag);
        }
        log.line(&format!(
            "SUMMARY scenario d (total ms from the command to the exit): {}",
            stats(&total).map(|s| s.to_string()).unwrap_or_default()
        ));
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Identity: a reused PID is not mistaken for the previous QEMU.
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
            "after death, handle still open: check_pid={:?}",
            old.check_pid()
        ));
        // Phase 1: with the handle open the PID cannot be reassigned.
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
                    log.line(&format!("  ... {k} processes created without reuse"));
                }
            }
            Ok(None)
        };
        let n1 = 2000;
        let r1 = probe(n1, &mut log)?;
        log.line(&format!(
            "phase 1 (QEMU handle open): {n1} processes created, reuse of PID {} = {}",
            old.pid,
            r1.is_some()
        ));
        // Phase 2: once the handle (and the job) is closed, the PID becomes available again.
        drop(vm);
        cleanup_sock(&c);
        log.line(&format!("handle closed: check_pid={:?}", old.check_pid()));
        match probe(max, &mut log)? {
            Some((k, mut ch)) => {
                let chk = old.check_pid();
                log.line(&format!(
                    "phase 2: PID {} reused after {k} processes; check_pid={chk:?} → mix-up avoided={}",
                    old.pid,
                    matches!(chk, IdentityCheck::Reused { .. } | IdentityCheck::Absent)
                ));
                let _ = ch.wait();
            }
            None => log.line(&format!(
                "phase 2: PID {} not reused within {max} processes (reuse test not run)",
                old.pid
            )),
        }
        residual_line(&mut log, tag);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Memory limits.
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
            "limit {kind}={limit_mb} MiB, guest -m {} pid={}",
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
                "t={:.0}s ws={} MiB private={} MiB job_peak={} MiB proc_peak={} MiB",
                t0.elapsed().as_secs_f64(),
                ws >> 20,
                pv >> 20,
                ext.peak_job_memory >> 20,
                ext.peak_process_memory >> 20
            ));
        }
        let msgs = vm.job.drain_messages(200);
        log.line(&format!(
            "exit={exit:?} after {:.1}s job_messages={:?}",
            t0.elapsed().as_secs_f64(),
            msgs.iter().map(|m| (m.name(), m.value)).collect::<Vec<_>>()
        ));
        if exit.is_none() {
            let (p, _) = vm.emergency_stop(Duration::from_secs(10))?;
            log.line(&format!(
                "still alive at the end: Emergency Stop in {p:.1} ms"
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
    // CPU cap and consumption measurements.
    // -----------------------------------------------------------------------
    fn cpu_cap(a: &Args) -> io::Result<()> {
        let rate = a.get("rate", 0u32); // hundredths of %; 0 = no cap
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
            "cap={} (hundredths of % of system cycles) from t={set_at}s, logical processors={ncpu}",
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
                log.line(&format!("t={sec}s cap applied"));
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
                    "t={sec}s job_cpu={core_pct:.1}% of one core ({sys_pct:.2}% of the system) job_cumulative_cpu={job_cpu:.1}s qemu_cumulative_cpu={proc_cpu:.1}s ws={} MiB private={} MiB",
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
            "SUMMARY [{label}] % system with cap: {} | without cap: {}",
            stats(&capped_sys)
                .map(|s| s.to_string())
                .unwrap_or("-".into()),
            stats(&free_sys)
                .map(|s| s.to_string())
                .unwrap_or("-".into())
        ));
        log.line(&format!(
            "SUMMARY [{label}] % of one core with cap: {} | without cap: {}",
            stats(&capped_core)
                .map(|s| s.to_string())
                .unwrap_or("-".into()),
            stats(&free_core)
                .map(|s| s.to_string())
                .unwrap_or("-".into())
        ));
        log.line(&format!(
            "final screen non-black={pct:?} screendump_size={shot_size} bytes"
        ));
        let (_, pk, pv) = win::process_memory(&vm.proc.process)?;
        log.line(&format!(
            "peak_ws={} MiB final_private={} MiB",
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
    // Disk exclusion through share modes (F0-05, item 7).
    // -----------------------------------------------------------------------
    fn exclusion(_a: &Args) -> io::Result<()> {
        crate::excl::run(Path::new(LOGS))
    }
}

#[cfg(windows)]
mod excl;
