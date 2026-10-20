//! Effective policy as an intersection (ROADMAP 6.3; `docs/ARCHITECTURE.md` 6).
//!
//! `effective_policy = global_limits ∩ VM_policy ∩ session_grant ∩ host_capacity`.
//! Pure functions: no layer can widen the previous ones, and a missing host
//! capability restricts instead of being ignored.

use lightvm_proto::{
    Capability, CapabilitySet, EffectivePolicy, HostCapacity, PolicyLayer, PolicyRevision,
    ResourceLimits,
};

/// Intersection of two layers.
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

/// Effective policy of an agent session on a VM.
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

/// Effective policy for the local user: as for the agent, without a grant (the UI
/// is not an additional trusted layer, but the user does not need a grant).
#[must_use]
pub fn effective_user_policy(
    global: &PolicyLayer,
    vm: &PolicyLayer,
    host: &PolicyLayer,
    revision: PolicyRevision,
) -> EffectivePolicy {
    effective_policy(global, vm, &PolicyLayer::permissive(), host, revision)
}

/// `true` if `inner` does not widen `outer` (capabilities ⊆ and quotas ≤).
#[must_use]
pub fn is_within(inner: &PolicyLayer, outer: &PolicyLayer) -> bool {
    inner.capabilities.is_subset(&outer.capabilities) && inner.quotas.is_within(&outer.quotas)
}

/// Layer derived from host capacity: capabilities without support are excluded.
///
/// - without an approved accelerator: no `vm.start` (no TCG fallback, ADR-0007);
/// - without capture: no `vm.screenshot`; without input: no `vm.input`;
/// - without baseline (before F3): neither `vm.snapshot` nor `vm.restore`.
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
            | Capability::VmLogs
            | Capability::NetworkEndpoints
            | Capability::ShareRead
            | Capability::ShareWrite
            | Capability::Clipboard
            | Capability::FilesTransfer
            | Capability::Usb
            | Capability::UiInspect
            | Capability::UiAct
            | Capability::GuestExec => true,
        })
        .collect();
    PolicyLayer {
        capabilities,
        quotas: lightvm_proto::Quotas::UNBOUNDED,
    }
}

/// Effective resource limits: global limits ∩ VM limits (if any) ∩ host capacity.
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
