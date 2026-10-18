//! Windows PE's native resolver and WinHTTP through the production stack.
use super::*;
use std::net::{Ipv4Addr, ToSocketAddrs};

#[test]
#[ignore = "approved Windows ISO/bundle on E: and compiled filtered_winpe.c probe required"]
fn winpe_filtered_https_and_forbidden_destinations() {
    let env = |name: &str| PathBuf::from(std::env::var_os(name).expect(name));
    let lab = env("LIGHTVM_LAB");
    let bundle = env("LIGHTVM_WIN11_BUNDLE");
    let base = env("LIGHTVM_WINDOWS_NETWORK_ROOT");
    assert!(!base.exists(), "use a new dedicated test directory");
    for name in ["run", "p", "tpm", "journal", "probe"] {
        std::fs::create_dir_all(base.join(name)).unwrap();
    }
    let vm = VmId::new_v7();
    let run = RunId::new_v7();
    let mut setup = QemuSetup::new(
        lab.join("qemu/qemu-11.1.0"),
        base.clone(),
        base.join("probe"),
    );
    setup.win11_dir = Some(bundle.clone());
    setup
        .probe
        .approved_win11_bundle_sha256
        .push(qemu_backend::probe::sha256_file(&bundle.join("SHA256SUMS")).unwrap());
    let (_host, backend, _) = probe(setup).unwrap();
    let (mut caps, _guard) = backend.windows11_capabilities().unwrap();
    let profile = DeviceProfile::for_guest(GuestOs::Windows11X64, Firmware::Uefi).unwrap();
    let machine = caps
        .recommended_machine(profile.machine_family())
        .unwrap()
        .clone();
    let cpu = CpuModel::nominal("Skylake-Client-v4").unwrap();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let private = ManagedPath::verify(base.join("probe")).unwrap();
    assert!(
        rt.block_on(qemu_backend::probe::probe_accelerator_for(
            &mut caps,
            Accelerator::Whpx,
            &profile,
            &machine,
            &cpu,
            &private,
            &Default::default()
        ))
        .is_available()
    );
    assert!(
        std::process::Command::new(caps.qemu_img.as_path())
            .args(["create", "-f", "qcow2"])
            .arg(base.join("disk.qcow2"))
            .arg("64G")
            .status()
            .unwrap()
            .success()
    );
    std::fs::copy(
        bundle.join("firmware/lightvm-x64-vars-win.fd"),
        base.join("vars.fd"),
    )
    .unwrap();
    let paths = Paths {
        base: base.clone(),
        bundle,
        iso: env("LIGHTVM_WINPE_ISO"),
        id: MediaId::new_v7(),
        restored: Default::default(),
    };
    for name in ["run", "p", "tpm", "journal"] {
        qemu_backend::prepare::restrict_to_current_user(&paths.path(name).unwrap()).unwrap();
    }
    let config = ToolsConfig {
        enabled: true,
        files_transfer: true,
        ..Default::default()
    };
    let fixture = env("LIGHTVM_WINPE_FIXTURE");
    let nonce = run.to_string();
    let script = format!(
        "@echo off\r\nwpeutil InitializeNetwork\r\nreg add \"HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Shell Folders\" /v Personal /t REG_SZ /d X:\\LightVMGuestDocs /f\r\nreg add \"HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\User Shell Folders\" /v Personal /t REG_EXPAND_SZ /d X:\\LightVMGuestDocs /f\r\nmkdir \"X:\\LightVMGuestDocs\\LightVM Tools\\outbox\"\r\n\"%~dp0NETTEST.EXE\" {nonce} >\"X:\\LightVMGuestDocs\\LightVM Tools\\outbox\\network.log\" 2>&1\r\nstart \"\" /b \"%~dp0TOOLS.EXE\"\r\nexit /b 0\r\n"
    );
    let mut tools = ToolsRun::start(
        vm,
        run,
        &config,
        &base.join("p"),
        &base.join("journal"),
        &[
            (
                "TOOLS.EXE",
                std::fs::read(fixture.join("TOOLS64.EXE")).unwrap(),
            ),
            (
                "NETTEST.EXE",
                std::fs::read(env("LIGHTVM_WINPE_NETWORK_PROBE")).unwrap(),
            ),
            (
                "AUTOUNATTEND.XML",
                include_bytes!("winpe-unattend.xml").to_vec(),
            ),
            ("PROBE.CMD", script.into_bytes()),
        ],
    )
    .unwrap();
    tools.apply(Policy::manual(&config));
    let spec = VmLaunchSpec {
        run_id: run,
        machine,
        profile,
        accelerator: Accelerator::Whpx,
        cpu,
        vcpus: 2,
        cores_per_socket: None,
        memory_mib: 4096,
        disk: Some(DiskSpec {
            format: DiskFormat::Qcow2,
        }),
        install_media: Some(paths.id),
        data_media: None,
        limits: Default::default(),
        network: GuestNetwork::Supervisor,
    };
    let answers = ("example.com", 443)
        .to_socket_addrs()
        .unwrap()
        .filter(|s| s.is_ipv4())
        .collect::<Vec<_>>();
    assert!(!answers.is_empty());
    for address in &answers {
        guest_net::egress::validate_target(*address).unwrap();
    }
    let target = answers[0];
    rt.block_on(async {
        let guest = qemu_backend::QemuRun::start(&caps, &spec, &paths, Default::default())
            .await
            .unwrap();
        let socket =
            socket2::Socket::new(socket2::Domain::UNIX, socket2::Type::STREAM, None).unwrap();
        socket
            .connect(&socket2::SockAddr::unix(guest.net_socket().unwrap()).unwrap())
            .unwrap();
        let mut endpoints = tools.endpoints().to_vec();
        endpoints.push(guest_net::EndpointSpec {
            name: "example.com".into(),
            virtual_ip: Ipv4Addr::new(10, 0, 2, 101),
            port: 443,
            target,
        });
        let mut net = NetConfig::new(endpoints);
        net.public_endpoints.insert("example.com".into());
        let allowed = ToolsRun::allowed_endpoints()
            .into_iter()
            .chain(["example.com".into()]);
        let mut stack =
            NetStack::start(net, guest_net::transport::attach(socket).unwrap(), allowed).unwrap();
        let boot = tokio::spawn(guest.boot_prompt_keys());
        let started = Instant::now();
        while !tools.control.connected() {
            assert!(
                started.elapsed() < Duration::from_secs(240),
                "Windows PE Tools timeout"
            );
            if let Ok(shot) = guest.screenshot().await {
                std::fs::write(base.join("guest.png"), shot.png).unwrap();
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        let response = tools
            .control
            .call(lightvm_tools::control::Command::File {
                request: lightvm_tools::protocol::FileRequest::Read {
                    share: "outbox".into(),
                    path: lightvm_tools::protocol::RelativePath::new(vec!["network.log".into()])
                        .unwrap(),
                    offset: 0,
                    length: 16384,
                },
            })
            .unwrap();
        let text = match response {
            lightvm_proto::ToolReply::File {
                reply: lightvm_tools::protocol::FileReply::Data { data, .. },
            } => String::from_utf8(lightvm_tools::protocol::decode(&data, 16384).unwrap()).unwrap(),
            other => panic!("{other:?}"),
        };
        std::fs::write(base.join("network.log"), &text).unwrap();
        println!("{}\n{text}", base.display());
        let closed = guest
            .shutdown(qemu_backend::run::ShutdownPolicy {
                powerdown_timeout: Duration::ZERO,
                quit_timeout: Duration::from_secs(5),
                kill_timeout: Duration::from_secs(10),
            })
            .await
            .unwrap();
        assert_eq!(closed.active_after, 0);
        stack.stop();
        boot.abort();
        std::fs::write(
            base.join("network-records.txt"),
            format!("{:#?}", stack.take_records()),
        )
        .unwrap();
        assert!(text.contains(&format!("nonce={nonce}")));
        assert!(
            text.lines().any(|line| line.trim() == "passed=1"),
            "WinPE network probe failed: {text}"
        );
    });
    drop(tools);
}
