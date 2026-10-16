//! Catalog models: VMs, media, baselines, runs, operations, limits
//! (ROADMAP 4, 6.3, 7.2, 8).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    BaselineId, ErrorCode, Label, ManagedRelPath, MediaId, OperationId, RunId, SessionId,
    Sha256Digest, UtcTimestamp, VmId, VmName, VmState,
};

/// Declared guest operating system. In the MVP the reference is Ubuntu LTS
/// x86_64 with the version and checksum pinned in F0 (ROADMAP 4). The
/// Windows variants (ADR-0027, ROADMAP R2c) select a hardware profile with
/// devices the guest has inbox drivers for; they are manual, offline guests
/// and their compatibility cells stay "not verified" until real evidence.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum GuestOs {
    /// Ubuntu LTS x86_64 (MVP reference guest).
    UbuntuLtsX86_64,
    /// Other Linux x86_64: manageable manually, without compatibility guarantees.
    OtherLinuxX86_64,
    /// Windows XP (NT 5.1, and XP x64 / NT 5.2): legacy BIOS, i440fx, PIIX IDE.
    WindowsXp,
    /// Windows Vista (NT 6.0): legacy BIOS, q35, AHCI.
    WindowsVista,
    /// Windows 7 (NT 6.1): legacy BIOS, q35, AHCI.
    Windows7,
    /// Windows 8.1 (NT 6.3): UEFI (x64) or legacy BIOS (32-bit), q35, AHCI.
    Windows81,
    /// Windows 10 (NT 10.0, build < 22000): UEFI (x64) or legacy BIOS
    /// (32-bit), q35, AHCI.
    Windows10,
    /// Windows 11 x64: project QEMU, Secure Boot and TPM 2.0 (ADR-0035).
    Windows11X64,
    /// Windows 11 on ARM64 (build 22000 and later): macOS/HVF hosts only,
    /// `virt`, UEFI with Secure Boot, TPM 2.0 (ADR-0028).
    Windows11Arm64,
}

impl GuestOs {
    /// Every variant, in display order.
    pub const ALL: [GuestOs; 9] = [
        GuestOs::UbuntuLtsX86_64,
        GuestOs::OtherLinuxX86_64,
        GuestOs::WindowsXp,
        GuestOs::WindowsVista,
        GuestOs::Windows7,
        GuestOs::Windows81,
        GuestOs::Windows10,
        GuestOs::Windows11X64,
        GuestOs::Windows11Arm64,
    ];

    /// CPU architecture the guest runs on: AArch64 for Windows 11 ARM64,
    /// x86 (32/64-bit) for every other guest.
    #[must_use]
    pub const fn is_aarch64(self) -> bool {
        matches!(self, GuestOs::Windows11Arm64)
    }

    /// Whether the guest needs an emulated TPM 2.0 and Secure Boot
    /// (Windows 11, ADR-0028).
    #[must_use]
    pub const fn needs_tpm(self) -> bool {
        matches!(self, Self::Windows11X64 | Self::Windows11Arm64)
    }

    /// Whether this build's host can run the guest with hardware
    /// acceleration (no TCG, ADR-0007): Apple Silicon macOS (HVF) runs only
    /// AArch64 guests, every other host only x86 guests.
    #[must_use]
    pub const fn runs_on_this_host(self) -> bool {
        if matches!(self, Self::Windows11X64) {
            return cfg!(windows);
        }
        if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            self.is_aarch64()
        } else {
            !self.is_aarch64()
        }
    }

    /// [`GuestOs::runs_on_this_host`] for a guest installed from media of
    /// architecture `arch` (`None` = no media or unknown architecture).
    /// Windows hosts also refuse Windows Vista x64 (ADR-0027): under QEMU
    /// with WHPX its kernel stops with STOP 0x5C (HAL_INITIALIZATION_FAILED)
    /// right after loading its files, with every machine and CPU model; the
    /// 32-bit edition runs.
    #[must_use]
    pub const fn runs_on_this_host_with(self, arch: Option<GuestArch>) -> bool {
        let whpx_vista_x64 = cfg!(windows)
            && matches!(self, GuestOs::WindowsVista)
            && matches!(arch, Some(GuestArch::X86_64));
        self.runs_on_this_host() && !whpx_vista_x64
    }

    /// Whether the guest is a Windows release.
    #[must_use]
    pub const fn is_windows(self) -> bool {
        !matches!(self, GuestOs::UbuntuLtsX86_64 | GuestOs::OtherLinuxX86_64)
    }

    /// Firmware allowed for this guest (ADR-0027). Linux keeps UEFI only
    /// (ADR-0011); XP, Vista and 7 boot with legacy BIOS only; 8.1 and 10
    /// boot with UEFI (x64 media) or legacy BIOS (32-bit media).
    #[must_use]
    pub const fn allowed_firmware(self) -> &'static [Firmware] {
        match self {
            GuestOs::UbuntuLtsX86_64 | GuestOs::OtherLinuxX86_64 => &[Firmware::Uefi],
            GuestOs::WindowsXp | GuestOs::WindowsVista | GuestOs::Windows7 => &[Firmware::Bios],
            GuestOs::Windows81 | GuestOs::Windows10 => &[Firmware::Uefi, Firmware::Bios],
            GuestOs::Windows11X64 | GuestOs::Windows11Arm64 => &[Firmware::Uefi],
        }
    }

    /// Firmware a VM **imported** from VMware may keep (ADR-0030): the
    /// guest is already installed, so its firmware cannot change. As
    /// [`Self::allowed_firmware`], plus legacy BIOS for Linux (most VMware
    /// Linux VMs are BIOS VMs). Vista and 7 stay BIOS only: under OVMF they
    /// depend on a legacy video path the firmware does not provide.
    #[must_use]
    pub const fn import_firmware(self) -> &'static [Firmware] {
        match self {
            GuestOs::UbuntuLtsX86_64 | GuestOs::OtherLinuxX86_64 => {
                &[Firmware::Uefi, Firmware::Bios]
            }
            other => other.allowed_firmware(),
        }
    }

    /// Recommended firmware for the guest and the architecture of its
    /// installation media (`None` = unknown).
    #[must_use]
    pub const fn recommended_firmware(self, arch: Option<GuestArch>) -> Firmware {
        match self {
            GuestOs::UbuntuLtsX86_64 | GuestOs::OtherLinuxX86_64 => Firmware::Uefi,
            GuestOs::WindowsXp | GuestOs::WindowsVista | GuestOs::Windows7 => Firmware::Bios,
            GuestOs::Windows81 | GuestOs::Windows10 => match arch {
                Some(GuestArch::X86) => Firmware::Bios,
                Some(GuestArch::X86_64 | GuestArch::Aarch64) | None => Firmware::Uefi,
            },
            GuestOs::Windows11X64 | GuestOs::Windows11Arm64 => Firmware::Uefi,
        }
    }

    /// Minimum guest memory accepted at creation, in MiB.
    #[must_use]
    pub const fn min_memory_mib(self) -> u64 {
        match self {
            GuestOs::UbuntuLtsX86_64 | GuestOs::OtherLinuxX86_64 => 512,
            GuestOs::WindowsXp => 256,
            GuestOs::WindowsVista | GuestOs::Windows7 => 1024,
            GuestOs::Windows81 | GuestOs::Windows10 => 2048,
            GuestOs::Windows11X64 | GuestOs::Windows11Arm64 => 4096,
        }
    }

    /// Minimum main disk size accepted at creation, in GiB.
    #[must_use]
    pub const fn min_disk_gib(self) -> u64 {
        match self {
            GuestOs::UbuntuLtsX86_64 | GuestOs::OtherLinuxX86_64 => 8,
            GuestOs::WindowsXp => 8,
            GuestOs::WindowsVista | GuestOs::Windows7 => 20,
            GuestOs::Windows81 | GuestOs::Windows10 => 32,
            // Microsoft lists 64 GB; the setup of the Windows 11 ARM64
            // media passes its requirements check with 32 GB (R2b-01), and
            // the qcow2 disk only grows with use.
            GuestOs::Windows11Arm64 => 32,
            GuestOs::Windows11X64 => 64,
        }
    }

    /// Standard starting template for the guest and the architecture of its
    /// installation media: typical requirements, comfortably above the
    /// vendor minimums. `None` = unknown architecture: x86 for XP (most XP
    /// media), x86_64 for everything else.
    #[must_use]
    pub const fn template(self, arch: Option<GuestArch>) -> ResourceTemplate {
        let x86 = match arch {
            Some(GuestArch::X86) => true,
            Some(GuestArch::X86_64 | GuestArch::Aarch64) => false,
            None => matches!(self, GuestOs::WindowsXp),
        };
        let (vcpus, memory_mib, disk_gib) = match (self, x86) {
            (GuestOs::UbuntuLtsX86_64, _) => (2, 4096, 32),
            (GuestOs::OtherLinuxX86_64, _) => (2, 2048, 20),
            (GuestOs::WindowsXp, true) => (1, 1024, 16),
            (GuestOs::WindowsXp, false) => (1, 2048, 20),
            (GuestOs::WindowsVista, _) => (2, 2048, 40),
            (GuestOs::Windows7, true) => (2, 2048, 32),
            (GuestOs::Windows7, false) => (2, 4096, 40),
            (GuestOs::Windows81, true) => (2, 2048, 32),
            (GuestOs::Windows81, false) => (2, 4096, 50),
            (GuestOs::Windows10, true) => (2, 2048, 40),
            (GuestOs::Windows10, false) => (2, 4096, 64),
            (GuestOs::Windows11X64 | GuestOs::Windows11Arm64, _) => (4, 6144, 64),
        };
        ResourceTemplate {
            vcpus,
            memory_mib,
            disk_gib,
        }
    }

    /// Largest guest memory that makes sense for the guest, in MiB: 32-bit
    /// x86 Windows clients address at most 4 GiB (an x86 architecture paired
    /// with Windows 11 is meaningless and not capped). `None` = no
    /// OS-specific cap (host and catalog limits still apply).
    #[must_use]
    pub const fn max_memory_mib(self, arch: Option<GuestArch>) -> Option<u64> {
        match (self.is_windows() && !self.needs_tpm(), arch) {
            (true, Some(GuestArch::X86)) => Some(4096),
            _ => None,
        }
    }

    /// Template resources for unknown architecture, as
    /// `(vcpus, memory_mib, disk_gib)`; see [`GuestOs::template`].
    #[must_use]
    pub const fn suggested_resources(self) -> (u32, u64, u64) {
        let t = self.template(None);
        (t.vcpus, t.memory_mib, t.disk_gib)
    }
}

/// Standard starting resources of a guest (see [`GuestOs::template`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ResourceTemplate {
    /// Number of vCPUs.
    pub vcpus: u32,
    /// Guest memory in MiB.
    pub memory_mib: u64,
    /// Main disk size in GiB.
    pub disk_gib: u64,
}

/// Architecture of an installation medium, as detected from its contents.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum GuestArch {
    /// 32-bit x86.
    X86,
    /// x86_64.
    X86_64,
    /// ARM64 (AArch64).
    Aarch64,
}

/// Guest detected from the contents of an installation ISO at import
/// (ADR-0027). A hint for the wizard, never a guarantee: the user confirms
/// or corrects it, and nothing in the guest is trusted because of it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DetectedGuest {
    /// Detected operating system.
    pub os: GuestOs,
    /// Detected architecture, if the medium says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arch: Option<GuestArch>,
    /// Build number (for example 6002 for Vista SP2, 19045 for Windows 10
    /// 22H2), if the medium says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<u32>,
    /// Edition or product name read from the medium (text, never interpreted).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edition: Option<Label>,
}

/// Network mode of a VM (ADR-0033). `offline` stays the default (ADR-0004):
/// no guest NIC, default network devices excluded, no forwarding.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum NetworkProfile {
    /// No NIC.
    #[default]
    Offline,
    /// QEMU user-mode network (like VMware NAT): the guest reaches the host,
    /// the LAN and the Internet. Manual use only: a run started while an
    /// agent session targets the VM gets the endpoint allowlist (when the
    /// session was granted `network.endpoints`) or no NIC, and a session is
    /// refused on a VM running with NAT.
    Nat,
    /// Endpoint allowlist: the guest NIC is attached to the supervisor's
    /// own network, where only the endpoints of [`VmConfig::endpoints`]
    /// answer (each a virtual address and port mapped to one approved
    /// `host:port`). In manual use every endpoint of the VM is allowed; in
    /// an agent session only those of the grant.
    Endpoints,
    /// Closed destination/port Internet policy through the supervisor's
    /// isolated stack. Only public IPv4 TCP 80/443 for the named endpoints;
    /// host/LAN, IPv6, UDP and every other destination stay unreachable.
    FilteredInternet,
}

impl NetworkProfile {
    /// Whether the guest has a NIC.
    #[must_use]
    pub const fn has_nic(self) -> bool {
        !matches!(self, NetworkProfile::Offline)
    }
    /// Supervisor-owned network with explicit, revocable endpoint grants.
    #[must_use]
    pub const fn is_restricted(self) -> bool {
        matches!(self, Self::Endpoints | Self::FilteredInternet)
    }
}

/// Endpoints a VM can have at most.
pub const MAX_NETWORK_ENDPOINTS: usize = 32;
/// First virtual address assigned to an endpoint (guest network
/// `10.0.2.0/24`; `.2` gateway, `.3` DNS, `.15` guest).
pub const FIRST_ENDPOINT_IP: std::net::Ipv4Addr = std::net::Ipv4Addr::new(10, 0, 2, 100);
/// Last virtual address assignable to an endpoint.
pub const LAST_ENDPOINT_IP: std::net::Ipv4Addr = std::net::Ipv4Addr::new(10, 0, 2, 254);
/// Longest endpoint name or host name.
pub const MAX_ENDPOINT_NAME_CHARS: usize = 253;

/// An endpoint of the allowlist (ADR-0033), approved by the user in the VM
/// settings: inside the guest `name` (DNS) or `virtual_ip`, port `port`;
/// on the host side the TCP service `host:port`. Only TCP is carried.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NetworkEndpoint {
    /// Name the guest uses (DNS host name syntax, no wildcard; compared
    /// without regard to case). Also resolves as `name.lightvm.internal`.
    pub name: String,
    /// Host name or IP address of the real service, resolved by the
    /// supervisor when the run starts (the address used is recorded).
    pub host: String,
    /// TCP port, the same inside the guest and on the host (1..=65535).
    pub port: u16,
    /// Virtual address in the guest network
    /// ([`FIRST_ENDPOINT_IP`]..=[`LAST_ENDPOINT_IP`]).
    pub virtual_ip: std::net::Ipv4Addr,
}

/// Checks a name of the DNS host syntax (labels of letters, digits and `-`).
fn check_dns_name(what: &str, n: &str) -> Result<(), crate::PublicError> {
    let invalid = |m: String| crate::PublicError::invalid_argument(format!("{what}: {m}"));
    let n = n.strip_suffix('.').unwrap_or(n);
    if n.is_empty() || n.len() > MAX_ENDPOINT_NAME_CHARS {
        return Err(invalid(format!(
            "must have 1..={MAX_ENDPOINT_NAME_CHARS} characters"
        )));
    }
    if n.contains('*') {
        return Err(invalid("wildcards are not allowed".into()));
    }
    for label in n.split('.') {
        if label.is_empty()
            || label.len() > 63
            || label.starts_with('-')
            || label.ends_with('-')
            || !label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(invalid(format!(
                "`{label}` is not a valid label (letters, digits and `-`, 1..=63 characters)"
            )));
        }
    }
    Ok(())
}

impl NetworkEndpoint {
    /// Normalized name (lower case, no trailing dot).
    #[must_use]
    pub fn key(&self) -> String {
        let n = self.name.strip_suffix('.').unwrap_or(&self.name);
        n.to_ascii_lowercase()
    }

    /// Structural checks of one endpoint.
    pub fn validate(&self) -> Result<(), crate::PublicError> {
        let invalid = crate::PublicError::invalid_argument;
        check_dns_name("endpoint name", &self.name)?;
        let key = self.key();
        if key
            .split('.')
            .all(|l| l.bytes().all(|b| b.is_ascii_digit()))
        {
            return Err(invalid("endpoint name: cannot be a numeric address"));
        }
        if key == "lightvm.internal" || key.ends_with(".lightvm.internal") {
            return Err(invalid(
                "endpoint name: must not end with `lightvm.internal` (added by LightVM)",
            ));
        }
        let host = self.host.trim();
        if host != self.host {
            return Err(invalid("endpoint host: no leading or trailing spaces"));
        }
        if host.contains('*') {
            return Err(invalid("endpoint host: wildcards are not allowed"));
        }
        if host.parse::<std::net::IpAddr>().is_err() {
            check_dns_name("endpoint host", host)?;
        } else if host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_unspecified() || ip.is_multicast())
        {
            return Err(invalid(
                "endpoint host: an unspecified or multicast address is not a service",
            ));
        }
        if self.port == 0 {
            return Err(invalid("endpoint port must be 1..=65535"));
        }
        if !(FIRST_ENDPOINT_IP..=LAST_ENDPOINT_IP).contains(&self.virtual_ip) {
            return Err(crate::PublicError::invalid_argument(format!(
                "endpoint virtual address must be in {FIRST_ENDPOINT_IP}..={LAST_ENDPOINT_IP}"
            )));
        }
        Ok(())
    }
}

/// Checks the endpoint list of a VM: each endpoint, at most
/// [`MAX_NETWORK_ENDPOINTS`], names unique (case-insensitive), virtual
/// `address:port` unique; the endpoint allowlist mode needs at least one.
pub fn validate_network(
    mode: NetworkProfile,
    endpoints: &[NetworkEndpoint],
) -> Result<(), crate::PublicError> {
    let invalid = crate::PublicError::invalid_argument;
    if endpoints.len() > MAX_NETWORK_ENDPOINTS {
        return Err(invalid(format!(
            "at most {MAX_NETWORK_ENDPOINTS} network endpoints"
        )));
    }
    let mut names = std::collections::BTreeSet::new();
    let mut addrs = std::collections::BTreeSet::new();
    for e in endpoints {
        e.validate()?;
        if mode == NetworkProfile::FilteredInternet
            && (!matches!(e.port, 80 | 443)
                || e.host.parse::<std::net::IpAddr>().is_ok()
                || !e.host.contains('.')
                || e.key() != e.host.trim_end_matches('.').to_ascii_lowercase())
        {
            return Err(invalid(
                "filtered Internet needs exact DNS names (name = host) and TCP port 80 or 443"
                    .into(),
            ));
        }
        if !names.insert(e.key()) {
            return Err(invalid(format!("endpoint name `{}` repeated", e.name)));
        }
        if !addrs.insert((e.virtual_ip, e.port)) {
            return Err(invalid(format!(
                "virtual endpoint {}:{} repeated",
                e.virtual_ip, e.port
            )));
        }
    }
    if mode.is_restricted() && endpoints.is_empty() {
        return Err(crate::PublicError::invalid_argument(
            "the endpoint allowlist mode needs at least one endpoint",
        ));
    }
    Ok(())
}

/// Next free virtual address for a new endpoint (the settings assign it).
#[must_use]
pub fn next_endpoint_ip(endpoints: &[NetworkEndpoint]) -> Option<std::net::Ipv4Addr> {
    (u32::from(FIRST_ENDPOINT_IP)..=u32::from(LAST_ENDPOINT_IP))
        .map(std::net::Ipv4Addr::from)
        .find(|ip| !endpoints.iter().any(|e| e.virtual_ip == *ip))
}

/// Guest firmware. Linux guests use UEFI with managed NVRAM state (ROADMAP 8,
/// ADR-0011); legacy Windows guests use SeaBIOS, which has no NVRAM
/// (ADR-0027). [`GuestOs::allowed_firmware`] says which applies.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Firmware {
    /// UEFI (OVMF) with per-VM NVRAM.
    #[default]
    Uefi,
    /// Legacy BIOS (SeaBIOS from the verified QEMU build), no NVRAM.
    Bios,
}

impl Firmware {
    /// Whether the firmware keeps per-VM NVRAM state.
    #[must_use]
    pub const fn has_nvram(self) -> bool {
        matches!(self, Firmware::Uefi)
    }
}

/// VM configuration requested by the user. Media reference the approved
/// registry. Tools share roots are explicit user-approved folder settings
/// (ADR-0031); no agent request or QEMU option is interpreted as a host path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VmConfig {
    /// Off by default. User-only settings for host integration.
    #[serde(default, skip_serializing_if = "crate::ToolsConfig::is_default")]
    pub tools: crate::ToolsConfig,
    /// Visible name.
    pub name: VmName,
    /// Declared guest operating system.
    pub guest_os: GuestOs,
    /// Number of vCPUs (not a CPU percentage limit: ROADMAP 7.2).
    pub vcpus: u32,
    /// Guest memory in MiB (not the limit of the whole QEMU process).
    pub memory_mib: u64,
    /// Virtual size of the main QCOW2 disk in GiB.
    pub disk_gib: u64,
    /// Network mode (ADR-0033); `offline` by default.
    #[serde(default)]
    pub network: NetworkProfile,
    /// Endpoints of the endpoint allowlist (ADR-0033), approved by the
    /// user; used by the `endpoints` mode and grantable to agent sessions
    /// (`network.endpoints`). Kept when the mode is another one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub endpoints: Vec<NetworkEndpoint>,
    /// Installation media from the media registry (kind `install_iso`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_media: Option<MediaId>,
    /// Optional read-only data media (kind `data_iso`), attached with the VM off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_media: Option<MediaId>,
    /// Firmware; must be one of [`GuestOs::allowed_firmware`].
    #[serde(default)]
    pub firmware: Firmware,
    /// Execution limits applied by the supervisor to the QEMU process
    /// (ADR-0017); missing in older catalogs → default values.
    #[serde(default)]
    pub limits: VmLimits,
    /// Hardware of a VM imported from VMware (ADR-0030), fixed at import:
    /// disk controller, display adapter and CPU topology of the source VM.
    /// `None` for every VM created in LightVM (the profile of ADR-0027
    /// applies unchanged).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imported: Option<ImportedHardware>,
}

/// Where an imported VM came from (ADR-0030).
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ImportSource {
    /// VMware Workstation or Player (`.vmx` + VMDK), converted to QCOW2.
    Vmware,
}

/// Main disk controller of an imported VM (ADR-0030): the controller the
/// guest was installed on, so that its boot driver finds the disk.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum DiskController {
    /// `virtio-blk-pci` (Linux guests only).
    Virtio,
    /// SATA AHCI (q35 ICH9 controller).
    Ahci,
    /// Parallel IDE (PIIX controller of the i440fx board).
    Ide,
    /// NVMe.
    Nvme,
    /// VMware paravirtual SCSI (`pvscsi`).
    Pvscsi,
    /// LSI Logic SAS 1068 (`mptsas1068`, VMware `lsisas1068`).
    LsiSas1068,
}

impl DiskController {
    /// Every controller, in display order.
    pub const ALL: [DiskController; 6] = [
        DiskController::Ahci,
        DiskController::Ide,
        DiskController::Nvme,
        DiskController::Pvscsi,
        DiskController::LsiSas1068,
        DiskController::Virtio,
    ];

    /// Whether `os` has a driver for the controller in a standard
    /// installation (Windows has no inbox virtio driver; XP has no AHCI
    /// driver).
    #[must_use]
    pub const fn usable_by(self, os: GuestOs) -> bool {
        match self {
            DiskController::Virtio => !os.is_windows(),
            DiskController::Ahci => !matches!(os, GuestOs::WindowsXp),
            DiskController::Ide
            | DiskController::Nvme
            | DiskController::Pvscsi
            | DiskController::LsiSas1068 => true,
        }
    }
}

/// Display adapter of an imported VM (ADR-0030).
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum DisplayAdapter {
    /// The adapter of the guest profile (ADR-0027: `VGA` for Windows,
    /// `virtio-vga` for Linux).
    #[default]
    Standard,
    /// VMware SVGA II (`vmware-svga`), for a Windows guest that has the
    /// VMware Tools display driver.
    VmwareSvga,
}

/// Largest number of vCPUs per socket accepted for an imported topology.
pub const MAX_CORES_PER_SOCKET: u32 = 64;

/// Hardware of a VM imported from VMware (ADR-0030), in
/// [`VmConfig::imported`]. Fixed at import: a configuration change cannot
/// alter it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ImportedHardware {
    /// Origin of the VM.
    pub source: ImportSource,
    /// Architecture of the guest, from the VMware guest type.
    pub arch: GuestArch,
    /// Controller of the main disk.
    pub disk_controller: DiskController,
    /// Display adapter.
    #[serde(default)]
    pub display: DisplayAdapter,
    /// vCPUs per socket of the source VM (`cpuid.coresPerSocket`): client
    /// editions of Windows use at most one or two sockets. `None` = one
    /// socket per vCPU, as for VMs created in LightVM.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cores_per_socket: Option<u32>,
    /// The source disk was on a controller LightVM cannot keep (VMware
    /// `lsilogic`, or NVMe with legacy BIOS) and the guest is Windows: the
    /// storage driver of [`Self::disk_controller`] must be enabled for boot
    /// in the guest before it can start from this disk (ADR-0030). Cleared
    /// when the import enabled it in the converted disk (storage helper);
    /// still set when the user must prepare the guest in VMware
    /// (`docs/USER_GUIDE.md`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub storage_driver_preparation: bool,
}

impl ImportedHardware {
    /// Structural checks against the guest and the vCPU count.
    pub fn validate(&self, os: GuestOs, vcpus: u32) -> Result<(), crate::PublicError> {
        let invalid = crate::PublicError::invalid_argument;
        if os.needs_tpm() || self.arch == GuestArch::Aarch64 {
            return Err(invalid("imported VMs are x86 guests only"));
        }
        if !self.disk_controller.usable_by(os) {
            return Err(invalid(
                "the disk controller is not usable by the guest system",
            ));
        }
        if self.display == DisplayAdapter::VmwareSvga && !os.is_windows() {
            return Err(invalid(
                "the VMware SVGA display is offered to Windows guests only",
            ));
        }
        if let Some(c) = self.cores_per_socket
            && (c == 0 || c > MAX_CORES_PER_SOCKET || !vcpus.is_multiple_of(c))
        {
            return Err(invalid("cores per socket must divide the number of vCPUs"));
        }
        Ok(())
    }
}

/// Maximum size of an installation ISO in the media registry (16 GiB;
/// ADR-0008).
pub const MAX_INSTALL_MEDIA_BYTES: u64 = 16 << 30;
/// Maximum size of a read-only data ISO (8 GiB, dual-layer
/// DVD; ADR-0008). It also counts towards the storage budget (ADR-0017).
pub const MAX_DATA_MEDIA_BYTES: u64 = 8 << 30;

impl MediaKind {
    /// Maximum size allowed for this media kind.
    #[must_use]
    pub const fn max_bytes(self) -> u64 {
        match self {
            MediaKind::InstallIso => MAX_INSTALL_MEDIA_BYTES,
            MediaKind::DataIso => MAX_DATA_MEDIA_BYTES,
        }
    }
}

/// Minimum allowed CPU cap, as a percentage of the vCPU capacity.
pub const MIN_CPU_MAX_PERCENT: u8 = 5;
/// Minimum memory margin beyond the guest RAM, in MiB.
pub const MIN_MEMORY_MARGIN_MIB: u32 = 256;
/// Maximum memory margin beyond the guest RAM, in MiB.
pub const MAX_MEMORY_MARGIN_MIB: u32 = 16_384;
/// Maximum storage budget per VM, in GiB.
pub const MAX_STORAGE_BUDGET_GIB: u64 = 16_384;

/// Execution limits of a VM (ADR-0017), set by the user in the
/// VM settings and applied by the supervisor, never by the client.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct VmLimits {
    /// CPU cap as a percentage of the capacity of the assigned vCPUs
    /// ([`MIN_CPU_MAX_PERCENT`]..=100; 100 = no cap). Applied by the
    /// operating system to the whole QEMU process.
    pub cpu_max_percent: u8,
    /// Memory granted to the QEMU process beyond the guest RAM, in MiB
    /// ([`MIN_MEMORY_MARGIN_MIB`]..=[`MAX_MEMORY_MARGIN_MIB`]). When the
    /// limit is exceeded, QEMU is terminated and the exit is classified as a resource limit.
    pub memory_margin_mib: u32,
    /// Maximum space in GiB for the VM's files beyond the main disk
    /// (overlays, baselines, firmware variables); `None` = no dedicated budget.
    /// Checked at every start.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storage_budget_gib: Option<u64>,
}

impl Default for VmLimits {
    fn default() -> Self {
        Self {
            cpu_max_percent: 100,
            memory_margin_mib: 1024,
            storage_budget_gib: None,
        }
    }
}

impl VmLimits {
    /// Structural validation of the limits.
    pub fn validate(&self) -> Result<(), crate::PublicError> {
        if !(MIN_CPU_MAX_PERCENT..=100).contains(&self.cpu_max_percent) {
            return Err(crate::PublicError::invalid_argument(format!(
                "cpu_max_percent outside the range {MIN_CPU_MAX_PERCENT}..=100"
            )));
        }
        if !(MIN_MEMORY_MARGIN_MIB..=MAX_MEMORY_MARGIN_MIB).contains(&self.memory_margin_mib) {
            return Err(crate::PublicError::invalid_argument(format!(
                "memory_margin_mib outside the range {MIN_MEMORY_MARGIN_MIB}..={MAX_MEMORY_MARGIN_MIB}"
            )));
        }
        if let Some(b) = self.storage_budget_gib
            && !(1..=MAX_STORAGE_BUDGET_GIB).contains(&b)
        {
            return Err(crate::PublicError::invalid_argument(format!(
                "storage_budget_gib outside the range 1..={MAX_STORAGE_BUDGET_GIB}"
            )));
        }
        Ok(())
    }
}

/// Catalog VM with configuration revision and last persisted state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VmRecord {
    /// Identifier.
    pub id: VmId,
    /// Current configuration.
    pub config: VmConfig,
    /// Configuration revision, incremented on every change (revision check).
    pub config_revision: u64,
    /// Last persisted state: a hint, the observed state takes precedence.
    pub persisted_state: VmState,
    /// Creation instant.
    pub created_at: UtcTimestamp,
}

/// Kind of media in the media registry.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    /// Guest installation ISO.
    InstallIso,
    /// Read-only data ISO to bring the software under test into the guest (ADR-0008).
    DataIso,
}

/// Approved media, as seen by clients. Sent only to the `ui` role
/// (`list_media`, `import_media`): agent sessions never receive it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MediaRecord {
    /// Identifier.
    pub id: MediaId,
    /// Kind of media.
    pub kind: MediaKind,
    /// SHA-256 of the file, computed at approval and re-verified before
    /// every start.
    pub sha256: Sha256Digest,
    /// Size in bytes of the file.
    pub size_bytes: u64,
    /// Origin declared by the user (text, never interpreted).
    pub declared_origin: Label,
    /// Instant of the approval.
    pub approved_at: UtcTimestamp,
    /// Guest detected from the contents at import (installation ISOs only;
    /// `None` = not recognized, data medium, or imported before detection).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detected_guest: Option<DetectedGuest>,
    /// Where the ISO is, shown to the user (ADR-0029): absolute path of the
    /// user's own file, used in place and never copied. `None` for a
    /// read-only copy kept in the catalog (media imported before ADR-0029).
    /// Informative text: no request carries it back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    /// The file was not found at [`Self::location`] the last time it was
    /// looked for (moved, renamed, deleted, or its drive is not connected):
    /// no VM can use the medium until the file is back there or the ISO is
    /// imported again.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub missing: bool,
}

/// Where the bytes of an approved medium are (internal only, no public
/// schema).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaLocation {
    /// Read-only copy in the catalog, relative to its root (media imported
    /// before ADR-0029).
    Managed(ManagedRelPath),
    /// The user's own file, used in place (ADR-0029): absolute path as
    /// resolved by the system when the file was selected.
    Referenced(std::path::PathBuf),
}

impl MediaLocation {
    /// File name of a referenced medium, for messages (never the full
    /// path); `None` for a managed copy.
    #[must_use]
    pub fn file_name(&self) -> Option<String> {
        match self {
            Self::Managed(_) => None,
            Self::Referenced(p) => p.file_name().map(|n| n.to_string_lossy().into_owned()),
        }
    }
}

/// Identity of the managed file according to the OS (for example volume serial
/// number and file index on Windows, `st_dev`/`st_ino` on Unix). Detects
/// replacement of the file even when hash and size have not been recomputed yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct FileIdentity {
    /// Volume or device.
    pub volume: u64,
    /// Identifier of the file in the volume (128 bits, like `FILE_ID_INFO` on Windows;
    /// on Unix the inode in the first 8 bytes, big-endian).
    pub file_id: [u8; 16],
}

/// Full media registry entry, **internal only** to the supervisor and
/// storage: it contains the location of the file. It has no public schema.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaEntry {
    /// Public data.
    pub record: MediaRecord,
    /// Read-only managed copy, or the user's file used in place.
    pub location: MediaLocation,
    /// OS identity recorded at approval, if available.
    pub file_identity: Option<FileIdentity>,
}

/// Origin of a baseline (ROADMAP 8).
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum BaselineOrigin {
    /// Recorded after an observed clean shutdown.
    CleanShutdown,
    /// Checkpoint materialized from an overlay with the VM off.
    Checkpoint,
}

/// Baseline with the VM off: standalone disk plus firmware state, immutable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BaselineRecord {
    /// Identifier.
    pub id: BaselineId,
    /// Owning VM.
    pub vm_id: VmId,
    /// VM configuration revision recorded with the baseline.
    pub config_revision: u64,
    /// Origin.
    pub origin: BaselineOrigin,
    /// SHA-256 of the baseline's disk image.
    pub disk_sha256: Sha256Digest,
    /// SHA-256 of the baseline's firmware state (NVRAM); `None` for a
    /// firmware without NVRAM (legacy BIOS, ADR-0027).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub firmware_sha256: Option<Sha256Digest>,
    /// Protected initial baseline: cannot be deleted or replaced by agent tools.
    pub protected: bool,
    /// Optional label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<Label>,
    /// Creation instant.
    pub created_at: UtcTimestamp,
}

/// Reason a run ended.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum RunEndReason {
    /// Ordinary stop completed (process end observed).
    Stopped,
    /// Forced stop requested (unclean state).
    ForceStopped,
    /// Emergency Stop.
    EmergencyStop,
    /// Start failed.
    StartFailed,
    /// Unexpected process exit.
    UnexpectedExit,
    /// Run closed by reconciliation after a supervisor crash.
    Reconciled,
}

/// Run: one QEMU process from start to observed end.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RunRecord {
    /// Identifier.
    pub id: RunId,
    /// VM.
    pub vm_id: VmId,
    /// Configuration revision used for the start.
    pub config_revision: u64,
    /// Starting baseline (from F3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_id: Option<BaselineId>,
    /// Start instant.
    pub started_at: UtcTimestamp,
    /// Instant of the observed end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<UtcTimestamp>,
    /// Reason for the end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_reason: Option<RunEndReason>,
}

/// Kind of operation of the per-VM queue.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    /// Creation of the VM in the catalog.
    Create,
    /// Start.
    Start,
    /// Ordinary stop.
    Stop,
    /// Forced stop.
    ForceStop,
    /// Guest reset.
    Reset,
    /// Screen capture.
    Screenshot,
    /// Typed input.
    Input,
    /// Checkpoint with the VM off (from F3).
    Snapshot,
    /// Restore of a baseline with the VM off (from F3).
    Restore,
    /// Emergency Stop (priority path, outside the queue).
    EmergencyStop,
    /// User snapshot taken, VM off or running (ADR-0032; outside the queue).
    UserSnapshot,
    /// Revert to a user snapshot with the VM off (ADR-0032; outside the
    /// queue).
    UserSnapshotRevert,
    /// Deletion of a user snapshot (ADR-0032; outside the queue).
    UserSnapshotDelete,
}

impl OperationKind {
    /// `true` for lifecycle mutations: at most one in progress or queued per VM.
    #[must_use]
    pub const fn is_lifecycle_mutation(self) -> bool {
        matches!(
            self,
            OperationKind::Create
                | OperationKind::Start
                | OperationKind::Stop
                | OperationKind::ForceStop
                | OperationKind::Reset
                | OperationKind::Snapshot
                | OperationKind::Restore
                | OperationKind::EmergencyStop
                | OperationKind::UserSnapshot
                | OperationKind::UserSnapshotRevert
                | OperationKind::UserSnapshotDelete
        )
    }

    /// `true` if repeating the operation with the same effect is safe (retry allowed).
    /// Input and reset are not (ROADMAP 6.3).
    #[must_use]
    pub const fn is_idempotent(self) -> bool {
        matches!(
            self,
            OperationKind::Start
                | OperationKind::Stop
                | OperationKind::ForceStop
                | OperationKind::Screenshot
                | OperationKind::EmergencyStop
        )
    }
}

/// Status of an operation.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum OperationStatus {
    /// Accepted, queued, not yet forwarded to the backend.
    Queued,
    /// Forwarded to the backend: it may produce effects even after a revocation.
    Forwarded,
    /// Completed successfully.
    Succeeded,
    /// Forwarded and failed.
    Failed,
    /// Discarded before forwarding (revocation, Emergency Stop, superseded `run_id`): no effect.
    Discarded,
}

impl OperationStatus {
    /// `true` if the status is final.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            OperationStatus::Succeeded | OperationStatus::Failed | OperationStatus::Discarded
        )
    }
}

/// Record of an operation (queryable by `operation_id`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OperationRecord {
    /// Identifier (idempotency key).
    pub id: OperationId,
    /// VM.
    pub vm_id: VmId,
    /// Run it refers to, if relevant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
    /// Requesting session, if an agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    /// Kind.
    pub kind: OperationKind,
    /// Status.
    pub status: OperationStatus,
    /// Error code if `failed` or `discarded`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorCode>,
    /// Acceptance instant.
    pub requested_at: UtcTimestamp,
    /// Instant of the last status change.
    pub updated_at: UtcTimestamp,
}

/// Resource limits (global or per VM). All fields are inclusive maxima.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct ResourceLimits {
    /// Maximum vCPUs per VM.
    pub max_vcpus: u32,
    /// Maximum guest memory per VM in MiB.
    pub max_memory_mib: u64,
    /// Maximum virtual disk per VM in GiB.
    pub max_disk_gib: u64,
    /// Concurrently running VMs (MVP: 1).
    pub max_running_vms: u32,
    /// Concurrent agent sessions (MVP: 1).
    pub max_agent_sessions: u32,
}

impl ResourceLimits {
    /// MVP limits: one running VM and one agent session (ROADMAP 4);
    /// conservative per-VM maxima, to be tuned with the F0 measurements (ADR-0017).
    pub const MVP_DEFAULT: ResourceLimits = ResourceLimits {
        max_vcpus: 8,
        max_memory_mib: 16 * 1024,
        max_disk_gib: 256,
        max_running_vms: 1,
        max_agent_sessions: 1,
    };

    /// Intersection: the minimum for each field. Neither side widens the other.
    #[must_use]
    pub fn intersect(&self, other: &ResourceLimits) -> ResourceLimits {
        ResourceLimits {
            max_vcpus: self.max_vcpus.min(other.max_vcpus),
            max_memory_mib: self.max_memory_mib.min(other.max_memory_mib),
            max_disk_gib: self.max_disk_gib.min(other.max_disk_gib),
            max_running_vms: self.max_running_vms.min(other.max_running_vms),
            max_agent_sessions: self.max_agent_sessions.min(other.max_agent_sessions),
        }
    }

    /// `true` if every field of `self` is ≤ the corresponding one of `other`.
    #[must_use]
    pub fn is_within(&self, other: &ResourceLimits) -> bool {
        self.max_vcpus <= other.max_vcpus
            && self.max_memory_mib <= other.max_memory_mib
            && self.max_disk_gib <= other.max_disk_gib
            && self.max_running_vms <= other.max_running_vms
            && self.max_agent_sessions <= other.max_agent_sessions
    }
}

/// Host capabilities detected by the probe (supplied to the core, never invented).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct HostCapacity {
    /// Logical processors.
    pub logical_cpus: u32,
    /// Total physical memory in MiB.
    pub total_memory_mib: u64,
    /// Memory to leave to the host in MiB (reserve).
    pub reserved_memory_mib: u64,
    /// Free space on the catalog volume in GiB.
    pub free_storage_gib: u64,
    /// Space to leave free in GiB (reserve, ADR-0017).
    pub reserved_storage_gib: u64,
    /// Hardware accelerator available and approved (no TCG fallback: ADR-0007).
    pub accelerator_available: bool,
    /// Screen capture available in the backend.
    pub screenshot_available: bool,
    /// Typed input available in the backend.
    pub input_available: bool,
    /// Baseline and restore available (from F3).
    pub baseline_available: bool,
}

impl HostCapacity {
    /// Limits derived from the host capacity: vCPUs ≤ logical processors, memory ≤
    /// total minus reserve, disk ≤ free minus reserve.
    #[must_use]
    pub fn as_limits(&self) -> ResourceLimits {
        ResourceLimits {
            max_vcpus: self.logical_cpus,
            max_memory_mib: self
                .total_memory_mib
                .saturating_sub(self.reserved_memory_mib),
            max_disk_gib: self
                .free_storage_gib
                .saturating_sub(self.reserved_storage_gib),
            max_running_vms: if self.accelerator_available {
                u32::MAX
            } else {
                0
            },
            max_agent_sessions: u32::MAX,
        }
    }
}

/// Guest display geometry in pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct DisplayGeometry {
    /// Width (1..=8192).
    #[schemars(range(min = 1, max = 8192))]
    pub width: u32,
    /// Height (1..=8192).
    #[schemars(range(min = 1, max = 8192))]
    pub height: u32,
}

impl DisplayGeometry {
    /// Maximum allowed side.
    pub const MAX_SIDE: u32 = 8192;

    /// Validates and constructs.
    pub fn new(width: u32, height: u32) -> Result<Self, crate::PublicError> {
        let g = Self { width, height };
        g.validate()?;
        Ok(g)
    }

    /// Checks the limits (useful after deserialization).
    pub fn validate(&self) -> Result<(), crate::PublicError> {
        if (1..=Self::MAX_SIDE).contains(&self.width) && (1..=Self::MAX_SIDE).contains(&self.height)
        {
            Ok(())
        } else {
            Err(crate::PublicError::invalid_argument(
                "display geometry outside 1..=8192",
            ))
        }
    }
}

/// Image format of a screenshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ImageFormat {
    /// PNG.
    Png,
}

/// Metadata of a screenshot produced by the backend (ROADMAP 6.4, 9). The image
/// bytes travel separately (MCP image content or IPC block),
/// never as a host path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Screenshot {
    /// VM.
    pub vm_id: VmId,
    /// Run in which it was captured.
    pub run_id: RunId,
    /// Display geometry at capture time.
    pub geometry: DisplayGeometry,
    /// Format.
    pub format: ImageFormat,
    /// SHA-256 of the image bytes.
    pub sha256: Sha256Digest,
    /// Size in bytes of the image.
    pub byte_len: u64,
    /// Capture instant.
    pub captured_at: UtcTimestamp,
}
