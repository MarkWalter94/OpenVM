//! Validazione delle risorse richieste (ROADMAP 7.2, 7.4 riga "Risorse").
//!
//! I limiti effettivi sono l'intersezione di due livelli:
//! - **utente**: limiti globali configurati nell'app (`CoreConfig::global_limits`,
//!   predefiniti [`ResourceLimits::MVP_DEFAULT`]);
//! - **host**: capacità rilevate dal probe ([`lightvm_proto::HostCapacity::as_limits`]:
//!   vCPU ≤ processori logici, memoria ≤ totale meno riserva, disco ≤ libero
//!   meno riserva, nessuna VM in esecuzione senza acceleratore).
//!
//! La validazione per VM avviene alla creazione e di nuovo a ogni avvio; all'avvio
//! il core verifica anche il numero di VM in esecuzione e la memoria guest
//! complessiva rispetto alla memoria host meno la riserva. Il budget di storage
//! complessivo (overlay, log, screenshot) non è contabilizzato qui (ADR-0017).

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

/// Limiti per una VM già creata: il disco esistente e il suo budget restano
/// ammessi anche se lo spazio libero dell'host è sceso sotto la loro
/// dimensione (il disco è già allocato); solo una crescita oltre lo spazio
/// disponibile viene rifiutata.
pub fn existing_disk_limits(limits: ResourceLimits, current: &VmConfig) -> ResourceLimits {
    ResourceLimits {
        max_disk_gib: limits
            .max_disk_gib
            .max(current.disk_gib)
            .max(current.limits.storage_budget_gib.unwrap_or(0)),
        ..limits
    }
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
            "supporto di installazione e supporto dati devono essere distinti",
        ));
    }
    Ok(())
}
