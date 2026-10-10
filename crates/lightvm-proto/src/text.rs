//! Validated strings: names, labels, managed paths, digests.
//!
//! No field received from a client is interpreted as a host path or option
//! (ROADMAP 6.3, 7.2 item 5): names are labels, managed paths are
//! relative to the directory controlled by the supervisor and never exposed to clients.

use std::borrow::Cow;
use std::fmt;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Validation error of a typed string. The rejected value is not
/// included: it might contain sensitive data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextError {
    /// Expected type.
    pub kind: &'static str,
    /// Stable reason for the rejection.
    pub reason: &'static str,
}

impl fmt::Display for TextError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid {}: {}", self.kind, self.reason)
    }
}

impl std::error::Error for TextError {}

macro_rules! validated_string {
    ($(#[$meta:meta])* $name:ident, $validate:path, $schema:tt) => {
        $(#[$meta])*
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            /// Validates and constructs.
            pub fn parse(value: impl Into<String>) -> Result<Self, TextError> {
                let value = value.into();
                $validate(&value).map_err(|reason| TextError { kind: stringify!($name), reason })?;
                Ok(Self(value))
            }

            /// Textual value.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let s = String::deserialize(deserializer)?;
                Self::parse(s).map_err(serde::de::Error::custom)
            }
        }

        impl JsonSchema for $name {
            fn schema_name() -> Cow<'static, str> {
                Cow::Borrowed(stringify!($name))
            }
            fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
                json_schema!($schema)
            }
        }
    };
}

fn validate_vm_name(s: &str) -> Result<(), &'static str> {
    let len = s.chars().count();
    if len == 0 || len > 64 {
        return Err("length outside 1..=64");
    }
    if !s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.'))
    {
        return Err("allowed characters: ASCII letters and digits, space, '-', '_', '.'");
    }
    if s.starts_with([' ', '.', '-']) || s.ends_with([' ', '.']) {
        return Err("cannot start with a space, '.' or '-' nor end with a space or '.'");
    }
    Ok(())
}

validated_string!(
    /// Visible name of a VM: a label, never a path. 1–64 ASCII characters
    /// `[A-Za-z0-9 ._-]`, without a leading or trailing space/dot nor a leading `-`.
    VmName,
    validate_vm_name,
    {
        "type": "string",
        "minLength": 1,
        "maxLength": 64,
        "pattern": "^[A-Za-z0-9_][A-Za-z0-9 ._-]*$",
        "description": "VM name: ASCII label, not a path."
    }
);

fn validate_label(s: &str) -> Result<(), &'static str> {
    let len = s.chars().count();
    if len == 0 || len > 256 {
        return Err("length outside 1..=256");
    }
    if s.chars().any(|c| c.is_control()) {
        return Err("control characters not allowed");
    }
    if s.trim() != s {
        return Err("leading or trailing spaces not allowed");
    }
    Ok(())
}

validated_string!(
    /// Free-text label declared by the user (for example the origin of
    /// a media item). Text only: never interpreted as a path, URL or markup.
    Label,
    validate_label,
    {
        "type": "string",
        "minLength": 1,
        "maxLength": 256,
        "description": "Text declared by the user, without control characters; never interpreted."
    }
);

fn validate_qcode(s: &str) -> Result<(), &'static str> {
    if s.is_empty() || s.len() > 32 {
        return Err("length outside 1..=32");
    }
    if !s
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    {
        return Err("allowed characters: [a-z0-9_]");
    }
    Ok(())
}

validated_string!(
    /// QEMU key name (`QKeyCode`, for example `a`, `ret`, `shift`,
    /// `bracket_left`). Grammar `[a-z0-9_]{1,32}` only: the backend rejects
    /// names not present in its own table.
    QCode,
    validate_qcode,
    {
        "type": "string",
        "pattern": "^[a-z0-9_]{1,32}$",
        "description": "QEMU QKeyCode."
    }
);

const WINDOWS_RESERVED: [&str; 4] = ["CON", "PRN", "AUX", "NUL"];

fn is_windows_reserved(component: &str) -> bool {
    let stem = component.split('.').next().unwrap_or(component);
    let upper = stem.to_ascii_uppercase();
    if WINDOWS_RESERVED.contains(&upper.as_str()) {
        return true;
    }
    let bytes = upper.as_bytes();
    bytes.len() == 4
        && (upper.starts_with("COM") || upper.starts_with("LPT"))
        && bytes[3].is_ascii_digit()
}

fn validate_managed_path(s: &str) -> Result<(), &'static str> {
    if s.is_empty() || s.len() > 255 {
        return Err("length outside 1..=255");
    }
    if s.contains('\\') {
        return Err("separator '\\' not allowed (UNC, device path)");
    }
    if s.contains(':') {
        return Err("':' not allowed (drives, alternate data streams)");
    }
    let components: Vec<&str> = s.split('/').collect();
    if components.len() > 4 {
        return Err("at most 4 components");
    }
    for c in components {
        if c.is_empty() {
            return Err("absolute path or empty component");
        }
        if c == "." || c == ".." {
            return Err("components '.' and '..' not allowed");
        }
        if c.len() > 64 {
            return Err("component longer than 64 bytes");
        }
        if !c
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
        {
            return Err("allowed characters: [A-Za-z0-9._-]");
        }
        if c.starts_with('.') || c.ends_with('.') {
            return Err("component with a leading or trailing '.'");
        }
        if is_windows_reserved(c) {
            return Err("reserved device name");
        }
    }
    Ok(())
}

validated_string!(
    /// Path **relative** to the directory managed by the supervisor, with `/`
    /// as separator. Rejects absolute paths, drives, UNC, device paths (`\\?\`, `\\.\`),
    /// `..`, alternate data streams and Windows reserved names.
    ///
    /// Internal to the supervisor and storage only: it does not appear in any type
    /// exposed to clients nor in the public schemas. Grammar validation does not
    /// replace the protected open against symlinks and junctions (ROADMAP 6.3).
    ManagedRelPath,
    validate_managed_path,
    {
        "type": "string",
        "description": "Managed relative path, internal only."
    }
);

/// SHA-256 digest (32 bytes). In JSON: 64 lowercase hexadecimal digits.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Sha256Digest([u8; 32]);

impl Sha256Digest {
    /// Digest from the 32 bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Bytes of the digest.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Parses 64 lowercase hexadecimal digits.
    pub fn parse_hex(s: &str) -> Result<Self, TextError> {
        let err = |reason| TextError {
            kind: "Sha256Digest",
            reason,
        };
        let b = s.as_bytes();
        if b.len() != 64 {
            return Err(err("64 hexadecimal digits required"));
        }
        let nibble = |c: u8| match c {
            b'0'..=b'9' => Ok(c - b'0'),
            b'a'..=b'f' => Ok(c - b'a' + 10),
            _ => Err(err("allowed digits: [0-9a-f]")),
        };
        let mut out = [0u8; 32];
        for (i, pair) in b.as_chunks::<2>().0.iter().enumerate() {
            out[i] = (nibble(pair[0])? << 4) | nibble(pair[1])?;
        }
        Ok(Self(out))
    }

    /// Lowercase hexadecimal form.
    #[must_use]
    pub fn to_hex(&self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut s = String::with_capacity(64);
        for byte in self.0 {
            s.push(char::from(HEX[usize::from(byte >> 4)]));
            s.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
        s
    }
}

impl fmt::Debug for Sha256Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Sha256Digest({})", self.to_hex())
    }
}

impl fmt::Display for Sha256Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl Serialize for Sha256Digest {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for Sha256Digest {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = Cow::<'de, str>::deserialize(deserializer)?;
        Self::parse_hex(&s).map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for Sha256Digest {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("Sha256Digest")
    }
    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "pattern": "^[0-9a-f]{64}$",
            "description": "SHA-256 in lowercase hexadecimal."
        })
    }
}
