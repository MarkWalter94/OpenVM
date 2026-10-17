//! Actual release service + unprivileged Xfce autostart/GVfs/X11 clipboard.
use super::*;
use lightvm_proto::{ToolReply, ToolShare};

async fn tool(ui: &mut Client, vm: VmId, run: RunId, call: ToolCall) -> ToolReply {
    match ok(
        ui,
        Request::GuestTools {
            vm_id: vm,
            run_id: run,
            call,
        },
    )
    .await
    {
        ResponseBody::GuestTools { reply } => reply,
        _ => panic!("guest Tools response missing"),
    }
}

async fn image(ui: &mut Client, vm: VmId, run: RunId, request: ToolFileRequest) -> ToolFileReply {
    match tool(ui, vm, run, ToolCall::ClipboardImage { request }).await {
        ToolReply::ClipboardImage { reply } => reply,
        _ => panic!("guest clipboard image failed"),
    }
}

#[test]
#[ignore = "prepared Xfce VM, WHPX and an extracted release supervisor required"]
fn installed_xfce_autostart_gvfs_and_clipboard() {
    let lab = PathBuf::from(std::env::var_os("LIGHTVM_DESKTOP_LAB").unwrap());
    let expect_login = std::env::var_os("LIGHTVM_DESKTOP_EXPECT_LOGIN").is_some();
    let fixture = lab.join("fixture");
    let root = lab.join("runs").join(&RunId::new_v7().to_string()[24..]);
    std::fs::create_dir_all(&root).unwrap();
    let shared = root.join("shared");
    std::fs::create_dir(&shared).unwrap();
    let source_sha = hash_file(&fixture.join("ubuntu.vmdk"));
    let executable = PathBuf::from(std::env::var_os("LIGHTVM_INSTALLED_SERVICE_EXE").unwrap());
    let (mut service, endpoint, credential) = ExternalService::start(&executable, &root);
    println!("Desktop Linux evidence: {}", root.display());
    tokio::runtime::Builder::new_multi_thread().worker_threads(4).enable_all().build().unwrap().block_on(async {
        let mut ui = Client::connect(&endpoint).await.unwrap();
        ui.hello(&credential).await.unwrap();
        let seed = import(&mut ui, &fixture.join("prepare.iso"), MediaKind::DataIso).await;
        let mut vm = match ok(&mut ui, Request::ImportVmware {
            vmx_path: fixture.join("ubuntu.vmx").to_str().unwrap().into(),
            name: VmName::parse("Xfce desktop Tools").unwrap(), display: DisplayAdapter::Standard,
        }).await {
            ResponseBody::VmwareImported { vm } => vm,
            _ => panic!("desktop VM import failed"),
        };
        vm.config.network = NetworkProfile::Offline;
        vm.config.data_media = Some(seed.id);
        vm.config.tools.enabled = true;
        vm.config.tools.files_transfer = true;
        vm.config.tools.clipboard = true;
        vm.config.tools.shares = [true, false].into_iter().map(|writable| ToolShare {
            name: if writable { "Builds" } else { "ReadOnly" }.into(),
            root: shared.to_str().unwrap().into(), writable,
        }).collect();
        let id = vm.id;
        ok(&mut ui, Request::UpdateVm { vm_id: id, expected_revision: vm.config_revision, config: vm.config }).await;
        let mut previous_boot = String::new();
        for attempt in 1..=2 {
            ok(&mut ui, Request::SubmitVm { request: VmRequest {
                operation_id: OperationId::new_v7(), vm_id: id, expected_run_id: None, action: VmAction::Start,
            }}).await;
            let run = wait_state(&mut ui, id, VmState::Running, T).await.unwrap();
            json_until(&mut ui, id, run, "ready.json", "boot_id", &previous_boot, false).await;
            let nonce = command(&mut ui, id, run, "status", "").await;
            let status = json_until(&mut ui, id, run, "result.json", "nonce", &nonce, true).await;
            assert_eq!(status["desktop"], "XFCE");
            assert_ne!(status["uid"], 0);
            let processes = status["tools"].as_array().unwrap();
            assert_eq!(processes.len(), 1, "installed Tools must run once");
            assert_eq!(processes[0]["uid"], status["uid"]);
            assert_eq!(processes[0]["args"].as_array().unwrap().len(), 2, "autostart without explicit bootstrap arguments");
            if expect_login {
                assert_eq!(status["session"]["Active"], "yes");
                assert_eq!(status["session"]["Remote"], "no");
                assert_eq!(status["session"]["Type"], "x11");
                assert_eq!(status["session"]["Seat"], "seat0");
                assert!(!status["tools_mounts"].as_array().unwrap().is_empty(), "Tools CD was not mounted by the desktop");
                assert_ne!(status["display"], ":99.0", "unexpected Xvfb session");
            }
            previous_boot = status["boot_id"].as_str().unwrap().to_owned();
            std::fs::write(root.join(format!("desktop-{attempt}.json")), serde_json::to_vec_pretty(&status).unwrap()).unwrap();
            for action in ["gio", "clipboard"] {
                let nonce = command(&mut ui, id, run, action, "").await;
                let result = json_until(&mut ui, id, run, "result.json", "nonce", &nonce, true).await;
                std::fs::write(root.join(format!("{action}-{attempt}.json")), serde_json::to_vec_pretty(&result).unwrap()).unwrap();
                if action == "gio" {
                    assert_eq!(result["gio"], "passed");
                    assert!(std::fs::read_dir(&shared).unwrap().next().is_none(), "GIO cleanup/readonly refusal left host files");
                } else {
                    assert_eq!(result["image"], "passed");
                    let capture = match image(&mut ui, id, run, ToolFileRequest::Begin {
                        replace: false, share: "clipboard-read".into(), path: path("image"), size: 0, sha256: "0".repeat(64),
                    }).await {
                        ToolFileReply::Transfer { transfer } => transfer,
                        _ => panic!("clipboard image capture begin failed"),
                    };
                    let pixels = match image(&mut ui, id, run, ToolFileRequest::Read {
                        share: capture.clone(), path: path("image"), offset: 0, length: 16384,
                    }).await {
                        ToolFileReply::Data { data, eof: true, .. } => lightvm_tools::protocol::decode(&data, 16384).unwrap(),
                        _ => panic!("clipboard image capture read failed"),
                    };
                    assert_eq!(pixels.len(), 12 + 31 * 23 * 4);
                    assert_eq!(result["image_sha256"], lightvm_tools::protocol::encode(&Sha256::digest(&pixels)));
                    assert!(matches!(image(&mut ui, id, run, ToolFileRequest::Cancel { transfer: capture }).await, ToolFileReply::Ok));
                    let text = format!("Host → Xfce clipboard {attempt}: caffè\n");
                    assert!(matches!(tool(&mut ui, id, run, ToolCall::ClipboardWrite { text: text.clone() }).await, ToolReply::Ok));
                    assert!(matches!(tool(&mut ui, id, run, ToolCall::ClipboardRead).await, ToolReply::Clipboard { text: received } if received == text));
                }
            }
            println!("desktop boot {attempt}: unprivileged XDG Tools autostart, GIO WebDAV and X11 text/image passed");
            ok(&mut ui, Request::SubmitVm { request: VmRequest {
                operation_id: OperationId::new_v7(), vm_id: id, expected_run_id: Some(run), action: VmAction::Stop,
            }}).await;
            wait_state(&mut ui, id, VmState::Stopped, T).await;
        }
        service.shutdown().await;
    });
    if expect_login {
        let diagnostic = std::fs::read_to_string(root.join("packaged-service.stderr.log")).unwrap();
        assert!(
            !diagnostic.contains("phase Quit") && !diagnostic.contains("phase Kill"),
            "desktop stop used fallback; inspect retained logind/input diagnostics"
        );
        assert_eq!(
            diagnostic
                .matches("phase Powerdown, cause GuestShutdown")
                .count(),
            2,
            "both desktop stops must be clean guest shutdowns"
        );
        std::fs::write(root.join("login-results.txt"), "active_lightdm_xorg_login=passed\ndesktop_cd_automount=passed\nacpi_clean_poweroff_two_boots=passed\n").unwrap();
    }
    assert_eq!(hash_file(&fixture.join("ubuntu.vmdk")), source_sha);
    std::fs::write(root.join("results.txt"), "xfce_autostart_two_boots=passed\ngvfs_mount_and_file_operations=passed\nread_only_refusal=passed\nunicode_filenames=passed\nx11_text_image_clipboard=passed\nactual_release_supervisor=passed\nsource_unchanged=passed\n").unwrap();
}
