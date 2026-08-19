//! Local calendar-day bounds for search date filters.
//!
//! `YYYY-MM-DD` is interpreted in the local timezone, matching Outlook OLE dates
//! which are stored as local-naive → unix ([`crate::mail::ole_date`]).

use chrono::{Duration, Local, NaiveDate, TimeZone};

/// Inclusive unix-second window. `None` means that side is unbounded.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DateFilter {
    pub after_unix: Option<i64>,
    pub before_unix: Option<i64>,
}

impl DateFilter {
    pub fn is_active(self) -> bool {
        self.after_unix.is_some() || self.before_unix.is_some()
    }

    pub fn contains(self, unix: i64) -> bool {
        if unix <= 0 {
            return false;
        }
        if let Some(after) = self.after_unix {
            if unix < after {
                return false;
            }
        }
        if let Some(before) = self.before_unix {
            if unix > before {
                return false;
            }
        }
        true
    }
}

/// Parse optional `YYYY-MM-DD` bounds into an inclusive local-day window.
pub fn parse_date_range(
    after: Option<&str>,
    before: Option<&str>,
) -> Result<DateFilter, String> {
    let after_unix = match after.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => Some(day_start_unix(parse_ymd(s)?)?),
        None => None,
    };
    let before_unix = match before.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => Some(day_end_unix(parse_ymd(s)?)?),
        None => None,
    };
    if let (Some(a), Some(b)) = (after_unix, before_unix) {
        if a > b {
            return Err("開始日が終了日より後です。".into());
        }
    }
    Ok(DateFilter {
        after_unix,
        before_unix,
    })
}

pub fn parse_ymd(raw: &str) -> Result<NaiveDate, String> {
    NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d")
        .map_err(|_| format!("日付は YYYY-MM-DD で指定してください: {raw}"))
}

fn day_start_unix(date: NaiveDate) -> Result<i64, String> {
    naive_local_unix(date, 0, 0, 0)
}

fn day_end_unix(date: NaiveDate) -> Result<i64, String> {
    let next = date
        .checked_add_signed(Duration::days(1))
        .ok_or_else(|| "日付が範囲外です。".to_string())?;
    let next_start = naive_local_unix(next, 0, 0, 0)?;
    Ok(next_start.saturating_sub(1))
}

fn naive_local_unix(date: NaiveDate, h: u32, m: u32, s: u32) -> Result<i64, String> {
    let naive = date
        .and_hms_opt(h, m, s)
        .ok_or_else(|| "時刻に変換できません。".to_string())?;
    match Local.from_local_datetime(&naive) {
        chrono::LocalResult::Single(dt) | chrono::LocalResult::Ambiguous(dt, _) => {
            Ok(dt.timestamp())
        }
        chrono::LocalResult::None => Ok(naive.and_utc().timestamp()),
    }
}

/// Format a stored unix-seconds string as local `YYYY-MM-DD`. Empty if missing.
pub fn format_unix_ymd(unix_str: &str) -> String {
    let unix: i64 = unix_str.trim().parse().unwrap_or(0);
    if unix <= 0 {
        return String::new();
    }
    Local
        .timestamp_opt(unix, 0)
        .single()
        .map(|dt| dt.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

/// Today's local date as `YYYY-MM-DD`.
pub fn today_ymd() -> String {
    Local::now().format("%Y-%m-%d").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rejects_swapped_range() {
        let err = parse_date_range(Some("2026-08-18"), Some("2026-08-01")).unwrap_err();
        assert!(err.contains("開始日"));
    }

    #[test]
    fn parse_rejects_bad_format() {
        assert!(parse_date_range(Some("2026/08/18"), None).is_err());
        assert!(parse_date_range(Some("08-18"), None).is_err());
    }

    #[test]
    fn empty_sides_are_inactive() {
        let f = parse_date_range(None, None).unwrap();
        assert!(!f.is_active());
        assert!(parse_date_range(Some("  "), Some("")).unwrap() == f);
    }

    #[test]
    fn local_day_is_inclusive() {
        let f = parse_date_range(Some("2026-08-18"), Some("2026-08-18")).unwrap();
        let start = f.after_unix.expect("after");
        let end = f.before_unix.expect("before");
        assert!(end >= start);
        assert!(f.contains(start));
        assert!(f.contains(end));
        assert!(!f.contains(start - 1));
        assert!(!f.contains(end + 1));
        // A full local day is 23h, 24h, or 25h depending on DST.
        assert!((end - start) >= 23 * 3600 - 1);
        assert!((end - start) <= 25 * 3600);
    }

    #[test]
    fn format_roundtrip_today() {
        let today = today_ymd();
        let f = parse_date_range(Some(&today), Some(&today)).unwrap();
        let mid = (f.after_unix.unwrap() + f.before_unix.unwrap()) / 2;
        assert_eq!(format_unix_ymd(&mid.to_string()), today);
    }
}
