//! Detection of host capabilities and real accelerator probe
//! (ROADMAP 3, 5, 10 F1; ADR-0007).
//!
//! A binary that was found does not prove that the VM can start: besides parsing
//! the output of `--version`, `-accel help`, `-machine help`, `-device help`,
//! `-cpu help` and of `qemu-img`, [`probe_accelerator`] starts QEMU paused with
//! the accelerator, machine and CPU model that will be used and verifies
//! via QMP `query-accelerators` which accelerator is active.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use crate::cmdline::{
    Accelerator, CpuModel, DeviceProfile, FirmwareKind, MachineFamily, MachineType, ProbeFirmware,
    ProbeTarget, build_probe_command, parse_versioned_machine,
};
use crate::error::{BackendError, Result};
use crate::paths::ManagedPath;
use crate::qmp::QmpConfig;
use lightvm_proto::RunId;

/// QEMU version.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct QemuVersion {
    /// Major.
    pub major: u32,
    /// Minor.
    pub minor: u32,
    /// Micro.
    pub micro: u32,
    /// Build description in parentheses, if present.
    pub package: Option<String>,
}

impl fmt::Display for QemuVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.micro)?;
        if let Some(p) = &self.package {
            write!(f, " ({p})")?;
        }
        Ok(())
    }
}

/// Entry of `-machine help`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MachineEntry {
    /// Name.
    pub name: String,
    /// Description.
    pub description: String,
    /// Name it is an alias of.
    pub alias_of: Option<String>,
    /// Deprecated.
    pub deprecated: bool,
    /// Default.
    pub default: bool,
}

/// UEFI firmware found next to the binary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FirmwareInfo {
    /// OVMF code.
    pub code: PathBuf,
    /// SHA-256 of the code.
    pub code_sha256: String,
    /// UEFI variables template.
    pub vars_template: PathBuf,
    /// SHA-256 of the variables template.
    pub vars_sha256: String,
    /// `true` if both hashes match those verified in F0-02.
    pub matches_verified: bool,
}

/// Hashes verified in F0-02 (2.3) for QEMU 11.1.0 from qemu.weilnetz.de.
pub const VERIFIED_OVMF_CODE_SHA256: &str =
    "33090cc07675baa5190d9f1e84bf5176b33bcbfa9bacac522961150cdb6dbb2a";
/// Verified hash of the UEFI variables template.
pub const VERIFIED_OVMF_VARS_SHA256: &str =
    "5d2ac383371b408398accee7ec27c8c09ea5b74a0de0ceea6513388b15be5d1e";

/// SHA-256 of `share/bios-256k.bin` (SeaBIOS) of the verified QEMU 11.1.0
/// build (F0-02, ADR-0027).
pub const VERIFIED_SEABIOS_SHA256: &str =
    "ae6f6aa973aaccc143f57aa960fb035fd9de4daee4ad0cd713322f8c259e7650";

/// File name of the SeaBIOS image in the `share` folder.
pub const SEABIOS_FILE: &str = "bios-256k.bin";

/// SeaBIOS image found next to the binary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BiosInfo {
    /// `share/bios-256k.bin`.
    pub path: PathBuf,
    /// SHA-256 of the image.
    pub sha256: String,
    /// `true` if the hash matches [`VERIFIED_SEABIOS_SHA256`].
    pub matches_verified: bool,
}

/// File names of the project's AArch64 edk2 build (ADR-0028,
/// `packaging/firmware/edk2-aarch64/build.sh`) in the firmware directory.
pub const AARCH64_CODE_FILE: &str = "lightvm-aarch64-code.fd";
/// Empty variables template (no Secure Boot keys).
pub const AARCH64_VARS_BLANK_FILE: &str = "lightvm-aarch64-vars-blank.fd";
/// Variables template for Windows guests: Secure Boot on, generated PK,
/// Microsoft KEK and Windows db (ADR-0028).
pub const AARCH64_VARS_WINDOWS_FILE: &str = "lightvm-aarch64-vars-win.fd";

/// SHA-256 of the released AArch64 firmware files. Empty: ADR-0028 pins
/// the hashes when the firmware is released; until then only the files
/// listed in [`ProbeConfig::approved_firmware_sha256`] (development
/// approvals given by configuration) are accepted.
pub const RELEASED_AARCH64_FIRMWARE_SHA256: &[&str] = &[];

/// One file of the AArch64 firmware, hashed by the probe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HashedFile {
    /// Absolute path.
    pub path: PathBuf,
    /// SHA-256 (lowercase hexadecimal).
    pub sha256: String,
    /// `true` if the hash is in the approved list.
    pub approved: bool,
}

/// AArch64 firmware found in the configured firmware directory (ADR-0028).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Aarch64FirmwareInfo {
    /// Code (`pflash0`, read-only).
    pub code: HashedFile,
    /// Empty variables template.
    pub vars_blank: HashedFile,
    /// Variables template for Windows guests (Secure Boot on).
    pub vars_windows: HashedFile,
}

impl Aarch64FirmwareInfo {
    /// `true` if every file has an approved hash.
    pub fn approved(&self) -> bool {
        self.code.approved && self.vars_blank.approved && self.vars_windows.approved
    }

    fn hashes(&self) -> Vec<String> {
        [&self.code, &self.vars_blank, &self.vars_windows]
            .iter()
            .map(|f| f.sha256.clone())
            .collect()
    }
}

/// TPM emulator found and hashed by the probe (ADR-0028).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SwtpmInfo {
    /// Executable (links resolved by the caller).
    pub path: PathBuf,
    /// SHA-256 of the executable.
    pub sha256: String,
    /// `true` if the hash is in [`ProbeConfig::approved_swtpm_sha256`]; an
    /// unapproved executable is never run (no version).
    pub approved: bool,
    /// First line of `swtpm --version` (approved executables only).
    pub version: Option<String>,
}

/// Required image formats.
pub const REQUIRED_IMAGE_FORMATS: &[&str] = &["qcow2", "raw", "file"];

/// Reason why the accelerator is not available.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UnavailableReason {
    /// The binary does not include the accelerator.
    NotCompiled {
        /// Accelerators listed by `-accel help`.
        available: Vec<String>,
    },
    /// QEMU rejected the accelerator at initialisation.
    InitFailed {
        /// stderr excerpt.
        stderr: String,
    },
    /// QEMU started but the active accelerator is a different one (for example TCG).
    WrongAccelerator {
        /// Active accelerator according to `query-accelerators`.
        enabled: String,
    },
    /// Incompatible configuration (machine or CPU rejected).
    IncompatibleConfiguration {
        /// stderr excerpt.
        stderr: String,
    },
    /// The probe did not complete.
    ProbeFailed {
        /// Description.
        detail: String,
    },
}

/// Outcome of the real accelerator probe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AcceleratorStatus {
    /// Accelerator active with the tested configuration.
    Available {
        /// Accelerator.
        accelerator: Accelerator,
        /// Machine used in the probe.
        machine: String,
        /// CPU model used in the probe.
        cpu: String,
    },
    /// Not available: no TCG fallback.
    Unavailable {
        /// Requested accelerator.
        accelerator: Accelerator,
        /// Reason.
        reason: UnavailableReason,
    },
    /// Probe not yet run.
    NotProbed,
}

impl AcceleratorStatus {
    /// `true` if the accelerator is usable.
    pub fn is_available(&self) -> bool {
        matches!(self, Self::Available { .. })
    }

    /// Message for the user without stderr excerpts (which may contain
    /// paths): short reason and hint, always with the statement
    /// that TCG is not used.
    pub fn public_message(&self) -> String {
        match self {
            Self::Available { .. } | Self::NotProbed => self.to_string(),
            Self::Unavailable {
                accelerator,
                reason,
            } => {
                let a = accelerator.qemu_name().to_uppercase();
                let what = match reason {
                    UnavailableReason::NotCompiled { .. } => {
                        format!("{a} is not included in the QEMU binary")
                    }
                    UnavailableReason::InitFailed { .. } => {
                        format!("{a} did not initialise")
                    }
                    UnavailableReason::WrongAccelerator { enabled } => {
                        format!("QEMU started with `{enabled}` instead of {a}")
                    }
                    UnavailableReason::IncompatibleConfiguration { .. } => {
                        format!("machine or CPU model rejected by QEMU with {a}")
                    }
                    UnavailableReason::ProbeFailed { .. } => {
                        format!("verification of {a} not completed")
                    }
                };
                format!(
                    "hardware accelerator not available: {what}. Launch refused: no \
                     fallback to TCG emulation. {}",
                    accel_hint(*accelerator)
                )
            }
        }
    }
}

fn accel_hint(a: Accelerator) -> &'static str {
    match a {
        Accelerator::Whpx => {
            "check that the Windows feature \"Windows Hypervisor Platform\" \
             (HypervisorPlatform) is enabled, that virtualization is enabled in the \
             firmware and that the QEMU binary includes WHPX; LightVM does not use TCG emulation"
        }
        Accelerator::Kvm => {
            "check that /dev/kvm exists and is accessible to the user (kvm group) and that \
             virtualization is enabled in the firmware; LightVM does not use TCG emulation"
        }
        Accelerator::Hvf => {
            "check that the QEMU binary includes HVF and that the host supports it; LightVM does \
             not use TCG emulation"
        }
    }
}

impl fmt::Display for AcceleratorStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Available {
                accelerator,
                machine,
                cpu,
            } => write!(
                f,
                "accelerator {} active (machine {machine}, CPU {cpu})",
                accelerator.qemu_name()
            ),
            Self::NotProbed => write!(f, "accelerator not yet verified"),
            Self::Unavailable {
                accelerator,
                reason,
            } => {
                let a = accelerator.qemu_name().to_uppercase();
                match reason {
                    UnavailableReason::NotCompiled { available } => write!(
                        f,
                        "{a} is not included in this QEMU binary (accelerators present: {}): {}",
                        available.join(", "),
                        accel_hint(*accelerator)
                    ),
                    UnavailableReason::InitFailed { stderr } => write!(
                        f,
                        "{a} not initialised: {}. QEMU message: {stderr}",
                        accel_hint(*accelerator)
                    ),
                    UnavailableReason::WrongAccelerator { enabled } => write!(
                        f,
                        "QEMU started with the `{enabled}` accelerator instead of {a}: launch \
                         refused (no TCG fallback)"
                    ),
                    UnavailableReason::IncompatibleConfiguration { stderr } => write!(
                        f,
                        "configuration rejected by QEMU with {a} (machine or CPU model): \
                         {stderr}"
                    ),
                    UnavailableReason::ProbeFailed { detail } => {
                        write!(f, "probe of {a} not completed: {detail}")
                    }
                }
            }
        }
    }
}

/// Detected capabilities.
#[derive(Clone, Debug)]
pub struct HostCapabilities {
    /// Approved `qemu-system-x86_64` binary.
    pub qemu_system: ManagedPath,
    /// SHA-256 of the binary.
    pub qemu_system_sha256: String,
    /// `qemu-img` binary.
    pub qemu_img: ManagedPath,
    /// SHA-256 of `qemu-img`.
    pub qemu_img_sha256: String,
    /// QEMU version.
    pub version: QemuVersion,
    /// `qemu-img` version.
    pub qemu_img_version: QemuVersion,
    /// Compiled-in accelerators.
    pub accelerators: Vec<String>,
    /// All listed machines.
    pub machines: Vec<MachineEntry>,
    /// Non-deprecated versioned q35 machines, newest first.
    pub q35_machines: Vec<MachineType>,
    /// Non-deprecated versioned i440fx machines, newest first.
    pub i440fx_machines: Vec<MachineType>,
    /// Non-deprecated versioned AArch64 `virt` machines, newest first.
    pub virt_machines: Vec<MachineType>,
    /// Devices listed by `-device help` (names and aliases); see
    /// [`HostCapabilities::missing_devices`].
    pub devices: BTreeSet<String>,
    /// Available CPU models.
    pub cpu_models: BTreeSet<String>,
    /// Formats supported by `qemu-img`.
    pub image_formats: BTreeSet<String>,
    /// Remote protocols present in the build (excluded by the allowlist).
    pub remote_protocols_present: Vec<String>,
    /// UEFI firmware.
    pub firmware: Option<FirmwareInfo>,
    /// SeaBIOS image.
    pub seabios: Option<BiosInfo>,
    /// AArch64 firmware of the project (ADR-0028), from the configured
    /// firmware directory.
    pub aarch64_firmware: Option<Aarch64FirmwareInfo>,
    /// TPM emulator (ADR-0028).
    pub swtpm: Option<SwtpmInfo>,
    /// Where the probe looked for the AArch64 firmware and swtpm, and the
    /// approved hashes: used again by [`current_fingerprint`].
    pub inputs: ProbeInputs,
    /// Outcome of the real accelerator probe for the host reference
    /// ([`ProbeTarget::host_reference`]: q35 + UEFI on x86 hosts, `virt` +
    /// AArch64 UEFI on Apple Silicon).
    pub accelerator: AcceleratorStatus,
    /// Outcomes of the accelerator probes for the other machine family /
    /// firmware combinations, one per combination (ADR-0007: the probe is
    /// verified with the configuration actually used). Read and written
    /// through [`HostCapabilities::accelerator_for`] and
    /// [`HostCapabilities::record_accelerator`].
    pub profile_accelerators: BTreeMap<ProbeTarget, AcceleratorStatus>,
}

/// Inputs of the probe beyond the two QEMU binaries (ADR-0028).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProbeInputs {
    /// Directory of the AArch64 firmware files.
    pub firmware_dir: Option<PathBuf>,
    /// swtpm executable.
    pub swtpm: Option<PathBuf>,
    /// Approved firmware hashes.
    pub approved_firmware_sha256: Vec<String>,
    /// Approved swtpm hashes.
    pub approved_swtpm_sha256: Vec<String>,
}

/// Problem detected by the probe, with a message for the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    /// Short stable code.
    pub code: &'static str,
    /// Message for the user.
    pub message: String,
}

impl HostCapabilities {
    /// Non-deprecated versioned machines of `family`, newest first.
    pub fn machines_of(&self, family: MachineFamily) -> &[MachineType] {
        match family {
            MachineFamily::Q35 => &self.q35_machines,
            MachineFamily::I440fx => &self.i440fx_machines,
            MachineFamily::Virt => &self.virt_machines,
        }
    }

    /// Recommended versioned machine of `family` (the newest non-deprecated one).
    pub fn recommended_machine(&self, family: MachineFamily) -> Option<&MachineType> {
        self.machines_of(family).first()
    }

    /// Devices of `profile` not listed by `-device help`: a start with that
    /// profile is refused, the other profiles are not affected.
    pub fn missing_devices(&self, profile: &DeviceProfile) -> Vec<&'static str> {
        profile
            .required_devices()
            .into_iter()
            .filter(|d| !self.devices.contains(*d))
            .collect()
    }

    /// Outcome of the accelerator probe for `target` (`NotProbed` if that
    /// combination was never probed).
    pub fn accelerator_for(&self, target: ProbeTarget) -> &AcceleratorStatus {
        if target == ProbeTarget::host_reference() {
            &self.accelerator
        } else {
            self.profile_accelerators
                .get(&target)
                .unwrap_or(&AcceleratorStatus::NotProbed)
        }
    }

    /// Records the outcome of the accelerator probe for `target` (cache per
    /// combination; [`ProbeTarget::host_reference`] is stored in
    /// `accelerator`).
    pub fn record_accelerator(&mut self, target: ProbeTarget, status: AcceleratorStatus) {
        if target == ProbeTarget::host_reference() {
            self.accelerator = status;
        } else {
            self.profile_accelerators.insert(target, status);
        }
    }

    /// Firmware image for the accelerator probe of `kind`: the OVMF code,
    /// or the SeaBIOS image only if its hash is the verified one.
    pub fn probe_firmware(&self, kind: FirmwareKind) -> Result<ProbeFirmware> {
        match kind {
            FirmwareKind::Uefi => {
                let fw = self
                    .firmware
                    .as_ref()
                    .ok_or_else(|| BackendError::Unsupported {
                        what: "UEFI firmware".into(),
                        hint: "edk2-x86_64-code.fd not found in the QEMU share folder".into(),
                    })?;
                Ok(ProbeFirmware::Uefi(ManagedPath::verify(fw.code.clone())?))
            }
            FirmwareKind::SeaBios => Ok(ProbeFirmware::SeaBios(ManagedPath::verify(
                self.verified_seabios()?.path.clone(),
            )?)),
            FirmwareKind::UefiAarch64 => Ok(ProbeFirmware::UefiAarch64(ManagedPath::verify(
                self.verified_aarch64_firmware()?.code.path.clone(),
            )?)),
        }
    }

    /// AArch64 firmware, only if every file has an approved hash
    /// (ADR-0028: a different file prevents the start).
    pub fn verified_aarch64_firmware(&self) -> Result<&Aarch64FirmwareInfo> {
        match &self.aarch64_firmware {
            None => Err(BackendError::Unsupported {
                what: "AArch64 UEFI firmware".into(),
                hint: format!(
                    "{AARCH64_CODE_FILE}, {AARCH64_VARS_BLANK_FILE} and \
                     {AARCH64_VARS_WINDOWS_FILE} not found in the firmware directory"
                ),
            }),
            Some(f) if !f.approved() => Err(BackendError::Unsupported {
                what: "AArch64 UEFI firmware".into(),
                hint: format!(
                    "firmware files with hashes not in the approved list (code {})",
                    f.code.sha256
                ),
            }),
            Some(f) => Ok(f),
        }
    }

    /// swtpm, only if found with an approved hash.
    pub fn verified_swtpm(&self) -> Result<&SwtpmInfo> {
        match &self.swtpm {
            None => Err(BackendError::Unsupported {
                what: "TPM emulator (swtpm)".into(),
                hint: "swtpm not found: install it next to QEMU or configure its path".into(),
            }),
            Some(t) if !t.approved => Err(BackendError::Unsupported {
                what: "TPM emulator (swtpm)".into(),
                hint: format!("swtpm with a hash not in the approved list ({})", t.sha256),
            }),
            Some(t) => Ok(t),
        }
    }

    /// Verifies that a CPU model (nominal or `host`, already validated by
    /// its constructor) is listed by `-cpu help` of this binary.
    pub fn check_cpu(&self, cpu: &CpuModel) -> Result<()> {
        if self.cpu_models.contains(cpu.as_str()) {
            Ok(())
        } else {
            Err(BackendError::Unsupported {
                what: format!("CPU model `{}`", cpu.as_str()),
                hint: "not listed by `-cpu help` of this binary".into(),
            })
        }
    }

    /// SeaBIOS image, only if its hash is [`VERIFIED_SEABIOS_SHA256`].
    pub fn verified_seabios(&self) -> Result<&BiosInfo> {
        match &self.seabios {
            None => Err(BackendError::Unsupported {
                what: "SeaBIOS firmware".into(),
                hint: format!("{SEABIOS_FILE} not found in the QEMU share folder"),
            }),
            Some(b) if !b.matches_verified => Err(BackendError::Unsupported {
                what: "SeaBIOS firmware".into(),
                hint: format!(
                    "{SEABIOS_FILE} has a hash different from the verified one ({})",
                    b.sha256
                ),
            }),
            Some(b) => Ok(b),
        }
    }

    /// Verified CPU model present in the binary.
    pub fn cpu_model(&self, name: &str) -> Result<CpuModel> {
        let m = CpuModel::nominal(name)?;
        if !self.cpu_models.contains(name) {
            return Err(BackendError::Unsupported {
                what: format!("CPU model `{name}`"),
                hint: "not listed by `-cpu help` of this binary".into(),
            });
        }
        Ok(m)
    }

    /// Versioned machine (q35, i440fx or virt) present, and not deprecated,
    /// in the binary; checked against the list of its own family.
    pub fn machine(&self, name: &str) -> Result<MachineType> {
        let m = MachineType::versioned(name)?;
        if !self.machines_of(m.family()).contains(&m) {
            return Err(BackendError::Unsupported {
                what: format!("machine `{name}`"),
                hint: "not listed (or deprecated) in `-machine help` of this binary".into(),
            });
        }
        Ok(m)
    }

    /// Problems that prevent starting a VM with the LightVM profiles of
    /// this host. Only the profiles the host can run are checked: x86
    /// machines and firmware are not flagged on Apple Silicon, the AArch64
    /// profile is not flagged on x86 hosts (ADR-0028).
    pub fn diagnostics(&self) -> Vec<Diagnostic> {
        self.diagnostics_for(&DeviceProfile::host_reference())
    }

    /// [`Self::diagnostics`] for the host whose reference profile is
    /// `reference` (x86 or AArch64).
    pub fn diagnostics_for(&self, reference: &DeviceProfile) -> Vec<Diagnostic> {
        let mut d = Vec::new();
        let aarch64 = reference.is_aarch64();
        if self.machines_of(reference.machine_family()).is_empty() {
            d.push(Diagnostic {
                code: "machine_missing",
                message: format!(
                    "no non-deprecated versioned {} machine available",
                    reference.machine_family().label()
                ),
            });
        }
        let missing = self.missing_devices(reference);
        if !missing.is_empty() {
            d.push(Diagnostic {
                code: "device_missing",
                message: format!(
                    "required devices missing from the binary: {}",
                    missing.join(", ")
                ),
            });
        }
        let mut other: Vec<&'static str> = DeviceProfile::ALL
            .iter()
            .filter(|p| p.is_aarch64() == aarch64)
            .flat_map(|p| self.missing_devices(p))
            .filter(|d| !missing.contains(d))
            .collect();
        other.sort_unstable();
        other.dedup();
        if !other.is_empty() {
            d.push(Diagnostic {
                code: "profile_device_missing",
                message: format!(
                    "devices of the Windows hardware profiles missing from the binary: {}",
                    other.join(", ")
                ),
            });
        }
        if aarch64 {
            self.aarch64_diagnostics(&mut d);
        } else {
            self.x86_diagnostics(&mut d);
        }
        let missing_fmt: Vec<_> = REQUIRED_IMAGE_FORMATS
            .iter()
            .filter(|f| !self.image_formats.contains(**f))
            .collect();
        if !missing_fmt.is_empty() {
            d.push(Diagnostic {
                code: "image_format_missing",
                message: format!("qemu-img does not support: {missing_fmt:?}"),
            });
        }
        if self.version != self.qemu_img_version {
            d.push(Diagnostic {
                code: "version_mismatch",
                message: format!(
                    "qemu-img {} differs from qemu-system {}",
                    self.qemu_img_version, self.version
                ),
            });
        }
        if !self.accelerator.is_available() {
            d.push(Diagnostic {
                code: "accelerator_unavailable",
                message: self.accelerator.to_string(),
            });
        }
        d
    }

    /// x86 hosts: i440fx machines, SeaBIOS and OVMF.
    fn x86_diagnostics(&self, d: &mut Vec<Diagnostic>) {
        if self.i440fx_machines.is_empty() {
            d.push(Diagnostic {
                code: "i440fx_machine_missing",
                message: "no non-deprecated versioned i440fx machine available (Windows XP \
                          profile)"
                    .into(),
            });
        }
        match &self.seabios {
            None => d.push(Diagnostic {
                code: "seabios_missing",
                message: format!(
                    "SeaBIOS ({SEABIOS_FILE}) not found in the QEMU share folder (legacy BIOS \
                     profiles)"
                ),
            }),
            Some(b) if !b.matches_verified => d.push(Diagnostic {
                code: "seabios_unverified",
                message: format!(
                    "SeaBIOS with a hash different from the verified one ({})",
                    b.sha256
                ),
            }),
            Some(_) => {}
        }
        match &self.firmware {
            None => d.push(Diagnostic {
                code: "firmware_missing",
                message: "UEFI firmware (edk2-x86_64-code.fd, edk2-i386-vars.fd) not found \
                          in the QEMU share folder"
                    .into(),
            }),
            Some(fw) if !fw.matches_verified => d.push(Diagnostic {
                code: "firmware_unverified",
                message: format!(
                    "UEFI firmware with a hash different from the verified one (code {})",
                    fw.code_sha256
                ),
            }),
            Some(_) => {}
        }
    }

    /// Apple Silicon (ADR-0028): project AArch64 firmware and swtpm.
    fn aarch64_diagnostics(&self, d: &mut Vec<Diagnostic>) {
        match &self.aarch64_firmware {
            None => d.push(Diagnostic {
                code: "firmware_missing",
                message: format!(
                    "AArch64 UEFI firmware ({AARCH64_CODE_FILE}, {AARCH64_VARS_BLANK_FILE}, \
                     {AARCH64_VARS_WINDOWS_FILE}) not found in the firmware directory"
                ),
            }),
            Some(fw) if !fw.approved() => d.push(Diagnostic {
                code: "firmware_unverified",
                message: format!(
                    "AArch64 UEFI firmware with hashes not in the approved list (code {})",
                    fw.code.sha256
                ),
            }),
            Some(_) => {}
        }
        match &self.swtpm {
            None => d.push(Diagnostic {
                code: "swtpm_missing",
                message: "TPM emulator (swtpm) not found (Windows 11 profile)".into(),
            }),
            Some(t) if !t.approved => d.push(Diagnostic {
                code: "swtpm_unverified",
                message: format!("swtpm with a hash not in the approved list ({})", t.sha256),
            }),
            Some(_) => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Parsers (real QEMU 11.1.0 output in the `tests/fixtures/` fixtures).
// ---------------------------------------------------------------------------

fn parse_version_after(text: &str, marker: &str) -> Option<QemuVersion> {
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix(marker) {
            let rest = rest.trim();
            let (ver, tail) = match rest.split_once(char::is_whitespace) {
                Some((v, t)) => (v, t.trim()),
                None => (rest, ""),
            };
            let mut it = ver.split('.');
            let major = it.next()?.parse().ok()?;
            let minor = it.next()?.parse().ok()?;
            let micro = it.next().unwrap_or("0").parse().ok()?;
            let package = tail
                .strip_prefix('(')
                .and_then(|t| t.strip_suffix(')'))
                .map(str::to_owned);
            return Some(QemuVersion {
                major,
                minor,
                micro,
                package,
            });
        }
    }
    None
}

/// `qemu-system-x86_64 --version`.
pub fn parse_qemu_version(text: &str) -> Option<QemuVersion> {
    parse_version_after(text, "QEMU emulator version")
}

/// `qemu-img --version`.
pub fn parse_qemu_img_version(text: &str) -> Option<QemuVersion> {
    parse_version_after(text, "qemu-img version")
}

/// `-accel help`.
pub fn parse_accel_help(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.ends_with(':'))
        .filter(|l| {
            l.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        })
        .map(str::to_owned)
        .collect()
}

/// `-machine help`.
pub fn parse_machine_help(text: &str) -> Vec<MachineEntry> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim_end();
        if line.is_empty() || line.ends_with(':') || line.starts_with(' ') {
            continue;
        }
        let (name, desc) = match line.split_once(char::is_whitespace) {
            Some((n, d)) => (n, d.trim()),
            None => (line, ""),
        };
        let alias_of = desc
            .find("(alias of ")
            .and_then(|i| desc[i + 10..].split(')').next())
            .map(str::to_owned);
        out.push(MachineEntry {
            name: name.to_owned(),
            description: desc.to_owned(),
            alias_of,
            deprecated: desc.contains("(deprecated)"),
            default: desc.contains("(default)"),
        });
    }
    out
}

/// Non-deprecated versioned q35 machines, newest first.
pub fn q35_versioned(machines: &[MachineEntry]) -> Vec<MachineType> {
    versioned_machines(machines, MachineFamily::Q35)
}

/// Non-deprecated versioned machines of `family`, newest first (aliases
/// such as `q35` and `pc` are not versioned names and are skipped).
pub fn versioned_machines(machines: &[MachineEntry], family: MachineFamily) -> Vec<MachineType> {
    let mut v: Vec<MachineType> = machines
        .iter()
        .filter(|m| !m.deprecated)
        .filter(|m| matches!(parse_versioned_machine(&m.name), Some((f, _, _)) if f == family))
        .filter_map(|m| MachineType::versioned_in(family, &m.name).ok())
        .collect();
    v.sort_by_key(|m| std::cmp::Reverse(m.version()));
    v
}

/// `-device help`: names and aliases.
pub fn parse_device_help(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for line in text.lines() {
        for key in ["name \"", "alias \""] {
            let mut rest = line;
            while let Some(i) = rest.find(key) {
                let after = &rest[i + key.len()..];
                if let Some(end) = after.find('"') {
                    out.insert(after[..end].to_owned());
                    rest = &after[end..];
                } else {
                    break;
                }
            }
        }
    }
    out
}

/// `-cpu help`: models of the "Available CPUs" section.
pub fn parse_cpu_help(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut in_list = false;
    for line in text.lines() {
        let line = line.trim_end();
        if !in_list {
            if line.trim_start().starts_with("Available CPUs") {
                in_list = true;
            }
            continue;
        }
        if line.trim().is_empty() {
            break;
        }
        if let Some(name) = line.split_whitespace().next() {
            out.insert(name.to_owned());
        }
    }
    out
}

/// `qemu-img --help`: formats of the "Supported image formats" section.
pub fn parse_qemu_img_formats(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut in_list = false;
    for line in text.lines() {
        let t = line.trim();
        if !in_list {
            if t.starts_with("Supported image formats") {
                in_list = true;
            }
            continue;
        }
        if t.is_empty() {
            if out.is_empty() {
                continue;
            }
            break;
        }
        out.extend(t.split_whitespace().map(str::to_owned));
    }
    out
}

/// Classifies the stderr of a failed launch with the requested accelerator.
pub fn classify_accel_stderr(accel: Accelerator, stderr: &str) -> UnavailableReason {
    let excerpt = stderr_excerpt(stderr);
    let lower = stderr.to_ascii_lowercase();
    let name = accel.qemu_name();
    if lower.contains(&format!("invalid accelerator {name}")) {
        return UnavailableReason::NotCompiled { available: vec![] };
    }
    if lower.contains(&format!("failed to initialize {name}"))
        || lower.contains(&format!("{name}:"))
        || lower.contains("no accelerator found")
    {
        return UnavailableReason::InitFailed { stderr: excerpt };
    }
    if lower.contains("unable to find cpu model")
        || lower.contains("unsupported machine type")
        || lower.contains("not supported by")
    {
        return UnavailableReason::IncompatibleConfiguration { stderr: excerpt };
    }
    UnavailableReason::ProbeFailed { detail: excerpt }
}

/// Last meaningful lines of stderr, without the path prefix.
pub fn stderr_excerpt(stderr: &str) -> String {
    let lines: Vec<String> = stderr
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| match l.find(".exe: ") {
            Some(i) => l[i + 6..].to_owned(),
            None => l.to_owned(),
        })
        .collect();
    let start = lines.len().saturating_sub(4);
    let mut s = lines[start..].join(" | ");
    if s.len() > 600 {
        let mut cut = 600;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
    }
    s
}

// ---------------------------------------------------------------------------
// Execution
// ---------------------------------------------------------------------------

/// Output of a probe command.
#[derive(Debug)]
pub struct CommandOutput {
    /// Exit code.
    pub code: Option<i32>,
    /// Stdout.
    pub stdout: String,
    /// Stderr.
    pub stderr: String,
}

/// Runs `exe args` without a shell, with a minimal environment, null stdin, no
/// visible console and a timeout (the process is terminated when it expires).
pub fn run_capture(exe: &Path, args: &[&str], timeout: Duration) -> Result<CommandOutput> {
    let mut cmd = Command::new(exe);
    cmd.args(args.iter().map(OsStr::new))
        .env_clear()
        .envs(crate::process::minimal_environment())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    if let Some(dir) = exe.parent() {
        cmd.current_dir(dir);
    }
    let mut child = cmd.spawn().map_err(BackendError::Spawn)?;
    let mut out = child.stdout.take();
    let mut err = child.stderr.take();
    let t_out = std::thread::spawn(move || {
        let mut s = Vec::new();
        if let Some(o) = out.as_mut() {
            let _ = o.take(8 * 1024 * 1024).read_to_end(&mut s);
        }
        s
    });
    let t_err = std::thread::spawn(move || {
        let mut s = Vec::new();
        if let Some(e) = err.as_mut() {
            let _ = e.take(1024 * 1024).read_to_end(&mut s);
        }
        s
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(st) = child.try_wait()? {
            break st;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(BackendError::Timeout("QEMU probe command"));
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let stdout = String::from_utf8_lossy(&t_out.join().unwrap_or_default()).into_owned();
    let stderr = String::from_utf8_lossy(&t_err.join().unwrap_or_default()).into_owned();
    Ok(CommandOutput {
        code: status.code(),
        stdout,
        stderr,
    })
}

/// Hexadecimal SHA-256 of a file.
pub fn sha256_file(path: &Path) -> Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Data folder of a QEMU distribution: `share/` next to the binary (the
/// Windows distribution of ADR-0010), else `../share/qemu` (Unix prefix
/// layout: Homebrew, `/usr`). `None` if neither exists.
pub fn qemu_share_dir(qemu_system: &Path) -> Option<PathBuf> {
    let dir = qemu_system.parent()?;
    let flat = dir.join("share");
    if flat.is_dir() {
        return Some(flat);
    }
    let prefix = dir.parent()?.join("share").join("qemu");
    prefix.is_dir().then_some(prefix)
}

/// Looks for the x86 UEFI firmware in the QEMU data folder
/// ([`qemu_share_dir`]).
pub fn find_firmware(qemu_system: &Path) -> Result<Option<FirmwareInfo>> {
    let Some(share) = qemu_share_dir(qemu_system) else {
        return Ok(None);
    };
    let code = share.join("edk2-x86_64-code.fd");
    let vars = share.join("edk2-i386-vars.fd");
    if !code.is_file() || !vars.is_file() {
        return Ok(None);
    }
    let code_sha256 = sha256_file(&code)?;
    let vars_sha256 = sha256_file(&vars)?;
    let matches_verified =
        code_sha256 == VERIFIED_OVMF_CODE_SHA256 && vars_sha256 == VERIFIED_OVMF_VARS_SHA256;
    Ok(Some(FirmwareInfo {
        code,
        code_sha256,
        vars_template: vars,
        vars_sha256,
        matches_verified,
    }))
}

/// Looks for the SeaBIOS image (`share/bios-256k.bin`) next to the binary
/// and hashes it.
pub fn find_seabios(qemu_system: &Path) -> Result<Option<BiosInfo>> {
    let Some(share) = qemu_share_dir(qemu_system) else {
        return Ok(None);
    };
    let path = share.join(SEABIOS_FILE);
    if !path.is_file() {
        return Ok(None);
    }
    let sha256 = sha256_file(&path)?;
    let matches_verified = sha256 == VERIFIED_SEABIOS_SHA256;
    Ok(Some(BiosInfo {
        path,
        sha256,
        matches_verified,
    }))
}

fn approved_in(list: &[String], sha256: &str) -> bool {
    list.iter().any(|a| a.eq_ignore_ascii_case(sha256))
}

/// Looks for the project's AArch64 firmware in `dir` and hashes it against
/// `approved` (released hashes of ADR-0028 plus configured approvals).
/// `None` if any of the three files is missing.
pub fn find_aarch64_firmware(
    dir: &Path,
    approved: &[String],
) -> Result<Option<Aarch64FirmwareInfo>> {
    let hashed = |name: &str| -> Result<Option<HashedFile>> {
        let path = dir.join(name);
        if !path.is_file() {
            return Ok(None);
        }
        let sha256 = sha256_file(&path)?;
        let approved = approved_in(approved, &sha256)
            || RELEASED_AARCH64_FIRMWARE_SHA256
                .iter()
                .any(|r| r.eq_ignore_ascii_case(&sha256));
        Ok(Some(HashedFile {
            path,
            sha256,
            approved,
        }))
    };
    let (Some(code), Some(vars_blank), Some(vars_windows)) = (
        hashed(AARCH64_CODE_FILE)?,
        hashed(AARCH64_VARS_BLANK_FILE)?,
        hashed(AARCH64_VARS_WINDOWS_FILE)?,
    ) else {
        return Ok(None);
    };
    Ok(Some(Aarch64FirmwareInfo {
        code,
        vars_blank,
        vars_windows,
    }))
}

/// Hashes swtpm and, only if the hash is approved, reads its version.
pub fn find_swtpm(
    path: &Path,
    approved: &[String],
    timeout: Duration,
) -> Result<Option<SwtpmInfo>> {
    if !path.is_file() {
        return Ok(None);
    }
    let sha256 = sha256_file(path)?;
    let ok = approved_in(approved, &sha256);
    let version = if ok {
        let o = run_capture(path, &["--version"], timeout)?;
        o.stdout.lines().next().map(|l| l.trim().to_owned())
    } else {
        None
    };
    Ok(Some(SwtpmInfo {
        path: path.to_path_buf(),
        sha256,
        approved: ok,
        version,
    }))
}

/// Hashes of the files a probe result depends on: the two binaries, the
/// UEFI and SeaBIOS firmware next to them, the AArch64 firmware and swtpm
/// (F6: "ripetizione dei probe dopo cambi QEMU/firmware").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProbeFingerprint {
    /// SHA-256 of `qemu-system-x86_64`.
    pub qemu_system_sha256: String,
    /// SHA-256 of `qemu-img`.
    pub qemu_img_sha256: String,
    /// SHA-256 of the firmware code and variables template, if present.
    pub firmware: Option<(String, String)>,
    /// SHA-256 of the SeaBIOS image, if present.
    pub seabios: Option<String>,
    /// SHA-256 of the AArch64 firmware files, if present.
    pub aarch64_firmware: Option<Vec<String>>,
    /// SHA-256 of swtpm, if present.
    pub swtpm: Option<String>,
}

impl ProbeFingerprint {
    /// Names of the parts that differ from `other` (empty: same files).
    pub fn changes(&self, other: &Self) -> Vec<&'static str> {
        let mut v = Vec::new();
        if !self
            .qemu_system_sha256
            .eq_ignore_ascii_case(&other.qemu_system_sha256)
        {
            v.push("qemu-system");
        }
        if !self
            .qemu_img_sha256
            .eq_ignore_ascii_case(&other.qemu_img_sha256)
        {
            v.push("qemu-img");
        }
        match (&self.firmware, &other.firmware) {
            (Some((c1, v1)), Some((c2, v2))) => {
                if c1 != c2 {
                    v.push("firmware code");
                }
                if v1 != v2 {
                    v.push("firmware variables template");
                }
            }
            (None, None) => {}
            _ => v.push("firmware presence"),
        }
        match (&self.seabios, &other.seabios) {
            (Some(a), Some(b)) if a != b => v.push("SeaBIOS image"),
            (Some(_), None) | (None, Some(_)) => v.push("SeaBIOS presence"),
            _ => {}
        }
        if self.aarch64_firmware != other.aarch64_firmware {
            v.push("AArch64 firmware");
        }
        if self.swtpm != other.swtpm {
            v.push("swtpm");
        }
        v
    }
}

impl HostCapabilities {
    /// Fingerprint recorded by the probe that produced these capabilities.
    pub fn fingerprint(&self) -> ProbeFingerprint {
        ProbeFingerprint {
            qemu_system_sha256: self.qemu_system_sha256.clone(),
            qemu_img_sha256: self.qemu_img_sha256.clone(),
            firmware: self
                .firmware
                .as_ref()
                .map(|f| (f.code_sha256.clone(), f.vars_sha256.clone())),
            seabios: self.seabios.as_ref().map(|b| b.sha256.clone()),
            aarch64_firmware: self
                .aarch64_firmware
                .as_ref()
                .map(Aarch64FirmwareInfo::hashes),
            swtpm: self.swtpm.as_ref().map(|t| t.sha256.clone()),
        }
    }
}

/// Current fingerprint of the files at the paths `caps` was probed from
/// (hashes computed now; the firmware is looked up again next to the
/// binary).
pub fn current_fingerprint(caps: &HostCapabilities) -> Result<ProbeFingerprint> {
    Ok(ProbeFingerprint {
        qemu_system_sha256: sha256_file(caps.qemu_system.as_path())?,
        qemu_img_sha256: sha256_file(caps.qemu_img.as_path())?,
        firmware: find_firmware(caps.qemu_system.as_path())?
            .map(|f| (f.code_sha256, f.vars_sha256)),
        seabios: find_seabios(caps.qemu_system.as_path())?.map(|b| b.sha256),
        aarch64_firmware: match &caps.inputs.firmware_dir {
            Some(dir) => find_aarch64_firmware(dir, &caps.inputs.approved_firmware_sha256)?
                .map(|f| f.hashes()),
            None => None,
        },
        swtpm: match &caps.inputs.swtpm {
            Some(p) if p.is_file() => Some(sha256_file(p)?),
            _ => None,
        },
    })
}

/// Repeats the probe when the files it depends on changed since it ran
/// (QEMU updated or replaced, firmware changed): `reprobe` runs only if
/// `current` differs from `recorded` and its result (or its refusal, for
/// example an unapproved binary) is returned with the changed parts.
/// `Ok(None)`: nothing changed, the recorded capabilities stay valid.
pub fn reprobe_if_changed<T>(
    recorded: &ProbeFingerprint,
    current: &ProbeFingerprint,
    reprobe: impl FnOnce() -> Result<T>,
) -> Result<Option<(T, Vec<&'static str>)>> {
    let changed = recorded.changes(current);
    if changed.is_empty() {
        return Ok(None);
    }
    reprobe().map(|t| Some((t, changed)))
}

/// SHA-256 of the approved `qemu-system-x86_64.exe` 11.1.0 (ADR-0010, F0-02 §2).
pub const APPROVED_QEMU_SYSTEM_SHA256: &str =
    "47d57a6072e0bb3bd98f87926eb129eb1736dfe818c67b3b81ef7ce4edd0b3cd";
/// SHA-256 of the approved `qemu-img.exe` 11.1.0 (ADR-0010, F0-02 §2).
pub const APPROVED_QEMU_IMG_SHA256: &str =
    "9d930582d0d574bb106d89982c547688e10b8f356192d302cc3d0ae4e8674534";

/// **Development, not approved for release**: `qemu-system-aarch64` of
/// Homebrew QEMU 11.1.2 on the macOS development host (R2b-01). Accepted by
/// default only on Apple Silicon builds, until ADR-0010 names an approved
/// macOS build.
pub const DEV_MACOS_QEMU_SYSTEM_SHA256: &str =
    "7299ef807db462f913be5d1b89a71ea12590416175be0be5e87c2349594c6542";
/// **Development, not approved for release**: `qemu-img` of Homebrew QEMU
/// 11.1.2 (same scope as [`DEV_MACOS_QEMU_SYSTEM_SHA256`]).
pub const DEV_MACOS_QEMU_IMG_SHA256: &str =
    "c1244165ace0db0c7550eecd477ba30fb75e5c318da541fc959f978830367224";
/// **Development, not approved for release**: `swtpm` 0.10.2 of Homebrew
/// (R2b-01; same scope as [`DEV_MACOS_QEMU_SYSTEM_SHA256`]).
pub const DEV_MACOS_SWTPM_SHA256: &str =
    "63fdc5090974a8fbca72f95ff9e7cf55a7b9152a584dc146840ad150c22a92d5";

/// Name of the QEMU system emulator of this host.
pub const QEMU_SYSTEM_NAME: &str = if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
    "qemu-system-aarch64"
} else {
    "qemu-system-x86_64"
};

/// Probe configuration.
#[derive(Clone, Debug)]
pub struct ProbeConfig {
    /// Timeout for each `help`/`--version` command.
    pub command_timeout: Duration,
    /// Overall timeout of the accelerator probe.
    pub accelerator_timeout: Duration,
    /// SHA-256 (lowercase hexadecimal) of the approved `qemu-system-x86_64` binaries.
    /// A binary with a different hash is not executed.
    pub approved_qemu_system_sha256: Vec<String>,
    /// SHA-256 of the approved `qemu-img` binaries.
    pub approved_qemu_img_sha256: Vec<String>,
    /// Directory of the AArch64 firmware files (ADR-0028); `None`: not
    /// looked for.
    pub firmware_dir: Option<PathBuf>,
    /// SHA-256 of the approved AArch64 firmware files, besides
    /// [`RELEASED_AARCH64_FIRMWARE_SHA256`].
    pub approved_firmware_sha256: Vec<String>,
    /// swtpm executable (ADR-0028); `None`: not looked for.
    pub swtpm: Option<PathBuf>,
    /// SHA-256 of the approved swtpm executables.
    pub approved_swtpm_sha256: Vec<String>,
    /// SHA-256 of approved storage helper files (ADR-0030), besides
    /// [`crate::storage_helper::APPROVED_HELPER_SHA256`].
    pub approved_storage_helper_sha256: Vec<String>,
}

impl Default for ProbeConfig {
    fn default() -> Self {
        let mac = cfg!(all(target_os = "macos", target_arch = "aarch64"));
        let mut approved_qemu_system_sha256 = vec![APPROVED_QEMU_SYSTEM_SHA256.to_owned()];
        let mut approved_qemu_img_sha256 = vec![APPROVED_QEMU_IMG_SHA256.to_owned()];
        let mut approved_swtpm_sha256 = Vec::new();
        if mac {
            approved_qemu_system_sha256.push(DEV_MACOS_QEMU_SYSTEM_SHA256.to_owned());
            approved_qemu_img_sha256.push(DEV_MACOS_QEMU_IMG_SHA256.to_owned());
            approved_swtpm_sha256.push(DEV_MACOS_SWTPM_SHA256.to_owned());
        }
        Self {
            command_timeout: Duration::from_secs(15),
            accelerator_timeout: Duration::from_secs(20),
            approved_qemu_system_sha256,
            approved_qemu_img_sha256,
            firmware_dir: None,
            approved_firmware_sha256: Vec::new(),
            swtpm: None,
            approved_swtpm_sha256,
            approved_storage_helper_sha256: Vec::new(),
        }
    }
}

/// Computes the SHA-256 of the binary and compares it with the approved list.
///
/// Declared limit: between computing the hash and executing it the file could
/// be replaced by a process of the same user; the directory of the
/// approved binaries must therefore be protected by the installation owner.
pub fn verify_approved_binary(
    binary: &'static str,
    path: &ManagedPath,
    approved: &[String],
) -> Result<String> {
    let sha256 = sha256_file(path.as_path())?;
    if approved.iter().any(|a| a.eq_ignore_ascii_case(&sha256)) {
        Ok(sha256)
    } else {
        Err(BackendError::UnapprovedBinary { binary, sha256 })
    }
}

/// Detects the static capabilities of the approved binaries. The `accelerator` field
/// stays [`AcceleratorStatus::NotProbed`] until
/// [`probe_accelerator`] is run.
pub fn probe_host(
    qemu_system: &ManagedPath,
    qemu_img: &ManagedPath,
    cfg: &ProbeConfig,
) -> Result<HostCapabilities> {
    let exe = qemu_system.as_path();
    // Hash before any execution (ADR-0010).
    let qemu_system_sha256 = verify_approved_binary(
        QEMU_SYSTEM_NAME,
        qemu_system,
        &cfg.approved_qemu_system_sha256,
    )?;
    let qemu_img_sha256 =
        verify_approved_binary("qemu-img", qemu_img, &cfg.approved_qemu_img_sha256)?;
    let run = |args: &[&str]| -> Result<String> {
        let o = run_capture(exe, args, cfg.command_timeout)?;
        if o.code != Some(0) {
            return Err(BackendError::Probe(format!(
                "`{}` exited with {:?}: {}",
                args.join(" "),
                o.code,
                stderr_excerpt(&o.stderr)
            )));
        }
        Ok(o.stdout)
    };
    let version_txt = run(&["--version"])?;
    let version = parse_qemu_version(&version_txt).ok_or_else(|| {
        BackendError::Probe(format!(
            "unrecognised version: {}",
            stderr_excerpt(&version_txt)
        ))
    })?;
    let accelerators = parse_accel_help(&run(&["-accel", "help"])?);
    let machines = parse_machine_help(&run(&["-machine", "help"])?);
    let devices = parse_device_help(&run(&["-device", "help"])?);
    let cpu_models = parse_cpu_help(&run(&["-cpu", "help"])?);
    let img_version_out = run_capture(qemu_img.as_path(), &["--version"], cfg.command_timeout)?;
    let qemu_img_version = parse_qemu_img_version(&img_version_out.stdout)
        .ok_or_else(|| BackendError::Probe("unrecognised qemu-img version".into()))?;
    // `qemu-img --help` exits with code 0 in recent versions; the output
    // is used anyway even with a different code.
    let img_help = run_capture(qemu_img.as_path(), &["--help"], cfg.command_timeout)?;
    let image_formats = parse_qemu_img_formats(&img_help.stdout);
    let remote_protocols_present = ["http", "https", "ftp", "ftps", "nbd", "nfs", "ssh"]
        .iter()
        .filter(|p| image_formats.contains(**p))
        .map(|p| (*p).to_owned())
        .collect();
    let q35_machines = versioned_machines(&machines, MachineFamily::Q35);
    let i440fx_machines = versioned_machines(&machines, MachineFamily::I440fx);
    let virt_machines = versioned_machines(&machines, MachineFamily::Virt);
    let aarch64_firmware = match &cfg.firmware_dir {
        Some(dir) => find_aarch64_firmware(dir, &cfg.approved_firmware_sha256)?,
        None => None,
    };
    let swtpm = match &cfg.swtpm {
        Some(p) => find_swtpm(p, &cfg.approved_swtpm_sha256, cfg.command_timeout)?,
        None => None,
    };
    Ok(HostCapabilities {
        qemu_system_sha256,
        qemu_img_sha256,
        qemu_system: qemu_system.clone(),
        qemu_img: qemu_img.clone(),
        version,
        qemu_img_version,
        accelerators,
        machines,
        q35_machines,
        i440fx_machines,
        virt_machines,
        devices,
        cpu_models,
        image_formats,
        remote_protocols_present,
        firmware: find_firmware(exe)?,
        seabios: find_seabios(exe)?,
        aarch64_firmware,
        swtpm,
        inputs: ProbeInputs {
            firmware_dir: cfg.firmware_dir.clone(),
            swtpm: cfg.swtpm.clone(),
            approved_firmware_sha256: cfg.approved_firmware_sha256.clone(),
            approved_swtpm_sha256: cfg.approved_swtpm_sha256.clone(),
        },
        accelerator: AcceleratorStatus::NotProbed,
        profile_accelerators: BTreeMap::new(),
    })
}

/// Real accelerator probe: paused launch (`-S`) with the given machine,
/// firmware kind and CPU, `query-accelerators` via QMP, then `quit`. The
/// process is in the run's container and is terminated in every case. The
/// caller stores the outcome with [`HostCapabilities::record_accelerator`]
/// under `ProbeTarget { family: machine.family(), firmware }`, or uses
/// [`probe_accelerator_for`].
pub async fn probe_accelerator(
    caps: &HostCapabilities,
    accelerator: Accelerator,
    machine: &MachineType,
    firmware: FirmwareKind,
    cpu: &CpuModel,
    private_dir: &ManagedPath,
    cfg: &ProbeConfig,
) -> AcceleratorStatus {
    let unavailable = |reason| AcceleratorStatus::Unavailable {
        accelerator,
        reason,
    };
    let image = match caps.probe_firmware(firmware) {
        Ok(i) => i,
        Err(e) => {
            return unavailable(UnavailableReason::ProbeFailed {
                detail: e.to_string(),
            });
        }
    };
    // Every probe is a distinct run with its own `run_id`.
    let tag = RunId::new_v7();
    let cmd = match build_probe_command(
        caps.qemu_system.clone(),
        machine,
        &image,
        accelerator,
        cpu,
        private_dir.clone(),
        &tag,
    ) {
        Ok(c) => c,
        Err(e) => {
            return unavailable(UnavailableReason::ProbeFailed {
                detail: e.to_string(),
            });
        }
    };
    let qmp_cfg = QmpConfig::default();
    let launched = crate::run::launch(&cmd, &qmp_cfg, cfg.accelerator_timeout).await;
    let launched = match launched {
        Ok(l) => l,
        Err(BackendError::StartupFailed { stderr_tail, .. }) => {
            let mut reason = classify_accel_stderr(accelerator, &stderr_tail);
            if let UnavailableReason::NotCompiled { available } = &mut reason {
                available.clone_from(&caps.accelerators);
            }
            return unavailable(reason);
        }
        Err(e) => {
            return unavailable(UnavailableReason::ProbeFailed {
                detail: e.to_string(),
            });
        }
    };
    let result = launched.qmp.query_accelerators().await;
    let _ = launched.qmp.quit().await;
    launched.finish(Duration::from_secs(5)).await;
    match result {
        Ok(info) if info.enabled == accelerator.qemu_name() => AcceleratorStatus::Available {
            accelerator,
            machine: machine.as_str().to_owned(),
            cpu: cpu.as_str().to_owned(),
        },
        Ok(info) => unavailable(UnavailableReason::WrongAccelerator {
            enabled: info.enabled,
        }),
        Err(e) => unavailable(UnavailableReason::ProbeFailed {
            detail: e.to_string(),
        }),
    }
}

/// Accelerator probe for the combination of `profile` with the cache of
/// `caps`: if that combination ([`DeviceProfile::probe_target`]) was
/// already probed its outcome is returned unchanged, otherwise the probe
/// runs with `machine` (which must belong to the profile's family) and its
/// outcome is recorded with [`HostCapabilities::record_accelerator`].
pub async fn probe_accelerator_for(
    caps: &mut HostCapabilities,
    accelerator: Accelerator,
    profile: &DeviceProfile,
    machine: &MachineType,
    cpu: &CpuModel,
    private_dir: &ManagedPath,
    cfg: &ProbeConfig,
) -> AcceleratorStatus {
    let target = profile.probe_target();
    if machine.family() != target.family {
        return AcceleratorStatus::Unavailable {
            accelerator,
            reason: UnavailableReason::ProbeFailed {
                detail: format!(
                    "machine `{}` is not a {} type, required by the hardware profile",
                    machine.as_str(),
                    target.family.label()
                ),
            },
        };
    }
    match caps.accelerator_for(target) {
        cached @ (AcceleratorStatus::Available { accelerator: a, .. }
        | AcceleratorStatus::Unavailable { accelerator: a, .. })
            if *a == accelerator =>
        {
            return cached.clone();
        }
        _ => {}
    }
    let status = probe_accelerator(
        caps,
        accelerator,
        machine,
        target.firmware,
        cpu,
        private_dir,
        cfg,
    )
    .await;
    caps.record_accelerator(target, status.clone());
    status
}

#[cfg(test)]
mod tests {
    use super::*;

    const VERSION: &str = include_str!("../tests/fixtures/version.txt");
    const ACCEL: &str = include_str!("../tests/fixtures/accel-help.txt");
    const MACHINE: &str = include_str!("../tests/fixtures/machine-help.txt");
    const DEVICE: &str = include_str!("../tests/fixtures/device-help.txt");
    const CPU: &str = include_str!("../tests/fixtures/cpu-help.txt");
    const IMG_VERSION: &str = include_str!("../tests/fixtures/qemu-img-version.txt");
    const IMG_HELP: &str = include_str!("../tests/fixtures/qemu-img-help.txt");
    const ERR_KVM: &str = include_str!("../tests/fixtures/stderr-accel-kvm.txt");
    const ERR_CPU: &str = include_str!("../tests/fixtures/stderr-cpu-unknown.txt");
    const ERR_WHPX: &str = include_str!("../tests/fixtures/stderr-whpx-noaccel-synthetic.txt");
    // Homebrew QEMU 11.1.2 `qemu-system-aarch64` on macOS (R2b-01).
    const A64_VERSION: &str = include_str!("../tests/fixtures/aarch64/version.txt");
    const A64_ACCEL: &str = include_str!("../tests/fixtures/aarch64/accel-help.txt");
    const A64_MACHINE: &str = include_str!("../tests/fixtures/aarch64/machine-help.txt");
    const A64_DEVICE: &str = include_str!("../tests/fixtures/aarch64/device-help.txt");
    const A64_CPU: &str = include_str!("../tests/fixtures/aarch64/cpu-help.txt");

    #[test]
    fn real_version() {
        let v = parse_qemu_version(VERSION).unwrap();
        assert_eq!((v.major, v.minor, v.micro), (11, 1, 0));
        assert_eq!(v.package.as_deref(), Some("v11.1.0-12130-ge470268ff4"));
        let i = parse_qemu_img_version(IMG_VERSION).unwrap();
        assert_eq!(i, v);
        assert!(parse_qemu_version("garbage").is_none());
        assert!(parse_qemu_version("QEMU emulator version x.y").is_none());
    }

    #[test]
    fn real_accelerators() {
        assert_eq!(parse_accel_help(ACCEL), vec!["tcg", "whpx"]);
        assert!(parse_accel_help("").is_empty());
    }

    #[test]
    fn real_machines() {
        let m = parse_machine_help(MACHINE);
        let q35 = m.iter().find(|e| e.name == "q35").unwrap();
        assert_eq!(q35.alias_of.as_deref(), Some("pc-q35-11.1"));
        assert!(m.iter().any(|e| e.name == "pc-q35-5.2" && e.deprecated));
        assert!(m.iter().any(|e| e.name == "pc-i440fx-11.1" && e.default));
        let v = q35_versioned(&m);
        let names: Vec<_> = v.iter().map(MachineType::as_str).collect();
        assert_eq!(
            names,
            vec![
                "pc-q35-11.1",
                "pc-q35-11.0",
                "pc-q35-10.2",
                "pc-q35-10.1",
                "pc-q35-10.0",
                "pc-q35-9.2",
                "pc-q35-9.1",
                "pc-q35-9.0",
                "pc-q35-8.2"
            ]
        );
        assert_eq!(v, versioned_machines(&m, MachineFamily::Q35));
        let fx = versioned_machines(&m, MachineFamily::I440fx);
        let names: Vec<_> = fx.iter().map(MachineType::as_str).collect();
        // `pc` alias skipped, deprecated ones (8.1 and older) excluded.
        assert_eq!(
            names,
            vec![
                "pc-i440fx-11.1",
                "pc-i440fx-11.0",
                "pc-i440fx-10.2",
                "pc-i440fx-10.1",
                "pc-i440fx-10.0",
                "pc-i440fx-9.2",
                "pc-i440fx-9.1",
                "pc-i440fx-9.0",
                "pc-i440fx-8.2"
            ]
        );
        assert!(fx.iter().all(|m| m.family() == MachineFamily::I440fx));
    }

    #[test]
    fn real_devices() {
        let d = parse_device_help(DEVICE);
        for p in DeviceProfile::ALL.iter().filter(|p| !p.is_aarch64()) {
            for r in p.required_devices() {
                assert!(d.contains(r), "{r} ({p:?})");
            }
        }
        let a = parse_device_help(A64_DEVICE);
        for r in DeviceProfile::WINDOWS11_ARM64.required_devices() {
            assert!(a.contains(r), "{r} (AArch64)");
        }
        assert!(d.contains("virtio-tablet"), "alias");
        assert!(d.contains("e1000e"));
    }

    /// Capabilities parsed from the real fixtures, without binaries.
    fn fixture_caps() -> HostCapabilities {
        let dir = std::env::temp_dir().join("lightvm-qemu-backend-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let mp = ManagedPath::verify(dir.join("fixture-qemu.bin")).unwrap();
        let machines = parse_machine_help(MACHINE);
        let v = parse_qemu_version(VERSION).unwrap();
        HostCapabilities {
            qemu_system: mp.clone(),
            qemu_system_sha256: String::new(),
            qemu_img: mp,
            qemu_img_sha256: String::new(),
            version: v.clone(),
            qemu_img_version: v,
            accelerators: parse_accel_help(ACCEL),
            q35_machines: versioned_machines(&machines, MachineFamily::Q35),
            i440fx_machines: versioned_machines(&machines, MachineFamily::I440fx),
            virt_machines: versioned_machines(&machines, MachineFamily::Virt),
            machines,
            devices: parse_device_help(DEVICE),
            cpu_models: parse_cpu_help(CPU),
            image_formats: parse_qemu_img_formats(IMG_HELP),
            remote_protocols_present: Vec::new(),
            firmware: None,
            seabios: None,
            aarch64_firmware: None,
            swtpm: None,
            inputs: ProbeInputs::default(),
            accelerator: AcceleratorStatus::NotProbed,
            profile_accelerators: BTreeMap::new(),
        }
    }

    #[test]
    fn machines_and_devices_per_family_and_profile() {
        let mut caps = fixture_caps();
        assert_eq!(
            caps.recommended_machine(MachineFamily::Q35)
                .unwrap()
                .as_str(),
            "pc-q35-11.1"
        );
        assert_eq!(
            caps.recommended_machine(MachineFamily::I440fx)
                .unwrap()
                .as_str(),
            "pc-i440fx-11.1"
        );
        assert_eq!(
            caps.machine("pc-i440fx-10.0").unwrap().family(),
            MachineFamily::I440fx
        );
        assert!(caps.machine("pc-i440fx-8.1").is_err(), "deprecated");
        assert!(caps.machine("pc-i440fx-12.0").is_err(), "not listed");
        assert!(caps.machine("pc").is_err(), "alias");
        assert!(caps.machine("pc-q35-11.1").is_ok());
        for p in DeviceProfile::ALL.iter().filter(|p| !p.is_aarch64()) {
            assert!(caps.missing_devices(p).is_empty(), "{p:?}");
        }
        // A binary without USB tablet: only the Windows profiles are affected.
        caps.devices.remove("usb-tablet");
        assert!(caps.missing_devices(&DeviceProfile::LINUX).is_empty());
        assert_eq!(
            caps.missing_devices(&DeviceProfile::WINDOWS_XP),
            ["usb-tablet"]
        );
        let d = caps.diagnostics_for(&DeviceProfile::LINUX);
        assert!(!d.iter().any(|x| x.code == "device_missing"));
        assert!(d.iter().any(|x| x.code == "profile_device_missing"));
        assert!(d.iter().any(|x| x.code == "seabios_missing"));
        caps.i440fx_machines.clear();
        assert!(caps.recommended_machine(MachineFamily::I440fx).is_none());
        assert!(caps.machine("pc-i440fx-11.1").is_err());
        assert!(
            caps.diagnostics_for(&DeviceProfile::LINUX)
                .iter()
                .any(|x| x.code == "i440fx_machine_missing")
        );
    }

    #[test]
    fn accelerator_cache_per_target() {
        let mut caps = fixture_caps();
        let avail = |m: &str| AcceleratorStatus::Available {
            accelerator: Accelerator::Whpx,
            machine: m.into(),
            cpu: "qemu64".into(),
        };
        let xp = DeviceProfile::WINDOWS_XP.probe_target();
        let w7 = DeviceProfile::WINDOWS_VISTA_7.probe_target();
        assert_eq!(caps.accelerator_for(xp), &AcceleratorStatus::NotProbed);
        caps.record_accelerator(ProbeTarget::REFERENCE, avail("pc-q35-11.1"));
        // The host reference is kept in `accelerator`, the others per target.
        assert_eq!(
            caps.accelerator.is_available(),
            ProbeTarget::host_reference() == ProbeTarget::REFERENCE
        );
        assert!(caps.accelerator_for(ProbeTarget::REFERENCE).is_available());
        // The reference probe does not cover the other combinations.
        assert_eq!(caps.accelerator_for(xp), &AcceleratorStatus::NotProbed);
        assert_eq!(
            caps.accelerator_for(DeviceProfile::WINDOWS_UEFI.probe_target()),
            caps.accelerator_for(ProbeTarget::REFERENCE),
            "same machine family and firmware as Linux"
        );
        caps.record_accelerator(xp, avail("pc-i440fx-11.1"));
        assert!(caps.accelerator_for(xp).is_available());
        assert_eq!(caps.accelerator_for(w7), &AcceleratorStatus::NotProbed);
        assert_eq!(w7, DeviceProfile::WINDOWS_BIOS.probe_target());
        // Probe firmware: none found in this distribution.
        assert!(caps.probe_firmware(FirmwareKind::Uefi).is_err());
        assert!(caps.probe_firmware(FirmwareKind::SeaBios).is_err());
        assert!(caps.probe_firmware(FirmwareKind::UefiAarch64).is_err());
    }

    /// Capabilities parsed from the Homebrew `qemu-system-aarch64` fixtures.
    fn fixture_caps_aarch64() -> HostCapabilities {
        let mut caps = fixture_caps();
        let machines = parse_machine_help(A64_MACHINE);
        let v = parse_qemu_version(A64_VERSION).unwrap();
        caps.version = v.clone();
        caps.qemu_img_version = v;
        caps.accelerators = parse_accel_help(A64_ACCEL);
        caps.q35_machines = versioned_machines(&machines, MachineFamily::Q35);
        caps.i440fx_machines = versioned_machines(&machines, MachineFamily::I440fx);
        caps.virt_machines = versioned_machines(&machines, MachineFamily::Virt);
        caps.machines = machines;
        caps.devices = parse_device_help(A64_DEVICE);
        caps.cpu_models = parse_cpu_help(A64_CPU);
        caps
    }

    /// ADR-0028: the `virt` machines, the profile devices and `-cpu host`
    /// of the Homebrew build; the diagnostics of an AArch64 host do not flag
    /// the x86 profiles and do flag the missing firmware and swtpm.
    #[test]
    fn aarch64_capabilities_and_diagnostics() {
        let caps = fixture_caps_aarch64();
        assert_eq!(caps.accelerators, ["hvf", "tcg"]);
        assert_eq!(
            caps.recommended_machine(MachineFamily::Virt)
                .unwrap()
                .as_str(),
            "virt-11.1"
        );
        assert!(caps.machine("virt-8.1").is_err(), "deprecated");
        assert!(caps.machine("virt").is_err(), "alias");
        assert!(caps.q35_machines.is_empty());
        assert!(
            caps.missing_devices(&DeviceProfile::WINDOWS11_ARM64)
                .is_empty()
        );
        let host = CpuModel::host_for(&DeviceProfile::WINDOWS11_ARM64, Accelerator::Hvf).unwrap();
        caps.check_cpu(&host).unwrap();
        assert!(caps.cpu_model("Skylake-Client-v4").is_err());
        let codes = |d: Vec<Diagnostic>| d.into_iter().map(|x| x.code).collect::<Vec<_>>();
        let d = codes(caps.diagnostics_for(&DeviceProfile::WINDOWS11_ARM64));
        for x86 in [
            "i440fx_machine_missing",
            "seabios_missing",
            "profile_device_missing",
            "machine_missing",
            "device_missing",
        ] {
            assert!(!d.contains(&x86), "{x86} flagged on an AArch64 host: {d:?}");
        }
        assert!(d.contains(&"firmware_missing"), "{d:?}");
        assert!(d.contains(&"swtpm_missing"), "{d:?}");
        // The same binary judged as an x86 host would be broken.
        let x = codes(caps.diagnostics_for(&DeviceProfile::LINUX));
        assert!(x.contains(&"machine_missing") && x.contains(&"seabios_missing"));
    }

    /// ADR-0028 firmware and swtpm: found in the configured places, hashed,
    /// accepted only with an approved hash (never silently), and part of
    /// the probe fingerprint.
    #[test]
    fn aarch64_firmware_and_swtpm_approval() {
        let dir =
            std::env::temp_dir().join(format!("lightvm-qemu-backend-a64fw-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(find_aarch64_firmware(&dir, &[]).unwrap().is_none());
        for (n, c) in [
            (AARCH64_CODE_FILE, "code"),
            (AARCH64_VARS_BLANK_FILE, "blank"),
            (AARCH64_VARS_WINDOWS_FILE, "win"),
        ] {
            std::fs::write(dir.join(n), c).unwrap();
        }
        let unapproved = find_aarch64_firmware(&dir, &[]).unwrap().unwrap();
        assert!(!unapproved.approved());
        let mut caps = fixture_caps_aarch64();
        caps.aarch64_firmware = Some(unapproved.clone());
        assert!(caps.verified_aarch64_firmware().is_err());
        assert!(caps.probe_firmware(FirmwareKind::UefiAarch64).is_err());
        assert!(
            caps.diagnostics_for(&DeviceProfile::WINDOWS11_ARM64)
                .iter()
                .any(|x| x.code == "firmware_unverified")
        );
        // Two of three approved: still refused.
        let partial = vec![
            unapproved.code.sha256.clone(),
            unapproved.vars_blank.sha256.to_uppercase(),
        ];
        assert!(
            !find_aarch64_firmware(&dir, &partial)
                .unwrap()
                .unwrap()
                .approved()
        );
        let mut all = partial;
        all.push(unapproved.vars_windows.sha256.clone());
        let ok = find_aarch64_firmware(&dir, &all).unwrap().unwrap();
        assert!(ok.approved());
        caps.aarch64_firmware = Some(ok);
        match caps.probe_firmware(FirmwareKind::UefiAarch64).unwrap() {
            ProbeFirmware::UefiAarch64(p) => assert_eq!(p.as_path(), dir.join(AARCH64_CODE_FILE)),
            other => panic!("{other:?}"),
        }
        // swtpm: an unapproved executable is hashed but never run.
        let tpm = dir.join("swtpm");
        assert!(
            find_swtpm(&tpm, &[], Duration::from_secs(5))
                .unwrap()
                .is_none()
        );
        std::fs::write(&tpm, b"#!/bin/sh\necho 'TPM emulator version 0.0.1'\n").unwrap();
        let t = find_swtpm(&tpm, &[], Duration::from_secs(5))
            .unwrap()
            .unwrap();
        assert!(!t.approved && t.version.is_none());
        caps.swtpm = Some(t.clone());
        assert!(caps.verified_swtpm().is_err());
        caps.swtpm = Some(SwtpmInfo {
            approved: true,
            ..t
        });
        assert!(caps.verified_swtpm().is_ok());
        // Fingerprint: a replaced firmware file or swtpm is a change.
        let bin = ManagedPath::verify(dir.join("qemu-system-aarch64")).unwrap();
        std::fs::write(bin.as_path(), b"qemu").unwrap();
        caps.qemu_system_sha256 = sha256_file(bin.as_path()).unwrap();
        caps.qemu_img_sha256 = caps.qemu_system_sha256.clone();
        caps.qemu_system = bin.clone();
        caps.qemu_img = bin;
        caps.inputs = ProbeInputs {
            firmware_dir: Some(dir.clone()),
            swtpm: Some(tpm.clone()),
            approved_firmware_sha256: all,
            approved_swtpm_sha256: Vec::new(),
        };
        let recorded = caps.fingerprint();
        assert!(
            recorded
                .changes(&current_fingerprint(&caps).unwrap())
                .is_empty()
        );
        std::fs::write(dir.join(AARCH64_VARS_WINDOWS_FILE), "other").unwrap();
        std::fs::write(&tpm, b"other").unwrap();
        assert_eq!(
            recorded.changes(&current_fingerprint(&caps).unwrap()),
            ["AArch64 firmware", "swtpm"]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Data folder of the Windows layout (`share/` next to the binary) and
    /// of the Unix prefix layout (`bin/` + `share/qemu/`, Homebrew).
    #[test]
    fn share_dir_layouts() {
        let dir =
            std::env::temp_dir().join(format!("lightvm-qemu-backend-share-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        let exe = dir.join("bin").join("qemu-system-aarch64");
        assert!(qemu_share_dir(&exe).is_none());
        std::fs::create_dir_all(dir.join("share").join("qemu")).unwrap();
        assert_eq!(
            qemu_share_dir(&exe).unwrap(),
            dir.join("share").join("qemu")
        );
        std::fs::create_dir_all(dir.join("bin").join("share")).unwrap();
        assert_eq!(qemu_share_dir(&exe).unwrap(), dir.join("bin").join("share"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn seabios_discovery_and_hash() {
        let dir = std::env::temp_dir().join(format!(
            "lightvm-qemu-backend-seabios-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("share")).unwrap();
        let exe = dir.join("qemu-system-x86_64.exe");
        assert!(find_seabios(&exe).unwrap().is_none());
        let bios = dir.join("share").join(SEABIOS_FILE);
        std::fs::write(&bios, b"not the verified image").unwrap();
        let found = find_seabios(&exe).unwrap().unwrap();
        assert_eq!(found.path, bios);
        assert_eq!(found.sha256, sha256_file(&bios).unwrap());
        assert!(!found.matches_verified, "hash mismatch");
        let mut caps = fixture_caps();
        caps.seabios = Some(found.clone());
        let e = caps.verified_seabios().unwrap_err();
        assert!(matches!(e, BackendError::Unsupported { .. }), "{e}");
        assert!(caps.probe_firmware(FirmwareKind::SeaBios).is_err());
        assert!(
            caps.diagnostics_for(&DeviceProfile::LINUX)
                .iter()
                .any(|x| x.code == "seabios_unverified")
        );
        // Same record with the verified hash: accepted.
        caps.seabios = Some(BiosInfo {
            sha256: VERIFIED_SEABIOS_SHA256.into(),
            matches_verified: true,
            ..found
        });
        assert_eq!(caps.verified_seabios().unwrap().path, bios);
        match caps.probe_firmware(FirmwareKind::SeaBios).unwrap() {
            ProbeFirmware::SeaBios(p) => assert_eq!(p.as_path(), bios.as_path()),
            other => panic!("{other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The SeaBIOS image of the lab distribution has the pinned hash (only
    /// when the lab QEMU is present).
    #[test]
    fn lab_seabios_hash_if_present() {
        let exe = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../lab/qemu/qemu-11.1.0/qemu-system-x86_64.exe");
        if let Some(b) = find_seabios(&exe).unwrap() {
            assert!(b.matches_verified, "{}", b.sha256);
        }
    }

    #[test]
    fn real_cpus() {
        let c = parse_cpu_help(CPU);
        for m in [
            "qemu64",
            "Skylake-Client-v4",
            "Haswell-v4",
            "host",
            "max",
            "486",
        ] {
            assert!(c.contains(m), "{m}");
        }
        // the CPUID flags section is not mistaken for models
        assert!(!c.contains("vmx-ple"));
        assert!(!c.contains("Recognized"));
    }

    #[test]
    fn real_formats() {
        let f = parse_qemu_img_formats(IMG_HELP);
        for x in ["qcow2", "raw", "file", "nbd", "vvfat"] {
            assert!(f.contains(x), "{x}");
        }
        assert!(!f.contains("See"));
    }

    #[test]
    fn stderr_diagnosis() {
        assert!(matches!(
            classify_accel_stderr(Accelerator::Kvm, ERR_KVM),
            UnavailableReason::NotCompiled { .. }
        ));
        assert!(matches!(
            classify_accel_stderr(Accelerator::Whpx, ERR_CPU),
            UnavailableReason::IncompatibleConfiguration { .. }
        ));
        match classify_accel_stderr(Accelerator::Whpx, ERR_WHPX) {
            UnavailableReason::InitFailed { stderr } => {
                assert!(stderr.contains("No accelerator found"));
                assert!(!stderr.contains(".exe"));
            }
            other => panic!("{other:?}"),
        }
        let msg = AcceleratorStatus::Unavailable {
            accelerator: Accelerator::Whpx,
            reason: classify_accel_stderr(Accelerator::Whpx, ERR_WHPX),
        }
        .to_string();
        assert!(msg.contains("WHPX not initialised"));
        assert!(msg.contains("Windows Hypervisor Platform"));
    }

    #[test]
    fn approved_binaries_list() {
        let dir = std::env::temp_dir().join("lightvm-qemu-backend-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let p = ManagedPath::verify(dir.join("fake-qemu.bin")).unwrap();
        std::fs::write(p.as_path(), b"abc").unwrap();
        // SHA-256 of "abc".
        let abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert_eq!(
            verify_approved_binary("qemu-img", &p, &[abc.to_uppercase()]).unwrap(),
            abc
        );
        let e = verify_approved_binary("qemu-img", &p, &["00".repeat(32)]).unwrap_err();
        assert!(matches!(e, BackendError::UnapprovedBinary { .. }), "{e}");
        assert!(verify_approved_binary("qemu-img", &p, &[]).is_err());
        let d = ProbeConfig::default();
        assert_eq!(
            d.approved_qemu_system_sha256[0],
            APPROVED_QEMU_SYSTEM_SHA256
        );
        assert_eq!(d.approved_qemu_img_sha256[0], APPROVED_QEMU_IMG_SHA256);
        // The Homebrew development entries only on Apple Silicon builds.
        let mac = cfg!(all(target_os = "macos", target_arch = "aarch64"));
        assert_eq!(
            d.approved_qemu_system_sha256
                .contains(&DEV_MACOS_QEMU_SYSTEM_SHA256.to_owned()),
            mac
        );
        assert_eq!(d.approved_qemu_system_sha256.len(), 1 + usize::from(mac));
        assert_eq!(d.approved_qemu_img_sha256.len(), 1 + usize::from(mac));
        assert_eq!(d.approved_swtpm_sha256.len(), usize::from(mac));
        assert!(d.approved_firmware_sha256.is_empty());
        assert!(RELEASED_AARCH64_FIRMWARE_SHA256.is_empty());
        std::fs::remove_file(p.as_path()).unwrap();
    }

    #[test]
    fn accelerator_public_message() {
        let s = AcceleratorStatus::Unavailable {
            accelerator: Accelerator::Whpx,
            reason: classify_accel_stderr(Accelerator::Whpx, ERR_WHPX),
        };
        let m = s.public_message();
        assert!(m.contains("WHPX") && m.contains("TCG"), "{m}");
        assert!(
            !m.contains("No accelerator found"),
            "no stderr in the public message"
        );
    }

    /// Fake QEMU distribution (two "binaries" and the firmware files) and
    /// the capabilities a probe of it would have recorded.
    fn fake_distribution(dir: &Path) -> HostCapabilities {
        std::fs::create_dir_all(dir.join("share")).unwrap();
        let sys = dir.join("qemu-system-x86_64.exe");
        let img = dir.join("qemu-img.exe");
        std::fs::write(&sys, b"system v1").unwrap();
        std::fs::write(&img, b"img v1").unwrap();
        std::fs::write(dir.join("share").join("edk2-x86_64-code.fd"), b"code v1").unwrap();
        std::fs::write(dir.join("share").join("edk2-i386-vars.fd"), b"vars v1").unwrap();
        std::fs::write(dir.join("share").join(SEABIOS_FILE), b"bios v1").unwrap();
        let v = QemuVersion {
            major: 11,
            minor: 1,
            micro: 0,
            package: None,
        };
        HostCapabilities {
            qemu_system_sha256: sha256_file(&sys).unwrap(),
            qemu_img_sha256: sha256_file(&img).unwrap(),
            qemu_system: ManagedPath::verify(sys).unwrap(),
            qemu_img: ManagedPath::verify(img).unwrap(),
            version: v.clone(),
            qemu_img_version: v,
            accelerators: vec!["whpx".into()],
            machines: Vec::new(),
            q35_machines: Vec::new(),
            i440fx_machines: Vec::new(),
            virt_machines: Vec::new(),
            devices: BTreeSet::new(),
            cpu_models: BTreeSet::new(),
            image_formats: BTreeSet::new(),
            remote_protocols_present: Vec::new(),
            firmware: find_firmware(&dir.join("qemu-system-x86_64.exe")).unwrap(),
            seabios: find_seabios(&dir.join("qemu-system-x86_64.exe")).unwrap(),
            aarch64_firmware: None,
            swtpm: None,
            inputs: ProbeInputs::default(),
            accelerator: AcceleratorStatus::NotProbed,
            profile_accelerators: BTreeMap::new(),
        }
    }

    /// F6: the probe is repeated when QEMU or the firmware change after it
    /// ran; nothing is re-run while the files are the same.
    #[test]
    fn probe_is_repeated_after_a_qemu_or_firmware_change() {
        let dir = std::env::temp_dir().join(format!(
            "lightvm-qemu-backend-reprobe-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let caps = fake_distribution(&dir);
        let recorded = caps.fingerprint();
        assert!(recorded.firmware.is_some());
        let mut probes = 0;
        // Same files: the recorded capabilities stay, no probe.
        let now = current_fingerprint(&caps).unwrap();
        let r = reprobe_if_changed(&recorded, &now, || {
            probes += 1;
            Ok("caps v2")
        })
        .unwrap();
        assert!(r.is_none());
        assert_eq!(probes, 0);

        // Firmware replaced: re-probe, the change is named.
        std::fs::write(dir.join("share").join("edk2-x86_64-code.fd"), b"code v2").unwrap();
        let now = current_fingerprint(&caps).unwrap();
        let (fresh, changed) = reprobe_if_changed(&recorded, &now, || {
            probes += 1;
            Ok("caps v2")
        })
        .unwrap()
        .unwrap();
        assert_eq!((fresh, probes), ("caps v2", 1));
        assert_eq!(changed, ["firmware code"]);

        // QEMU binary replaced by an unapproved one: the probe's refusal is
        // returned (the caller refuses the start).
        std::fs::write(dir.join("qemu-system-x86_64.exe"), b"system v2").unwrap();
        let now = current_fingerprint(&caps).unwrap();
        assert_eq!(recorded.changes(&now), ["qemu-system", "firmware code"]);
        let sys = caps.qemu_system.clone();
        let e = reprobe_if_changed(&recorded, &now, || {
            verify_approved_binary(
                "qemu-system-x86_64",
                &sys,
                std::slice::from_ref(&recorded.qemu_system_sha256),
            )
        })
        .unwrap_err();
        assert!(matches!(e, BackendError::UnapprovedBinary { .. }), "{e}");

        // Firmware removed: also a change.
        std::fs::remove_file(dir.join("share").join("edk2-i386-vars.fd")).unwrap();
        let now = current_fingerprint(&caps).unwrap();
        assert!(now.firmware.is_none());
        assert!(recorded.changes(&now).contains(&"firmware presence"));

        // SeaBIOS image replaced, then removed.
        assert!(recorded.seabios.is_some());
        std::fs::write(dir.join("share").join(SEABIOS_FILE), b"bios v2").unwrap();
        let now = current_fingerprint(&caps).unwrap();
        assert!(recorded.changes(&now).contains(&"SeaBIOS image"));
        std::fs::remove_file(dir.join("share").join(SEABIOS_FILE)).unwrap();
        let now = current_fingerprint(&caps).unwrap();
        assert!(recorded.changes(&now).contains(&"SeaBIOS presence"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn limited_excerpt() {
        let long = "x".repeat(5000);
        assert!(stderr_excerpt(&long).len() <= 600);
        assert_eq!(stderr_excerpt("a\n\nb\nc\nd\ne\n"), "b | c | d | e");
    }
}
