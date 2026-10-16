//! Helpers of the `vmware-import` tests: temporary VM folders written by
//! the tests themselves (never a real VMware VM of the machine).

#![allow(dead_code, unreachable_pub)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

static SEQ: AtomicU32 = AtomicU32::new(0);

/// New empty directory under the system temporary folder.
pub fn temp_dir(prefix: &str) -> PathBuf {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let d = std::env::temp_dir().join(format!(
        "lvmw-{prefix}-{}-{n}-{nanos:x}",
        std::process::id()
    ));
    std::fs::create_dir_all(&d).unwrap();
    // The path the system resolves (no 8.3 names, no `\\?\`).
    let c = std::fs::canonicalize(&d).unwrap();
    let s = c.to_string_lossy().into_owned();
    PathBuf::from(s.strip_prefix(r"\\?\").unwrap_or(&s))
}

/// A VM folder `<tmp>/<name>/` with `<name>.vmx`.
pub struct VmDir {
    pub root: PathBuf,
    pub folder: PathBuf,
    pub vmx: PathBuf,
}

impl VmDir {
    pub fn new(prefix: &str) -> Self {
        let root = temp_dir(prefix);
        let folder = root.join("Test VM");
        std::fs::create_dir_all(&folder).unwrap();
        let vmx = folder.join("Test VM.vmx");
        Self { root, folder, vmx }
    }

    /// Writes the `.vmx` with the base keys of a powered-off VM, the disk
    /// `disk` on `slot` and the `extra` lines.
    pub fn write_vmx(&self, guest: &str, slot: &str, disk: &str, extra: &str) {
        let (ctl, _) = slot.split_once(':').unwrap();
        let mut s = String::from(
            ".encoding = \"UTF-8\"\nconfig.version = \"8\"\nvirtualHW.version = \"19\"\n",
        );
        s.push_str(&format!(
            "displayName = \"Test VM\"\nguestOS = \"{guest}\"\n"
        ));
        s.push_str("numvcpus = \"2\"\nmemsize = \"2048\"\ncleanShutdown = \"TRUE\"\n");
        if !ctl.starts_with("ide") {
            s.push_str(&format!("{ctl}.present = \"TRUE\"\n"));
        }
        s.push_str(&format!(
            "{slot}.present = \"TRUE\"\n{slot}.fileName = \"{}\"\n",
            disk.replace('|', "|7C").replace('"', "|22")
        ));
        s.push_str(extra);
        std::fs::write(&self.vmx, s).unwrap();
    }

    pub fn vmx_str(&self) -> String {
        self.vmx.to_string_lossy().into_owned()
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.folder.join(name)
    }
}

impl Drop for VmDir {
    fn drop(&mut self) {
        let _ = remove_tree(&self.root);
    }
}

/// Removes a tree without following junctions or links.
pub fn remove_tree(p: &Path) -> std::io::Result<()> {
    let m = std::fs::symlink_metadata(p)?;
    if vmware_import::paths::is_link(&m) {
        return std::fs::remove_dir(p).or_else(|_| std::fs::remove_file(p));
    }
    if m.is_dir() {
        for e in std::fs::read_dir(p)? {
            remove_tree(&e?.path())?;
        }
        std::fs::remove_dir(p)
    } else {
        std::fs::remove_file(p)
    }
}

/// A text descriptor with one FLAT extent of `sectors`.
pub fn flat_descriptor(extent: &str, sectors: u64) -> String {
    format!(
        "# Disk DescriptorFile\nversion=1\nCID=fd32bc87\nparentCID=ffffffff\n\
         createType=\"monolithicFlat\"\n\nRW {sectors} FLAT \"{extent}\" 0\n\nddb.adapterType = \"lsilogic\"\n"
    )
}

/// Writes `name` (descriptor) and its flat extent `name-flat.vmdk` of
/// `sectors` sectors (sparse file).
pub fn write_flat_disk(dir: &Path, name: &str, sectors: u64) {
    let stem = name.trim_end_matches(".vmdk");
    let extent = format!("{stem}-flat.vmdk");
    std::fs::write(dir.join(name), flat_descriptor(&extent, sectors)).unwrap();
    let f = std::fs::File::create(dir.join(&extent)).unwrap();
    f.set_len(sectors * 512).unwrap();
}

/// `qemu-img` of the approved QEMU build (`LIGHTVM_QEMU_DIR`), if set.
pub fn qemu_img() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("LIGHTVM_QEMU_DIR")?);
    let exe = dir.join(if cfg!(windows) {
        "qemu-img.exe"
    } else {
        "qemu-img"
    });
    exe.is_file().then_some(exe)
}

/// Runs `qemu-img` with `args`, panicking on failure; returns stdout.
pub fn run_qemu_img(qemu_img: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new(qemu_img)
        .args(args)
        .output()
        .expect("qemu-img runs");
    assert!(
        out.status.success(),
        "qemu-img {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// Creates a directory junction `link` → `target` (Windows), `false` if
/// not possible.
pub fn junction(link: &Path, target: &Path) -> bool {
    if cfg!(windows) {
        std::process::Command::new("cmd")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .is_ok_and(|o| o.status.success())
    } else {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link).is_ok()
        }
        #[cfg(not(unix))]
        false
    }
}
