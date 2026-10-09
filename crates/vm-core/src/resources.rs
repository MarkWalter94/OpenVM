//! Validazione delle risorse richieste (ROADMAP 7.2, 7.4 riga "Risorse").

use lightvm_proto::{PublicError, ResourceLimits, VmConfig};

/// Memoria guest minima ammessa in MiB.
pub const MIN_MEMORY_MIB: u64 = 512;
/// Disco virtuale minimo in GiB.
pub const MIN_DISK_GIB: u64 = 4;

fn over(field: &'static str, requested: u64, limit: u64) -> PublicError {
    PublicError::resource_limit("risorsa richiesta oltre il limite effettivo")
        .with_detail("field", field)
        .with_detail("requested", requested.to_string())
        .with_detail("limit", limit.to_string())
}

/// Valida una configurazione contro i limiti effettivi (già intersecati con la
/// capacità host, vedi [`crate::policy::effective_resource_limits`]).
///
/// Valori nulli o sotto il minimo → `invalid_argument`; oltre il limite →
/// `resource_limit` con il campo in `details`.
pub fn validate_config(config: &VmConfig, limits: &ResourceLimits) -> Result<(), PublicError> {
    if config.vcpus == 0 {
        return Err(PublicError::invalid_argument("vcpus deve essere almeno 1"));
    }
    if config.memory_mib < MIN_MEMORY_MIB {
        return Err(PublicError::invalid_argument("memoria sotto il minimo")
            .with_detail("min", MIN_MEMORY_MIB.to_string()));
    }
    if config.disk_gib < MIN_DISK_GIB {
        return Err(PublicError::invalid_argument("disco sotto il minimo")
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
        return Err(over(
            "memory_mib",
            config.memory_mib,
            limits.max_memory_mib,
        ));
    }
    if config.disk_gib > limits.max_disk_gib {
        return Err(over("disk_gib", config.disk_gib, limits.max_disk_gib));
    }
    if config.install_media.is_some() && config.install_media == config.data_media {
        return Err(PublicError::invalid_argument(
            "supporto di installazione e supporto dati devono essere distinti",
        ));
    }
    Ok(())
}
