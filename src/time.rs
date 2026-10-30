//! Times: now, the local calendar, the ISO 8601 times of web APIs, and how long until or since.

use crate::i18n::t;

/// Seconds since 1970.
pub fn now() -> f64 {
    objc2_foundation::NSDate::now().timeIntervalSince1970()
}

/// The local date and time of `t`.
pub fn local(t: f64) -> libc::tm {
    unsafe {
        let secs = t as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&secs, &mut tm);
        tm
    }
}

/// `2026-10-30T18:30:00.123456Z` or `…+02:00` in seconds since 1970; None for anything else, and
/// for Go's zero time (`0001-01-01T00:00:00Z`), which Docker gives for what never happened.
pub fn iso8601(s: &str) -> Option<f64> {
    let num = |r: std::ops::Range<usize>| s.get(r).and_then(|v| v.parse::<i64>().ok());
    let (y, mo, d, h, mi, sec) = (num(0..4)?, num(5..7)?, num(8..10)?, num(11..13)?, num(14..16)?, num(17..19)?);
    if y < 1970 || s.as_bytes().get(10).is_none_or(|c| *c != b'T' && *c != b' ') {
        return None;
    }
    let mut rest = &s[19..];
    let mut frac = 0.0;
    if let Some(r) = rest.strip_prefix('.') {
        let digits = r.find(|c: char| !c.is_ascii_digit()).unwrap_or(r.len());
        frac = format!("0.{}", &r[..digits]).parse().unwrap_or(0.0);
        rest = &r[digits..];
    }
    let offset = match rest {
        "" | "Z" | "z" => 0,
        _ => {
            let sign = if rest.starts_with('-') { -1 } else { 1 };
            let hm = rest.get(1..)?.replace(':', "");
            let (oh, om) = (hm.get(0..2)?.parse::<i64>().ok()?, hm.get(2..4).and_then(|m| m.parse::<i64>().ok()).unwrap_or(0));
            sign * (oh * 3600 + om * 60)
        }
    };
    // Days from the civil date (Howard Hinnant's algorithm)
    let (y, m) = if mo <= 2 { (y - 1, mo + 9) } else { (y, mo - 3) };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * m + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some((days * 86_400 + h * 3600 + mi * 60 + sec - offset) as f64 + frac)
}

/// A span of time in its two largest units: `3 g 5 h`, `2 h 57 min`, `12 min`.
pub fn span(secs: f64) -> String {
    let s = secs.max(0.0) as i64;
    let (d, h, m) = (s / 86_400, s % 86_400 / 3600, s % 3600 / 60);
    let days = t("g", "d");
    if d > 0 {
        format!("{d} {days} {h} h")
    } else if h > 0 {
        format!("{h} h {m} min")
    } else {
        format!("{m} min")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso() {
        assert_eq!(iso8601("1970-01-01T00:00:00Z"), Some(0.0));
        assert_eq!(iso8601("2026-10-30T18:30:00+00:00"), Some(1_793_385_000.0));
        assert_eq!(iso8601("2026-10-30T20:30:00.5+02:00"), Some(1_793_385_000.5));
        assert_eq!(iso8601("2026-10-30T18:30:00.123456789Z").map(|t| t.floor()), Some(1_793_385_000.0));
        assert_eq!(iso8601("0001-01-01T00:00:00Z"), None);
        assert_eq!(iso8601("soon"), None);
    }

    #[test]
    fn spans() {
        assert_eq!(span(2.0 * 3600.0 + 57.0 * 60.0 + 30.0), "2 h 57 min");
        assert_eq!(span(12.0 * 60.0), "12 min");
        assert!(span(3.0 * 86_400.0 + 5.0 * 3600.0).ends_with(" 5 h"));
    }
}
