//! Mapping of a VMware VM to a LightVM configuration (ADR-0030 table).
//!
//! | VMware | LightVM |
//! | --- | --- |
//! | `guestOS` | guest system (ADR-0027); unknown → refused |
//! | `numvcpus`, `cpuid.coresPerSocket`, `memsize` | vCPUs, topology, RAM within the host limits |
//! | `firmware = "efi"` / absent | UEFI (new variable store) / SeaBIOS |
//! | `pvscsi`, `lsisas1068`, SATA, IDE, NVMe | `pvscsi`, `mptsas1068`, AHCI, PIIX IDE, NVMe |
//! | `lsilogic`, `buslogic` (not emulated by QEMU) | the guest profile's controller; Windows: warning and storage driver preparation |
//! | NVMe with legacy BIOS (SeaBIOS does not boot from NVMe, R6-01b) | the guest profile's controller; Windows: warning and storage driver preparation |
//!
//! The network, sound, USB, shared folders, 3D, serial and parallel ports,
//! floppy, CD drives, other disks and intermediate snapshots are listed as
//! not imported.

use lightvm_proto::ipc::{
    ImportWarning, ImportWarningKind, NotImported, NotImportedItem, VmwareController,
    VmwareDiskPreview, VmwareImportPreview,
};
use lightvm_proto::{
    DiskController, DisplayAdapter, Firmware, GuestArch, GuestOs, ImportSource, ImportedHardware,
    NetworkProfile, VmConfig, VmLimits, VmName,
};

use crate::qemu_info::ChainFacts;
use crate::{ImportError, Inspection};

/// Guest system read from the VMware guest type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmwareGuest {
    /// `guestOS` as written (shown, never interpreted further).
    pub raw: String,
    /// LightVM guest system.
    pub os: GuestOs,
    /// Architecture.
    pub arch: GuestArch,
    /// VMware uses `windows8` for both 8 and 8.1.
    pub windows8_or_81: bool,
}

/// Distributions VMware names in 64-bit Linux guest types.
const LINUX_STEMS: &[&str] = &[
    "ubuntu",
    "debian",
    "centos",
    "rhel",
    "fedora",
    "opensuse",
    "sles",
    "suse",
    "oraclelinux",
    "almalinux",
    "rockylinux",
    "mandriva",
    "mandrake",
    "asianux",
    "turbolinux",
    "nld",
    "amazonlinux",
    "coreos",
    "vmware-photon",
    "otherlinux",
    "other24xlinux",
    "other26xlinux",
    "other3xlinux",
    "other4xlinux",
    "other5xlinux",
    "other6xlinux",
    "crxpod",
    "crxsys",
];

/// LightVM guest of a VMware `guestOS` value.
pub fn guest_from_vmware(raw: &str) -> Result<VmwareGuest, ImportError> {
    let shown = crate::shown(raw);
    let g = raw.trim().to_ascii_lowercase();
    let (stem, x64) = match g.strip_suffix("-64") {
        Some(s) => (s, true),
        None => (g.as_str(), false),
    };
    let arch = if x64 {
        GuestArch::X86_64
    } else {
        GuestArch::X86
    };
    let win = |os| {
        Ok(VmwareGuest {
            raw: shown.clone(),
            os,
            arch,
            windows8_or_81: stem == "windows8",
        })
    };
    match stem {
        "winxphome" | "winxppro" => return win(GuestOs::WindowsXp),
        "winvista" => return win(GuestOs::WindowsVista),
        "windows7" => return win(GuestOs::Windows7),
        "windows8" => return win(GuestOs::Windows81),
        "windows9" | "windows10" => return win(GuestOs::Windows10),
        "windows11" | "windows12" => {
            return Err(ImportError::UnsupportedGuest(
                shown,
                "Windows 11 needs TPM 2.0 and Secure Boot, which x86 LightVM VMs do not have (ADR-0027)",
            ));
        }
        _ => {}
    }
    let linux = LINUX_STEMS.iter().any(|s| stem.starts_with(s));
    if linux && x64 {
        return Ok(VmwareGuest {
            raw: shown,
            os: GuestOs::OtherLinuxX86_64,
            arch: GuestArch::X86_64,
            windows8_or_81: false,
        });
    }
    if linux {
        return Err(ImportError::UnsupportedGuest(
            shown,
            "only 64-bit Linux guests are supported",
        ));
    }
    Err(ImportError::UnknownGuest(shown))
}

/// Firmware the imported guest may keep (see
/// [`GuestOs::import_firmware`]); 32-bit guests boot only with BIOS (the
/// OVMF build is x86_64 only).
pub fn check_firmware(g: &VmwareGuest, fw: Firmware) -> Result<(), ImportError> {
    if fw == Firmware::Uefi && g.arch == GuestArch::X86 {
        return Err(ImportError::UnsupportedFirmware(
            "32-bit guests with EFI firmware cannot be imported: the UEFI firmware of LightVM is 64-bit only",
        ));
    }
    if !g.os.import_firmware().contains(&fw) {
        return Err(ImportError::UnsupportedFirmware(
            "Windows XP, Vista and 7 guests with EFI firmware cannot be imported: they need legacy BIOS in LightVM (ADR-0027)",
        ));
    }
    Ok(())
}

/// Limits the plan must fit (from the core's effective limits).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostLimits {
    /// Most vCPUs.
    pub max_vcpus: u32,
    /// Most guest memory, MiB.
    pub max_memory_mib: u64,
    /// Largest virtual disk, GiB.
    pub max_disk_gib: u64,
}

/// Configuration and lists for the preview and the import.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// Configuration to create (with the given name).
    pub config: VmConfig,
    /// What is not imported.
    pub not_imported: Vec<NotImported>,
    /// Warnings.
    pub warnings: Vec<ImportWarning>,
    /// VMware Tools hint from the `.vmx`.
    pub tools_hint: bool,
}

/// Default memory of a VMware VM without `memsize` (MiB).
const DEFAULT_MEMSIZE: u64 = 256;
const GIB: u64 = 1 << 30;

/// LightVM controller for a VMware controller, guest and firmware:
/// `(controller, replaced)`, `replaced` when QEMU does not emulate the
/// VMware one (or the guest has no driver for the equivalent, or the firmware
/// cannot boot it) and the profile's is used. SeaBIOS does not read NVMe
/// boot disks (R6-01b run 3: "could not read the boot disk"), so an NVMe
/// disk of a legacy-BIOS guest moves to the profile's controller; UEFI
/// (OVMF) boots NVMe and keeps it.
#[must_use]
pub fn controller_for(c: VmwareController, os: GuestOs, fw: Firmware) -> (DiskController, bool) {
    let mapped = match c {
        VmwareController::Ide => Some(DiskController::Ide),
        VmwareController::Sata => Some(DiskController::Ahci),
        VmwareController::Nvme if fw == Firmware::Uefi => Some(DiskController::Nvme),
        VmwareController::Nvme => None,
        VmwareController::Pvscsi => Some(DiskController::Pvscsi),
        VmwareController::LsiSas1068 => Some(DiskController::LsiSas1068),
        VmwareController::LsiLogic | VmwareController::BusLogic => None,
    };
    match mapped {
        Some(m) if m.usable_by(os) => (m, false),
        _ => {
            let profile = if !os.is_windows() {
                DiskController::Virtio
            } else if os == GuestOs::WindowsXp {
                DiskController::Ide
            } else {
                DiskController::Ahci
            };
            (profile, true)
        }
    }
}

/// Builds the plan for `insp` with the chain facts read by `qemu-img`.
pub fn plan(
    insp: &Inspection,
    chain: &ChainFacts,
    limits: &HostLimits,
    name: VmName,
) -> Result<Plan, ImportError> {
    let g = &insp.guest;
    let vmx = &insp.vmx;
    let mut warnings = Vec::new();
    let warn = |w: &mut Vec<ImportWarning>, kind, detail: Option<String>| {
        w.push(ImportWarning { kind, detail });
    };
    if g.windows8_or_81 {
        warn(&mut warnings, ImportWarningKind::Windows8Or81, None);
    }
    // vCPUs and topology.
    let wanted = u32::try_from(vmx.number("numvcpus").unwrap_or(1).max(1)).unwrap_or(u32::MAX);
    let max_vcpus = limits.max_vcpus.max(1);
    let vcpus = wanted.min(max_vcpus);
    if vcpus < wanted {
        warn(
            &mut warnings,
            ImportWarningKind::VcpusLowered,
            Some(format!("{wanted} → {vcpus}")),
        );
    }
    let cores = vmx
        .number("cpuid.corespersocket")
        .and_then(|c| u32::try_from(c).ok())
        .unwrap_or(1);
    let cores_per_socket = if cores <= 1 {
        None
    } else if cores <= vcpus && vcpus % cores == 0 && cores <= lightvm_proto::MAX_CORES_PER_SOCKET {
        Some(cores)
    } else {
        warn(
            &mut warnings,
            ImportWarningKind::TopologyChanged,
            Some(format!("{cores} cores per socket")),
        );
        None
    };
    // Memory.
    let wanted_mem = vmx.number("memsize").unwrap_or(DEFAULT_MEMSIZE);
    let min = g.os.min_memory_mib();
    let max =
        g.os.max_memory_mib(Some(g.arch))
            .map_or(limits.max_memory_mib, |m| m.min(limits.max_memory_mib));
    let memory_mib = if wanted_mem < min {
        warn(
            &mut warnings,
            ImportWarningKind::MemoryRaised,
            Some(format!("{wanted_mem} MiB → {min} MiB")),
        );
        min
    } else if wanted_mem > max {
        warn(
            &mut warnings,
            ImportWarningKind::MemoryLowered,
            Some(format!("{wanted_mem} MiB → {max} MiB")),
        );
        max
    } else {
        wanted_mem
    };
    // Disk.
    let disk_gib = chain.virtual_size.div_ceil(GIB).max(1);
    if disk_gib > limits.max_disk_gib {
        return Err(ImportError::DiskTooLarge {
            gib: disk_gib,
            max_gib: limits.max_disk_gib,
        });
    }
    let (disk_controller, replaced) = controller_for(insp.disk.controller, g.os, insp.firmware);
    let storage_driver_preparation = replaced && g.os.is_windows();
    if storage_driver_preparation {
        warn(
            &mut warnings,
            ImportWarningKind::StorageDriverPreparation,
            Some(format!(
                "{:?} → {:?}",
                insp.disk.controller, disk_controller
            )),
        );
    } else if replaced {
        warn(
            &mut warnings,
            ImportWarningKind::ControllerReplaced,
            Some(format!(
                "{:?} → {:?}",
                insp.disk.controller, disk_controller
            )),
        );
    }
    if insp.firmware == Firmware::Uefi {
        warn(&mut warnings, ImportWarningKind::NewUefiVariables, None);
    }
    if chain.dirty {
        warn(&mut warnings, ImportWarningKind::DirtyDisk, None);
    }
    let config = VmConfig {
        name,
        guest_os: g.os,
        vcpus,
        memory_mib,
        disk_gib,
        network: NetworkProfile::Offline,
        install_media: None,
        data_media: None,
        firmware: insp.firmware,
        limits: VmLimits::default(),
        imported: Some(ImportedHardware {
            source: ImportSource::Vmware,
            arch: g.arch,
            disk_controller,
            display: DisplayAdapter::Standard,
            cores_per_socket,
            storage_driver_preparation,
        }),
    };
    // Not imported.
    let mut not_imported = Vec::new();
    let mut ni = |item, detail: Option<String>| not_imported.push(NotImported { item, detail });
    let count = |n: usize| (n > 1).then(|| n.to_string());
    let nets = vmx.present_indices("ethernet");
    if nets > 0 {
        ni(NotImportedItem::Network, count(nets));
    }
    let shared = vmx.present_indices("sharedfolder");
    if shared > 0 || vmx.number("sharedfolder.maxnum").is_some_and(|n| n > 0) {
        ni(NotImportedItem::SharedFolders, count(shared));
    }
    if ["usb.present", "ehci.present", "usb_xhci.present"]
        .iter()
        .any(|k| vmx.is_true(k))
    {
        ni(NotImportedItem::Usb, None);
    }
    if vmx.is_true("sound.present") {
        ni(NotImportedItem::Sound, None);
    }
    if vmx.is_true("mks.enable3d") {
        ni(NotImportedItem::Accelerated3d, None);
    }
    let levels = insp.disk.snapshot_levels();
    if levels > 0 {
        ni(NotImportedItem::Snapshots, Some(levels.to_string()));
    }
    for d in &insp.other_disks {
        ni(NotImportedItem::OtherDisk, Some(d.clone()));
    }
    for c in &insp.cdroms {
        ni(NotImportedItem::CdRom, Some(c.clone()));
    }
    if vmx.is_true("floppy0.present") {
        ni(NotImportedItem::Floppy, None);
    }
    let serial = vmx.present_indices("serial");
    if serial > 0 {
        ni(NotImportedItem::SerialPort, count(serial));
    }
    let parallel = vmx.present_indices("parallel");
    if parallel > 0 {
        ni(NotImportedItem::ParallelPort, count(parallel));
    }
    if vmx.is_true("printer.present") {
        ni(NotImportedItem::Printer, None);
    }
    let tools_hint = vmx.tools_install_keys
        || vmx
            .get("tools.remindinstall")
            .is_some_and(|v| v.eq_ignore_ascii_case("false"));
    if tools_hint {
        ni(NotImportedItem::VmwareToolsFeatures, None);
    }
    Ok(Plan {
        config,
        not_imported,
        warnings,
        tools_hint,
    })
}

/// Name for the new VM from the VMware display name: allowed characters
/// kept (others become `-`), trimmed to 64 characters; `fallback` when
/// nothing usable is left.
#[must_use]
pub fn suggested_name(display: Option<&str>, fallback: &str) -> VmName {
    let clean = |s: &str| -> String {
        let mapped: String = s
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.') {
                    c
                } else {
                    '-'
                }
            })
            .take(64)
            .collect();
        mapped
            .trim_start_matches([' ', '.', '-'])
            .trim_end_matches([' ', '.'])
            .to_owned()
    };
    for candidate in [display.map(clean), Some(clean(fallback))]
        .into_iter()
        .flatten()
    {
        if let Ok(n) = VmName::parse(candidate) {
            return n;
        }
    }
    VmName::parse("Imported VM").unwrap_or_else(|_| unreachable!("valid literal"))
}

/// Preview for the UI.
#[must_use]
pub fn preview(
    insp: &Inspection,
    chain: &ChainFacts,
    plan: &Plan,
    required_bytes: u64,
    free_bytes: Option<u64>,
) -> VmwareImportPreview {
    VmwareImportPreview {
        vmx_file: insp.vmx_file.clone(),
        display_name: insp.vmx.get("displayname").map(crate::shown),
        vmware_guest_os: insp.guest.raw.clone(),
        config: plan.config.clone(),
        disk: VmwareDiskPreview {
            file_name: crate::file_name(&insp.disk.top),
            slot: insp.disk.device.slot(),
            vmware_controller: insp.disk.controller,
            create_type: crate::shown(insp.disk.create_type()),
            virtual_size_bytes: chain.virtual_size,
            allocated_bytes: chain.allocated_bytes,
            snapshot_levels: insp.disk.snapshot_levels(),
        },
        required_bytes,
        free_bytes,
        not_imported: plan.not_imported.clone(),
        warnings: plan.warnings.clone(),
        vmware_svga_offered: insp.guest.os.is_windows(),
        vmware_tools_hint: plan.tools_hint,
    }
}

/// Bytes to reserve on the catalog volume for the converted disk: the data
/// of the chain (at most the virtual size) plus QCOW2 metadata.
#[must_use]
pub fn required_bytes(chain: &ChainFacts) -> u64 {
    let data = chain.allocated_bytes.min(chain.virtual_size);
    // One L2 table entry per 64 KiB cluster plus headers: under 0.2 %.
    data.saturating_add(chain.virtual_size / 512)
        .saturating_add(64 << 20)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guest_types() {
        let g = |s| guest_from_vmware(s).map(|g| (g.os, g.arch));
        assert_eq!(
            g("winvista").unwrap(),
            (GuestOs::WindowsVista, GuestArch::X86)
        );
        assert_eq!(
            g("winVista-64").unwrap(),
            (GuestOs::WindowsVista, GuestArch::X86_64)
        );
        assert_eq!(g("winXPPro").unwrap(), (GuestOs::WindowsXp, GuestArch::X86));
        assert_eq!(
            g("windows7-64").unwrap(),
            (GuestOs::Windows7, GuestArch::X86_64)
        );
        assert_eq!(g("windows8-64").unwrap().0, GuestOs::Windows81);
        assert!(guest_from_vmware("windows8").unwrap().windows8_or_81);
        assert_eq!(g("windows9-64").unwrap().0, GuestOs::Windows10);
        assert_eq!(
            g("ubuntu-64").unwrap(),
            (GuestOs::OtherLinuxX86_64, GuestArch::X86_64)
        );
        assert_eq!(g("other5xlinux-64").unwrap().0, GuestOs::OtherLinuxX86_64);
        for bad in [
            "ubuntu",
            "windows11-64",
            "darwin-64",
            "freebsd-64",
            "",
            "winNetStandard",
        ] {
            assert!(guest_from_vmware(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn firmware_rules() {
        let g = |s| guest_from_vmware(s).unwrap();
        assert!(check_firmware(&g("windows7-64"), Firmware::Uefi).is_err());
        assert!(check_firmware(&g("windows7-64"), Firmware::Bios).is_ok());
        assert!(check_firmware(&g("windows9-64"), Firmware::Uefi).is_ok());
        assert!(check_firmware(&g("windows9"), Firmware::Uefi).is_err());
        assert!(check_firmware(&g("ubuntu-64"), Firmware::Bios).is_ok());
        assert!(check_firmware(&g("ubuntu-64"), Firmware::Uefi).is_ok());
    }

    #[test]
    fn controllers() {
        use VmwareController as V;
        let bios = Firmware::Bios;
        let uefi = Firmware::Uefi;
        assert_eq!(
            controller_for(V::Pvscsi, GuestOs::Windows7, uefi),
            (DiskController::Pvscsi, false)
        );
        assert_eq!(
            controller_for(V::LsiLogic, GuestOs::WindowsVista, bios),
            (DiskController::Ahci, true)
        );
        assert_eq!(
            controller_for(V::LsiLogic, GuestOs::WindowsXp, bios),
            (DiskController::Ide, true)
        );
        assert_eq!(
            controller_for(V::BusLogic, GuestOs::OtherLinuxX86_64, uefi),
            (DiskController::Virtio, true)
        );
        assert_eq!(
            controller_for(V::Sata, GuestOs::WindowsXp, bios),
            (DiskController::Ide, true)
        );
        assert_eq!(
            controller_for(V::Ide, GuestOs::Windows7, uefi),
            (DiskController::Ide, false)
        );
        assert_eq!(
            controller_for(V::LsiSas1068, GuestOs::Windows10, uefi),
            (DiskController::LsiSas1068, false)
        );
    }

    /// NVMe is kept only with UEFI: SeaBIOS cannot read an NVMe boot disk
    /// (R6-01b run 3, frame evidence `VMCR2011-last.png`), so a legacy-BIOS
    /// NVMe guest moves to the profile's controller (which SeaBIOS does
    /// boot: IDE and AHCI natively, virtio since the Debian run of R6-01b).
    #[test]
    fn nvme_needs_uefi() {
        use VmwareController as V;
        assert_eq!(
            controller_for(V::Nvme, GuestOs::Windows10, Firmware::Uefi),
            (DiskController::Nvme, false)
        );
        assert_eq!(
            controller_for(V::Nvme, GuestOs::Windows10, Firmware::Bios),
            (DiskController::Ahci, true)
        );
        assert_eq!(
            controller_for(V::Nvme, GuestOs::Windows7, Firmware::Bios),
            (DiskController::Ahci, true)
        );
        assert_eq!(
            controller_for(V::Nvme, GuestOs::WindowsVista, Firmware::Bios),
            (DiskController::Ahci, true)
        );
        assert_eq!(
            controller_for(V::Nvme, GuestOs::WindowsXp, Firmware::Bios),
            (DiskController::Ide, true)
        );
        assert_eq!(
            controller_for(V::Nvme, GuestOs::OtherLinuxX86_64, Firmware::Bios),
            (DiskController::Virtio, true)
        );
    }

    #[test]
    fn names() {
        assert_eq!(
            suggested_name(Some("Windows Vista-VS2010"), "x").as_str(),
            "Windows Vista-VS2010"
        );
        assert_eq!(
            suggested_name(Some("Ünïcode/VM*"), "x").as_str(),
            "-n-code-VM-".trim_start_matches('-')
        );
        assert_eq!(suggested_name(Some("..."), "Win7.vmx").as_str(), "Win7.vmx");
        assert_eq!(suggested_name(None, "///").as_str(), "Imported VM");
    }
}
