//! Identifiers.
//!
//! `VmId`, `RunId`, `OperationId`, `BaselineId`, `MediaId`, `SessionId` and
//! `GrantId` are those of `lightvm-proto` (UUID v7), re-exported from the crate
//! root. The storage-specific identifiers (`DiskId`, `FirmwareId`,
//! `ArtifactId`) do not appear in the protocol and have the same text form.
//! In the DB they are all stored as canonical text (lowercase, hyphenated)
//! through the internal traits [`IdSql`] and [`IdCol`]: the orphan rule
//! prevents implementing the `rusqlite` traits on `lightvm-proto` types.

use std::fmt;
use std::str::FromStr;

pub use lightvm_proto::{BaselineId, GrantId, MediaId, OperationId, RunId, SessionId, VmId};

macro_rules! local_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(uuid::Uuid);

        impl $name {
            /// New UUID v7 identifier from the system clock.
            #[must_use]
            pub fn new_v7() -> Self {
                Self(uuid::Uuid::now_v7())
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0.hyphenated(), f)
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                uuid::Uuid::try_parse(s).map(Self)
            }
        }

        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.collect_str(&self.0.hyphenated())
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let s = String::deserialize(d)?;
                s.parse().map_err(serde::de::Error::custom)
            }
        }
    };
}

local_id!(
    /// Identifier of a managed disk (storage only).
    DiskId
);
local_id!(
    /// Identifier of a managed firmware state (storage only).
    FirmwareId
);
local_id!(
    /// Identifier of an artifact (screenshot, log, report).
    ArtifactId
);

/// Identifier stored as text in the DB.
pub(crate) trait StorageId: fmt::Display + FromStr {}

macro_rules! storage_id {
    ($($t:ty),+) => { $(impl StorageId for $t {})+ };
}
storage_id!(
    VmId,
    RunId,
    OperationId,
    BaselineId,
    MediaId,
    SessionId,
    GrantId,
    DiskId,
    FirmwareId,
    ArtifactId
);

/// Conversion to an SQL parameter (text or NULL).
pub(crate) trait IdSql {
    fn sql(&self) -> Option<String>;
}

impl<T: StorageId> IdSql for T {
    fn sql(&self) -> Option<String> {
        Some(self.to_string())
    }
}

impl<T: StorageId> IdSql for Option<T> {
    fn sql(&self) -> Option<String> {
        self.as_ref().map(ToString::to_string)
    }
}

/// Reading from a text column (possibly NULL).
pub(crate) trait IdCol: Sized {
    fn from_text(v: Option<String>) -> Result<Self, String>;
}

impl<T: StorageId> IdCol for T {
    fn from_text(v: Option<String>) -> Result<Self, String> {
        let s = v.ok_or_else(|| "NULL identifier".to_owned())?;
        s.parse().map_err(|_| format!("invalid identifier: {s}"))
    }
}

impl<T: StorageId> IdCol for Option<T> {
    fn from_text(v: Option<String>) -> Result<Self, String> {
        v.map(|s| s.parse().map_err(|_| format!("invalid identifier: {s}")))
            .transpose()
    }
}

/// Reads an identifier (or an optional identifier) from column `i`.
pub(crate) fn idc<T: IdCol>(r: &rusqlite::Row<'_>, i: usize) -> rusqlite::Result<T> {
    let v: Option<String> = r.get(i)?;
    T::from_text(v).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(i, rusqlite::types::Type::Text, e.into())
    })
}

/// Parses an identifier read from the journal.
pub(crate) fn parse_id<T: FromStr>(s: &str) -> crate::Result<T> {
    s.parse()
        .map_err(|_| crate::StorageError::Constraint(format!("invalid identifier: {s}")))
}
