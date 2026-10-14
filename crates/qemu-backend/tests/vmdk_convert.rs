//! Conversion of VMware disks with the approved `qemu-img` (ADR-0030):
//! chain info, conversion of a snapshot chain with data in every level
//! (content identical to the source, source files unchanged), progress, and
//! cancellation. Runs when `LIGHTVM_QEMU_DIR` points at the QEMU build;
//! skipped with a message otherwise. Fixtures are made at test time.

use std::path::{Path, PathBuf};
use std::time::Duration;

use qemu_backend::ManagedPath;
use qemu_backend::image::{self, ImageCommand};

fn qemu_dir() -> Option<PathBuf> {
    let d = PathBuf::from(std::env::var_os("LIGHTVM_QEMU_DIR")?);
    d.join(exe("qemu-img")).is_file().then_some(d)
}

fn exe(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    }
}

fn tmp(prefix: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let d = std::env::temp_dir().join(format!("lvconv-{prefix}-{}-{nanos:x}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    let c = std::fs::canonicalize(&d).unwrap();
    let s = c.to_string_lossy().into_owned();
    PathBuf::from(s.strip_prefix(r"\\?\").unwrap_or(&s))
}

fn run(dir: &Path, prog: &Path, args: &[&str]) {
    let o = std::process::Command::new(prog)
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
}

fn sha(p: &Path) -> Vec<u8> {
    use sha2::Digest as _;
    sha2::Sha256::digest(std::fs::read(p).unwrap()).to_vec()
}

#[test]
fn snapshot_chain_converted_flattened_and_source_unchanged() {
    let Some(q) = qemu_dir() else {
        eprintln!("LIGHTVM_QEMU_DIR not set: skipped");
        return;
    };
    let img = q.join(exe("qemu-img"));
    let io = q.join(exe("qemu-io"));
    let dir = tmp("chain");
    run(
        &dir,
        &img,
        &["create", "-q", "-f", "vmdk", "base.vmdk", "64M"],
    );
    run(
        &dir,
        &io,
        &["-f", "vmdk", "-c", "write -P 0x11 0 1M", "base.vmdk"],
    );
    run(
        &dir,
        &img,
        &[
            "create",
            "-q",
            "-f",
            "vmdk",
            "-b",
            "base.vmdk",
            "-F",
            "vmdk",
            "s1.vmdk",
        ],
    );
    run(
        &dir,
        &io,
        &["-f", "vmdk", "-c", "write -P 0x22 1M 1M", "s1.vmdk"],
    );
    run(
        &dir,
        &img,
        &[
            "create", "-q", "-f", "vmdk", "-b", "s1.vmdk", "-F", "vmdk", "s2.vmdk",
        ],
    );
    run(
        &dir,
        &io,
        &["-f", "vmdk", "-c", "write -P 0x33 0 512k", "s2.vmdk"],
    );
    let before: Vec<_> = ["base.vmdk", "s1.vmdk", "s2.vmdk"]
        .iter()
        .map(|f| sha(&dir.join(f)))
        .collect();
    let logs = tmp("logs");
    let qemu_img = ManagedPath::verify(&img).unwrap();
    let log_dir = ManagedPath::verify(&logs).unwrap();
    let cmd = ImageCommand {
        qemu_img: &qemu_img,
        log_dir: &log_dir,
        timeout: Duration::from_secs(300),
    };
    let top = ManagedPath::verify(dir.join("s2.vmdk")).unwrap();
    let json = image::vmdk_chain_info(&cmd, &top, &|| false).unwrap();
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 3);
    let out = ManagedPath::verify(dir.join("out.qcow2")).unwrap();
    let mut seen = Vec::new();
    image::convert_vmdk(&cmd, &top, &out, &|| false, &mut |p| seen.push(p)).unwrap();
    assert_eq!(seen.last(), Some(&100));
    // Same guest-visible content, no backing file, consistent image.
    run(
        &dir,
        &img,
        &[
            "compare",
            "-q",
            "-f",
            "vmdk",
            "-F",
            "qcow2",
            "s2.vmdk",
            "out.qcow2",
        ],
    );
    image::check_image(&cmd, &out, &|| false).unwrap();
    assert_eq!(image::virtual_size(&cmd, &out).unwrap(), 64 << 20);
    let info: serde_json::Value = serde_json::from_str(&{
        let o = std::process::Command::new(&img)
            .args(["info", "--output=json", "-f", "qcow2"])
            .arg(dir.join("out.qcow2"))
            .output()
            .unwrap();
        String::from_utf8(o.stdout).unwrap()
    })
    .unwrap();
    assert!(info.get("backing-filename").is_none(), "flattened: {info}");
    // The VMware files are untouched.
    let after: Vec<_> = ["base.vmdk", "s1.vmdk", "s2.vmdk"]
        .iter()
        .map(|f| sha(&dir.join(f)))
        .collect();
    assert_eq!(before, after);
    // An existing destination is never overwritten.
    assert!(image::convert_vmdk(&cmd, &top, &out, &|| false, &mut |_| {}).is_err());
    // Cancellation stops qemu-img and reports it.
    let out2 = ManagedPath::verify(dir.join("out2.qcow2")).unwrap();
    let e = image::convert_vmdk(&cmd, &top, &out2, &|| true, &mut |_| {}).unwrap_err();
    assert!(e.to_string().contains("cancelled"), "{e}");
    // No output files of qemu-img are left in the log directory.
    assert_eq!(std::fs::read_dir(&logs).unwrap().count(), 0);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&logs);
}
