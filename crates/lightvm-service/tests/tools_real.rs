//! Real Ubuntu Tools over the production isolated network and run-owned
//! channels. Explicitly ignored: requires QEMU/WHPX and the approved live ISO.
//! No Internet, host port forwarding, guest shell RPC or source disk writes.
#![cfg(windows)]

include!("common/tools_guest.rs");

use guest_net::{NetConfig, NetStack};
use lightvm_proto::{RunId, ToolShare, ToolsConfig, VmId};
use lightvm_service::tools::{Policy, ToolsRun};
use lightvm_tools::control::Command as ToolsCommand;
use lightvm_tools::protocol::{self, FileReply, FileRequest, RelativePath};
use sha2::{Digest, Sha256};

fn rpc(tools: &ToolsRun, request: FileRequest) -> FileReply {
    match tools
        .control
        .call(ToolsCommand::File { request })
        .expect("guest file RPC")
    {
        lightvm_proto::ToolReply::File { reply } => reply,
        other => panic!("unexpected guest reply: {other:?}"),
    }
}

fn expect_ok(reply: FileReply) {
    assert!(matches!(reply, FileReply::Ok), "{reply:?}");
}

fn image_rpc(tools: &ToolsRun, request: FileRequest) -> FileReply {
    match tools
        .control
        .call(ToolsCommand::ClipboardImage { request })
        .unwrap()
    {
        lightvm_proto::ToolReply::ClipboardImage { reply } => reply,
        other => panic!("unexpected image reply: {other:?}"),
    }
}

struct TestVm(Child);
impl Drop for TestVm {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn capture_winpe(base: &std::path::Path) {
    let transport = rfb_client::Transport::Unix {
        path: base.join("private/vnc.sock"),
        imp: rfb_client::UnixImpl::Socket2,
    };
    if let Ok(mut conn) = rfb_client::Connection::connect(&transport)
        && conn.request_update().is_ok()
        && conn.read_update().is_ok()
    {
        let fb = conn.framebuffer();
        let image = qemu_backend::screenshot::RgbImage {
            width: u32::from(fb.width()),
            height: u32::from(fb.height()),
            data: fb
                .data()
                .chunks(4)
                .flat_map(|p| [p[2], p[1], p[0]])
                .collect(),
        };
        if let Ok(png) = qemu_backend::screenshot::encode_png(&image) {
            let _ = std::fs::write(base.join("winpe.png"), png);
        }
    }
}

#[test]
#[ignore = "Windows x64 setup ISO, WHPX and the explicit disposable WinPE fixture required"]
fn windows_pe_tools_native_clipboard_and_file_channel() {
    let root =
        PathBuf::from(std::env::var_os("LIGHTVM_WINPE_FIXTURE").expect("LIGHTVM_WINPE_FIXTURE"));
    let iso = PathBuf::from(std::env::var_os("LIGHTVM_WINPE_ISO").expect("LIGHTVM_WINPE_ISO"));
    let id = RunId::new_v7();
    let base = lab().join("tools-real").join(format!("winpe-{id}"));
    std::fs::create_dir_all(&base).unwrap();
    for d in ["private", "journal"] {
        std::fs::create_dir(base.join(d)).unwrap();
        qemu_backend::prepare::restrict_to_current_user(
            &qemu_backend::ManagedPath::verify(base.join(d)).unwrap(),
        )
        .unwrap();
    }
    let config = ToolsConfig {
        enabled: true,
        clipboard: true,
        files_transfer: true,
        ..Default::default()
    };
    let mut tools = ToolsRun::start(
        VmId::new_v7(),
        id,
        &config,
        &base.join("private"),
        &base.join("journal"),
        &[
            (
                "TOOLS.EXE",
                std::fs::read(root.join("TOOLS64.EXE")).unwrap(),
            ),
            (
                "CLIPTEST.EXE",
                std::fs::read(root.join("CLIPTEST.EXE")).unwrap(),
            ),
            (
                "PROBE.CMD",
                include_bytes!("common/winpe-probe.cmd").to_vec(),
            ),
        ],
    )
    .unwrap();
    tools.apply(Policy::manual(&config));
    let args = vec![
        "-no-user-config".into(),
        "-nodefaults".into(),
        "-accel".into(),
        "whpx".into(),
        "-machine".into(),
        "q35".into(),
        "-cpu".into(),
        "Skylake-Client-v4".into(),
        "-m".into(),
        "4096".into(),
        "-smp".into(),
        "2".into(),
        "-display".into(),
        "none".into(),
        "-device".into(),
        "VGA".into(),
        "-boot".into(),
        "order=d".into(),
        "-vnc".into(),
        format!("unix:{}", base.join("private/vnc.sock").display()),
        "-drive".into(),
        format!(
            "if=none,id=cd0,media=cdrom,readonly=on,file={}",
            iso.display()
        ),
        "-device".into(),
        "ide-cd,drive=cd0,bus=ide.0".into(),
        "-drive".into(),
        format!(
            "if=none,id=tools,media=cdrom,readonly=on,file={}",
            base.join("private/tools.iso").display()
        ),
        "-device".into(),
        "ide-cd,drive=tools,bus=ide.1".into(),
        "-device".into(),
        "qemu-xhci,id=xhci".into(),
        "-drive".into(),
        format!(
            "if=none,id=answer,format=raw,file={}",
            root.join("answer.img").display()
        ),
        "-device".into(),
        "usb-storage,drive=answer,removable=on".into(),
        "-netdev".into(),
        format!(
            "stream,id=n0,server=on,addr.type=unix,addr.path={}",
            base.join("private/net.sock").display()
        ),
        "-device".into(),
        "e1000,netdev=n0,mac=52:54:00:12:34:57".into(),
    ];
    std::fs::write(base.join("command.txt"), args.join(" ")).unwrap();
    let _vm = TestVm(
        Command::new(lab().join("qemu/qemu-11.1.0/qemu-system-x86_64.exe"))
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(base.join("qemu.log")).unwrap())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(20);
    let socket = loop {
        let socket =
            socket2::Socket::new(socket2::Domain::UNIX, socket2::Type::STREAM, None).unwrap();
        if socket
            .connect(&socket2::SockAddr::unix(base.join("private/net.sock")).unwrap())
            .is_ok()
        {
            break socket;
        }
        assert!(Instant::now() < deadline, "WinPE QEMU network socket");
        std::thread::sleep(Duration::from_millis(100));
    };
    let mut cfg = NetConfig::new(tools.endpoints().into());
    cfg.advertise_dns = false;
    let mut stack = NetStack::start(
        cfg,
        guest_net::transport::attach(socket).unwrap(),
        ToolsRun::allowed_endpoints(),
    )
    .unwrap();
    println!("WinPE lab guest booting; no installation disk or requirements bypass");
    let start = Instant::now();
    let mut next_capture = Duration::from_secs(30);
    while !tools.control.connected() {
        if start.elapsed() >= next_capture {
            capture_winpe(&base);
            println!(
                "WinPE connection pending, {:.0}s; {}",
                start.elapsed().as_secs_f64(),
                base.display()
            );
            next_capture += Duration::from_secs(30);
        }
        assert!(
            start.elapsed() < Duration::from_secs(240),
            "WinPE Tools did not connect; see {}",
            base.display()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    capture_winpe(&base);
    let log = rpc(
        &tools,
        FileRequest::Read {
            share: "outbox".into(),
            path: RelativePath::new(vec!["native.log".into()]).unwrap(),
            offset: 0,
            length: 16384,
        },
    );
    let native_log = match log {
        FileReply::Data { data, .. } => protocol::decode(&data, 16384).unwrap(),
        other => panic!("native log: {other:?}"),
    };
    std::fs::write(base.join("native.log"), &native_log).unwrap();
    assert!(
        String::from_utf8_lossy(&native_log).contains("RGBA round trip passed"),
        "{}",
        String::from_utf8_lossy(&native_log)
    );
    // Exercise native Windows clipboard on this disposable guest, not the host.
    assert!(matches!(
        tools
            .control
            .call(ToolsCommand::ClipboardWrite {
                text: "WinPE Tools round trip".into()
            })
            .unwrap(),
        lightvm_proto::ToolReply::Ok
    ));
    assert!(
        matches!(tools.control.call(ToolsCommand::ClipboardRead).unwrap(), lightvm_proto::ToolReply::Clipboard { text } if text == "WinPE Tools round trip")
    );
    // More than one wire chunk; exercise the native clipboard image protocol,
    // including its immutable capture token and streamed SHA-256 commit.
    let mut pixels = b"LIMG".to_vec();
    pixels.extend(128u32.to_le_bytes());
    pixels.extend(64u32.to_le_bytes());
    pixels.extend((0..8192).flat_map(|i| [(i % 251) as u8, 42, 91, 255]));
    let path = RelativePath::new(vec!["image".into()]).unwrap();
    let transfer = match image_rpc(
        &tools,
        FileRequest::Begin {
            share: "clipboard-write".into(),
            path: path.clone(),
            size: pixels.len() as u64,
            sha256: protocol::encode(&Sha256::digest(&pixels)),
            replace: false,
        },
    ) {
        FileReply::Transfer { transfer } => transfer,
        other => panic!("image upload begin: {other:?}"),
    };
    for (i, chunk) in pixels.chunks(16384).enumerate() {
        expect_ok(image_rpc(
            &tools,
            FileRequest::Write {
                transfer: transfer.clone(),
                offset: (i * 16384) as u64,
                data: protocol::encode(chunk),
            },
        ));
    }
    expect_ok(image_rpc(&tools, FileRequest::Commit { transfer }));
    let capture = match image_rpc(
        &tools,
        FileRequest::Begin {
            share: "clipboard-read".into(),
            path: path.clone(),
            size: 0,
            sha256: "0".repeat(64),
            replace: false,
        },
    ) {
        FileReply::Transfer { transfer } => transfer,
        other => panic!("image capture begin: {other:?}"),
    };
    let mut received = Vec::new();
    loop {
        match image_rpc(
            &tools,
            FileRequest::Read {
                share: capture.clone(),
                path: path.clone(),
                offset: received.len() as u64,
                length: 16384,
            },
        ) {
            FileReply::Data { offset, data, eof } => {
                assert_eq!(offset, received.len() as u64);
                let bytes = protocol::decode(&data, 16384).unwrap();
                assert!(received.len() + bytes.len() <= pixels.len());
                assert!(eof || !bytes.is_empty());
                received.extend(bytes);
                if eof {
                    break;
                }
            }
            other => panic!("image capture read: {other:?}"),
        }
    }
    expect_ok(image_rpc(&tools, FileRequest::Cancel { transfer: capture }));
    assert_eq!(received, pixels);
    let payload: Vec<u8> = (0..262144).map(|i| (i % 251) as u8).collect();
    let transfer = match rpc(
        &tools,
        FileRequest::Begin {
            share: "inbox".into(),
            path: RelativePath::new(vec!["upload.bin".into()]).unwrap(),
            size: payload.len() as u64,
            sha256: protocol::encode(&Sha256::digest(&payload)),
            replace: false,
        },
    ) {
        FileReply::Transfer { transfer } => transfer,
        other => panic!("file upload begin: {other:?}"),
    };
    for (i, chunk) in payload.chunks(16384).enumerate() {
        expect_ok(rpc(
            &tools,
            FileRequest::Write {
                transfer: transfer.clone(),
                offset: (i * 16384) as u64,
                data: protocol::encode(chunk),
            },
        ));
    }
    expect_ok(rpc(&tools, FileRequest::Commit { transfer }));
    let result = "Windows PE x64/WHPX\nlab_only_x64_tools=true\ninstalled_windows=false\nnative_clipboard_text_and_rgba=passed\nclipboard_image_stream_round_trip=passed\nfile_upload_sha256_commit=passed\nfile_channel_log_return=passed\n";
    std::fs::write(base.join("results.txt"), result).unwrap();
    println!("{result}");
    stack.stop();
}

#[test]
#[ignore = "real QEMU/WHPX, Ubuntu live ISO/kernel and guest Tools bundle required"]
fn linux_tools_large_transfer_and_revocation() {
    let id = RunId::new_v7();
    let base = lab().join("tools-real").join(id.to_string());
    std::fs::create_dir_all(&base).unwrap();
    for dir in ["private", "journal", "share"] {
        std::fs::create_dir(base.join(dir)).unwrap();
    }
    for dir in ["private", "journal"] {
        qemu_backend::prepare::restrict_to_current_user(
            &qemu_backend::ManagedPath::verify(base.join(dir)).unwrap(),
        )
        .unwrap();
    }
    let config = ToolsConfig {
        enabled: true,
        files_transfer: true,
        clipboard: true,
        shares: vec![
            ToolShare {
                name: "Builds".into(),
                root: std::fs::canonicalize(base.join("share"))
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                writable: true,
            },
            ToolShare {
                name: "ReadOnly".into(),
                root: std::fs::canonicalize(base.join("share"))
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                writable: false,
            },
        ],
        ..Default::default()
    };
    let bundle = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/tools");
    let mut tools = ToolsRun::start(
        VmId::new_v7(),
        id,
        &config,
        &base.join("private"),
        &base.join("journal"),
        &[
            (
                "TOOLS.EXE",
                std::fs::read(bundle.join("TOOLS.EXE")).expect("build the guest bundle"),
            ),
            ("TOOLS.PY", std::fs::read(bundle.join("TOOLS.PY")).unwrap()),
            ("PROBE.PY", include_bytes!("common/tools_probe.py").to_vec()),
        ],
    )
    .unwrap();
    tools.apply(Policy::manual(&config));
    let net = base.join("private/net.sock");
    let mut guest = Guest::boot(
        &format!("tools-{id}"),
        &[
            "-netdev".into(),
            format!(
                "stream,id=n0,server=on,addr.type=unix,addr.path={}",
                net.display()
            ),
            "-device".into(),
            "e1000,netdev=n0,mac=52:54:00:12:34:56".into(),
            "-drive".into(),
            format!(
                "if=none,id=tools,media=cdrom,readonly=on,file={}",
                base.join("private/tools.iso").display()
            ),
            "-device".into(),
            "ide-cd,drive=tools,bus=ide.1".into(),
        ],
    );
    let start = Instant::now();
    let socket = loop {
        let socket =
            socket2::Socket::new(socket2::Domain::UNIX, socket2::Type::STREAM, None).unwrap();
        if socket
            .connect(&socket2::SockAddr::unix(&net).unwrap())
            .is_ok()
        {
            break socket;
        }
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "QEMU network socket"
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    let mut net_config = NetConfig::new(tools.endpoints().into());
    net_config.advertise_dns = false;
    let mut stack = NetStack::start(
        net_config,
        guest_net::transport::attach(socket).unwrap(),
        ToolsRun::allowed_endpoints(),
    )
    .unwrap();
    println!("Tools real guest: booting Ubuntu on WHPX");
    guest.login();
    let mounted = guest.cmd("sudo mkdir -p /mnt/LIGHTVM_TOOLS && sudo mount -o ro /dev/sr1 /mnt/LIGHTVM_TOOLS && test -f /mnt/LIGHTVM_TOOLS/tools.py", Duration::from_secs(30));
    assert!(mounted.contains("rc=0"), "{mounted}");
    let launched = guest.cmd("(nohup python3 /mnt/LIGHTVM_TOOLS/tools.py --config /mnt/LIGHTVM_TOOLS/tools.cfg >/tmp/lightvm-tools.log 2>&1 </dev/null & echo $! >/tmp/lightvm-tools.pid)", Duration::from_secs(10));
    assert!(launched.contains("rc=0"), "{launched}");
    let start = Instant::now();
    while !tools.control.connected() {
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "Tools connection: {}",
            guest.cmd(
                "tail -15 /tmp/lightvm-tools.log; ip -4 addr",
                Duration::from_secs(10)
            )
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    println!("Tools connected; checking real guest WebDAV operations");
    let probe = guest.cmd(
        "python3 /mnt/LIGHTVM_TOOLS/probe.py",
        Duration::from_secs(120),
    );
    assert!(probe.contains("WEBDAV_PROBE_PASSED"), "{probe}");
    std::fs::write(base.join("webdav.txt"), &probe).unwrap();
    tools
        .flush_before_snapshot()
        .expect("guest filesystem flush");
    let size = std::env::var("LIGHTVM_TOOLS_TEST_BYTES")
        .ok()
        .map(|s| s.parse::<u64>().unwrap())
        .unwrap_or(750_000_000);
    let block: Vec<u8> = (0..16384).map(|i| (i % 251) as u8).collect();
    let mut digest = Sha256::new();
    let mut left = size;
    while left > 0 {
        let n = left.min(block.len() as u64) as usize;
        digest.update(&block[..n]);
        left -= n as u64;
    }
    let expected = protocol::encode(&digest.finalize());
    let path = RelativePath::new(vec!["build.bin".into()]).unwrap();
    let transfer = match rpc(
        &tools,
        FileRequest::Begin {
            replace: false,
            share: "inbox".into(),
            path: path.clone(),
            size,
            sha256: expected.clone(),
        },
    ) {
        FileReply::Transfer { transfer } => transfer,
        other => panic!("begin: {other:?}"),
    };
    assert!(
        tools.flush_before_snapshot().is_err(),
        "an active guest transfer must block the snapshot flush"
    );
    let start = Instant::now();
    let mut sent = 0;
    let mut next_progress = 50_000_000;
    while sent < size {
        let n = (size - sent).min(block.len() as u64) as usize;
        expect_ok(rpc(
            &tools,
            FileRequest::Write {
                transfer: transfer.clone(),
                offset: sent,
                data: protocol::encode(&block[..n]),
            },
        ));
        sent += n as u64;
        if sent >= next_progress {
            println!(
                "Tools upload: {sent}/{size} bytes, {:.1}s",
                start.elapsed().as_secs_f64()
            );
            next_progress += 50_000_000;
        }
    }
    expect_ok(rpc(&tools, FileRequest::Commit { transfer }));
    let up_seconds = start.elapsed().as_secs_f64();
    let verified = guest.cmd("sha256sum ~/Documents/'LightVM Tools'/inbox/build.bin; cp ~/Documents/'LightVM Tools'/inbox/build.bin ~/Documents/'LightVM Tools'/outbox/build.bin; printf 'tools log return verified\\n' > ~/Documents/'LightVM Tools'/outbox/build.log", Duration::from_secs(120));
    assert!(
        verified.contains(&expected),
        "guest upload digest mismatch: {verified}"
    );
    let mut received = 0;
    let mut digest = Sha256::new();
    let start = Instant::now();
    next_progress = 50_000_000;
    loop {
        match rpc(
            &tools,
            FileRequest::Read {
                share: "outbox".into(),
                path: path.clone(),
                offset: received,
                length: 16384,
            },
        ) {
            FileReply::Data { offset, data, eof } => {
                assert_eq!(offset, received);
                let data = protocol::decode(&data, 16384).unwrap();
                assert!(received + data.len() as u64 <= size);
                digest.update(&data);
                received += data.len() as u64;
                if received >= next_progress {
                    println!(
                        "Tools download: {received}/{size} bytes, {:.1}s",
                        start.elapsed().as_secs_f64()
                    );
                    next_progress += 50_000_000;
                }
                if eof {
                    break;
                }
                assert!(!data.is_empty());
            }
            other => panic!("read: {other:?}"),
        }
    }
    assert_eq!(received, size);
    assert_eq!(protocol::encode(&digest.finalize()), expected);
    let down_seconds = start.elapsed().as_secs_f64();
    let log = rpc(
        &tools,
        FileRequest::Read {
            share: "outbox".into(),
            path: RelativePath::new(vec!["build.log".into()]).unwrap(),
            offset: 0,
            length: 16384,
        },
    );
    assert!(
        matches!(log, FileReply::Data { data, eof: true, .. } if protocol::decode(&data, 16384).unwrap() == b"tools log return verified\n")
    );
    let partial = match rpc(
        &tools,
        FileRequest::Begin {
            replace: false,
            share: "inbox".into(),
            path: RelativePath::new(vec!["cancelled.bin".into()]).unwrap(),
            size: 2,
            sha256: protocol::encode(&Sha256::digest(b"ab")),
        },
    ) {
        FileReply::Transfer { transfer } => transfer,
        other => panic!("begin cancellation test: {other:?}"),
    };
    expect_ok(rpc(
        &tools,
        FileRequest::Write {
            transfer: partial,
            offset: 0,
            data: "61".into(),
        },
    ));
    // Scope revocation invalidates the control connection and partial upload.
    tools.apply(Policy::default());
    assert!(
        tools
            .control
            .call(ToolsCommand::File {
                request: FileRequest::List {
                    share: "outbox".into(),
                    path: RelativePath::new(vec![]).unwrap()
                }
            })
            .is_err()
    );
    tools.apply(Policy::manual(&config));
    let deadline = Instant::now() + Duration::from_secs(15);
    while !tools.control.connected() {
        assert!(Instant::now() < deadline, "scope reconnect");
        std::thread::sleep(Duration::from_millis(50));
    }
    let clean = guest.cmd("test ! -e ~/Documents/'LightVM Tools'/inbox/cancelled.bin && test -z \"$(find ~/Documents/'LightVM Tools'/inbox -name '.lightvm-transfer-*' -print -quit)\"", Duration::from_secs(10));
    assert!(clean.contains("rc=0"), "{clean}");
    tools.flush_before_snapshot().unwrap();
    let result = format!(
        "Ubuntu live/WHPX; production ToolsRun and isolated stack\nbytes={size}\nupload_seconds={up_seconds:.3}\ndownload_seconds={down_seconds:.3}\nsha256={expected}\nlog_return=passed\nrevocation=passed\npartial_upload_cleanup=passed\nscope_reconnect=passed\nflush=passed\nwebdav=passed\nsynthetic_payload=true\n"
    );
    std::fs::write(base.join("results.txt"), &result).unwrap();
    println!("{result}");
    stack.stop();
}
