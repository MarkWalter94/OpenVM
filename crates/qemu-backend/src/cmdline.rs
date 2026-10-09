//! Costruzione del comando QEMU da opzioni controllate (ROADMAP 6.3, 7.2).
//!
//! [`VmLaunchSpec`] descrive la VM con tipi chiusi; [`VmLaunchSpec::build`]
//! produce gli argomenti come `Vec<OsString>` passati direttamente al processo,
//! senza shell. Proprietà garantite per costruzione:
//!
//! - opzioni sempre presenti: `-no-user-config -nodefaults -nic none`,
//!   `-display none`, `-S` (il guest parte solo dopo le verifiche post-avvio,
//!   vedi [`crate::run`]);
//! - acceleratore esplicito, mai TCG (il tipo [`Accelerator`] non ha una
//!   variante TCG: ADR-0007);
//! - macchina q35 **versionata** (`pc-q35-X.Y`) scelta dal probe;
//! - modello CPU nominale (rifiutati `host` e `max`, F0-02 9.1);
//! - firmware pflash: codice in sola lettura, variabili per esecuzione;
//! - dischi con `-blockdev` strutturato (`driver`, `node-name`,
//!   `file.driver=file`, `file.filename`), formato esplicito e solo protocollo
//!   `file`;
//! - ISO di installazione e supporto dati in sola lettura su `ide-cd`;
//! - display VNC su socket Unix e QMP su socket Unix nella directory privata;
//! - dispositivi HID da allowlist (`virtio-tablet-pci`, `virtio-keyboard-pci`).

use std::ffi::OsString;

use crate::error::{BackendError, Result};
use crate::paths::{ManagedPath, ManagedPaths, names};
use crate::validate::{check_block_protocol, check_token};
use lightvm_proto::{MediaId, RunId};

/// Acceleratore hardware. Non esiste una variante TCG (ADR-0007).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Accelerator {
    /// Windows Hypervisor Platform.
    Whpx,
    /// Linux KVM.
    Kvm,
    /// macOS Hypervisor.framework.
    Hvf,
}

impl Accelerator {
    /// Nome usato da QEMU (`-accel`, `query-accelerators`).
    pub fn qemu_name(self) -> &'static str {
        match self {
            Self::Whpx => "whpx",
            Self::Kvm => "kvm",
            Self::Hvf => "hvf",
        }
    }

    /// Acceleratore previsto per l'host corrente (ROADMAP sezione 5).
    pub fn for_current_host() -> Self {
        if cfg!(windows) {
            Self::Whpx
        } else if cfg!(target_os = "macos") {
            Self::Hvf
        } else {
            Self::Kvm
        }
    }
}

/// Tipo di macchina q35 versionato (`pc-q35-X.Y`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct MachineType(String);

impl MachineType {
    /// Accetta solo nomi `pc-q35-<maggiore>.<minore>`; l'alias `q35` è rifiutato
    /// perché il suo significato cambia con la versione di QEMU.
    pub fn versioned_q35(name: &str) -> Result<Self> {
        check_token("machine", name)?;
        if parse_q35_version(name).is_none() {
            return Err(BackendError::invalid(
                "machine",
                format!("`{name}` non è un tipo q35 versionato (atteso pc-q35-X.Y)"),
            ));
        }
        Ok(Self(name.to_owned()))
    }

    /// Nome QEMU.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Versione `(maggiore, minore)`.
    pub fn version(&self) -> (u32, u32) {
        parse_q35_version(&self.0).unwrap_or((0, 0))
    }
}

/// Estrae `(maggiore, minore)` da `pc-q35-X.Y`.
pub(crate) fn parse_q35_version(name: &str) -> Option<(u32, u32)> {
    let v = name.strip_prefix("pc-q35-")?;
    let (a, b) = v.split_once('.')?;
    if a.is_empty() || b.is_empty() || !a.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    if !b.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some((a.parse().ok()?, b.parse().ok()?))
}

/// Modello CPU nominale.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CpuModel(String);

impl CpuModel {
    /// Modello verificato fino al desktop live su questo host (F0-02 9.2).
    pub const VERIFIED_DEFAULT: &'static str = "qemu64";

    /// Accetta un modello nominale; `host` e `max` sono rifiutati (con OVMF
    /// sotto WHPX non avviano il firmware, F0-02 9.1). La presenza del modello
    /// nel binario va verificata con [`crate::HostCapabilities::cpu_model`].
    pub fn nominal(name: &str) -> Result<Self> {
        check_token("cpu", name)?;
        if matches!(name, "host" | "max") {
            return Err(BackendError::Unsupported {
                what: format!("modello CPU `{name}`"),
                hint: "con OVMF sotto WHPX `host`/`max` bloccano il firmware (F0-02); \
                       usare un modello nominale come `qemu64` o `Skylake-Client-v4`"
                    .into(),
            });
        }
        Ok(Self(name.to_owned()))
    }

    /// Nome QEMU.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Formato esplicito del disco (nessuna rilevazione automatica del formato).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DiskFormat {
    /// qcow2 (overlay della baseline).
    Qcow2,
    /// raw.
    Raw,
}

impl DiskFormat {
    pub(crate) fn driver(self) -> &'static str {
        match self {
            Self::Qcow2 => "qcow2",
            Self::Raw => "raw",
        }
    }
}

/// Disco della VM. Il percorso è fornito da [`ManagedPaths::disk`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiskSpec {
    /// Formato del file.
    pub format: DiskFormat,
}

/// Limiti di validazione delle risorse virtuali.
pub mod limits {
    /// vCPU minime.
    pub const MIN_VCPUS: u32 = 1;
    /// vCPU massime accettate dal builder (il limite effettivo dipende anche
    /// dall'host e dalla policy, verificati a monte).
    pub const MAX_VCPUS: u32 = 64;
    /// RAM minima in MiB.
    pub const MIN_MEMORY_MIB: u32 = 128;
    /// RAM massima in MiB accettata dal builder.
    pub const MAX_MEMORY_MIB: u32 = 256 * 1024;
}

/// Specifica di avvio di una VM.
#[derive(Clone, Debug)]
pub struct VmLaunchSpec {
    /// Esecuzione (usata in `-name` e per ottenere i percorsi).
    pub run_id: RunId,
    /// Macchina q35 versionata.
    pub machine: MachineType,
    /// Acceleratore.
    pub accelerator: Accelerator,
    /// Modello CPU nominale.
    pub cpu: CpuModel,
    /// Numero di vCPU.
    pub vcpus: u32,
    /// RAM in MiB.
    pub memory_mib: u32,
    /// Disco, se presente.
    pub disk: Option<DiskSpec>,
    /// ISO di installazione (avviabile), dal registro media.
    pub install_media: Option<MediaId>,
    /// Supporto dati in sola lettura, dal registro media.
    pub data_media: Option<MediaId>,
}

/// Comando QEMU pronto per lo spawn.
#[derive(Clone, Debug)]
pub struct QemuCommand {
    /// Esecuzione a cui appartiene il processo (identità con handle e tempo di
    /// creazione).
    pub run_id: RunId,
    /// Eseguibile.
    pub program: ManagedPath,
    /// Argomenti strutturati.
    pub args: Vec<OsString>,
    /// Socket QMP.
    pub qmp_socket: ManagedPath,
    /// Socket VNC (`None` per il comando di probe).
    pub vnc_socket: Option<ManagedPath>,
    /// File di stdout.
    pub stdout: ManagedPath,
    /// File di stderr.
    pub stderr: ManagedPath,
    /// Directory privata (socket, screendump).
    pub private_dir: ManagedPath,
}

struct Args(Vec<OsString>);

impl Args {
    fn opt(&mut self, name: &'static str, value: impl Into<String>) {
        self.0.push(OsString::from(name));
        self.0.push(OsString::from(value.into()));
    }
    fn flag(&mut self, name: &'static str) {
        self.0.push(OsString::from(name));
    }
}

impl VmLaunchSpec {
    /// Valida le risorse virtuali.
    pub fn validate(&self) -> Result<()> {
        if !(limits::MIN_VCPUS..=limits::MAX_VCPUS).contains(&self.vcpus) {
            return Err(BackendError::ResourceLimit(format!(
                "{} vCPU fuori dall'intervallo {}..={}",
                self.vcpus,
                limits::MIN_VCPUS,
                limits::MAX_VCPUS
            )));
        }
        if !(limits::MIN_MEMORY_MIB..=limits::MAX_MEMORY_MIB).contains(&self.memory_mib) {
            return Err(BackendError::ResourceLimit(format!(
                "{} MiB di RAM fuori dall'intervallo {}..={}",
                self.memory_mib,
                limits::MIN_MEMORY_MIB,
                limits::MAX_MEMORY_MIB
            )));
        }
        if self.install_media.is_some() && self.install_media == self.data_media {
            return Err(BackendError::invalid(
                "data_media",
                "lo stesso supporto come installazione e dati",
            ));
        }
        Ok(())
    }

    /// Costruisce il comando. Tutti i percorsi provengono da `paths`.
    pub fn build(&self, paths: &dyn ManagedPaths) -> Result<QemuCommand> {
        self.validate()?;
        let run = &self.run_id;
        let program = paths.qemu_system()?;
        let private_dir = paths.private_dir(run)?;
        let run_dir = paths.run_dir(run)?;
        let qmp_socket = private_dir.join_file(names::QMP_SOCKET)?;
        let vnc_socket = private_dir.join_file(names::VNC_SOCKET)?;
        qmp_socket.check_socket_len("qmp_socket")?;
        vnc_socket.check_socket_len("vnc_socket")?;
        let fw_code = paths.firmware_code()?;
        let fw_vars = paths.firmware_vars(run)?;
        // Unico protocollo di blocco ammesso (allowlist, ADR-0004/0010).
        let proto = check_block_protocol("file")?;

        let mut a = Args(Vec::with_capacity(64));
        a.opt("-name", format!("guest=lightvm-{run}"));
        a.flag("-no-user-config");
        a.flag("-nodefaults");
        a.opt(
            "-machine",
            format!(
                "type={},pflash0=fw-code,pflash1=fw-vars",
                self.machine.as_str()
            ),
        );
        a.opt("-accel", self.accelerator.qemu_name());
        a.opt("-cpu", self.cpu.as_str());
        a.opt("-smp", format!("cpus={}", self.vcpus));
        a.opt("-m", format!("size={}M", self.memory_mib));
        a.opt(
            "-blockdev",
            format!(
                "driver={proto},node-name=fw-code,filename={},read-only=on",
                fw_code.qemu_value()
            ),
        );
        a.opt(
            "-blockdev",
            format!(
                "driver={proto},node-name=fw-vars,filename={}",
                fw_vars.qemu_value()
            ),
        );
        let mut next_boot = 0u32;
        if let Some(media) = &self.install_media {
            let iso = paths.media(media)?;
            push_cdrom(&mut a, proto, "cd-install", &iso, 0, Some(next_boot));
            next_boot += 1;
        }
        if let Some(spec) = self.disk {
            let disk = paths.disk(run)?.ok_or_else(|| {
                BackendError::invalid("disk", "nessun disco gestito per questa esecuzione")
            })?;
            a.opt(
                "-blockdev",
                format!(
                    "driver={},node-name=disk0,file.driver={proto},file.filename={}",
                    spec.format.driver(),
                    disk.qemu_value()
                ),
            );
            a.opt(
                "-device",
                format!("virtio-blk-pci,drive=disk0,bootindex={next_boot}"),
            );
        } else if paths.disk(run)?.is_some() {
            return Err(BackendError::invalid(
                "disk",
                "disco gestito presente ma non dichiarato nella specifica",
            ));
        }
        if let Some(media) = &self.data_media {
            let iso = paths.media(media)?;
            push_cdrom(&mut a, proto, "cd-data", &iso, 1, None);
        }
        a.opt("-nic", "none");
        a.opt("-device", "virtio-vga");
        a.opt("-device", "virtio-tablet-pci");
        a.opt("-device", "virtio-keyboard-pci");
        a.opt("-display", "none");
        a.opt("-vnc", format!("unix:{}", vnc_socket.qemu_value()));
        a.opt(
            "-qmp",
            format!("unix:{},server=on,wait=off", qmp_socket.qemu_value()),
        );
        a.flag("-S");

        Ok(QemuCommand {
            run_id: *run,
            program,
            args: a.0,
            qmp_socket,
            vnc_socket: Some(vnc_socket),
            stdout: run_dir.join_file(names::STDOUT)?,
            stderr: run_dir.join_file(names::STDERR)?,
            private_dir,
        })
    }
}

fn push_cdrom(
    a: &mut Args,
    proto: &str,
    node: &str,
    iso: &ManagedPath,
    ide_bus: u8,
    boot: Option<u32>,
) {
    a.opt(
        "-blockdev",
        format!(
            "driver=raw,node-name={node},read-only=on,file.driver={proto},file.filename={},file.read-only=on",
            iso.qemu_value()
        ),
    );
    let mut dev = format!("ide-cd,drive={node},bus=ide.{ide_bus}");
    if let Some(b) = boot {
        dev.push_str(&format!(",bootindex={b}"));
    }
    a.opt("-device", dev);
}

/// Comando minimo per il probe dell'acceleratore: VM in pausa senza firmware
/// pflash, senza dischi, senza display, QMP nella directory privata.
pub fn build_probe_command(
    program: ManagedPath,
    machine: &MachineType,
    accelerator: Accelerator,
    cpu: &CpuModel,
    private_dir: ManagedPath,
    tag: &RunId,
) -> Result<QemuCommand> {
    let qmp_socket = private_dir.join_file("probe-qmp.sock")?;
    qmp_socket.check_socket_len("qmp_socket")?;
    let mut a = Args(Vec::with_capacity(24));
    a.opt("-name", format!("guest=lightvm-probe-{tag}"));
    a.flag("-no-user-config");
    a.flag("-nodefaults");
    a.opt("-machine", format!("type={}", machine.as_str()));
    a.opt("-accel", accelerator.qemu_name());
    a.opt("-cpu", cpu.as_str());
    a.opt("-m", "size=128M");
    a.opt("-nic", "none");
    a.opt("-display", "none");
    a.opt(
        "-qmp",
        format!("unix:{},server=on,wait=off", qmp_socket.qemu_value()),
    );
    a.flag("-S");
    Ok(QemuCommand {
        run_id: *tag,
        program,
        args: a.0,
        qmp_socket,
        vnc_socket: None,
        stdout: private_dir.join_file("probe.stdout.log")?,
        stderr: private_dir.join_file("probe.stderr.log")?,
        private_dir,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;

    /// Percorsi di prova in una radice fissa (non toccano il disco).
    pub(crate) struct FakePaths {
        pub(crate) root: PathBuf,
        pub(crate) with_disk: bool,
        pub(crate) media: HashMap<MediaId, PathBuf>,
    }

    impl FakePaths {
        pub(crate) fn new() -> Self {
            let root = if cfg!(windows) {
                PathBuf::from(r"C:\lightvm-fake")
            } else {
                PathBuf::from("/lightvm-fake")
            };
            let mut media = HashMap::new();
            media.insert(media_ubuntu(), root.join("media").join("ubuntu.iso"));
            media.insert(media_dati(), root.join("media").join("dati.iso"));
            Self {
                root,
                with_disk: true,
                media,
            }
        }
    }

    impl ManagedPaths for FakePaths {
        fn qemu_system(&self) -> Result<ManagedPath> {
            ManagedPath::verify(self.root.join("qemu").join("qemu-system-x86_64.exe"))
        }
        fn firmware_code(&self) -> Result<ManagedPath> {
            ManagedPath::verify(self.root.join("fw").join("code.fd"))
        }
        fn firmware_vars(&self, run: &RunId) -> Result<ManagedPath> {
            ManagedPath::verify(self.root.join("runs").join(run.to_string()).join("vars.fd"))
        }
        fn disk(&self, run: &RunId) -> Result<Option<ManagedPath>> {
            if !self.with_disk {
                return Ok(None);
            }
            ManagedPath::verify(
                self.root
                    .join("runs")
                    .join(run.to_string())
                    .join("overlay.qcow2"),
            )
            .map(Some)
        }
        fn protected_readonly(&self, _run: &RunId) -> Result<Vec<ManagedPath>> {
            Ok(vec![])
        }
        fn media(&self, media: &MediaId) -> Result<ManagedPath> {
            let p = self
                .media
                .get(media)
                .ok_or_else(|| BackendError::invalid("media", "non registrato"))?;
            ManagedPath::verify(p)
        }
        fn run_dir(&self, run: &RunId) -> Result<ManagedPath> {
            ManagedPath::verify(self.root.join("runs").join(run.to_string()))
        }
        fn private_dir(&self, run: &RunId) -> Result<ManagedPath> {
            ManagedPath::verify(self.root.join("p").join(run.to_string()))
        }
    }

    /// Identificativo di esecuzione deterministico per i test.
    pub(crate) fn run_id(n: u8) -> RunId {
        RunId::from_unix_millis(1_700_000_000_000, [n; 10])
    }

    pub(crate) fn media_ubuntu() -> MediaId {
        MediaId::from_unix_millis(1_700_000_000_000, [1; 10])
    }

    pub(crate) fn media_dati() -> MediaId {
        MediaId::from_unix_millis(1_700_000_000_000, [2; 10])
    }

    pub(crate) fn spec() -> VmLaunchSpec {
        VmLaunchSpec {
            run_id: run_id(1),
            machine: MachineType::versioned_q35("pc-q35-11.1").unwrap(),
            accelerator: Accelerator::Whpx,
            cpu: CpuModel::nominal("qemu64").unwrap(),
            vcpus: 2,
            memory_mib: 4096,
            disk: Some(DiskSpec {
                format: DiskFormat::Qcow2,
            }),
            install_media: Some(media_ubuntu()),
            data_media: None,
        }
    }

    /// Opzioni ammesse con le chiavi consentite (`None` = valore senza chiavi,
    /// `Some(&[])` = flag senza valore).
    fn option_table() -> HashMap<&'static str, Option<&'static [&'static str]>> {
        let mut t: HashMap<&'static str, Option<&'static [&'static str]>> = HashMap::new();
        t.insert("-name", Some(&["guest"]));
        t.insert("-no-user-config", Some(&[]));
        t.insert("-nodefaults", Some(&[]));
        t.insert("-S", Some(&[]));
        t.insert("-machine", Some(&["type", "pflash0", "pflash1"]));
        t.insert("-accel", None);
        t.insert("-cpu", None);
        t.insert("-smp", Some(&["cpus"]));
        t.insert("-m", Some(&["size"]));
        t.insert(
            "-blockdev",
            Some(&[
                "driver",
                "node-name",
                "filename",
                "read-only",
                "file.driver",
                "file.filename",
                "file.read-only",
            ]),
        );
        t.insert("-device", Some(&["drive", "bootindex", "bus"]));
        t.insert("-nic", None);
        t.insert("-display", None);
        t.insert("-vnc", Some(&["unix"]));
        t.insert("-qmp", Some(&["unix", "server", "wait"]));
        t
    }

    const FLAGS: &[&str] = &["-no-user-config", "-nodefaults", "-S"];
    const DEVICES: &[&str] = &[
        "virtio-vga",
        "virtio-tablet-pci",
        "virtio-keyboard-pci",
        "virtio-blk-pci",
        "ide-cd",
    ];

    /// Verifica che gli argomenti siano soltanto coppie opzione/valore della
    /// tabella, con le sole chiavi ammesse, e che nessun valore contenga
    /// separatori in eccesso. Restituisce la descrizione della violazione.
    pub(crate) fn audit_args(args: &[OsString]) -> std::result::Result<(), String> {
        let table = option_table();
        let mut i = 0;
        let mut seen_nodefaults = false;
        let mut seen_nic_none = false;
        while i < args.len() {
            let opt = args[i].to_str().ok_or("argomento non UTF-8")?;
            let keys = table
                .get(opt)
                .ok_or_else(|| format!("opzione non ammessa: {opt:?}"))?;
            if FLAGS.contains(&opt) {
                seen_nodefaults |= opt == "-nodefaults";
                i += 1;
                continue;
            }
            let val = args
                .get(i + 1)
                .ok_or_else(|| format!("{opt} senza valore"))?
                .to_str()
                .ok_or("valore non UTF-8")?;
            if val.starts_with('-') {
                return Err(format!("valore di {opt} inizia con `-`: {val:?}"));
            }
            if val.chars().any(char::is_control) {
                return Err(format!("carattere di controllo in {opt}"));
            }
            match keys {
                None => {
                    if val.contains([',', '=', ':']) {
                        return Err(format!("{opt}: valore con separatori {val:?}"));
                    }
                    if opt == "-nic" {
                        if val != "none" {
                            return Err(format!("-nic {val}"));
                        }
                        seen_nic_none = true;
                    }
                    if opt == "-accel" && val == "tcg" {
                        return Err("TCG".into());
                    }
                }
                Some(allowed) => {
                    let mut parts = val.split(',');
                    if opt == "-device" {
                        let dev = parts.next().unwrap_or_default();
                        if !DEVICES.contains(&dev) {
                            return Err(format!("dispositivo non ammesso {dev:?}"));
                        }
                    }
                    for p in parts {
                        let (k, v) = if opt == "-vnc" || opt == "-qmp" {
                            match p.split_once(':') {
                                Some(("unix", v)) => ("unix", v),
                                _ => p
                                    .split_once('=')
                                    .ok_or_else(|| format!("{opt}: parte senza chiave {p:?}"))?,
                            }
                        } else {
                            p.split_once('=')
                                .ok_or_else(|| format!("{opt}: parte senza chiave {p:?}"))?
                        };
                        if !allowed.contains(&k) {
                            return Err(format!("{opt}: chiave non ammessa {k:?}"));
                        }
                        if k == "file.driver" && v != "file" {
                            return Err(format!("protocollo non ammesso {v:?}"));
                        }
                        if k == "driver" && !["file", "qcow2", "raw"].contains(&v) {
                            return Err(format!("driver di blocco non ammesso {v:?}"));
                        }
                        if v.is_empty() {
                            return Err(format!("{opt}: valore vuoto per {k}"));
                        }
                        let colon_ok = k == "filename" || k == "file.filename" || k == "unix";
                        let extra = if colon_ok {
                            // l'unico `:` ammesso è la lettera di unità
                            let bytes = v.as_bytes();
                            let rest = if cfg!(windows) && bytes.len() >= 2 && bytes[1] == b':' {
                                &v[2..]
                            } else {
                                v
                            };
                            rest.contains([':', '='])
                        } else {
                            v.contains([':', '='])
                        };
                        if extra {
                            return Err(format!("{opt}: separatori nel valore di {k}: {v:?}"));
                        }
                    }
                }
            }
            i += 2;
        }
        if !seen_nodefaults || !seen_nic_none {
            return Err("manca -nodefaults o -nic none".into());
        }
        Ok(())
    }

    fn strs(c: &QemuCommand) -> Vec<String> {
        c.args
            .iter()
            .map(|a| a.to_str().unwrap().to_owned())
            .collect()
    }

    #[test]
    fn comando_completo() {
        let cmd = spec().build(&FakePaths::new()).unwrap();
        let a = strs(&cmd);
        audit_args(&cmd.args).unwrap();
        let joined = a.join(" ");
        assert!(joined.contains("-machine type=pc-q35-11.1,pflash0=fw-code,pflash1=fw-vars"));
        assert!(joined.contains("-accel whpx"));
        assert!(joined.contains("-cpu qemu64"));
        assert!(joined.contains("-nodefaults"));
        assert!(joined.contains("-nic none"));
        assert!(joined.contains("-no-user-config"));
        assert!(joined.contains("-device ide-cd,drive=cd-install,bus=ide.0,bootindex=0"));
        assert!(joined.contains("-device virtio-blk-pci,drive=disk0,bootindex=1"));
        assert!(joined.contains("driver=qcow2,node-name=disk0,file.driver=file,file.filename="));
        assert!(joined.contains("read-only=on"));
        assert!(joined.contains("server=on,wait=off"));
        assert!(a.last().unwrap() == "-S");
        assert!(!joined.contains("tcg"));
        assert!(joined.contains(&format!("-name guest=lightvm-{}", run_id(1))));
        if cfg!(windows) {
            let r = run_id(1);
            assert!(joined.contains(&format!("-vnc unix:C:/lightvm-fake/p/{r}/vnc.sock")));
            assert!(joined.contains(&format!(
                "-qmp unix:C:/lightvm-fake/p/{r}/qmp.sock,server=on,wait=off"
            )));
            assert!(joined.contains("filename=C:/lightvm-fake/fw/code.fd,read-only=on"));
        }
        assert!(cmd.vnc_socket.is_some());
    }

    #[test]
    fn supporto_dati_senza_boot() {
        let mut s = spec();
        s.data_media = Some(media_dati());
        let cmd = s.build(&FakePaths::new()).unwrap();
        audit_args(&cmd.args).unwrap();
        assert!(strs(&cmd).contains(&"ide-cd,drive=cd-data,bus=ide.1".to_owned()));
        s.data_media = s.install_media;
        assert!(s.build(&FakePaths::new()).is_err());
    }

    #[test]
    fn senza_disco_e_senza_media() {
        let mut s = spec();
        s.disk = None;
        s.install_media = None;
        let mut p = FakePaths::new();
        assert!(s.build(&p).is_err(), "disco gestito non dichiarato");
        p.with_disk = false;
        let cmd = s.build(&p).unwrap();
        audit_args(&cmd.args).unwrap();
        assert!(!strs(&cmd).iter().any(|a| a.contains("bootindex")));
        let mut s2 = spec();
        s2.install_media = None;
        let mut p2 = FakePaths::new();
        p2.with_disk = false;
        assert!(s2.build(&p2).is_err(), "disco dichiarato ma assente");
    }

    #[test]
    fn risorse_non_valide() {
        let mut s = spec();
        s.vcpus = 0;
        assert!(matches!(
            s.build(&FakePaths::new()),
            Err(BackendError::ResourceLimit(_))
        ));
        s.vcpus = 2;
        s.memory_mib = 64;
        assert!(s.build(&FakePaths::new()).is_err());
        s.memory_mib = u32::MAX;
        assert!(s.build(&FakePaths::new()).is_err());
    }

    #[test]
    fn media_non_registrato() {
        let mut s = spec();
        s.install_media = Some(MediaId::from_unix_millis(1_700_000_000_000, [9; 10]));
        assert!(s.build(&FakePaths::new()).is_err());
    }

    #[test]
    fn macchina_e_cpu() {
        assert!(MachineType::versioned_q35("q35").is_err());
        assert!(MachineType::versioned_q35("pc-i440fx-11.1").is_err());
        assert!(MachineType::versioned_q35("pc-q35-11.1,accel=tcg").is_err());
        assert!(MachineType::versioned_q35("pc-q35-11").is_err());
        assert!(MachineType::versioned_q35("pc-q35-1x.1").is_err());
        assert_eq!(
            MachineType::versioned_q35("pc-q35-10.2").unwrap().version(),
            (10, 2)
        );
        assert!(CpuModel::nominal("host").is_err());
        assert!(CpuModel::nominal("max").is_err());
        assert!(CpuModel::nominal("qemu64,+vmx").is_err());
        assert!(CpuModel::nominal("Skylake-Client-v4").is_ok());
    }

    #[test]
    fn percorso_con_virgola_rifiutato() {
        let mut p = FakePaths::new();
        p.media.insert(
            media_ubuntu(),
            p.root.join("media").join("a,file.driver=nbd"),
        );
        assert!(spec().build(&p).is_err());
        let mut p = FakePaths::new();
        p.media.insert(media_ubuntu(), p.root.join("x:y.iso"));
        assert!(spec().build(&p).is_err());
    }

    #[test]
    fn comando_probe() {
        let p = FakePaths::new();
        let run = run_id(2);
        let cmd = build_probe_command(
            p.qemu_system().unwrap(),
            &MachineType::versioned_q35("pc-q35-11.1").unwrap(),
            Accelerator::Whpx,
            &CpuModel::nominal("qemu64").unwrap(),
            p.private_dir(&run).unwrap(),
            &run,
        )
        .unwrap();
        audit_args(&cmd.args).unwrap();
        assert!(cmd.vnc_socket.is_none());
    }
}

#[cfg(test)]
mod proptests {
    use super::tests::{FakePaths, audit_args, media_dati, media_ubuntu, spec};
    use super::*;
    use proptest::prelude::*;

    /// Stringhe ostili: separatori QEMU, prefissi di opzione e di protocollo,
    /// controllo, Unicode, percorsi UNC/dispositivo e traversal.
    fn hostile() -> impl Strategy<Value = String> {
        prop_oneof![
            "[ -~]{0,40}",
            r"\PC{0,24}",
            prop::sample::select(vec![
                "-S".to_owned(),
                "a,file.driver=nbd,file.host=evil".to_owned(),
                "x,if=none".to_owned(),
                "unix:/tmp/x,server=on".to_owned(),
                "tcp:0.0.0.0:4444".to_owned(),
                "nbd://evil/x".to_owned(),
                "..".to_owned(),
                "a\nb".to_owned(),
                "a\u{0}b".to_owned(),
                "host".to_owned(),
                "q35".to_owned(),
                "tcg".to_owned(),
                "pc-q35-11.1,accel=tcg".to_owned(),
                "qemu64,+vmx".to_owned(),
                r"\\server\share\x".to_owned(),
                r"\\?\C:\x".to_owned(),
                r"\\.\PhysicalDrive0".to_owned(),
                r"C:\a\..\b".to_owned(),
                r"C:\a b\x.iso".to_owned(),
                "e1000,netdev=n".to_owned(),
            ]),
        ]
    }

    fn check(cmd: &Result<QemuCommand>) -> std::result::Result<(), TestCaseError> {
        if let Ok(c) = cmd
            && let Err(e) = audit_args(&c.args)
        {
            return Err(TestCaseError::fail(format!("iniezione: {e}\n{:?}", c.args)));
        }
        Ok(())
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(2000))]

        /// Gli identificativi sono UUID v7 tipizzati (`lightvm-proto`): non
        /// possono contenere separatori. Si provano UUID arbitrari, inclusi
        /// rifiuti da stringhe ostili.
        #[test]
        fn identificativi_qualsiasi(
            ms in any::<u64>(),
            r in any::<[u8; 10]>(),
            m in any::<[u8; 10]>(),
            text in hostile(),
        ) {
            prop_assert!(text.parse::<RunId>().is_err() || text.len() == 36);
            prop_assert!(text.parse::<MediaId>().is_err() || text.len() == 36);
            let ms = ms % (1u64 << 48);
            let mut s = spec();
            s.run_id = RunId::from_unix_millis(ms, r);
            let mut p = FakePaths::new();
            let install = MediaId::from_unix_millis(ms, m);
            p.media.insert(install, p.root.join("media").join("x.iso"));
            s.install_media = Some(install);
            check(&s.build(&p))?;
        }

        #[test]
        fn macchina_e_cpu_ostili(m in hostile(), c in hostile()) {
            let mut s = spec();
            if let Ok(mt) = MachineType::versioned_q35(&m) { s.machine = mt; }
            if let Ok(cm) = CpuModel::nominal(&c) { s.cpu = cm; }
            check(&s.build(&FakePaths::new()))?;
        }

        #[test]
        fn percorsi_ostili(a in hostile(), b in hostile(), c in hostile()) {
            let mut p = FakePaths::new();
            p.media.insert(media_ubuntu(), p.root.join(&a).join(&b));
            let mut s = spec();
            s.data_media = Some(media_dati());
            p.media.insert(media_dati(), p.root.join(&c));
            check(&s.build(&p))?;
            // percorso radice arbitrario (anche relativo, UNC o dispositivo)
            p.root = std::path::PathBuf::from(&a);
            check(&s.build(&p))?;
        }

        #[test]
        fn risorse_qualsiasi(v in any::<u32>(), m in any::<u32>()) {
            let mut s = spec();
            s.vcpus = v;
            s.memory_mib = m;
            let r = s.build(&FakePaths::new());
            check(&r)?;
            let ok_v = (limits::MIN_VCPUS..=limits::MAX_VCPUS).contains(&v);
            let ok_m = (limits::MIN_MEMORY_MIB..=limits::MAX_MEMORY_MIB).contains(&m);
            prop_assert_eq!(r.is_ok(), ok_v && ok_m);
        }
    }
}
