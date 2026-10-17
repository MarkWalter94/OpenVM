//! Actual packaged supervisor termination, owned QEMU containment and restart.
use super::*;
use lightvm_proto::NetworkEndpoint;
use std::os::windows::process::CommandExt;
use sysinfo::{Pid, ProcessesToUpdate, System};

fn tcp_rows(pid: u32) -> Vec<Vec<String>> {
    let output = std::process::Command::new("netstat.exe")
        .args(["-ano", "-p", "tcp"])
        .creation_flags(0x0800_0000)
        .output()
        .unwrap();
    assert!(output.status.success());
    // Keep only our supervisor's connections, never other host applications.
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| {
            line.split_whitespace()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .filter(|row| row.len() == 5 && row[4] == pid.to_string())
        .collect()
}

fn children(pid: u32) -> Vec<u32> {
    System::new_all()
        .processes()
        .values()
        .filter(|p| {
            p.parent() == Some(Pid::from_u32(pid))
                && p.name().to_string_lossy().starts_with("qemu-system")
        })
        .map(|p| p.pid().as_u32())
        .collect()
}

fn alive(pids: &[u32]) -> bool {
    let ids = pids.iter().copied().map(Pid::from_u32).collect::<Vec<_>>();
    let mut system = System::new();
    system.refresh_processes(ProcessesToUpdate::Some(&ids), true);
    ids.iter().any(|pid| system.process(*pid).is_some())
}

async fn network(ui: &mut Client, vm: VmId, run: RunId, root: &Path, tag: &str, hold: bool) {
    let nonce = RunId::new_v7().to_string();
    let app = include_str!("filtered_crash_probe.py")
        .replace("NONCE_PLACEHOLDER", &nonce)
        .replace("HOLD_PLACEHOLDER", if hold { "True" } else { "False" });
    put(ui, vm, run, "application.py", app.as_bytes()).await;
    let build_nonce = command(ui, vm, run, "build", tag).await;
    let result = json_until(ui, vm, run, "network-connected.json", "nonce", &nonce, true).await;
    assert_eq!(result["dns"], "10.0.2.101");
    assert_eq!(result["blocked_attempts"], 3);
    assert!(result["http"].as_str().unwrap().contains(" 200 "));
    std::fs::write(
        root.join(format!("{tag}-network.json")),
        serde_json::to_vec_pretty(&result).unwrap(),
    )
    .unwrap();
    if !hold {
        let build = json_until(ui, vm, run, "result.json", "nonce", &build_nonce, true).await;
        let artifact = read(ui, vm, run, "application.pyz").await.unwrap();
        assert_eq!(
            build["sha256"],
            lightvm_tools::protocol::encode(&Sha256::digest(&artifact))
        );
        std::fs::write(root.join("recovered-application.pyz"), artifact).unwrap();
        std::fs::write(
            root.join("recovered-build.log"),
            read(ui, vm, run, "build.log").await.unwrap(),
        )
        .unwrap();
    }
}

#[test]
#[ignore = "real Windows/WHPX, prepared installed Ubuntu fixture, packaged service and Internet"]
fn packaged_filtered_supervisor_crash_and_recovery() {
    let fixture = PathBuf::from(std::env::var_os("LIGHTVM_INSTALLED_LINUX_FIXTURE").unwrap());
    let executable = PathBuf::from(std::env::var_os("LIGHTVM_INSTALLED_SERVICE_EXE").unwrap());
    let root = PathBuf::from(std::env::var_os("LIGHTVM_FILTERED_CRASH_ROOT").unwrap());
    assert!(!root.exists(), "use a fresh, dedicated test root");
    std::fs::create_dir_all(&root).unwrap();
    let source_sha = hash_file(&fixture.join("ubuntu.vmdk"));
    println!("Filtered crash evidence: {}", root.display());
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap()
        .block_on(scenario(&fixture, &executable, &root));
    assert_eq!(hash_file(&fixture.join("ubuntu.vmdk")), source_sha);
    std::fs::write(
        root.join("fixture-sha256.txt"),
        lightvm_tools::protocol::encode(&source_sha),
    )
    .unwrap();
}

async fn scenario(fixture: &Path, executable: &Path, root: &Path) {
    let (mut service, endpoint, credential) = ExternalService::start(executable, root);
    let mut ui = Client::connect(&endpoint).await.unwrap();
    ui.hello(&credential).await.unwrap();
    let seed = import(&mut ui, &fixture.join("seed.iso"), MediaKind::DataIso).await;
    let mut vm = match ok(
        &mut ui,
        Request::ImportVmware {
            vmx_path: fixture.join("ubuntu.vmx").to_str().unwrap().into(),
            name: VmName::parse("Filtered crash qualification").unwrap(),
            display: DisplayAdapter::Standard,
        },
    )
    .await
    {
        ResponseBody::VmwareImported { vm } => vm,
        _ => panic!("import failed"),
    };
    vm.config.network = NetworkProfile::FilteredInternet;
    vm.config.endpoints = vec![NetworkEndpoint {
        name: "example.com".into(),
        host: "example.com".into(),
        port: 443,
        virtual_ip: std::net::Ipv4Addr::new(10, 0, 2, 101),
    }];
    vm.config.data_media = Some(seed.id);
    vm.config.tools.enabled = true;
    vm.config.tools.files_transfer = true;
    let id = vm.id;
    ok(
        &mut ui,
        Request::UpdateVm {
            vm_id: id,
            expected_revision: vm.config_revision,
            config: vm.config,
        },
    )
    .await;
    // Complete the NoCloud installation and persist its new systemd units
    // before injecting power loss. This qualifies an installed system,
    // not crash durability of the fixture's first-boot file creation.
    let (setup_run, setup_boot) = boot(&mut ui, id, "", root, "setup").await;
    poweroff(&mut ui, id, setup_run).await;
    let (run, first_boot) = boot(&mut ui, id, &setup_boot, root, "first").await;
    network(&mut ui, id, run, root, "before-crash", true).await;
    let pid = service.0.id();
    let qemu = children(pid);
    assert!(!qemu.is_empty(), "owned QEMU child must exist before crash");
    let connections = tcp_rows(pid);
    assert!(
        connections
            .iter()
            .any(|r| r[2].ends_with(":443") && r[3] == "ESTABLISHED"),
        "persistent real egress must exist before crash"
    );
    let started = Instant::now();
    service.0.kill().unwrap();
    service.0.wait().unwrap();
    while alive(&qemu) {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "owned QEMU survived supervisor"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let elapsed = started.elapsed().as_millis();
    assert!(!tcp_rows(pid).iter().any(|r| r[3] == "ESTABLISHED"));
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(2), Client::connect(&endpoint)).await,
        Ok(Err(_))
    ));
    std::fs::write(root.join("crash.json"), serde_json::to_vec_pretty(&serde_json::json!({"supervisor_pid":pid,"owned_qemu_pids":qemu,"connections_before":connections,"qemu_gone_ms":elapsed,"old_ipc_closed":true,"old_egress_closed":true})).unwrap()).unwrap();
    drop(ui);
    drop(service);
    for name in ["packaged-service.stderr.log", "packaged-service.json"] {
        std::fs::rename(root.join(name), root.join(format!("crashed-{name}"))).unwrap();
    }
    let (mut service, endpoint, renewed) = ExternalService::start(executable, root);
    let mut stale = Client::connect(&endpoint).await.unwrap();
    assert!(
        matches!(stale.hello(&credential).await.unwrap(),
            ServerMessage::Error { error: ReplyError::Operational(error), .. }
                if error.code == ErrorCode::Unauthenticated),
        "old credential accepted after restart"
    );
    drop(stale);
    let mut ui = Client::connect(&endpoint).await.unwrap();
    assert!(matches!(
        ui.hello(&renewed).await.unwrap(),
        ServerMessage::HelloOk { .. }
    ));
    tokio::time::sleep(Duration::from_secs(2)).await;
    match ok(&mut ui, Request::VmStatus { vm_id: id }).await {
        ResponseBody::Vm { vm } => {
            assert_eq!(vm.state, VmState::Stopped);
            assert_eq!(vm.run_id, None);
            assert_eq!(vm.config.network, NetworkProfile::FilteredInternet);
            assert_eq!(vm.config.endpoints.len(), 1);
            assert_eq!(vm.config.endpoints[0].host, "example.com");
            assert_eq!(vm.config.endpoints[0].port, 443);
            std::fs::write(
                root.join("reconciled-vm.json"),
                serde_json::to_vec_pretty(&vm).unwrap(),
            )
            .unwrap();
        }
        _ => panic!("VM status unavailable after service restart"),
    }
    assert!(
        children(service.0.id()).is_empty(),
        "VM auto-started after crash"
    );
    let previous_run = run;
    let (run, _) = boot(&mut ui, id, &first_boot, root, "recovered").await;
    assert_ne!(run, previous_run);
    network(&mut ui, id, run, root, "recovered", false).await;
    poweroff(&mut ui, id, run).await;
    service.shutdown().await;
    std::fs::write(root.join("results.txt"), "packaged_supervisor=passed\nreal_filtered_https_before_crash=passed\nowned_qemu_job_containment=passed\nold_egress_and_ipc_closed=passed\ncredential_rotated=passed\nreconciled_stopped_no_auto_restart=passed\nexplicit_restart_fresh_boot=passed\nfiltered_https_and_denials_after_recovery=passed\nreturned_application_sha256=passed\nguest_poweroff=passed\n").unwrap();
}
