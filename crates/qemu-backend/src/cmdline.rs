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
//! - **versioned** machine (`pc-q35-X.Y` or `pc-i440fx-X.Y`) chosen by the
//!   probe, of the family required by the [`DeviceProfile`];
//! - nominal CPU model (`host` and `max` rejected, F0-02 9.1);
//! - firmware per profile: UEFI as pflash (read-only code, per-run
//!   variables) or SeaBIOS with `-bios` (verified image, no variables);
//! - disks with structured `-blockdev` (`driver`, `node-name`,
//!   `file.driver=file`, `file.filename`), explicit format and only the `file`
//!   protocol;
//! - installation ISO and data medium read-only on `ide-cd`;
//! - VNC display on a Unix socket and QMP on a Unix socket in the private directory;
//! - devices only from the closed set of the hardware profiles (ADR-0027):
//!   `virtio-blk-pci` or `ide-hd`, `virtio-vga` or `VGA`, virtio
//!   tablet/keyboard or a USB controller (`qemu-xhci`, `piix3-usb-uhci`)
//!   with `usb-tablet`.

use std::ffi::OsString;

use crate::error::{BackendError, Result};
use crate::paths::{ManagedPath, ManagedPaths, names};
use crate::validate::{check_block_protocol, check_token};
use lightvm_proto::{Firmware, GuestOs, MediaId, RunId};

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

/// Chipset family of a versioned machine type (ADR-0027).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MachineFamily {
    /// Q35 + ICH9 (`pc-q35-X.Y`): AHCI controller with ports `ide.0`..`ide.5`.
    Q35,
    /// i440FX + PIIX (`pc-i440fx-X.Y`): PIIX IDE, buses `ide.0`/`ide.1` with
    /// two units each.
    I440fx,
}

impl MachineFamily {
    /// Both families, in display order.
    pub const ALL: [MachineFamily; 2] = [MachineFamily::Q35, MachineFamily::I440fx];

    /// Prefix of the versioned names of the family.
    pub fn prefix(self) -> &'static str {
        match self {
            Self::Q35 => "pc-q35-",
            Self::I440fx => "pc-i440fx-",
        }
    }

    /// Short name for messages.
    pub fn label(self) -> &'static str {
        match self {
            Self::Q35 => "q35",
            Self::I440fx => "i440fx",
        }
    }
}

/// Versioned machine type (`pc-q35-X.Y` or `pc-i440fx-X.Y`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct MachineType {
    name: String,
    family: MachineFamily,
    version: (u32, u32),
}

impl MachineType {
    /// Accepts only `pc-q35-<major>.<minor>` and `pc-i440fx-<major>.<minor>`
    /// names; the `q35` and `pc` aliases are rejected because their meaning
    /// changes with the QEMU version.
    pub fn versioned(name: &str) -> Result<Self> {
        check_token("machine", name)?;
        let (family, major, minor) = parse_versioned_machine(name).ok_or_else(|| {
            BackendError::invalid(
                "machine",
                format!(
                    "`{name}` is not a versioned machine type (expected pc-q35-X.Y or \
                     pc-i440fx-X.Y)"
                ),
            )
        })?;
        Ok(Self {
            name: name.to_owned(),
            family,
            version: (major, minor),
        })
    }

    /// Accepts only `pc-q35-<major>.<minor>` names.
    pub fn versioned_q35(name: &str) -> Result<Self> {
        Self::versioned_in(MachineFamily::Q35, name)
    }

    /// Accepts only `pc-i440fx-<major>.<minor>` names.
    pub fn versioned_i440fx(name: &str) -> Result<Self> {
        Self::versioned_in(MachineFamily::I440fx, name)
    }

    /// Accepts only versioned names of `family`.
    pub fn versioned_in(family: MachineFamily, name: &str) -> Result<Self> {
        match Self::versioned(name) {
            Ok(m) if m.family == family => Ok(m),
            _ => Err(BackendError::invalid(
                "machine",
                format!(
                    "`{name}` is not a versioned {} type (expected {}X.Y)",
                    family.label(),
                    family.prefix()
                ),
            )),
        }
    }

    /// QEMU name.
    pub fn as_str(&self) -> &str {
        &self.name
    }

    /// Chipset family.
    pub fn family(&self) -> MachineFamily {
        self.family
    }

    /// Version `(major, minor)`.
    pub fn version(&self) -> (u32, u32) {
        self.version
    }
}

/// Extracts the family and `(major, minor)` from `pc-q35-X.Y` or
/// `pc-i440fx-X.Y` (decimal digits only, nothing after the minor).
pub(crate) fn parse_versioned_machine(name: &str) -> Option<(MachineFamily, u32, u32)> {
    let (family, v) = MachineFamily::ALL
        .iter()
        .find_map(|f| name.strip_prefix(f.prefix()).map(|v| (*f, v)))?;
    let (a, b) = v.split_once('.')?;
    if a.is_empty() || b.is_empty() || !a.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    if !b.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some((family, a.parse().ok()?, b.parse().ok()?))
}

/// Guest firmware kind of a hardware profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FirmwareKind {
    /// OVMF as pflash: read-only code plus per-VM writable variables.
    Uefi,
    /// SeaBIOS (`share/bios-256k.bin` of the verified build) passed with
    /// `-bios`: no pflash, no variables.
    SeaBios,
}

impl FirmwareKind {
    /// Kind for the firmware of the VM configuration.
    pub fn from_proto(firmware: Firmware) -> Self {
        match firmware {
            Firmware::Uefi => Self::Uefi,
            Firmware::Bios => Self::SeaBios,
        }
    }

    /// Whether the firmware needs per-VM variables (`pflash1`).
    pub fn has_vars(self) -> bool {
        matches!(self, Self::Uefi)
    }

    /// Short name for messages.
    pub fn label(self) -> &'static str {
        match self {
            Self::Uefi => "UEFI",
            Self::SeaBios => "SeaBIOS",
        }
    }
}

/// Bus of the main disk.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DiskBus {
    /// `virtio-blk-pci`.
    Virtio,
    /// `ide-hd` on port `ide.2` of the q35 ICH9 AHCI controller.
    Ahci,
    /// `ide-hd` on `ide.0`, unit 0, of the i440fx PIIX IDE controller.
    PiixIde,
}

/// Display adapter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DisplayDevice {
    /// `virtio-vga`.
    VirtioVga,
    /// `VGA` (standard VGA with Bochs VBE).
    StdVga,
}

impl DisplayDevice {
    /// QEMU device name.
    pub fn qemu_name(self) -> &'static str {
        match self {
            Self::VirtioVga => "virtio-vga",
            Self::StdVga => "VGA",
        }
    }
}

/// Absolute pointer device (the console sends absolute coordinates) and,
/// for virtio, the keyboard. The Windows profiles use the board's i8042
/// PS/2 keyboard, present with `-nodefaults`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PointerDevice {
    /// `virtio-tablet-pci` + `virtio-keyboard-pci`.
    Virtio,
    /// `qemu-xhci` controller + `usb-tablet` on its bus.
    UsbXhciTablet,
    /// `piix3-usb-uhci` controller + `usb-tablet` on its bus.
    UsbUhciTablet,
}

/// Id of the USB controller of the USB pointer profiles.
const USB_CONTROLLER_ID: &str = "usb0";

/// Hardware profile of a VM (ADR-0027): one of the closed set of
/// combinations of the contract table, obtainable only through
/// [`DeviceProfile::for_guest`] or the associated constants.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DeviceProfile {
    machine_family: MachineFamily,
    firmware: FirmwareKind,
    disk_bus: DiskBus,
    display: DisplayDevice,
    pointer: PointerDevice,
}

impl DeviceProfile {
    /// Ubuntu / other Linux: q35, UEFI, virtio disk, display and input.
    pub const LINUX: Self = Self {
        machine_family: MachineFamily::Q35,
        firmware: FirmwareKind::Uefi,
        disk_bus: DiskBus::Virtio,
        display: DisplayDevice::VirtioVga,
        pointer: PointerDevice::Virtio,
    };
    /// Windows 8.1 / 10 with UEFI: q35, AHCI, VGA, xHCI tablet.
    pub const WINDOWS_UEFI: Self = Self {
        machine_family: MachineFamily::Q35,
        firmware: FirmwareKind::Uefi,
        disk_bus: DiskBus::Ahci,
        display: DisplayDevice::StdVga,
        pointer: PointerDevice::UsbXhciTablet,
    };
    /// Windows 8.1 / 10 with legacy BIOS: q35, SeaBIOS, AHCI, VGA, xHCI tablet.
    pub const WINDOWS_BIOS: Self = Self {
        machine_family: MachineFamily::Q35,
        firmware: FirmwareKind::SeaBios,
        disk_bus: DiskBus::Ahci,
        display: DisplayDevice::StdVga,
        pointer: PointerDevice::UsbXhciTablet,
    };
    /// Windows Vista / 7: q35, SeaBIOS, AHCI, VGA, UHCI tablet.
    pub const WINDOWS_VISTA_7: Self = Self {
        machine_family: MachineFamily::Q35,
        firmware: FirmwareKind::SeaBios,
        disk_bus: DiskBus::Ahci,
        display: DisplayDevice::StdVga,
        pointer: PointerDevice::UsbUhciTablet,
    };
    /// Windows XP: i440fx, SeaBIOS, PIIX IDE, VGA, UHCI tablet.
    pub const WINDOWS_XP: Self = Self {
        machine_family: MachineFamily::I440fx,
        firmware: FirmwareKind::SeaBios,
        disk_bus: DiskBus::PiixIde,
        display: DisplayDevice::StdVga,
        pointer: PointerDevice::UsbUhciTablet,
    };

    /// Every profile, in the order of the contract table.
    pub const ALL: [DeviceProfile; 5] = [
        Self::LINUX,
        Self::WINDOWS_UEFI,
        Self::WINDOWS_BIOS,
        Self::WINDOWS_VISTA_7,
        Self::WINDOWS_XP,
    ];

    /// Profile for a guest and its firmware (contract table, ADR-0027).
    /// Combinations outside [`GuestOs::allowed_firmware`] are rejected.
    pub fn for_guest(os: GuestOs, firmware: Firmware) -> Result<Self> {
        if !os.allowed_firmware().contains(&firmware) {
            return Err(BackendError::Unsupported {
                what: format!("firmware {firmware:?} for the guest {os:?}"),
                hint: "the guest supports only its allowed firmware (ADR-0027)".into(),
            });
        }
        Ok(match (os, firmware) {
            (GuestOs::UbuntuLtsX86_64 | GuestOs::OtherLinuxX86_64, _) => Self::LINUX,
            (GuestOs::Windows81 | GuestOs::Windows10, Firmware::Uefi) => Self::WINDOWS_UEFI,
            (GuestOs::Windows81 | GuestOs::Windows10, Firmware::Bios) => Self::WINDOWS_BIOS,
            (GuestOs::WindowsVista | GuestOs::Windows7, _) => Self::WINDOWS_VISTA_7,
            (GuestOs::WindowsXp, _) => Self::WINDOWS_XP,
        })
    }

    /// Chipset family the machine type must belong to.
    pub fn machine_family(&self) -> MachineFamily {
        self.machine_family
    }

    /// Firmware kind.
    pub fn firmware(&self) -> FirmwareKind {
        self.firmware
    }

    /// Bus of the main disk.
    pub fn disk_bus(&self) -> DiskBus {
        self.disk_bus
    }

    /// Display adapter.
    pub fn display(&self) -> DisplayDevice {
        self.display
    }

    /// Pointer (and keyboard) devices.
    pub fn pointer(&self) -> PointerDevice {
        self.pointer
    }

    /// Combination the accelerator probe must have verified before a start
    /// with this profile (ADR-0007: probe with the configuration actually used).
    pub fn probe_target(&self) -> ProbeTarget {
        ProbeTarget {
            family: self.machine_family,
            firmware: self.firmware,
        }
    }

    /// `-device` names the command can emit for this profile (the board's
    /// built-in controllers, AHCI, PIIX IDE and i8042, are not devices).
    pub fn required_devices(&self) -> Vec<&'static str> {
        let mut v = vec![self.display.qemu_name()];
        match self.pointer {
            PointerDevice::Virtio => v.extend(["virtio-tablet-pci", "virtio-keyboard-pci"]),
            PointerDevice::UsbXhciTablet => v.extend(["qemu-xhci", "usb-tablet"]),
            PointerDevice::UsbUhciTablet => v.extend(["piix3-usb-uhci", "usb-tablet"]),
        }
        v.push(match self.disk_bus {
            DiskBus::Virtio => "virtio-blk-pci",
            DiskBus::Ahci | DiskBus::PiixIde => "ide-hd",
        });
        v.push("ide-cd");
        v
    }

    /// `-device` value of the main disk.
    fn disk_device(&self, bootindex: u32) -> String {
        match self.disk_bus {
            DiskBus::Virtio => format!("virtio-blk-pci,drive=disk0,bootindex={bootindex}"),
            DiskBus::Ahci => format!("ide-hd,drive=disk0,bus=ide.2,bootindex={bootindex}"),
            DiskBus::PiixIde => {
                format!("ide-hd,drive=disk0,bus=ide.0,unit=0,bootindex={bootindex}")
            }
        }
    }

    /// Bus (and unit) of the installation (`slot` 0) or data (`slot` 1) CD.
    fn cdrom_location(&self, slot: u8) -> String {
        match self.machine_family {
            MachineFamily::Q35 => format!("bus=ide.{slot}"),
            MachineFamily::I440fx => format!("bus=ide.1,unit={slot}"),
        }
    }
}

/// Machine family and firmware kind an accelerator probe was run with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProbeTarget {
    /// Chipset family.
    pub family: MachineFamily,
    /// Firmware kind.
    pub firmware: FirmwareKind,
}

impl ProbeTarget {
    /// q35 + UEFI: the reference (Linux) configuration probed at service start.
    pub const REFERENCE: Self = Self {
        family: MachineFamily::Q35,
        firmware: FirmwareKind::Uefi,
    };
}

/// Firmware image passed to the accelerator probe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProbeFirmware {
    /// OVMF code, attached read-only as `pflash0` (no variables: the probe
    /// stays paused and never runs the firmware).
    Uefi(ManagedPath),
    /// SeaBIOS image passed with `-bios`.
    SeaBios(ManagedPath),
}

impl ProbeFirmware {
    /// Firmware kind.
    pub fn kind(&self) -> FirmwareKind {
        match self {
            Self::Uefi(_) => FirmwareKind::Uefi,
            Self::SeaBios(_) => FirmwareKind::SeaBios,
        }
    }
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
    /// Versioned machine; its family must match `profile`.
    pub machine: MachineType,
    /// Hardware profile (firmware, disk bus, display, pointer).
    pub profile: DeviceProfile,
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
        if self.machine.family() != self.profile.machine_family() {
            return Err(BackendError::invalid(
                "machine",
                format!(
                    "machine `{}` is not a {} type, required by the hardware profile",
                    self.machine.as_str(),
                    self.profile.machine_family().label()
                ),
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
        let profile = &self.profile;
        // Firmware: pflash code + per-run variables (UEFI) or `-bios` image
        // (SeaBIOS, no variables).
        let firmware = match profile.firmware() {
            FirmwareKind::Uefi => {
                let code = paths.firmware_code()?;
                let vars = paths.firmware_vars(run)?.ok_or_else(|| {
                    BackendError::invalid(
                        "firmware_vars",
                        "the UEFI profile needs the per-run variables",
                    )
                })?;
                (code, Some(vars))
            }
            FirmwareKind::SeaBios => {
                if paths.firmware_vars(run)?.is_some() {
                    return Err(BackendError::invalid(
                        "firmware_vars",
                        "UEFI variables given for a SeaBIOS profile",
                    ));
                }
                (paths.seabios()?, None)
            }
        };
        // The only allowed block protocol (allowlist, ADR-0004/0010).
        let proto = check_block_protocol("file")?;

        let mut a = Args(Vec::with_capacity(64));
        a.opt("-name", format!("guest=lightvm-{run}"));
        a.flag("-no-user-config");
        a.flag("-nodefaults");
        match &firmware {
            (_, Some(_)) => a.opt(
                "-machine",
                format!(
                    "type={},pflash0=fw-code,pflash1=fw-vars",
                    self.machine.as_str()
                ),
            ),
            (_, None) => a.opt("-machine", format!("type={}", self.machine.as_str())),
        }
        a.opt("-accel", self.accelerator.qemu_name());
        a.opt("-cpu", self.cpu.as_str());
        a.opt("-smp", format!("cpus={}", self.vcpus));
        a.opt("-m", format!("size={}M", self.memory_mib));
        match &firmware {
            (code, Some(vars)) => {
                a.opt(
                    "-blockdev",
                    format!(
                        "driver={proto},node-name=fw-code,filename={},read-only=on",
                        code.qemu_value()
                    ),
                );
                a.opt(
                    "-blockdev",
                    format!(
                        "driver={proto},node-name=fw-vars,filename={}",
                        vars.qemu_value()
                    ),
                );
            }
            (bios, None) => a.opt("-bios", bios.qemu_value()),
        }
        let mut next_boot = 0u32;
        if let Some(media) = &self.install_media {
            let iso = paths.media(media)?;
            push_cdrom(
                &mut a,
                proto,
                "cd-install",
                &iso,
                profile,
                0,
                Some(next_boot),
            );
            next_boot += 1;
        }
        if let Some(spec) = self.disk {
            let disk = paths
                .disk(run)?
                .ok_or_else(|| BackendError::invalid("disk", "no managed disk for this run"))?;
            match paths.disk_backing(run)? {
                Some(base) => {
                    if spec.format != DiskFormat::Qcow2 {
                        return Err(BackendError::invalid(
                            "disk",
                            "only a qcow2 overlay can have a backing file",
                        ));
                    }
                    // Explicit read-only base node and overlay bound to it
                    // (F3): one level, the header reference is not used.
                    a.opt(
                        "-blockdev",
                        format!(
                            "driver=qcow2,node-name=disk0-base,read-only=on,file.driver={proto},file.filename={},file.read-only=on",
                            base.qemu_value()
                        ),
                    );
                    a.opt(
                        "-blockdev",
                        format!(
                            "driver=qcow2,node-name=disk0,backing=disk0-base,file.driver={proto},file.filename={}",
                            disk.qemu_value()
                        ),
                    );
                }
                None => a.opt(
                    "-blockdev",
                    format!(
                        "driver={},node-name=disk0,file.driver={proto},file.filename={}",
                        spec.format.driver(),
                        disk.qemu_value()
                    ),
                ),
            }
            a.opt("-device", profile.disk_device(next_boot));
        } else if paths.disk(run)?.is_some() {
            return Err(BackendError::invalid(
                "disk",
                "managed disk present but not declared in the specification",
            ));
        }
        if let Some(media) = &self.data_media {
            let iso = paths.media(media)?;
            push_cdrom(&mut a, proto, "cd-data", &iso, profile, 1, None);
        }
        a.opt("-nic", "none");
        a.opt("-device", profile.display().qemu_name());
        match profile.pointer() {
            PointerDevice::Virtio => {
                a.opt("-device", "virtio-tablet-pci");
                a.opt("-device", "virtio-keyboard-pci");
            }
            PointerDevice::UsbXhciTablet | PointerDevice::UsbUhciTablet => {
                let controller = if profile.pointer() == PointerDevice::UsbXhciTablet {
                    "qemu-xhci"
                } else {
                    "piix3-usb-uhci"
                };
                a.opt("-device", format!("{controller},id={USB_CONTROLLER_ID}"));
                a.opt("-device", format!("usb-tablet,bus={USB_CONTROLLER_ID}.0"));
            }
        }
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
    profile: &DeviceProfile,
    slot: u8,
    boot: Option<u32>,
) {
    a.opt(
        "-blockdev",
        format!(
            "driver=raw,node-name={node},read-only=on,file.driver={proto},file.filename={},file.read-only=on",
            iso.qemu_value()
        ),
    );
    let mut dev = format!("ide-cd,drive={node},{}", profile.cdrom_location(slot));
    if let Some(b) = boot {
        dev.push_str(&format!(",bootindex={b}"));
    }
    a.opt("-device", dev);
}

/// Minimal command for the accelerator probe: paused VM with the machine and
/// firmware kind of the profile to verify (OVMF code read-only as `pflash0`
/// without variables, or SeaBIOS with `-bios`), without disks, without
/// display, QMP in the private directory.
pub fn build_probe_command(
    program: ManagedPath,
    machine: &MachineType,
    firmware: &ProbeFirmware,
    accelerator: Accelerator,
    cpu: &CpuModel,
    private_dir: ManagedPath,
    tag: &RunId,
) -> Result<QemuCommand> {
    let qmp_socket = private_dir.join_file("probe-qmp.sock")?;
    qmp_socket.check_socket_len("qmp_socket")?;
    let proto = check_block_protocol("file")?;
    let mut a = Args(Vec::with_capacity(24));
    a.opt("-name", format!("guest=lightvm-probe-{tag}"));
    a.flag("-no-user-config");
    a.flag("-nodefaults");
    match firmware {
        ProbeFirmware::Uefi(code) => {
            a.opt(
                "-machine",
                format!("type={},pflash0=fw-code", machine.as_str()),
            );
            a.opt(
                "-blockdev",
                format!(
                    "driver={proto},node-name=fw-code,filename={},read-only=on",
                    code.qemu_value()
                ),
            );
        }
        ProbeFirmware::SeaBios(bios) => {
            a.opt("-machine", format!("type={}", machine.as_str()));
            a.opt("-bios", bios.qemu_value());
        }
    }
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
        /// Per-run UEFI variables provided (UEFI profiles).
        pub(crate) with_vars: bool,
        /// SeaBIOS image provided (SeaBIOS profiles).
        pub(crate) with_seabios: bool,
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
                with_vars: true,
                with_seabios: true,
                media,
            }
        }

        /// Paths matching the firmware kind of `profile`.
        pub(crate) fn for_profile(profile: &DeviceProfile) -> Self {
            let mut p = Self::new();
            p.with_vars = profile.firmware().has_vars();
            p
        }
    }

    impl ManagedPaths for FakePaths {
        fn qemu_system(&self) -> Result<ManagedPath> {
            ManagedPath::verify(self.root.join("qemu").join("qemu-system-x86_64.exe"))
        }
        fn firmware_code(&self) -> Result<ManagedPath> {
            ManagedPath::verify(self.root.join("fw").join("code.fd"))
        }
        fn firmware_vars(&self, run: &RunId) -> Result<Option<ManagedPath>> {
            if !self.with_vars {
                return Ok(None);
            }
            ManagedPath::verify(self.root.join("runs").join(run.to_string()).join("vars.fd"))
                .map(Some)
        }
        fn seabios(&self) -> Result<ManagedPath> {
            if !self.with_seabios {
                return Err(BackendError::invalid("seabios", "not provided"));
            }
            ManagedPath::verify(self.root.join("qemu").join("share").join("bios-256k.bin"))
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
            profile: DeviceProfile::LINUX,
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

    /// [`spec`] with `profile` and a machine of its family.
    pub(crate) fn spec_for(profile: DeviceProfile) -> VmLaunchSpec {
        let mut s = spec();
        s.machine = match profile.machine_family() {
            MachineFamily::Q35 => MachineType::versioned_q35("pc-q35-11.1").unwrap(),
            MachineFamily::I440fx => MachineType::versioned_i440fx("pc-i440fx-11.1").unwrap(),
        };
        s.profile = profile;
        s
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
                "backing",
            ]),
        );
        t.insert(
            "-device",
            Some(&["drive", "bootindex", "bus", "unit", "id"]),
        );
        // Path value, checked separately.
        t.insert("-bios", None);
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
        "ide-hd",
        "VGA",
        "qemu-xhci",
        "piix3-usb-uhci",
        "usb-tablet",
    ];

    /// The only `:` allowed in a path value is the drive letter.
    fn path_has_separators(v: &str) -> bool {
        let bytes = v.as_bytes();
        let rest = if cfg!(windows) && bytes.len() >= 2 && bytes[1] == b':' {
            &v[2..]
        } else {
            v
        };
        rest.contains([':', '='])
    }

    /// Verifies that the arguments are only option/value pairs from the
    /// table, with only the allowed keys, and that no value contains
    /// extra separators. Returns the description of the violation.
    pub(crate) fn audit_args(args: &[OsString]) -> std::result::Result<(), String> {
        let table = option_table();
        let mut i = 0;
        let mut seen_nodefaults = false;
        let mut seen_nic_none = false;
        let mut seen_bios = false;
        let mut seen_pflash = false;
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
                None if opt == "-bios" => {
                    if val.contains(',') || path_has_separators(val) {
                        return Err(format!("-bios: path with separators {val:?}"));
                    }
                    seen_bios = true;
                }
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
                            path_has_separators(v)
                        } else {
                            v.contains([':', '='])
                        };
                        if extra {
                            return Err(format!("{opt}: separators in the value of {k}: {v:?}"));
                        }
                        seen_pflash |= opt == "-machine" && k.starts_with("pflash");
                    }
                }
            }
            i += 2;
        }
        if !seen_nodefaults || !seen_nic_none {
            return Err("-nodefaults or -nic none missing".into());
        }
        if seen_bios && seen_pflash {
            return Err("-bios together with pflash firmware".into());
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

    /// F3: overlay with an explicit read-only base node (`backing=`).
    #[test]
    fn overlay_with_explicit_backing_node() {
        struct WithBase(FakePaths);
        impl ManagedPaths for WithBase {
            fn qemu_system(&self) -> Result<ManagedPath> {
                self.0.qemu_system()
            }
            fn firmware_code(&self) -> Result<ManagedPath> {
                self.0.firmware_code()
            }
            fn firmware_vars(&self, run: &RunId) -> Result<Option<ManagedPath>> {
                self.0.firmware_vars(run)
            }
            fn disk(&self, run: &RunId) -> Result<Option<ManagedPath>> {
                self.0.disk(run)
            }
            fn protected_readonly(&self, run: &RunId) -> Result<Vec<ManagedPath>> {
                self.0.protected_readonly(run)
            }
            fn media(&self, media: &MediaId) -> Result<ManagedPath> {
                self.0.media(media)
            }
            fn run_dir(&self, run: &RunId) -> Result<ManagedPath> {
                self.0.run_dir(run)
            }
            fn private_dir(&self, run: &RunId) -> Result<ManagedPath> {
                self.0.private_dir(run)
            }
            fn disk_backing(&self, _run: &RunId) -> Result<Option<ManagedPath>> {
                ManagedPath::verify(self.0.root.join("base").join("disk.qcow2")).map(Some)
            }
        }
        let c = spec().build(&WithBase(FakePaths::new())).unwrap();
        audit_args(&c.args).unwrap();
        let s = strs(&c);
        let base = s
            .iter()
            .position(|a| a.contains("node-name=disk0-base"))
            .expect("base node");
        assert!(s[base].contains("read-only=on") && s[base].contains("file.read-only=on"));
        assert!(s[base].starts_with("driver=qcow2,"));
        let ov = s
            .iter()
            .position(|a| a.contains("node-name=disk0,"))
            .expect("overlay node");
        assert!(ov > base, "base declared before the overlay");
        assert!(s[ov].contains("backing=disk0-base"));
        // A raw disk cannot have a backing node.
        let mut raw = spec();
        raw.disk = Some(DiskSpec {
            format: DiskFormat::Raw,
        });
        assert!(raw.build(&WithBase(FakePaths::new())).is_err());
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
        // i440fx is a versioned type of its own family, not a q35 one.
        assert!(MachineType::versioned_q35("pc-i440fx-11.1").is_err());
        assert!(MachineType::versioned_q35("pc-q35-11.1,accel=tcg").is_err());
        assert!(MachineType::versioned_q35("pc-q35-11").is_err());
        assert!(MachineType::versioned_q35("pc-q35-1x.1").is_err());
        let q = MachineType::versioned_q35("pc-q35-10.2").unwrap();
        assert_eq!((q.version(), q.family()), ((10, 2), MachineFamily::Q35));
        assert_eq!(MachineType::versioned("pc-q35-10.2").unwrap(), q);
        let fx = MachineType::versioned("pc-i440fx-11.1").unwrap();
        assert_eq!(
            (fx.as_str(), fx.version(), fx.family()),
            ("pc-i440fx-11.1", (11, 1), MachineFamily::I440fx)
        );
        assert_eq!(MachineType::versioned_i440fx("pc-i440fx-11.1").unwrap(), fx);
        assert!(MachineType::versioned_i440fx("pc-q35-11.1").is_err());
        for bad in [
            "pc",
            "q35",
            "isapc",
            "microvm",
            "none",
            "pc-i440fx",
            "pc-i440fx-",
            "pc-i440fx-11",
            "pc-i440fx-11.",
            "pc-i440fx-.1",
            "pc-i440fx-11.1.0",
            "pc-i440fx-11.1,accel=tcg",
            "pc-i440fx-11.1,pflash0=x",
            "pc-i440fx-1x.1",
            "pc-i440fx-11.1 ",
            "pc-i440fx-+1.1",
            "PC-I440FX-11.1",
            "pc-q35-11.1-pc-i440fx-11.1",
            "xpc-i440fx-11.1",
        ] {
            assert!(MachineType::versioned(bad).is_err(), "{bad}");
            assert!(MachineType::versioned_i440fx(bad).is_err(), "{bad}");
        }
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
        let code = p.firmware_code().unwrap();
        let bios = p.seabios().unwrap();
        for (machine, fw) in [
            ("pc-q35-11.1", ProbeFirmware::Uefi(code.clone())),
            ("pc-q35-11.1", ProbeFirmware::SeaBios(bios.clone())),
            ("pc-i440fx-11.1", ProbeFirmware::SeaBios(bios.clone())),
        ] {
            let cmd = build_probe_command(
                p.qemu_system().unwrap(),
                &MachineType::versioned(machine).unwrap(),
                &fw,
                Accelerator::Whpx,
                &CpuModel::nominal("qemu64").unwrap(),
                p.private_dir(&run).unwrap(),
                &run,
            )
            .unwrap();
            audit_args(&cmd.args).unwrap();
            assert!(cmd.vnc_socket.is_none());
            let s = strs(&cmd);
            let j = s.join(" ");
            assert!(!j.contains("pflash1"), "the probe has no variables");
            match fw {
                ProbeFirmware::Uefi(_) => {
                    assert!(j.contains(&format!("-machine type={machine},pflash0=fw-code")));
                    assert!(j.contains(&format!(
                        "driver=file,node-name=fw-code,filename={},read-only=on",
                        code.qemu_value()
                    )));
                    assert!(!s.contains(&"-bios".to_owned()));
                }
                ProbeFirmware::SeaBios(_) => {
                    assert!(s.contains(&format!("type={machine}")));
                    assert!(j.contains(&format!("-bios {}", bios.qemu_value())));
                    assert!(!j.contains("pflash"));
                }
            }
        }
    }

    fn vec_of(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    /// Exact argument list for each row of the contract table, with
    /// installation ISO, data medium and disk (no backing file).
    #[test]
    fn profile_rows_exact_argv() {
        for profile in DeviceProfile::ALL {
            let mut s = spec_for(profile);
            s.data_media = Some(media_data());
            let p = FakePaths::for_profile(&profile);
            let cmd = s.build(&p).unwrap();
            audit_args(&cmd.args).unwrap();
            let r = s.run_id;
            let iso = p.media(&media_ubuntu()).unwrap();
            let data = p.media(&media_data()).unwrap();
            let disk = p.disk(&r).unwrap().unwrap();
            let pd = p.private_dir(&r).unwrap();
            let machine = s.machine.as_str();
            let mut want: Vec<String> = vec_of(&[
                "-name",
                &format!("guest=lightvm-{r}"),
                "-no-user-config",
                "-nodefaults",
                "-machine",
            ]);
            if profile.firmware() == FirmwareKind::Uefi {
                want.push(format!("type={machine},pflash0=fw-code,pflash1=fw-vars"));
            } else {
                want.push(format!("type={machine}"));
            }
            want.extend(vec_of(&[
                "-accel",
                "whpx",
                "-cpu",
                "qemu64",
                "-smp",
                "cpus=2",
                "-m",
                "size=4096M",
            ]));
            if profile.firmware() == FirmwareKind::Uefi {
                want.extend([
                    "-blockdev".to_owned(),
                    format!(
                        "driver=file,node-name=fw-code,filename={},read-only=on",
                        p.firmware_code().unwrap().qemu_value()
                    ),
                    "-blockdev".to_owned(),
                    format!(
                        "driver=file,node-name=fw-vars,filename={}",
                        p.firmware_vars(&r).unwrap().unwrap().qemu_value()
                    ),
                ]);
            } else {
                want.extend([
                    "-bios".to_owned(),
                    p.seabios().unwrap().qemu_value().to_owned(),
                ]);
            }
            let (cd0, cd1, hd) = match (profile.machine_family(), profile.disk_bus()) {
                (MachineFamily::Q35, DiskBus::Virtio) => (
                    "bus=ide.0",
                    "bus=ide.1",
                    "virtio-blk-pci,drive=disk0,bootindex=1",
                ),
                (MachineFamily::Q35, DiskBus::Ahci) => (
                    "bus=ide.0",
                    "bus=ide.1",
                    "ide-hd,drive=disk0,bus=ide.2,bootindex=1",
                ),
                (MachineFamily::I440fx, DiskBus::PiixIde) => (
                    "bus=ide.1,unit=0",
                    "bus=ide.1,unit=1",
                    "ide-hd,drive=disk0,bus=ide.0,unit=0,bootindex=1",
                ),
                other => panic!("combination outside the contract: {other:?}"),
            };
            want.extend([
                "-blockdev".to_owned(),
                format!(
                    "driver=raw,node-name=cd-install,read-only=on,file.driver=file,file.filename={},file.read-only=on",
                    iso.qemu_value()
                ),
                "-device".to_owned(),
                format!("ide-cd,drive=cd-install,{cd0},bootindex=0"),
                "-blockdev".to_owned(),
                format!(
                    "driver=qcow2,node-name=disk0,file.driver=file,file.filename={}",
                    disk.qemu_value()
                ),
                "-device".to_owned(),
                hd.to_owned(),
                "-blockdev".to_owned(),
                format!(
                    "driver=raw,node-name=cd-data,read-only=on,file.driver=file,file.filename={},file.read-only=on",
                    data.qemu_value()
                ),
                "-device".to_owned(),
                format!("ide-cd,drive=cd-data,{cd1}"),
                "-nic".to_owned(),
                "none".to_owned(),
            ]);
            match profile.pointer() {
                PointerDevice::Virtio => want.extend(vec_of(&[
                    "-device",
                    "virtio-vga",
                    "-device",
                    "virtio-tablet-pci",
                    "-device",
                    "virtio-keyboard-pci",
                ])),
                PointerDevice::UsbXhciTablet => want.extend(vec_of(&[
                    "-device",
                    "VGA",
                    "-device",
                    "qemu-xhci,id=usb0",
                    "-device",
                    "usb-tablet,bus=usb0.0",
                ])),
                PointerDevice::UsbUhciTablet => want.extend(vec_of(&[
                    "-device",
                    "VGA",
                    "-device",
                    "piix3-usb-uhci,id=usb0",
                    "-device",
                    "usb-tablet,bus=usb0.0",
                ])),
            }
            want.extend([
                "-display".to_owned(),
                "none".to_owned(),
                "-vnc".to_owned(),
                format!("unix:{}", pd.join_file("vnc.sock").unwrap().qemu_value()),
                "-qmp".to_owned(),
                format!(
                    "unix:{},server=on,wait=off",
                    pd.join_file("qmp.sock").unwrap().qemu_value()
                ),
                "-S".to_owned(),
            ]);
            assert_eq!(strs(&cmd), want, "{profile:?}");
            // The devices emitted are exactly the profile's required ones.
            let mut emitted: Vec<String> = strs(&cmd)
                .windows(2)
                .filter(|w| w[0] == "-device")
                .map(|w| w[1].split(',').next().unwrap().to_owned())
                .collect();
            emitted.sort();
            emitted.dedup();
            let mut req = vec_of(&profile.required_devices());
            req.sort();
            assert_eq!(emitted, req, "{profile:?}");
        }
    }

    /// Contract table: `(guest, firmware) -> profile`, and the rejected
    /// combinations (`GuestOs::allowed_firmware`).
    #[test]
    fn profile_for_guest_table() {
        use DeviceProfile as P;
        let table = [
            (GuestOs::UbuntuLtsX86_64, Firmware::Uefi, P::LINUX),
            (GuestOs::OtherLinuxX86_64, Firmware::Uefi, P::LINUX),
            (GuestOs::Windows81, Firmware::Uefi, P::WINDOWS_UEFI),
            (GuestOs::Windows10, Firmware::Uefi, P::WINDOWS_UEFI),
            (GuestOs::Windows81, Firmware::Bios, P::WINDOWS_BIOS),
            (GuestOs::Windows10, Firmware::Bios, P::WINDOWS_BIOS),
            (GuestOs::WindowsVista, Firmware::Bios, P::WINDOWS_VISTA_7),
            (GuestOs::Windows7, Firmware::Bios, P::WINDOWS_VISTA_7),
            (GuestOs::WindowsXp, Firmware::Bios, P::WINDOWS_XP),
        ];
        for (os, fw, want) in table {
            assert_eq!(
                DeviceProfile::for_guest(os, fw).unwrap(),
                want,
                "{os:?} {fw:?}"
            );
            assert_eq!(want.firmware(), FirmwareKind::from_proto(fw));
        }
        for os in GuestOs::ALL {
            for fw in [Firmware::Uefi, Firmware::Bios] {
                let r = DeviceProfile::for_guest(os, fw);
                assert_eq!(
                    r.is_ok(),
                    os.allowed_firmware().contains(&fw),
                    "{os:?} {fw:?}"
                );
                if let Err(e) = r {
                    assert!(matches!(e, BackendError::Unsupported { .. }), "{e}");
                }
            }
        }
        for (os, fw) in [
            (GuestOs::UbuntuLtsX86_64, Firmware::Bios),
            (GuestOs::OtherLinuxX86_64, Firmware::Bios),
            (GuestOs::WindowsXp, Firmware::Uefi),
            (GuestOs::WindowsVista, Firmware::Uefi),
            (GuestOs::Windows7, Firmware::Uefi),
        ] {
            assert!(DeviceProfile::for_guest(os, fw).is_err(), "{os:?} {fw:?}");
        }
        // Field values of the table rows.
        assert_eq!(
            (
                P::LINUX.machine_family(),
                P::LINUX.disk_bus(),
                P::LINUX.display(),
                P::LINUX.pointer()
            ),
            (
                MachineFamily::Q35,
                DiskBus::Virtio,
                DisplayDevice::VirtioVga,
                PointerDevice::Virtio
            )
        );
        assert_eq!(
            (
                P::WINDOWS_XP.machine_family(),
                P::WINDOWS_XP.firmware(),
                P::WINDOWS_XP.disk_bus(),
                P::WINDOWS_XP.pointer()
            ),
            (
                MachineFamily::I440fx,
                FirmwareKind::SeaBios,
                DiskBus::PiixIde,
                PointerDevice::UsbUhciTablet
            )
        );
        assert_eq!(P::LINUX.probe_target(), ProbeTarget::REFERENCE);
        assert_eq!(
            P::WINDOWS_XP.probe_target(),
            ProbeTarget {
                family: MachineFamily::I440fx,
                firmware: FirmwareKind::SeaBios
            }
        );
    }

    /// The machine family must match the profile.
    #[test]
    fn machine_family_mismatch_rejected() {
        let fx = MachineType::versioned_i440fx("pc-i440fx-11.1").unwrap();
        let q35 = MachineType::versioned_q35("pc-q35-11.1").unwrap();
        for profile in DeviceProfile::ALL {
            let mut s = spec_for(profile);
            s.machine = match profile.machine_family() {
                MachineFamily::Q35 => fx.clone(),
                MachineFamily::I440fx => q35.clone(),
            };
            let e = s.build(&FakePaths::for_profile(&profile)).unwrap_err();
            assert!(matches!(e, BackendError::InvalidArgument { .. }), "{e}");
        }
    }

    /// UEFI needs the per-run variables; SeaBIOS refuses them and needs
    /// the image.
    #[test]
    fn firmware_paths_per_profile() {
        let mut p = FakePaths::new();
        p.with_vars = false;
        assert!(spec().build(&p).is_err(), "UEFI without variables");
        assert!(spec_for(DeviceProfile::WINDOWS_UEFI).build(&p).is_err());
        let p = FakePaths::new();
        for profile in [
            DeviceProfile::WINDOWS_BIOS,
            DeviceProfile::WINDOWS_VISTA_7,
            DeviceProfile::WINDOWS_XP,
        ] {
            assert!(
                spec_for(profile).build(&p).is_err(),
                "SeaBIOS with variables"
            );
            let mut q = FakePaths::for_profile(&profile);
            q.with_seabios = false;
            assert!(
                spec_for(profile).build(&q).is_err(),
                "SeaBIOS image missing"
            );
            let ok = spec_for(profile)
                .build(&FakePaths::for_profile(&profile))
                .unwrap();
            let j = strs(&ok).join(" ");
            assert!(!j.contains("pflash") && !j.contains("fw-vars") && j.contains("-bios "));
        }
    }

    /// i440fx without install media: the data CD stays on ide.1 unit 1, the
    /// disk boots first.
    #[test]
    fn i440fx_data_only_and_no_disk() {
        let mut s = spec_for(DeviceProfile::WINDOWS_XP);
        s.install_media = None;
        s.data_media = Some(media_data());
        let cmd = s
            .build(&FakePaths::for_profile(&DeviceProfile::WINDOWS_XP))
            .unwrap();
        audit_args(&cmd.args).unwrap();
        let a = strs(&cmd);
        assert!(a.contains(&"ide-cd,drive=cd-data,bus=ide.1,unit=1".to_owned()));
        assert!(a.contains(&"ide-hd,drive=disk0,bus=ide.0,unit=0,bootindex=0".to_owned()));
        let mut s = spec_for(DeviceProfile::WINDOWS_VISTA_7);
        s.disk = None;
        let mut p = FakePaths::for_profile(&DeviceProfile::WINDOWS_VISTA_7);
        p.with_disk = false;
        let cmd = s.build(&p).unwrap();
        audit_args(&cmd.args).unwrap();
        assert!(!strs(&cmd).iter().any(|x| x.starts_with("ide-hd")));
    }

    /// The audit itself rejects what the builder must never emit.
    #[test]
    fn audit_rejects_foreign_devices_and_keys() {
        let base = spec().build(&FakePaths::new()).unwrap().args;
        let with = |extra: &[&str]| {
            let mut v = base.clone();
            v.extend(extra.iter().map(OsString::from));
            audit_args(&v)
        };
        assert!(with(&["-device", "e1000,id=n0"]).is_err());
        assert!(with(&["-device", "usb-tablet,bus=usb0.0,netdev=n"]).is_err());
        assert!(with(&["-device", "ahci,id=a"]).is_err());
        assert!(with(&["-bios", "C:/x,y"]).is_err());
        assert!(with(&["-bios", "/x/y"]).is_err(), "-bios with pflash");
        assert!(with(&["-netdev", "user,id=n"]).is_err());
    }
}

#[cfg(test)]
mod proptests {
    use super::tests::{FakePaths, audit_args, media_data, media_ubuntu, spec, spec_for};
    use super::*;
    use proptest::prelude::*;

    fn profile() -> impl Strategy<Value = DeviceProfile> {
        prop::sample::select(DeviceProfile::ALL.to_vec())
    }

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
                "pc".to_owned(),
                "pc-i440fx-11.1,accel=tcg".to_owned(),
                "pc-i440fx-11.1".to_owned(),
                "usb-tablet,bus=usb0.0".to_owned(),
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
        fn hostile_machine_and_cpu(m in hostile(), c in hostile(), pr in profile()) {
            let mut s = spec_for(pr);
            let mt = MachineType::versioned(&m);
            let family_ok = mt.as_ref().is_ok_and(|t| t.family() == pr.machine_family());
            if let Ok(mt) = mt { s.machine = mt; }
            if let Ok(cm) = CpuModel::nominal(&c) { s.cpu = cm; }
            let r = s.build(&FakePaths::for_profile(&pr));
            check(&r)?;
            if MachineType::versioned(&m).is_ok() && !family_ok {
                prop_assert!(r.is_err(), "machine of another family accepted");
            }
        }

        #[test]
        fn hostile_paths(a in hostile(), b in hostile(), c in hostile(), pr in profile()) {
            let mut p = FakePaths::for_profile(&pr);
            p.media.insert(media_ubuntu(), p.root.join(&a).join(&b));
            let mut s = spec_for(pr);
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
