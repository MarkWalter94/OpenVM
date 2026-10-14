//! Examination of a VMware VM folder (ADR-0030).
//!
//! [`inspect`] runs the checks in this order, each refusing the import with
//! its own message: path of the `.vmx` (text, then on the file system: a
//! regular file, no link, the location the system resolves is the path the
//! user chose, so no link, junction, mount point, network or substituted
//! drive on the way); lock files in the folder (VM open in VMware);
//! suspended state (`.vmss` file or `checkpoint.vmState`); the `.vmx`
//! itself ([`crate::vmx`]); encryption; unclean shutdown; guest type and
//! firmware; the main disk and its chain of descriptors, every file of
//! which must be inside the folder ([`crate::paths`]), bounded by
//! [`crate::MAX_CHAIN_DEPTH`], with consistent sizes and parent CIDs.
//!
//! Every file is opened read-only; on Windows with `FILE_SHARE_READ` only
//! and without following a reparse point. [`open_guard`] keeps the files of
//! the chain open that way while `qemu-img` reads them, so nobody can
//! write, rename or delete them meanwhile.

use std::path::{Path, PathBuf};

use lightvm_proto::Firmware;
use lightvm_proto::ipc::VmwareController;

use crate::paths::{is_link, resolve_in_folder, same_path};
use crate::vmdk::{Descriptor, DescriptorSource, read_descriptor};
use crate::vmx::{Bus, Device, Vmx, parse_vmx};
use crate::{ImportError, MAX_CHAIN_DEPTH, MAX_FOLDER_ENTRIES, file_name};

pub use crate::plan::VmwareGuest;

/// Main disk of the VM and its chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskSource {
    /// Device of the `.vmx`.
    pub device: Device,
    /// Controller in VMware.
    pub controller: VmwareController,
    /// Current VMDK (top of the chain).
    pub top: PathBuf,
    /// Descriptor files of the chain, current first, base last.
    pub chain: Vec<PathBuf>,
    /// Descriptors of the chain, in the same order.
    pub descriptors: Vec<Descriptor>,
    /// Every file of the chain (descriptors and extents), each once.
    pub files: Vec<PathBuf>,
}

impl DiskSource {
    /// Snapshot levels flattened by a conversion.
    #[must_use]
    pub fn snapshot_levels(&self) -> u32 {
        u32::try_from(self.chain.len().saturating_sub(1)).unwrap_or(u32::MAX)
    }

    /// `createType` of the current VMDK.
    #[must_use]
    pub fn create_type(&self) -> &str {
        self.descriptors
            .first()
            .map_or("", |d| d.create_type.as_str())
    }
}

/// Outcome of [`inspect`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inspection {
    /// `.vmx` as resolved by the system.
    pub vmx_path: PathBuf,
    /// File name of the `.vmx`.
    pub vmx_file: String,
    /// VM folder (folder of the `.vmx`).
    pub folder: PathBuf,
    /// Known keys of the `.vmx`.
    pub vmx: Vmx,
    /// Guest system.
    pub guest: VmwareGuest,
    /// Firmware of the VM.
    pub firmware: Firmware,
    /// Main disk.
    pub disk: DiskSource,
    /// Other hard disks (`slot file`), not imported.
    pub other_disks: Vec<String>,
    /// CD/DVD drives (`slot` or `slot file`), not imported.
    pub cdroms: Vec<String>,
}

/// Opens a file read-only: on Windows sharing only reads and without
/// following a reparse point on the last component.
pub fn open_read(path: &Path) -> Result<std::fs::File, ImportError> {
    let mut o = std::fs::OpenOptions::new();
    o.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_SHARE_READ, FILE_FLAG_OPEN_REPARSE_POINT.
        o.share_mode(0x1);
        o.custom_flags(0x0020_0000);
    }
    let f = o.open(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => ImportError::NotFound(file_name(path)),
        _ => ImportError::Io(file_name(path)),
    })?;
    let meta = f.metadata().map_err(|_| ImportError::Io(file_name(path)))?;
    if is_link(&meta) {
        return Err(ImportError::Link(file_name(path)));
    }
    if !meta.is_file() {
        return Err(ImportError::NotAFile(file_name(path)));
    }
    Ok(f)
}

/// Handles keeping the files of a chain open read-only during a
/// conversion (see the module documentation).
#[derive(Debug)]
pub struct Guard(#[allow(dead_code)] Vec<std::fs::File>);

/// Opens every file of the chain as [`open_read`] does and keeps the
/// handles until the [`Guard`] is dropped.
pub fn open_guard(disk: &DiskSource) -> Result<Guard, ImportError> {
    disk.files
        .iter()
        .map(|p| open_read(p))
        .collect::<Result<Vec<_>, _>>()
        .map(Guard)
}

/// Path the system resolves for `p` (`canonicalize`, with the `\\?\`
/// prefix of a drive path removed).
fn resolved(p: &Path) -> Result<PathBuf, ImportError> {
    let c = std::fs::canonicalize(p).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => ImportError::NotFound(file_name(p)),
        _ => ImportError::Io(file_name(p)),
    })?;
    let s = c.to_string_lossy();
    Ok(match s.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => c.clone(),
    })
}

/// Examines the VMware VM whose `.vmx` is `vmx_raw` (see the module
/// documentation).
pub fn inspect(vmx_raw: &str) -> Result<Inspection, ImportError> {
    let requested = crate::paths::check_vmx_path(vmx_raw)?;
    match std::fs::symlink_metadata(&requested) {
        Ok(m) if is_link(&m) => return Err(ImportError::Link(file_name(&requested))),
        Ok(m) if !m.is_file() => return Err(ImportError::NotAFile(file_name(&requested))),
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(ImportError::NotFound(file_name(&requested)));
        }
        Err(_) => return Err(ImportError::Io(file_name(&requested))),
    }
    let vmx_path = resolved(&requested)?;
    if !same_path(&vmx_path, &requested) {
        return Err(ImportError::NotLocal);
    }
    let folder = vmx_path
        .parent()
        .filter(|p| p.parent().is_some())
        .ok_or(ImportError::InvalidPath("the .vmx cannot be at the root of a drive"))?
        .to_path_buf();
    let vmx_file = file_name(&vmx_path);
    check_folder(&folder)?;
    let mut f = open_read(&vmx_path)?;
    let len = f
        .metadata()
        .map_err(|_| ImportError::Io(vmx_file.clone()))?
        .len();
    if len > crate::MAX_VMX_BYTES {
        return Err(ImportError::TooLarge(vmx_file));
    }
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(
        &mut std::io::Read::take(&mut f, crate::MAX_VMX_BYTES + 1),
        &mut bytes,
    )
    .map_err(|_| ImportError::Io(vmx_file.clone()))?;
    drop(f);
    let vmx = parse_vmx(&bytes)?;
    if vmx.encrypted {
        return Err(ImportError::Encrypted);
    }
    if vmx
        .get("checkpoint.vmstate")
        .is_some_and(|s| !s.trim().is_empty())
    {
        return Err(ImportError::Suspended);
    }
    if vmx
        .get("cleanshutdown")
        .is_some_and(|v| v.eq_ignore_ascii_case("false"))
    {
        return Err(ImportError::NotCleanShutdown);
    }
    let raw_guest = vmx
        .get("guestos")
        .ok_or(ImportError::InvalidVmx("guestOS missing"))?;
    let guest = crate::plan::guest_from_vmware(raw_guest)?;
    let firmware = match vmx.get("firmware").map(str::to_ascii_lowercase).as_deref() {
        None | Some("bios") => Firmware::Bios,
        Some("efi") => Firmware::Uefi,
        Some(_) => return Err(ImportError::InvalidVmx("unknown firmware")),
    };
    crate::plan::check_firmware(&guest, firmware)?;
    let devices = vmx.devices();
    let disks: Vec<&Device> = devices.iter().filter(|d| d.is_hard_disk()).collect();
    let main = choose_disk(&vmx, &disks).ok_or(ImportError::NoDisk)?;
    let controller = controller_of(&vmx, main)?;
    let disk = walk_chain(&folder, main, controller)?;
    let other_disks = disks
        .iter()
        .filter(|d| d.slot() != main.slot())
        .map(|d| {
            let name = d
                .file_name
                .as_deref()
                .map(|f| file_name(Path::new(&f.replace('\\', "/"))))
                .unwrap_or_default();
            crate::shown(&format!("{} {name}", d.slot()))
        })
        .collect();
    let cdroms = devices
        .iter()
        .filter(|d| d.is_cdrom())
        .map(|d| match (&d.device_type, &d.file_name) {
            (Some(t), Some(f)) if t == "cdrom-image" => crate::shown(&format!(
                "{} {}",
                d.slot(),
                file_name(Path::new(&f.replace('\\', "/")))
            )),
            _ => d.slot(),
        })
        .collect();
    Ok(Inspection {
        vmx_path,
        vmx_file,
        folder,
        vmx,
        guest,
        firmware,
        disk,
        other_disks,
        cdroms,
    })
}

/// Lock files (`*.lck`, VM open in VMware) and suspended state (`*.vmss`).
fn check_folder(folder: &Path) -> Result<(), ImportError> {
    let rd = std::fs::read_dir(folder).map_err(|_| ImportError::Io("the VM folder".into()))?;
    let mut suspended = false;
    for (n, e) in rd.enumerate() {
        if n >= MAX_FOLDER_ENTRIES {
            return Err(ImportError::InvalidVmx("too many files in the VM folder"));
        }
        let e = e.map_err(|_| ImportError::Io("the VM folder".into()))?;
        let name = e.file_name().to_string_lossy().to_ascii_lowercase();
        if name.ends_with(".lck") {
            return Err(ImportError::Locked);
        }
        if name.ends_with(".vmss") {
            suspended = true;
        }
    }
    if suspended {
        return Err(ImportError::Suspended);
    }
    Ok(())
}

/// Main disk: the first entry of `bios.hddOrder` that is a disk, otherwise
/// the first disk in IDE, SATA, SCSI, NVMe order.
fn choose_disk<'a>(vmx: &Vmx, disks: &[&'a Device]) -> Option<&'a Device> {
    if let Some(order) = vmx.get("bios.hddorder") {
        for slot in order.split([' ', ',', ';']).filter(|s| !s.is_empty()) {
            if let Some(d) = disks.iter().find(|d| d.slot().eq_ignore_ascii_case(slot)) {
                return Some(d);
            }
        }
    }
    disks.first().copied()
}

fn controller_of(vmx: &Vmx, d: &Device) -> Result<VmwareController, ImportError> {
    Ok(match d.bus {
        Bus::Ide => VmwareController::Ide,
        Bus::Sata => VmwareController::Sata,
        Bus::Nvme => VmwareController::Nvme,
        Bus::Scsi => match vmx.scsi_virtual_dev(d.controller).as_deref() {
            Some("pvscsi") => VmwareController::Pvscsi,
            Some("lsisas1068") => VmwareController::LsiSas1068,
            Some("lsilogic") => VmwareController::LsiLogic,
            // VMware's default SCSI adapter when `virtualDev` is absent.
            Some("buslogic") | None => VmwareController::BusLogic,
            Some(_) => {
                return Err(ImportError::UnsupportedDisk(
                    d.slot(),
                    "unknown SCSI controller type",
                ));
            }
        },
    })
}

/// Follows the chain of descriptors from the disk of `d`.
fn walk_chain(
    folder: &Path,
    d: &Device,
    controller: VmwareController,
) -> Result<DiskSource, ImportError> {
    let raw = d.file_name.as_deref().ok_or(ImportError::NoDisk)?;
    let top = resolve_in_folder(folder, folder, raw)?;
    let mut chain: Vec<PathBuf> = Vec::new();
    let mut descriptors: Vec<Descriptor> = Vec::new();
    let mut files: Vec<PathBuf> = Vec::new();
    let add = |files: &mut Vec<PathBuf>, p: &Path| {
        if !files.iter().any(|f| same_path(f, p)) {
            files.push(p.to_path_buf());
        }
    };
    let mut cur = top.clone();
    loop {
        if chain.len() >= MAX_CHAIN_DEPTH || chain.iter().any(|c| same_path(c, &cur)) {
            return Err(ImportError::ChainTooDeep);
        }
        let name = file_name(&cur);
        let mut f = open_read(&cur)?;
        let (desc, source) = read_descriptor(&mut f, &name)?;
        drop(f);
        let dir = cur.parent().unwrap_or(folder).to_path_buf();
        add(&mut files, &cur);
        for e in &desc.extents {
            if let Some(x) = &e.file {
                let p = resolve_in_folder(folder, &dir, x)?;
                // An extent is data: it must not be a descriptor of the
                // chain (except a sparse file that embeds its own).
                let own_text = source == DescriptorSource::Text && same_path(&p, &cur);
                if own_text || chain.iter().any(|c| same_path(c, &p)) {
                    return Err(ImportError::InvalidDescriptor(
                        name.clone(),
                        "an extent is another disk of the chain",
                    ));
                }
                add(&mut files, &p);
            }
        }
        if let Some(prev) = descriptors.last() {
            if prev.sectors() != desc.sectors() {
                return Err(ImportError::InvalidDescriptor(
                    name.clone(),
                    "size differs from the snapshot that refers to it",
                ));
            }
            if prev.parent_cid != desc.cid {
                return Err(ImportError::InvalidDescriptor(
                    name.clone(),
                    "the snapshot chain is inconsistent (parent CID differs)",
                ));
            }
        }
        let parent = desc.parent_hint.clone();
        chain.push(cur.clone());
        descriptors.push(desc);
        match parent {
            Some(h) => cur = resolve_in_folder(folder, &dir, &h)?,
            None => break,
        }
    }
    Ok(DiskSource {
        device: d.clone(),
        controller,
        top,
        chain,
        descriptors,
        files,
    })
}
