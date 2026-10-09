//! Policy effettiva come intersezione (ROADMAP 6.3; `docs/ARCHITECTURE.md` 6).
//!
//! `policy_effettiva = limiti_globali ∩ policy_VM ∩ grant_sessione ∩ capacità_host`.
//! Funzioni pure: nessun livello può ampliare i precedenti, e l'assenza di una
//! capacità host restringe invece di essere ignorata.

use lightvm_proto::{
    Capability, CapabilitySet, EffectivePolicy, HostCapacity, PolicyLayer, PolicyRevision,
    ResourceLimits,
};

/// Intersezione di due livelli.
#[must_use]
pub fn intersect_layers(a: &PolicyLayer, b: &PolicyLayer) -> PolicyLayer {
    PolicyLayer {
        capabilities: a
            .capabilities
            .intersection(&b.capabilities)
            .copied()
            .collect(),
        quotas: a.quotas.intersect(&b.quotas),
    }
}

/// Policy effettiva di una sessione agente su una VM.
#[must_use]
pub fn effective_policy(
    global: &PolicyLayer,
    vm: &PolicyLayer,
    grant: &PolicyLayer,
    host: &PolicyLayer,
    revision: PolicyRevision,
) -> EffectivePolicy {
    let layer = intersect_layers(
        &intersect_layers(&intersect_layers(global, vm), grant),
        host,
    );
    EffectivePolicy {
        capabilities: layer.capabilities,
        quotas: layer.quotas,
        revision,
    }
}

/// Policy effettiva per l'utente locale: come per l'agente, senza grant (la UI
/// non è un livello fidato aggiuntivo, ma l'utente non ha bisogno di un grant).
#[must_use]
pub fn effective_user_policy(
    global: &PolicyLayer,
    vm: &PolicyLayer,
    host: &PolicyLayer,
    revision: PolicyRevision,
) -> EffectivePolicy {
    effective_policy(global, vm, &PolicyLayer::permissive(), host, revision)
}

/// `true` se `inner` non amplia `outer` (capability ⊆ e quote ≤).
#[must_use]
pub fn is_within(inner: &PolicyLayer, outer: &PolicyLayer) -> bool {
    inner.capabilities.is_subset(&outer.capabilities) && inner.quotas.is_within(&outer.quotas)
}

/// Livello derivato dalla capacità host: le capability prive di supporto sono escluse.
///
/// - senza acceleratore approvato: niente `vm.start` (nessun fallback TCG, ADR-0007);
/// - senza cattura: niente `vm.screenshot`; senza input: niente `vm.input`;
/// - senza baseline (prima di F3): niente `vm.snapshot` né `vm.restore`.
#[must_use]
pub fn host_layer(host: &HostCapacity) -> PolicyLayer {
    let capabilities: CapabilitySet = Capability::ALL
        .into_iter()
        .filter(|c| match c {
            Capability::VmStart => host.accelerator_available,
            Capability::VmScreenshot => host.screenshot_available,
            Capability::VmInput => host.input_available,
            Capability::VmSnapshot | Capability::VmRestore => host.baseline_available,
            Capability::VmList
            | Capability::VmStatus
            | Capability::VmStop
            | Capability::VmReset
            | Capability::VmLogs => true,
        })
        .collect();
    PolicyLayer {
        capabilities,
        quotas: lightvm_proto::Quotas::UNBOUNDED,
    }
}

/// Limiti di risorse effettivi: limiti globali ∩ limiti VM (se presenti) ∩ capacità host.
#[must_use]
pub fn effective_resource_limits(
    global: &ResourceLimits,
    vm: Option<&ResourceLimits>,
    host: &HostCapacity,
) -> ResourceLimits {
    let base = match vm {
        Some(v) => global.intersect(v),
        None => *global,
    };
    base.intersect(&host.as_limits())
}
