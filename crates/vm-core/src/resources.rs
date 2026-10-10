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

use lightvm_proto::{PublicError, ResourceLimits, VmConfig};

/// Minimum allowed guest memory in MiB.
pub const MIN_MEMORY_MIB: u64 = 512;
/// Minimum virtual disk in GiB.
pub const MIN_DISK_GIB: u64 = 4;

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
    if config.vcpus == 0 {
        return Err(PublicError::invalid_argument("vcpus must be at least 1"));
    }
    if config.memory_mib < MIN_MEMORY_MIB {
        return Err(PublicError::invalid_argument("memory below the minimum")
            .with_detail("min", MIN_MEMORY_MIB.to_string()));
    }
    if config.disk_gib < MIN_DISK_GIB {
        return Err(PublicError::invalid_argument("disk below the minimum")
            .with_detail("min", MIN_DISK_GIB.to_string()));
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
