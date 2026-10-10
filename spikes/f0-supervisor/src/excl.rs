//! Disk exclusion without relying on QEMU's lock (F0-05, item 7).
//!
//! The supervisor keeps `base.qcow2` (and, in variants, the overlay) open with
//! `CreateFile` through `OpenOptions` + `share_mode`, and checks what QEMU,
//! `qemu-img`, `qemu-io` and the filesystem can still do.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use f0_supervisor::win::ConsoleMode;
use f0_supervisor::{EnvProfile, Job, JobLimits, Log, Vm, VmConfig, qemu_img, qemu_io};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
};

const DIR: &str = r"E:\OpenVM\lab\vms\f0-05\excl";

fn sha256_of(p: &Path) -> String {
    // Hash via certutil so as not to add dependencies to this spike.
    Command::new("certutil.exe")
        .args(["-hashfile", p.to_str().unwrap_or(""), "SHA256"])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .nth(1)
                .unwrap_or("?")
                .trim()
                .to_string()
        })
        .unwrap_or_else(|_| "?".into())
}

fn tool(exe: &Path, args: &[&str]) -> (Option<i32>, String) {
    match Command::new(exe).args(args).current_dir(DIR).output() {
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
        Err(e) => (None, format!("spawn failed: {e}")),
    }
}

fn open_share(p: &Path, write: bool, share: u32) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(write)
        .share_mode(share)
        .open(p)
}

fn share_name(s: u32) -> String {
    let mut v = Vec::new();
    if s & FILE_SHARE_READ != 0 {
        v.push("READ");
    }
    if s & FILE_SHARE_WRITE != 0 {
        v.push("WRITE");
    }
    if s & FILE_SHARE_DELETE != 0 {
        v.push("DELETE");
    }
    if v.is_empty() {
        "0".into()
    } else {
        v.join("|")
    }
}

struct Lab {
    base: PathBuf,
    overlay: PathBuf,
    vars: PathBuf,
}

fn prepare(log: &mut Log) -> io::Result<Lab> {
    std::fs::create_dir_all(DIR)?;
    let base = Path::new(DIR).join("base.qcow2");
    let overlay = Path::new(DIR).join("overlay.qcow2");
    let vars = Path::new(DIR).join("run-vars.fd");
    for p in [&base, &overlay, &vars] {
        let _ = std::fs::remove_file(p);
    }
    let r = tool(
        &qemu_img(),
        &["create", "-q", "-f", "qcow2", "base.qcow2", "20G"],
    );
    log.line(&format!("qemu-img create base: {r:?}"));
    let r = tool(
        &qemu_io(),
        &["-f", "qcow2", "-c", "write -P 0x11 0 1M", "base.qcow2"],
    );
    log.line(&format!("qemu-io writes 1 MiB 0x11 into the base: {r:?}"));
    let r = tool(
        &qemu_img(),
        &[
            "create",
            "-q",
            "-f",
            "qcow2",
            "-b",
            "base.qcow2",
            "-F",
            "qcow2",
            "overlay.qcow2",
        ],
    );
    log.line(&format!("qemu-img create overlay: {r:?}"));
    std::fs::copy(f0_supervisor::fw_vars_template(), &vars)?;
    Ok(Lab {
        base,
        overlay,
        vars,
    })
}

fn start_qemu(lab: &Lab, tag: &str) -> io::Result<Vm> {
    let cfg = VmConfig {
        tag: tag.into(),
        mem_mb: 1024,
        smp: 1,
        iso: false,
        overlay: Some(lab.overlay.clone()),
        vars: Some(lab.vars.clone()),
        qmp_sock: Path::new(r"E:\OpenVM\lab\vms\f0-06\priv").join(format!("{tag}.sock")),
        paused: false,
        env: EnvProfile::SystemRootOnly,
        console: ConsoleMode::NoWindow,
    };
    let mut job = Job::create(JobLimits::default())?;
    job.attach_port()?;
    let vm = Vm::start(&cfg, job, Path::new(r"E:\OpenVM\lab\logs"))?;
    std::thread::sleep(Duration::from_secs(3));
    Ok(vm)
}

fn qemu_state(vm: &Vm, log: &mut Log, tag: &str) -> bool {
    match vm.wait(Duration::ZERO) {
        Ok(None) => {
            let sock = Path::new(r"E:\OpenVM\lab\vms\f0-06\priv").join(format!("{tag}.sock"));
            if let Ok(mut q) = f0_supervisor::qmp::Qmp::connect(&sock, Duration::from_secs(5))
                && let Ok(nodes) = q.execute("query-named-block-nodes", None)
            {
                let list: Vec<String> = nodes
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .map(|n| {
                                format!(
                                    "{}:{}:ro={}",
                                    n["node-name"].as_str().unwrap_or("?"),
                                    Path::new(n["file"].as_str().unwrap_or("?"))
                                        .file_name()
                                        .map(|f| f.to_string_lossy().to_string())
                                        .unwrap_or_default(),
                                    n["ro"]
                                )
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                log.line(&format!("  QEMU alive, nodes: {list:?}"));
            }
            true
        }
        Ok(Some(code)) => {
            let err = std::fs::read_to_string(&vm.stderr).unwrap_or_default();
            log.line(&format!(
                "  QEMU EXITED code={code} stderr={}",
                err.trim().replace(['\r', '\n'], " | ")
            ));
            false
        }
        Err(e) => {
            log.line(&format!("  wait error: {e}"));
            false
        }
    }
}

fn attempts_on_base(lab: &Lab, log: &mut Log) {
    let checks: Vec<(&str, PathBuf, Vec<&str>)> = vec![
        ("qemu-img info base", qemu_img(), vec!["info", "base.qcow2"]),
        (
            "qemu-io -r read base",
            qemu_io(),
            vec![
                "-r",
                "-f",
                "qcow2",
                "-c",
                "read -P 0x11 0 64k",
                "base.qcow2",
            ],
        ),
        (
            "qemu-io write base",
            qemu_io(),
            vec!["-f", "qcow2", "-c", "write -P 0xee 0 64k", "base.qcow2"],
        ),
        (
            "qemu-img resize base +1G",
            qemu_img(),
            vec!["resize", "base.qcow2", "+1G"],
        ),
        (
            "qemu-img snapshot -c base",
            qemu_img(),
            vec!["snapshot", "-c", "s1", "base.qcow2"],
        ),
        (
            "qemu-img commit overlay→base",
            qemu_img(),
            vec!["commit", "overlay.qcow2"],
        ),
        (
            "qemu-img check base",
            qemu_img(),
            vec!["check", "base.qcow2"],
        ),
    ];
    for (name, exe, args) in checks {
        let (code, out) = tool(&exe, &args);
        let short: String = out.chars().take(220).collect();
        log.line(&format!("  {name}: exit={code:?} {short}"));
    }
    let tmp = lab.base.with_extension("renamed");
    log.line(&format!(
        "  rename base: {:?}",
        std::fs::rename(&lab.base, &tmp).map_err(|e| e.to_string())
    ));
    if tmp.exists() {
        let _ = std::fs::rename(&tmp, &lab.base);
    }
    log.line(&format!(
        "  delete base: {:?}",
        std::fs::remove_file(&lab.base).map_err(|e| e.to_string())
    ));
    log.line(&format!(
        "  open for writing (share R|W|D) from the test process: {:?}",
        open_share(
            &lab.base,
            true,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
    ));
    let ro = std::fs::metadata(&lab.base).map(|m| m.permissions());
    if let Ok(mut perm) = ro {
        perm.set_readonly(true);
        log.line(&format!(
            "  set read-only attribute: {:?}",
            std::fs::set_permissions(&lab.base, perm.clone()).map_err(|e| e.to_string())
        ));
        #[allow(clippy::permissions_set_readonly_false)]
        perm.set_readonly(false);
        let _ = std::fs::set_permissions(&lab.base, perm);
    }
}

fn attempts_on_overlay(lab: &Lab, log: &mut Log) {
    let checks: Vec<(&str, PathBuf, Vec<&str>)> = vec![
        (
            "qemu-img info overlay",
            qemu_img(),
            vec!["info", "overlay.qcow2"],
        ),
        (
            "qemu-io write overlay",
            qemu_io(),
            vec!["-f", "qcow2", "-c", "write -P 0xcd 2M 64k", "overlay.qcow2"],
        ),
        (
            "qemu-img resize overlay +1G",
            qemu_img(),
            vec!["resize", "overlay.qcow2", "+1G"],
        ),
    ];
    for (name, exe, args) in checks {
        let (code, out) = tool(&exe, &args);
        let short: String = out.chars().take(220).collect();
        log.line(&format!("  {name}: exit={code:?} {short}"));
    }
    log.line(&format!(
        "  delete overlay: {:?}",
        std::fs::remove_file(&lab.overlay).map_err(|e| e.to_string())
    ));
}

/// Probes the share modes with which QEMU opened a file: tries
/// opens with different access and share modes while QEMU keeps it open.
fn probe_qemu_share(p: &Path, log: &mut Log) {
    let all = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
    for (write, share) in [
        (false, all),
        (true, all),
        (false, FILE_SHARE_READ),
        (false, FILE_SHARE_READ | FILE_SHARE_WRITE),
    ] {
        let r = open_share(p, write, share)
            .map(|_| ())
            .map_err(|e| e.to_string());
        log.line(&format!(
            "  open {} access={} share={} → {r:?}",
            p.file_name().unwrap_or_default().to_string_lossy(),
            if write { "RW" } else { "R" },
            share_name(share)
        ));
    }
}

pub(crate) fn run(logs: &Path) -> io::Result<()> {
    let mut log = Log::create(&logs.join("f0-05-exclusion.log"))?;

    // --- Control: no supervisor handle (reproduces F0-02 8.3).
    log.line("== T0 control: QEMU with overlay, no supervisor handle");
    let lab = prepare(&mut log)?;
    let h0 = sha256_of(&lab.base);
    log.line(&format!("  initial base sha256={h0}"));
    let vm = start_qemu(&lab, "f0-05-excl-t0")?;
    qemu_state(&vm, &mut log, "f0-05-excl-t0");
    log.line("  probe of QEMU's share modes on the base:");
    probe_qemu_share(&lab.base, &mut log);
    log.line("  probe of QEMU's share modes on the overlay:");
    probe_qemu_share(&lab.overlay, &mut log);
    let (code, out) = tool(
        &qemu_io(),
        &["-f", "qcow2", "-c", "write -P 0xee 0 64k", "base.qcow2"],
    );
    log.line(&format!(
        "  qemu-io write base (without protection): exit={code:?} {out}"
    ));
    log.line(&format!("  base sha256 after={}", sha256_of(&lab.base)));
    vm.emergency_stop(Duration::from_secs(10))?;
    drop(vm);

    // --- T1: base opened by the supervisor with FILE_SHARE_READ before QEMU.
    log.line("== T1 supervisor opens base.qcow2 (access R, share READ) BEFORE QEMU");
    let lab = prepare(&mut log)?;
    let h1 = sha256_of(&lab.base);
    let guard = open_share(&lab.base, false, FILE_SHARE_READ)?;
    log.line(&format!("  supervisor handle open; base sha256={h1}"));
    let vm = start_qemu(&lab, "f0-05-excl-t1")?;
    let alive = qemu_state(&vm, &mut log, "f0-05-excl-t1");
    log.line(&format!(
        "  QEMU started with overlay and backing base: {alive}"
    ));
    attempts_on_base(&lab, &mut log);
    log.line("  overlay (no supervisor handle on the overlay):");
    attempts_on_overlay(&lab, &mut log);
    let still = qemu_state(&vm, &mut log, "f0-05-excl-t1");
    log.line(&format!("  QEMU still alive after the attempts: {still}"));
    vm.emergency_stop(Duration::from_secs(10))?;
    drop(vm);
    drop(guard);
    let h1b = sha256_of(&lab.base);
    log.line(&format!(
        "  base sha256 after T1={h1b} unchanged={}",
        h1 == h1b
    ));

    // --- T1b: supervisor opens the base AFTER QEMU.
    log.line("== T1b supervisor opens base.qcow2 (R, share READ) AFTER QEMU has started");
    let lab = prepare(&mut log)?;
    let vm = start_qemu(&lab, "f0-05-excl-t1b")?;
    qemu_state(&vm, &mut log, "f0-05-excl-t1b");
    let g = open_share(&lab.base, false, FILE_SHARE_READ);
    log.line(&format!(
        "  supervisor open: {:?}",
        g.as_ref().map(|_| ()).map_err(|e| e.to_string())
    ));
    if g.is_ok() {
        let (code, out) = tool(
            &qemu_io(),
            &["-f", "qcow2", "-c", "write -P 0xee 0 64k", "base.qcow2"],
        );
        log.line(&format!("  qemu-io write base: exit={code:?} {out}"));
    }
    vm.emergency_stop(Duration::from_secs(10))?;
    drop(vm);
    drop(g);

    // --- T2: overlay opened by the supervisor BEFORE QEMU.
    for share in [FILE_SHARE_READ | FILE_SHARE_WRITE, FILE_SHARE_READ] {
        log.line(&format!(
            "== T2 supervisor opens overlay (R, share {}) BEFORE QEMU; base protected as in T1",
            share_name(share)
        ));
        let lab = prepare(&mut log)?;
        let gb = open_share(&lab.base, false, FILE_SHARE_READ)?;
        let go = open_share(&lab.overlay, false, share)?;
        let tag = format!("f0-05-excl-t2-{}", share);
        let vm = start_qemu(&lab, &tag)?;
        let alive = qemu_state(&vm, &mut log, &tag);
        if alive {
            attempts_on_overlay(&lab, &mut log);
            vm.emergency_stop(Duration::from_secs(10))?;
        }
        drop(vm);
        drop(go);
        drop(gb);
    }

    // --- T3: overlay opened by the supervisor AFTER QEMU.
    for share in [FILE_SHARE_READ | FILE_SHARE_WRITE, FILE_SHARE_READ] {
        log.line(&format!(
            "== T3 supervisor opens overlay (R, share {}) AFTER QEMU",
            share_name(share)
        ));
        let lab = prepare(&mut log)?;
        let gb = open_share(&lab.base, false, FILE_SHARE_READ)?;
        let tag = format!("f0-05-excl-t3-{}", share);
        let vm = start_qemu(&lab, &tag)?;
        qemu_state(&vm, &mut log, &tag);
        let go = open_share(&lab.overlay, false, share);
        log.line(&format!(
            "  supervisor overlay open: {:?}",
            go.as_ref().map(|_| ()).map_err(|e| e.to_string())
        ));
        if go.is_ok() {
            attempts_on_overlay(&lab, &mut log);
        }
        vm.emergency_stop(Duration::from_secs(10))?;
        drop(vm);
        drop(go);
        drop(gb);
    }

    // --- T4: another process already has the base open for writing.
    log.line("== T4 interactive qemu-io keeps the base open for writing before the supervisor");
    let lab = prepare(&mut log)?;
    let mut qio = Command::new(qemu_io())
        .args(["-f", "qcow2", "base.qcow2"])
        .current_dir(DIR)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    std::thread::sleep(Duration::from_secs(1));
    let g = open_share(&lab.base, false, FILE_SHARE_READ);
    log.line(&format!(
        "  supervisor open (R, share READ) with an existing writer: {:?}",
        g.as_ref().map(|_| ()).map_err(|e| e.to_string())
    ));
    // Observed: qemu-io with piped stdin does not process "quit" on Windows and keeps
    // waiting; after the attempt it is killed.
    if let Some(mut stdin) = qio.stdin.take() {
        let _ = stdin.write_all(b"quit\n");
    }
    std::thread::sleep(Duration::from_secs(2));
    if qio.try_wait()?.is_none() {
        log.line("  qemu-io did not process 'quit' from piped stdin: killed");
        let _ = qio.kill();
    }
    let _ = qio.wait();
    let mut s = String::new();
    if let Some(mut o) = qio.stdout.take() {
        let _ = o.read_to_string(&mut s);
    }
    drop(g);
    let g2 = open_share(&lab.base, false, FILE_SHARE_READ);
    log.line(&format!(
        "  supervisor open after qemu-io closed: {:?}",
        g2.as_ref().map(|_| ()).map_err(|e| e.to_string())
    ));
    drop(g2);

    // --- T5: baseline vars protected with share READ.
    log.line("== T5 baseline vars opened by the supervisor (R, share READ)");
    let bvars = Path::new(DIR).join("baseline-vars.fd");
    std::fs::copy(f0_supervisor::fw_vars_template(), &bvars)?;
    let gv = open_share(&bvars, false, FILE_SHARE_READ)?;
    let copy_to = Path::new(DIR).join("copy-vars.fd");
    log.line(&format!(
        "  copy for the run: {:?}",
        std::fs::copy(&bvars, &copy_to).map_err(|e| e.to_string())
    ));
    log.line(&format!(
        "  write to the baseline vars: {:?}",
        OpenOptions::new()
            .write(true)
            .open(&bvars)
            .map(|_| ())
            .map_err(|e| e.to_string())
    ));
    drop(gv);
    for p in [&copy_to, &bvars, &lab.vars] {
        let _ = std::fs::remove_file(p);
    }
    let r = f0_supervisor::qemu_residuals("lightvm-f0-05-excl");
    log.line(&format!("residuals (host total, spike)={r:?}"));
    Ok(())
}
