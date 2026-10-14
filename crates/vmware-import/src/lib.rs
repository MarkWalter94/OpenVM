//! `vmware-import`: examination of a VMware VM before its import by
//! conversion (ADR-0030, ROADMAP R6).
//!
//! Every byte read here comes from the user's files and is **untrusted**:
//!
//! - [`vmx`]: the `.vmx` is read as text with size, line and value limits;
//!   only `key = "value"` lines are considered and only the keys LightVM
//!   knows are kept (guest type, CPU, memory, firmware, disks, state,
//!   encryption, the devices listed as "not imported"); everything else is
//!   ignored and nothing is ever passed to QEMU;
//! - [`vmdk`]: VMDK descriptors, as a text file or embedded in the header
//!   of a sparse extent (`KDMV`: monolithicSparse, streamOptimized, the
//!   delta disks of snapshots), are parsed with the same kind of limits;
//!   only the create types and extent types QEMU reads as plain disk data
//!   are accepted (no raw device mappings, no ESX-only formats);
//! - [`paths`]: every file a disk refers to (extents, `parentFileNameHint`)
//!   must be a regular file **inside the VM folder**: absolute paths outside
//!   it, `..`, links and reparse points on any component, device names,
//!   UNC and other remote paths are refused, before any process opens
//!   them; the snapshot chain is bounded ([`MAX_CHAIN_DEPTH`]);
//! - [`inspect`]: the VM folder as a whole: the `.vmx` chosen by the user
//!   (checked as for media, ADR-0029), lock files (VM in use), suspended
//!   state, encryption, unclean shutdown, unknown guest type, the main disk
//!   and its chain;
//! - [`qemu_info`]: cross-check of the output of
//!   `qemu-img info --backing-chain --output=json` against the files
//!   validated here (format, chain, virtual size, allocation);
//! - [`plan`]: mapping to a LightVM configuration (ADR-0030 table) within
//!   the host limits, with the lists of what is and is not imported.
//!
//! **Why the descriptors are parsed here and not only by `qemu-img`.**
//! `qemu-img info` gives the format, the chain and the virtual size
//! reliably, but it opens every extent and parent named by a descriptor:
//! a hostile descriptor pointing at `\\server\share\x` or `\\.\PhysicalDrive0`
//! would already have been opened (credential leak to a remote host, raw
//! device read) before its output could be checked. So the paths are
//! validated from the descriptors first, `qemu-img` runs only on a chain
//! whose every file has been checked, and its output is then compared with
//! what was validated.
//!
//! The crate does not write anything and starts no process: the caller
//! (`lightvm-service`) runs `qemu-img` from the approved QEMU build.

pub mod inspect;
pub mod paths;
pub mod plan;
pub mod qemu_info;
pub mod vmdk;
pub mod vmx;

use lightvm_proto::PublicError;

pub use inspect::{DiskSource, Inspection, VmwareGuest, inspect, open_guard};
pub use plan::{HostLimits, Plan, plan};
pub use qemu_info::{ChainFacts, check_chain_info};

/// Longest `.vmx` accepted (VMware writes a few KiB).
pub const MAX_VMX_BYTES: u64 = 1 << 20;
/// Longest VMDK descriptor accepted, text file or embedded.
pub const MAX_DESCRIPTOR_BYTES: u64 = 64 << 10;
/// Most descriptors in a disk chain (the current disk plus its snapshot
/// parents; VMware allows 31 snapshots in a chain).
pub const MAX_CHAIN_DEPTH: usize = 32;
/// Most extents in one descriptor (a 2 TiB twoGbMaxExtent disk has 1024).
pub const MAX_EXTENTS: usize = 4096;
/// Most entries of the VM folder looked at for lock and suspend files.
pub const MAX_FOLDER_ENTRIES: usize = 10_000;

/// Why a VMware VM cannot be imported. [`ImportError::to_public`] gives the
/// message for the user: it names files, never their folders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportError {
    /// The path of the `.vmx` is not acceptable (relative, UNC, device,
    /// wrong extension, characters).
    InvalidPath(&'static str),
    /// A file is missing.
    NotFound(String),
    /// A link, junction or reparse point on the way to a file.
    Link(String),
    /// The `.vmx` is not where the user saw it (link, mount point,
    /// substituted or network drive on the way), or not on a local volume.
    NotLocal,
    /// A file is not a regular file.
    NotAFile(String),
    /// A file is larger than its limit.
    TooLarge(String),
    /// The `.vmx` cannot be read as a VMware configuration.
    InvalidVmx(&'static str),
    /// A lock file is present: the VM is open in VMware.
    Locked,
    /// The VM is suspended.
    Suspended,
    /// The VM was not shut down cleanly.
    NotCleanShutdown,
    /// The VM is encrypted.
    Encrypted,
    /// The VMware guest type is not one LightVM supports.
    UnknownGuest(String),
    /// The guest type is known but not supported (with the reason).
    UnsupportedGuest(String, &'static str),
    /// Firmware not usable for the guest (EFI Vista/7, EFI 32-bit).
    UnsupportedFirmware(&'static str),
    /// No hard disk in the `.vmx`.
    NoDisk,
    /// A disk file is outside the VM folder or its name is not acceptable.
    OutsideFolder(String),
    /// A VMDK descriptor cannot be used (with the file and the reason).
    InvalidDescriptor(String, &'static str),
    /// A VMDK feature QEMU cannot convert safely (raw device, ESX format).
    UnsupportedDisk(String, &'static str),
    /// The snapshot chain is longer than [`MAX_CHAIN_DEPTH`] or loops.
    ChainTooDeep,
    /// `qemu-img info` does not describe the chain that was validated.
    ChainMismatch(&'static str),
    /// The disk does not fit the limits of the host.
    DiskTooLarge {
        /// Virtual size in GiB (rounded up).
        gib: u64,
        /// Largest disk accepted in GiB.
        max_gib: u64,
    },
    /// I/O error reading a file (the file name only).
    Io(String),
}

impl ImportError {
    /// Public error for the user (no host path).
    #[must_use]
    pub fn to_public(&self) -> PublicError {
        match self {
            Self::InvalidPath(why) => {
                PublicError::invalid_argument(format!("VMware VM path not accepted: {why}"))
            }
            Self::NotFound(f) => PublicError::not_found(format!("file not found: {f}")),
            Self::Link(f) => PublicError::permission_denied(format!(
                "links, junctions and reparse points are not allowed ({f})"
            )),
            Self::NotLocal => PublicError::permission_denied(
                "the .vmx path goes through a link, junction, mount point, network or substituted drive: choose the file in its real location on a local drive",
            ),
            Self::NotAFile(f) => {
                PublicError::invalid_argument(format!("not a regular file: {f}"))
            }
            Self::TooLarge(f) => PublicError::invalid_argument(format!(
                "{f} is larger than a VMware configuration or descriptor can be"
            )),
            Self::InvalidVmx(why) => {
                PublicError::invalid_argument(format!("not a usable VMware .vmx file: {why}"))
            }
            Self::Locked => PublicError::state_conflict(
                "the VM is in use in VMware (lock files present): shut it down and close VMware, then import it",
            )
            .with_detail("reason", "locked"),
            Self::Suspended => PublicError::state_conflict(
                "the VM is suspended in VMware: resume it in VMware and shut it down, then import it",
            )
            .with_detail("reason", "suspended"),
            Self::NotCleanShutdown => PublicError::state_conflict(
                "the VM was not shut down cleanly in VMware: start it in VMware and shut it down from the guest, then import it",
            )
            .with_detail("reason", "unclean_shutdown"),
            Self::Encrypted => PublicError::unsupported(
                "encrypted VMware VMs cannot be imported: remove the encryption in VMware first",
            )
            .with_detail("reason", "encrypted"),
            Self::UnknownGuest(g) => PublicError::unsupported(format!(
                "the VMware guest type \"{g}\" is not a system LightVM supports"
            ))
            .with_detail("reason", "unknown_guest"),
            Self::UnsupportedGuest(g, why) => {
                PublicError::unsupported(format!("the VMware guest type \"{g}\" is not supported: {why}"))
                    .with_detail("reason", "unsupported_guest")
            }
            Self::UnsupportedFirmware(why) => PublicError::unsupported(why.to_string()),
            Self::NoDisk => PublicError::invalid_argument("the VMware VM has no hard disk"),
            Self::OutsideFolder(f) => PublicError::permission_denied(format!(
                "the disk file \"{f}\" is not inside the VM folder: only files in the folder of the .vmx are read"
            )),
            Self::InvalidDescriptor(f, why) => {
                PublicError::invalid_argument(format!("VMDK descriptor of {f} not usable: {why}"))
            }
            Self::UnsupportedDisk(f, why) => {
                PublicError::unsupported(format!("disk {f} cannot be imported: {why}"))
            }
            Self::ChainTooDeep => PublicError::invalid_argument(format!(
                "the snapshot chain of the disk is longer than {MAX_CHAIN_DEPTH} or loops"
            )),
            Self::ChainMismatch(why) => PublicError::invalid_argument(format!(
                "the disk chain read by qemu-img differs from the checked one: {why}"
            )),
            Self::DiskTooLarge { gib, max_gib } => PublicError::resource_limit(format!(
                "the disk ({gib} GiB) is larger than the {max_gib} GiB allowed on this host"
            ))
            .with_detail("disk_gib", gib.to_string())
            .with_detail("max_gib", max_gib.to_string()),
            Self::Io(f) => PublicError::permission_denied(format!("cannot read {f}")),
        }
    }
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_public().message)
    }
}

impl std::error::Error for ImportError {}

/// Text from a VMware file made safe to show: control characters removed,
/// at most [`lightvm_proto::ipc::MAX_VMWARE_TEXT_CHARS`] characters.
#[must_use]
pub fn shown(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control())
        .take(lightvm_proto::ipc::MAX_VMWARE_TEXT_CHARS)
        .collect()
}

/// File name of a path, for messages (never the folder).
#[must_use]
pub fn file_name(p: &std::path::Path) -> String {
    p.file_name()
        .map(|n| shown(&n.to_string_lossy()))
        .unwrap_or_default()
}
