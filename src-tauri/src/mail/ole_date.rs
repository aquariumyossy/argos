//! Convert Outlook/OLE date values to Unix seconds without going through propsys.

use chrono::{Local, NaiveDate, NaiveDateTime, TimeZone};

/// Interpret a numeric VARIANT payload as Unix seconds.
///
/// Outlook may hand back an OLE Automation date (days since 1899-12-30), a Unix
/// timestamp (seconds or milliseconds), or a Windows FILETIME.
pub fn numeric_to_unix(n: f64) -> i64 {
    if !n.is_finite() {
        return 0;
    }
    let abs = n.abs();
    if abs < f64::EPSILON {
        return 0;
    }
    // FILETIME: 100-ns ticks since 1601-01-01 (~1.3e17 in the 2020s).
    if abs >= 1.0e16 {
        let unix = (n / 10_000_000.0) - 11_644_473_600.0;
        return positive(unix.round() as i64);
    }
    // Unix milliseconds (~1.7e12 in the 2020s).
    if abs >= 1.0e12 {
        return positive((n / 1000.0).round() as i64);
    }
    // Unix seconds (~1.7e9 in the 2020s). OLE days never reach 1e8.
    if abs >= 1.0e8 {
        return positive(n.round() as i64);
    }
    ole_to_naive(n)
        .map(naive_local_to_unix)
        .unwrap_or(0)
}

pub fn parse_outlook_date_string(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    const FORMATS: &[&str] = &[
        "%Y/%m/%d %H:%M:%S",
        "%Y/%m/%d %H:%M",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M",
        "%Y/%m/%d",
        "%Y-%m-%d",
        "%Y年%m月%d日 %H:%M:%S",
        "%Y年%m月%d日 %H:%M",
        "%Y年%m月%d日",
    ];
    for fmt in FORMATS {
        if let Ok(dt) = NaiveDateTime::parse_from_str(s, fmt) {
            return Some(naive_local_to_unix(dt));
        }
        if let Ok(d) = NaiveDate::parse_from_str(s, fmt) {
            if let Some(dt) = d.and_hms_opt(0, 0, 0) {
                return Some(naive_local_to_unix(dt));
            }
        }
    }
    let nums = extract_ints(s);
    if nums.len() < 3 {
        return None;
    }
    let y = nums[0];
    if !(1970..=2100).contains(&y) {
        return None;
    }
    let date = NaiveDate::from_ymd_opt(y, nums[1] as u32, nums[2] as u32)?;
    let hour = *nums.get(3).unwrap_or(&0);
    let min = *nums.get(4).unwrap_or(&0);
    let sec = *nums.get(5).unwrap_or(&0);
    let time = chrono::NaiveTime::from_hms_opt(hour as u32, min as u32, sec as u32)?;
    Some(naive_local_to_unix(date.and_time(time)))
}

fn ole_to_naive(ole: f64) -> Option<NaiveDateTime> {
    if !ole.is_finite() || ole.abs() < f64::EPSILON {
        return None;
    }
    let days = ole.trunc() as i64;
    let frac = ole - ole.trunc();
    let base = NaiveDate::from_ymd_opt(1899, 12, 30)?.and_hms_opt(0, 0, 0)?;
    let midnight = base.checked_add_signed(chrono::Duration::days(days))?;
    let day_secs = (frac * 86400.0).round() as i64;
    midnight.checked_add_signed(chrono::Duration::seconds(day_secs))
}

fn naive_local_to_unix(naive: NaiveDateTime) -> i64 {
    match Local.from_local_datetime(&naive) {
        chrono::LocalResult::Single(dt) | chrono::LocalResult::Ambiguous(dt, _) => dt.timestamp(),
        chrono::LocalResult::None => naive.and_utc().timestamp(),
    }
}

fn positive(n: i64) -> i64 {
    if n > 0 {
        n
    } else {
        0
    }
}

fn extract_ints(s: &str) -> Vec<i32> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in s.chars() {
        if c.is_ascii_digit() {
            cur.push(c);
        } else if !cur.is_empty() {
            if let Ok(n) = cur.parse() {
                out.push(n);
            }
            cur.clear();
        }
    }
    if !cur.is_empty() {
        if let Ok(n) = cur.parse() {
            out.push(n);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, Timelike};

    #[test]
    fn ole_epoch_is_1970_01_01() {
        let dt = ole_to_naive(25569.0).unwrap();
        assert_eq!(dt.date(), NaiveDate::from_ymd_opt(1970, 1, 1).unwrap());
        assert_eq!(dt.time().hour(), 0);
    }

    #[test]
    fn ole_fraction_is_noon() {
        let dt = ole_to_naive(25569.5).unwrap();
        assert_eq!(dt.time().hour(), 12);
    }

    #[test]
    fn ole_2026_05_25() {
        let want = NaiveDate::from_ymd_opt(2026, 5, 25)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap();
        let ole = 25569.0 + (want.and_utc().timestamp() as f64 / 86400.0);
        let dt = ole_to_naive(ole).unwrap();
        assert_eq!(dt.date(), want.date());
    }

    #[test]
    fn unix_seconds_pass_through() {
        assert_eq!(numeric_to_unix(1_717_200_000.0), 1_717_200_000);
    }

    #[test]
    fn unix_millis_divide() {
        assert_eq!(numeric_to_unix(1_717_200_000_000.0), 1_717_200_000);
    }

    #[test]
    fn filetime_epoch() {
        // 1970-01-01 FILETIME → 0, treated as missing.
        assert_eq!(numeric_to_unix(116_444_736_000_000_000.0), 0);
    }

    #[test]
    fn parse_slash_date_local_calendar() {
        let unix = parse_outlook_date_string("2026/05/25 10:00:00").unwrap();
        let local = chrono::DateTime::from_timestamp(unix, 0)
            .unwrap()
            .with_timezone(&Local);
        assert_eq!(local.year(), 2026);
        assert_eq!(local.month(), 5);
        assert_eq!(local.day(), 25);
    }

    #[test]
    fn parse_japanese_date() {
        let unix = parse_outlook_date_string("2026年5月25日 10:00:00").unwrap();
        let local = chrono::DateTime::from_timestamp(unix, 0)
            .unwrap()
            .with_timezone(&Local);
        assert_eq!((local.year(), local.month(), local.day()), (2026, 5, 25));
    }

    #[test]
    fn empty_is_none() {
        assert!(parse_outlook_date_string("").is_none());
        assert_eq!(numeric_to_unix(0.0), 0);
    }
}
