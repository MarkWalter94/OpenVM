//! UTC instants with millisecond precision (ROADMAP 9: "UTC timestamp").

use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// UTC instant, truncated to the millisecond.
///
/// In JSON it is an RFC 3339 string with a `Z` suffix (for example
/// `2026-10-09T10:00:00.123Z`); for SQLite the integer form in Unix
/// milliseconds is available ([`UtcTimestamp::as_unix_millis`]). Truncating to
/// the millisecond makes the round-trip between the two forms stable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UtcTimestamp(jiff::Timestamp);

/// Error constructing or parsing a [`UtcTimestamp`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimestampError;

impl fmt::Display for TimestampError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid or out-of-range UTC instant")
    }
}

impl std::error::Error for TimestampError {}

impl UtcTimestamp {
    /// Instant from Unix milliseconds.
    pub fn from_unix_millis(millis: i64) -> Result<Self, TimestampError> {
        jiff::Timestamp::from_millisecond(millis)
            .map(Self)
            .map_err(|_| TimestampError)
    }

    /// Unix milliseconds.
    #[must_use]
    pub fn as_unix_millis(&self) -> i64 {
        self.0.as_millisecond()
    }

    /// Instant shifted by `millis` (may be negative); `None` if out of range.
    #[must_use]
    pub fn checked_add_millis(&self, millis: i64) -> Option<Self> {
        self.as_unix_millis()
            .checked_add(millis)
            .and_then(|m| Self::from_unix_millis(m).ok())
    }

    /// Milliseconds elapsed from `earlier` to `self` (negative if `earlier` is later).
    #[must_use]
    pub fn millis_since(&self, earlier: UtcTimestamp) -> i64 {
        self.as_unix_millis()
            .saturating_sub(earlier.as_unix_millis())
    }

    /// Current instant from the system clock (supervisor only; the core uses the `Clock` port).
    #[cfg(feature = "system-clock")]
    #[must_use]
    pub fn now() -> Self {
        let now = jiff::Timestamp::now();
        // Truncating to the millisecond cannot go out of range.
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
            "description": "RFC 3339 UTC instant with Z suffix, millisecond precision."
        })
    }
}
