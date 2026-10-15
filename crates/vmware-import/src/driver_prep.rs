//! Offline enablement of the guest's own storage driver (ADR-0030).
//!
//! A Windows guest boots only from a controller whose driver starts at boot
//! (otherwise stop 0x7B). When the import moves the boot disk to another
//! controller (VMware LSI Logic, which QEMU does not emulate; an NVMe disk
//! with legacy BIOS, which SeaBIOS does not boot), the inbox driver of the
//! new controller is enabled in the **converted copy** by editing its
//! offline `SYSTEM` hive: no driver is added, the guest's own one is set to
//! start at boot.
//!
//! | Guest | Controller (QEMU device, PCI id) | Edit |
//! | --- | --- | --- |
//! | Windows Vista, 7 | AHCI (`ich9-ahci`, 8086:2922, class 010601) | `msahci` `Start`=0; critical device `PCI#VEN_8086&DEV_2922&CC_010601` → `msahci` |
//! | Windows 8.1, 10 | AHCI (same) | `storahci` `Start`=0, every `StartOverride` value 0 (no critical device database since Windows 8) |
//! | Windows XP | PIIX3 IDE (8086:7010, i440fx) | `intelide`, `pciide`, `atapi` `Start`=0; critical devices `PCI#VEN_8086&DEV_7010` → `intelide`, `Primary_IDE_Channel`/`Secondary_IDE_Channel` → `atapi` |
//!
//! The edit is done by the storage helper (`packaging/storage-helper`) in
//! two runs: an inspection (read-only) that exports the service keys, then
//! the import of the `.reg` built here and the export of every key it
//! touched, verified here. Everything read back comes from the guest disk
//! and is parsed as untrusted text (bounded, only the values named here are
//! looked at).

use lightvm_proto::{DiskController, GuestOs, PublicError};

/// Device class of storage controllers (`hdc`).
pub const HDC_CLASS_GUID: &str = "{4D36E96A-E325-11CE-BFC1-08002BE10318}";

/// What makes a guest boot from its new controller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriverPlan {
    /// Services (kernel drivers) that must start at boot.
    pub services: &'static [&'static str],
    /// Critical device database entries `(key, service)` (Windows XP,
    /// Vista, 7).
    pub critical_devices: &'static [(&'static str, &'static str)],
    /// Windows 8 and later: the `StartOverride` values of the services are
    /// reset to 0.
    pub start_override: bool,
}

const VISTA_AHCI: DriverPlan = DriverPlan {
    services: &["msahci"],
    critical_devices: &[("PCI#VEN_8086&DEV_2922&CC_010601", "msahci")],
    start_override: false,
};
const WIN8_AHCI: DriverPlan = DriverPlan {
    services: &["storahci"],
    critical_devices: &[],
    start_override: true,
};
const XP_IDE: DriverPlan = DriverPlan {
    services: &["intelide", "pciide", "atapi"],
    critical_devices: &[
        ("PCI#VEN_8086&DEV_7010", "intelide"),
        ("Primary_IDE_Channel", "atapi"),
        ("Secondary_IDE_Channel", "atapi"),
    ],
    start_override: false,
};

/// The plan for a guest moved to `controller`; `None` when the automatic
/// preparation does not cover the combination (manual preparation).
#[must_use]
pub fn driver_plan(os: GuestOs, controller: DiskController) -> Option<DriverPlan> {
    match (os, controller) {
        (GuestOs::WindowsVista | GuestOs::Windows7, DiskController::Ahci) => Some(VISTA_AHCI),
        (GuestOs::Windows81 | GuestOs::Windows10, DiskController::Ahci) => Some(WIN8_AHCI),
        (GuestOs::WindowsXp, DiskController::Ide) => Some(XP_IDE),
        _ => None,
    }
}

const SERVICES: &str = "Services\\";
const CDDB: &str = "Control\\CriticalDeviceDatabase\\";

impl DriverPlan {
    /// Keys (below `CurrentControlSet`) exported by the inspection run.
    #[must_use]
    pub fn inspect_keys(&self) -> Vec<String> {
        self.services
            .iter()
            .map(|s| format!("{SERVICES}{s}"))
            .collect()
    }

    /// Keys exported after the edit.
    #[must_use]
    pub fn verify_keys(&self) -> Vec<String> {
        let mut k = self.inspect_keys();
        k.extend(
            self.critical_devices
                .iter()
                .map(|(d, _)| format!("{CDDB}{d}")),
        );
        k
    }
}

/// A key of a `reged` export: path below the control set, values (name in
/// lower case, raw value text after `=`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegKey {
    /// Path below `ControlSetNNN`, as written.
    pub path: String,
    /// `(name, value)`; continuation lines of `hex` values are joined.
    pub values: Vec<(String, String)>,
}

impl RegKey {
    /// Raw value of `name` (case-insensitive).
    #[must_use]
    pub fn value(&self, name: &str) -> Option<&str> {
        self.values
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Most keys and values read from one export.
const MAX_KEYS: usize = 4096;
const MAX_VALUES: usize = 256;

/// Parses a `reged -x` export of control set `cs` (untrusted text from the
/// guest disk): keys outside `HKEY_LOCAL_MACHINE\SYSTEM\<cs>\` and
/// malformed lines are ignored; at most [`MAX_KEYS`] keys of at most
/// [`MAX_VALUES`] values.
#[must_use]
pub fn parse_export(text: &str, cs: &str) -> Vec<RegKey> {
    let prefix = format!("HKEY_LOCAL_MACHINE\\SYSTEM\\{cs}\\");
    let mut keys: Vec<RegKey> = Vec::new();
    let mut current: Option<usize> = None;
    let mut continuing = false;
    for line in text.lines() {
        if continuing {
            if let Some(i) = current
                && let Some((_, v)) = keys[i].values.last_mut()
                && v.len() < 64 << 10
            {
                v.push_str(line.trim().trim_end_matches('\\'));
            }
            continuing = line.ends_with('\\');
            continue;
        }
        if let Some(h) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            current = None;
            let Some(rel) = h
                .get(..prefix.len())
                .filter(|p| p.eq_ignore_ascii_case(&prefix))
                .and_then(|_| h.get(prefix.len()..))
            else {
                continue;
            };
            if keys.len() < MAX_KEYS {
                keys.push(RegKey {
                    path: rel.to_owned(),
                    values: Vec::new(),
                });
                current = Some(keys.len() - 1);
            }
            continue;
        }
        let Some(i) = current else { continue };
        let Some(rest) = line.strip_prefix('"') else {
            continue;
        };
        let Some((name, value)) = rest.split_once("\"=") else {
            continue;
        };
        if keys[i].values.len() < MAX_VALUES {
            keys[i].values.push((
                name.to_ascii_lowercase(),
                value.trim_end_matches('\\').to_owned(),
            ));
            continuing = value.ends_with('\\');
        }
    }
    keys
}

fn find<'a>(keys: &'a [RegKey], path: &str) -> Option<&'a RegKey> {
    keys.iter().find(|k| k.path.eq_ignore_ascii_case(path))
}

/// Why the automatic preparation did not complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrepError {
    /// A driver of the plan is not installed in the guest.
    NotInstalled(String),
    /// The helper refused or failed (its code and text).
    Helper(String, String),
    /// The helper did not finish (no result).
    NoResult,
    /// The keys read back do not show the edit.
    NotVerified(String),
}

impl PrepError {
    /// Message for the user.
    #[must_use]
    pub fn to_public(&self) -> PublicError {
        let manual = "or prepare the VM in VMware as described in the user guide (\u{201c}LSI Logic disks\u{201d}) and import it again";
        match self {
            Self::NotInstalled(s) => PublicError::unsupported(format!(
                "the Windows storage driver {s} is not installed in the guest, so LightVM cannot enable it: prepare the VM in VMware as described in the user guide (\u{201c}LSI Logic disks\u{201d}) and import it again"
            )),
            Self::Helper(code, text) => match code.as_str() {
                "hibernated" => PublicError::state_conflict(format!(
                    "Windows in the VMware VM is hibernated or was shut down with Fast Startup, so its disk cannot be prepared: start the VM in VMware, shut it down completely (Shift + Shut down, or `shutdown /s /t 0`) and import it again, {manual}"
                )),
                "unclean" | "hive_dirty" => PublicError::state_conflict(format!(
                    "Windows in the VMware VM was not shut down cleanly, so its disk cannot be prepared: start the VM in VMware, shut it down from Windows and import it again, {manual}"
                )),
                "no_windows" => PublicError::unsupported(format!(
                    "no Windows installation was found on the disk, so its storage driver cannot be prepared: {manual}"
                )),
                "several_windows" => PublicError::unsupported(format!(
                    "the disk holds more than one Windows installation, so LightVM does not choose which to prepare: {manual}"
                )),
                _ => PublicError::infrastructure(format!(
                    "the storage driver preparation failed ({code}: {}): {manual}",
                    crate::shown(text)
                )),
            },
            Self::NoResult => PublicError::infrastructure(format!(
                "the storage driver preparation did not complete: {manual}"
            )),
            Self::NotVerified(what) => PublicError::infrastructure(format!(
                "the storage driver preparation could not be verified ({what}): {manual}"
            )),
        }
    }
}

/// What the inspection found and the edit needs.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Inspected {
    /// Per service, the `StartOverride` value names present (Windows 8+).
    pub start_overrides: Vec<(String, Vec<String>)>,
}

fn is_dword(v: Option<&str>, n: u32) -> bool {
    v.and_then(|v| v.strip_prefix("dword:"))
        .and_then(|h| u32::from_str_radix(h.trim(), 16).ok())
        == Some(n)
}

/// A `StartOverride` value name the `.reg` may name back: short, digits and
/// letters only (Windows uses `0`, `1`, ...).
fn safe_value_name(n: &str) -> bool {
    !n.is_empty() && n.len() <= 16 && n.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// Checks the inspection exports (per control set): every service of the
/// plan is installed as a kernel driver (`Type`=1). Returns the
/// `StartOverride` values to reset.
pub fn check_inspection(
    plan: &DriverPlan,
    exports: &[(String, String)],
) -> Result<Inspected, PrepError> {
    if exports.is_empty() {
        return Err(PrepError::NotVerified("no control set read".into()));
    }
    let mut out = Inspected::default();
    for (cs, text) in exports {
        let keys = parse_export(text, cs);
        for s in plan.services {
            let k = find(&keys, &format!("{SERVICES}{s}"))
                .ok_or_else(|| PrepError::NotInstalled((*s).to_owned()))?;
            if !is_dword(k.value("Type"), 1) {
                return Err(PrepError::NotInstalled((*s).to_owned()));
            }
            if plan.start_override
                && let Some(o) = find(&keys, &format!("{SERVICES}{s}\\StartOverride"))
            {
                let names: Vec<String> = o.values.iter().map(|(n, _)| n.clone()).collect();
                if let Some(bad) = names.iter().find(|n| !safe_value_name(n)) {
                    return Err(PrepError::NotVerified(format!(
                        "unexpected StartOverride value {}",
                        crate::shown(bad)
                    )));
                }
                match out.start_overrides.iter_mut().find(|(n, _)| n == s) {
                    Some((_, v)) => {
                        for n in names {
                            if !v.contains(&n) {
                                v.push(n);
                            }
                        }
                    }
                    None => out.start_overrides.push(((*s).to_owned(), names)),
                }
            }
        }
    }
    Ok(out)
}

/// The `.reg` the helper imports (`CurrentControlSet` is replaced by the
/// helper with the guest's current and default control sets).
#[must_use]
pub fn job_reg(plan: &DriverPlan, insp: &Inspected) -> String {
    let root = "HKEY_LOCAL_MACHINE\\SYSTEM\\CurrentControlSet\\";
    let mut r = String::from("Windows Registry Editor Version 5.00\r\n");
    for s in plan.services {
        r.push_str(&format!(
            "\r\n[{root}{SERVICES}{s}]\r\n\"Start\"=dword:00000000\r\n"
        ));
        if let Some((_, names)) = insp.start_overrides.iter().find(|(n, _)| n == s)
            && !names.is_empty()
        {
            r.push_str(&format!("\r\n[{root}{SERVICES}{s}\\StartOverride]\r\n"));
            for n in names {
                r.push_str(&format!("\"{n}\"=dword:00000000\r\n"));
            }
        }
    }
    for (dev, svc) in plan.critical_devices {
        r.push_str(&format!(
            "\r\n[{root}{CDDB}{dev}]\r\n\"ClassGUID\"=\"{HDC_CLASS_GUID}\"\r\n\"Service\"=\"{svc}\"\r\n"
        ));
    }
    r
}

/// Checks the exports after the edit: in every control set the services
/// start at boot, their `StartOverride` values are 0 and the critical
/// device entries name the right service and class.
pub fn verify(plan: &DriverPlan, exports: &[(String, String)]) -> Result<(), PrepError> {
    if exports.is_empty() {
        return Err(PrepError::NotVerified("no control set read".into()));
    }
    let bad = |m: String| Err(PrepError::NotVerified(m));
    for (cs, text) in exports {
        let keys = parse_export(text, cs);
        for s in plan.services {
            let Some(k) = find(&keys, &format!("{SERVICES}{s}")) else {
                return bad(format!("{s} missing in {cs}"));
            };
            if !is_dword(k.value("Start"), 0) || !is_dword(k.value("Type"), 1) {
                return bad(format!("{s} does not start at boot in {cs}"));
            }
            if plan.start_override
                && let Some(o) = find(&keys, &format!("{SERVICES}{s}\\StartOverride"))
                && o.values.iter().any(|(_, v)| !is_dword(Some(v), 0))
            {
                return bad(format!("StartOverride of {s} not reset in {cs}"));
            }
        }
        for (dev, svc) in plan.critical_devices {
            let Some(k) = find(&keys, &format!("{CDDB}{dev}")) else {
                return bad(format!("critical device {dev} missing in {cs}"));
            };
            let str_is = |name: &str, want: &str| {
                k.value(name)
                    .and_then(|v| v.strip_prefix('"'))
                    .and_then(|v| v.strip_suffix('"'))
                    .is_some_and(|v| v.eq_ignore_ascii_case(want))
            };
            if !str_is("Service", svc) || !str_is("ClassGUID", HDC_CLASS_GUID) {
                return bad(format!("critical device {dev} wrong in {cs}"));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The export of the inspection of the real Vista x86 guest of R6-02
    /// (`Services\msahci` as `reged -x` printed it).
    const VISTA_MSAHCI: &str = "Windows Registry Editor Version 5.00\n\n\
[HKEY_LOCAL_MACHINE\\SYSTEM\\ControlSet001\\Services\\msahci]\n\
\"Group\"=\"SCSI Miniport\"\n\
\"ImagePath\"=hex(2):5c,00,53,00,79,00,73,00,74,00,65,00,6d,00,52,00,6f,00,6f,00,\\\n\
  74,00,5c,00,73,00,79,00,73,00,74,00,65,00,6d,00,33,00,32,00,5c,00,64,00,72,\\\n\
  2e,00,73,00,79,00,73,00,00,00\n\
\"ErrorControl\"=dword:00000003\n\
\"Start\"=dword:00000004\n\
\"Type\"=dword:00000001\n\n";

    fn ex(cs: &str, t: &str) -> Vec<(String, String)> {
        vec![(cs.to_owned(), t.to_owned())]
    }

    #[test]
    fn plans() {
        assert_eq!(
            driver_plan(GuestOs::WindowsVista, DiskController::Ahci),
            Some(VISTA_AHCI)
        );
        assert_eq!(
            driver_plan(GuestOs::Windows7, DiskController::Ahci),
            Some(VISTA_AHCI)
        );
        assert_eq!(
            driver_plan(GuestOs::Windows10, DiskController::Ahci),
            Some(WIN8_AHCI)
        );
        assert_eq!(
            driver_plan(GuestOs::Windows81, DiskController::Ahci),
            Some(WIN8_AHCI)
        );
        assert_eq!(
            driver_plan(GuestOs::WindowsXp, DiskController::Ide),
            Some(XP_IDE)
        );
        assert_eq!(driver_plan(GuestOs::WindowsXp, DiskController::Ahci), None);
        assert_eq!(
            driver_plan(GuestOs::OtherLinuxX86_64, DiskController::Virtio),
            None
        );
        assert_eq!(driver_plan(GuestOs::Windows10, DiskController::Nvme), None);
        let v = VISTA_AHCI.verify_keys();
        assert_eq!(
            v,
            vec![
                "Services\\msahci".to_owned(),
                "Control\\CriticalDeviceDatabase\\PCI#VEN_8086&DEV_2922&CC_010601".to_owned()
            ]
        );
    }

    #[test]
    fn export_parsing() {
        let k = parse_export(VISTA_MSAHCI, "ControlSet001");
        assert_eq!(k.len(), 1);
        assert_eq!(k[0].path, "Services\\msahci");
        assert_eq!(k[0].value("start"), Some("dword:00000004"));
        assert!(k[0].value("ImagePath").unwrap().ends_with("00,00"));
        assert_eq!(k[0].value("Type"), Some("dword:00000001"));
        // Another control set's keys are ignored.
        assert!(parse_export(VISTA_MSAHCI, "ControlSet002").is_empty());
        // Garbage does not panic.
        assert!(parse_export("[\n\"=\n\\\n[HKEY", "ControlSet001").is_empty());
    }

    #[test]
    fn vista_job_and_verification() {
        let insp = check_inspection(&VISTA_AHCI, &ex("ControlSet001", VISTA_MSAHCI)).unwrap();
        let reg = job_reg(&VISTA_AHCI, &insp);
        assert!(reg.starts_with("Windows Registry Editor Version 5.00\r\n"));
        assert!(reg.contains(
            "[HKEY_LOCAL_MACHINE\\SYSTEM\\CurrentControlSet\\Services\\msahci]\r\n\"Start\"=dword:00000000\r\n"
        ));
        assert!(reg.contains(
            "[HKEY_LOCAL_MACHINE\\SYSTEM\\CurrentControlSet\\Control\\CriticalDeviceDatabase\\PCI#VEN_8086&DEV_2922&CC_010601]\r\n\"ClassGUID\"=\"{4D36E96A-E325-11CE-BFC1-08002BE10318}\"\r\n\"Service\"=\"msahci\"\r\n"
        ));
        // Before the edit the verification fails (Start=4, no entry).
        assert!(verify(&VISTA_AHCI, &ex("ControlSet001", VISTA_MSAHCI)).is_err());
        let after = VISTA_MSAHCI.replace("dword:00000004", "dword:00000000")
            + "Windows Registry Editor Version 5.00\n\n\
[HKEY_LOCAL_MACHINE\\SYSTEM\\ControlSet001\\Control\\CriticalDeviceDatabase\\PCI#VEN_8086&DEV_2922&CC_010601]\n\
\"ClassGUID\"=\"{4D36E96A-E325-11CE-BFC1-08002BE10318}\"\n\"Service\"=\"msahci\"\n\n";
        verify(&VISTA_AHCI, &ex("ControlSet001", &after)).unwrap();
        // A wrong service in the entry is caught.
        let wrong = after.replace("\"Service\"=\"msahci\"", "\"Service\"=\"iaStorV\"");
        assert!(verify(&VISTA_AHCI, &ex("ControlSet001", &wrong)).is_err());
    }

    #[test]
    fn missing_driver_is_refused() {
        assert_eq!(
            check_inspection(&VISTA_AHCI, &ex("ControlSet001", "")),
            Err(PrepError::NotInstalled("msahci".into()))
        );
        // A key without Type=1 (for example only the Start value an
        // earlier tool wrote) is not an installed driver.
        let t = "[HKEY_LOCAL_MACHINE\\SYSTEM\\ControlSet001\\Services\\msahci]\n\"Start\"=dword:00000000\n";
        assert!(check_inspection(&VISTA_AHCI, &ex("ControlSet001", t)).is_err());
        assert!(check_inspection(&VISTA_AHCI, &[]).is_err());
    }

    #[test]
    fn windows10_start_override() {
        let t = "[HKEY_LOCAL_MACHINE\\SYSTEM\\ControlSet001\\Services\\storahci]\n\"Start\"=dword:00000003\n\"Type\"=dword:00000001\n\n\
[HKEY_LOCAL_MACHINE\\SYSTEM\\ControlSet001\\Services\\storahci\\StartOverride]\n\"0\"=dword:00000003\n\n";
        let insp = check_inspection(&WIN8_AHCI, &ex("ControlSet001", t)).unwrap();
        assert_eq!(
            insp.start_overrides,
            vec![("storahci".to_owned(), vec!["0".to_owned()])]
        );
        let reg = job_reg(&WIN8_AHCI, &insp);
        assert!(reg.contains("\\Services\\storahci\\StartOverride]\r\n\"0\"=dword:00000000\r\n"));
        assert!(!reg.contains("CriticalDeviceDatabase"));
        assert!(verify(&WIN8_AHCI, &ex("ControlSet001", t)).is_err());
        let after = t.replace("dword:00000003", "dword:00000000");
        verify(&WIN8_AHCI, &ex("ControlSet001", &after)).unwrap();
        // Without StartOverride only Start is set.
        let t2 = "[HKEY_LOCAL_MACHINE\\SYSTEM\\ControlSet001\\Services\\storahci]\n\"Start\"=dword:00000003\n\"Type\"=dword:00000001\n";
        let insp = check_inspection(&WIN8_AHCI, &ex("ControlSet001", t2)).unwrap();
        assert!(!job_reg(&WIN8_AHCI, &insp).contains("StartOverride"));
        // A hostile value name is not echoed into the .reg.
        let t3 = format!(
            "{t2}\n[HKEY_LOCAL_MACHINE\\SYSTEM\\ControlSet001\\Services\\storahci\\StartOverride]\n\"0]\\r\\n[x\"=dword:00000003\n"
        );
        assert!(check_inspection(&WIN8_AHCI, &ex("ControlSet001", &t3)).is_err());
    }

    #[test]
    fn messages() {
        let m = PrepError::Helper("hibernated".into(), "x".into()).to_public();
        assert!(m.message.contains("Fast Startup"), "{}", m.message);
        let m = PrepError::NotInstalled("msahci".into()).to_public();
        assert!(m.message.contains("msahci"));
        let m = PrepError::Helper("weird".into(), "a\u{7}b".into()).to_public();
        assert!(!m.message.contains('\u{7}'));
    }
}
