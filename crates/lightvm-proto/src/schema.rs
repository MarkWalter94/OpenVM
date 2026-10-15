//! JSON Schemas generated from the single source (ROADMAP 6.3).
//!
//! [`public_schemas`] lists every public type with its file name in
//! `crates/lightvm-proto/schema/`. The `schema_snapshot` test compares the committed
//! files with the generated ones; with `LIGHTVM_UPDATE_SCHEMAS=1` it rewrites them.

use schemars::{JsonSchema, Schema, generate::SchemaSettings};

use crate::*;

fn generate<T: JsonSchema>() -> Schema {
    SchemaSettings::draft2020_12()
        .into_generator()
        .into_root_schema_for::<T>()
}

/// Pairs (file name without extension, schema) for all public types.
/// The order is stable. Internal-only types (`MediaEntry`, `ManagedRelPath`,
/// `FileIdentity`) are excluded on purpose.
#[must_use]
pub fn public_schemas() -> Vec<(&'static str, Schema)> {
    vec![
        ("vm_id", generate::<VmId>()),
        ("run_id", generate::<RunId>()),
        ("operation_id", generate::<OperationId>()),
        ("session_id", generate::<SessionId>()),
        ("baseline_id", generate::<BaselineId>()),
        ("media_id", generate::<MediaId>()),
        ("snapshot_id", generate::<SnapshotId>()),
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
        ("session_report", generate::<report::SessionReport>()),
    ]
    .into_iter()
    .chain(ipc::schemas())
    .collect()
}

/// Canonical serialization of a schema (indented JSON, trailing `\n`).
#[must_use]
pub fn render(schema: &Schema) -> String {
    let mut s = serde_json::to_string_pretty(schema).unwrap_or_default();
    s.push('\n');
    s
}
