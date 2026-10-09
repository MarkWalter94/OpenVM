//! Implementazione Linux dello spike. Vedi `main.rs` per la motivazione di `unsafe`.
//!
//! Struttura dei processi usata dagli scenari (lo stesso eseguibile in ruoli diversi):
//!
//! ```text
//! misuratore ── role-supervisor <modo> ── figlio "VM" (sleep 1000, oppure albero sh)
//! misuratore ── role-ui ── role-supervisor ui-monitor ── figlio "VM"
//! ```
//!
//! Il misuratore tiene un pidfd su ogni processo osservato e considera terminato un
//! processo quando il pidfd diventa leggibile (uscita del processo, `poll` su pidfd,
//! Linux >= 5.3). I tempi sono misurati con `Instant` (CLOCK_MONOTONIC).

use std::ffi::CString;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Seek, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

/// Tempo massimo di attesa della fine dei processi dopo un segnale: oltre, il
/// campione è registrato come timeout (e quindi come possibile orfano).
const TIMEOUT: Duration = Duration::from_secs(5);
const SLEEP_ARG: &str = "1000";

type Res<T> = Result<T, String>;

fn err<E: std::fmt::Display>(ctx: &str) -> impl FnOnce(E) -> String + '_ {
    move |e| format!("{ctx}: {e}")
}

pub(crate) fn main() -> i32 {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(String::as_str).unwrap_or("help");
    let samples = flag(&args, "--samples").unwrap_or(30);
    let r = match cmd {
        // Ruoli interni.
        "role-supervisor" => role_supervisor(&args[2..]),
        "role-ui" => role_ui(),
        "hog-mem" => hog_mem(args.get(2).and_then(|s| s.parse().ok()).unwrap_or(512)),
        "hog-cpu" => hog_cpu(
            args.get(2).and_then(|s| s.parse().ok()).unwrap_or(4),
            args.get(3).and_then(|s| s.parse().ok()).unwrap_or(3),
        ),
        // Scenari.
        "processi" => scenari_processi(samples),
        "cgroup" => scenari_cgroup(samples),
        _ => {
            println!(
                "uso: f0-linux-procs processi [--samples N] | cgroup [--samples N]\n\
                 `cgroup` richiede un cgroup delegato, per esempio:\n\
                 systemd-run --user --scope -p Delegate=yes -- f0-linux-procs cgroup"
            );
            Ok(())
        }
    };
    match r {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("ERRORE {e}");
            1
        }
    }
}

fn flag(args: &[String], name: &str) -> Option<usize> {
    let i = args.iter().position(|a| a == name)?;
    args.get(i + 1)?.parse().ok()
}

// ---------------------------------------------------------------------------
// Primitive di sistema
// ---------------------------------------------------------------------------

fn pidfd_open(pid: i32) -> io::Result<OwnedFd> {
    // SAFETY: syscall con soli argomenti interi; nessun puntatore passato al kernel.
    let r = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    if r < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `r` è un descrittore appena creato dal kernel e non posseduto da altri.
    Ok(unsafe { OwnedFd::from_raw_fd(r as RawFd) })
}

fn pidfd_send_signal(fd: &OwnedFd, sig: i32) -> io::Result<()> {
    // SAFETY: `fd` è un pidfd valido per la durata della chiamata; `info` nullo è
    // ammesso dalla syscall; flags = 0.
    let r = unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            fd.as_raw_fd(),
            sig,
            std::ptr::null::<libc::siginfo_t>(),
            0,
        )
    };
    if r < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// `waitid(P_PIDFD, fd, WEXITED)`: raccoglie un figlio diretto tramite pidfd.
/// Restituisce (si_code, si_status).
fn waitid_pidfd(fd: &OwnedFd) -> io::Result<(i32, i32)> {
    // SAFETY: siginfo_t è un tipo C plain-old-data; l'azzeramento è un valore valido.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    loop {
        // SAFETY: `info` è un buffer valido e scrivibile; `fd` è un pidfd valido.
        let r = unsafe {
            libc::waitid(
                libc::P_PIDFD,
                fd.as_raw_fd() as libc::id_t,
                &mut info,
                libc::WEXITED,
            )
        };
        if r == 0 {
            break;
        }
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
    // SAFETY: dopo un waitid riuscito con WEXITED il campo si_status è valorizzato.
    let status = unsafe { info.si_status() };
    Ok((info.si_code, status))
}

/// Attende che tutti i pidfd diventino leggibili (processi terminati) entro `deadline`.
fn wait_all_exit(fds: &[&OwnedFd], deadline: Instant) -> io::Result<bool> {
    let mut pending: Vec<RawFd> = fds.iter().map(|f| f.as_raw_fd()).collect();
    // Almeno un `poll` viene sempre eseguito, anche con scadenza già passata
    // (timeout 0): serve per il controllo istantaneo dei sopravvissuti.
    let mut primo = true;
    while !pending.is_empty() {
        let now = Instant::now();
        if now >= deadline && !primo {
            return Ok(false);
        }
        primo = false;
        let ms = deadline
            .saturating_duration_since(now)
            .as_millis()
            .min(i32::MAX as u128) as i32;
        let mut pfds: Vec<libc::pollfd> = pending
            .iter()
            .map(|&fd| libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            })
            .collect();
        // SAFETY: `pfds` è un buffer valido di `pfds.len()` elementi per tutta la chiamata.
        let r = unsafe { libc::poll(pfds.as_mut_ptr(), pfds.len() as libc::nfds_t, ms) };
        if r < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        pending = pfds
            .iter()
            .filter(|p| p.revents == 0)
            .map(|p| p.fd)
            .collect();
    }
    Ok(true)
}

fn kill_raw(pid: i32, sig: i32) -> io::Result<()> {
    // SAFETY: kill(2) con argomenti interi; pid negativo indica il gruppo di processi.
    if unsafe { libc::kill(pid, sig) } != 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Imposta nel figlio, tra fork ed exec, `PR_SET_PDEATHSIG(sig)` e verifica che il
/// padre sia ancora quello atteso (altrimenti il segnale non arriverebbe mai).
fn with_pdeathsig(cmd: &mut Command, sig: i32) {
    let parent = std::process::id() as libc::pid_t;
    // SAFETY: la closure viene eseguita nel figlio dopo fork e prima di exec; usa solo
    // prctl(2) e getppid(2), async-signal-safe, senza allocazioni né lock.
    // `io::Error::from_raw_os_error`/`last_os_error` non allocano.
    unsafe {
        cmd.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, sig as libc::c_ulong, 0, 0, 0) != 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::getppid() != parent {
                return Err(io::Error::from_raw_os_error(libc::ESRCH));
            }
            Ok(())
        });
    }
}

/// Sposta il figlio, tra fork ed exec, nel cgroup indicato scrivendo "0" in
/// `cgroup.procs` (il kernel interpreta 0 come "il processo chiamante").
fn into_cgroup(cmd: &mut Command, cg: &Path) -> Res<()> {
    let procs = CString::new(
        cg.join("cgroup.procs")
            .into_os_string()
            .into_encoded_bytes(),
    )
    .map_err(err("percorso cgroup"))?;
    // SAFETY: la closure viene eseguita nel figlio dopo fork e prima di exec; usa solo
    // open(2), write(2), close(2), async-signal-safe. `procs` è allocato prima del
    // fork e catturato per spostamento, quindi il puntatore resta valido.
    unsafe {
        cmd.pre_exec(move || {
            let fd = libc::open(procs.as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC);
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            let n = libc::write(fd, b"0".as_ptr().cast(), 1);
            let e = io::Error::last_os_error();
            libc::close(fd);
            if n != 1 {
                return Err(e);
            }
            Ok(())
        });
    }
    Ok(())
}

fn read_tag(rd: &mut BufReader<ChildStdout>, tag: &str) -> Res<i32> {
    let mut line = String::new();
    loop {
        line.clear();
        let n = rd.read_line(&mut line).map_err(err("lettura pipe"))?;
        if n == 0 {
            return Err(format!("pipe chiusa prima di {tag}"));
        }
        if let Some(rest) = line.trim().strip_prefix(tag) {
            return rest.trim().parse().map_err(err("pid non valido"));
        }
    }
}

/// Attende che `pid` abbia eseguito exec del programma `comm` (evita di misurare la
/// finestra tra fork ed exec).
fn wait_comm(pid: i32, comm: &str) -> Res<()> {
    let deadline = Instant::now() + Duration::from_secs(3);
    let p = format!("/proc/{pid}/comm");
    while Instant::now() < deadline {
        if fs::read_to_string(&p)
            .map(|s| s.trim() == comm)
            .unwrap_or(false)
        {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    Err(format!("pid {pid} non ha eseguito {comm} entro 3 s"))
}

/// Legge (pgrp, session) da /proc/<pid>/stat.
fn pgrp_sid(pid: i32) -> Option<(i32, i32)> {
    let s = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after = &s[s.rfind(')')? + 2..];
    let f: Vec<&str> = after.split_whitespace().collect();
    Some((f.get(2)?.parse().ok()?, f.get(3)?.parse().ok()?))
}

/// Processi "sleep 1000" dell'utente corrente ancora vivi (controllo orfani).
fn sleep_survivors() -> Vec<i32> {
    let me = fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("Uid:"))
                .and_then(|l| l.split_whitespace().nth(1).map(str::to_owned))
        })
        .unwrap_or_default();
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir("/proc") else {
        return out;
    };
    for e in rd.flatten() {
        let Ok(pid) = e.file_name().to_string_lossy().parse::<i32>() else {
            continue;
        };
        let cmdline = fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
        if cmdline != b"sleep\x001000\x00" {
            continue;
        }
        let uid = fs::read_to_string(format!("/proc/{pid}/status"))
            .ok()
            .and_then(|s| {
                s.lines()
                    .find(|l| l.starts_with("Uid:"))
                    .and_then(|l| l.split_whitespace().nth(1).map(str::to_owned))
            })
            .unwrap_or_default();
        let state = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
        let zombie = state
            .rfind(')')
            .and_then(|i| state.get(i + 2..i + 3))
            .map(|c| c == "Z")
            .unwrap_or(false);
        if uid == me && !zombie {
            out.push(pid);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Statistiche
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct Serie {
    ms: Vec<f64>,
    timeout: usize,
    errori: Vec<String>,
    orfani: usize,
}

impl Serie {
    fn report(&mut self, nome: &str) {
        self.ms.sort_by(|a, b| a.total_cmp(b));
        let n = self.ms.len();
        let pick = |p: f64| -> f64 {
            if n == 0 {
                return f64::NAN;
            }
            let k = ((p * n as f64).ceil() as usize).clamp(1, n);
            self.ms[k - 1]
        };
        println!(
            "RISULTATO scenario={nome} campioni_ok={n} p50_ms={:.3} p95_ms={:.3} max_ms={:.3} timeout={} errori={} orfani={}",
            pick(0.50),
            pick(0.95),
            pick(1.0),
            self.timeout,
            self.errori.len(),
            self.orfani
        );
        for e in &self.errori {
            println!("  errore: {e}");
        }
    }
}

// ---------------------------------------------------------------------------
// Ruoli interni
// ---------------------------------------------------------------------------

fn park_forever() -> ! {
    loop {
        std::thread::park();
    }
}

fn role_supervisor(args: &[String]) -> Res<()> {
    let mode = args.first().map(String::as_str).unwrap_or("single");
    let out = io::stdout();
    match mode {
        // Figlio diretto con PDEATHSIG=SIGKILL.
        "single" => {
            let mut c = Command::new("sleep");
            c.arg(SLEEP_ARG).stdout(Stdio::null());
            with_pdeathsig(&mut c, libc::SIGKILL);
            let ch = c.spawn().map_err(err("spawn"))?;
            writeln!(out.lock(), "CHILD {}", ch.id()).map_err(err("stdout"))?;
            park_forever();
        }
        // Figlio con PDEATHSIG che a sua volta crea un nipote (non ereditano PDEATHSIG).
        "tree" => {
            let mut c = Command::new("sh");
            c.args(["-c", "sleep 1000 & echo GC $!; exec sleep 1000"]);
            with_pdeathsig(&mut c, libc::SIGKILL);
            let ch = c.spawn().map_err(err("spawn"))?;
            writeln!(out.lock(), "CHILD {}", ch.id()).map_err(err("stdout"))?;
            park_forever();
        }
        // Figlio capogruppo di un nuovo gruppo di processi, con due nipoti.
        "pgid" | "pgid-escape" => {
            let script = if mode == "pgid" {
                "sleep 1000 & echo GC $!; sleep 1000 & echo GC $!; wait"
            } else {
                // Il primo nipote esce dal gruppo e dalla sessione con setsid.
                "setsid sleep 1000 & echo GC $!; sleep 1000 & echo GC $!; wait"
            };
            let mut c = Command::new("sh");
            c.args(["-c", script]).process_group(0);
            with_pdeathsig(&mut c, libc::SIGKILL);
            let ch = c.spawn().map_err(err("spawn"))?;
            writeln!(out.lock(), "CHILD {}", ch.id()).map_err(err("stdout"))?;
            park_forever();
        }
        // Supervisor che sorveglia la UI con un pidfd e, alla sua morte, arresta la VM
        // in modo controllato (SIGTERM, attesa 2 s, poi SIGKILL) ed esce.
        "ui-monitor" => {
            let ui_pid: i32 = args
                .get(1)
                .and_then(|s| s.parse().ok())
                .ok_or("pid UI mancante")?;
            let ui_fd = pidfd_open(ui_pid).map_err(err("pidfd_open UI"))?;
            // SAFETY: getppid(2) non ha precondizioni.
            if unsafe { libc::getppid() } != ui_pid {
                return Err("UI già terminata prima del pidfd".into());
            }
            let mut c = Command::new("sleep");
            c.arg(SLEEP_ARG).stdout(Stdio::null());
            with_pdeathsig(&mut c, libc::SIGKILL);
            let mut vm = c.spawn().map_err(err("spawn vm"))?;
            let vm_fd = pidfd_open(vm.id() as i32).map_err(err("pidfd_open vm"))?;
            writeln!(out.lock(), "CHILD {}", vm.id()).map_err(err("stdout"))?;
            wait_all_exit(&[&ui_fd], Instant::now() + Duration::from_secs(3600))
                .map_err(err("poll UI"))?;
            pidfd_send_signal(&vm_fd, libc::SIGTERM).map_err(err("SIGTERM vm"))?;
            let ok = wait_all_exit(&[&vm_fd], Instant::now() + Duration::from_secs(2))
                .map_err(err("poll vm"))?;
            if !ok {
                pidfd_send_signal(&vm_fd, libc::SIGKILL).map_err(err("SIGKILL vm"))?;
            }
            let _ = vm.wait();
            Ok(())
        }
        other => Err(format!("modo supervisor sconosciuto: {other}")),
    }
}

fn role_ui() -> Res<()> {
    let exe = std::env::current_exe().map_err(err("current_exe"))?;
    let me = std::process::id().to_string();
    // stdout ereditato: il supervisor scrive CHILD direttamente al misuratore.
    let sup = Command::new(exe)
        .args(["role-supervisor", "ui-monitor", &me])
        .spawn()
        .map_err(err("spawn supervisor"))?;
    writeln!(io::stdout().lock(), "SUP {}", sup.id()).map_err(err("stdout"))?;
    park_forever();
}

fn hog_mem(mb: usize) -> Res<()> {
    let mut v: Vec<Vec<u8>> = Vec::new();
    for _ in 0..mb {
        let mut chunk = vec![0u8; 1 << 20];
        for i in (0..chunk.len()).step_by(4096) {
            chunk[i] = 1;
        }
        v.push(std::hint::black_box(chunk));
    }
    println!("HOG-MEM completato {mb} MiB senza essere terminato");
    Ok(())
}

fn hog_cpu(threads: usize, secs: u64) -> Res<()> {
    let end = Instant::now() + Duration::from_secs(secs);
    let hs: Vec<_> = (0..threads)
        .map(|_| {
            std::thread::spawn(move || {
                let mut x = 0u64;
                while Instant::now() < end {
                    for _ in 0..10_000 {
                        x = std::hint::black_box(
                            x.wrapping_mul(6364136223846793005).wrapping_add(1),
                        );
                    }
                }
                x
            })
        })
        .collect();
    for h in hs {
        let _ = h.join();
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Scenari sui processi (non richiedono privilegi né cgroup)
// ---------------------------------------------------------------------------

fn spawn_role(args: &[&str]) -> Res<(Child, BufReader<ChildStdout>)> {
    let exe = std::env::current_exe().map_err(err("current_exe"))?;
    let mut ch = Command::new(exe)
        .args(args)
        .stdout(Stdio::piped())
        .spawn()
        .map_err(err("spawn ruolo"))?;
    let out = ch.stdout.take().ok_or("stdout mancante")?;
    Ok((ch, BufReader::new(out)))
}

fn scenari_processi(samples: usize) -> Res<()> {
    println!(
        "INFO kernel={}",
        fs::read_to_string("/proc/sys/kernel/osrelease")
            .unwrap_or_default()
            .trim()
    );
    println!(
        "INFO campioni_per_scenario={samples} timeout_s={}",
        TIMEOUT.as_secs()
    );
    let prima = sleep_survivors();
    if !prima.is_empty() {
        println!("AVVISO processi 'sleep 1000' già presenti prima della prova: {prima:?}");
    }

    // (a) PDEATHSIG: SIGKILL al supervisor, misura fino alla fine del figlio diretto.
    let mut s = Serie::default();
    for i in 0..samples {
        match campione_pdeathsig() {
            Ok(Some(d)) => s.ms.push(d),
            Ok(None) => {
                s.timeout += 1;
                s.orfani += 1;
            }
            Err(e) => s.errori.push(format!("campione {i}: {e}")),
        }
    }
    s.report("a_pdeathsig_sigkill_supervisor");

    // (a2) PDEATHSIG non si propaga ai nipoti.
    match campione_pdeathsig_tree() {
        Ok((figlio_ok, nipote_vivo)) => println!(
            "RISULTATO scenario=a2_pdeathsig_albero figlio_diretto_terminato={figlio_ok} nipote_sopravvissuto={nipote_vivo}"
        ),
        Err(e) => println!("RISULTATO scenario=a2_pdeathsig_albero errore={e}"),
    }

    // (b) pidfd: segnale e raccolta tramite pidfd, poi ESRCH sul pidfd vecchio.
    let mut s = Serie::default();
    let mut esrch = 0usize;
    let mut killed = 0usize;
    for i in 0..samples {
        match campione_pidfd() {
            Ok((d, k, e)) => {
                s.ms.push(d);
                killed += usize::from(k);
                esrch += usize::from(e);
            }
            Err(e) => s.errori.push(format!("campione {i}: {e}")),
        }
    }
    s.report("b_pidfd_send_signal_waitid");
    println!(
        "RISULTATO scenario=b_pidfd_identita si_code_CLD_KILLED_SIGKILL={killed}/{samples} esrch_dopo_raccolta={esrch}/{samples} pidfd_open_su_pid_raccolto={}",
        pidfd_open_dopo_raccolta()
    );

    // (c) gruppo di processi: kill(-pgid, SIGKILL), misura fino alla fine di tutti i membri.
    let mut s = Serie::default();
    for i in 0..samples {
        match campione_pgid("pgid") {
            Ok((Some(d), _)) => s.ms.push(d),
            Ok((None, _)) => {
                s.timeout += 1;
                s.orfani += 1;
            }
            Err(e) => s.errori.push(format!("campione {i}: {e}")),
        }
    }
    s.report("c_kill_pgid");

    // (c2) un nipote che chiama setsid esce dal gruppo e sopravvive a kill(-pgid).
    match campione_pgid("pgid-escape") {
        Ok((_, sopravvissuti)) => println!(
            "RISULTATO scenario=c2_pgid_fuga_setsid sopravvissuti_a_kill_pgid={sopravvissuti}"
        ),
        Err(e) => println!("RISULTATO scenario=c2_pgid_fuga_setsid errore={e}"),
    }

    // Crash della UI: il supervisor se ne accorge via pidfd e arresta la VM.
    let mut s = Serie::default();
    for i in 0..samples {
        match campione_ui_crash() {
            Ok(Some(d)) => s.ms.push(d),
            Ok(None) => {
                s.timeout += 1;
                s.orfani += 1;
            }
            Err(e) => s.errori.push(format!("campione {i}: {e}")),
        }
    }
    s.report("ui_crash_arresto_controllato_vm");

    let dopo: Vec<i32> = sleep_survivors()
        .into_iter()
        .filter(|p| !prima.contains(p))
        .collect();
    println!(
        "RISULTATO scenario=controllo_orfani_finale sleep_1000_rimasti={}",
        dopo.len()
    );
    for p in dopo {
        let _ = kill_raw(p, libc::SIGKILL);
    }
    Ok(())
}

fn campione_pdeathsig() -> Res<Option<f64>> {
    let (mut sup, mut rd) = spawn_role(&["role-supervisor", "single"])?;
    let child = read_tag(&mut rd, "CHILD")?;
    wait_comm(child, "sleep")?;
    let cfd = pidfd_open(child).map_err(err("pidfd figlio"))?;
    let sfd = pidfd_open(sup.id() as i32).map_err(err("pidfd supervisor"))?;
    let t0 = Instant::now();
    pidfd_send_signal(&sfd, libc::SIGKILL).map_err(err("SIGKILL supervisor"))?;
    let ok = wait_all_exit(&[&cfd], t0 + TIMEOUT).map_err(err("poll"))?;
    let d = t0.elapsed().as_secs_f64() * 1e3;
    let _ = sup.wait();
    if !ok {
        let _ = pidfd_send_signal(&cfd, libc::SIGKILL);
        return Ok(None);
    }
    Ok(Some(d))
}

fn campione_pdeathsig_tree() -> Res<(bool, bool)> {
    let (mut sup, mut rd) = spawn_role(&["role-supervisor", "tree"])?;
    let child = read_tag(&mut rd, "CHILD")?;
    let gc = read_tag(&mut rd, "GC")?;
    wait_comm(child, "sleep")?;
    wait_comm(gc, "sleep")?;
    let cfd = pidfd_open(child).map_err(err("pidfd figlio"))?;
    let gfd = pidfd_open(gc).map_err(err("pidfd nipote"))?;
    pidfd_send_signal(
        &pidfd_open(sup.id() as i32).map_err(err("pidfd sup"))?,
        libc::SIGKILL,
    )
    .map_err(err("SIGKILL"))?;
    let _ = sup.wait();
    let figlio_ok = wait_all_exit(&[&cfd], Instant::now() + TIMEOUT).map_err(err("poll"))?;
    let nipote_morto =
        wait_all_exit(&[&gfd], Instant::now() + Duration::from_secs(1)).map_err(err("poll"))?;
    // Pulizia del nipote orfano.
    let _ = pidfd_send_signal(&gfd, libc::SIGKILL);
    Ok((figlio_ok, !nipote_morto))
}

fn campione_pidfd() -> Res<(f64, bool, bool)> {
    let ch = Command::new("sleep")
        .arg(SLEEP_ARG)
        .spawn()
        .map_err(err("spawn"))?;
    let pid = ch.id() as i32;
    wait_comm(pid, "sleep")?;
    let fd = pidfd_open(pid).map_err(err("pidfd_open"))?;
    let t0 = Instant::now();
    pidfd_send_signal(&fd, libc::SIGKILL).map_err(err("pidfd_send_signal"))?;
    let (code, status) = waitid_pidfd(&fd).map_err(err("waitid P_PIDFD"))?;
    let d = t0.elapsed().as_secs_f64() * 1e3;
    // Il processo è stato raccolto: il pidfd continua a riferirsi a quel processo e
    // non a un eventuale nuovo processo con lo stesso PID.
    let esrch = matches!(
        pidfd_send_signal(&fd, 0),
        Err(e) if e.raw_os_error() == Some(libc::ESRCH)
    );
    // `ch` è già stato raccolto con waitid: non chiamare ch.wait().
    drop(ch);
    Ok((
        d,
        code == libc::CLD_KILLED && status == libc::SIGKILL,
        esrch,
    ))
}

fn pidfd_open_dopo_raccolta() -> String {
    let Ok(mut ch) = Command::new("true").spawn() else {
        return "errore_spawn".into();
    };
    let pid = ch.id() as i32;
    let _ = ch.wait();
    match pidfd_open(pid) {
        Ok(_) => "aperto(PID_riusato?)".into(),
        Err(e) if e.raw_os_error() == Some(libc::ESRCH) => "ESRCH".into(),
        Err(e) => format!("errore:{e}"),
    }
}

/// Restituisce (durata fino alla fine di tutti i membri del gruppo, numero di
/// processi osservati sopravvissuti a kill(-pgid)).
fn campione_pgid(mode: &str) -> Res<(Option<f64>, usize)> {
    let (mut sup, mut rd) = spawn_role(&["role-supervisor", mode])?;
    let leader = read_tag(&mut rd, "CHILD")?;
    let g1 = read_tag(&mut rd, "GC")?;
    let g2 = read_tag(&mut rd, "GC")?;
    wait_comm(g1, "sleep")?;
    wait_comm(g2, "sleep")?;
    let pids = [leader, g1, g2];
    let fds: Vec<OwnedFd> = pids
        .iter()
        .map(|&p| pidfd_open(p))
        .collect::<io::Result<_>>()
        .map_err(err("pidfd membri"))?;
    let info: Vec<String> = pids
        .iter()
        .map(|&p| {
            let (g, s) = pgrp_sid(p).unwrap_or((-1, -1));
            format!("{p}:pgid={g},sid={s}")
        })
        .collect();
    if mode == "pgid-escape" {
        println!("INFO c2 membri {}", info.join(" "));
    }
    let t0 = Instant::now();
    kill_raw(-leader, libc::SIGKILL).map_err(err("kill(-pgid)"))?;
    let refs: Vec<&OwnedFd> = fds.iter().collect();
    let ok = wait_all_exit(
        &refs,
        t0 + if mode == "pgid" {
            TIMEOUT
        } else {
            Duration::from_secs(1)
        },
    )
    .map_err(err("poll"))?;
    let d = t0.elapsed().as_secs_f64() * 1e3;
    let mut sopravvissuti = 0;
    for f in &fds {
        if !wait_all_exit(&[f], Instant::now()).unwrap_or(false) {
            sopravvissuti += 1;
            let _ = pidfd_send_signal(f, libc::SIGKILL);
        }
    }
    let _ = pidfd_send_signal(
        &pidfd_open(sup.id() as i32).map_err(err("pidfd sup"))?,
        libc::SIGKILL,
    );
    let _ = sup.wait();
    Ok((ok.then_some(d), sopravvissuti))
}

fn campione_ui_crash() -> Res<Option<f64>> {
    let (mut ui, mut rd) = spawn_role(&["role-ui"])?;
    let sup = read_tag(&mut rd, "SUP")?;
    let vm = read_tag(&mut rd, "CHILD")?;
    wait_comm(vm, "sleep")?;
    let sfd = pidfd_open(sup).map_err(err("pidfd sup"))?;
    let vfd = pidfd_open(vm).map_err(err("pidfd vm"))?;
    let ufd = pidfd_open(ui.id() as i32).map_err(err("pidfd ui"))?;
    let t0 = Instant::now();
    pidfd_send_signal(&ufd, libc::SIGKILL).map_err(err("SIGKILL ui"))?;
    let ok = wait_all_exit(&[&sfd, &vfd], t0 + TIMEOUT).map_err(err("poll"))?;
    let d = t0.elapsed().as_secs_f64() * 1e3;
    let _ = ui.wait();
    if !ok {
        let _ = pidfd_send_signal(&vfd, libc::SIGKILL);
        let _ = pidfd_send_signal(&sfd, libc::SIGKILL);
        return Ok(None);
    }
    Ok(Some(d))
}

// ---------------------------------------------------------------------------
// Scenari cgroup v2
// ---------------------------------------------------------------------------

fn wr(p: &Path, v: &str) -> Res<()> {
    fs::write(p, v).map_err(|e| format!("scrittura {} <- {v:?}: {e}", p.display()))
}

fn rd_kv(p: &Path, key: &str) -> Option<u64> {
    fs::read_to_string(p).ok()?.lines().find_map(|l| {
        let mut it = l.split_whitespace();
        (it.next()? == key).then(|| it.next()?.parse().ok())?
    })
}

fn scenari_cgroup(samples: usize) -> Res<()> {
    let self_cg = fs::read_to_string("/proc/self/cgroup").map_err(err("/proc/self/cgroup"))?;
    let rel = self_cg
        .lines()
        .find_map(|l| l.strip_prefix("0::"))
        .ok_or("cgroup v2 non montato in modalità unificata")?
        .trim()
        .to_owned();
    let base = PathBuf::from(format!("/sys/fs/cgroup{rel}"));
    println!("INFO cgroup_corrente={rel}");
    println!(
        "INFO controller_disponibili={:?}",
        fs::read_to_string(base.join("cgroup.controllers"))
            .unwrap_or_default()
            .trim()
    );
    let leaf = base.join("supervisor");
    if let Err(e) = fs::create_dir(&leaf)
        && e.kind() != io::ErrorKind::AlreadyExists
    {
        println!(
            "RISULTATO scenario=d_cgroup_delega disponibile=false motivo=\"mkdir {}: {e}\"",
            leaf.display()
        );
        return Ok(());
    }
    // Regola "nessun processo interno": il supervisor si sposta in una foglia prima di
    // abilitare i controller per i sotto-cgroup.
    if let Err(e) = wr(&leaf.join("cgroup.procs"), &std::process::id().to_string()) {
        println!("RISULTATO scenario=d_cgroup_delega disponibile=false motivo=\"{e}\"");
        let _ = fs::remove_dir(&leaf);
        return Ok(());
    }
    if let Err(e) = wr(&base.join("cgroup.subtree_control"), "+cpu +memory +pids") {
        println!("RISULTATO scenario=d_cgroup_delega disponibile=false motivo=\"{e}\"");
        return Ok(());
    }
    println!(
        "RISULTATO scenario=d_cgroup_delega disponibile=true base={}",
        base.display()
    );

    let exe = std::env::current_exe().map_err(err("current_exe"))?;

    // Memoria: memory.max 128 MiB, swap 0, processo che tenta di allocare 512 MiB.
    let cg = base.join("vm-mem");
    let _ = fs::create_dir(&cg);
    wr(&cg.join("memory.max"), "134217728")?;
    let swap = wr(&cg.join("memory.swap.max"), "0").is_ok();
    let mut c = Command::new(&exe);
    c.args(["hog-mem", "512"]);
    into_cgroup(&mut c, &cg)?;
    let st = c.status().map_err(err("hog-mem"))?;
    use std::os::unix::process::ExitStatusExt;
    println!(
        "RISULTATO scenario=d_memory_max limite_mib=128 richiesta_mib=512 swap_max_0={swap} segnale_uscita={:?} codice={:?} oom_kill={:?} memory_peak={:?}",
        st.signal(),
        st.code(),
        rd_kv(&cg.join("memory.events"), "oom_kill"),
        fs::read_to_string(cg.join("memory.peak"))
            .ok()
            .map(|s| s.trim().to_owned()),
    );
    let _ = fs::remove_dir(&cg);

    // CPU: cpu.max 20 ms ogni 100 ms (0,2 CPU) contro controllo senza limite.
    for (nome, max) in [
        ("d_cpu_max_20pct", "20000 100000"),
        ("d_cpu_controllo_senza_limite", "max 100000"),
    ] {
        let cg = base.join(format!("vm-{nome}"));
        let _ = fs::create_dir(&cg);
        wr(&cg.join("cpu.max"), max)?;
        let stat = cg.join("cpu.stat");
        let u0 = rd_kv(&stat, "usage_usec").unwrap_or(0);
        let mut c = Command::new(&exe);
        c.args(["hog-cpu", "4", "3"]);
        into_cgroup(&mut c, &cg)?;
        let t0 = Instant::now();
        c.status().map_err(err("hog-cpu"))?;
        let wall = t0.elapsed().as_secs_f64();
        let used = (rd_kv(&stat, "usage_usec").unwrap_or(0) - u0) as f64 / 1e6;
        println!(
            "RISULTATO scenario={nome} cpu_max=\"{max}\" thread=4 durata_s={wall:.3} cpu_usata_s={used:.3} core_medi={:.3} nr_throttled={:?} throttled_usec={:?}",
            used / wall,
            rd_kv(&stat, "nr_throttled"),
            rd_kv(&stat, "throttled_usec"),
        );
        let _ = fs::remove_dir(&cg);
    }

    // pids.max: limita i processi del gruppo.
    let cg = base.join("vm-pids");
    let _ = fs::create_dir(&cg);
    wr(&cg.join("pids.max"), "4")?;
    // La shell conta come un processo: con limite 4 al massimo 3 `sleep` partono.
    let mut c = Command::new("sh");
    c.args([
        "-c",
        "for i in 1 2 3 4 5 6 7 8; do sleep 2 & echo avviato $i; done; wait",
    ]);
    into_cgroup(&mut c, &cg)?;
    let out = c.output().map_err(err("pids"))?;
    let avviati = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.starts_with("avviato"))
        .count();
    println!(
        "RISULTATO scenario=d_pids_max limite=4 sleep_avviati={avviati}/8 uscita_shell={:?} stderr={:?} pids_events_max={:?}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr).trim(),
        rd_kv(&cg.join("pids.events"), "max"),
    );
    let _ = fs::remove_dir(&cg);

    // cgroup.kill: termina tutto il sotto-albero, incluso un processo fuggito con setsid.
    let mut s = Serie::default();
    let mut s_pop = Serie::default();
    let mut membri_min = usize::MAX;
    for i in 0..samples {
        match campione_cgroup_kill(&base, i) {
            Ok((m, p, n)) => {
                membri_min = membri_min.min(n);
                match m {
                    Some(d) => s.ms.push(d),
                    None => {
                        s.timeout += 1;
                        s.orfani += 1;
                    }
                }
                match p {
                    Some(d) => s_pop.ms.push(d),
                    None => s_pop.timeout += 1,
                }
            }
            Err(e) => s.errori.push(format!("campione {i}: {e}")),
        }
    }
    println!("INFO cgroup_kill membri_per_campione_min={membri_min} (sh, sleep, setsid sleep)");
    s.report("d_cgroup_kill_fine_membri_pidfd");
    s_pop.report("d_cgroup_kill_populated_0");
    Ok(())
}

/// Restituisce (ms fino alla fine di tutti i membri, ms fino a `populated 0`,
/// numero di membri osservati).
fn campione_cgroup_kill(base: &Path, i: usize) -> Res<(Option<f64>, Option<f64>, usize)> {
    let cg = base.join(format!("vm-kill-{i}"));
    let _ = fs::create_dir(&cg);
    let mut c = Command::new("sh");
    c.args(["-c", "setsid sleep 1000 & sleep 1000 & echo READY; wait"])
        .stdout(Stdio::piped());
    into_cgroup(&mut c, &cg)?;
    let mut ch = c.spawn().map_err(err("spawn"))?;
    let mut line = String::new();
    BufReader::new(ch.stdout.take().ok_or("stdout")?)
        .read_line(&mut line)
        .map_err(err("READY"))?;
    // Attende che i tre processi siano nel cgroup e abbiano eseguito exec.
    let procs = cg.join("cgroup.procs");
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let list = fs::read_to_string(&procs).unwrap_or_default();
        let pids: Vec<i32> = list.lines().filter_map(|l| l.parse().ok()).collect();
        let sleeps = pids
            .iter()
            .filter(|p| {
                fs::read_to_string(format!("/proc/{p}/comm"))
                    .map(|s| s.trim() == "sleep")
                    .unwrap_or(false)
            })
            .count();
        if pids.len() >= 3 && sleeps >= 2 {
            break;
        }
        if Instant::now() > deadline {
            return Err(format!("cgroup non popolato: {list:?}"));
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let members: Vec<OwnedFd> = fs::read_to_string(&procs)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.parse().ok())
        .filter_map(|p| pidfd_open(p).ok())
        .collect();
    let mut ev = fs::File::open(cg.join("cgroup.events")).map_err(err("cgroup.events"))?;
    let t0 = Instant::now();
    wr(&cg.join("cgroup.kill"), "1")?;
    // Prima misura: fine effettiva di tutti i membri (pidfd).
    let refs: Vec<&OwnedFd> = members.iter().collect();
    let morti = wait_all_exit(&refs, t0 + TIMEOUT).map_err(err("poll membri"))?;
    let d_membri = t0.elapsed().as_secs_f64() * 1e3;
    // Seconda misura: `populated 0` in cgroup.events (lettura ogni <= 1 ms; il kernel
    // limita la frequenza delle notifiche di cgroup.events).
    let mut ok = false;
    while Instant::now() < t0 + TIMEOUT {
        let mut s = String::new();
        ev.rewind().map_err(err("seek"))?;
        ev.read_to_string(&mut s).map_err(err("read events"))?;
        if s.lines().any(|l| l == "populated 0") {
            ok = true;
            break;
        }
        let mut p = libc::pollfd {
            fd: ev.as_raw_fd(),
            events: libc::POLLPRI,
            revents: 0,
        };
        // SAFETY: `p` è un singolo pollfd valido per la durata della chiamata.
        unsafe { libc::poll(&mut p, 1, 1) };
    }
    let d = t0.elapsed().as_secs_f64() * 1e3;
    let _ = ch.wait();
    let _ = fs::remove_dir(&cg);
    Ok((morti.then_some(d_membri), ok.then_some(d), members.len()))
}
