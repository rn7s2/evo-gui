//! UTC time helpers.
//!
//! Every timestamp this crate writes is RFC 3339 in UTC with a `Z` suffix, so
//! logs and state files never depend on the machine's local timezone.

use std::time::{SystemTime, UNIX_EPOCH};

const SECS_PER_DAY: u64 = 86_400;

/// Seconds since the Unix epoch, UTC.
pub fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `2026-09-29T09:09:56Z` — the shape evo itself writes in journal headers.
pub fn format_rfc3339(epoch: u64) -> String {
    let days = (epoch / SECS_PER_DAY) as i64;
    let rem = epoch % SECS_PER_DAY;
    let (y, m, d) = civil_from_days(days);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y,
        m,
        d,
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Now, as `2026-09-29T09:09:56Z`.
pub fn now_rfc3339() -> String {
    format_rfc3339(now_epoch())
}

/// Parse an RFC 3339 / ISO-8601 UTC timestamp into epoch seconds.
///
/// Accepts `2026-09-29T09:09:56Z`, optional fractional seconds, and an
/// explicit numeric offset (`+08:00`). Local/naive timestamps without an
/// offset are still read as UTC — evo only ever writes `Z`.
pub fn parse_rfc3339(s: &str) -> Option<u64> {
    let s = s.trim();
    let bytes = s.as_bytes();
    if bytes.len() < 19 {
        return None;
    }
    let num = |a: usize, b: usize| -> Option<i64> { s.get(a..b)?.parse::<i64>().ok() };
    if bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let (year, month, day) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let mut rest = match bytes[10] {
        b'T' | b't' | b' ' => &s[11..],
        _ => return None,
    };
    if rest.len() < 8 {
        return None;
    }
    let h: i64 = rest.get(0..2)?.parse().ok()?;
    let m: i64 = rest.get(3..5)?.parse().ok()?;
    let sec: i64 = rest.get(6..8)?.parse().ok()?;
    if h > 23 || m > 59 || sec > 60 {
        return None;
    }
    let secs_of_day = h * 3600 + m * 60 + sec;
    rest = &rest[8..];
    // Fractional seconds: parsed and dropped, we keep whole seconds.
    if let Some(stripped) = rest.strip_prefix('.') {
        let end = stripped
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(stripped.len());
        rest = &stripped[end..];
    }
    let offset = match rest.as_bytes().first() {
        None | Some(b'Z') | Some(b'z') => 0,
        Some(b'+') | Some(b'-') => {
            let sign = if rest.as_bytes()[0] == b'+' { 1 } else { -1 };
            let body = &rest[1..];
            let h: i64 = body.get(0..2)?.parse().ok()?;
            let m: i64 = match body.as_bytes().get(2) {
                Some(b':') => body.get(3..5)?.parse().ok()?,
                Some(_) => body.get(2..4)?.parse().ok()?,
                None => 0,
            };
            sign * (h * 3600 + m * 60)
        }
        _ => return None,
    };
    let days = days_from_civil(year, month, day);
    let total = days * SECS_PER_DAY as i64 + secs_of_day - offset;
    u64::try_from(total).ok()
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Civil date from days since 1970-01-01 (same algorithm, inverted).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m as u32, d as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_known_epochs() {
        assert_eq!(format_rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_rfc3339(1_000_000_000), "2001-09-09T01:46:40Z");
        // The timestamp of this repo's own journal header.
        assert_eq!(
            format_rfc3339(parse_rfc3339("2026-09-29T09:09:56Z").unwrap()),
            "2026-09-29T09:09:56Z"
        );
    }

    #[test]
    fn round_trips_and_offsets() {
        let t = parse_rfc3339("2026-09-29T09:09:56Z").unwrap();
        assert_eq!(format_rfc3339(t), "2026-09-29T09:09:56Z");
        assert_eq!(parse_rfc3339("2026-09-29T17:09:56+08:00").unwrap(), t);
        assert_eq!(parse_rfc3339("2026-09-29T09:09:56.123Z").unwrap(), t);
        assert_eq!(
            parse_rfc3339("2026-09-29T09:09:56+0800").unwrap(),
            t - 8 * 3600
        );
        assert_eq!(parse_rfc3339("2026-09-29T17:09:56+0800").unwrap(), t);
        assert!(parse_rfc3339("not a date").is_none());
        assert!(parse_rfc3339("2026-13-29T09:09:56Z").is_none());
    }

    #[test]
    fn civil_round_trip_over_a_wide_range() {
        for &(y, m, d) in &[(1970, 1, 1), (2000, 2, 29), (2026, 12, 31), (1969, 7, 20)] {
            let days = days_from_civil(y, m, d);
            let (cy, cm, cd) = civil_from_days(days);
            assert_eq!((cy, cm as i64, cd as i64), (y, m, d), "{y}-{m}-{d}");
        }
    }
}
