//! R1 through production IPC, QemuVmBackend, DNS resolution, session grants,
//! guest input and the authenticated Tools channel. No test NIC or shell RPC.
use super::*;
use lightvm_proto::{NetworkEndpoint, NetworkProfile, ToolCall, ToolFileReply, ToolFileRequest};

async fn file(ui: &mut Client, vm: VmId, run: RunId, name: &str) -> Option<Vec<u8>> {
    match req(
        ui,
        Request::GuestTools {
            vm_id: vm,
            run_id: run,
            call: ToolCall::File {
                request: ToolFileRequest::Read {
                    share: "outbox".into(),
                    path: lightvm_proto::ToolRelativePath::new(vec![name.into()]).unwrap(),
                    offset: 0,
                    length: 16384,
                },
            },
        },
    )
    .await
    {
        ServerMessage::Response {
            result:
                ResponseBody::GuestTools {
                    reply:
                        lightvm_proto::ToolReply::File {
                            reply: ToolFileReply::Data { data, .. },
                        },
                },
            ..
        } => Some(lightvm_tools::protocol::decode(&data, 16384).unwrap()),
        _ => None,
    }
}

#[test]
#[ignore = "real Windows/WHPX, Ubuntu live ISO, packaged Tools and Internet access required"]
fn filtered_internet_through_service_and_agent_grant() {
    lightvm_service::tools::ToolsRun::load_binaries().expect("stage the packaged Tools beside the test executable and launch it through its real E: path (no target junction)");
    let lab = PathBuf::from(std::env::var_os("LIGHTVM_LAB").expect("LIGHTVM_LAB must be on E:"));
    assert!(lab.is_absolute());
    let root = lab
        .join("r1-product")
        .join(&RunId::new_v7().to_string()[24..]);
    std::fs::create_dir_all(&root).unwrap();
    let iso = lab.join("media/ubuntu-26.04.1-desktop-amd64.iso");
    let nonce = RunId::new_v7().to_string();
    let probe_iso = root.join("probe.iso");
    std::fs::write(
        &probe_iso,
        lightvm_fixture::iso9660::write_iso(
            "LIGHTVMTEST",
            &[("PROBE.PY", include_bytes!("filtered_probe.py").as_slice())],
        ),
    )
    .unwrap();
    let store = SqliteStore::open(&root.join("catalog")).unwrap();
    let mut setup = QemuSetup::new(qemu_dir(), store.root(), root.join("p"));
    setup.cpu_model = "Skylake-Client-v4".into();
    let (host, backend, capacity) = probe(setup).unwrap();
    assert!(capacity.accelerator_available);
    let audit_path = root.join("audit.jsonl");
    let audit = FileAuditSink::open(&audit_path).unwrap();
    let inspector = ManagedMediaInspector::new(store.root());
    let engine = CoreEngine::new(store, audit, backend, inspector, core_config(capacity)).unwrap();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();
    println!("R1 product evidence: {}", root.display());
    rt.block_on(product_scenario(
        &root, &iso, &probe_iso, &nonce, engine, host,
    ));

    let events = audit_events(&audit_path);
    assert!(events.iter().any(|e| {
        e.action == AuditAction::NetworkSetup
            && e.network
                .as_ref()
                .is_some_and(|n| n.mode.as_deref() == Some("filtered_internet"))
    }));
    assert!(events.iter().any(|e| {
        e.action == AuditAction::NetworkConnection
            && e.network.as_ref().is_some_and(|n| {
                n.endpoint.as_deref() == Some("example.com") && n.bytes_to_guest.unwrap_or(0) > 100
            })
    }));
    assert!(events.iter().any(|e| {
        e.action == AuditAction::NetworkPolicy
            && e.network
                .as_ref()
                .is_some_and(|n| n.reason.as_deref() == Some("allowed: none"))
    }));
    assert!(events.iter().any(|e| {
        e.action == AuditAction::NetworkConnection
            && e.network.as_ref().is_some_and(|n| {
                n.endpoint.as_deref() == Some("example.com")
                    && n.reason.as_deref() == Some("revoked")
            })
    }));
    std::fs::write(root.join("results.txt"),"production_service_and_backend=passed\nagent_endpoint_subset=passed\nreal_guest_tls=passed\nblocked_destinations=passed\nguest_default_route_does_not_bypass=passed\napplication_build_and_log=passed\nartifact_sha256=passed\ntools_file_return=passed\nrevoke_closes_egress=passed\ntest_vm_force_stopped=passed\n").unwrap();
}

async fn product_scenario(
    root: &Path,
    iso: &Path,
    probe_iso: &Path,
    nonce: &str,
    engine: CoreEngine<
        SqliteStore,
        FileAuditSink,
        lightvm_service::adapters::backend::QemuVmBackend,
        ManagedMediaInspector,
    >,
    host: lightvm_service::adapters::backend::QemuHost,
) {
    let mut config = ServiceConfig::new(root.join("catalog"));
    config.runtime_dir = Some(root.join("ipc"));
    config.display_dir = Some(root.join("d"));
    let (handle, credential) = start(config, Box::new(engine), Box::new(host))
        .await
        .unwrap();
    let endpoint = handle.endpoint().to_owned();
    let mut ui = Client::connect(&endpoint).await.unwrap();
    ui.hello(&credential).await.unwrap();
    let media = import(&mut ui, iso, MediaKind::InstallIso).await;
    let data = import(&mut ui, probe_iso, MediaKind::DataIso).await;
    let mut vmc = sample_config("r1-product");
    vmc.vcpus = 2;
    vmc.memory_mib = 6144;
    vmc.disk_gib = 8;
    vmc.install_media = Some(media.id);
    vmc.data_media = Some(data.id);
    vmc.network = NetworkProfile::FilteredInternet;
    vmc.endpoints = [
        ("example.com", 101),
        ("www.iana.org", 102),
        ("localtest.me", 103),
    ]
    .into_iter()
    .map(|(name, last)| NetworkEndpoint {
        name: name.into(),
        host: name.into(),
        port: 443,
        virtual_ip: std::net::Ipv4Addr::new(10, 0, 2, last),
    })
    .collect();
    vmc.tools.enabled = true;
    vmc.tools.files_transfer = true;
    let vm = match ok(&mut ui, Request::CreateVm { config: vmc }).await {
        ResponseBody::VmCreated { vm } => vm.id,
        other => panic!("{other:?}"),
    };
    ok(&mut ui, Request::SetAgentAccess { enabled: true }).await;
    let mut grant = approval(vm, true);
    grant.capabilities.insert(Capability::NetworkEndpoints);
    grant.capabilities.insert(Capability::FilesTransfer);
    grant.network_endpoints = vec!["example.com".into(), "localtest.me".into()];
    let mut agent = new_agent(&endpoint, &mut ui, grant).await;
    let mut starter_ui = Client::connect(&endpoint).await.unwrap();
    starter_ui.hello(&credential).await.unwrap();
    let starter = tokio::spawn(async move {
        ok(
            &mut starter_ui,
            Request::SubmitVm {
                request: VmRequest {
                    operation_id: OperationId::new_v7(),
                    vm_id: vm,
                    expected_run_id: None,
                    action: VmAction::Start,
                },
            },
        )
        .await
    });
    let started = Instant::now();
    while !starter.is_finished() {
        assert!(started.elapsed() < Duration::from_secs(600));
        agent.sleep(Duration::from_secs(1)).await;
    }
    assert!(matches!(
        starter.await.unwrap(),
        ResponseBody::Operation { .. }
    ));
    let run = loop {
        let (state, run) = vm_state(&mut ui, vm).await;
        if state == VmState::Running {
            break run.unwrap();
        }
        assert_ne!(
            state,
            VmState::Failed,
            "product start failed; inspect the audit and run logs in {}",
            root.display()
        );
        assert!(
            started.elapsed() < Duration::from_secs(600),
            "start: {state:?}"
        );
        agent.sleep(Duration::from_secs(1)).await;
    };
    // Six GiB avoids the previous two-GiB live-desktop qualification
    // limit. Use a VT so the test doesn't depend on GNOME/installer UI.
    agent.sleep(Duration::from_secs(100)).await;
    let command = format!(
        "sudo mkdir -p /mnt/LIGHTVM_TOOLS /mnt/LIGHTVMTEST; sudo mount -r -L LIGHTVM_TOOLS /mnt/LIGHTVM_TOOLS; sudo mount -r -L LIGHTVMTEST /mnt/LIGHTVMTEST; python3 $(find /mnt/LIGHTVM_TOOLS -iname tools.py) --config $(find /mnt/LIGHTVM_TOOLS -iname tools.cfg) >/tmp/lightvm-tools.log 2>&1 & python3 $(find /mnt/LIGHTVMTEST -iname probe.py) {nonce}\n"
    );
    let mut result = None;
    for attempt in 0..3 {
        agent
            .keys(
                vm,
                run,
                vec![
                    key("ctrl", true),
                    key("alt", true),
                    key("f3", true),
                    key("f3", false),
                    key("alt", false),
                    key("ctrl", false),
                ],
            )
            .await;
        agent.sleep(Duration::from_secs(8)).await;
        agent.type_text(vm, run, "ubuntu\n").await;
        agent.sleep(Duration::from_secs(8)).await;
        agent.type_text(vm, run, &command).await;
        let waiting = Instant::now();
        while waiting.elapsed() < Duration::from_secs(100) {
            agent.sleep(Duration::from_secs(2)).await;
            if let Some(bytes) = file(&mut ui, vm, run, "filtered.json").await {
                result = Some(bytes);
                break;
            }
        }
        let (_, _, _, png) = agent.screenshot(vm, run).await;
        std::fs::write(root.join(format!("guest-{attempt}.png")), png).unwrap();
        if result.is_some() {
            break;
        }
    }
    let bytes = result.expect("guest probe did not return through production Tools");
    std::fs::write(root.join("guest-result.json"), &bytes).unwrap();
    let result: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(result["nonce"], nonce);
    assert_eq!(result["default_route"], "changed", "{result}");
    assert_eq!(result["build"], "passed", "{result}");
    let artifact = file(&mut ui, vm, run, "application.pyz")
        .await
        .expect("built application returned through Tools");
    let build_log = file(&mut ui, vm, run, "build.log")
        .await
        .expect("build log returned through Tools");
    use sha2::Digest;
    let artifact_hash = lightvm_tools::protocol::encode(&sha2::Sha256::digest(&artifact));
    assert_eq!(result["artifact_sha256"], artifact_hash);
    assert!(String::from_utf8_lossy(&build_log).contains(&format!("LightVM application {nonce}")));
    std::fs::write(root.join("application.pyz"), artifact).unwrap();
    std::fs::write(root.join("build.log"), build_log).unwrap();
    assert_eq!(result["allowed_dns"], "10.0.2.101");
    assert!(
        result["https"].as_str().unwrap_or_default().contains("200"),
        "{result}"
    );
    for item in [
        "dns_www.iana.org",
        "dns_localtest.me",
        "dns_unlisted.invalid",
        "other_port",
        "ungranted_ip",
        "internet_ip",
        "host",
        "metadata",
        "ipv6",
        "external_dns",
    ] {
        assert_eq!(result[item], "blocked", "{item}: {result}");
    }
    let mut persistent = false;
    for _ in 0..20 {
        if file(&mut ui, vm, run, "persistent.txt").await.is_some() {
            persistent = true;
            break;
        }
        agent.sleep(Duration::from_secs(1)).await;
    }
    assert!(persistent, "persistent guest TLS connection was not opened");
    ok(
        &mut ui,
        Request::RevokeSession {
            session_id: agent.auth.session_id,
        },
    )
    .await;
    // Explicit revocation withdraws egress without implicitly stopping a
    // VM started by its user. Observe the network closure before cleanup.
    let revoked = Instant::now();
    while !audit_events(&root.join("audit.jsonl")).iter().any(|e| {
        e.action == AuditAction::NetworkConnection
            && e.network.as_ref().is_some_and(|n| {
                n.endpoint.as_deref() == Some("example.com")
                    && n.reason.as_deref() == Some("revoked")
            })
    }) {
        assert!(
            revoked.elapsed() < Duration::from_secs(15),
            "persistent Internet connection was not revoked"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    ok(
        &mut ui,
        Request::SubmitVm {
            request: VmRequest {
                operation_id: OperationId::new_v7(),
                vm_id: vm,
                expected_run_id: Some(run),
                action: VmAction::ForceStop,
            },
        },
    )
    .await;
    wait_state(&mut ui, vm, VmState::Stopped, T).await;
    handle.shutdown().await;
}
