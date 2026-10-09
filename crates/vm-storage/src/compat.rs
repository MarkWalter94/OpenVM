//! Identificativi.
//!
//! `VmId`, `RunId`, `OperationId`, `BaselineId`, `MediaId`, `SessionId` e
//! `GrantId` sono quelli di `lightvm-proto` (UUID v7). Gli identificativi
//! propri dello storage (`DiskId`, `FirmwareId`, `ArtifactId`) non compaiono nel
//! protocollo e hanno la stessa forma. Nel DB tutti sono conservati come testo
//! canonico (minuscolo, con trattini) tramite i tratti interni [`IdSql`] e
//! [`IdCol`]: la regola di orfanità impedisce di implementare i tratti di
//! `rusqlite` sui tipi di `lightvm-proto`.

use std::fmt;
use std::str::FromStr;

pub use lightvm_proto::{BaselineId, GrantId, MediaId, OperationId, RunId, SessionId, VmId};

macro_rules! local_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(uuid::Uuid);

        impl $name {
            /// Nuovo identificativo UUID v7 dall'orologio di sistema.
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
    /// Identificativo di un disco gestito (solo storage).
    DiskId
);
local_id!(
    /// Identificativo di uno stato firmware gestito (solo storage).
    FirmwareId
);
local_id!(
    /// Identificativo di un artefatto (screenshot, log, report).
    ArtifactId
);

/// Identificativo conservato come testo nel DB.
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

/// Conversione in parametro SQL (testo o NULL).
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

/// Lettura da una colonna di testo (eventualmente NULL).
pub(crate) trait IdCol: Sized {
    fn from_text(v: Option<String>) -> Result<Self, String>;
}

impl<T: StorageId> IdCol for T {
    fn from_text(v: Option<String>) -> Result<Self, String> {
        let s = v.ok_or_else(|| "identificativo NULL".to_owned())?;
        s.parse()
            .map_err(|_| format!("identificativo non valido: {s}"))
    }
}

impl<T: StorageId> IdCol for Option<T> {
    fn from_text(v: Option<String>) -> Result<Self, String> {
        v.map(|s| {
            s.parse()
                .map_err(|_| format!("identificativo non valido: {s}"))
        })
        .transpose()
    }
}

/// Legge un identificativo (o un identificativo opzionale) dalla colonna `i`.
pub(crate) fn idc<T: IdCol>(r: &rusqlite::Row<'_>, i: usize) -> rusqlite::Result<T> {
    let v: Option<String> = r.get(i)?;
    T::from_text(v).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(i, rusqlite::types::Type::Text, e.into())
    })
}

/// Analizza un identificativo letto dal journal.
pub(crate) fn parse_id<T: FromStr>(s: &str) -> crate::Result<T> {
    s.parse()
        .map_err(|_| crate::StorageError::Constraint(format!("identificativo non valido: {s}")))
}
