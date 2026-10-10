//! Prove con QEMU reale e WHPX (criterio di uscita F1, voce F1-12 di
//! `docs/RELEASE_PLAN.md`). Ignorate per default: richiedono il laboratorio
//! locale e si abilitano con la variabile d'ambiente `LIGHTVM_QEMU_DIR`
//! (cartella di QEMU 11.1.0 portabile con `share/edk2-*`).
//!
//! ```text
//! LIGHTVM_QEMU_DIR=E:/OpenVM/lab/qemu/qemu-11.1.0 \
//!   cargo test -p qemu-backend --test real_qemu -- --ignored --nocapture --test-threads=1
//! ```
//!
//! Variabili facoltative: `LIGHTVM_LAB_DIR` (predefinita `<workspace>/lab`),
//! `LIGHTVM_ISO` (predefinita `lab/media/ubuntu-26.04.1-desktop-amd64.iso`),
//! `LIGHTVM_F1_CYCLES` (predefinito 20).
//!
//! Nessuna prova usa TCG. I tempi registrati in `lab/logs/f1-03/` provengono
//! da un host condiviso e non sono misure di release.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use lightvm_proto::{
    DisplayGeometry, ErrorCode, InputEvent, KeyAction, MediaId, PointerMove, QCode, RunId,
    UtcTimestamp, VmState,
};
use qemu_backend::cmdline::{Accelerator, CpuModel, DiskFormat, DiskSpec, VmLaunchSpec};
use qemu_backend::probe::{self, AcceleratorStatus, HostCapabilities, ProbeConfig};
use qemu_backend::qmp::{QmpClient, QmpConfig};
use qemu_backend::run::{ExitCause, ProcessState, StopPhase, check_no_nic};
use qemu_backend::{
    BackendError, ExitCodeClass, IdentityStatus, ManagedPath, ManagedPaths, ProcessIdentity,
    QemuRun, RunConfig, ShutdownPolicy, prepare,
};

/// Le prove reali non devono sovrapporsi (host condiviso, tempi misurati).
static SERIAL: Mutex<()> = Mutex::new(());

const ISO_NAME: &str = "ubuntu-26.04.1-desktop-amd64.iso";

struct Lab {
    qemu_dir: PathBuf,
    iso: PathBuf,
    /// Radice di lavoro della sessione (`lab/vms/f1-03/<sessione>`).
    work: PathBuf,
    /// Directory dei log (`lab/logs/f1-03`).
    logs: PathBuf,
    session: String,
}

fn lab() -> Option<Lab> {
    let qemu_dir = PathBuf::from(std::env::var_os("LIGHTVM_QEMU_DIR")?);
    let lab_dir = std::env::var_os("LIGHTVM_LAB_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../lab"));
    let lab_dir = std::fs::canonicalize(&lab_dir).expect("cartella lab");
    let lab_dir = strip_verbatim(&lab_dir);
    let iso = std::env::var_os("LIGHTVM_ISO")
        .map(PathBuf::from)
        .unwrap_or_else(|| lab_dir.join("media").join(ISO_NAME));
    let session = format!("s{}", UtcTimestamp::now().as_unix_millis());
    let work = lab_dir.join("vms").join("f1-03").join(&session);
    let logs = lab_dir.join("logs").join("f1-03");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::create_dir_all(&logs).unwrap();
    Some(Lab {
        qemu_dir,
        iso,
        work,
        logs,
        session,
    })
}

/// `canonicalize` su Windows restituisce `\\?\E:\...`, rifiutato (giustamente)
/// da `ManagedPath`.
fn strip_verbatim(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    PathBuf::from(s.strip_prefix(r"\\?\").unwrap_or(&s).to_string())
}

macro_rules! require_lab {
    () => {
        match lab() {
            Some(l) => l,
            None => {
                eprintln!("LIGHTVM_QEMU_DIR non impostata: prova reale saltata");
                return;
            }
        }
    };
}

struct Log {
    file: std::fs::File,
}

impl Log {
    fn new(lab: &Lab, name: &str) -> Self {
        let path = lab.logs.join(format!("{}-{name}.log", lab.session));
        eprintln!("log: {}", path.display());
        Self {
            file: std::fs::File::create(path).unwrap(),
        }
    }
    fn line(&mut self, s: impl AsRef<str>) {
        let s = s.as_ref();
        eprintln!("{s}");
        let _ = writeln!(self.file, "{s}");
    }
}

/// Percorsi del laboratorio per le esecuzioni della prova.
struct LabPaths {
    qemu_dir: PathBuf,
    work: PathBuf,
    iso_id: MediaId,
    iso: PathBuf,
    base: PathBuf,
    /// Supporto dati opzionale (ISO in sola lettura sul secondo CD-ROM).
    data: Option<(MediaId, PathBuf)>,
}

impl LabPaths {
    fn run_dir_path(&self, run: &RunId) -> PathBuf {
        self.work.join("runs").join(run.to_string())
    }
    fn private_dir_path(&self, run: &RunId) -> PathBuf {
        self.work.join("p").join(run.to_string())
    }
    fn qemu_img(&self) -> PathBuf {
        self.qemu_dir.join("qemu-img.exe")
    }
    /// Prepara le directory dell'esecuzione, la copia delle variabili UEFI e
    /// l'overlay qcow2 creato con `qemu-img` sulla base condivisa.
    fn prepare_run(&self, run: &RunId) -> Duration {
        let t0 = Instant::now();
        let mp = |p: PathBuf| ManagedPath::verify(p).unwrap();
        let rd = self.run_dir_path(run);
        let dirs = prepare::prepare_run_dirs(&mp(rd.clone()), &mp(self.private_dir_path(run)))
            .expect("directory dell'esecuzione");
        assert_eq!(dirs.private_acl_entries, 1);
        prepare::copy_firmware_vars(
            &mp(self.qemu_dir.join("share").join("edk2-i386-vars.fd")),
            &mp(rd.join("vars.fd")),
        )
        .unwrap();
        prepare::create_overlay(
            &mp(self.qemu_img()),
            &mp(self.base.clone()),
            DiskFormat::Qcow2,
            &mp(rd.join("overlay.qcow2")),
            prepare::QEMU_IMG_TIMEOUT,
        )
        .expect("qemu-img create overlay");
        t0.elapsed()
    }
}

impl ManagedPaths for LabPaths {
    fn qemu_system(&self) -> qemu_backend::error::Result<ManagedPath> {
        ManagedPath::verify(self.qemu_dir.join("qemu-system-x86_64.exe"))
    }
    fn firmware_code(&self) -> qemu_backend::error::Result<ManagedPath> {
        ManagedPath::verify(self.qemu_dir.join("share").join("edk2-x86_64-code.fd"))
    }
    fn firmware_vars(&self, run: &RunId) -> qemu_backend::error::Result<ManagedPath> {
        ManagedPath::verify(self.run_dir_path(run).join("vars.fd"))
    }
    fn disk(&self, run: &RunId) -> qemu_backend::error::Result<Option<ManagedPath>> {
        ManagedPath::verify(self.run_dir_path(run).join("overlay.qcow2")).map(Some)
    }
    fn protected_readonly(&self, _run: &RunId) -> qemu_backend::error::Result<Vec<ManagedPath>> {
        Ok(vec![ManagedPath::verify(&self.base)?])
    }
    fn media(&self, media: &MediaId) -> qemu_backend::error::Result<ManagedPath> {
        if *media == self.iso_id {
            ManagedPath::verify(&self.iso)
        } else if let Some((id, path)) = &self.data
            && id == media
        {
            ManagedPath::verify(path)
        } else {
            Err(BackendError::invalid("media", "supporto non registrato"))
        }
    }
    fn run_dir(&self, run: &RunId) -> qemu_backend::error::Result<ManagedPath> {
        ManagedPath::verify(self.run_dir_path(run))
    }
    fn private_dir(&self, run: &RunId) -> qemu_backend::error::Result<ManagedPath> {
        ManagedPath::verify(self.private_dir_path(run))
    }
}

fn lab_paths(lab: &Lab, log: &mut Log) -> LabPaths {
    let base = lab.work.join("base.qcow2");
    let p = LabPaths {
        qemu_dir: lab.qemu_dir.clone(),
        work: lab.work.clone(),
        iso_id: MediaId::new_v7(),
        iso: lab.iso.clone(),
        base: base.clone(),
        data: None,
    };
    // Creazione del disco principale (criterio F1 "creazione disco").
    let t0 = Instant::now();
    let base_mp = ManagedPath::verify(&base).unwrap();
    prepare::create_disk(
        &ManagedPath::verify(p.qemu_img()).unwrap(),
        &base_mp,
        DiskFormat::Qcow2,
        1024,
        prepare::QEMU_IMG_TIMEOUT,
    )
    .expect("qemu-img create base");
    log.line(format!(
        "disco qcow2 da 1024 MiB creato con prepare::create_disk in {:?}: {} ({} byte)",
        t0.elapsed(),
        base.display(),
        std::fs::metadata(&base).unwrap().len()
    ));
    let again = prepare::create_disk(
        &ManagedPath::verify(p.qemu_img()).unwrap(),
        &base_mp,
        DiskFormat::Qcow2,
        1024,
        prepare::QEMU_IMG_TIMEOUT,
    );
    assert!(
        again.is_err(),
        "il backend non sovrascrive un disco esistente"
    );
    let res = prepare::host_resources(&ManagedPath::verify(&lab.work).unwrap()).unwrap();
    log.line(format!(
        "risorse misurate: memoria totale {} MiB, disponibile {} MiB, spazio libero {} MiB",
        res.total_memory_bytes >> 20,
        res.available_memory_bytes >> 20,
        res.free_disk_bytes >> 20
    ));
    std::fs::create_dir_all(lab.work.join("p")).unwrap();
    p
}

fn probe_caps(lab: &Lab, paths: &LabPaths, log: &mut Log) -> HostCapabilities {
    let cfg = ProbeConfig::default();
    let t0 = Instant::now();
    let mut caps = probe::probe_host(
        &paths.qemu_system().unwrap(),
        &ManagedPath::verify(paths.qemu_img()).unwrap(),
        &cfg,
    )
    .expect("probe_host");
    log.line(format!(
        "probe_host: {:?}; QEMU {} sha256 {}; qemu-img sha256 {}; acceleratori {:?}; macchina consigliata {:?}; diagnostica {:?}",
        t0.elapsed(),
        caps.version,
        caps.qemu_system_sha256,
        caps.qemu_img_sha256,
        caps.accelerators,
        caps.recommended_machine().map(|m| m.as_str().to_owned()),
        caps.diagnostics()
    ));
    let probe_dir = ManagedPath::verify(lab.work.join("p").join("probe")).unwrap();
    std::fs::create_dir_all(probe_dir.as_path()).unwrap();
    let machine = caps.recommended_machine().unwrap().clone();
    let cpu = caps.cpu_model(CpuModel::VERIFIED_DEFAULT).unwrap();
    let rt = rt();
    let t0 = Instant::now();
    let status = rt.block_on(probe::probe_accelerator(
        &caps,
        Accelerator::Whpx,
        &machine,
        &cpu,
        &probe_dir,
        &cfg,
    ));
    log.line(format!(
        "probe_accelerator whpx: {:?} in {:?}",
        status,
        t0.elapsed()
    ));
    assert!(status.is_available(), "WHPX non disponibile: {status}");
    caps.accelerator = status;
    caps
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn spec(caps: &HostCapabilities, paths: &LabPaths, run_id: RunId) -> VmLaunchSpec {
    VmLaunchSpec {
        run_id,
        machine: caps.recommended_machine().unwrap().clone(),
        accelerator: Accelerator::Whpx,
        cpu: caps.cpu_model(CpuModel::VERIFIED_DEFAULT).unwrap(),
        vcpus: 2,
        memory_mib: 2048,
        disk: Some(DiskSpec {
            format: DiskFormat::Qcow2,
        }),
        install_media: Some(paths.iso_id),
        data_media: None,
        limits: qemu_backend::JobLimits::default(),
    }
}

/// PID dei processi `qemu-system-x86_64.exe` presenti (tutti gli utenti e
/// tutti i proprietari: l'host è condiviso, si usa solo per confronto).
fn qemu_pids() -> Vec<u32> {
    let out = Command::new("tasklist")
        .args([
            "/FI",
            "IMAGENAME eq qemu-system-x86_64.exe",
            "/FO",
            "CSV",
            "/NH",
        ])
        .stdin(Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    out.lines()
        .filter_map(|l| l.split("\",\"").nth(1).and_then(|p| p.parse().ok()))
        .collect()
}

/// Nessun processo in esecuzione con l'identità registrata (PID + tempo di
/// creazione). `Exited` indica un oggetto processo già terminato ma ancora
/// referenziato da un handle di terzi (per esempio antivirus o strumenti di
/// monitoraggio dell'host): non è un orfano. Si attende fino a 2 s che sparisca
/// e si restituisce lo stato finale.
fn assert_no_orphan(id: &ProcessIdentity) -> IdentityStatus {
    let t0 = Instant::now();
    let st = loop {
        let st = id.status().expect("stato dell'identità");
        if st != IdentityStatus::Exited || t0.elapsed() > Duration::from_secs(2) {
            break st;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(
        !st.is_alive(),
        "processo QEMU orfano ancora in esecuzione: {id:?} {st:?}"
    );
    if qemu_pids().contains(&id.pid) {
        // Un qemu con lo stesso PID esiste ma non è il nostro (tempo di
        // creazione diverso): ammesso solo se l'identità dice `Reused`.
        assert_eq!(st, IdentityStatus::Reused, "PID {} ancora nostro", id.pid);
    }
    st
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn stats(v: &[f64]) -> (f64, f64, f64) {
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    let med = if s.len() % 2 == 1 {
        s[s.len() / 2]
    } else {
        (s[s.len() / 2 - 1] + s[s.len() / 2]) / 2.0
    };
    (s[0], med, s[s.len() - 1])
}

/// Probe dei binari approvati, hash rifiutato e acceleratore assente simulato
/// con `-accel kvm` (non compilato nella build Windows: stesso percorso di un
/// binario senza WHPX).
#[test]
#[ignore = "richiede QEMU reale: impostare LIGHTVM_QEMU_DIR"]
fn reale_probe_hash_e_acceleratore_assente() {
    let _g = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let lab = require_lab!();
    let mut log = Log::new(&lab, "probe");
    let paths = lab_paths(&lab, &mut log);
    let caps = probe_caps(&lab, &paths, &mut log);

    // 1. Hash non approvato: il binario non viene eseguito.
    let cfg = ProbeConfig {
        approved_qemu_system_sha256: vec!["00".repeat(32)],
        ..ProbeConfig::default()
    };
    let e = probe::probe_host(
        &paths.qemu_system().unwrap(),
        &ManagedPath::verify(paths.qemu_img()).unwrap(),
        &cfg,
    )
    .unwrap_err();
    let p = e.to_public();
    log.line(format!(
        "hash non approvato: {e} -> {} / {}",
        p.code, p.message
    ));
    assert!(matches!(e, BackendError::UnapprovedBinary { .. }));
    assert_eq!(p.code, ErrorCode::Unsupported);

    // 2. Acceleratore assente: probe reale con `-accel kvm`.
    let probe_dir = ManagedPath::verify(lab.work.join("p").join("probe")).unwrap();
    let machine = caps.recommended_machine().unwrap().clone();
    let cpu = caps.cpu_model("qemu64").unwrap();
    let before = qemu_pids();
    let t0 = Instant::now();
    let status = rt().block_on(probe::probe_accelerator(
        &caps,
        Accelerator::Kvm,
        &machine,
        &cpu,
        &probe_dir,
        &ProbeConfig::default(),
    ));
    log.line(format!(
        "probe_accelerator kvm: {:?} in {:?}",
        status,
        t0.elapsed()
    ));
    assert!(matches!(status, AcceleratorStatus::Unavailable { .. }));
    let leftover: Vec<_> = qemu_pids()
        .into_iter()
        .filter(|p| !before.contains(p))
        .collect();
    assert!(
        leftover.is_empty(),
        "processi QEMU nuovi dopo il probe: {leftover:?}"
    );

    // 3. Avvio rifiutato prima dello spawn con l'esito del probe negativo.
    let mut caps_kvm = caps.clone();
    caps_kvm.accelerator = status;
    let run_id = RunId::new_v7();
    paths.prepare_run(&run_id);
    let mut s = spec(&caps, &paths, run_id);
    s.accelerator = Accelerator::Kvm;
    let e = rt()
        .block_on(QemuRun::start(&caps_kvm, &s, &paths, RunConfig::default()))
        .unwrap_err();
    let p = e.to_public();
    log.line(format!(
        "avvio con acceleratore assente: {} / {}",
        p.code, p.message
    ));
    assert_eq!(p.code, ErrorCode::Unsupported);
    assert!(
        p.message.contains("TCG") && p.message.contains("KVM"),
        "{}",
        p.message
    );
    assert!(!paths.private_dir_path(&run_id).join("qmp.sock").exists());

    // 4. Specifica WHPX ma probe che ha verificato un altro acceleratore.
    let mut caps_mismatch = caps.clone();
    caps_mismatch.accelerator = AcceleratorStatus::Available {
        accelerator: Accelerator::Kvm,
        machine: machine.as_str().to_owned(),
        cpu: "qemu64".into(),
    };
    let s = spec(&caps, &paths, run_id);
    let e = rt()
        .block_on(QemuRun::start(
            &caps_mismatch,
            &s,
            &paths,
            RunConfig::default(),
        ))
        .unwrap_err();
    log.line(format!(
        "acceleratore diverso dal probe: {}",
        e.to_public().message
    ));
    assert_eq!(e.code(), ErrorCode::Unsupported);
}

/// Percorsi e risorse non validi rifiutati prima dello spawn.
#[test]
#[ignore = "richiede QEMU reale: impostare LIGHTVM_QEMU_DIR"]
fn reale_rifiuto_percorsi_e_risorse() {
    let _g = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let lab = require_lab!();
    let mut log = Log::new(&lab, "rifiuti");
    let paths = lab_paths(&lab, &mut log);
    let caps = probe_caps(&lab, &paths, &mut log);
    let before = qemu_pids();
    let rt = rt();

    let mut checks: Vec<(&str, BackendError)> = Vec::new();
    // Percorsi.
    for (name, p) in [
        ("virgola", lab.work.join("a,file.driver=nbd").join("x.iso")),
        ("relativo", PathBuf::from(r"lab\media\x.iso")),
        ("unc", PathBuf::from(r"\\server\share\x.iso")),
        ("dispositivo", PathBuf::from(r"\\.\PhysicalDrive0")),
        ("traversal", lab.work.join("..").join("x.iso")),
        ("protocollo", PathBuf::from("nbd://127.0.0.1/x")),
    ] {
        checks.push((name, ManagedPath::verify(p).unwrap_err()));
    }
    // Media con percorso non valido restituito dal registro.
    let bad_iso = LabPaths {
        iso: lab.work.join("a,b.iso"),
        qemu_dir: paths.qemu_dir.clone(),
        work: paths.work.clone(),
        iso_id: paths.iso_id,
        base: paths.base.clone(),
        data: None,
    };
    let run_id = RunId::new_v7();
    paths.prepare_run(&run_id);
    let e = rt
        .block_on(QemuRun::start(
            &caps,
            &spec(&caps, &bad_iso, run_id),
            &bad_iso,
            RunConfig::default(),
        ))
        .unwrap_err();
    checks.push(("media con virgola", e));
    // Media non registrato.
    let mut s = spec(&caps, &paths, run_id);
    s.install_media = Some(MediaId::new_v7());
    checks.push((
        "media non registrato",
        rt.block_on(QemuRun::start(&caps, &s, &paths, RunConfig::default()))
            .unwrap_err(),
    ));
    // Risorse.
    for (name, vcpus, mem) in [
        ("vcpu 0", 0, 2048),
        ("vcpu 65", 65, 2048),
        ("ram 64 MiB", 2, 64),
        ("ram eccessiva", 2, u32::MAX),
    ] {
        let mut s = spec(&caps, &paths, run_id);
        s.vcpus = vcpus;
        s.memory_mib = mem;
        checks.push((
            name,
            rt.block_on(QemuRun::start(&caps, &s, &paths, RunConfig::default()))
                .unwrap_err(),
        ));
    }
    checks.push(("cpu host", CpuModel::nominal("host").unwrap_err()));
    checks.push((
        "cpu con flag",
        CpuModel::nominal("qemu64,+vmx").unwrap_err(),
    ));
    checks.push((
        "macchina alias q35",
        qemu_backend::MachineType::versioned_q35("q35").unwrap_err(),
    ));
    checks.push(("cpu assente", caps.cpu_model("Fantasia-v9").unwrap_err()));
    checks.push((
        "protocollo nbd",
        qemu_backend::validate::check_block_protocol("nbd").unwrap_err(),
    ));
    checks.push((
        "protocollo https",
        qemu_backend::validate::check_block_protocol("https").unwrap_err(),
    ));

    for (name, e) in &checks {
        let p = e.to_public();
        log.line(format!("rifiuto [{name}]: {} / {}", p.code, p.message));
        assert!(
            matches!(
                p.code,
                ErrorCode::InvalidArgument | ErrorCode::ResourceLimit | ErrorCode::Unsupported
            ),
            "{name}: {p:?}"
        );
    }
    assert!(!paths.private_dir_path(&run_id).join("qmp.sock").exists());
    let nuovi: Vec<_> = qemu_pids()
        .into_iter()
        .filter(|p| !before.contains(p))
        .collect();
    assert!(
        nuovi.is_empty(),
        "QEMU avviato da una richiesta rifiutata: {nuovi:?}"
    );
    log.line(format!(
        "rifiuti verificati: {}, nessun processo QEMU creato",
        checks.len()
    ));
}

/// Il rilevatore di NIC funziona su QEMU reale: con una NIC esplicita (senza
/// backend di rete, VM in pausa) `info network` e `query-pci` la riportano.
/// Il processo è avviato direttamente (fuori dall'API, che non può produrre una
/// NIC) e terminato al termine.
#[test]
#[ignore = "richiede QEMU reale: impostare LIGHTVM_QEMU_DIR"]
fn reale_rilevatore_nic() {
    let _g = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let lab = require_lab!();
    let mut log = Log::new(&lab, "nic");
    let dir = lab.work.join("p").join("nic");
    std::fs::create_dir_all(&dir).unwrap();
    let sock = ManagedPath::verify(dir.join("qmp.sock")).unwrap();
    let _ = std::fs::remove_file(sock.as_path());

    struct Kill(std::process::Child);
    impl Drop for Kill {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let child = Command::new(lab.qemu_dir.join("qemu-system-x86_64.exe"))
        .args([
            "-no-user-config",
            "-nodefaults",
            "-machine",
            "type=pc-q35-11.1",
            "-accel",
            "whpx",
            "-cpu",
            "qemu64",
            "-m",
            "size=256M",
            "-display",
            "none",
            "-device",
            "virtio-net-pci",
            "-qmp",
        ])
        .arg(format!("unix:{},server=on,wait=off", sock.qemu_value()))
        .arg("-S")
        .env_clear()
        .env("SystemRoot", std::env::var_os("SystemRoot").unwrap())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut child = Kill(child);
    let rt = rt();
    let (info, pci) = rt.block_on(async {
        let t0 = Instant::now();
        let qmp = loop {
            match QmpClient::connect(&sock, QmpConfig::default()).await {
                Ok(q) => break q,
                Err(e) if t0.elapsed() < Duration::from_secs(20) => {
                    let _ = e;
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Err(e) => panic!("QMP: {e}"),
            }
        };
        let info = qmp.network_diagnostics().await.unwrap();
        let pci = qmp.pci_network_devices().await.unwrap();
        let _ = qmp.quit().await;
        (info, pci)
    });
    let code = child.0.wait().ok().and_then(|s| s.code());
    log.line(format!("info network con NIC: {:?}", info.trim()));
    log.line(format!("query-pci classe rete: {pci:?}; uscita {code:?}"));
    let e = check_no_nic(&info, &pci).unwrap_err();
    log.line(format!("verifica NIC: {e} -> {}", e.to_public().code));
    assert!(!pci.is_empty());
    assert!(!info.trim().is_empty());
    let _ = std::fs::remove_file(sock.as_path());
}

/// Criterio di uscita F1: almeno 20 cicli start/stop reali con WHPX, disco
/// overlay creato con `qemu-img`, nessun orfano, nessuna NIC inattesa.
#[test]
#[ignore = "richiede QEMU reale: impostare LIGHTVM_QEMU_DIR"]
fn reale_cicli_start_stop() {
    let _g = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let lab = require_lab!();
    let cycles: usize = std::env::var("LIGHTVM_F1_CYCLES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20);
    let mut log = Log::new(&lab, "cicli");
    log.line("Host condiviso: i tempi seguenti non sono misure di release.");
    let paths = lab_paths(&lab, &mut log);
    let caps = probe_caps(&lab, &paths, &mut log);
    let csv_path = lab.logs.join(format!("{}-cicli.csv", lab.session));
    let mut csv = std::fs::File::create(&csv_path).unwrap();
    writeln!(
        csv,
        "ciclo,run_id,pid,overlay_ms,start_ms,stop_ms,close_ms,fase,codice,classe,causa,attivi_dopo,job_chiuso,handle_rilasciato,identita_dopo,nic_ok,stato_riconciliato,note"
    )
    .unwrap();
    let rt = rt();
    let mut start_ms = Vec::new();
    let mut stop_ms = Vec::new();
    let mut identities = Vec::new();
    let mut phases: BTreeMap<String, usize> = BTreeMap::new();

    for i in 1..=cycles {
        let run_id = RunId::new_v7();
        let overlay = paths.prepare_run(&run_id);
        if i == 1 {
            // Socket orfano di un'esecuzione precedente: va rimosso dal backend.
            std::fs::write(paths.private_dir_path(&run_id).join("qmp.sock"), b"orfano").unwrap();
        }
        let s = spec(&caps, &paths, run_id);
        let t0 = Instant::now();
        let run = rt
            .block_on(QemuRun::start(&caps, &s, &paths, RunConfig::default()))
            .unwrap_or_else(|e| panic!("ciclo {i}: avvio fallito: {e}"));
        let t_start = t0.elapsed();
        let id = run.identity();
        identities.push(id);
        assert_eq!(id.run_id, run_id);
        assert_eq!(run.state(), ProcessState::Running);
        assert_eq!(run.accelerator().enabled, "whpx");
        assert_eq!(
            run.active_processes().unwrap(),
            1,
            "un solo processo nel job"
        );
        let nic_ok = rt.block_on(run.verify_no_nic()).is_ok();
        assert!(nic_ok, "ciclo {i}: NIC inattesa");
        let rec = rt.block_on(run.reconcile(Duration::from_secs(5))).unwrap();
        assert_eq!(rec.observed.state, VmState::Running, "{rec:?}");

        if i == 1 {
            // Cattura e input reali sulla prima esecuzione: screenshot ripetuti
            // fino a un'immagine non nera (firmware o boot loader), massimo 30 s.
            let t_shot = Instant::now();
            let (shot, img) = loop {
                std::thread::sleep(Duration::from_secs(1));
                let (shot, img) = rt.block_on(run.screenshot_rgb()).unwrap();
                if img.non_black_percent() > 0.5 || t_shot.elapsed() > Duration::from_secs(30) {
                    break (shot, img);
                }
            };
            log.line(format!(
                "screendump dopo {:?}: {}x{}, PNG {} byte, sha256 {}, non nero {:.2}%",
                t_shot.elapsed(),
                shot.geometry.width,
                shot.geometry.height,
                shot.png.len(),
                shot.sha256,
                img.non_black_percent()
            ));
            let png_path = lab.logs.join(format!("{}-screendump.png", lab.session));
            std::fs::write(&png_path, &shot.png).unwrap();
            log.line(format!("PNG salvato: {}", png_path.display()));
            let leftovers: Vec<_> = std::fs::read_dir(paths.private_dir_path(&run_id))
                .unwrap()
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.ends_with(".ppm"))
                .collect();
            assert!(
                leftovers.is_empty(),
                "file screendump rimasti: {leftovers:?}"
            );
            let g = shot.geometry;
            let shift_down = InputEvent::Key(KeyAction {
                qcode: QCode::parse("shift").unwrap(),
                down: true,
            });
            let mv = InputEvent::PointerMove(
                PointerMove::from_pixels(g.width / 2, g.height / 2, g).unwrap(),
            );
            rt.block_on(run.send_input(run_id, g, &[mv, shift_down]))
                .unwrap();
            assert!(run.has_pressed_input());
            let released = rt.block_on(run.release_all_input(run_id)).unwrap();
            log.line(format!(
                "input: movimento + shift premuto, rilasciati {released} tasti"
            ));
            assert_eq!(released, 1);
            assert!(!run.has_pressed_input());
            // Geometria obsoleta e run_id superato rifiutati.
            let stale = DisplayGeometry::new(g.width + 1, g.height).unwrap();
            assert!(rt.block_on(run.send_input(run_id, stale, &[])).is_err());
            assert!(
                rt.block_on(run.send_input(RunId::new_v7(), g, &[]))
                    .is_err()
            );
            let power = InputEvent::Key(KeyAction {
                qcode: QCode::parse("power").unwrap(),
                down: true,
            });
            assert!(rt.block_on(run.send_input(run_id, g, &[power])).is_err());
            // Limite di dimensione dello screenshot.
            let e = rt.block_on(run.screenshot_limited(16)).unwrap_err();
            assert_eq!(e.code(), ErrorCode::ResourceLimit);
        } else {
            std::thread::sleep(Duration::from_millis(1500));
        }

        // Arresto: alternanza fra scalata completa (powerdown → quit), solo
        // quit ed Emergency Stop nell'ultimo ciclo.
        let t1 = Instant::now();
        let report = if i == cycles {
            rt.block_on(run.emergency_stop(Duration::from_secs(10)))
                .unwrap()
        } else {
            let policy = if i % 2 == 0 {
                ShutdownPolicy {
                    powerdown_timeout: Duration::from_secs(2),
                    quit_timeout: Duration::from_secs(5),
                    kill_timeout: Duration::from_secs(10),
                }
            } else {
                ShutdownPolicy {
                    powerdown_timeout: Duration::ZERO,
                    quit_timeout: Duration::from_secs(5),
                    kill_timeout: Duration::from_secs(10),
                }
            };
            rt.block_on(run.shutdown(policy)).unwrap()
        };
        let t_stop = t1.elapsed();
        assert_eq!(
            report.active_after, 0,
            "ciclo {i}: processi nel job dopo l'uscita"
        );
        match report.phase {
            StopPhase::Quit => {
                assert_eq!(report.cause, ExitCause::QmpQuit);
                assert_eq!(report.class, ExitCodeClass::Clean);
            }
            StopPhase::Kill => {
                assert_eq!(report.cause, ExitCause::EmergencyStop);
                assert_eq!(report.class, ExitCodeClass::EmergencyStop);
            }
            StopPhase::Powerdown => assert_eq!(report.cause, ExitCause::GuestShutdown),
            StopPhase::AlreadyExited => panic!("ciclo {i}: uscita prima dell'arresto"),
        }
        let rec = rt.block_on(run.reconcile(Duration::from_secs(1))).unwrap();
        assert_eq!(rec.observed.state, VmState::Stopped, "{rec:?}");
        let t2 = Instant::now();
        let closed = rt.block_on(run.close()).unwrap();
        let t_close = t2.elapsed();
        assert!(closed.job_closed, "ciclo {i}: Job Object non chiuso");
        assert!(
            closed.process_handle_released,
            "ciclo {i}: handle del processo trattenuto"
        );
        assert!(!closed.forced);
        assert_eq!(closed.active_at_close, 0);
        assert!(!closed.qmp_socket_left, "ciclo {i}: socket QMP rimasto");
        let after = assert_no_orphan(&id);
        *phases.entry(format!("{:?}", report.phase)).or_default() += 1;
        start_ms.push(ms(t_start));
        stop_ms.push(ms(t_stop));
        let line = format!(
            "{i},{run_id},{},{:.1},{:.1},{:.1},{:.1},{:?},{:?},{:?},{:?},{},{},{},{:?},{},{:?},{}",
            id.pid,
            ms(overlay),
            ms(t_start),
            ms(t_stop),
            ms(t_close),
            report.phase,
            report.code,
            report.class,
            report.cause,
            report.active_after,
            closed.job_closed,
            closed.process_handle_released,
            after,
            nic_ok,
            rec.observed.state,
            report.notes.join(" / ").replace(',', ";")
        );
        writeln!(csv, "{line}").unwrap();
        log.line(format!("ciclo {line}"));
    }

    // Verifica finale su tutte le identità della sessione.
    for id in &identities {
        assert_no_orphan(id);
    }
    let (a0, a1, a2) = stats(&start_ms);
    let (b0, b1, b2) = stats(&stop_ms);
    log.line(format!(
        "RIEPILOGO {cycles} cicli: avvio ms min {a0:.1} mediana {a1:.1} max {a2:.1}; \
         arresto ms min {b0:.1} mediana {b1:.1} max {b2:.1}; fasi {phases:?}; \
         nessun orfano su {} identità; csv {}",
        identities.len(),
        csv_path.display()
    ));
    assert!(cycles >= 20 || std::env::var("LIGHTVM_F1_CYCLES").is_ok());
}

/// F1-17 (D-1, ADR-0011 criterio 2): boot della ISO live con un modello CPU
/// nominale più ricco di `qemu64` (`LIGHTVM_F1_CPU`, predefinito
/// `Skylake-Client-v4`), 2 vCPU, 4096 MiB. Uno screendump ogni 10 s per al
/// massimo `LIGHTVM_F1_BOOT_S` secondi (predefinito 240), salvati in
/// `lab/logs/f1-03/`. Il test verifica solo che il processo resti vivo e che
/// le immagini non siano nere; l'arrivo al desktop si giudica dalle immagini
/// e si registra nel file di verifica.
#[test]
#[ignore = "richiede QEMU reale: impostare LIGHTVM_QEMU_DIR"]
fn reale_boot_live_cpu_nominale() {
    let _g = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let lab = require_lab!();
    let cpu_name = std::env::var("LIGHTVM_F1_CPU").unwrap_or_else(|_| "Skylake-Client-v4".into());
    let boot_s: u64 = std::env::var("LIGHTVM_F1_BOOT_S")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(240);
    let mut log = Log::new(&lab, "boot-cpu");
    let paths = lab_paths(&lab, &mut log);
    let caps = probe_caps(&lab, &paths, &mut log);
    let cpu = caps
        .cpu_model(&cpu_name)
        .unwrap_or_else(|e| panic!("modello {cpu_name}: {e}"));
    let run_id = RunId::new_v7();
    paths.prepare_run(&run_id);
    let mut s = spec(&caps, &paths, run_id);
    s.cpu = cpu;
    s.memory_mib = 4096;
    let rt = rt();
    let t0 = Instant::now();
    let run = rt
        .block_on(QemuRun::start(&caps, &s, &paths, RunConfig::default()))
        .unwrap_or_else(|e| panic!("avvio con {cpu_name}: {e}"));
    log.line(format!(
        "avvio con -cpu {cpu_name}: {:?}, pid {}",
        t0.elapsed(),
        run.identity().pid
    ));
    let mut n = 0u32;
    while t0.elapsed() < Duration::from_secs(boot_s) {
        std::thread::sleep(Duration::from_secs(10));
        assert_eq!(
            run.state(),
            ProcessState::Running,
            "QEMU uscito durante il boot con {cpu_name}"
        );
        let (shot, img) = rt.block_on(run.screenshot_rgb()).unwrap();
        n += 1;
        let png = lab.logs.join(format!(
            "{}-boot-{cpu_name}-{:03}s.png",
            lab.session,
            t0.elapsed().as_secs()
        ));
        std::fs::write(&png, &shot.png).unwrap();
        log.line(format!(
            "t={:>4}s {}x{} non nero {:.2}% sha256 {} -> {}",
            t0.elapsed().as_secs(),
            shot.geometry.width,
            shot.geometry.height,
            img.non_black_percent(),
            shot.sha256,
            png.display()
        ));
    }
    let report = rt
        .block_on(run.emergency_stop(Duration::from_secs(10)))
        .unwrap();
    assert_eq!(report.active_after, 0);
    let closed = rt.block_on(run.close()).unwrap();
    assert!(closed.job_closed);
    log.line(format!("{n} screendump; arresto {:?}", report.phase));
}

/// ADR-0017: limiti del Job Object applicati a QEMU reale. (1) Tetto CPU e
/// memoria del job = RAM + 1024 MiB: riletti dal job, VM avviata, picco
/// registrato. (2) Memoria del job inferiore alla RAM del guest (512 MiB contro
/// 2048 MiB): QEMU non deve restare in esecuzione oltre il limite.
#[test]
#[ignore = "richiede QEMU reale: impostare LIGHTVM_QEMU_DIR"]
fn reale_limiti_job_cpu_e_memoria() {
    let _g = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let lab = require_lab!();
    let mut log = Log::new(&lab, "limiti-job");
    let paths = lab_paths(&lab, &mut log);
    let caps = probe_caps(&lab, &paths, &mut log);
    let rt = rt();

    // (1) Limiti compatibili con la VM.
    let run_id = RunId::new_v7();
    paths.prepare_run(&run_id);
    let mut s = spec(&caps, &paths, run_id);
    s.limits = qemu_backend::JobLimits {
        cpu_rate: Some(300),
        job_memory_bytes: Some((2048 + 1024) << 20),
    };
    let run = rt
        .block_on(QemuRun::start(&caps, &s, &paths, RunConfig::default()))
        .expect("avvio con limiti");
    std::thread::sleep(Duration::from_secs(5));
    assert_eq!(run.state(), ProcessState::Running);
    let r = run.limit_report().unwrap();
    log.line(format!("limiti applicati: {r:?}"));
    assert_eq!(r.cpu_rate, Some(300));
    assert_eq!(r.job_memory_bytes, Some((2048 + 1024) << 20));
    assert!(r.peak_job_memory_bytes > 0);
    assert!(!r.memory_limit_reached());
    let id = run.identity();
    let report = rt
        .block_on(run.emergency_stop(Duration::from_secs(10)))
        .unwrap();
    assert_eq!(report.active_after, 0);
    let _ = rt.block_on(run.close()).unwrap();
    assert_no_orphan(&id);

    // (2) Memoria del job sotto la RAM del guest.
    let run_id = RunId::new_v7();
    paths.prepare_run(&run_id);
    let mut s = spec(&caps, &paths, run_id);
    s.limits = qemu_backend::JobLimits {
        cpu_rate: None,
        job_memory_bytes: Some(512 << 20),
    };
    let t0 = Instant::now();
    match rt.block_on(QemuRun::start(&caps, &s, &paths, RunConfig::default())) {
        Err(e) => log.line(format!(
            "memoria del job 512 MiB: avvio rifiutato dopo {:?}: {e}",
            t0.elapsed()
        )),
        Ok(run) => {
            let id = run.identity();
            let mut uscito = None;
            while t0.elapsed() < Duration::from_secs(60) {
                if let ProcessState::Exited { .. } = run.state() {
                    uscito = Some(t0.elapsed());
                    break;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            let r = run.limit_report().unwrap();
            log.line(format!(
                "memoria del job 512 MiB: stato {:?}, uscita dopo {uscito:?}, rapporto {r:?}, limite raggiunto {}",
                run.state(),
                r.memory_limit_reached()
            ));
            if uscito.is_none() {
                let _ = rt.block_on(run.emergency_stop(Duration::from_secs(10)));
                let _ = rt.block_on(run.close());
                assert_no_orphan(&id);
                panic!("QEMU ancora in esecuzione oltre il limite di memoria del job");
            }
            assert!(r.peak_job_memory_bytes <= 512 << 20);
            let _ = rt.block_on(run.close());
            assert_no_orphan(&id);
        }
    }
}

/// Qcode (layout US) e Shift per un carattere del comando digitato nel guest.
fn qcode_us(c: char) -> (&'static str, bool) {
    const LETTERE: [&str; 26] = [
        "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p", "q", "r",
        "s", "t", "u", "v", "w", "x", "y", "z",
    ];
    const CIFRE: [&str; 10] = ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"];
    match c {
        'a'..='z' => (LETTERE[(c as u8 - b'a') as usize], false),
        '0'..='9' => (CIFRE[(c as u8 - b'0') as usize], false),
        ' ' => ("spc", false),
        '-' => ("minus", false),
        '/' => ("slash", false),
        '.' => ("dot", false),
        '&' => ("7", true),
        '\n' => ("ret", false),
        _ => panic!("carattere non previsto: {c:?}"),
    }
}

fn tasto(qcode: &str, down: bool) -> InputEvent {
    InputEvent::Key(KeyAction {
        qcode: QCode::parse(qcode).unwrap(),
        down,
    })
}

/// P-5, parte Linux (docs/STATUS.md): nel guest Ubuntu live, con l'ISO dati
/// `lab/media/p5-dati.iso` (script `crates/lightvm-service/scripts/
/// p5-prova-guest.sh` e `lightvm-peer-check` per Linux) sul secondo CD-ROM,
/// apre un terminale e lancia la prova; salva gli screenshot in
/// `lab/logs/f1-03/` per la lettura dell'esito. `LIGHTVM_P5_ISO` sostituisce
/// il percorso dell'ISO dati.
#[test]
#[ignore = "richiede QEMU reale: impostare LIGHTVM_QEMU_DIR"]
fn reale_p5_secondo_utente_linux_in_vm() {
    let _g = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let lab = require_lab!();
    let dati = std::env::var_os("LIGHTVM_P5_ISO")
        .map(PathBuf::from)
        .unwrap_or_else(|| lab.iso.with_file_name("p5-dati.iso"));
    assert!(dati.is_file(), "ISO dati assente: {}", dati.display());
    let mut log = Log::new(&lab, "p5");
    let mut paths = lab_paths(&lab, &mut log);
    let data_id = MediaId::new_v7();
    paths.data = Some((data_id, dati));
    let caps = probe_caps(&lab, &paths, &mut log);
    let run_id = RunId::new_v7();
    paths.prepare_run(&run_id);
    let mut s = spec(&caps, &paths, run_id);
    s.cpu = caps.cpu_model("Skylake-Client-v4").unwrap();
    s.memory_mib = 4096;
    s.data_media = Some(data_id);
    let rt = rt();
    let run = rt
        .block_on(QemuRun::start(&caps, &s, &paths, RunConfig::default()))
        .expect("avvio");
    let t0 = Instant::now();
    let salva = |nome: &str, log: &mut Log| {
        let (shot, img) = rt.block_on(run.screenshot_rgb()).unwrap();
        let png = lab.logs.join(format!("{}-p5-{nome}.png", lab.session));
        std::fs::write(&png, &shot.png).unwrap();
        log.line(format!(
            "t={:>4}s {nome}: non nero {:.2}% -> {}",
            t0.elapsed().as_secs(),
            img.non_black_percent(),
            png.display()
        ));
        shot.geometry
    };
    // Desktop live: circa 100 s con Skylake-Client-v4 (F1-03).
    std::thread::sleep(Duration::from_secs(130));
    let g = salva("desktop", &mut log);
    // Terminale: Ctrl+Alt+T.
    let combo = [
        tasto("ctrl", true),
        tasto("alt", true),
        tasto("t", true),
        tasto("t", false),
        tasto("alt", false),
        tasto("ctrl", false),
    ];
    rt.block_on(run.send_input(run_id, g, &combo)).unwrap();
    std::thread::sleep(Duration::from_secs(8));
    salva("terminale", &mut log);
    let comando = "sudo mount -o ro /dev/sr1 /mnt && bash /mnt/prova.sh\n";
    for c in comando.chars() {
        let (q, shift) = qcode_us(c);
        let mut ev = Vec::new();
        if shift {
            ev.push(tasto("shift", true));
        }
        ev.push(tasto(q, true));
        ev.push(tasto(q, false));
        if shift {
            ev.push(tasto("shift", false));
        }
        rt.block_on(run.send_input(run_id, g, &ev)).unwrap();
        std::thread::sleep(Duration::from_millis(40));
    }
    std::thread::sleep(Duration::from_secs(20));
    salva("esito", &mut log);
    let id = run.identity();
    let _ = rt.block_on(run.emergency_stop(Duration::from_secs(10)));
    let _ = rt.block_on(run.close());
    assert_no_orphan(&id);
}

/// Percorsi con disco e variabili UEFI persistenti fra esecuzioni (F1-17):
/// stessi percorsi del laboratorio per il resto.
struct DiscoFisso<'a> {
    base: &'a LabPaths,
    disco: PathBuf,
    vars: PathBuf,
}

impl ManagedPaths for DiscoFisso<'_> {
    fn qemu_system(&self) -> qemu_backend::error::Result<ManagedPath> {
        self.base.qemu_system()
    }
    fn firmware_code(&self) -> qemu_backend::error::Result<ManagedPath> {
        self.base.firmware_code()
    }
    fn firmware_vars(&self, _run: &RunId) -> qemu_backend::error::Result<ManagedPath> {
        ManagedPath::verify(&self.vars)
    }
    fn disk(&self, _run: &RunId) -> qemu_backend::error::Result<Option<ManagedPath>> {
        ManagedPath::verify(&self.disco).map(Some)
    }
    fn protected_readonly(&self, _run: &RunId) -> qemu_backend::error::Result<Vec<ManagedPath>> {
        Ok(Vec::new())
    }
    fn media(&self, media: &MediaId) -> qemu_backend::error::Result<ManagedPath> {
        self.base.media(media)
    }
    fn run_dir(&self, run: &RunId) -> qemu_backend::error::Result<ManagedPath> {
        self.base.run_dir(run)
    }
    fn private_dir(&self, run: &RunId) -> qemu_backend::error::Result<ManagedPath> {
        self.base.private_dir(run)
    }
}

/// Disco (32 GiB) e variabili UEFI di F1-17 in `lab/vms/f1-17/`, creati se assenti.
fn disco_f1_17(lab: &Lab, paths: &LabPaths) -> (PathBuf, PathBuf) {
    let dir = lab.work.parent().unwrap().parent().unwrap().join("f1-17");
    std::fs::create_dir_all(&dir).unwrap();
    let disco = dir.join("disco.qcow2");
    let vars = dir.join("vars.fd");
    if !disco.exists() {
        prepare::create_disk(
            &ManagedPath::verify(paths.qemu_img()).unwrap(),
            &ManagedPath::verify(&disco).unwrap(),
            DiskFormat::Qcow2,
            32 * 1024,
            prepare::QEMU_IMG_TIMEOUT,
        )
        .unwrap();
    }
    if !vars.exists() {
        std::fs::copy(
            paths.qemu_dir.join("share").join("edk2-i386-vars.fd"),
            &vars,
        )
        .unwrap();
    }
    (disco, vars)
}

/// F1-17, parte 1: installazione automatica di Ubuntu su disco (autoinstall da
/// un volume `CIDATA`, `lab/media/f1-17-cidata.iso`, sul secondo CD-ROM). Il
/// guest si spegne da solo a fine installazione (`shutdown: poweroff`).
/// Screenshot ogni 2 minuti; attesa massima `LIGHTVM_F1_17_MIN` minuti (45).
#[test]
#[ignore = "richiede QEMU reale: impostare LIGHTVM_QEMU_DIR"]
fn reale_f1_17_installazione_su_disco() {
    let _g = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let lab = require_lab!();
    let minuti: u64 = std::env::var("LIGHTVM_F1_17_MIN")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(45);
    let mut log = Log::new(&lab, "f1-17-installazione");
    let mut paths = lab_paths(&lab, &mut log);
    let cidata = lab.iso.with_file_name("f1-17-cidata.iso");
    assert!(
        cidata.is_file(),
        "volume CIDATA assente: {}",
        cidata.display()
    );
    let cid = MediaId::new_v7();
    paths.data = Some((cid, cidata));
    let caps = probe_caps(&lab, &paths, &mut log);
    let (disco, vars) = disco_f1_17(&lab, &paths);
    let fisso = DiscoFisso {
        base: &paths,
        disco,
        vars,
    };
    let run_id = RunId::new_v7();
    paths.prepare_run(&run_id);
    let mut s = spec(&caps, &paths, run_id);
    s.cpu = caps.cpu_model("Skylake-Client-v4").unwrap();
    s.vcpus = 4;
    s.memory_mib = 6144;
    s.data_media = Some(cid);
    let rt = rt();
    let run = rt
        .block_on(QemuRun::start(&caps, &s, &fisso, RunConfig::default()))
        .expect("avvio dell'installazione");
    let t0 = Instant::now();
    let mut n = 0u32;
    let mut cliccato = false;
    let esito = loop {
        if let ProcessState::Exited { cause, code, .. } = run.state() {
            break Some((cause, code));
        }
        // L'installatore desktop mostra il riepilogo dell'autoinstall e
        // attende "Install" (in basso a destra) circa 2 minuti dopo l'avvio.
        if !cliccato && t0.elapsed() >= Duration::from_secs(140) {
            cliccato = true;
            let (shot, _) = rt.block_on(run.screenshot_rgb()).unwrap();
            let g = shot.geometry;
            let png = lab
                .logs
                .join(format!("{}-f1-17-inst-riepilogo.png", lab.session));
            std::fs::write(&png, &shot.png).unwrap();
            let x = g.width * 1035 / 1280;
            let y = g.height * 716 / 800;
            let click = [
                InputEvent::PointerMove(PointerMove::from_pixels(x, y, g).unwrap()),
                InputEvent::PointerButton(lightvm_proto::PointerButton {
                    button: lightvm_proto::MouseButton::Left,
                    down: true,
                }),
                InputEvent::PointerButton(lightvm_proto::PointerButton {
                    button: lightvm_proto::MouseButton::Left,
                    down: false,
                }),
            ];
            rt.block_on(run.send_input(run_id, g, &click)).unwrap();
            log.line(format!(
                "clic su Install ({x},{y}) a t={}s",
                t0.elapsed().as_secs()
            ));
        }
        if t0.elapsed() > Duration::from_secs(minuti * 60) {
            break None;
        }
        std::thread::sleep(Duration::from_secs(10));
        if t0.elapsed().as_secs() / 120 >= u64::from(n) {
            n += 1;
            if let Ok((shot, img)) = rt.block_on(run.screenshot_rgb()) {
                let png = lab.logs.join(format!(
                    "{}-f1-17-inst-{:03}min.png",
                    lab.session,
                    t0.elapsed().as_secs() / 60
                ));
                std::fs::write(&png, &shot.png).unwrap();
                log.line(format!(
                    "t={:>5}s non nero {:.2}% -> {}",
                    t0.elapsed().as_secs(),
                    img.non_black_percent(),
                    png.display()
                ));
            }
        }
    };
    log.line(format!("esito dopo {:?}: {esito:?}", t0.elapsed()));
    let id = run.identity();
    if esito.is_none() {
        let _ = rt.block_on(run.emergency_stop(Duration::from_secs(10)));
    }
    let _ = rt.block_on(run.close());
    assert_no_orphan(&id);
    assert!(
        esito.is_some(),
        "installazione non conclusa in {minuti} minuti"
    );
}

/// F1-17, parte 2: boot di Ubuntu dal disco installato (nessuna ISO), poi
/// arresto ACPI ordinario (`system_powerdown`): un guest reale deve spegnersi
/// da solo, senza `quit` né terminazione.
#[test]
#[ignore = "richiede QEMU reale: impostare LIGHTVM_QEMU_DIR"]
fn reale_f1_17_boot_da_disco_e_arresto_acpi() {
    let _g = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let lab = require_lab!();
    let mut log = Log::new(&lab, "f1-17-boot");
    let paths = lab_paths(&lab, &mut log);
    let caps = probe_caps(&lab, &paths, &mut log);
    let (disco, vars) = disco_f1_17(&lab, &paths);
    let fisso = DiscoFisso {
        base: &paths,
        disco,
        vars,
    };
    let run_id = RunId::new_v7();
    paths.prepare_run(&run_id);
    let mut s = spec(&caps, &paths, run_id);
    s.cpu = caps.cpu_model("Skylake-Client-v4").unwrap();
    s.memory_mib = 4096;
    s.install_media = None;
    let rt = rt();
    let run = rt
        .block_on(QemuRun::start(&caps, &s, &fisso, RunConfig::default()))
        .expect("avvio dal disco");
    let t0 = Instant::now();
    for i in 1..=8 {
        std::thread::sleep(Duration::from_secs(15));
        let (shot, img) = rt.block_on(run.screenshot_rgb()).unwrap();
        let png = lab.logs.join(format!(
            "{}-f1-17-boot-{:03}s.png",
            lab.session,
            t0.elapsed().as_secs()
        ));
        std::fs::write(&png, &shot.png).unwrap();
        log.line(format!(
            "boot {i}: t={}s non nero {:.2}% -> {}",
            t0.elapsed().as_secs(),
            img.non_black_percent(),
            png.display()
        ));
    }
    let t1 = Instant::now();
    let report = rt
        .block_on(run.shutdown(ShutdownPolicy {
            powerdown_timeout: Duration::from_secs(90),
            quit_timeout: Duration::from_secs(10),
            kill_timeout: Duration::from_secs(10),
        }))
        .unwrap();
    log.line(format!(
        "arresto ACPI: fase {:?}, causa {:?}, codice {:?}, durata {:?}",
        report.phase,
        report.cause,
        report.code,
        t1.elapsed()
    ));
    let id = run.identity();
    let _ = rt.block_on(run.close());
    assert_no_orphan(&id);
    assert_eq!(
        report.phase,
        StopPhase::Powerdown,
        "il guest non si è spento con ACPI"
    );
}

/// Tempo CPU totale (secondi) di un processo, letto dal sistema operativo.
fn cpu_secondi(pid: u32) -> f64 {
    let out = Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &format!("(Get-Process -Id {pid}).TotalProcessorTime.TotalSeconds"),
        ])
        .stdin(Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().replace(',', "."))
        .unwrap_or_default();
    out.parse().unwrap_or(f64::NAN)
}

/// ADR-0017 / F1-10: effetto del tetto CPU del Job Object su un guest reale.
/// Boot di Ubuntu dal disco di F1-17 (2 vCPU) per 60 s senza tetto e con il
/// tetto minimo (5 % dei vCPU = 0,1 core): il boot usa in media circa 0,2
/// core (prima esecuzione: un tetto al 25 % non veniva mai raggiunto). Il
/// tempo CPU di QEMU con il tetto deve restare vicino alla quota concessa e
/// ben sotto quello senza tetto.
#[test]
#[ignore = "richiede QEMU reale e il disco di F1-17: impostare LIGHTVM_QEMU_DIR"]
fn reale_effetto_tetto_cpu_durante_il_boot() {
    let _g = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let lab = require_lab!();
    let mut log = Log::new(&lab, "tetto-cpu");
    let paths = lab_paths(&lab, &mut log);
    let caps = probe_caps(&lab, &paths, &mut log);
    let (disco, vars) = disco_f1_17(&lab, &paths);
    let fisso = DiscoFisso {
        base: &paths,
        disco,
        vars,
    };
    let logical = u32::try_from(
        std::thread::available_parallelism()
            .map(std::num::NonZero::get)
            .unwrap_or(1),
    )
    .unwrap();
    let rt = rt();
    let mut misura = |cpu_rate: Option<u32>| -> f64 {
        let run_id = RunId::new_v7();
        paths.prepare_run(&run_id);
        let mut s = spec(&caps, &paths, run_id);
        s.cpu = caps.cpu_model("Skylake-Client-v4").unwrap();
        s.vcpus = 2;
        s.memory_mib = 4096;
        s.install_media = None;
        s.limits = qemu_backend::JobLimits {
            cpu_rate,
            job_memory_bytes: None,
        };
        let run = rt
            .block_on(QemuRun::start(&caps, &s, &fisso, RunConfig::default()))
            .expect("avvio dal disco");
        let pid = run.identity().pid;
        let c0 = cpu_secondi(pid);
        std::thread::sleep(Duration::from_secs(60));
        let c1 = cpu_secondi(pid);
        let id = run.identity();
        let _ = rt.block_on(run.emergency_stop(Duration::from_secs(10)));
        let _ = rt.block_on(run.close());
        assert_no_orphan(&id);
        let usati = c1 - c0;
        log.line(format!(
            "tetto {cpu_rate:?}: {usati:.1} s di CPU in 60 s ({:.2} core medi)",
            usati / 60.0
        ));
        usati
    };
    let libero = misura(None);
    // 5 % di 2 vCPU = 0,1 core: quota della macchina in centesimi di punto.
    let rate = (2 * 5 * 100 / logical).max(1);
    let con_tetto = misura(Some(rate));
    let quota_core = f64::from(rate) / 10_000.0 * f64::from(logical);
    log.line(format!(
        "quota concessa {quota_core:.2} core; senza tetto {libero:.1} s, con tetto {con_tetto:.1} s"
    ));
    assert!(
        con_tetto <= quota_core * 60.0 * 1.25,
        "tempo CPU con tetto oltre la quota: {con_tetto:.1} s"
    );
    assert!(
        libero > con_tetto * 1.5,
        "il tetto non riduce il consumo: senza {libero:.1} s, con {con_tetto:.1} s"
    );
}
