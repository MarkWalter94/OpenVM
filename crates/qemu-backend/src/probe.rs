//! Rilevamento delle capacità dell'host e probe reale dell'acceleratore
//! (ROADMAP 3, 5, 10 F1; ADR-0007).
//!
//! Un binario trovato non dimostra che la VM possa avviarsi: oltre al parsing
//! dell'output di `--version`, `-accel help`, `-machine help`, `-device help`,
//! `-cpu help` e di `qemu-img`, [`probe_accelerator`] avvia QEMU in pausa con
//! l'acceleratore, la macchina e il modello CPU che verranno usati e verifica
//! via QMP `query-accelerators` quale acceleratore è attivo.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use crate::cmdline::{Accelerator, CpuModel, MachineType, build_probe_command, parse_q35_version};
use crate::compat::RunId;
use crate::error::{BackendError, Result};
use crate::paths::ManagedPath;
use crate::qmp::QmpConfig;

/// Versione di QEMU.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct QemuVersion {
    /// Maggiore.
    pub major: u32,
    /// Minore.
    pub minor: u32,
    /// Micro.
    pub micro: u32,
    /// Descrizione della build tra parentesi, se presente.
    pub package: Option<String>,
}

impl fmt::Display for QemuVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.micro)?;
        if let Some(p) = &self.package {
            write!(f, " ({p})")?;
        }
        Ok(())
    }
}

/// Voce di `-machine help`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MachineEntry {
    /// Nome.
    pub name: String,
    /// Descrizione.
    pub description: String,
    /// Nome di cui è alias.
    pub alias_of: Option<String>,
    /// Deprecata.
    pub deprecated: bool,
    /// Predefinita.
    pub default: bool,
}

/// Firmware UEFI trovato accanto al binario.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FirmwareInfo {
    /// Codice OVMF.
    pub code: PathBuf,
    /// SHA-256 del codice.
    pub code_sha256: String,
    /// Modello delle variabili UEFI.
    pub vars_template: PathBuf,
    /// SHA-256 del modello delle variabili.
    pub vars_sha256: String,
    /// `true` se entrambi gli hash coincidono con quelli verificati in F0-02.
    pub matches_verified: bool,
}

/// Hash verificati in F0-02 (2.3) per QEMU 11.1.0 di qemu.weilnetz.de.
pub const VERIFIED_OVMF_CODE_SHA256: &str =
    "33090cc07675baa5190d9f1e84bf5176b33bcbfa9bacac522961150cdb6dbb2a";
/// Hash verificato del modello delle variabili UEFI.
pub const VERIFIED_OVMF_VARS_SHA256: &str =
    "5d2ac383371b408398accee7ec27c8c09ea5b74a0de0ceea6513388b15be5d1e";

/// Dispositivi richiesti dal comando costruito da [`crate::cmdline`].
pub const REQUIRED_DEVICES: &[&str] = &[
    "virtio-vga",
    "virtio-tablet-pci",
    "virtio-keyboard-pci",
    "virtio-blk-pci",
    "ide-cd",
];

/// Formati immagine richiesti.
pub const REQUIRED_IMAGE_FORMATS: &[&str] = &["qcow2", "raw", "file"];

/// Motivo per cui l'acceleratore non è disponibile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UnavailableReason {
    /// Il binario non include l'acceleratore.
    NotCompiled {
        /// Acceleratori elencati da `-accel help`.
        available: Vec<String>,
    },
    /// QEMU ha rifiutato l'acceleratore all'inizializzazione.
    InitFailed {
        /// Estratto di stderr.
        stderr: String,
    },
    /// QEMU è partito ma l'acceleratore attivo è un altro (per esempio TCG).
    WrongAccelerator {
        /// Acceleratore attivo secondo `query-accelerators`.
        enabled: String,
    },
    /// Configurazione non compatibile (macchina o CPU rifiutate).
    IncompatibleConfiguration {
        /// Estratto di stderr.
        stderr: String,
    },
    /// Il probe non si è concluso.
    ProbeFailed {
        /// Descrizione.
        detail: String,
    },
}

/// Esito del probe reale dell'acceleratore.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AcceleratorStatus {
    /// Acceleratore attivo con la configurazione provata.
    Available {
        /// Acceleratore.
        accelerator: Accelerator,
        /// Macchina usata nel probe.
        machine: String,
        /// Modello CPU usato nel probe.
        cpu: String,
    },
    /// Non disponibile: nessun fallback TCG.
    Unavailable {
        /// Acceleratore richiesto.
        accelerator: Accelerator,
        /// Motivo.
        reason: UnavailableReason,
    },
    /// Probe non ancora eseguito.
    NotProbed,
}

impl AcceleratorStatus {
    /// `true` se l'acceleratore è utilizzabile.
    pub fn is_available(&self) -> bool {
        matches!(self, Self::Available { .. })
    }
}

fn accel_hint(a: Accelerator) -> &'static str {
    match a {
        Accelerator::Whpx => {
            "verificare che la funzionalità Windows \"Piattaforma hypervisor Windows\" \
             (HypervisorPlatform) sia attiva, che la virtualizzazione sia abilitata nel \
             firmware e che il binario QEMU includa WHPX; LightVM non usa l'emulazione TCG"
        }
        Accelerator::Kvm => {
            "verificare che /dev/kvm esista e sia accessibile all'utente (gruppo kvm) e che \
             la virtualizzazione sia abilitata nel firmware; LightVM non usa l'emulazione TCG"
        }
        Accelerator::Hvf => {
            "verificare che il binario QEMU includa HVF e che l'host lo supporti; LightVM non \
             usa l'emulazione TCG"
        }
    }
}

impl fmt::Display for AcceleratorStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Available {
                accelerator,
                machine,
                cpu,
            } => write!(
                f,
                "acceleratore {} attivo (macchina {machine}, CPU {cpu})",
                accelerator.qemu_name()
            ),
            Self::NotProbed => write!(f, "acceleratore non ancora verificato"),
            Self::Unavailable {
                accelerator,
                reason,
            } => {
                let a = accelerator.qemu_name().to_uppercase();
                match reason {
                    UnavailableReason::NotCompiled { available } => write!(
                        f,
                        "{a} non è incluso in questo binario QEMU (acceleratori presenti: {}): {}",
                        available.join(", "),
                        accel_hint(*accelerator)
                    ),
                    UnavailableReason::InitFailed { stderr } => write!(
                        f,
                        "{a} non inizializzato: {}. Messaggio di QEMU: {stderr}",
                        accel_hint(*accelerator)
                    ),
                    UnavailableReason::WrongAccelerator { enabled } => write!(
                        f,
                        "QEMU è partito con l'acceleratore `{enabled}` invece di {a}: avvio \
                         rifiutato (nessun fallback TCG)"
                    ),
                    UnavailableReason::IncompatibleConfiguration { stderr } => write!(
                        f,
                        "configurazione rifiutata da QEMU con {a} (macchina o modello CPU): \
                         {stderr}"
                    ),
                    UnavailableReason::ProbeFailed { detail } => {
                        write!(f, "probe di {a} non concluso: {detail}")
                    }
                }
            }
        }
    }
}

/// Capacità rilevate.
#[derive(Clone, Debug)]
pub struct HostCapabilities {
    /// Binario `qemu-system-x86_64` approvato.
    pub qemu_system: ManagedPath,
    /// SHA-256 del binario.
    pub qemu_system_sha256: String,
    /// Binario `qemu-img`.
    pub qemu_img: ManagedPath,
    /// Versione di QEMU.
    pub version: QemuVersion,
    /// Versione di `qemu-img`.
    pub qemu_img_version: QemuVersion,
    /// Acceleratori compilati.
    pub accelerators: Vec<String>,
    /// Tutte le macchine elencate.
    pub machines: Vec<MachineEntry>,
    /// Macchine q35 versionate non deprecate, dalla più recente.
    pub q35_machines: Vec<MachineType>,
    /// Dispositivi elencati da `-device help` (nomi e alias).
    pub devices: BTreeSet<String>,
    /// Dispositivi richiesti assenti.
    pub missing_devices: Vec<String>,
    /// Modelli CPU disponibili.
    pub cpu_models: BTreeSet<String>,
    /// Formati supportati da `qemu-img`.
    pub image_formats: BTreeSet<String>,
    /// Protocolli remoti presenti nella build (esclusi dall'allowlist).
    pub remote_protocols_present: Vec<String>,
    /// Firmware UEFI.
    pub firmware: Option<FirmwareInfo>,
    /// Esito del probe reale dell'acceleratore.
    pub accelerator: AcceleratorStatus,
}

/// Problema rilevato dal probe, con messaggio per l'utente.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    /// Codice breve stabile.
    pub code: &'static str,
    /// Messaggio per l'utente.
    pub message: String,
}

impl HostCapabilities {
    /// Macchina q35 versionata consigliata (la più recente non deprecata).
    pub fn recommended_machine(&self) -> Option<&MachineType> {
        self.q35_machines.first()
    }

    /// Modello CPU verificato presente nel binario.
    pub fn cpu_model(&self, name: &str) -> Result<CpuModel> {
        let m = CpuModel::nominal(name)?;
        if !self.cpu_models.contains(name) {
            return Err(BackendError::Unsupported {
                what: format!("modello CPU `{name}`"),
                hint: "non elencato da `-cpu help` di questo binario".into(),
            });
        }
        Ok(m)
    }

    /// Macchina q35 versionata presente nel binario.
    pub fn machine(&self, name: &str) -> Result<MachineType> {
        let m = MachineType::versioned_q35(name)?;
        if !self.q35_machines.contains(&m) {
            return Err(BackendError::Unsupported {
                what: format!("macchina `{name}`"),
                hint: "non elencata (o deprecata) in `-machine help` di questo binario".into(),
            });
        }
        Ok(m)
    }

    /// Problemi che impediscono l'avvio di una VM con il profilo LightVM.
    pub fn diagnostics(&self) -> Vec<Diagnostic> {
        let mut d = Vec::new();
        if self.q35_machines.is_empty() {
            d.push(Diagnostic {
                code: "machine_missing",
                message: "nessuna macchina q35 versionata non deprecata disponibile".into(),
            });
        }
        if !self.missing_devices.is_empty() {
            d.push(Diagnostic {
                code: "device_missing",
                message: format!(
                    "dispositivi richiesti assenti dal binario: {}",
                    self.missing_devices.join(", ")
                ),
            });
        }
        let missing_fmt: Vec<_> = REQUIRED_IMAGE_FORMATS
            .iter()
            .filter(|f| !self.image_formats.contains(**f))
            .collect();
        if !missing_fmt.is_empty() {
            d.push(Diagnostic {
                code: "image_format_missing",
                message: format!("qemu-img non supporta: {missing_fmt:?}"),
            });
        }
        match &self.firmware {
            None => d.push(Diagnostic {
                code: "firmware_missing",
                message: "firmware UEFI (edk2-x86_64-code.fd, edk2-i386-vars.fd) non trovato \
                          nella cartella share di QEMU"
                    .into(),
            }),
            Some(fw) if !fw.matches_verified => d.push(Diagnostic {
                code: "firmware_unverified",
                message: format!(
                    "firmware UEFI con hash diverso da quello verificato (codice {})",
                    fw.code_sha256
                ),
            }),
            Some(_) => {}
        }
        if self.version != self.qemu_img_version {
            d.push(Diagnostic {
                code: "version_mismatch",
                message: format!(
                    "qemu-img {} diverso da qemu-system {}",
                    self.qemu_img_version, self.version
                ),
            });
        }
        if !self.accelerator.is_available() {
            d.push(Diagnostic {
                code: "accelerator_unavailable",
                message: self.accelerator.to_string(),
            });
        }
        d
    }
}

// ---------------------------------------------------------------------------
// Parser (output reale di QEMU 11.1.0 nelle fixture `tests/fixtures/`).
// ---------------------------------------------------------------------------

fn parse_version_after(text: &str, marker: &str) -> Option<QemuVersion> {
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix(marker) {
            let rest = rest.trim();
            let (ver, tail) = match rest.split_once(char::is_whitespace) {
                Some((v, t)) => (v, t.trim()),
                None => (rest, ""),
            };
            let mut it = ver.split('.');
            let major = it.next()?.parse().ok()?;
            let minor = it.next()?.parse().ok()?;
            let micro = it.next().unwrap_or("0").parse().ok()?;
            let package = tail
                .strip_prefix('(')
                .and_then(|t| t.strip_suffix(')'))
                .map(str::to_owned);
            return Some(QemuVersion {
                major,
                minor,
                micro,
                package,
            });
        }
    }
    None
}

/// `qemu-system-x86_64 --version`.
pub fn parse_qemu_version(text: &str) -> Option<QemuVersion> {
    parse_version_after(text, "QEMU emulator version")
}

/// `qemu-img --version`.
pub fn parse_qemu_img_version(text: &str) -> Option<QemuVersion> {
    parse_version_after(text, "qemu-img version")
}

/// `-accel help`.
pub fn parse_accel_help(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.ends_with(':'))
        .filter(|l| l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        .map(str::to_owned)
        .collect()
}

/// `-machine help`.
pub fn parse_machine_help(text: &str) -> Vec<MachineEntry> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim_end();
        if line.is_empty() || line.ends_with(':') || line.starts_with(' ') {
            continue;
        }
        let (name, desc) = match line.split_once(char::is_whitespace) {
            Some((n, d)) => (n, d.trim()),
            None => (line, ""),
        };
        let alias_of = desc
            .find("(alias of ")
            .and_then(|i| desc[i + 10..].split(')').next())
            .map(str::to_owned);
        out.push(MachineEntry {
            name: name.to_owned(),
            description: desc.to_owned(),
            alias_of,
            deprecated: desc.contains("(deprecated)"),
            default: desc.contains("(default)"),
        });
    }
    out
}

/// Macchine q35 versionate non deprecate, dalla più recente.
pub fn q35_versioned(machines: &[MachineEntry]) -> Vec<MachineType> {
    let mut v: Vec<(u32, u32, MachineType)> = machines
        .iter()
        .filter(|m| !m.deprecated)
        .filter_map(|m| {
            let (a, b) = parse_q35_version(&m.name)?;
            Some((a, b, MachineType::versioned_q35(&m.name).ok()?))
        })
        .collect();
    v.sort_by_key(|x| std::cmp::Reverse((x.0, x.1)));
    v.into_iter().map(|(_, _, m)| m).collect()
}

/// `-device help`: nomi e alias.
pub fn parse_device_help(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for line in text.lines() {
        for key in ["name \"", "alias \""] {
            let mut rest = line;
            while let Some(i) = rest.find(key) {
                let after = &rest[i + key.len()..];
                if let Some(end) = after.find('"') {
                    out.insert(after[..end].to_owned());
                    rest = &after[end..];
                } else {
                    break;
                }
            }
        }
    }
    out
}

/// `-cpu help`: modelli della sezione "Available CPUs".
pub fn parse_cpu_help(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut in_list = false;
    for line in text.lines() {
        let line = line.trim_end();
        if !in_list {
            if line.trim_start().starts_with("Available CPUs") {
                in_list = true;
            }
            continue;
        }
        if line.trim().is_empty() {
            break;
        }
        if let Some(name) = line.split_whitespace().next() {
            out.insert(name.to_owned());
        }
    }
    out
}

/// `qemu-img --help`: formati della sezione "Supported image formats".
pub fn parse_qemu_img_formats(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut in_list = false;
    for line in text.lines() {
        let t = line.trim();
        if !in_list {
            if t.starts_with("Supported image formats") {
                in_list = true;
            }
            continue;
        }
        if t.is_empty() {
            if out.is_empty() {
                continue;
            }
            break;
        }
        out.extend(t.split_whitespace().map(str::to_owned));
    }
    out
}

/// Classifica lo stderr di un avvio fallito con l'acceleratore richiesto.
pub fn classify_accel_stderr(accel: Accelerator, stderr: &str) -> UnavailableReason {
    let excerpt = stderr_excerpt(stderr);
    let lower = stderr.to_ascii_lowercase();
    let name = accel.qemu_name();
    if lower.contains(&format!("invalid accelerator {name}")) {
        return UnavailableReason::NotCompiled { available: vec![] };
    }
    if lower.contains(&format!("failed to initialize {name}"))
        || lower.contains(&format!("{name}:"))
        || lower.contains("no accelerator found")
    {
        return UnavailableReason::InitFailed { stderr: excerpt };
    }
    if lower.contains("unable to find cpu model")
        || lower.contains("unsupported machine type")
        || lower.contains("not supported by")
    {
        return UnavailableReason::IncompatibleConfiguration { stderr: excerpt };
    }
    UnavailableReason::ProbeFailed { detail: excerpt }
}

/// Ultime righe significative di stderr, senza il prefisso del percorso.
pub fn stderr_excerpt(stderr: &str) -> String {
    let lines: Vec<String> = stderr
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| match l.find(".exe: ") {
            Some(i) => l[i + 6..].to_owned(),
            None => l.to_owned(),
        })
        .collect();
    let start = lines.len().saturating_sub(4);
    let mut s = lines[start..].join(" | ");
    if s.len() > 600 {
        let mut cut = 600;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
    }
    s
}

// ---------------------------------------------------------------------------
// Esecuzione
// ---------------------------------------------------------------------------

/// Output di un comando di probe.
#[derive(Debug)]
pub struct CommandOutput {
    /// Codice d'uscita.
    pub code: Option<i32>,
    /// Stdout.
    pub stdout: String,
    /// Stderr.
    pub stderr: String,
}

/// Esegue `exe args` senza shell, con ambiente minimo, stdin nullo, nessuna
/// console visibile e timeout (il processo viene terminato allo scadere).
pub fn run_capture(exe: &Path, args: &[&str], timeout: Duration) -> Result<CommandOutput> {
    let mut cmd = Command::new(exe);
    cmd.args(args.iter().map(OsStr::new))
        .env_clear()
        .envs(crate::process::minimal_environment())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    if let Some(dir) = exe.parent() {
        cmd.current_dir(dir);
    }
    let mut child = cmd.spawn().map_err(BackendError::Spawn)?;
    let mut out = child.stdout.take();
    let mut err = child.stderr.take();
    let t_out = std::thread::spawn(move || {
        let mut s = Vec::new();
        if let Some(o) = out.as_mut() {
            let _ = o.take(8 * 1024 * 1024).read_to_end(&mut s);
        }
        s
    });
    let t_err = std::thread::spawn(move || {
        let mut s = Vec::new();
        if let Some(e) = err.as_mut() {
            let _ = e.take(1024 * 1024).read_to_end(&mut s);
        }
        s
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(st) = child.try_wait()? {
            break st;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(BackendError::Timeout("comando di probe QEMU"));
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let stdout = String::from_utf8_lossy(&t_out.join().unwrap_or_default()).into_owned();
    let stderr = String::from_utf8_lossy(&t_err.join().unwrap_or_default()).into_owned();
    Ok(CommandOutput {
        code: status.code(),
        stdout,
        stderr,
    })
}

/// SHA-256 esadecimale di un file.
pub fn sha256_file(path: &Path) -> Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Cerca il firmware UEFI nella cartella `share` accanto al binario.
pub fn find_firmware(qemu_system: &Path) -> Result<Option<FirmwareInfo>> {
    let Some(dir) = qemu_system.parent() else {
        return Ok(None);
    };
    let share = dir.join("share");
    let code = share.join("edk2-x86_64-code.fd");
    let vars = share.join("edk2-i386-vars.fd");
    if !code.is_file() || !vars.is_file() {
        return Ok(None);
    }
    let code_sha256 = sha256_file(&code)?;
    let vars_sha256 = sha256_file(&vars)?;
    let matches_verified =
        code_sha256 == VERIFIED_OVMF_CODE_SHA256 && vars_sha256 == VERIFIED_OVMF_VARS_SHA256;
    Ok(Some(FirmwareInfo {
        code,
        code_sha256,
        vars_template: vars,
        vars_sha256,
        matches_verified,
    }))
}

/// Configurazione del probe.
#[derive(Clone, Debug)]
pub struct ProbeConfig {
    /// Timeout per ciascun comando `help`/`--version`.
    pub command_timeout: Duration,
    /// Timeout complessivo del probe dell'acceleratore.
    pub accelerator_timeout: Duration,
}

impl Default for ProbeConfig {
    fn default() -> Self {
        Self {
            command_timeout: Duration::from_secs(15),
            accelerator_timeout: Duration::from_secs(20),
        }
    }
}

/// Rileva le capacità statiche dei binari approvati. Il campo `accelerator`
/// resta [`AcceleratorStatus::NotProbed`] finché non si esegue
/// [`probe_accelerator`].
pub fn probe_host(
    qemu_system: &ManagedPath,
    qemu_img: &ManagedPath,
    cfg: &ProbeConfig,
) -> Result<HostCapabilities> {
    let exe = qemu_system.as_path();
    let run = |args: &[&str]| -> Result<String> {
        let o = run_capture(exe, args, cfg.command_timeout)?;
        if o.code != Some(0) {
            return Err(BackendError::Probe(format!(
                "`{}` è uscito con {:?}: {}",
                args.join(" "),
                o.code,
                stderr_excerpt(&o.stderr)
            )));
        }
        Ok(o.stdout)
    };
    let version_txt = run(&["--version"])?;
    let version = parse_qemu_version(&version_txt).ok_or_else(|| {
        BackendError::Probe(format!(
            "versione non riconosciuta: {}",
            stderr_excerpt(&version_txt)
        ))
    })?;
    let accelerators = parse_accel_help(&run(&["-accel", "help"])?);
    let machines = parse_machine_help(&run(&["-machine", "help"])?);
    let devices = parse_device_help(&run(&["-device", "help"])?);
    let cpu_models = parse_cpu_help(&run(&["-cpu", "help"])?);
    let img_version_out = run_capture(qemu_img.as_path(), &["--version"], cfg.command_timeout)?;
    let qemu_img_version = parse_qemu_img_version(&img_version_out.stdout)
        .ok_or_else(|| BackendError::Probe("versione di qemu-img non riconosciuta".into()))?;
    // `qemu-img --help` esce con codice 0 nelle versioni recenti; si usa
    // comunque l'output anche con codice diverso.
    let img_help = run_capture(qemu_img.as_path(), &["--help"], cfg.command_timeout)?;
    let image_formats = parse_qemu_img_formats(&img_help.stdout);
    let remote_protocols_present = ["http", "https", "ftp", "ftps", "nbd", "nfs", "ssh"]
        .iter()
        .filter(|p| image_formats.contains(**p))
        .map(|p| (*p).to_owned())
        .collect();
    let missing_devices = REQUIRED_DEVICES
        .iter()
        .filter(|d| !devices.contains(**d))
        .map(|d| (*d).to_owned())
        .collect();
    let q35_machines = q35_versioned(&machines);
    Ok(HostCapabilities {
        qemu_system_sha256: sha256_file(exe)?,
        qemu_system: qemu_system.clone(),
        qemu_img: qemu_img.clone(),
        version,
        qemu_img_version,
        accelerators,
        machines,
        q35_machines,
        devices,
        missing_devices,
        cpu_models,
        image_formats,
        remote_protocols_present,
        firmware: find_firmware(exe)?,
        accelerator: AcceleratorStatus::NotProbed,
    })
}

/// Probe reale dell'acceleratore: avvio in pausa (`-S`) con macchina e CPU
/// indicate, `query-accelerators` via QMP, poi `quit`. Il processo è nel
/// contenitore dell'esecuzione e viene terminato in ogni caso.
pub async fn probe_accelerator(
    caps: &HostCapabilities,
    accelerator: Accelerator,
    machine: &MachineType,
    cpu: &CpuModel,
    private_dir: &ManagedPath,
    cfg: &ProbeConfig,
) -> AcceleratorStatus {
    let unavailable = |reason| AcceleratorStatus::Unavailable {
        accelerator,
        reason,
    };
    let tag = match RunId::new(format!("probe-{}", std::process::id())) {
        Ok(t) => t,
        Err(e) => {
            return unavailable(UnavailableReason::ProbeFailed {
                detail: e.to_string(),
            });
        }
    };
    let cmd = match build_probe_command(
        caps.qemu_system.clone(),
        machine,
        accelerator,
        cpu,
        private_dir.clone(),
        &tag,
    ) {
        Ok(c) => c,
        Err(e) => {
            return unavailable(UnavailableReason::ProbeFailed {
                detail: e.to_string(),
            });
        }
    };
    let qmp_cfg = QmpConfig::default();
    let launched = crate::run::launch(&cmd, &qmp_cfg, cfg.accelerator_timeout).await;
    let launched = match launched {
        Ok(l) => l,
        Err(BackendError::StartupFailed { stderr_tail, .. }) => {
            let mut reason = classify_accel_stderr(accelerator, &stderr_tail);
            if let UnavailableReason::NotCompiled { available } = &mut reason {
                available.clone_from(&caps.accelerators);
            }
            return unavailable(reason);
        }
        Err(e) => {
            return unavailable(UnavailableReason::ProbeFailed {
                detail: e.to_string(),
            });
        }
    };
    let result = launched.qmp.query_accelerators().await;
    let _ = launched.qmp.quit().await;
    launched.finish(Duration::from_secs(5)).await;
    match result {
        Ok(info) if info.enabled == accelerator.qemu_name() => AcceleratorStatus::Available {
            accelerator,
            machine: machine.as_str().to_owned(),
            cpu: cpu.as_str().to_owned(),
        },
        Ok(info) => unavailable(UnavailableReason::WrongAccelerator {
            enabled: info.enabled,
        }),
        Err(e) => unavailable(UnavailableReason::ProbeFailed {
            detail: e.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VERSION: &str = include_str!("../tests/fixtures/version.txt");
    const ACCEL: &str = include_str!("../tests/fixtures/accel-help.txt");
    const MACHINE: &str = include_str!("../tests/fixtures/machine-help.txt");
    const DEVICE: &str = include_str!("../tests/fixtures/device-help.txt");
    const CPU: &str = include_str!("../tests/fixtures/cpu-help.txt");
    const IMG_VERSION: &str = include_str!("../tests/fixtures/qemu-img-version.txt");
    const IMG_HELP: &str = include_str!("../tests/fixtures/qemu-img-help.txt");
    const ERR_KVM: &str = include_str!("../tests/fixtures/stderr-accel-kvm.txt");
    const ERR_CPU: &str = include_str!("../tests/fixtures/stderr-cpu-unknown.txt");
    const ERR_WHPX: &str = include_str!("../tests/fixtures/stderr-whpx-noaccel-synthetic.txt");

    #[test]
    fn versione_reale() {
        let v = parse_qemu_version(VERSION).unwrap();
        assert_eq!((v.major, v.minor, v.micro), (11, 1, 0));
        assert_eq!(v.package.as_deref(), Some("v11.1.0-12130-ge470268ff4"));
        let i = parse_qemu_img_version(IMG_VERSION).unwrap();
        assert_eq!(i, v);
        assert!(parse_qemu_version("garbage").is_none());
        assert!(parse_qemu_version("QEMU emulator version x.y").is_none());
    }

    #[test]
    fn acceleratori_reali() {
        assert_eq!(parse_accel_help(ACCEL), vec!["tcg", "whpx"]);
        assert!(parse_accel_help("").is_empty());
    }

    #[test]
    fn macchine_reali() {
        let m = parse_machine_help(MACHINE);
        let q35 = m.iter().find(|e| e.name == "q35").unwrap();
        assert_eq!(q35.alias_of.as_deref(), Some("pc-q35-11.1"));
        assert!(m.iter().any(|e| e.name == "pc-q35-5.2" && e.deprecated));
        assert!(m.iter().any(|e| e.name == "pc-i440fx-11.1" && e.default));
        let v = q35_versioned(&m);
        let names: Vec<_> = v.iter().map(MachineType::as_str).collect();
        assert_eq!(
            names,
            vec![
                "pc-q35-11.1",
                "pc-q35-11.0",
                "pc-q35-10.2",
                "pc-q35-10.1",
                "pc-q35-10.0",
                "pc-q35-9.2",
                "pc-q35-9.1",
                "pc-q35-9.0",
                "pc-q35-8.2"
            ]
        );
    }

    #[test]
    fn dispositivi_reali() {
        let d = parse_device_help(DEVICE);
        for r in REQUIRED_DEVICES {
            assert!(d.contains(*r), "{r}");
        }
        assert!(d.contains("virtio-tablet"), "alias");
        assert!(d.contains("e1000e"));
    }

    #[test]
    fn cpu_reali() {
        let c = parse_cpu_help(CPU);
        for m in ["qemu64", "Skylake-Client-v4", "Haswell-v4", "host", "max", "486"] {
            assert!(c.contains(m), "{m}");
        }
        // la sezione dei flag CPUID non è confusa con i modelli
        assert!(!c.contains("vmx-ple"));
        assert!(!c.contains("Recognized"));
    }

    #[test]
    fn formati_reali() {
        let f = parse_qemu_img_formats(IMG_HELP);
        for x in ["qcow2", "raw", "file", "nbd", "vvfat"] {
            assert!(f.contains(x), "{x}");
        }
        assert!(!f.contains("See"));
    }

    #[test]
    fn diagnosi_stderr() {
        assert!(matches!(
            classify_accel_stderr(Accelerator::Kvm, ERR_KVM),
            UnavailableReason::NotCompiled { .. }
        ));
        assert!(matches!(
            classify_accel_stderr(Accelerator::Whpx, ERR_CPU),
            UnavailableReason::IncompatibleConfiguration { .. }
        ));
        match classify_accel_stderr(Accelerator::Whpx, ERR_WHPX) {
            UnavailableReason::InitFailed { stderr } => {
                assert!(stderr.contains("No accelerator found"));
                assert!(!stderr.contains(".exe"));
            }
            other => panic!("{other:?}"),
        }
        let msg = AcceleratorStatus::Unavailable {
            accelerator: Accelerator::Whpx,
            reason: classify_accel_stderr(Accelerator::Whpx, ERR_WHPX),
        }
        .to_string();
        assert!(msg.contains("WHPX non inizializzato"));
        assert!(msg.contains("Piattaforma hypervisor Windows"));
    }

    #[test]
    fn estratto_limitato() {
        let long = "x".repeat(5000);
        assert!(stderr_excerpt(&long).len() <= 600);
        assert_eq!(stderr_excerpt("a\n\nb\nc\nd\ne\n"), "b | c | d | e");
    }
}
