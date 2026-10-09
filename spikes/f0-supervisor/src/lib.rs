//! Spike F0-06 / F0-08: supervisione di QEMU su Windows.
//!
//! Non è codice di prodotto. Dimostra: spawn senza shell con ambiente ripulito
//! ed eredità limitata degli handle, appartenenza a un Job Object con
//! `KILL_ON_JOB_CLOSE`, identità del processo tramite handle e tempo di
//! creazione, Emergency Stop con `TerminateJobObject`, limiti CPU/memoria.
//!
//! `unsafe_code` è negato a livello di workspace. Qui è consentito perché le
//! API Job Object usate (CPU rate control, `PROC_THREAD_ATTRIBUTE_JOB_LIST`,
//! `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`, porta di completamento del job) non
//! sono esposte da `std` né dal crate `win32job`. Tutto il codice `unsafe` è
//! confinato nel modulo `win`, con un commento `SAFETY:` per ogni blocco.
#![cfg(windows)]
#![allow(unsafe_code)]
#![allow(unreachable_pub)]

pub mod qmp;
pub mod win;

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub use win::{ConsoleMode, Job, JobLimits, Spawned};

/// Radice del laboratorio.
pub const LAB: &str = r"E:\OpenVM\lab";
pub const QEMU_DIR: &str = r"E:\OpenVM\lab\qemu\qemu-11.1.0";
pub const ISO: &str = r"E:\OpenVM\lab\media\ubuntu-26.04.1-desktop-amd64.iso";

pub fn qemu_exe() -> PathBuf {
    Path::new(QEMU_DIR).join("qemu-system-x86_64.exe")
}
pub fn qemu_img() -> PathBuf {
    Path::new(QEMU_DIR).join("qemu-img.exe")
}
pub fn qemu_io() -> PathBuf {
    Path::new(QEMU_DIR).join("qemu-io.exe")
}
pub fn fw_code() -> PathBuf {
    Path::new(QEMU_DIR).join(r"share\edk2-x86_64-code.fd")
}
pub fn fw_vars_template() -> PathBuf {
    Path::new(QEMU_DIR).join(r"share\edk2-i386-vars.fd")
}

/// Ambiente minimo passato a QEMU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvProfile {
    /// Nessuna variabile.
    Empty,
    /// Solo `SystemRoot` (richiesta da Winsock per inizializzarsi).
    SystemRootOnly,
}

pub fn env_for(profile: EnvProfile) -> Vec<(OsString, OsString)> {
    match profile {
        EnvProfile::Empty => Vec::new(),
        EnvProfile::SystemRootOnly => {
            let sr = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
            vec![("SystemRoot".into(), sr)]
        }
    }
}

/// Rifiuta percorsi che la grammatica delle opzioni QEMU interpreterebbe
/// (virgole) o che non sono assoluti/UTF-8.
pub fn qemu_path(p: &Path) -> io::Result<String> {
    let s = p
        .to_str()
        .ok_or_else(|| io::Error::other("percorso non UTF-8"))?;
    if !p.is_absolute() || s.contains(',') || s.contains('\n') {
        return Err(io::Error::other(format!(
            "percorso non ammesso per QEMU: {s}"
        )));
    }
    Ok(s.to_string())
}

/// Configurazione di una VM di prova.
#[derive(Debug, Clone)]
pub struct VmConfig {
    pub tag: String,
    pub mem_mb: u32,
    pub smp: u32,
    pub iso: bool,
    pub overlay: Option<PathBuf>,
    /// Copia dei vars OVMF per questa esecuzione (se `None`, niente pflash).
    pub vars: Option<PathBuf>,
    pub qmp_sock: PathBuf,
    pub paused: bool,
    pub env: EnvProfile,
    pub console: ConsoleMode,
}

impl VmConfig {
    pub fn args(&self) -> io::Result<Vec<OsString>> {
        let mut a: Vec<String> = vec!["-name".into(), format!("lightvm-{}", self.tag)];
        if self.vars.is_some() {
            a.extend([
                "-machine".into(),
                "q35,pflash0=fwcode,pflash1=fwvars".into(),
            ]);
        } else {
            a.extend(["-machine".into(), "q35".into()]);
        }
        a.extend(
            [
                "-accel",
                "whpx",
                "-cpu",
                "qemu64",
                "-nodefaults",
                "-no-user-config",
                "-nic",
                "none",
            ]
            .map(String::from),
        );
        a.extend([
            "-smp".into(),
            self.smp.to_string(),
            "-m".into(),
            self.mem_mb.to_string(),
        ]);
        if let Some(v) = &self.vars {
            a.extend([
                "-blockdev".into(),
                format!(
                    "driver=file,node-name=fwcode,filename={},read-only=on",
                    qemu_path(&fw_code())?
                ),
                "-blockdev".into(),
                format!("driver=file,node-name=fwvars,filename={}", qemu_path(v)?),
            ]);
        }
        if self.iso {
            a.extend([
                "-blockdev".into(),
                format!(
                    "driver=file,node-name=isofile,filename={},read-only=on",
                    qemu_path(Path::new(ISO))?
                ),
                "-blockdev".into(),
                "driver=raw,node-name=iso,file=isofile,read-only=on".into(),
                "-device".into(),
                "ide-cd,drive=iso,bus=ide.0,bootindex=0".into(),
            ]);
        }
        if let Some(o) = &self.overlay {
            a.extend([
                "-blockdev".into(),
                format!("driver=file,node-name=ovlfile,filename={}", qemu_path(o)?),
                "-blockdev".into(),
                "driver=qcow2,node-name=ovl,file=ovlfile".into(),
                "-device".into(),
                "virtio-blk-pci,drive=ovl,bootindex=1".into(),
            ]);
        }
        a.extend(
            [
                "-device",
                "virtio-vga",
                "-device",
                "virtio-tablet-pci",
                "-device",
                "virtio-keyboard-pci",
                "-display",
                "none",
            ]
            .map(String::from),
        );
        a.extend([
            "-qmp".into(),
            format!("unix:{},server=on,wait=off", qemu_path(&self.qmp_sock)?),
        ]);
        if self.paused {
            a.push("-S".into());
        }
        Ok(a.into_iter().map(OsString::from).collect())
    }
}

/// VM avviata: job posseduto, handle di processo, identità.
#[derive(Debug)]
pub struct Vm {
    pub job: Job,
    pub proc: Spawned,
    pub identity: Identity,
    pub started: Instant,
    pub stderr: PathBuf,
}

/// Identità di un processo: PID più tempo di creazione (FILETIME).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Identity {
    pub pid: u32,
    pub creation: u64,
}

/// Esito del confronto tra un'identità registrata e il PID attuale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityCheck {
    Same,
    /// Il PID esiste ma appartiene a un processo con tempo di creazione diverso.
    Reused {
        creation: u64,
    },
    /// Nessun processo con quel PID è apribile.
    Absent,
}

impl Identity {
    pub fn of(h: &std::os::windows::io::OwnedHandle, pid: u32) -> io::Result<Identity> {
        let (creation, _, _) = win::process_times(h)?;
        Ok(Identity { pid, creation })
    }

    /// Controlla se il PID registrato indica ancora lo stesso processo.
    pub fn check_pid(&self) -> IdentityCheck {
        match win::open_process(self.pid, false) {
            Ok(h) => match win::process_times(&h) {
                Ok((c, _, _)) if c == self.creation => IdentityCheck::Same,
                Ok((c, _, _)) => IdentityCheck::Reused { creation: c },
                Err(_) => IdentityCheck::Absent,
            },
            Err(_) => IdentityCheck::Absent,
        }
    }
}

impl Vm {
    /// Avvia QEMU nel job indicato (creato dal chiamante con i limiti voluti).
    pub fn start(cfg: &VmConfig, job: Job, logs: &Path) -> io::Result<Vm> {
        let _ = std::fs::remove_file(&cfg.qmp_sock);
        let args = cfg.args()?;
        let env = env_for(cfg.env);
        let stdout = logs.join(format!("{}.stdout.txt", cfg.tag));
        let stderr = logs.join(format!("{}.stderr.txt", cfg.tag));
        let exe = qemu_exe();
        let spec = win::SpawnSpec {
            exe: &exe,
            args: &args,
            env: &env,
            stdout: &stdout,
            stderr: &stderr,
            job: Some(&job),
            console: cfg.console,
            cwd: Some(Path::new(QEMU_DIR)),
        };
        let proc = win::spawn(&spec)?;
        let identity = Identity::of(&proc.process, proc.pid)?;
        Ok(Vm {
            job,
            proc,
            identity,
            started: Instant::now(),
            stderr,
        })
    }

    pub fn wait(&self, timeout: Duration) -> io::Result<Option<u32>> {
        win::wait_process(
            &self.proc.process,
            timeout.as_millis().min(u32::MAX as u128) as u32,
        )
    }

    /// Emergency Stop: `TerminateJobObject`, poi attesa dell'uscita osservata.
    /// Restituisce (ms fino all'uscita di QEMU, ms fino a job vuoto).
    pub fn emergency_stop(&self, timeout: Duration) -> io::Result<(f64, f64)> {
        let t0 = Instant::now();
        self.job.terminate(0xE5E5)?;
        let code = self.wait(timeout)?;
        let t_proc = t0.elapsed().as_secs_f64() * 1000.0;
        if code.is_none() {
            return Err(io::Error::other("QEMU non terminato entro il timeout"));
        }
        loop {
            if self.job.accounting()?.active_processes == 0 {
                break;
            }
            if t0.elapsed() > timeout {
                return Err(io::Error::other("job non vuoto entro il timeout"));
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        Ok((t_proc, t0.elapsed().as_secs_f64() * 1000.0))
    }

    /// Figli diretti di QEMU (per PID padre) con appartenenza al job.
    pub fn children(&self) -> io::Result<Vec<(u32, String, Option<bool>)>> {
        let all = win::list_processes()?;
        Ok(all
            .into_iter()
            .filter(|p| p.ppid == self.proc.pid)
            .map(|p| {
                let inj = win::open_process(p.pid, false)
                    .ok()
                    .and_then(|h| win::is_in_job(&h, Some(&self.job)).ok());
                (p.pid, p.name, inj)
            })
            .collect())
    }
}

/// Conteggio dei processi QEMU sull'host: (totale per nome immagine,
/// di questo spike per riga di comando). Il secondo usa `Get-CimInstance`
/// perché la riga di comando non è nel Toolhelp; altri agenti possono avere
/// QEMU attivi in parallelo e sono esclusi dal secondo conteggio.
pub fn qemu_residuals(marker: &str) -> (usize, Option<usize>) {
    let total = win::list_processes()
        .map(|v| {
            v.iter()
                .filter(|p| p.name.eq_ignore_ascii_case("qemu-system-x86_64.exe"))
                .count()
        })
        .unwrap_or(usize::MAX);
    let script = format!(
        "@(Get-CimInstance Win32_Process -Filter \"Name='qemu-system-x86_64.exe'\" | Where-Object {{ $_.CommandLine -like '*{marker}*' }}).Count"
    );
    let mine = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
        .ok()
        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse().ok());
    (total, mine)
}

/// Statistiche su campioni (nearest-rank).
#[derive(Debug, Clone, Copy)]
pub struct Stats {
    pub n: usize,
    pub min: f64,
    pub p50: f64,
    pub p95: f64,
    pub max: f64,
    pub mean: f64,
}

pub fn stats(v: &[f64]) -> Option<Stats> {
    if v.is_empty() {
        return None;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.total_cmp(b));
    let rank = |p: f64| {
        let r = ((p / 100.0) * s.len() as f64).ceil() as usize;
        s[r.clamp(1, s.len()) - 1]
    };
    Some(Stats {
        n: s.len(),
        min: s[0],
        p50: rank(50.0),
        p95: rank(95.0),
        max: s[s.len() - 1],
        mean: s.iter().sum::<f64>() / s.len() as f64,
    })
}

impl std::fmt::Display for Stats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "n={} min={:.1} p50={:.1} p95={:.1} max={:.1} media={:.1}",
            self.n, self.min, self.p50, self.p95, self.max, self.mean
        )
    }
}

/// Percentuale di pixel non neri e dimensioni di un PPM P6 (screendump).
pub fn ppm_nonblack(path: &Path) -> io::Result<(u32, u32, f64)> {
    let data = std::fs::read(path)?;
    // Intestazione: "P6\n<w> <h>\n<max>\n"
    let mut fields = Vec::new();
    let mut i = 0;
    while fields.len() < 4 && i < data.len() {
        while i < data.len() && data[i].is_ascii_whitespace() {
            i += 1;
        }
        let start = i;
        while i < data.len() && !data[i].is_ascii_whitespace() {
            i += 1;
        }
        fields.push(String::from_utf8_lossy(&data[start..i]).to_string());
    }
    i += 1;
    if fields.len() < 4 || fields[0] != "P6" {
        return Err(io::Error::other("PPM non valido"));
    }
    let w: u32 = fields[1].parse().map_err(io::Error::other)?;
    let h: u32 = fields[2].parse().map_err(io::Error::other)?;
    let px = &data[i.min(data.len())..];
    let total = (w as usize) * (h as usize);
    let nonblack = px
        .as_chunks::<3>()
        .0
        .iter()
        .take(total)
        .filter(|c| c.iter().any(|&b| b > 16))
        .count();
    Ok((w, h, 100.0 * nonblack as f64 / total.max(1) as f64))
}

/// Timestamp UTC leggibile (secondi dall'epoca con millisecondi).
pub fn now_utc() -> String {
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = d.as_secs();
    let (h, m, s) = ((secs / 3600) % 24, (secs / 60) % 60, secs % 60);
    let days = secs / 86400;
    // Conversione civile da giorni (algoritmo di Howard Hinnant).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let dd = doy - (153 * mp + 2) / 5 + 1;
    let mm = if mp < 10 { mp + 3 } else { mp - 9 };
    let yy = if mm <= 2 { y + 1 } else { y };
    format!(
        "{yy:04}-{mm:02}-{dd:02}T{h:02}:{m:02}:{s:02}.{:03}Z",
        d.subsec_millis()
    )
}

/// Logger semplice su file e stdout.
#[derive(Debug)]
pub struct Log {
    file: std::fs::File,
}

impl Log {
    pub fn create(path: &Path) -> io::Result<Log> {
        Ok(Log {
            file: std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)?,
        })
    }
    pub fn line(&mut self, s: &str) {
        use std::io::Write;
        let l = format!("[{}] {s}", now_utc());
        println!("{l}");
        let _ = writeln!(self.file, "{l}");
    }
}
