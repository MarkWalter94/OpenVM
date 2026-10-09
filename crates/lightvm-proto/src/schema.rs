//! JSON Schema generati dalla fonte unica (ROADMAP 6.3).
//!
//! [`public_schemas`] elenca ogni tipo pubblico con il nome del file in
//! `crates/lightvm-proto/schema/`. Il test `schema_snapshot` confronta i file
//! committati con quelli generati; con `LIGHTVM_UPDATE_SCHEMAS=1` li riscrive.

use schemars::{JsonSchema, Schema, generate::SchemaSettings};

use crate::*;

fn generate<T: JsonSchema>() -> Schema {
    SchemaSettings::draft2020_12()
        .into_generator()
        .into_root_schema_for::<T>()
}

/// Coppie (nome file senza estensione, schema) per tutti i tipi pubblici.
/// L'ordine è stabile. Tipi solo interni (`MediaEntry`, `ManagedRelPath`,
/// `FileIdentity`) sono esclusi di proposito.
#[must_use]
pub fn public_schemas() -> Vec<(&'static str, Schema)> {
    vec![
        ("vm_id", generate::<VmId>()),
        ("run_id", generate::<RunId>()),
        ("operation_id", generate::<OperationId>()),
        ("session_id", generate::<SessionId>()),
        ("baseline_id", generate::<BaselineId>()),
        ("media_id", generate::<MediaId>()),
        ("grant_id", generate::<GrantId>()),
        ("utc_timestamp", generate::<UtcTimestamp>()),
        ("vm_state", generate::<VmState>()),
        ("observed_state", generate::<ObservedState>()),
        ("backend_observation", generate::<BackendObservation>()),
        ("error_code", generate::<ErrorCode>()),
        ("public_error", generate::<PublicError>()),
        ("protocol_error", generate::<ProtocolError>()),
        ("reply_error", generate::<ReplyError>()),
        ("vm_config", generate::<VmConfig>()),
        ("vm_record", generate::<VmRecord>()),
        ("media_record", generate::<MediaRecord>()),
        ("baseline_record", generate::<BaselineRecord>()),
        ("run_record", generate::<RunRecord>()),
        ("operation_record", generate::<OperationRecord>()),
        ("audit_event", generate::<AuditEvent>()),
        ("resource_limits", generate::<ResourceLimits>()),
        ("host_capacity", generate::<HostCapacity>()),
        ("display_geometry", generate::<DisplayGeometry>()),
        ("screenshot", generate::<Screenshot>()),
        ("input_event", generate::<InputEvent>()),
        ("input_batch", generate::<InputBatch>()),
        ("capability", generate::<Capability>()),
        ("quotas", generate::<Quotas>()),
        ("policy_layer", generate::<PolicyLayer>()),
        ("effective_policy", generate::<EffectivePolicy>()),
        ("grant", generate::<Grant>()),
        ("lease", generate::<Lease>()),
        ("session_state", generate::<SessionState>()),
        ("termination_cause", generate::<TerminationCause>()),
        ("test_outcome", generate::<TestOutcome>()),
        ("revocation_report", generate::<RevocationReport>()),
        ("caller", generate::<Caller>()),
        ("vm_request", generate::<VmRequest>()),
        ("admission", generate::<Admission>()),
    ]
    .into_iter()
    .chain(ipc::schemas())
    .collect()
}

/// Serializzazione canonica di uno schema (JSON indentato, `\n` finale).
#[must_use]
pub fn render(schema: &Schema) -> String {
    let mut s = serde_json::to_string_pretty(schema).unwrap_or_default();
    s.push('\n');
    s
}
