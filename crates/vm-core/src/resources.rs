//! Validation of requested resources (ROADMAP 7.2, 7.4 row "Resources").
//!
//! The effective limits are the intersection of two layers:
//! - **user**: global limits configured in the app (`CoreConfig::global_limits`,
//!   default [`ResourceLimits::MVP_DEFAULT`]);
//! - **host**: capacity detected by the probe ([`lightvm_proto::HostCapacity::as_limits`]:
//!   vCPU ≤ logical processors, memory ≤ total minus reserve, disk ≤ free
//!   minus reserve, no VM running without an accelerator).
//!
//! Per-VM validation happens at creation and again at every start; at start
//! the core also checks the number of running VMs and the total guest memory
//! against host memory minus the reserve. The overall storage budget
//! (overlays, logs, screenshots) is not accounted for here (ADR-0017).

use lightvm_proto::{Firmware, GuestArch, GuestOs, PublicError, ResourceLimits, VmConfig};

/// Minimum allowed guest memory in MiB for the Linux guests (the reference
/// profile); each guest has its own ([`min_memory_mib`]).
pub const MIN_MEMORY_MIB: u64 = 512;
/// Minimum virtual disk in GiB for the Linux guests; each guest has its own
/// ([`min_disk_gib`]).
pub const MIN_DISK_GIB: u64 = 4;

/// Minimum guest memory for `os` in MiB: [`GuestOs::min_memory_mib`]
/// (512 MiB for Linux, as before the Windows guests).
pub fn min_memory_mib(os: GuestOs) -> u64 {
    os.min_memory_mib()
}

/// Minimum main disk for `os` in GiB: [`GuestOs::min_disk_gib`] for the
/// Windows guests; the Linux guests keep [`MIN_DISK_GIB`], the floor VMs
/// were created with before the Windows guests (an existing 4 GiB Linux VM
/// stays valid).
pub fn min_disk_gib(os: GuestOs) -> u64 {
    if os.is_windows() {
        os.min_disk_gib()
    } else {
        MIN_DISK_GIB
    }
}

/// Short name of the guest for messages.
fn guest_name(os: GuestOs) -> &'static str {
    match os {
        GuestOs::UbuntuLtsX86_64 => "Ubuntu LTS",
        GuestOs::OtherLinuxX86_64 => "Linux",
        GuestOs::WindowsXp => "Windows XP",
        GuestOs::WindowsVista => "Windows Vista",
        GuestOs::Windows7 => "Windows 7",
        GuestOs::Windows81 => "Windows 8.1",
        GuestOs::Windows10 => "Windows 10",
        GuestOs::Windows11Arm64 => "Windows 11 ARM64",
    }
}

/// Which guests the host can run with hardware acceleration (ADR-0007,
/// ADR-0028), given the architecture detected in the installation media of
/// the VM (`None` = no media or not detected). The production rule is
/// [`GuestOs::runs_on_this_host_with`] (Apple Silicon macOS runs only
/// AArch64 guests, every other host only x86 guests, Windows hosts not
/// Windows Vista x64); tests and fake backends replace it with
/// [`crate::Core::set_guest_support`].
pub type GuestSupport = fn(GuestOs, Option<GuestArch>) -> bool;

/// Every guest is accepted (fake backends, tests).
#[must_use]
pub fn any_guest(_: GuestOs, _: Option<GuestArch>) -> bool {
    true
}

fn arch_name(arch: GuestArch) -> &'static str {
    match arch {
        GuestArch::X86 => "32-bit",
        GuestArch::X86_64 => "64-bit",
        GuestArch::Aarch64 => "ARM64",
    }
}

/// The guest must be one the host can run (`unsupported` otherwise): an
/// x86 guest on Apple Silicon or Windows 11 ARM64 on an x86 host would only
/// run under TCG, which ADR-0007 excludes. `arch` is the architecture
/// detected in the installation media: a guest may run on a host in one
/// edition only (Windows Vista x64 stops at boot under WHPX, ADR-0027).
pub fn check_guest_host(
    os: GuestOs,
    arch: Option<GuestArch>,
    supported: GuestSupport,
) -> Result<(), PublicError> {
    if !supported(os, None) {
        return Err(PublicError::unsupported(format!(
            "{} cannot run on this host: no hardware acceleration for its architecture",
            guest_name(os)
        ))
        .with_detail("field", "guest_os"));
    }
    match arch {
        Some(a) if !supported(os, arch) => {
            let hint = if os == GuestOs::WindowsVista && a == GuestArch::X86_64 {
                "it stops at boot under the hypervisor of this host; use the 32-bit installation media"
            } else {
                "use installation media of another edition"
            };
            Err(PublicError::unsupported(format!(
                "{} {} cannot run on this host: {hint}",
                guest_name(os),
                arch_name(a)
            ))
            .with_detail("field", "install_media")
            .with_detail(
                "arch",
                match a {
                    GuestArch::X86 => "x86",
                    GuestArch::X86_64 => "x86_64",
                    GuestArch::Aarch64 => "aarch64",
                },
            ))
        }
        _ => Ok(()),
    }
}

fn firmware_name(f: Firmware) -> &'static str {
    match f {
        Firmware::Uefi => "UEFI",
        Firmware::Bios => "legacy BIOS",
    }
}

/// The firmware must be one the guest supports (ADR-0027): Linux boots with
/// UEFI only, Windows XP, Vista and 7 with legacy BIOS only, Windows 8.1
/// and 10 with either. A VM imported from VMware keeps the firmware it was
/// installed with ([`lightvm_proto::GuestOs::import_firmware`], ADR-0030:
/// Linux also with legacy BIOS).
pub fn check_firmware(config: &VmConfig) -> Result<(), PublicError> {
    let allowed = if config.imported.is_some() {
        config.guest_os.import_firmware()
    } else {
        config.guest_os.allowed_firmware()
    };
    if allowed.contains(&config.firmware) {
        return Ok(());
    }
    let names: Vec<&str> = allowed.iter().map(|f| firmware_name(*f)).collect();
    Err(PublicError::invalid_argument(format!(
        "{} cannot boot with {} firmware: choose {}",
        guest_name(config.guest_os),
        firmware_name(config.firmware),
        names.join(" or ")
    ))
    .with_detail("field", "firmware")
    .with_detail("allowed", names.join(",")))
}

/// Changes of the hardware profile after creation are refused (Windows 11
/// ARM64 included, ADR-0028): the guest
/// was installed for one firmware, disk controller and display, and would
/// not boot on another (ADR-0027). Changing between the two Linux variants
/// is allowed (same profile).
pub fn check_profile_change(current: &VmConfig, new: &VmConfig) -> Result<(), PublicError> {
    if new.firmware != current.firmware {
        return Err(PublicError::invalid_argument(
            "the firmware of a VM cannot be changed after creation: the installed system would no longer boot; create a new VM instead",
        )
        .with_detail("field", "firmware"));
    }
    if new.imported != current.imported {
        return Err(PublicError::invalid_argument(
            "the hardware of an imported VM (disk controller, display, CPU topology) cannot be changed: the installed system was set up for it",
        )
        .with_detail("field", "imported"));
    }
    let both_linux = !current.guest_os.is_windows() && !new.guest_os.is_windows();
    if new.guest_os != current.guest_os && !both_linux {
        return Err(PublicError::invalid_argument(format!(
            "the operating system of a VM cannot be changed from {} to {}: its virtual hardware was chosen for the installed system; create a new VM instead",
            guest_name(current.guest_os),
            guest_name(new.guest_os)
        ))
        .with_detail("field", "guest_os"));
    }
    Ok(())
}

fn over(field: &'static str, requested: u64, limit: u64) -> PublicError {
    PublicError::resource_limit("requested resource exceeds the effective limit")
        .with_detail("field", field)
        .with_detail("requested", requested.to_string())
        .with_detail("limit", limit.to_string())
}

/// Limits for an already created VM: the existing disk and its budget stay
/// allowed even if the host free space has dropped below their size (the
/// disk is already allocated); only growth beyond the available space is
/// rejected.
pub fn existing_disk_limits(limits: ResourceLimits, current: &VmConfig) -> ResourceLimits {
    ResourceLimits {
        max_disk_gib: limits
            .max_disk_gib
            .max(current.disk_gib)
            .max(current.limits.storage_budget_gib.unwrap_or(0)),
        ..limits
    }
}

/// Validates a configuration against the effective limits (already intersected with
/// host capacity, see [`crate::policy::effective_resource_limits`]).
///
/// Zero values or below the minimum → `invalid_argument`; above the limit →
/// `resource_limit` with the field in `details`.
pub fn validate_config(config: &VmConfig, limits: &ResourceLimits) -> Result<(), PublicError> {
    config.tools.validate()?;
    if config.vcpus == 0 {
        return Err(PublicError::invalid_argument("vcpus must be at least 1"));
    }
    check_firmware(config)?;
    let min_memory = min_memory_mib(config.guest_os);
    if config.memory_mib < min_memory {
        return Err(PublicError::invalid_argument(format!(
            "memory below the minimum for {} ({min_memory} MiB)",
            guest_name(config.guest_os)
        ))
        .with_detail("min", min_memory.to_string()));
    }
    if let Some(hw) = &config.imported {
        hw.validate(config.guest_os, config.vcpus)?;
    }
    // ADR-0033: endpoints checked; no NIC for Windows on ARM (no inbox
    // driver for the emulated adapters).
    lightvm_proto::validate_network(config.network, &config.endpoints)?;
    if config.network.has_nic() && config.guest_os.is_aarch64() {
        return Err(PublicError::unsupported(
            "network modes other than offline are not available for Windows 11 ARM64",
        ));
    }
    // An imported disk already holds the installed system: its size is the
    // source's (ADR-0030), not a choice the guest minimum applies to.
    let min_disk = min_disk_gib(config.guest_os);
    if config.imported.is_none() && config.disk_gib < min_disk {
        return Err(PublicError::invalid_argument(format!(
            "disk below the minimum for {} ({min_disk} GiB)",
            guest_name(config.guest_os)
        ))
        .with_detail("min", min_disk.to_string()));
    }
    if config.vcpus > limits.max_vcpus {
        return Err(over(
            "vcpus",
            u64::from(config.vcpus),
            u64::from(limits.max_vcpus),
        ));
    }
    if config.memory_mib > limits.max_memory_mib {
        return Err(over("memory_mib", config.memory_mib, limits.max_memory_mib));
    }
    if config.disk_gib > limits.max_disk_gib {
        return Err(over("disk_gib", config.disk_gib, limits.max_disk_gib));
    }
    config.limits.validate()?;
    if let Some(b) = config.limits.storage_budget_gib
        && b > limits.max_disk_gib
    {
        return Err(over("storage_budget_gib", b, limits.max_disk_gib));
    }
    if config.install_media.is_some() && config.install_media == config.data_media {
        return Err(PublicError::invalid_argument(
            "installation medium and data medium must be distinct",
        ));
    }
    Ok(())
}
