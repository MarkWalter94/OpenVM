//! Product-owned medium, channel endpoints and changing session scopes.
use lightvm_proto::{Capability, RunId, ToolGrant, ToolShare, ToolsConfig, VmId};
use lightvm_service::{
    test_support::temp_dir,
    tools::{Bootstrap, Policy, ToolsRun},
};
use lightvm_tools::{
    protocol::{FileReply, FileRequest, Hello, RelativePath},
    server::FileClient,
};

#[test]
fn bootstrap_hides_host_paths_and_scope_changes_close_existing_channels() {
    let base = std::fs::canonicalize(temp_dir("tools-product")).unwrap();
    for dir in ["private", "journal", "share"] {
        std::fs::create_dir(base.join(dir)).unwrap();
    }
    let share = base.join("share");
    std::fs::write(share.join("fixture.txt"), b"guest-visible").unwrap();
    let config = ToolsConfig {
        enabled: true,
        shares: vec![ToolShare {
            name: "Builds".into(),
            root: share.to_string_lossy().into_owned(),
            writable: false,
        }],
        clipboard: true,
        ..Default::default()
    };
    let vm = VmId::from_unix_millis(1, [1; 10]);
    let run = RunId::from_unix_millis(1, [2; 10]);
    let mut tools = ToolsRun::start(
        vm,
        run,
        &config,
        &base.join("private"),
        &base.join("journal"),
        &[
            ("TOOLS.EXE", b"test-only".to_vec()),
            ("TOOLS.PY", b"test-only".to_vec()),
        ],
    )
    .unwrap();
    let iso = std::fs::read(base.join("private/tools.iso")).unwrap();
    let files = lightvm_fixture::iso9660::read_root(&iso).unwrap();
    let cfg = &files
        .iter()
        .find(|(name, _)| name == "TOOLS.CFG")
        .unwrap()
        .1;
    assert!(!String::from_utf8_lossy(cfg).contains(share.to_str().unwrap()));
    let bootstrap: Bootstrap = serde_json::from_slice(cfg).unwrap();
    let hello = Hello {
        version: bootstrap.version,
        vm: bootstrap.vm,
        run: bootstrap.run,
        credential: bootstrap.credential,
    };
    assert_eq!(
        tools.endpoints()[0].virtual_ip,
        lightvm_service::tools::VIRTUAL_IP
    );
    let read = || FileRequest::Read {
        share: "Builds".into(),
        path: RelativePath::new(vec!["fixture.txt".into()]).unwrap(),
        offset: 0,
        length: 1024,
    };
    let mut client = FileClient::connect(tools.files.address(), &hello).unwrap();
    assert!(
        matches!(client.call(&read()).unwrap(),FileReply::Error { code } if code=="permission_denied")
    );
    tools.apply(Policy::manual(&config));
    assert!(client.call(&read()).is_err());
    let mut client = FileClient::connect(tools.files.address(), &hello).unwrap();
    assert!(matches!(
        client.call(&read()).unwrap(),
        FileReply::Data { .. }
    ));
    tools.apply(Policy {
        capabilities: [Capability::Clipboard].into(),
        grant: ToolGrant::default(),
        expires: None,
    });
    assert!(client.call(&read()).is_err());
    let mut client = FileClient::connect(tools.files.address(), &hello).unwrap();
    assert!(
        matches!(client.call(&read()).unwrap(),FileReply::Error { code } if code=="permission_denied")
    );
    drop(client);
    drop(tools);
    std::fs::remove_dir_all(base).unwrap();
}
