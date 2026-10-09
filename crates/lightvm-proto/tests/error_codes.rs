//! Stabilità dei codici d'errore pubblici (ROADMAP 6.3).

use lightvm_proto::{
    ErrorCode, ProtocolError, ProtocolErrorCode, PublicError, ReplyError,
};
use serde_json::json;

/// Elenco congelato: modificare questa tabella è un cambiamento incompatibile.
const FROZEN: [(ErrorCode, &str, bool); 9] = [
    (ErrorCode::PermissionDenied, "permission_denied", false),
    (ErrorCode::SessionExpired, "session_expired", false),
    (ErrorCode::VmBusy, "vm_busy", true),
    (ErrorCode::Unsupported, "unsupported", false),
    (ErrorCode::StateConflict, "state_conflict", false),
    (ErrorCode::ResourceLimit, "resource_limit", false),
    (ErrorCode::InvalidArgument, "invalid_argument", false),
    (ErrorCode::NotFound, "not_found", false),
    (ErrorCode::InfrastructureError, "infrastructure_error", true),
];

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
        assert!(serde_json::from_value::<ErrorCode>(json!(name)).is_ok(), "{name}");
    }
    assert!(serde_json::from_value::<ErrorCode>(json!("PermissionDenied")).is_err());
}

#[test]
fn public_error_wire_shape() {
    let e = PublicError::vm_busy("operazione in corso").with_detail("vm", "x");
    let v = serde_json::to_value(&e).unwrap();
    assert_eq!(
        v,
        json!({"code": "vm_busy", "message": "operazione in corso", "details": {"vm": "x"}, "retryable": true})
    );
    let plain = serde_json::to_value(PublicError::permission_denied("no")).unwrap();
    assert_eq!(plain, json!({"code": "permission_denied", "message": "no", "retryable": false}));
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
    assert_eq!(serde_json::to_value(&op).unwrap()["kind"], json!("operational"));
    assert_eq!(serde_json::to_value(&proto).unwrap()["kind"], json!("protocol"));
    // Un codice operativo non è accettato come codice di protocollo e viceversa.
    assert!(serde_json::from_value::<ProtocolErrorCode>(json!("vm_busy")).is_err());
    assert!(serde_json::from_value::<ErrorCode>(json!("malformed_message")).is_err());
    let names: Vec<_> = ProtocolErrorCode::ALL.iter().map(|c| c.as_str()).collect();
    assert_eq!(
        names,
        ["malformed_message", "unsupported_version", "unknown_method", "message_too_large", "unauthenticated"]
    );
}
