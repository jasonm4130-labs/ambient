//! Wall-clock helpers. Timestamps are written as RFC 3339 with the local UTC
//! offset (`2026-11-20T10:15:30.123456+10:00`), the form capture has always
//! persisted, so older sessions and newer ones parse alike.

use jiff::{fmt::strtime, Timestamp, Zoned};

/// Now, in the system time zone.
pub fn now() -> Zoned {
    Zoned::now()
}

/// `z` as RFC 3339 with its local offset and microsecond precision.
pub fn rfc3339(z: &Zoned) -> String {
    strtime::format("%Y-%m-%dT%H:%M:%S%.6f%:z", z).expect("fixed format string is valid")
}

/// The `%Y-%m-%dT%H%M` stamp that prefixes a session directory name.
pub fn dir_stamp(z: &Zoned) -> String {
    strtime::format("%Y-%m-%dT%H%M", z).expect("fixed format string is valid")
}

/// Now as RFC 3339 with the local offset.
pub fn now_rfc3339() -> String {
    rfc3339(&now())
}

/// Parse an RFC 3339 timestamp (any offset, or `Z`) to an instant.
pub fn parse_rfc3339(s: &str) -> Option<Timestamp> {
    s.trim().parse().ok()
}

/// Whole milliseconds from `earlier` to `later`; negative if reversed.
pub fn millis_between(earlier: Timestamp, later: Timestamp) -> i64 {
    later.as_millisecond() - earlier.as_millisecond()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_round_trips_through_parse() {
        let now = now();
        let s = rfc3339(&now);
        let back = parse_rfc3339(&s).expect("own output parses");
        assert!(millis_between(back, now.timestamp()).abs() <= 1, "{s}");
        assert!(s.contains('T') && (s.ends_with('Z') || s[s.len() - 6..].contains(':')));
    }

    #[test]
    fn parses_chrono_era_strings() {
        assert!(parse_rfc3339("2026-11-01T09:30:00.123456789+10:00").is_some());
        assert!(parse_rfc3339("2026-11-01T09:30:00Z\n").is_some());
        assert!(parse_rfc3339("not a time").is_none());
    }
}
