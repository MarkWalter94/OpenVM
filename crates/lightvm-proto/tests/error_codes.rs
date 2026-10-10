//! Stability of the public error codes (ROADMAP 6.3).

use lightvm_proto::{ErrorCode, ProtocolError, ProtocolErrorCode, PublicError, ReplyError};
use serde_json::json;

/// Frozen list: changing or reordering the existing rows is an incompatible
/// change; new codes are only ever appended at the end.
const FROZEN: [(ErrorCode, &str, bool); 10] = [
    (ErrorCode::PermissionDenied, "permission_denied", false),
    (ErrorCode::SessionExpired, "session_expired", false),
    (ErrorCode::VmBusy, "vm_busy", true),
    (ErrorCode::Unsupported, "unsupported", false),
    (ErrorCode::StateConflict, "state_conflict", false),
    (ErrorCode::ResourceLimit, "resource_limit", false),
    (ErrorCode::InvalidArgument, "invalid_argument", false),
    (ErrorCode::NotFound, "not_found", false),
    (ErrorCode::InfrastructureError, "infrastructure_error", true),
    (ErrorCode::Unauthenticated, "unauthenticated", false),
];

/// Codes present in the committed schema `schema/error_code.json`: the frozen
/// table, the Rust type and the schema must match.
#[test]
fn committed_error_code_schema_lists_every_code() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("schema/error_code.json");
    let text = std::fs::read_to_string(path).expect("schema error_code.json");
    let schema: serde_json::Value = serde_json::from_str(&text).unwrap();
    let mut in_schema: Vec<String> = Vec::new();
    collect_consts(&schema, &mut in_schema);
    in_schema.sort();
    let mut expected: Vec<String> = FROZEN.iter().map(|(_, n, _)| (*n).to_owned()).collect();
    expected.sort();
    assert_eq!(in_schema, expected);
}

fn collect_consts(v: &serde_json::Value, out: &mut Vec<String>) {
    match v {
        serde_json::Value::Object(m) => {
            if let Some(serde_json::Value::String(s)) = m.get("const") {
                out.push(s.clone());
            }
            if let Some(serde_json::Value::Array(a)) = m.get("enum") {
                out.extend(a.iter().filter_map(|x| x.as_str().map(str::to_owned)));
            }
            for (k, child) in m {
                if k != "const" && k != "enum" {
                    collect_consts(child, out);
                }
            }
        }
        serde_json::Value::Array(a) => a.iter().for_each(|x| collect_consts(x, out)),
        _ => {}
    }
}

/// Authentication and authorization semantics (documented in the `error`
/// module and in `docs/verification/F1-01-proto-core.md`): three distinct codes,
/// none automatically retryable, and the operational code `unauthenticated`
/// is not a protocol code serialized in the same family.
#[test]
fn authentication_codes_are_distinct_and_not_retryable() {
    let codes = [
        ErrorCode::Unauthenticated,
        ErrorCode::SessionExpired,
        ErrorCode::PermissionDenied,
    ];
    for c in codes {
        assert!(!c.default_retryable(), "{c}");
    }
    let e = PublicError::unauthenticated("invalid credential");
    assert_eq!(
        serde_json::to_value(&e).unwrap(),
        json!({"code": "unauthenticated", "message": "invalid credential", "retryable": false})
    );
    let reply: ReplyError = e.into();
    assert_eq!(
        serde_json::to_value(&reply).unwrap()["kind"],
        json!("operational")
    );
    // The same name also exists among the protocol codes (kept for
    // compatibility, not emitted): the two families stay separated by `kind`.
    assert_eq!(
        ProtocolErrorCode::Unauthenticated.as_str(),
        "unauthenticated"
    );
    let proto: ReplyError = ProtocolError {
        code: ProtocolErrorCode::Unauthenticated,
        message: "x".into(),
    }
    .into();
    assert_eq!(
        serde_json::to_value(&proto).unwrap()["kind"],
        json!("protocol")
    );
}

#[test]
fn error_codes_are_frozen() {
    assert_eq!(ErrorCode::ALL.len(), FROZEN.len());
    for (i, (code, name, retryable)) in FROZEN.iter().enumerate() {
        assert_eq!(ErrorCode::ALL[i], *code);
        assert_eq!(code.as_str(), *name);
        assert_eq!(serde_json::to_value(code).unwrap(), json!(name));
        let back: ErrorCode = serde_json::from_value(json!(name)).unwrap();
        assert_eq!(back, *code);
        assert_eq!(code.default_retryable(), *retryable, "{name}");
    }
}

#[test]
fn roadmap_codes_present() {
    for name in [
        "permission_denied",
        "session_expired",
        "vm_busy",
        "unsupported",
        "resource_limit",
        "state_conflict",
    ] {
        assert!(
            serde_json::from_value::<ErrorCode>(json!(name)).is_ok(),
            "{name}"
        );
    }
    assert!(serde_json::from_value::<ErrorCode>(json!("PermissionDenied")).is_err());
}

#[test]
fn public_error_wire_shape() {
    let e = PublicError::vm_busy("operation in progress").with_detail("vm", "x");
    let v = serde_json::to_value(&e).unwrap();
    assert_eq!(
        v,
        json!({"code": "vm_busy", "message": "operation in progress", "details": {"vm": "x"}, "retryable": true})
    );
    let plain = serde_json::to_value(PublicError::permission_denied("no")).unwrap();
    assert_eq!(
        plain,
        json!({"code": "permission_denied", "message": "no", "retryable": false})
    );
    let back: PublicError = serde_json::from_value(v).unwrap();
    assert_eq!(back, e);
}

#[test]
fn operational_and_protocol_errors_are_distinct() {
    let op: ReplyError = PublicError::not_found("vm").into();
    let proto: ReplyError = ProtocolError {
        code: ProtocolErrorCode::MalformedMessage,
        message: "json".into(),
    }
    .into();
    assert_eq!(
        serde_json::to_value(&op).unwrap()["kind"],
        json!("operational")
    );
    assert_eq!(
        serde_json::to_value(&proto).unwrap()["kind"],
        json!("protocol")
    );
    // An operational code is not accepted as a protocol code and vice versa.
    assert!(serde_json::from_value::<ProtocolErrorCode>(json!("vm_busy")).is_err());
    assert!(serde_json::from_value::<ErrorCode>(json!("malformed_message")).is_err());
    let names: Vec<_> = ProtocolErrorCode::ALL.iter().map(|c| c.as_str()).collect();
    assert_eq!(
        names,
        [
            "malformed_message",
            "unsupported_version",
            "unknown_method",
            "message_too_large",
            "unauthenticated"
        ]
    );
}
