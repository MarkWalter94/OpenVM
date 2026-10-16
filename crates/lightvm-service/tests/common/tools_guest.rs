// WHPX Ubuntu-live serial harness for Tools qualification.
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
fn lab() -> PathBuf {
    std::env::var_os("LIGHTVM_LAB").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../lab"))
}
fn free_port() -> u16 { TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port() }
struct Guest {
    child: Child,
    serial: TcpStream,
    log: std::fs::File,
}

impl Drop for Guest {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Guest {
    /// Boots the live system with the given network arguments.
    fn boot(tag: &str, net_args: &[String]) -> Guest {
        let lab = lab();
        let q = lab.join(r"qemu\qemu-11.1.0\qemu-system-x86_64.exe");
        let casper = lab.join(r"tools-real\ubuntu-boot\casper");
        let iso = lab.join(r"media\ubuntu-26.04.1-desktop-amd64.iso");
        for p in [&q, &casper.join("vmlinuz"), &casper.join("initrd"), &iso] {
            assert!(p.is_file(), "missing lab file {}", p.display());
        }
        let dir = lab.join("tools-real").join("logs");
        std::fs::create_dir_all(&dir).unwrap();
        let serial_port = free_port();
        let mut args: Vec<String> = [
            "-no-user-config",
            "-nodefaults",
            "-accel",
            "whpx",
            "-machine",
            "q35",
            "-cpu",
            "Skylake-Client-v4",
            "-m",
            "6144",
            "-smp",
            "2",
            "-display",
            "none",
            "-device",
            "VGA",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
        args.extend([
            "-kernel".into(),
            casper.join("vmlinuz").display().to_string(),
            "-initrd".into(),
            casper.join("initrd").display().to_string(),
            "-append".into(),
            "boot=casper console=ttyS0,115200 systemd.unit=multi-user.target".into(),
            "-drive".into(),
            format!(
                "if=none,id=cd0,media=cdrom,readonly=on,file={}",
                iso.display()
            ),
            "-device".into(),
            "ide-cd,drive=cd0,bus=ide.0".into(),
            "-serial".into(),
            format!("tcp:127.0.0.1:{serial_port},server=on,wait=off"),
        ]);
        args.extend(net_args.iter().cloned());
        std::fs::write(dir.join(format!("{tag}.cmdline.txt")), args.join(" ")).unwrap();
        let child = Command::new(&q)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(std::fs::File::create(dir.join(format!("{tag}.stdout.txt"))).unwrap())
            .stderr(std::fs::File::create(dir.join(format!("{tag}.stderr.txt"))).unwrap())
            .spawn()
            .unwrap();
        // BelowNormal priority (shared host).
        let _ = Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                &format!(
                    "(Get-Process -Id {}).PriorityClass = 'BelowNormal'",
                    child.id()
                ),
            ])
            .status();
        let t = Instant::now();
        let serial = loop {
            if let Ok(s) = TcpStream::connect(("127.0.0.1", serial_port)) {
                break s;
            }
            assert!(t.elapsed() < Duration::from_secs(20), "serial port");
            std::thread::sleep(Duration::from_millis(200));
        };
        serial
            .set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        let log = std::fs::File::create(dir.join(format!("{tag}.serial.txt"))).unwrap();
        Guest { child, serial, log }
    }

    fn read_until(&mut self, pat: &str, timeout: Duration) -> Option<String> {
        let t = Instant::now();
        let mut acc = Vec::new();
        let mut b = [0u8; 65536];
        while t.elapsed() < timeout {
            match self.serial.read(&mut b) {
                Ok(0) => break,
                Ok(n) => {
                    let _ = self.log.write_all(&b[..n]);
                    acc.extend_from_slice(&b[..n]);
                    if String::from_utf8_lossy(&acc).contains(pat) {
                        return Some(String::from_utf8_lossy(&acc).into_owned());
                    }
                }
                Err(_) => {}
            }
        }
        None
    }

    fn login(&mut self) {
        let t = Instant::now();
        loop {
            self.serial.write_all(b"\n").unwrap();
            if self.read_until("login:", Duration::from_secs(10)).is_some() {
                break;
            }
            assert!(t.elapsed() < Duration::from_secs(600), "no login prompt");
        }
        self.serial.write_all(b"ubuntu\n").unwrap();
        assert!(
            self.read_until("ubuntu@ubuntu", Duration::from_secs(60))
                .is_some(),
            "no shell"
        );
        self.cmd("export TERM=dumb; stty -echo", Duration::from_secs(10));
    }

    /// Runs a shell command; returns its output up to the end marker.
    fn cmd(&mut self, c: &str, timeout: Duration) -> String {
        let line = format!("{c}; echo \"@@E\"\"ND@@ rc=$?\"\n");
        self.serial.write_all(line.as_bytes()).unwrap();
        let out = self.read_until("@@END@@ rc=", timeout).unwrap_or_else(|| {
            panic!("command timed out: {c}");
        });
        // Read the rest of the marker line.
        let mut tail = String::new();
        if let Some(t) = self.read_until("\n", Duration::from_millis(500)) {
            tail = t;
        }
        out + &tail
    }

}
