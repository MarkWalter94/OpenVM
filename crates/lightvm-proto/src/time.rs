//! Istanti UTC con precisione al millisecondo (ROADMAP 9: "timestamp UTC").

use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Istante UTC, troncato al millisecondo.
///
/// In JSON è una stringa RFC 3339 con suffisso `Z` (per esempio
/// `2026-10-09T10:00:00.123Z`); per SQLite è disponibile la forma intera in
/// millisecondi Unix ([`UtcTimestamp::as_unix_millis`]). La troncatura al
/// millisecondo rende stabile il round-trip tra le due forme.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UtcTimestamp(jiff::Timestamp);

/// Errore di costruzione o analisi di un [`UtcTimestamp`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimestampError;

impl fmt::Display for TimestampError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("istante UTC non valido o fuori intervallo")
    }
}

impl std::error::Error for TimestampError {}

impl UtcTimestamp {
    /// Istante da millisecondi Unix.
    pub fn from_unix_millis(millis: i64) -> Result<Self, TimestampError> {
        jiff::Timestamp::from_millisecond(millis)
            .map(Self)
            .map_err(|_| TimestampError)
    }

    /// Millisecondi Unix.
    #[must_use]
    pub fn as_unix_millis(&self) -> i64 {
        self.0.as_millisecond()
    }

    /// Istante spostato di `millis` (anche negativo); `None` se fuori intervallo.
    #[must_use]
    pub fn checked_add_millis(&self, millis: i64) -> Option<Self> {
        self.as_unix_millis()
            .checked_add(millis)
            .and_then(|m| Self::from_unix_millis(m).ok())
    }

    /// Millisecondi trascorsi da `earlier` a `self` (negativo se `earlier` è successivo).
    #[must_use]
    pub fn millis_since(&self, earlier: UtcTimestamp) -> i64 {
        self.as_unix_millis()
            .saturating_sub(earlier.as_unix_millis())
    }

    /// Istante corrente dall'orologio di sistema (solo supervisor; il core usa la porta `Clock`).
    #[cfg(feature = "system-clock")]
    #[must_use]
    pub fn now() -> Self {
        let now = jiff::Timestamp::now();
        // La troncatura al millisecondo non può uscire dall'intervallo.
        Self::from_unix_millis(now.as_millisecond()).unwrap_or(Self(now))
    }
}

impl fmt::Display for UtcTimestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:.3}", self.0)
    }
}

impl FromStr for UtcTimestamp {
    type Err = TimestampError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let ts: jiff::Timestamp = s.parse().map_err(|_| TimestampError)?;
        Self::from_unix_millis(ts.as_millisecond())
    }
}

impl Serialize for UtcTimestamp {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for UtcTimestamp {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = Cow::<'de, str>::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for UtcTimestamp {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("UtcTimestamp")
    }
    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "format": "date-time",
            "description": "Istante UTC RFC 3339 con suffisso Z, precisione al millisecondo."
        })
    }
}
