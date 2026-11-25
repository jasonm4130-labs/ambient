//! Wall-clock helpers. Timestamps are RFC 3339 with the local offset, the
//! format sessions already on disk use.

use jiff::{fmt::strtime, Timestamp, Zoned};

pub fn now() -> Zoned {
    Zoned::now()
}

pub fn rfc3339(z: &Zoned) -> String {
    strtime::format("%Y-%m-%dT%H:%M:%S%.6f%:z", z).expect("fixed format string is valid")
}

pub fn dir_stamp(z: &Zoned) -> String {
    strtime::format("%Y-%m-%dT%H%M", z).expect("fixed format string is valid")
}

pub fn now_rfc3339() -> String {
    rfc3339(&now())
}

pub fn parse_rfc3339(s: &str) -> Option<Timestamp> {
    s.trim().parse().ok()
}

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
