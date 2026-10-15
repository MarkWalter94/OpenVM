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
//! - **versioned** machine (`pc-q35-X.Y`, `pc-i440fx-X.Y` or `virt-X.Y` with
//!   `gic-version=3`) chosen by the probe, of the family required by the
//!   [`DeviceProfile`];
//! - nominal CPU model (`host` and `max` rejected, F0-02 9.1), except the
//!   AArch64 profile under HVF, which runs only with `-cpu host` (ADR-0028);
//! - firmware per profile: UEFI as pflash (read-only code, per-run
//!   variables; x86 OVMF or the project's AArch64 Secure Boot build) or
//!   SeaBIOS with `-bios` (verified image, no variables);
//! - TPM 2.0 for the profiles that need it (ADR-0028): `tpm-tis-device` with
//!   `ppi=off` on an `emulator` backend whose control socket is in the
//!   private directory; the swtpm process is described by [`TpmCommand`]
//!   and started by [`crate::run`] in the run's container before QEMU;
//! - disks with structured `-blockdev` (`driver`, `node-name`,
//!   `file.driver=file`, `file.filename`), explicit format and only the `file`
//!   protocol;
//! - installation ISO and data medium read-only on `ide-cd`, or on `scsi-cd`
//!   behind one `usb-bot` per medium (AArch64 profile);
//! - VNC display on a Unix socket and QMP on a Unix socket in the private directory;
//! - devices only from the closed set of the hardware profiles (ADR-0027):
//!   `virtio-blk-pci`, `ide-hd` or `nvme`, `virtio-vga`, `VGA` or `ramfb`,
//!   (imported VMware VMs, ADR-0030: `scsi-hd` on `pvscsi` or
//!   `mptsas1068`, `vmware-svga`),
//!   virtio tablet/keyboard or a USB controller (`qemu-xhci`,
//!   `piix3-usb-uhci`) with `usb-tablet` (and `usb-kbd` on AArch64).

use std::ffi::OsString;

use crate::error::{BackendError, Result};
use crate::paths::{ManagedPath, ManagedPaths, names};
use crate::validate::{check_block_protocol, check_token};
use lightvm_proto::{
    DiskController, DisplayAdapter, Firmware, GuestOs, ImportedHardware, MediaId, RunId,
};

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
    /// AArch64 `virt-X.Y` with GICv3 (ADR-0028): no IDE, PCIe root.
    Virt,
}

impl MachineFamily {
    /// Every family, in display order.
    pub const ALL: [MachineFamily; 3] = [
        MachineFamily::Q35,
        MachineFamily::I440fx,
        MachineFamily::Virt,
    ];

    /// Prefix of the versioned names of the family.
    pub fn prefix(self) -> &'static str {
        match self {
            Self::Q35 => "pc-q35-",
            Self::I440fx => "pc-i440fx-",
            Self::Virt => "virt-",
        }
    }

    /// Short name for messages.
    pub fn label(self) -> &'static str {
        match self {
            Self::Q35 => "q35",
            Self::I440fx => "i440fx",
            Self::Virt => "virt",
        }
    }

    /// `true` for the AArch64 family (run by `qemu-system-aarch64`).
    pub fn is_aarch64(self) -> bool {
        matches!(self, Self::Virt)
    }

    /// Fixed machine properties of the family, appended to `type=`
    /// (`virt` needs GICv3 under HVF, ADR-0028).
    pub(crate) fn fixed_properties(self) -> &'static str {
        match self {
            Self::Q35 | Self::I440fx => "",
            Self::Virt => ",gic-version=3",
        }
    }
}

/// Versioned machine type (`pc-q35-X.Y`, `pc-i440fx-X.Y` or `virt-X.Y`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct MachineType {
    name: String,
    family: MachineFamily,
    version: (u32, u32),
}

impl MachineType {
    /// Accepts only `pc-q35-<major>.<minor>`, `pc-i440fx-<major>.<minor>`
    /// and `virt-<major>.<minor>` names; the `q35`, `pc` and `virt` aliases
    /// are rejected because their meaning changes with the QEMU version.
    pub fn versioned(name: &str) -> Result<Self> {
        check_token("machine", name)?;
        let (family, major, minor) = parse_versioned_machine(name).ok_or_else(|| {
            BackendError::invalid(
                "machine",
                format!(
                    "`{name}` is not a versioned machine type (expected pc-q35-X.Y, \
                     pc-i440fx-X.Y or virt-X.Y)"
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

/// Extracts the family and `(major, minor)` from `pc-q35-X.Y`,
/// `pc-i440fx-X.Y` or `virt-X.Y` (decimal digits only, nothing after the
/// minor).
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
    /// Project build of edk2 `ArmVirtQemu` with Secure Boot and TPM 2.0
    /// (ADR-0028) as pflash: read-only code plus per-VM variables created
    /// from the guest's template.
    UefiAarch64,
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
        matches!(self, Self::Uefi | Self::UefiAarch64)
    }

    /// Short name for messages.
    pub fn label(self) -> &'static str {
        match self {
            Self::Uefi => "UEFI",
            Self::SeaBios => "SeaBIOS",
            Self::UefiAarch64 => "UEFI AArch64 Secure Boot",
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
    /// `nvme` with a fixed serial (inbox driver of Windows on ARM, ADR-0028).
    Nvme,
    /// `scsi-hd` on a `pvscsi` controller (VMware paravirtual SCSI;
    /// imported VMware VMs only, ADR-0030).
    Pvscsi,
    /// `scsi-hd` on a `mptsas1068` controller (LSI Logic SAS 1068, VMware
    /// `lsisas1068`; imported VMware VMs only, ADR-0030).
    Mptsas1068,
}

/// Id of the SCSI controller of the [`DiskBus::Pvscsi`] and
/// [`DiskBus::Mptsas1068`] disks.
const SCSI_CONTROLLER_ID: &str = "scsi0";

/// Bus of the installation and data CDs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CdromBus {
    /// `ide-cd` on the board's IDE/AHCI controller.
    Ide,
    /// `scsi-cd` behind one `usb-bot` per medium on the xHCI controller
    /// (a plain `usb-storage` is seen as a RAW disk by Windows setup, R2b-01).
    UsbBot,
}

/// CPU model policy of a profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CpuPolicy {
    /// A nominal model (`host`/`max` refused, F0-02 9.1).
    Nominal,
    /// `host`: the only model HVF accelerates (AArch64 profile, ADR-0028).
    Host,
}

/// Display adapter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DisplayDevice {
    /// `virtio-vga`.
    VirtioVga,
    /// `VGA` (standard VGA with Bochs VBE).
    StdVga,
    /// `ramfb` (Windows on ARM has no inbox virtio-gpu driver, ADR-0028).
    Ramfb,
    /// `vmware-svga` (VMware SVGA II), offered to Windows guests imported
    /// from VMware with the VMware Tools display driver (ADR-0030).
    VmwareSvga,
}

impl DisplayDevice {
    /// QEMU device name.
    pub fn qemu_name(self) -> &'static str {
        match self {
            Self::VirtioVga => "virtio-vga",
            Self::StdVga => "VGA",
            Self::Ramfb => "ramfb",
            Self::VmwareSvga => "vmware-svga",
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
    /// `qemu-xhci` controller + `usb-kbd` + `usb-tablet` on its bus (no
    /// i8042 on `virt`, ADR-0028).
    UsbXhciKeyboardTablet,
}

impl PointerDevice {
    /// USB controller of the USB pointer devices.
    fn usb_controller(self) -> Option<&'static str> {
        match self {
            Self::Virtio => None,
            Self::UsbXhciTablet | Self::UsbXhciKeyboardTablet => Some("qemu-xhci"),
            Self::UsbUhciTablet => Some("piix3-usb-uhci"),
        }
    }
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
    cdrom: CdromBus,
    tpm: bool,
    cpu: CpuPolicy,
}

impl DeviceProfile {
    /// Ubuntu / other Linux: q35, UEFI, virtio disk, display and input.
    pub const LINUX: Self = Self {
        machine_family: MachineFamily::Q35,
        firmware: FirmwareKind::Uefi,
        disk_bus: DiskBus::Virtio,
        display: DisplayDevice::VirtioVga,
        pointer: PointerDevice::Virtio,
        cdrom: CdromBus::Ide,
        tpm: false,
        cpu: CpuPolicy::Nominal,
    };
    /// Windows 8.1 / 10 with UEFI: q35, AHCI, VGA, xHCI tablet.
    pub const WINDOWS_UEFI: Self = Self {
        machine_family: MachineFamily::Q35,
        firmware: FirmwareKind::Uefi,
        disk_bus: DiskBus::Ahci,
        display: DisplayDevice::StdVga,
        pointer: PointerDevice::UsbXhciTablet,
        cdrom: CdromBus::Ide,
        tpm: false,
        cpu: CpuPolicy::Nominal,
    };
    /// Windows 8.1 / 10 with legacy BIOS: q35, SeaBIOS, AHCI, VGA, xHCI tablet.
    pub const WINDOWS_BIOS: Self = Self {
        machine_family: MachineFamily::Q35,
        firmware: FirmwareKind::SeaBios,
        disk_bus: DiskBus::Ahci,
        display: DisplayDevice::StdVga,
        pointer: PointerDevice::UsbXhciTablet,
        cdrom: CdromBus::Ide,
        tpm: false,
        cpu: CpuPolicy::Nominal,
    };
    /// Windows Vista / 7: q35, SeaBIOS, AHCI, VGA, UHCI tablet.
    pub const WINDOWS_VISTA_7: Self = Self {
        machine_family: MachineFamily::Q35,
        firmware: FirmwareKind::SeaBios,
        disk_bus: DiskBus::Ahci,
        display: DisplayDevice::StdVga,
        pointer: PointerDevice::UsbUhciTablet,
        cdrom: CdromBus::Ide,
        tpm: false,
        cpu: CpuPolicy::Nominal,
    };
    /// Windows XP: i440fx, SeaBIOS, PIIX IDE, VGA, UHCI tablet.
    pub const WINDOWS_XP: Self = Self {
        machine_family: MachineFamily::I440fx,
        firmware: FirmwareKind::SeaBios,
        disk_bus: DiskBus::PiixIde,
        display: DisplayDevice::StdVga,
        pointer: PointerDevice::UsbUhciTablet,
        cdrom: CdromBus::Ide,
        tpm: false,
        cpu: CpuPolicy::Nominal,
    };

    /// Windows 11 ARM64 (ADR-0028): `virt` + GICv3, project edk2 with Secure
    /// Boot, NVMe disk, CDs on `usb-bot`, `ramfb`, xHCI keyboard and tablet,
    /// TPM 2.0 (`tpm-tis-device`, swtpm), `-cpu host` under HVF.
    pub const WINDOWS11_ARM64: Self = Self {
        machine_family: MachineFamily::Virt,
        firmware: FirmwareKind::UefiAarch64,
        disk_bus: DiskBus::Nvme,
        display: DisplayDevice::Ramfb,
        pointer: PointerDevice::UsbXhciKeyboardTablet,
        cdrom: CdromBus::UsbBot,
        tpm: true,
        cpu: CpuPolicy::Host,
    };

    /// Every profile, in the order of the contract table.
    pub const ALL: [DeviceProfile; 6] = [
        Self::LINUX,
        Self::WINDOWS_UEFI,
        Self::WINDOWS_BIOS,
        Self::WINDOWS_VISTA_7,
        Self::WINDOWS_XP,
        Self::WINDOWS11_ARM64,
    ];

    /// Reference profile of this host: probed at service start (ADR-0007).
    /// Apple Silicon macOS runs only the AArch64 profile, every other host
    /// the x86 Linux one.
    pub const fn host_reference() -> Self {
        if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            Self::WINDOWS11_ARM64
        } else {
            Self::LINUX
        }
    }

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
            (GuestOs::Windows11Arm64, _) => Self::WINDOWS11_ARM64,
        })
    }

    /// Profile of a VM imported from VMware (ADR-0030): the guest's profile
    /// ([`Self::for_guest`]; for Linux with legacy BIOS the Linux profile on
    /// SeaBIOS) with the disk controller and display adapter of the source
    /// VM. A PIIX IDE disk runs on the i440fx board (the board that has that
    /// controller, as VMware's), an AHCI disk on q35; the other controllers
    /// keep the guest's board. Every value comes from the closed sets of
    /// [`DiskBus`] and [`DisplayDevice`]: nothing of the source VM reaches
    /// the command line as text.
    pub fn for_imported(os: GuestOs, firmware: Firmware, hw: &ImportedHardware) -> Result<Self> {
        let unsupported = |what: String| BackendError::Unsupported {
            what,
            hint: "imported VMs keep the hardware allowed by ADR-0030".into(),
        };
        if os.is_aarch64() {
            return Err(unsupported(format!("imported guest {os:?}")));
        }
        if !os.import_firmware().contains(&firmware) {
            return Err(unsupported(format!(
                "firmware {firmware:?} for the imported guest {os:?}"
            )));
        }
        if !hw.disk_controller.usable_by(os)
            || (hw.display == DisplayAdapter::VmwareSvga && !os.is_windows())
        {
            return Err(unsupported(format!(
                "{:?} / {:?} for the guest {os:?}",
                hw.disk_controller, hw.display
            )));
        }
        let mut p = if os.is_windows() {
            Self::for_guest(os, firmware)?
        } else {
            Self {
                firmware: FirmwareKind::from_proto(firmware),
                ..Self::LINUX
            }
        };
        p.disk_bus = match hw.disk_controller {
            DiskController::Virtio => DiskBus::Virtio,
            DiskController::Ahci => DiskBus::Ahci,
            DiskController::Ide => DiskBus::PiixIde,
            DiskController::Nvme => DiskBus::Nvme,
            DiskController::Pvscsi => DiskBus::Pvscsi,
            DiskController::LsiSas1068 => DiskBus::Mptsas1068,
        };
        match p.disk_bus {
            DiskBus::PiixIde => p.machine_family = MachineFamily::I440fx,
            DiskBus::Ahci => p.machine_family = MachineFamily::Q35,
            _ => {}
        }
        if hw.display == DisplayAdapter::VmwareSvga {
            p.display = DisplayDevice::VmwareSvga;
        }
        Ok(p)
    }

    /// Profile of a VM configuration: [`Self::for_imported`] for a VM
    /// imported from VMware, [`Self::for_guest`] otherwise.
    pub fn for_config(
        os: GuestOs,
        firmware: Firmware,
        imported: Option<&ImportedHardware>,
    ) -> Result<Self> {
        match imported {
            Some(hw) => Self::for_imported(os, firmware, hw),
            None => Self::for_guest(os, firmware),
        }
    }

    /// `true` for the AArch64 profiles (`qemu-system-aarch64`).
    pub fn is_aarch64(&self) -> bool {
        self.machine_family.is_aarch64()
    }

    /// Whether this host can run the profile accelerated (ADR-0007,
    /// ADR-0028): Apple Silicon macOS only the AArch64 profiles, every other
    /// host only the x86 ones.
    pub fn runs_on_this_host(&self) -> bool {
        self.is_aarch64() == cfg!(all(target_os = "macos", target_arch = "aarch64"))
    }

    /// Bus of the CDs.
    pub fn cdrom_bus(&self) -> CdromBus {
        self.cdrom
    }

    /// Whether the profile has a TPM 2.0 (swtpm per VM, ADR-0028).
    pub fn needs_tpm(&self) -> bool {
        self.tpm
    }

    /// CPU model policy.
    pub fn cpu_policy(&self) -> CpuPolicy {
        self.cpu
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
            PointerDevice::UsbXhciKeyboardTablet => {
                v.extend(["qemu-xhci", "usb-kbd", "usb-tablet"]);
            }
        }
        match self.disk_bus {
            DiskBus::Virtio => v.push("virtio-blk-pci"),
            DiskBus::Ahci | DiskBus::PiixIde => v.push("ide-hd"),
            DiskBus::Nvme => v.push("nvme"),
            DiskBus::Pvscsi => v.extend(["pvscsi", "scsi-hd"]),
            DiskBus::Mptsas1068 => v.extend(["mptsas1068", "scsi-hd"]),
        }
        match self.cdrom {
            CdromBus::Ide => v.push("ide-cd"),
            CdromBus::UsbBot => v.extend(["usb-bot", "scsi-cd"]),
        }
        if self.tpm {
            v.push("tpm-tis-device");
        }
        v
    }

    /// `-device` values of the main disk (with its SCSI controller first,
    /// for the SCSI buses).
    fn disk_devices(&self, bootindex: u32) -> Vec<String> {
        let scsi = |controller: &str| {
            vec![
                format!("{controller},id={SCSI_CONTROLLER_ID}"),
                format!("scsi-hd,drive=disk0,bus={SCSI_CONTROLLER_ID}.0,bootindex={bootindex}"),
            ]
        };
        match self.disk_bus {
            DiskBus::Virtio => vec![format!("virtio-blk-pci,drive=disk0,bootindex={bootindex}")],
            DiskBus::Ahci => vec![format!(
                "ide-hd,drive=disk0,bus=ide.2,bootindex={bootindex}"
            )],
            DiskBus::PiixIde => {
                vec![format!(
                    "ide-hd,drive=disk0,bus=ide.0,unit=0,bootindex={bootindex}"
                )]
            }
            DiskBus::Nvme => vec![format!("nvme,drive=disk0,serial=lv0,bootindex={bootindex}")],
            DiskBus::Pvscsi => scsi("pvscsi"),
            DiskBus::Mptsas1068 => scsi("mptsas1068"),
        }
    }

    /// Bus (and unit) of the installation (`slot` 0) or data (`slot` 1)
    /// IDE CD.
    fn cdrom_location(&self, slot: u8) -> String {
        match self.machine_family {
            MachineFamily::I440fx => format!("bus=ide.1,unit={slot}"),
            MachineFamily::Q35 | MachineFamily::Virt => format!("bus=ide.{slot}"),
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
    /// q35 + UEFI: the reference (Linux) configuration of the x86 hosts.
    pub const REFERENCE: Self = Self {
        family: MachineFamily::Q35,
        firmware: FirmwareKind::Uefi,
    };

    /// `virt` + AArch64 UEFI: the reference configuration of Apple Silicon
    /// macOS (ADR-0028).
    pub const REFERENCE_AARCH64: Self = Self {
        family: MachineFamily::Virt,
        firmware: FirmwareKind::UefiAarch64,
    };

    /// Reference configuration of this host, probed at service start:
    /// the probe target of [`DeviceProfile::host_reference`].
    pub const fn host_reference() -> Self {
        if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            Self::REFERENCE_AARCH64
        } else {
            Self::REFERENCE
        }
    }
}

/// Firmware image passed to the accelerator probe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProbeFirmware {
    /// OVMF code, attached read-only as `pflash0` (no variables: the probe
    /// stays paused and never runs the firmware).
    Uefi(ManagedPath),
    /// SeaBIOS image passed with `-bios`.
    SeaBios(ManagedPath),
    /// AArch64 edk2 code (ADR-0028), attached read-only as `pflash0`.
    UefiAarch64(ManagedPath),
}

impl ProbeFirmware {
    /// Firmware kind.
    pub fn kind(&self) -> FirmwareKind {
        match self {
            Self::Uefi(_) => FirmwareKind::Uefi,
            Self::SeaBios(_) => FirmwareKind::SeaBios,
            Self::UefiAarch64(_) => FirmwareKind::UefiAarch64,
        }
    }
}

/// Nominal CPU model.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CpuModel(String);

impl CpuModel {
    /// Model verified up to the live desktop on this host (F0-02 9.2).
    pub const VERIFIED_DEFAULT: &'static str = "qemu64";

    /// `host`, allowed only for a profile whose [`CpuPolicy`] is
    /// [`CpuPolicy::Host`] under HVF (ADR-0028: the only CPU model HVF
    /// accelerates; the exclusion of F0-02 9.1 concerns OVMF under WHPX).
    pub fn host_for(profile: &DeviceProfile, accelerator: Accelerator) -> Result<Self> {
        if profile.cpu_policy() != CpuPolicy::Host || accelerator != Accelerator::Hvf {
            return Err(BackendError::Unsupported {
                what: "CPU model `host`".into(),
                hint: "`-cpu host` is allowed only for the AArch64 profile under HVF \
                       (ADR-0028)"
                    .into(),
            });
        }
        Ok(Self("host".to_owned()))
    }

    /// `true` for `host` (see [`Self::host_for`]).
    pub fn is_host(&self) -> bool {
        self.0 == "host"
    }

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
    /// vCPUs per socket (imported VMs, ADR-0030): `-smp` with an explicit
    /// topology; `None` = `-smp cpus=N` as for every other VM.
    pub cores_per_socket: Option<u32>,
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
    /// swtpm to start in the run's container before QEMU (TPM profiles).
    pub tpm: Option<TpmCommand>,
}

/// swtpm process of a run (ADR-0028): TPM 2.0 with a per-VM state directory
/// and its control socket in the private directory; `--terminate` ends it
/// when QEMU closes the connection.
#[derive(Clone, Debug)]
pub struct TpmCommand {
    /// Approved swtpm executable.
    pub program: ManagedPath,
    /// Structured arguments.
    pub args: Vec<OsString>,
    /// Control socket QEMU connects to.
    pub socket: ManagedPath,
    /// swtpm log (`--log file=`).
    pub log: ManagedPath,
    /// stdout file.
    pub stdout: ManagedPath,
    /// stderr file.
    pub stderr: ManagedPath,
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
        if let Some(c) = self.cores_per_socket
            && (c == 0 || c > self.vcpus || !self.vcpus.is_multiple_of(c))
        {
            return Err(BackendError::ResourceLimit(format!(
                "{c} cores per socket do not divide {} vCPUs",
                self.vcpus
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
        // `host` only where the profile requires it, under HVF; a profile
        // that requires it runs with nothing else (ADR-0028).
        let wants_host = self.profile.cpu_policy() == CpuPolicy::Host;
        let host_ok = wants_host && self.accelerator == Accelerator::Hvf;
        if self.cpu.is_host() != wants_host || (self.cpu.is_host() && !host_ok) {
            return Err(BackendError::invalid(
                "cpu",
                format!(
                    "CPU model `{}` not allowed for the hardware profile with {}",
                    self.cpu.as_str(),
                    self.accelerator.qemu_name()
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
            FirmwareKind::Uefi | FirmwareKind::UefiAarch64 => {
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
        let fixed = self.machine.family().fixed_properties();
        match &firmware {
            (_, Some(_)) => a.opt(
                "-machine",
                format!(
                    "type={}{fixed},pflash0=fw-code,pflash1=fw-vars",
                    self.machine.as_str()
                ),
            ),
            (_, None) => a.opt("-machine", format!("type={}{fixed}", self.machine.as_str())),
        }
        a.opt("-accel", self.accelerator.qemu_name());
        a.opt("-cpu", self.cpu.as_str());
        match self.cores_per_socket {
            Some(c) if c > 1 => a.opt(
                "-smp",
                format!(
                    "cpus={},sockets={},cores={c},threads=1",
                    self.vcpus,
                    self.vcpus / c
                ),
            ),
            _ => a.opt("-smp", format!("cpus={}", self.vcpus)),
        }
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
        // CDs on `usb-bot` need the USB controller before them.
        let usb_first = profile.cdrom_bus() == CdromBus::UsbBot;
        if usb_first {
            push_input(&mut a, profile);
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
            for d in profile.disk_devices(next_boot) {
                a.opt("-device", d);
            }
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
        if !usb_first {
            push_input(&mut a, profile);
        }
        let tpm = if profile.needs_tpm() {
            let program = paths.swtpm()?;
            let state = paths.tpm_state_dir(run)?.ok_or_else(|| {
                BackendError::invalid("tpm_state", "the TPM profile needs the VM's TPM state")
            })?;
            let socket = private_dir.join_file(names::TPM_SOCKET)?;
            socket.check_socket_len("tpm_socket")?;
            let log = run_dir.join_file(names::SWTPM_LOG)?;
            a.opt(
                "-chardev",
                format!("socket,id=chrtpm,path={}", socket.qemu_value()),
            );
            a.opt("-tpmdev", "emulator,id=tpm0,chardev=chrtpm");
            a.opt("-device", "tpm-tis-device,tpmdev=tpm0,ppi=off");
            let args = [
                "socket".to_owned(),
                "--tpm2".to_owned(),
                "--tpmstate".to_owned(),
                format!("dir={}", state.qemu_value()),
                "--ctrl".to_owned(),
                format!("type=unixio,path={}", socket.qemu_value()),
                "--terminate".to_owned(),
                "--log".to_owned(),
                format!("file={}", log.qemu_value()),
            ]
            .into_iter()
            .map(OsString::from)
            .collect();
            Some(TpmCommand {
                program,
                args,
                socket,
                log,
                stdout: run_dir.join_file(names::SWTPM_STDOUT)?,
                stderr: run_dir.join_file(names::SWTPM_STDERR)?,
            })
        } else {
            None
        };
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
            tpm,
        })
    }
}

/// Pointer (and keyboard) devices of `profile`, with their USB controller.
fn push_input(a: &mut Args, profile: &DeviceProfile) {
    let pointer = profile.pointer();
    match pointer.usb_controller() {
        None => {
            a.opt("-device", "virtio-tablet-pci");
            a.opt("-device", "virtio-keyboard-pci");
        }
        Some(controller) => {
            a.opt("-device", format!("{controller},id={USB_CONTROLLER_ID}"));
            if pointer == PointerDevice::UsbXhciKeyboardTablet {
                a.opt("-device", format!("usb-kbd,bus={USB_CONTROLLER_ID}.0"));
            }
            a.opt("-device", format!("usb-tablet,bus={USB_CONTROLLER_ID}.0"));
        }
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
    let mut dev = match profile.cdrom_bus() {
        CdromBus::Ide => format!("ide-cd,drive={node},{}", profile.cdrom_location(slot)),
        CdromBus::UsbBot => {
            a.opt(
                "-device",
                format!("usb-bot,id=bot{slot},bus={USB_CONTROLLER_ID}.0"),
            );
            format!("scsi-cd,drive={node},bus=bot{slot}.0")
        }
    };
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
    let fixed = machine.family().fixed_properties();
    match firmware {
        ProbeFirmware::Uefi(code) | ProbeFirmware::UefiAarch64(code) => {
            a.opt(
                "-machine",
                format!("type={}{fixed},pflash0=fw-code", machine.as_str()),
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
            a.opt("-machine", format!("type={}{fixed}", machine.as_str()));
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
        tpm: None,
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
        fn swtpm(&self) -> Result<ManagedPath> {
            ManagedPath::verify(self.root.join("qemu").join("swtpm"))
        }
        fn tpm_state_dir(&self, _run: &RunId) -> Result<Option<ManagedPath>> {
            ManagedPath::verify(self.root.join("vms").join("vm1").join("tpm")).map(Some)
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
            cores_per_socket: None,
            memory_mib: 4096,
            disk: Some(DiskSpec {
                format: DiskFormat::Qcow2,
            }),
            install_media: Some(media_ubuntu()),
            data_media: None,
            limits: crate::process::JobLimits::default(),
        }
    }

    /// [`spec`] with `profile` and a machine of its family (AArch64:
    /// `virt`, HVF and `-cpu host`).
    pub(crate) fn spec_for(profile: DeviceProfile) -> VmLaunchSpec {
        let mut s = spec();
        s.machine = match profile.machine_family() {
            MachineFamily::Q35 => MachineType::versioned_q35("pc-q35-11.1").unwrap(),
            MachineFamily::I440fx => MachineType::versioned_i440fx("pc-i440fx-11.1").unwrap(),
            MachineFamily::Virt => {
                MachineType::versioned_in(MachineFamily::Virt, "virt-11.1").unwrap()
            }
        };
        if profile.cpu_policy() == CpuPolicy::Host {
            s.accelerator = Accelerator::Hvf;
            s.cpu = CpuModel::host_for(&profile, Accelerator::Hvf).unwrap();
        }
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
        t.insert(
            "-machine",
            Some(&["type", "gic-version", "pflash0", "pflash1"]),
        );
        t.insert("-accel", None);
        t.insert("-cpu", None);
        t.insert("-smp", Some(&["cpus", "sockets", "cores", "threads"]));
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
            Some(&[
                "drive",
                "bootindex",
                "bus",
                "unit",
                "id",
                "serial",
                "tpmdev",
                "ppi",
            ]),
        );
        t.insert("-chardev", Some(&["id", "path"]));
        t.insert("-tpmdev", Some(&["id", "chardev"]));
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
        "nvme",
        "ramfb",
        "usb-kbd",
        "usb-bot",
        "scsi-cd",
        "tpm-tis-device",
        "pvscsi",
        "mptsas1068",
        "scsi-hd",
        "vmware-svga",
    ];
    /// Allowed first part (type) of the options that have one.
    fn typed_option(opt: &str) -> Option<&'static [&'static str]> {
        match opt {
            "-device" => Some(DEVICES),
            "-chardev" => Some(&["socket"]),
            "-tpmdev" => Some(&["emulator"]),
            _ => None,
        }
    }

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
        let mut accel = String::new();
        let mut cpu = String::new();
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
                    if opt == "-accel" {
                        val.clone_into(&mut accel);
                    }
                    if opt == "-cpu" {
                        val.clone_into(&mut cpu);
                    }
                }
                Some(allowed) => {
                    let mut parts = val.split(',');
                    if let Some(types) = typed_option(opt) {
                        let dev = parts.next().unwrap_or_default();
                        if !types.contains(&dev) {
                            return Err(format!("{opt} type not allowed {dev:?}"));
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
                        let colon_ok = k == "filename"
                            || k == "file.filename"
                            || k == "unix"
                            || (opt == "-chardev" && k == "path");
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
        if (cpu == "host" || cpu == "max") && accel != "hvf" {
            return Err(format!("-cpu {cpu} with -accel {accel}"));
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
        let host = CpuModel::host_for(&DeviceProfile::WINDOWS11_ARM64, Accelerator::Hvf).unwrap();
        for (machine, fw) in [
            ("pc-q35-11.1", ProbeFirmware::Uefi(code.clone())),
            ("pc-q35-11.1", ProbeFirmware::SeaBios(bios.clone())),
            ("pc-i440fx-11.1", ProbeFirmware::SeaBios(bios.clone())),
            ("virt-11.1", ProbeFirmware::UefiAarch64(code.clone())),
        ] {
            let aarch64 = machine.starts_with("virt-");
            let cmd = build_probe_command(
                p.qemu_system().unwrap(),
                &MachineType::versioned(machine).unwrap(),
                &fw,
                if aarch64 {
                    Accelerator::Hvf
                } else {
                    Accelerator::Whpx
                },
                &if aarch64 {
                    host.clone()
                } else {
                    CpuModel::nominal("qemu64").unwrap()
                },
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
                ProbeFirmware::UefiAarch64(_) => {
                    assert!(j.contains(&format!(
                        "-machine type={machine},gic-version=3,pflash0=fw-code"
                    )));
                    assert!(j.contains("-accel hvf -cpu host"));
                }
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
        for profile in DeviceProfile::ALL.into_iter().filter(|p| !p.is_aarch64()) {
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
                PointerDevice::UsbXhciKeyboardTablet => panic!("AArch64 profile"),
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

    fn imported(c: DiskController, d: DisplayAdapter) -> ImportedHardware {
        ImportedHardware {
            source: lightvm_proto::ImportSource::Vmware,
            arch: lightvm_proto::GuestArch::X86_64,
            disk_controller: c,
            display: d,
            cores_per_socket: None,
            storage_driver_preparation: false,
        }
    }

    /// ADR-0030: imported VMs keep their disk controller, display and CPU
    /// topology through the closed device set; the arguments of the disk
    /// and display are exact, the rest of the command is the guest profile's.
    #[test]
    fn imported_hardware_argv() {
        use DisplayAdapter::{Standard, VmwareSvga};
        let cases = [
            (
                GuestOs::Windows7,
                Firmware::Bios,
                DiskController::Pvscsi,
                VmwareSvga,
                MachineFamily::Q35,
                vec![
                    "pvscsi,id=scsi0",
                    "scsi-hd,drive=disk0,bus=scsi0.0,bootindex=1",
                ],
                "vmware-svga",
            ),
            (
                GuestOs::Windows10,
                Firmware::Uefi,
                DiskController::LsiSas1068,
                Standard,
                MachineFamily::Q35,
                vec![
                    "mptsas1068,id=scsi0",
                    "scsi-hd,drive=disk0,bus=scsi0.0,bootindex=1",
                ],
                "VGA",
            ),
            (
                GuestOs::WindowsVista,
                Firmware::Bios,
                DiskController::Ide,
                Standard,
                MachineFamily::I440fx,
                vec!["ide-hd,drive=disk0,bus=ide.0,unit=0,bootindex=1"],
                "VGA",
            ),
            (
                GuestOs::WindowsXp,
                Firmware::Bios,
                DiskController::Pvscsi,
                Standard,
                MachineFamily::I440fx,
                vec![
                    "pvscsi,id=scsi0",
                    "scsi-hd,drive=disk0,bus=scsi0.0,bootindex=1",
                ],
                "VGA",
            ),
            (
                GuestOs::Windows10,
                Firmware::Bios,
                DiskController::Nvme,
                Standard,
                MachineFamily::Q35,
                vec!["nvme,drive=disk0,serial=lv0,bootindex=1"],
                "VGA",
            ),
            (
                GuestOs::OtherLinuxX86_64,
                Firmware::Bios,
                DiskController::Ahci,
                Standard,
                MachineFamily::Q35,
                vec!["ide-hd,drive=disk0,bus=ide.2,bootindex=1"],
                "virtio-vga",
            ),
            (
                GuestOs::OtherLinuxX86_64,
                Firmware::Uefi,
                DiskController::Virtio,
                Standard,
                MachineFamily::Q35,
                vec!["virtio-blk-pci,drive=disk0,bootindex=1"],
                "virtio-vga",
            ),
        ];
        for (os, fw, c, d, family, disk_devs, display) in cases {
            let hw = imported(c, d);
            let profile = DeviceProfile::for_imported(os, fw, &hw).unwrap();
            assert_eq!(profile.machine_family(), family, "{os:?} {c:?}");
            assert_eq!(profile.firmware(), FirmwareKind::from_proto(fw));
            assert_eq!(
                DeviceProfile::for_config(os, fw, Some(&hw)).unwrap(),
                profile
            );
            let mut s = spec_for(profile);
            s.vcpus = 4;
            s.cores_per_socket = Some(2);
            let p = FakePaths::for_profile(&profile);
            let cmd = s.build(&p).unwrap();
            audit_args(&cmd.args).unwrap();
            let a = strs(&cmd);
            let devices: Vec<&str> = a
                .windows(2)
                .filter(|w| w[0] == "-device")
                .map(|w| w[1].as_str())
                .collect();
            for want in &disk_devs {
                assert!(devices.contains(want), "{os:?} {c:?}: {devices:?}");
            }
            assert!(devices.contains(&display), "{os:?}: {devices:?}");
            let smp = a.iter().position(|x| x == "-smp").unwrap();
            assert_eq!(a[smp + 1], "cpus=4,sockets=2,cores=2,threads=1");
            // Devices emitted = the profile's required devices.
            let mut emitted: Vec<String> = devices
                .iter()
                .map(|d| d.split(',').next().unwrap().to_owned())
                .collect();
            emitted.sort();
            emitted.dedup();
            let mut req = vec_of(&profile.required_devices());
            req.sort();
            assert_eq!(emitted, req, "{os:?} {c:?}");
        }
        // Refused combinations.
        let bad = [
            (
                GuestOs::Windows10,
                Firmware::Uefi,
                DiskController::Virtio,
                Standard,
            ),
            (
                GuestOs::WindowsXp,
                Firmware::Bios,
                DiskController::Ahci,
                Standard,
            ),
            (
                GuestOs::OtherLinuxX86_64,
                Firmware::Uefi,
                DiskController::Ahci,
                VmwareSvga,
            ),
            (
                GuestOs::Windows7,
                Firmware::Uefi,
                DiskController::Ahci,
                Standard,
            ),
            (
                GuestOs::Windows11Arm64,
                Firmware::Uefi,
                DiskController::Nvme,
                Standard,
            ),
        ];
        for (os, fw, c, d) in bad {
            assert!(
                DeviceProfile::for_imported(os, fw, &imported(c, d)).is_err(),
                "{os:?} {fw:?} {c:?} {d:?}"
            );
        }
        // A topology that does not divide the vCPUs is refused.
        let mut s = spec();
        s.cores_per_socket = Some(3);
        assert!(s.build(&FakePaths::new()).is_err());
        s.cores_per_socket = Some(0);
        assert!(s.build(&FakePaths::new()).is_err());
        // Not imported: the profile of the guest, unchanged.
        assert_eq!(
            DeviceProfile::for_config(GuestOs::Windows7, Firmware::Bios, None).unwrap(),
            DeviceProfile::WINDOWS_VISTA_7
        );
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
            (GuestOs::Windows11Arm64, Firmware::Uefi, P::WINDOWS11_ARM64),
        ];
        for (os, fw, want) in table {
            assert_eq!(
                DeviceProfile::for_guest(os, fw).unwrap(),
                want,
                "{os:?} {fw:?}"
            );
            assert_eq!(want.is_aarch64(), os.is_aarch64(), "{os:?}");
            assert_eq!(want.needs_tpm(), os.needs_tpm(), "{os:?}");
            assert_eq!(want.runs_on_this_host(), os.runs_on_this_host(), "{os:?}");
            if !want.is_aarch64() {
                assert_eq!(want.firmware(), FirmwareKind::from_proto(fw));
            }
        }
        assert_eq!(P::WINDOWS11_ARM64.firmware(), FirmwareKind::UefiAarch64);
        assert_eq!(
            P::WINDOWS11_ARM64.probe_target(),
            ProbeTarget::REFERENCE_AARCH64
        );
        assert_eq!(
            P::host_reference().probe_target(),
            ProbeTarget::host_reference()
        );
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
            (GuestOs::Windows11Arm64, Firmware::Bios),
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
                MachineFamily::Q35 | MachineFamily::Virt => fx.clone(),
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

    /// ADR-0028 row: exact argument list of the Windows 11 ARM64 profile
    /// (HVF, `virt` + GICv3, `-cpu host`, edk2 pflash, CDs on `usb-bot`
    /// after the xHCI controller, NVMe, `ramfb`, TPM on the private socket),
    /// and the swtpm command.
    #[test]
    fn windows11_arm64_exact_argv() {
        let profile = DeviceProfile::WINDOWS11_ARM64;
        let mut s = spec_for(profile);
        s.data_media = Some(media_data());
        let p = FakePaths::for_profile(&profile);
        let cmd = s.build(&p).unwrap();
        audit_args(&cmd.args).unwrap();
        let r = s.run_id;
        let pd = p.private_dir(&r).unwrap();
        let rd = p.run_dir(&r).unwrap();
        let iso = p.media(&media_ubuntu()).unwrap();
        let data = p.media(&media_data()).unwrap();
        let disk = p.disk(&r).unwrap().unwrap();
        let tpm_sock = pd.join_file("tpm.sock").unwrap();
        let want: Vec<String> = vec![
            "-name".into(),
            format!("guest=lightvm-{r}"),
            "-no-user-config".into(),
            "-nodefaults".into(),
            "-machine".into(),
            "type=virt-11.1,gic-version=3,pflash0=fw-code,pflash1=fw-vars".into(),
            "-accel".into(),
            "hvf".into(),
            "-cpu".into(),
            "host".into(),
            "-smp".into(),
            "cpus=2".into(),
            "-m".into(),
            "size=4096M".into(),
            "-blockdev".into(),
            format!(
                "driver=file,node-name=fw-code,filename={},read-only=on",
                p.firmware_code().unwrap().qemu_value()
            ),
            "-blockdev".into(),
            format!(
                "driver=file,node-name=fw-vars,filename={}",
                p.firmware_vars(&r).unwrap().unwrap().qemu_value()
            ),
            "-device".into(),
            "qemu-xhci,id=usb0".into(),
            "-device".into(),
            "usb-kbd,bus=usb0.0".into(),
            "-device".into(),
            "usb-tablet,bus=usb0.0".into(),
            "-blockdev".into(),
            format!(
                "driver=raw,node-name=cd-install,read-only=on,file.driver=file,file.filename={},file.read-only=on",
                iso.qemu_value()
            ),
            "-device".into(),
            "usb-bot,id=bot0,bus=usb0.0".into(),
            "-device".into(),
            "scsi-cd,drive=cd-install,bus=bot0.0,bootindex=0".into(),
            "-blockdev".into(),
            format!(
                "driver=qcow2,node-name=disk0,file.driver=file,file.filename={}",
                disk.qemu_value()
            ),
            "-device".into(),
            "nvme,drive=disk0,serial=lv0,bootindex=1".into(),
            "-blockdev".into(),
            format!(
                "driver=raw,node-name=cd-data,read-only=on,file.driver=file,file.filename={},file.read-only=on",
                data.qemu_value()
            ),
            "-device".into(),
            "usb-bot,id=bot1,bus=usb0.0".into(),
            "-device".into(),
            "scsi-cd,drive=cd-data,bus=bot1.0".into(),
            "-nic".into(),
            "none".into(),
            "-device".into(),
            "ramfb".into(),
            "-chardev".into(),
            format!("socket,id=chrtpm,path={}", tpm_sock.qemu_value()),
            "-tpmdev".into(),
            "emulator,id=tpm0,chardev=chrtpm".into(),
            "-device".into(),
            "tpm-tis-device,tpmdev=tpm0,ppi=off".into(),
            "-display".into(),
            "none".into(),
            "-vnc".into(),
            format!("unix:{}", pd.join_file("vnc.sock").unwrap().qemu_value()),
            "-qmp".into(),
            format!(
                "unix:{},server=on,wait=off",
                pd.join_file("qmp.sock").unwrap().qemu_value()
            ),
            "-S".into(),
        ];
        assert_eq!(strs(&cmd), want);
        let mut emitted: Vec<String> = strs(&cmd)
            .windows(2)
            .filter(|w| w[0] == "-device")
            .map(|w| w[1].split(',').next().unwrap().to_owned())
            .collect();
        emitted.sort();
        emitted.dedup();
        let mut req = vec_of(&profile.required_devices());
        req.sort();
        assert_eq!(emitted, req);
        // swtpm: per-VM state, control socket in the private directory,
        // `--terminate`, log in the run directory.
        let tpm = cmd.tpm.expect("swtpm command");
        assert_eq!(tpm.program, p.swtpm().unwrap());
        assert_eq!(tpm.socket, tpm_sock);
        let targs: Vec<String> = tpm
            .args
            .iter()
            .map(|a| a.to_str().unwrap().to_owned())
            .collect();
        assert_eq!(
            targs,
            vec![
                "socket".to_owned(),
                "--tpm2".into(),
                "--tpmstate".into(),
                format!("dir={}", p.tpm_state_dir(&r).unwrap().unwrap().qemu_value()),
                "--ctrl".into(),
                format!("type=unixio,path={}", tpm_sock.qemu_value()),
                "--terminate".into(),
                "--log".into(),
                format!("file={}", rd.join_file("swtpm.log").unwrap().qemu_value()),
            ]
        );
        // The x86 profiles have no TPM.
        assert!(spec().build(&FakePaths::new()).unwrap().tpm.is_none());
    }

    /// `-cpu host` only for the AArch64 profile under HVF; that profile
    /// runs with nothing else (ADR-0028).
    #[test]
    fn host_cpu_only_for_aarch64_under_hvf() {
        let a64 = DeviceProfile::WINDOWS11_ARM64;
        assert!(CpuModel::host_for(&a64, Accelerator::Hvf).is_ok());
        assert!(CpuModel::host_for(&a64, Accelerator::Kvm).is_err());
        assert!(CpuModel::host_for(&a64, Accelerator::Whpx).is_err());
        for p in DeviceProfile::ALL.iter().filter(|p| !p.is_aarch64()) {
            assert!(CpuModel::host_for(p, Accelerator::Hvf).is_err(), "{p:?}");
        }
        assert!(CpuModel::nominal("host").is_err());
        let paths = FakePaths::for_profile(&a64);
        // Nominal model on the AArch64 profile: refused.
        let mut s = spec_for(a64);
        s.cpu = CpuModel::nominal("cortex-a76").unwrap();
        assert!(s.build(&paths).is_err());
        // `host` under another accelerator: refused.
        let mut s = spec_for(a64);
        s.accelerator = Accelerator::Kvm;
        assert!(s.build(&paths).is_err());
        // `host` on an x86 profile: refused even under HVF.
        let mut s = spec();
        s.accelerator = Accelerator::Hvf;
        s.cpu = CpuModel::host_for(&a64, Accelerator::Hvf).unwrap();
        assert!(s.build(&FakePaths::new()).is_err());
        // The audit refuses `-cpu host` without HVF.
        let mut args = spec_for(a64).build(&paths).unwrap().args;
        let i = args.iter().position(|a| a == "hvf").unwrap();
        args[i] = OsString::from("kvm");
        assert!(audit_args(&args).is_err());
    }

    /// The TPM profile needs the VM's TPM state and swtpm.
    #[test]
    fn tpm_profile_needs_state_and_swtpm() {
        struct NoTpm(FakePaths);
        impl ManagedPaths for NoTpm {
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
        }
        let a64 = DeviceProfile::WINDOWS11_ARM64;
        assert!(
            spec_for(a64)
                .build(&NoTpm(FakePaths::for_profile(&a64)))
                .is_err()
        );
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
        assert!(with(&["-chardev", "pty,id=c"]).is_err());
        assert!(with(&["-chardev", "socket,id=c,host=0.0.0.0"]).is_err());
        assert!(with(&["-tpmdev", "passthrough,id=t"]).is_err());
        assert!(with(&["-device", "virtio-net-pci,id=n"]).is_err());
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
