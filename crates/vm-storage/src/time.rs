//! Timestamp UTC senza dipendenze esterne.
//!
//! Formato RFC 3339 con millisecondi (`2026-10-09T12:34:56.789Z`): ordinabile
//! lessicograficamente, quindi confrontabile anche in SQL.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Istante UTC in millisecondi dall'epoca Unix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct UtcMillis(pub i64);

impl UtcMillis {
    /// Istante corrente.
    pub fn now() -> Self {
        Self::from_system_time(SystemTime::now())
    }

    /// Conversione da [`SystemTime`] (istanti prima del 1970 diventano 0).
    pub fn from_system_time(t: SystemTime) -> Self {
        let ms = t
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_millis();
        Self(i64::try_from(ms).unwrap_or(i64::MAX))
    }

    /// Conversione verso [`SystemTime`].
    pub fn to_system_time(self) -> SystemTime {
        UNIX_EPOCH + Duration::from_millis(u64::try_from(self.0).unwrap_or(0))
    }

    /// RFC 3339 con millisecondi.
    pub fn rfc3339(self) -> String {
        let (y, mo, d, h, mi, s, ms) = self.parts();
        format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{ms:03}Z")
    }

    /// Forma compatta per nomi di file (`20261009T123456Z`).
    pub fn compact(self) -> String {
        let (y, mo, d, h, mi, s, _) = self.parts();
        format!("{y:04}{mo:02}{d:02}T{h:02}{mi:02}{s:02}Z")
    }

    /// Lettura del formato prodotto da [`Self::rfc3339`].
    pub fn parse_rfc3339(s: &str) -> Option<Self> {
        let b = s.as_bytes();
        if b.len() != 24 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[23] != b'Z' {
            return None;
        }
        let num = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
        let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
        let (h, mi, sec, ms) = (num(11..13)?, num(14..16)?, num(17..19)?, num(20..23)?);
        if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || sec > 60 {
            return None;
        }
        let days = days_from_civil(y, mo, d);
        Some(Self(
            ((days * 86_400 + h * 3_600 + mi * 60 + sec) * 1_000) + ms,
        ))
    }

    fn parts(self) -> (i64, i64, i64, i64, i64, i64, i64) {
        let ms = self.0.rem_euclid(1_000);
        let secs = self.0.div_euclid(1_000);
        let days = secs.div_euclid(86_400);
        let sod = secs.rem_euclid(86_400);
        let (y, mo, d) = civil_from_days(days);
        (y, mo, d, sod / 3_600, (sod % 3_600) / 60, sod % 60, ms)
    }
}

/// Timestamp corrente in RFC 3339.
pub fn now_rfc3339() -> String {
    UtcMillis::now().rfc3339()
}

// Algoritmi di H. Hinnant per il calendario gregoriano prolettico.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_known_instant() {
        // 2026-10-09T12:34:56.789Z
        let t = UtcMillis(1_791_549_296_789);
        assert_eq!(t.rfc3339(), "2026-10-09T12:34:56.789Z");
        assert_eq!(t.compact(), "20261009T123456Z");
        assert_eq!(UtcMillis::parse_rfc3339(&t.rfc3339()), Some(t));
    }

    #[test]
    fn epoch_and_leap_day() {
        assert_eq!(UtcMillis(0).rfc3339(), "1970-01-01T00:00:00.000Z");
        let leap = UtcMillis::parse_rfc3339("2024-02-29T23:59:59.999Z").unwrap();
        assert_eq!(leap.rfc3339(), "2024-02-29T23:59:59.999Z");
        assert_eq!(UtcMillis::parse_rfc3339("2024-02-29 23:59:59"), None);
    }
}
