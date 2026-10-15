//! Typed identifiers (ROADMAP 6.3, 9).
//!
//! Every identifier is a newtype over a version 7 UUID (RFC 9562): 128 bits,
//! sortable by creation instant, represented in JSON as a lowercase hyphenated
//! hexadecimal string. Distinct types prevent passing a `RunId` where an
//! `OperationId` is needed.
//!
//! Identifiers **are not credentials**: a `SessionId` may appear in
//! logs and reports; the session secret is [`crate::SessionToken`]. A v7 UUID
//! contains the creation instant in milliseconds: it must not be used for data
//! that must stay confidential.

use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uuid::Uuid;

/// Error parsing an identifier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IdParseError {
    /// The string is not a UUID in valid textual form.
    Malformed,
    /// The UUID is valid but is not version 7 with the RFC 9562 variant.
    NotVersion7,
}

impl fmt::Display for IdParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed => f.write_str("invalid identifier"),
            Self::NotVersion7 => f.write_str("identifier is not a UUID v7"),
        }
    }
}

impl std::error::Error for IdParseError {}

fn check_v7(uuid: Uuid) -> Result<Uuid, IdParseError> {
    if uuid.get_version_num() == 7 && uuid.get_variant() == uuid::Variant::RFC4122 {
        Ok(uuid)
    } else {
        Err(IdParseError::NotVersion7)
    }
}

macro_rules! define_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(Uuid);

        impl $name {
            /// New identifier from the system clock and the OS random
            /// generator. Reserved for the supervisor: the core uses the
            /// `IdGenerator` port.
            #[cfg(feature = "system-clock")]
            #[must_use]
            pub fn new_v7() -> Self {
                Self(Uuid::now_v7())
            }

            /// Deterministic construction from Unix milliseconds and 10 bytes of
            /// counter/randomness (for injected generators and tests).
            #[must_use]
            pub const fn from_unix_millis(millis: u64, counter_random: [u8; 10]) -> Self {
                Self(uuid::Builder::from_unix_timestamp_millis(millis, &counter_random).into_uuid())
            }

            /// Converts an existing UUID; rejects UUIDs that are not v7.
            pub fn from_uuid(uuid: Uuid) -> Result<Self, IdParseError> {
                check_v7(uuid).map(Self)
            }

            /// Underlying UUID.
            #[must_use]
            pub const fn as_uuid(&self) -> Uuid {
                self.0
            }

            /// Bytes in network order (for 16-byte BLOB columns).
            #[must_use]
            pub const fn to_bytes(&self) -> [u8; 16] {
                *self.0.as_bytes()
            }

            /// Reconstruction from the 16 bytes; rejects UUIDs that are not v7.
            pub fn from_bytes(bytes: [u8; 16]) -> Result<Self, IdParseError> {
                Self::from_uuid(Uuid::from_bytes(bytes))
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0.hyphenated(), f)
            }
        }

        impl FromStr for $name {
            type Err = IdParseError;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                // Hyphenated form only (36 characters): no braces, URN or compact form.
                if s.len() != 36 {
                    return Err(IdParseError::Malformed);
                }
                let uuid = Uuid::try_parse(s).map_err(|_| IdParseError::Malformed)?;
                Self::from_uuid(uuid)
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.collect_str(&self.0.hyphenated())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let s = Cow::<'de, str>::deserialize(deserializer)?;
                s.parse().map_err(serde::de::Error::custom)
            }
        }

        impl JsonSchema for $name {
            fn schema_name() -> Cow<'static, str> {
                Cow::Borrowed(stringify!($name))
            }
            fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
                json_schema!({
                    "type": "string",
                    "format": "uuid",
                    "pattern": "^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$",
                    "description": concat!(stringify!($name), ": version 7 UUID, lowercase with hyphens.")
                })
            }
        }
    };
}

define_id!(
    /// Stable identifier of a catalog VM.
    VmId
);
define_id!(
    /// Identifier of a single run (one QEMU process). Requests
    /// referring to a previous `RunId` are rejected (ROADMAP 6.3).
    RunId
);
define_id!(
    /// Identifier of a requested effect: idempotency key of the per-VM queue.
    OperationId
);
define_id!(
    /// Identifier of an agent session. It is not a secret.
    SessionId
);
define_id!(
    /// Identifier of a baseline taken with the VM off (ROADMAP 8).
    BaselineId
);
define_id!(
    /// Identifier of a media item in the approved media registry (ADR-0008).
    MediaId
);
define_id!(
    /// Identifier of a user snapshot of a VM (ADR-0032): a named disk state
    /// inside the VM's own disk, with its firmware state and configuration.
    SnapshotId
);
define_id!(
    /// Identifier of a grant issued by the supervisor (ROADMAP 9).
    GrantId
);
