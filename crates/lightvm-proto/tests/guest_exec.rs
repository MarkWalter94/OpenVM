use lightvm_proto::{Capability, ToolCall, ToolGrant, ToolReply, ToolsConfig};

#[test]
fn execution_is_disabled_by_default_and_requires_a_separate_vm_setting() {
    let capabilities = [Capability::GuestExec].into();
    let mut config = ToolsConfig {
        enabled: true,
        clipboard: true,
        files_transfer: true,
        ..Default::default()
    };
    assert!(
        config
            .validate_grant(&capabilities, &ToolGrant::default())
            .is_err()
    );
    config.guest_exec = true;
    config
        .validate_grant(&capabilities, &ToolGrant::default())
        .unwrap();
    assert!(Capability::GuestExec.is_mutation());
    assert!(Capability::GuestExec.is_integration());
    assert_eq!(Capability::GuestExec.as_str(), "guest.exec");
}

#[test]
fn execution_replies_are_bounded_and_tied_to_the_requested_job() {
    let reply = ToolReply::Exec {
        job: 10,
        running: false,
        exit_code: Some(7),
        stdout: "hello".into(),
        stderr: "error".into(),
        truncated: false,
        timed_out: false,
        cancelled: false,
    };
    reply.validate_for(&ToolCall::ExecPoll { job: 10 }).unwrap();
    assert!(reply.validate_for(&ToolCall::ExecPoll { job: 11 }).is_err());
    assert!(reply.validate_for(&ToolCall::ClipboardRead).is_err());
    let mut bad = reply.clone();
    if let ToolReply::Exec { stdout, .. } = &mut bad {
        *stdout = "x".repeat(6145);
    }
    assert!(bad.validate().is_err());
    let mut bad = reply;
    if let ToolReply::Exec { running, .. } = &mut bad {
        *running = true;
    }
    assert!(bad.validate().is_err());
}
