//! Per-run Tools channels, bootstrap medium and effective permissions.
//! Only user-approved share roots reach this module; guests see names.

use std::collections::{BTreeMap, BTreeSet};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::time::{Duration, Instant};

use lightvm_proto::{
    Capability, CapabilitySet, PublicError, RunId, ToolGrant, ToolsConfig, UtcTimestamp, VmId,
};
use lightvm_tools::control::ControlServer;
use lightvm_tools::files::{FileRecord, FileService, Share};
use lightvm_tools::policy::{Feature, Gate, Grant, Secret};
use lightvm_tools::server::FileServer;
use serde::{Deserialize, Serialize};

pub const VIRTUAL_IP: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 99);
pub const FILE_PORT: u16 = 47081;
pub const CONTROL_PORT: u16 = 47082;
pub const FILE_ENDPOINT: &str = "lightvm-tools-files";
pub const CONTROL_ENDPOINT: &str = "lightvm-tools-control";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Policy {
    pub capabilities: CapabilitySet,
    pub grant: ToolGrant,
    pub expires: Option<UtcTimestamp>,
}

impl Policy {
    pub fn manual(config: &ToolsConfig) -> Self {
        if !config.enabled {
            return Self::default();
        }
        let mut capabilities = CapabilitySet::new();
        if !config.shares.is_empty() {
            capabilities.insert(Capability::ShareRead);
        }
        if config.shares.iter().any(|s| s.writable) {
            capabilities.insert(Capability::ShareWrite);
        }
        for (c, yes) in [
            (Capability::Clipboard, config.clipboard),
            (Capability::FilesTransfer, config.files_transfer),
            (Capability::UiInspect, config.ui_inspect),
            (Capability::UiAct, config.ui_act),
            (Capability::Usb, !config.usb_devices.is_empty()),
        ] {
            if yes {
                capabilities.insert(c);
            }
        }
        Self {
            capabilities,
            grant: ToolGrant {
                shares: config
                    .shares
                    .iter()
                    .map(|s| (s.name.clone(), s.writable))
                    .collect(),
                usb_devices: config.usb_devices.clone(),
            },
            expires: None,
        }
    }
    fn deadline(&self) -> Option<Instant> {
        self.expires.map(|t| {
            Instant::now()
                + Duration::from_millis(
                    t.as_unix_millis()
                        .saturating_sub(UtcTimestamp::now().as_unix_millis())
                        .max(0) as u64,
                )
        })
    }
    fn grant(&self) -> Grant {
        Grant {
            features: self
                .capabilities
                .iter()
                .filter_map(|c| {
                    Some(match c {
                        Capability::ShareRead => Feature::ShareRead,
                        Capability::ShareWrite => Feature::ShareWrite,
                        Capability::Clipboard => Feature::Clipboard,
                        Capability::FilesTransfer => Feature::FilesTransfer,
                        Capability::Usb => Feature::Usb,
                        Capability::UiInspect => Feature::UiInspect,
                        Capability::UiAct => Feature::UiAct,
                        _ => return None,
                    })
                })
                .collect(),
            shares: self.grant.shares.clone(),
            usb: self.grant.usb_devices.clone(),
            expires: self.deadline(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Stop {
    files: lightvm_tools::server::Stop,
    control: lightvm_tools::control::Close,
}
impl Stop {
    pub fn suspend(&self) {
        self.files.suspend();
        self.control.disconnect();
    }
    pub fn close(&self) {
        self.files.close();
        self.control.close();
    }
}

/// Secrets are serialized only onto the private per-run bootstrap medium.
#[derive(Serialize, Deserialize)]
pub struct Bootstrap {
    pub version: u32,
    pub vm: String,
    pub run: String,
    pub credential: String,
    pub control_credential: String,
    pub host: Ipv4Addr,
    pub files_port: u16,
    pub control_port: u16,
    pub shares: Vec<BootstrapShare>,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct BootstrapShare {
    pub name: String,
    pub writable: bool,
}
impl std::fmt::Debug for Bootstrap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bootstrap")
            .field("vm", &self.vm)
            .field("run", &self.run)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub struct ToolsRun {
    pub files: FileServer,
    pub control: ControlServer,
    gate: Gate,
    applied: Option<Policy>,
}

fn public(_: std::io::Error) -> PublicError {
    PublicError::infrastructure("LightVM Tools channel failed")
}

impl ToolsRun {
    pub fn load_binaries() -> Result<Vec<(&'static str, Vec<u8>)>, PublicError> {
        use sha2::{Digest, Sha256};
        use std::io::Read;
        let exe = std::env::current_exe().map_err(public)?;
        let root = exe
            .parent()
            .ok_or_else(|| PublicError::infrastructure("Tools package directory missing"))?
            .join("tools");
        let root = lightvm_tools::sandbox::Directory::open(&root).map_err(|_| PublicError::unsupported("LightVM Tools package missing: install the guest Tools bundle beside the supervisor"))?;
        let mut result = Vec::new();
        for name in ["TOOLS.EXE", "TOOLS.PY"] {
            let mut bytes = Vec::new();
            root.read_file(name)
                .map_err(public)?
                .take(16 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)
                .map_err(public)?;
            if bytes.len() > 16 * 1024 * 1024 {
                return Err(PublicError::resource_limit("Tools binary too large"));
            }
            let mut expected = String::new();
            root.read_file(&format!("{name}.sha256"))
                .map_err(public)?
                .take(128)
                .read_to_string(&mut expected)
                .map_err(public)?;
            let digest = lightvm_tools::protocol::encode(&Sha256::digest(&bytes));
            if expected.trim() != digest {
                return Err(PublicError::permission_denied(
                    "Tools binary checksum mismatch",
                ));
            }
            result.push((name, bytes));
        }
        Ok(result)
    }
    /// The caller has already created/restricted the private run directory.
    /// `binaries` are release-approved guest artifacts, never guest paths.
    pub fn start(
        vm: VmId,
        run: RunId,
        config: &ToolsConfig,
        private: &Path,
        journal: &Path,
        binaries: &[(&str, Vec<u8>)],
    ) -> Result<Self, PublicError> {
        config.validate()?;
        let shares = config
            .shares
            .iter()
            .map(|s| Share::open(Path::new(&s.root), s.writable).map(|root| (s.name.clone(), root)))
            .collect::<Result<BTreeMap<_, _>, _>>()
            .map_err(|_| {
                PublicError::permission_denied(
                    "a Tools share is missing, inaccessible or contains a link",
                )
            })?;
        let gate = Gate::new(Grant::default());
        let secret = Secret::generate().map_err(public)?;
        let control_secret = Secret::generate().map_err(public)?;
        let bootstrap = Bootstrap {
            version: 1,
            vm: vm.to_string(),
            run: run.to_string(),
            credential: secret.export(),
            control_credential: control_secret.export(),
            host: VIRTUAL_IP,
            files_port: FILE_PORT,
            control_port: CONTROL_PORT,
            shares: config
                .shares
                .iter()
                .map(|s| BootstrapShare {
                    name: s.name.clone(),
                    writable: s.writable,
                })
                .collect(),
        };
        let config_bytes = serde_json::to_vec(&bootstrap)
            .map_err(|_| PublicError::infrastructure("Tools bootstrap encoding failed"))?;
        let install = include_bytes!("../../../packaging/tools/INSTALL.CMD");
        let mut iso_files = vec![
            ("TOOLS.CFG", config_bytes.as_slice()),
            ("INSTALL.CMD", install.as_slice()),
            (
                "INSTALL.SH",
                include_bytes!("../../../packaging/tools/INSTALL.SH").as_slice(),
            ),
        ];
        iso_files.extend(
            binaries
                .iter()
                .map(|(name, bytes)| (*name, bytes.as_slice())),
        );
        let iso = lightvm_fixture::iso9660::write_iso("LIGHTVM_TOOLS", &iso_files);
        std::fs::write(private.join("tools.iso"), iso).map_err(public)?;
        let files = FileServer::start(
            vm.to_string(),
            run.to_string(),
            secret,
            gate.clone(),
            FileService::new(shares)
                .and_then(|files| {
                    files.with_journal(lightvm_tools::journal::Journal::open(journal)?)
                })
                .map_err(public)?,
        )
        .map_err(public)?;
        let control = ControlServer::start(
            vm.to_string(),
            run.to_string(),
            control_secret,
            gate.clone(),
        )
        .map_err(public)?;
        Ok(Self {
            files,
            control,
            gate,
            applied: None,
        })
    }
    pub fn stop_handle(&self) -> Stop {
        Stop {
            files: self.files.stop_handle(),
            control: self.control.close_handle(),
        }
    }
    pub fn endpoints(&self) -> [guest_net::EndpointSpec; 2] {
        [
            (FILE_ENDPOINT, FILE_PORT, self.files.address()),
            (CONTROL_ENDPOINT, CONTROL_PORT, self.control.address()),
        ]
        .map(|(name, port, target)| guest_net::EndpointSpec {
            name: name.into(),
            virtual_ip: VIRTUAL_IP,
            port,
            target,
        })
    }
    pub fn apply(&mut self, policy: Policy) {
        if self.applied.as_ref() == Some(&policy) && !self.gate.revoked() {
            return;
        }
        if !self.gate.revoked()
            && self
                .applied
                .as_ref()
                .is_some_and(|p| p.capabilities == policy.capabilities && p.grant == policy.grant)
        {
            self.gate.renew(policy.deadline());
        } else {
            self.stop_handle().suspend();
            self.gate.replace(policy.grant());
        }
        self.applied = Some(policy);
    }
    pub fn records(&self) -> Vec<FileRecord> {
        self.files.take_records()
    }
    pub fn allowed_endpoints() -> BTreeSet<String> {
        [FILE_ENDPOINT.into(), CONTROL_ENDPOINT.into()].into()
    }
    pub fn addresses(&self) -> [SocketAddr; 2] {
        [self.files.address(), self.control.address()]
    }
}
