//! Construction of the QEMU command from controlled options (ROADMAP 6.3, 7.2).
//!
//! [`VmLaunchSpec`] describes the VM with closed types; [`VmLaunchSpec::build`]
//! produces the arguments as a `Vec<OsString>` passed directly to the process,
//! without a shell. Properties guaranteed by construction:
//!
//! - options always present: `-no-user-config -nodefaults -nic none`,
//!   `-display none`, `-S` (the guest starts only after the post-launch checks,
//!   see [`crate::run`]);
//! - explicit accelerator, never TCG (the [`Accelerator`] type has no
//!   TCG variant: ADR-0007);
//! - **versioned** q35 machine (`pc-q35-X.Y`) chosen by the probe;
//! - nominal CPU model (`host` and `max` rejected, F0-02 9.1);
//! - pflash firmware: read-only code, per-run variables;
//! - disks with structured `-blockdev` (`driver`, `node-name`,
//!   `file.driver=file`, `file.filename`), explicit format and only the `file`
//!   protocol;
//! - installation ISO and data medium read-only on `ide-cd`;
//! - VNC display on a Unix socket and QMP on a Unix socket in the private directory;
//! - HID devices from an allowlist (`virtio-tablet-pci`, `virtio-keyboard-pci`).

use std::ffi::OsString;

use crate::error::{BackendError, Result};
use crate::paths::{ManagedPath, ManagedPaths, names};
use crate::validate::{check_block_protocol, check_token};
use lightvm_proto::{MediaId, RunId};

/// Hardware accelerator. There is no TCG variant (ADR-0007).
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
    /// Name used by QEMU (`-accel`, `query-accelerators`).
    pub fn qemu_name(self) -> &'static str {
        match self {
            Self::Whpx => "whpx",
            Self::Kvm => "kvm",
            Self::Hvf => "hvf",
        }
    }

    /// Accelerator expected for the current host (ROADMAP section 5).
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

/// Versioned q35 machine type (`pc-q35-X.Y`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct MachineType(String);

impl MachineType {
    /// Accepts only `pc-q35-<major>.<minor>` names; the `q35` alias is rejected
    /// because its meaning changes with the QEMU version.
    pub fn versioned_q35(name: &str) -> Result<Self> {
        check_token("machine", name)?;
        if parse_q35_version(name).is_none() {
            return Err(BackendError::invalid(
                "machine",
                format!("`{name}` is not a versioned q35 type (expected pc-q35-X.Y)"),
            ));
        }
        Ok(Self(name.to_owned()))
    }

    /// QEMU name.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Version `(major, minor)`.
    pub fn version(&self) -> (u32, u32) {
        parse_q35_version(&self.0).unwrap_or((0, 0))
    }
}

/// Extracts `(major, minor)` from `pc-q35-X.Y`.
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

/// Nominal CPU model.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CpuModel(String);

impl CpuModel {
    /// Model verified up to the live desktop on this host (F0-02 9.2).
    pub const VERIFIED_DEFAULT: &'static str = "qemu64";

    /// Accepts a nominal model; `host` and `max` are rejected (with OVMF
    /// under WHPX they do not boot the firmware, F0-02 9.1). The presence of the model
    /// in the binary must be verified with [`crate::HostCapabilities::cpu_model`].
    pub fn nominal(name: &str) -> Result<Self> {
        check_token("cpu", name)?;
        if matches!(name, "host" | "max") {
            return Err(BackendError::Unsupported {
                what: format!("CPU model `{name}`"),
                hint: "with OVMF under WHPX `host`/`max` hang the firmware (F0-02); \
                       use a nominal model such as `qemu64` or `Skylake-Client-v4`"
                    .into(),
            });
        }
        Ok(Self(name.to_owned()))
    }

    /// QEMU name.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Explicit disk format (no automatic format detection).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DiskFormat {
    /// qcow2 (baseline overlay).
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

/// VM disk. The path is supplied by [`ManagedPaths::disk`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiskSpec {
    /// File format.
    pub format: DiskFormat,
}

/// Validation limits of the virtual resources.
pub mod limits {
    /// Minimum vCPUs.
    pub const MIN_VCPUS: u32 = 1;
    /// Maximum vCPUs accepted by the builder (the effective limit also depends
    /// on the host and on the policy, verified upstream).
    pub const MAX_VCPUS: u32 = 64;
    /// Minimum RAM in MiB.
    pub const MIN_MEMORY_MIB: u32 = 128;
    /// Maximum RAM in MiB accepted by the builder.
    pub const MAX_MEMORY_MIB: u32 = 256 * 1024;
}

/// Launch specification of a VM.
#[derive(Clone, Debug)]
pub struct VmLaunchSpec {
    /// Run (used in `-name` and to obtain the paths).
    pub run_id: RunId,
    /// Versioned q35 machine.
    pub machine: MachineType,
    /// Accelerator.
    pub accelerator: Accelerator,
    /// Nominal CPU model.
    pub cpu: CpuModel,
    /// Number of vCPUs.
    pub vcpus: u32,
    /// RAM in MiB.
    pub memory_mib: u32,
    /// Disk, if any.
    pub disk: Option<DiskSpec>,
    /// Installation ISO (bootable), from the media registry.
    pub install_media: Option<MediaId>,
    /// Read-only data medium, from the media registry.
    pub data_media: Option<MediaId>,
    /// Job Object limits (CPU cap, job memory), ADR-0017.
    pub limits: crate::process::JobLimits,
}

/// QEMU command ready to spawn.
#[derive(Clone, Debug)]
pub struct QemuCommand {
    /// Run the process belongs to (identity with handle and creation
    /// time).
    pub run_id: RunId,
    /// Executable.
    pub program: ManagedPath,
    /// Structured arguments.
    pub args: Vec<OsString>,
    /// QMP socket.
    pub qmp_socket: ManagedPath,
    /// VNC socket (`None` for the probe command).
    pub vnc_socket: Option<ManagedPath>,
    /// stdout file.
    pub stdout: ManagedPath,
    /// stderr file.
    pub stderr: ManagedPath,
    /// Private directory (sockets, screendumps).
    pub private_dir: ManagedPath,
    /// Limits of the process container.
    pub limits: crate::process::JobLimits,
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
    /// Validates the virtual resources.
    pub fn validate(&self) -> Result<()> {
        if !(limits::MIN_VCPUS..=limits::MAX_VCPUS).contains(&self.vcpus) {
            return Err(BackendError::ResourceLimit(format!(
                "{} vCPUs outside the range {}..={}",
                self.vcpus,
                limits::MIN_VCPUS,
                limits::MAX_VCPUS
            )));
        }
        if !(limits::MIN_MEMORY_MIB..=limits::MAX_MEMORY_MIB).contains(&self.memory_mib) {
            return Err(BackendError::ResourceLimit(format!(
                "{} MiB of RAM outside the range {}..={}",
                self.memory_mib,
                limits::MIN_MEMORY_MIB,
                limits::MAX_MEMORY_MIB
            )));
        }
        if self.install_media.is_some() && self.install_media == self.data_media {
            return Err(BackendError::invalid(
                "data_media",
                "the same medium as both installation and data",
            ));
        }
        Ok(())
    }

    /// Builds the command. All paths come from `paths`.
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
        // The only allowed block protocol (allowlist, ADR-0004/0010).
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
            let disk = paths
                .disk(run)?
                .ok_or_else(|| BackendError::invalid("disk", "no managed disk for this run"))?;
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
                "managed disk present but not declared in the specification",
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
            limits: self.limits,
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

/// Minimal command for the accelerator probe: paused VM without pflash
/// firmware, without disks, without display, QMP in the private directory.
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
        limits: crate::process::JobLimits::default(),
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;

    /// Test paths under a fixed root (they do not touch the disk).
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
            media.insert(media_data(), root.join("media").join("data.iso"));
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
                .ok_or_else(|| BackendError::invalid("media", "not registered"))?;
            ManagedPath::verify(p)
        }
        fn run_dir(&self, run: &RunId) -> Result<ManagedPath> {
            ManagedPath::verify(self.root.join("runs").join(run.to_string()))
        }
        fn private_dir(&self, run: &RunId) -> Result<ManagedPath> {
            ManagedPath::verify(self.root.join("p").join(run.to_string()))
        }
    }

    /// Deterministic run identifier for tests.
    pub(crate) fn run_id(n: u8) -> RunId {
        RunId::from_unix_millis(1_700_000_000_000, [n; 10])
    }

    pub(crate) fn media_ubuntu() -> MediaId {
        MediaId::from_unix_millis(1_700_000_000_000, [1; 10])
    }

    pub(crate) fn media_data() -> MediaId {
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
            limits: crate::process::JobLimits::default(),
        }
    }

    /// Allowed options with the permitted keys (`None` = value without keys,
    /// `Some(&[])` = flag without value).
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

    /// Verifies that the arguments are only option/value pairs from the
    /// table, with only the allowed keys, and that no value contains
    /// extra separators. Returns the description of the violation.
    pub(crate) fn audit_args(args: &[OsString]) -> std::result::Result<(), String> {
        let table = option_table();
        let mut i = 0;
        let mut seen_nodefaults = false;
        let mut seen_nic_none = false;
        while i < args.len() {
            let opt = args[i].to_str().ok_or("argument is not UTF-8")?;
            let keys = table
                .get(opt)
                .ok_or_else(|| format!("option not allowed: {opt:?}"))?;
            if FLAGS.contains(&opt) {
                seen_nodefaults |= opt == "-nodefaults";
                i += 1;
                continue;
            }
            let val = args
                .get(i + 1)
                .ok_or_else(|| format!("{opt} without value"))?
                .to_str()
                .ok_or("value is not UTF-8")?;
            if val.starts_with('-') {
                return Err(format!("value of {opt} starts with `-`: {val:?}"));
            }
            if val.chars().any(char::is_control) {
                return Err(format!("control character in {opt}"));
            }
            match keys {
                None => {
                    if val.contains([',', '=', ':']) {
                        return Err(format!("{opt}: value with separators {val:?}"));
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
                            return Err(format!("device not allowed {dev:?}"));
                        }
                    }
                    for p in parts {
                        let (k, v) = if opt == "-vnc" || opt == "-qmp" {
                            match p.split_once(':') {
                                Some(("unix", v)) => ("unix", v),
                                _ => p
                                    .split_once('=')
                                    .ok_or_else(|| format!("{opt}: part without key {p:?}"))?,
                            }
                        } else {
                            p.split_once('=')
                                .ok_or_else(|| format!("{opt}: part without key {p:?}"))?
                        };
                        if !allowed.contains(&k) {
                            return Err(format!("{opt}: key not allowed {k:?}"));
                        }
                        if k == "file.driver" && v != "file" {
                            return Err(format!("protocol not allowed {v:?}"));
                        }
                        if k == "driver" && !["file", "qcow2", "raw"].contains(&v) {
                            return Err(format!("block driver not allowed {v:?}"));
                        }
                        if v.is_empty() {
                            return Err(format!("{opt}: empty value for {k}"));
                        }
                        let colon_ok = k == "filename" || k == "file.filename" || k == "unix";
                        let extra = if colon_ok {
                            // the only `:` allowed is the drive letter
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
                            return Err(format!("{opt}: separators in the value of {k}: {v:?}"));
                        }
                    }
                }
            }
            i += 2;
        }
        if !seen_nodefaults || !seen_nic_none {
            return Err("-nodefaults or -nic none missing".into());
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
    fn full_command() {
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
    fn data_medium_without_boot() {
        let mut s = spec();
        s.data_media = Some(media_data());
        let cmd = s.build(&FakePaths::new()).unwrap();
        audit_args(&cmd.args).unwrap();
        assert!(strs(&cmd).contains(&"ide-cd,drive=cd-data,bus=ide.1".to_owned()));
        s.data_media = s.install_media;
        assert!(s.build(&FakePaths::new()).is_err());
    }

    #[test]
    fn without_disk_and_without_media() {
        let mut s = spec();
        s.disk = None;
        s.install_media = None;
        let mut p = FakePaths::new();
        assert!(s.build(&p).is_err(), "managed disk not declared");
        p.with_disk = false;
        let cmd = s.build(&p).unwrap();
        audit_args(&cmd.args).unwrap();
        assert!(!strs(&cmd).iter().any(|a| a.contains("bootindex")));
        let mut s2 = spec();
        s2.install_media = None;
        let mut p2 = FakePaths::new();
        p2.with_disk = false;
        assert!(s2.build(&p2).is_err(), "disk declared but missing");
    }

    #[test]
    fn invalid_resources() {
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
    fn unregistered_media() {
        let mut s = spec();
        s.install_media = Some(MediaId::from_unix_millis(1_700_000_000_000, [9; 10]));
        assert!(s.build(&FakePaths::new()).is_err());
    }

    #[test]
    fn machine_and_cpu() {
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
    fn path_with_comma_rejected() {
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
    fn probe_command() {
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
    use super::tests::{FakePaths, audit_args, media_data, media_ubuntu, spec};
    use super::*;
    use proptest::prelude::*;

    /// Hostile strings: QEMU separators, option and protocol prefixes,
    /// control characters, Unicode, UNC/device paths and traversal.
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
            return Err(TestCaseError::fail(format!("injection: {e}\n{:?}", c.args)));
        }
        Ok(())
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(2000))]

        /// Identifiers are typed UUID v7s (`lightvm-proto`): they
        /// cannot contain separators. Arbitrary UUIDs are tried, including
        /// rejections of hostile strings.
        #[test]
        fn arbitrary_identifiers(
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
        fn hostile_machine_and_cpu(m in hostile(), c in hostile()) {
            let mut s = spec();
            if let Ok(mt) = MachineType::versioned_q35(&m) { s.machine = mt; }
            if let Ok(cm) = CpuModel::nominal(&c) { s.cpu = cm; }
            check(&s.build(&FakePaths::new()))?;
        }

        #[test]
        fn hostile_paths(a in hostile(), b in hostile(), c in hostile()) {
            let mut p = FakePaths::new();
            p.media.insert(media_ubuntu(), p.root.join(&a).join(&b));
            let mut s = spec();
            s.data_media = Some(media_data());
            p.media.insert(media_data(), p.root.join(&c));
            check(&s.build(&p))?;
            // arbitrary root path (also relative, UNC or device)
            p.root = std::path::PathBuf::from(&a);
            check(&s.build(&p))?;
        }

        #[test]
        fn arbitrary_resources(v in any::<u32>(), m in any::<u32>()) {
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
