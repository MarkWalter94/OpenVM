//! Storage-driver preparation helper (ADR-0030): a minimal Linux appliance
//! (`packaging/storage-helper`) booted headless by QEMU with one disk, the
//! converted copy of an imported Windows guest, to edit its offline `SYSTEM`
//! hive (service start types and critical device entries) so that Windows
//! starts from the controller LightVM gives it.
//!
//! The helper is a separate GPL image (Alpine Linux kernel, busybox, ntfs-3g,
//! chntpw `reged`), not part of LightVM: it is looked for in a folder chosen
//! by the installation ([`HelperImage::locate`]) and used only when the
//! SHA-256 of both files is approved ([`APPROVED_HELPER_SHA256`] or a
//! development approval). Without it, imports keep the manual preparation.
//!
//! The VM has no network, no display, 256 MiB, one vCPU, the converted disk
//! on `virtio-blk-pci` and the job in two `fw_cfg` files; its serial console
//! goes to a file in the private directory, parsed by [`parse_report`]. QEMU
//! runs in its own Job Object ([`crate::image::ImageCommand`]): cancellation,
//! the Emergency Stop and the death of the supervisor terminate it.

use std::path::Path;
use std::time::Duration;

use crate::cmdline::{Accelerator, CpuModel, MachineType};
use crate::error::{BackendError, Result};
use crate::image::ImageCommand;
use crate::paths::ManagedPath;
use crate::probe::sha256_file;

/// Kernel file of the helper.
pub const KERNEL_FILE: &str = "lightvm-storage-helper.vmlinuz";
/// Initramfs file of the helper.
pub const INITRD_FILE: &str = "lightvm-storage-helper.initrd";

/// SHA-256 of the helper files built by `packaging/storage-helper/build.py`
/// from its `packages.lock` and `init` (reproducible build).
pub const APPROVED_HELPER_SHA256: &[&str] = &[
    // lightvm-storage-helper.vmlinuz (Alpine linux-virt 6.18.56-r0)
    "dc99c86bb8e58cb4eda5f01eac44c4d972105945a46dfe76a9e86e8529915c72",
    // lightvm-storage-helper.initrd
    "4efcd0badce86a2e5ac6916a61732c85ab2cbf12df0688f88f555a72487e7cf2",
];

/// Longest run of the helper (boot, mount, edit, power off take seconds).
pub const HELPER_TIMEOUT: Duration = Duration::from_secs(300);
/// Largest serial output read back.
const MAX_SERIAL: u64 = 4 << 20;
/// Largest job file.
const MAX_JOB: usize = 64 << 10;

/// The two files of an approved helper.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HelperImage {
    /// Kernel.
    pub kernel: ManagedPath,
    /// Initramfs.
    pub initrd: ManagedPath,
}

impl HelperImage {
    /// The helper in `dir`: `Ok(None)` when neither file is there, an error
    /// when only one is or when a file's SHA-256 is not in `approved` (the
    /// built-in [`APPROVED_HELPER_SHA256`] plus development approvals).
    pub fn locate(dir: &Path, approved: &[String]) -> Result<Option<Self>> {
        let k = dir.join(KERNEL_FILE);
        let i = dir.join(INITRD_FILE);
        let present = |p: &Path| std::fs::symlink_metadata(p).is_ok();
        match (present(&k), present(&i)) {
            (false, false) => return Ok(None),
            (true, true) => {}
            _ => {
                return Err(BackendError::Unsupported {
                    what: "storage helper".into(),
                    hint: format!("{KERNEL_FILE} and {INITRD_FILE} must both be present"),
                });
            }
        }
        let kernel = ManagedPath::verify(k)?;
        let initrd = ManagedPath::verify(i)?;
        for (p, name) in [(&kernel, "storage helper kernel"), (&initrd, "storage helper initramfs")] {
            let meta = std::fs::metadata(p.as_path()).map_err(BackendError::Io)?;
            if !meta.is_file() {
                return Err(BackendError::invalid("storage_helper", "not a regular file"));
            }
            let sha = sha256_file(p.as_path())?;
            let ok = APPROVED_HELPER_SHA256
                .iter()
                .any(|a| a.eq_ignore_ascii_case(&sha))
                || approved.iter().any(|a| a.eq_ignore_ascii_case(&sha));
            if !ok {
                return Err(BackendError::UnapprovedBinary {
                    binary: name,
                    sha256: sha,
                });
            }
        }
        Ok(Some(Self { kernel, initrd }))
    }
}

/// Machine, CPU and firmware the helper runs with (those the probe
/// verified for the host reference machine family, SeaBIOS).
#[derive(Clone, Debug)]
pub struct HelperMachine {
    /// Approved `qemu-system-x86_64`.
    pub qemu_system: ManagedPath,
    /// q35 machine verified by the probe.
    pub machine: MachineType,
    /// Accelerator (never TCG).
    pub accelerator: Accelerator,
    /// Named CPU model.
    pub cpu: CpuModel,
    /// SeaBIOS image verified by the probe; `None`: QEMU's default.
    pub bios: Option<ManagedPath>,
}

/// What the helper does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HelperJob {
    /// Keys below `CurrentControlSet` exported after the edit, one per line.
    pub keys: Vec<String>,
    /// REGEDIT4 text imported into the hive; `None`: read-only inspection.
    pub reg: Option<String>,
}

/// Outcome reported by the helper.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HelperOutcome {
    /// Done; the volume was unmounted cleanly.
    Ok,
    /// Refused or failed (`code` is a short stable word of `init`).
    Error {
        /// Code.
        code: String,
        /// Text.
        text: String,
    },
    /// No result line: the helper did not finish.
    Missing,
}

/// Parsed serial output of a helper run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HelperReport {
    /// Outcome.
    pub outcome: HelperOutcome,
    /// Information lines (`INFO`).
    pub info: Vec<String>,
    /// Per control set (`ControlSetNNN`), the exported `.reg` text.
    pub exports: Vec<(String, String)>,
    /// Keys the helper did not find, per control set.
    pub missing: Vec<(String, String)>,
}

/// Parses the serial output of the helper (`packaging/storage-helper/init`).
/// Lines that do not start with `LIGHTVM-PREP ` (kernel messages) are
/// ignored; exported lines are prefixed `LIGHTVM-PREP X ` by the helper, so
/// no text from the guest hive can produce a record.
#[must_use]
pub fn parse_report(serial: &str) -> HelperReport {
    let mut r = HelperReport {
        outcome: HelperOutcome::Missing,
        info: Vec::new(),
        exports: Vec::new(),
        missing: Vec::new(),
    };
    for raw in serial.split('\n') {
        let line = raw.trim_end_matches('\r');
        let Some(rest) = line.strip_prefix("LIGHTVM-PREP ") else {
            continue;
        };
        if let Some(x) = rest.strip_prefix("X ").or_else(|| (rest == "X").then_some("")) {
            if let Some((_, text)) = r.exports.last_mut() {
                text.push_str(x);
                text.push('\n');
            }
        } else if let Some(cs) = rest.strip_prefix("CS ") {
            r.exports.push((cs.trim().to_owned(), String::new()));
        } else if let Some(k) = rest.strip_prefix("MISSING ") {
            let cs = r.exports.last().map(|(c, _)| c.clone()).unwrap_or_default();
            r.missing.push((cs, k.trim().to_owned()));
        } else if let Some(i) = rest.strip_prefix("INFO ") {
            if r.info.len() < 200 {
                r.info.push(i.to_owned());
            }
        } else if let Some(res) = rest.strip_prefix("RESULT ") {
            r.outcome = if res.trim() == "ok" {
                HelperOutcome::Ok
            } else if let Some(e) = res.strip_prefix("error ") {
                let (code, text) = e.split_once(' ').unwrap_or((e, ""));
                HelperOutcome::Error {
                    code: code.to_owned(),
                    text: text.trim().to_owned(),
                }
            } else {
                HelperOutcome::Error {
                    code: "unknown".into(),
                    text: res.to_owned(),
                }
            };
        }
    }
    r
}

/// QEMU arguments of a helper run (pure; paths already verified).
#[must_use]
pub fn helper_args(
    m: &HelperMachine,
    image: &HelperImage,
    disk: &ManagedPath,
    keys: &ManagedPath,
    reg: Option<&ManagedPath>,
    serial: &ManagedPath,
) -> Vec<String> {
    let fixed = m.machine.family().fixed_properties();
    let mut a: Vec<String> = vec![
        "-name".into(),
        "guest=lightvm-storage-helper".into(),
        "-no-user-config".into(),
        "-nodefaults".into(),
        "-machine".into(),
        format!("type={}{fixed}", m.machine.as_str()),
    ];
    if let Some(b) = &m.bios {
        a.extend(["-bios".into(), b.qemu_value().to_owned()]);
    }
    a.extend([
        "-accel".into(),
        m.accelerator.qemu_name().to_owned(),
        "-cpu".into(),
        m.cpu.as_str().to_owned(),
        "-m".into(),
        "size=256M".into(),
        "-smp".into(),
        "cpus=1".into(),
        "-nic".into(),
        "none".into(),
        "-display".into(),
        "none".into(),
        "-no-reboot".into(),
        "-kernel".into(),
        image.kernel.qemu_value().to_owned(),
        "-initrd".into(),
        image.initrd.qemu_value().to_owned(),
        "-append".into(),
        "console=ttyS0 quiet loglevel=3 panic=-1".into(),
        "-chardev".into(),
        format!("file,id=ser0,path={}", serial.qemu_value()),
        "-serial".into(),
        "chardev:ser0".into(),
        "-blockdev".into(),
        format!(
            "driver=file,node-name=disk-file,filename={}",
            disk.qemu_value()
        ),
        "-blockdev".into(),
        "driver=qcow2,node-name=disk,file=disk-file".into(),
        "-device".into(),
        "virtio-blk-pci,drive=disk".into(),
        "-fw_cfg".into(),
        format!("name=opt/org.lightvm/prep.keys,file={}", keys.qemu_value()),
    ]);
    if let Some(r) = reg {
        a.extend([
            "-fw_cfg".into(),
            format!("name=opt/org.lightvm/prep.reg,file={}", r.qemu_value()),
        ]);
    }
    a
}

/// A key path the helper may receive: letters, digits and `_ # & . - { }`
/// separated by single backslashes (no quotes, spaces or control
/// characters: the helper uses it in a shell word).
#[must_use]
pub fn is_safe_key(k: &str) -> bool {
    !k.is_empty()
        && k.len() <= 255
        && k.split('\\').all(|c| {
            !c.is_empty()
                && c.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_#&.-{}".contains(&b))
        })
}

/// Runs the helper on `disk` (a QCOW2 file nobody else has open) and
/// returns its report. `work` is a private directory for the job and the
/// serial output, removed afterwards. Errors: QEMU failing to start or
/// exiting with an error, timeout, cancellation.
pub fn run_helper(
    m: &HelperMachine,
    image: &HelperImage,
    disk: &ManagedPath,
    job: &HelperJob,
    work: &ManagedPath,
    cancel: &dyn Fn() -> bool,
) -> Result<HelperReport> {
    if job.keys.iter().any(|k| !is_safe_key(k)) {
        return Err(BackendError::invalid("storage_helper", "key not allowed"));
    }
    let keys_text: String = job.keys.iter().map(|k| format!("{k}\n")).collect();
    if keys_text.len() > MAX_JOB || job.reg.as_ref().is_some_and(|r| r.len() > MAX_JOB) {
        return Err(BackendError::invalid("storage_helper", "job too large"));
    }
    let keys = work.join_file("prep-keys.txt")?;
    let reg = work.join_file("prep-job.reg")?;
    let serial = work.join_file("prep-serial.log")?;
    let cleanup = || {
        for p in [&keys, &reg, &serial] {
            let _ = std::fs::remove_file(p.as_path());
        }
    };
    cleanup();
    let write = |p: &ManagedPath, t: &str| std::fs::write(p.as_path(), t).map_err(BackendError::Io);
    let res = (|| {
        write(&keys, &keys_text)?;
        if let Some(r) = &job.reg {
            write(&reg, r)?;
        }
        let args = helper_args(m, image, disk, &keys, job.reg.as_ref().map(|_| &reg), &serial);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let cmd = ImageCommand {
            qemu_img: &m.qemu_system,
            log_dir: work,
            timeout: HELPER_TIMEOUT,
        };
        let out = cmd.run(&args, cancel)?;
        let text = read_serial(&serial);
        if out.code != Some(0) {
            return Err(BackendError::Io(std::io::Error::other(format!(
                "storage helper: QEMU exited with {:?}: {}",
                out.code,
                crate::probe::stderr_excerpt(&out.stderr)
            ))));
        }
        Ok(parse_report(&text))
    })();
    cleanup();
    res
}

fn read_serial(p: &ManagedPath) -> String {
    use std::io::Read as _;
    let Ok(f) = std::fs::File::open(p.as_path()) else {
        return String::new();
    };
    let mut buf = Vec::new();
    let _ = f.take(MAX_SERIAL).read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_parsing() {
        let serial = "[    0.1] boot\r\nLIGHTVM-PREP INFO mode rw\nLIGHTVM-PREP INFO windows on /dev/vda1: Windows/System32/config/SYSTEM\n\
LIGHTVM-PREP CS ControlSet001\nLIGHTVM-PREP X REGEDIT4\nLIGHTVM-PREP X \nLIGHTVM-PREP X [HKEY_LOCAL_MACHINE\\SYSTEM\\ControlSet001\\Services\\msahci]\n\
LIGHTVM-PREP X \"Start\"=dword:00000000\nLIGHTVM-PREP MISSING Services\\x\nLIGHTVM-PREP RESULT ok\n[ 2.0] reboot: Power down\n";
        let r = parse_report(serial);
        assert_eq!(r.outcome, HelperOutcome::Ok);
        assert_eq!(r.info.len(), 2);
        assert_eq!(r.exports.len(), 1);
        assert_eq!(r.exports[0].0, "ControlSet001");
        assert!(r.exports[0].1.contains("\"Start\"=dword:00000000\n"));
        assert_eq!(r.missing, vec![("ControlSet001".into(), "Services\\x".into())]);

        let r = parse_report("LIGHTVM-PREP RESULT error hibernated Windows is hibernated\n");
        assert_eq!(
            r.outcome,
            HelperOutcome::Error {
                code: "hibernated".into(),
                text: "Windows is hibernated".into()
            }
        );
        assert_eq!(parse_report("garbage\n").outcome, HelperOutcome::Missing);
        // Exported text cannot forge a record: it is always behind `X `.
        let r = parse_report("LIGHTVM-PREP CS ControlSet001\nLIGHTVM-PREP X LIGHTVM-PREP RESULT ok\n");
        assert_eq!(r.outcome, HelperOutcome::Missing);
    }

    #[test]
    fn keys_allowed() {
        assert!(is_safe_key("Services\\msahci"));
        assert!(is_safe_key(
            "Control\\CriticalDeviceDatabase\\pci#ven_8086&dev_2922&cc_010601"
        ));
        for bad in ["", "a\\\\b", "a b", "a\"b", "a'b", "a$b", "a\nb", "\\a", "a;b", "a`b"] {
            assert!(!is_safe_key(bad), "{bad:?}");
        }
    }

    #[test]
    fn arguments() {
        let dir = std::env::temp_dir();
        let p = |n: &str| ManagedPath::verify(dir.join(n)).unwrap();
        let m = HelperMachine {
            qemu_system: p("qemu-system-x86_64"),
            machine: MachineType::versioned_q35("pc-q35-11.1").unwrap(),
            accelerator: Accelerator::Whpx,
            cpu: CpuModel::nominal("Skylake-Client-v4").unwrap(),
            bios: Some(p("bios-256k.bin")),
        };
        let image = HelperImage {
            kernel: p(KERNEL_FILE),
            initrd: p(INITRD_FILE),
        };
        let a = helper_args(&m, &image, &p("disk.qcow2"), &p("k.txt"), None, &p("s.log"));
        let s = a.join(" ");
        assert!(s.contains("-nodefaults"));
        assert!(s.contains("-nic none"));
        assert!(s.contains("-accel whpx"));
        assert!(s.contains("virtio-blk-pci,drive=disk"));
        assert!(!s.contains("prep.reg"));
        assert!(!s.contains("tcg"));
        let a = helper_args(&m, &image, &p("disk.qcow2"), &p("k.txt"), Some(&p("j.reg")), &p("s.log"));
        assert!(a.join(" ").contains("name=opt/org.lightvm/prep.reg,file="));
    }
}
