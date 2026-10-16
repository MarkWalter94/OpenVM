mod common;
use common::*;
use vm_core::{proto::*, session::SessionSpec};

fn configured(h: &mut Harness) -> VmId {
    let mut config = h.vm_config("tools", None);
    config.tools = ToolsConfig {
        enabled: true,
        clipboard: true,
        files_transfer: true,
        ui_inspect: true,
        ui_act: true,
        shares: vec![ToolShare {
            name: "Builds".into(),
            root: std::env::temp_dir().to_string_lossy().into_owned(),
            writable: true,
        }],
        ..Default::default()
    };
    h.core.create_vm(&Caller::User, config).unwrap().id
}
fn request(h: &Harness, vm: VmId, caps: &[Capability]) -> SessionSpec {
    SessionSpec {
        vm_allowlist: [vm].into(),
        capabilities: caps.iter().copied().collect(),
        tools: Default::default(),
        quotas: Quotas {
            max_operations: 2,
            ..Quotas::UNBOUNDED
        },
        network_endpoints: Default::default(),
        expires_at: UtcTimestamp::from_unix_millis(h.clock.0.get() + 3600000).unwrap(),
        lease_millis: 10000,
    }
}
#[test]
fn named_scopes_cannot_exceed_vm_settings() {
    let mut h = Harness::new();
    let vm = configured(&mut h);
    let mut spec = request(&h, vm, &[Capability::ShareRead]);
    spec.tools.shares.insert("Builds".into(), true);
    assert!(
        h.core
            .create_session(spec.clone(), SessionToken::from_bytes([1; 32]))
            .is_err()
    );
    spec.capabilities.insert(Capability::ShareWrite);
    spec.tools.shares.insert("Other".into(), false);
    assert!(
        h.core
            .create_session(spec.clone(), SessionToken::from_bytes([1; 32]))
            .is_err()
    );
    spec.tools.shares.remove("Other");
    h.core
        .create_session(spec, SessionToken::from_bytes([1; 32]))
        .unwrap();
    let (caps, grant, _) = h.core.tools_permissions(vm, true);
    assert_eq!(grant.shares, [("Builds".into(), true)].into());
    assert!(!caps.contains(&Capability::Clipboard));
}
#[test]
fn calls_require_current_run_grant_input_owner_and_quota() {
    let mut h = Harness::new();
    let vm = configured(&mut h);
    let run = h.start(vm);
    let spec = request(
        &h,
        vm,
        &[
            Capability::Clipboard,
            Capability::UiInspect,
            Capability::UiAct,
            Capability::VmInput,
        ],
    );
    let (grant, _) = h
        .core
        .create_session(spec, SessionToken::from_bytes([1; 32]))
        .unwrap();
    let caller = Caller::Agent {
        session_id: grant.session_id,
        token: Some(SessionToken::from_bytes([1; 32])),
    };
    assert!(
        h.core
            .authorize_tools(
                &caller,
                vm,
                RunId::from_unix_millis(1, [0xfe; 10]),
                Capability::Clipboard
            )
            .is_err()
    );
    assert_eq!(
        h.core
            .authorize_tools(&caller, vm, run, Capability::UiAct)
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
    h.core
        .set_input_owner(
            Actor::User,
            vm,
            ipc::InputOwner::Agent {
                session_id: grant.session_id,
            },
        )
        .unwrap();
    h.core
        .admit_tools(&caller, vm, run, Capability::UiAct)
        .unwrap();
    h.core
        .admit_tools(&caller, vm, run, Capability::Clipboard)
        .unwrap();
    assert_eq!(
        h.core
            .admit_tools(&caller, vm, run, Capability::Clipboard)
            .unwrap_err()
            .code,
        ErrorCode::ResourceLimit
    );
    h.core.pause_session(grant.session_id).unwrap();
    assert!(h.core.tools_permissions(vm, false).0.is_empty());
    assert!(
        h.core
            .authorize_tools(&caller, vm, run, Capability::Clipboard)
            .is_err()
    );
}
#[test]
fn manual_scope_disappears_during_a_session_and_does_not_override_pausing() {
    let mut h = Harness::new();
    let vm = configured(&mut h);
    assert!(
        h.core
            .tools_permissions(vm, true)
            .0
            .contains(&Capability::Clipboard)
    );
    let spec = request(&h, vm, &[Capability::VmList]);
    let (grant, _) = h
        .core
        .create_session(spec, SessionToken::from_bytes([1; 32]))
        .unwrap();
    assert!(h.core.tools_permissions(vm, true).0.is_empty());
    h.core.pause_session(grant.session_id).unwrap();
    assert!(h.core.tools_permissions(vm, true).0.is_empty());
}
