//! Explicitly approved host integration (ADR-0031/0034/0036). Host paths
//! occur only in user-owned VM settings; agent tools accept share names.

use std::collections::{BTreeMap, BTreeSet};
use std::io;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{Capability, CapabilitySet, PublicError};

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum ToolCall {
    ClipboardRead,
    ClipboardWrite {
        text: String,
    },
    UiTree {
        max_depth: u32,
        max_nodes: u32,
    },
    UiInvoke {
        tree: u64,
        element: u32,
        action: ToolUiAction,
        value: Option<String>,
    },
    File {
        request: crate::ToolFileRequest,
    },
}

impl ToolCall {
    pub fn capability(&self) -> Capability {
        match self {
            Self::ClipboardRead | Self::ClipboardWrite { .. } => Capability::Clipboard,
            Self::UiTree { .. } => Capability::UiInspect,
            Self::UiInvoke { .. } => Capability::UiAct,
            Self::File { .. } => Capability::FilesTransfer,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolUiAction {
    Invoke,
    SetValue,
    Select,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ToolUiNode {
    pub id: u32,
    pub parent: Option<u32>,
    pub control_type: String,
    pub name: String,
    pub automation_id: String,
    pub value: Option<String>,
    pub enabled: bool,
    pub focused: bool,
    pub rect: [i32; 4],
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum ToolReply {
    Ok,
    Clipboard {
        text: String,
    },
    Tree {
        tree: u64,
        nodes: Vec<ToolUiNode>,
        truncated: bool,
    },
    File {
        reply: crate::ToolFileReply,
    },
    Error {
        code: String,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ToolsConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shares: Vec<ToolShare>,
    #[serde(default)]
    pub clipboard: bool,
    #[serde(default)]
    pub files_transfer: bool,
    #[serde(default)]
    pub ui_inspect: bool,
    #[serde(default)]
    pub ui_act: bool,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub usb_devices: BTreeSet<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ToolShare {
    pub name: String,
    /// Host folder chosen explicitly in the user UI. Never sent to agents
    /// or the guest; the supervisor opens it without following links.
    pub root: String,
    #[serde(default)]
    pub writable: bool,
}

/// Named subsets granted by the user. Booleans in `shares` denote write
/// permission; feature capabilities remain independently required.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ToolGrant {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub shares: BTreeMap<String, bool>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub usb_devices: BTreeSet<String>,
}

impl ToolGrant {
    pub fn is_empty(&self) -> bool {
        self.shares.is_empty() && self.usb_devices.is_empty()
    }
}

/// Names are also URL path components and Windows mapping names.
pub fn valid_tool_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 48
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        && name.as_bytes()[0].is_ascii_alphabetic()
}

impl ToolsConfig {
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }

    pub fn validate(&self) -> Result<(), PublicError> {
        let bad = |m| PublicError::invalid_argument(m).with_detail("field", "tools");
        if self.shares.len() > 32 || self.usb_devices.len() > 16 {
            return Err(bad("too many Tools shares or devices"));
        }
        if self.ui_act && !self.ui_inspect {
            return Err(bad("UI actions require UI inspection"));
        }
        let mut names = BTreeSet::new();
        for share in &self.shares {
            if !valid_tool_name(&share.name) || !names.insert(share.name.to_ascii_lowercase()) {
                return Err(bad("invalid or duplicate share name"));
            }
            if share.root.is_empty()
                || share.root.len() > 4096
                || share.root.chars().any(char::is_control)
                || !std::path::Path::new(&share.root).is_absolute()
            {
                return Err(bad("share root must be an absolute local folder"));
            }
        }
        if self.usb_devices.iter().any(|n| !valid_tool_name(n)) {
            return Err(bad("invalid USB device name"));
        }
        Ok(())
    }

    pub fn validate_grant(
        &self,
        capabilities: &CapabilitySet,
        grant: &ToolGrant,
    ) -> Result<(), PublicError> {
        let denied = |m| PublicError::invalid_argument(m).with_detail("field", "tools");
        let requested = capabilities.iter().any(|c| c.is_integration()) || !grant.is_empty();
        if requested && !self.enabled {
            return Err(denied("LightVM Tools are disabled for this VM"));
        }
        for (name, write) in &grant.shares {
            let Some(share) = self.shares.iter().find(|s| s.name == *name) else {
                return Err(denied("share is not configured on this VM"));
            };
            if !capabilities.contains(&Capability::ShareRead)
                || (*write && (!share.writable || !capabilities.contains(&Capability::ShareWrite)))
            {
                return Err(denied(
                    "share permissions exceed the configured or granted access",
                ));
            }
        }
        if capabilities.contains(&Capability::ShareWrite)
            && !capabilities.contains(&Capability::ShareRead)
        {
            return Err(denied("share.write requires share.read"));
        }
        for (cap, configured) in [
            (Capability::Clipboard, self.clipboard),
            (Capability::FilesTransfer, self.files_transfer),
            (Capability::UiInspect, self.ui_inspect),
            (Capability::UiAct, self.ui_act),
        ] {
            if capabilities.contains(&cap) && !configured {
                return Err(denied("integration is not enabled for this VM"));
            }
        }
        if capabilities.contains(&Capability::UiAct)
            && !capabilities.contains(&Capability::UiInspect)
        {
            return Err(denied("ui.act requires ui.inspect"));
        }
        if !grant.usb_devices.is_subset(&self.usb_devices)
            || (!grant.usb_devices.is_empty() && !capabilities.contains(&Capability::Usb))
        {
            return Err(denied(
                "USB devices exceed the configured or granted access",
            ));
        }
        Ok(())
    }
}

impl ToolReply {
    pub fn validate(&self) -> io::Result<()> {
        match self {
            Self::Clipboard { text } if text.len() > 64 * 1024 || text.contains('\0') => {
                Err(invalid_reply("clipboard text out of bounds"))
            }
            Self::Tree { nodes, .. } => {
                if nodes.len() > 1000 {
                    return Err(invalid_reply("tree too large"));
                }
                let mut seen = BTreeSet::new();
                for n in nodes {
                    if n.parent.is_some_and(|p| !seen.contains(&p)) || !seen.insert(n.id) {
                        return Err(invalid_reply("invalid tree links"));
                    }
                    for s in [&n.control_type, &n.name, &n.automation_id]
                        .into_iter()
                        .chain(n.value.as_ref())
                    {
                        if s.len() > 4096
                            || s.chars()
                                .any(|c| c.is_control() && !matches!(c, '\n' | '\t'))
                        {
                            return Err(invalid_reply("invalid UI text"));
                        }
                    }
                    if n.rect[2] < 0 || n.rect[3] < 0 {
                        return Err(invalid_reply("invalid UI rectangle"));
                    }
                }
                Ok(())
            }
            Self::File { reply } => reply.validate(),
            Self::Error { code }
                if code.is_empty()
                    || code.len() > 64
                    || !code.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') =>
            {
                Err(invalid_reply("invalid guest error"))
            }
            _ => Ok(()),
        }
    }
}

fn invalid_reply(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

impl ToolReply {
    pub fn validate_for(&self, call: &ToolCall) -> io::Result<()> {
        self.validate()?;
        let valid = match (call, self) {
            (_, Self::Error { .. }) => true,
            (ToolCall::ClipboardRead, Self::Clipboard { .. }) => true,
            (ToolCall::ClipboardWrite { .. } | ToolCall::UiInvoke { .. }, Self::Ok) => true,
            (
                ToolCall::UiTree {
                    max_depth,
                    max_nodes,
                },
                Self::Tree { nodes, .. },
            ) => {
                let mut depths = BTreeMap::new();
                nodes.len() <= *max_nodes as usize
                    && nodes.iter().all(|node| {
                        let depth = node
                            .parent
                            .map_or(0, |p| depths.get(&p).copied().unwrap_or(32) + 1);
                        depths.insert(node.id, depth);
                        depth <= *max_depth
                    })
            }
            (ToolCall::File { request }, Self::File { reply }) => match (request, reply) {
                (_, crate::ToolFileReply::Error { .. }) => true,
                (crate::ToolFileRequest::List { .. }, crate::ToolFileReply::Entries { .. }) => true,
                (
                    crate::ToolFileRequest::Read { offset, length, .. },
                    crate::ToolFileReply::Data {
                        offset: got, data, ..
                    },
                ) => offset == got && data.len() / 2 <= *length as usize,
                (crate::ToolFileRequest::Begin { .. }, crate::ToolFileReply::Transfer { .. }) => {
                    true
                }
                (
                    crate::ToolFileRequest::Write { .. }
                    | crate::ToolFileRequest::Commit { .. }
                    | crate::ToolFileRequest::Cancel { .. },
                    crate::ToolFileReply::Ok,
                ) => true,
                _ => false,
            },
            _ => false,
        };
        if valid {
            Ok(())
        } else {
            Err(invalid_reply("reply does not match request"))
        }
    }
}
impl std::fmt::Debug for ToolCall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolCall")
            .field("capability", &self.capability())
            .finish_non_exhaustive()
    }
}
impl std::fmt::Debug for ToolReply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ok => f.write_str("Ok"),
            Self::Clipboard { text } => f
                .debug_struct("Clipboard")
                .field("bytes", &text.len())
                .finish_non_exhaustive(),
            Self::Tree {
                tree,
                nodes,
                truncated,
            } => f
                .debug_struct("Tree")
                .field("tree", tree)
                .field("nodes", &nodes.len())
                .field("truncated", truncated)
                .finish(),
            Self::File { reply } => f.debug_tuple("File").field(reply).finish(),
            Self::Error { code } => f.debug_tuple("Error").field(code).finish(),
        }
    }
}
