//! `.vmx` reader: `key = "value"` lines, known keys only.
//!
//! VMware writes one `key = "value"` per line; keys are case-insensitive,
//! values are quoted and escape `"`, `|` and control characters as `|XX`
//! (two hexadecimal digits). Rules applied to the untrusted text:
//!
//! - at most [`crate::MAX_VMX_BYTES`] bytes, at most [`MAX_LINES`] lines,
//!   lines up to [`MAX_LINE_BYTES`];
//! - a line that is not `key = "value"` (comments, blank lines, anything
//!   else) is ignored;
//! - keys outside [`is_known_key`] are ignored and never stored;
//! - values longer than [`MAX_VALUE_CHARS`] are refused;
//! - a key written twice keeps the last value (as VMware does);
//! - the text must be UTF-8 (VMware's default `.encoding`); a NUL byte
//!   refuses the file.

use std::collections::BTreeMap;

use crate::ImportError;

/// Most lines read.
pub const MAX_LINES: usize = 50_000;
/// Longest line.
pub const MAX_LINE_BYTES: usize = 64 << 10;
/// Longest value kept.
pub const MAX_VALUE_CHARS: usize = 4096;
/// Highest controller index looked at (`scsi0`..`scsi3`, `sata0`..`sata3`,
/// `nvme0`..`nvme3`, `ide0`..`ide1`).
pub const MAX_CONTROLLERS: u32 = 4;
/// Highest unit index on a controller (SATA has 30 ports).
pub const MAX_UNITS: u32 = 64;

/// Known keys of a `.vmx`, lower case.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Vmx {
    keys: BTreeMap<String, String>,
    /// At least one `encryption.*` key is present.
    pub encrypted: bool,
    /// At least one `toolsInstallManager.*` key is present (hint that the
    /// VMware Tools were installed in the guest).
    pub tools_install_keys: bool,
}

impl Vmx {
    /// Value of a known key (lower case), if present.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.keys.get(key).map(String::as_str)
    }

    /// `true` if the key is `TRUE` (any case).
    #[must_use]
    pub fn is_true(&self, key: &str) -> bool {
        self.get(key).is_some_and(|v| v.eq_ignore_ascii_case("true"))
    }

    /// Unsigned number of a key; `None` if absent or not a number.
    #[must_use]
    pub fn number(&self, key: &str) -> Option<u64> {
        let v = self.get(key)?.trim();
        if v.is_empty() || v.len() > 12 || !v.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        v.parse().ok()
    }

    /// Keys kept (lower case), for diagnostics and tests.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.keys.keys().map(String::as_str)
    }
}

/// Parses the bytes of a `.vmx`.
pub fn parse_vmx(bytes: &[u8]) -> Result<Vmx, ImportError> {
    if bytes.len() as u64 > crate::MAX_VMX_BYTES {
        return Err(ImportError::TooLarge("the .vmx".into()));
    }
    if bytes.contains(&0) {
        return Err(ImportError::InvalidVmx("binary content"));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| ImportError::InvalidVmx("not UTF-8 text"))?;
    let mut out = Vmx::default();
    for (n, raw) in text.split('\n').enumerate() {
        if n >= MAX_LINES {
            return Err(ImportError::InvalidVmx("too many lines"));
        }
        if raw.len() > MAX_LINE_BYTES {
            return Err(ImportError::InvalidVmx("line too long"));
        }
        let Some((key, value)) = split_line(raw.trim_end_matches('\r')) else {
            continue;
        };
        let key = key.to_ascii_lowercase();
        if key.starts_with("encryption.") {
            out.encrypted = true;
            continue;
        }
        if key.starts_with("toolsinstallmanager.") {
            out.tools_install_keys = true;
            continue;
        }
        if !is_known_key(&key) {
            continue;
        }
        let value = unescape(value)?;
        if value.chars().count() > MAX_VALUE_CHARS {
            return Err(ImportError::InvalidVmx("value too long"));
        }
        out.keys.insert(key, value);
    }
    Ok(out)
}

/// `key = "value"`: key of `[A-Za-z0-9._:-]`, value between double quotes
/// with nothing after the closing quote but blanks.
fn split_line(line: &str) -> Option<(&str, &str)> {
    let (k, rest) = line.split_once('=')?;
    let k = k.trim();
    if k.is_empty()
        || k.len() > 128
        || !k
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
    {
        return None;
    }
    let rest = rest.trim();
    let inner = rest.strip_prefix('"')?;
    let end = inner.find('"')?;
    if !inner[end + 1..].trim().is_empty() {
        return None;
    }
    Some((k, &inner[..end]))
}

/// Decodes VMware's `|XX` escapes; refuses control characters other than
/// tab in the result.
fn unescape(v: &str) -> Result<String, ImportError> {
    let b = v.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'|' {
            let hex = b
                .get(i + 1..i + 3)
                .and_then(|h| std::str::from_utf8(h).ok())
                .and_then(|h| u8::from_str_radix(h, 16).ok())
                .ok_or(ImportError::InvalidVmx("malformed escape in a value"))?;
            out.push(hex);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    let s = String::from_utf8(out).map_err(|_| ImportError::InvalidVmx("value not UTF-8"))?;
    if s.chars().any(|c| c.is_control() && c != '\t') {
        return Err(ImportError::InvalidVmx("control character in a value"));
    }
    Ok(s)
}

const EXACT_KEYS: &[&str] = &[
    "guestos",
    "displayname",
    "numvcpus",
    "cpuid.corespersocket",
    "memsize",
    "firmware",
    "checkpoint.vmstate",
    "cleanshutdown",
    "bios.hddorder",
    "mks.enable3d",
    "sound.present",
    "usb.present",
    "ehci.present",
    "usb_xhci.present",
    "floppy0.present",
    "sharedfolder.maxnum",
    "printer.present",
    "tools.remindinstall",
    "virtualhw.version",
];

/// Whether a lower-case key is one LightVM reads.
#[must_use]
pub fn is_known_key(k: &str) -> bool {
    if EXACT_KEYS.contains(&k) {
        return true;
    }
    // ethernetN.present, serialN.present, parallelN.present,
    // sharedFolderN.present / .enabled.
    for (prefix, props) in [
        ("ethernet", &["present"][..]),
        ("serial", &["present"][..]),
        ("parallel", &["present"][..]),
        ("sharedfolder", &["present", "enabled"][..]),
    ] {
        if let Some(rest) = k.strip_prefix(prefix)
            && let Some((n, prop)) = rest.split_once('.')
            && small_index(n, 16)
            && props.contains(&prop)
        {
            return true;
        }
    }
    device_key(k).is_some()
}

/// Kind of disk controller in a `.vmx`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Bus {
    /// `ideN`.
    Ide,
    /// `scsiN`.
    Scsi,
    /// `sataN`.
    Sata,
    /// `nvmeN`.
    Nvme,
}

impl Bus {
    /// Prefix in the `.vmx`.
    #[must_use]
    pub const fn prefix(self) -> &'static str {
        match self {
            Self::Ide => "ide",
            Self::Scsi => "scsi",
            Self::Sata => "sata",
            Self::Nvme => "nvme",
        }
    }

    /// Every bus, in the order disks are looked for when the `.vmx` does
    /// not say which disk boots (PC BIOS order: IDE first).
    pub const ALL: [Bus; 4] = [Bus::Ide, Bus::Sata, Bus::Scsi, Bus::Nvme];
}

/// Controller (`scsi0`) or device (`scsi0:1`) key: `(bus, controller,
/// unit, property)`.
fn device_key(k: &str) -> Option<(Bus, u32, Option<u32>, &str)> {
    let (head, prop) = k.split_once('.')?;
    let bus = Bus::ALL
        .into_iter()
        .find(|b| head.starts_with(b.prefix()))?;
    let rest = &head[bus.prefix().len()..];
    let (ctl, unit) = match rest.split_once(':') {
        Some((c, u)) => (c, Some(u)),
        None => (rest, None),
    };
    if !small_index(ctl, MAX_CONTROLLERS) {
        return None;
    }
    let ctl: u32 = ctl.parse().ok()?;
    let unit = match unit {
        Some(u) if small_index(u, MAX_UNITS) => Some(u.parse().ok()?),
        Some(_) => return None,
        None => None,
    };
    let ok = match unit {
        None => matches!(prop, "present" | "virtualdev"),
        Some(_) => matches!(prop, "present" | "filename" | "devicetype" | "mode"),
    };
    ok.then_some((bus, ctl, unit, prop))
}

/// Decimal index below `max` without leading zeros.
fn small_index(s: &str, max: u32) -> bool {
    !s.is_empty()
        && s.len() <= 2
        && s.bytes().all(|b| b.is_ascii_digit())
        && !(s.len() > 1 && s.starts_with('0'))
        && s.parse::<u32>().is_ok_and(|n| n < max)
}

/// A device of the `.vmx` with `present = "TRUE"` on a present controller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// Bus kind.
    pub bus: Bus,
    /// Controller index.
    pub controller: u32,
    /// Unit on the controller.
    pub unit: u32,
    /// `fileName` (raw value), if any.
    pub file_name: Option<String>,
    /// `deviceType` (lower case), if any.
    pub device_type: Option<String>,
    /// `mode` (lower case), if any.
    pub mode: Option<String>,
}

impl Device {
    /// VMware slot, e.g. `scsi0:0`.
    #[must_use]
    pub fn slot(&self) -> String {
        format!("{}{}:{}", self.bus.prefix(), self.controller, self.unit)
    }

    /// Whether the device is a hard disk backed by a VMDK file.
    #[must_use]
    pub fn is_hard_disk(&self) -> bool {
        let vmdk = self
            .file_name
            .as_deref()
            .is_some_and(|f| f.to_ascii_lowercase().ends_with(".vmdk"));
        let disk_type = match self.device_type.as_deref() {
            None => true,
            Some(t) => matches!(
                t,
                "disk" | "scsi-harddisk" | "ata-harddisk" | "nvme-harddisk" | "sata-harddisk"
            ),
        };
        vmdk && disk_type
    }

    /// Whether the device is a CD/DVD drive.
    #[must_use]
    pub fn is_cdrom(&self) -> bool {
        self.device_type
            .as_deref()
            .is_some_and(|t| t.contains("cdrom"))
    }
}

impl Vmx {
    /// Whether controller `bus`/`n` is present. IDE controllers are always
    /// there (board controllers); the others need `present = "TRUE"`.
    #[must_use]
    pub fn controller_present(&self, bus: Bus, n: u32) -> bool {
        bus == Bus::Ide || self.is_true(&format!("{}{n}.present", bus.prefix()))
    }

    /// `virtualDev` of SCSI controller `n` (lower case), if set.
    #[must_use]
    pub fn scsi_virtual_dev(&self, n: u32) -> Option<String> {
        self.get(&format!("scsi{n}.virtualdev"))
            .map(str::to_ascii_lowercase)
    }

    /// Present devices on present controllers, in bus/controller/unit order.
    #[must_use]
    pub fn devices(&self) -> Vec<Device> {
        let mut out = Vec::new();
        for (k, v) in &self.keys {
            let Some((bus, ctl, Some(unit), "present")) = device_key(k) else {
                continue;
            };
            if !v.eq_ignore_ascii_case("true") || !self.controller_present(bus, ctl) {
                continue;
            }
            let p = |prop: &str| {
                self.get(&format!("{}{ctl}:{unit}.{prop}", bus.prefix()))
                    .map(str::to_owned)
            };
            out.push(Device {
                bus,
                controller: ctl,
                unit,
                file_name: p("filename"),
                device_type: p("devicetype").map(|s| s.to_ascii_lowercase()),
                mode: p("mode").map(|s| s.to_ascii_lowercase()),
            });
        }
        out.sort_by_key(|d| (Bus::ALL.iter().position(|b| *b == d.bus), d.controller, d.unit));
        out
    }

    /// Indices `N` of `<prefix>N.present = "TRUE"` (network, serial,
    /// parallel, shared folders).
    #[must_use]
    pub fn present_indices(&self, prefix: &str) -> usize {
        (0..16)
            .filter(|n| self.is_true(&format!("{prefix}{n}.present")))
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_keys_only_and_escapes() {
        let v = parse_vmx(
            b".encoding = \"UTF-8\"\r\nguestOS = \"winvista\"\r\ndisplayName = \"A |22quoted|22 name\"\r\n\
              scsi0.present = \"TRUE\"\r\nscsi0.virtualDev = \"lsilogic\"\r\nscsi0:0.present = \"TRUE\"\r\n\
              scsi0:0.fileName = \"Disk.vmdk\"\r\nnot.a.known.key = \"x\"\r\n# comment = \"y\"\r\n\
              garbage line\r\nmemsize = 2048 \r\nnumvcpus = \"2\" trailing\r\n",
        )
        .unwrap();
        assert_eq!(v.get("guestos"), Some("winvista"));
        assert_eq!(v.get("displayname"), Some("A \"quoted\" name"));
        assert_eq!(v.get("not.a.known.key"), None);
        // Unquoted value and text after the quote: ignored.
        assert_eq!(v.get("memsize"), None);
        assert_eq!(v.get("numvcpus"), None);
        let d = v.devices();
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].slot(), "scsi0:0");
        assert!(d[0].is_hard_disk());
        assert_eq!(v.scsi_virtual_dev(0).as_deref(), Some("lsilogic"));
    }

    #[test]
    fn devices_need_a_present_controller() {
        let v = parse_vmx(
            b"sata0:0.present = \"TRUE\"\nsata0:0.fileName = \"a.vmdk\"\n\
              ide1:0.present = \"TRUE\"\nide1:0.deviceType = \"cdrom-image\"\nide1:0.fileName = \"x.iso\"\n",
        )
        .unwrap();
        let d = v.devices();
        assert_eq!(d.len(), 1, "sata0 is not present: {d:?}");
        assert!(d[0].is_cdrom());
    }

    #[test]
    fn hostile_text_is_refused_or_ignored() {
        assert!(matches!(
            parse_vmx(&vec![b'a'; (crate::MAX_VMX_BYTES + 1) as usize]),
            Err(ImportError::TooLarge(_))
        ));
        assert!(parse_vmx(b"guestOS = \"win\0vista\"").is_err());
        assert!(parse_vmx(&[0xff, 0xfe, b'a']).is_err());
        assert!(parse_vmx(b"displayName = \"|zz\"").is_err());
        assert!(parse_vmx(b"displayName = \"a|0Ab\"").is_err(), "newline escape");
        let long = format!("displayName = \"{}\"", "x".repeat(MAX_VALUE_CHARS + 1));
        assert!(parse_vmx(long.as_bytes()).is_err());
        // Unknown keys with odd shapes are ignored, not errors.
        let v = parse_vmx(b"scsi99:0.present = \"TRUE\"\nscsi0:007.present = \"TRUE\"\n").unwrap();
        assert_eq!(v.keys().count(), 0);
        let v = parse_vmx(b"encryption.keySafe = \"vmware:key/...\"\n").unwrap();
        assert!(v.encrypted);
    }
}
