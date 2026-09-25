//! Calendar window helpers. COM-free so sync rules can be tested without Outlook.

use chrono::{DateTime, Duration, Local, NaiveDate, TimeZone};

use crate::search::date::{self, DateFilter};

pub const BODY_CAP_CHARS: usize = 2_000;
pub const MAX_APPOINTMENTS_PER_FOLDER: usize = 1_500;
pub const RELATED_LIMIT: usize = 5;
pub const CONTENT_TERM_LIMIT: usize = 8;
pub const LIST_DEFAULT_DAYS: i64 = 14;
/// olAppointment
pub const OL_APPOINTMENT: i32 = 26;
/// olMeetingCanceled
pub const OL_MEETING_CANCELED: i32 = 5;
/// olPrivate
pub const OL_SENSITIVITY_PRIVATE: i32 = 2;
/// olFolderCalendar
pub const OL_FOLDER_CALENDAR: i32 = 9;
/// olAppointmentItem
pub const OL_APPOINTMENT_ITEM: i32 = 1;

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarSyncStats {
    pub indexed: u32,
    pub errors: u32,
    pub folders: u32,
    pub truncated: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarSyncProgress {
    pub phase: String,
    pub folder_label: String,
    pub current: u32,
    pub total: u32,
    pub message: String,
    pub indexed_total: u32,
}

#[derive(Debug, Clone)]
pub struct CalendarFolderInfo {
    pub store_id: String,
    pub entry_id: String,
    pub name: String,
    pub path_label: String,
    pub item_count: i32,
    pub is_default: bool,
}

#[derive(Debug, Clone)]
pub struct OutlookAppointment {
    pub store_id: String,
    pub entry_id: String,
    pub folder_entry_id: String,
    pub calendar_name: String,
    pub subject: String,
    pub location: String,
    pub organizer: String,
    pub attendees: String,
    pub categories: String,
    pub body: String,
    pub start_unix: i64,
    pub end_unix: i64,
    pub all_day: bool,
    pub busy_status: i32,
    pub private: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarEventView {
    pub start: String,
    pub end: String,
    pub start_unix: i64,
    pub end_unix: i64,
    pub all_day: bool,
    pub subject: String,
    pub location: String,
    pub organizer: String,
    pub attendees: String,
    pub categories: String,
    pub body: String,
    pub calendar_name: String,
    pub private: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowWalk {
    /// Before the window. Keep scanning; the next item may still be inside.
    Skip,
    /// Inside the window. Take this occurrence.
    Keep,
    /// At or after the exclusive end. Recurrence expansion must stop here.
    Stop,
}

/// Recurring occurrences are only kept inside `[start, end)`.
/// A start at or beyond `end_exclusive` ends the walk so an open-ended series
/// is not expanded forever.
pub fn walk_occurrence(start_unix: i64, window_start: i64, end_exclusive: i64) -> WindowWalk {
    if start_unix <= 0 || start_unix < window_start {
        WindowWalk::Skip
    } else if start_unix >= end_exclusive {
        WindowWalk::Stop
    } else {
        WindowWalk::Keep
    }
}

/// Jet Restrict filter. ja-JP short date is `yyyy/MM/dd`. A clock time does not
/// match that pattern (`00:00` is not `H:mm`), so Restrict returns no rows.
/// Midnight bounds are already date boundaries, so the filter is the date only.
pub fn restrict_filter(start_unix: i64, end_exclusive_unix: i64) -> String {
    format!(
        "[Start] >= '{}' AND [Start] < '{}'",
        format_outlook_jet_datetime(start_unix),
        format_outlook_jet_datetime(end_exclusive_unix)
    )
}

fn format_outlook_jet_datetime(unix: i64) -> String {
    Local
        .timestamp_opt(unix, 0)
        .single()
        .map(|dt| dt.format("%Y/%m/%d").to_string())
        .unwrap_or_else(|| "1970/01/01".into())
}

fn format_local_minute(unix: i64) -> String {
    Local
        .timestamp_opt(unix, 0)
        .single()
        .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "1970-01-01 00:00".into())
}

/// Inclusive local days: today−back through today+ahead. End unix is exclusive (next midnight).
pub fn window_bounds(days_back: u32, days_ahead: u32, now: DateTime<Local>) -> (i64, i64) {
    let today = now.date_naive();
    let start_day = today - Duration::days(days_back as i64);
    let end_day = today + Duration::days(days_ahead as i64 + 1);
    (
        naive_midnight_unix(start_day),
        naive_midnight_unix(end_day),
    )
}

fn naive_midnight_unix(date: NaiveDate) -> i64 {
    let naive = date
        .and_hms_opt(0, 0, 0)
        .expect("midnight");
    match Local.from_local_datetime(&naive) {
        chrono::LocalResult::Single(dt) | chrono::LocalResult::Ambiguous(dt, _) => dt.timestamp(),
        chrono::LocalResult::None => naive.and_utc().timestamp(),
    }
}

pub fn cap_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    s.chars().take(max_chars).collect()
}

/// Private items keep subject and times only.
pub fn redact_private(appt: &mut OutlookAppointment) {
    if !appt.private {
        return;
    }
    appt.location.clear();
    appt.organizer.clear();
    appt.attendees.clear();
    appt.categories.clear();
    appt.body.clear();
}

pub fn whitespace_terms(query: &str) -> Vec<String> {
    query
        .split(|c: char| c.is_whitespace())
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(|t| t.to_string())
        .collect()
}

pub fn take_content_terms(surfaces: &[String]) -> Vec<String> {
    surfaces
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .take(CONTENT_TERM_LIMIT)
        .collect()
}

pub fn event_matches_terms(event: &CalendarEventView, terms: &[String]) -> bool {
    if terms.is_empty() {
        return true;
    }
    let hay = if event.private {
        event.subject.to_lowercase()
    } else {
        format!(
            "{} {} {} {} {} {}",
            event.subject,
            event.location,
            event.organizer,
            event.attendees,
            event.categories,
            event.body
        )
        .to_lowercase()
    };
    terms
        .iter()
        .all(|t| hay.contains(&t.to_lowercase()))
}

pub fn in_date_filter(start_unix: i64, filter: DateFilter) -> bool {
    if !filter.is_active() {
        return true;
    }
    filter.contains(start_unix)
}

/// Today onward, nearest first, then past newest-first. Capped.
pub fn sort_related(events: Vec<CalendarEventView>, today_start_unix: i64) -> Vec<CalendarEventView> {
    let mut future = Vec::new();
    let mut past = Vec::new();
    for e in events {
        if e.start_unix >= today_start_unix {
            future.push(e);
        } else {
            past.push(e);
        }
    }
    future.sort_by_key(|e| e.start_unix);
    past.sort_by_key(|e| std::cmp::Reverse(e.start_unix));
    future.extend(past);
    future.truncate(RELATED_LIMIT);
    future
}

pub fn sort_by_start(mut events: Vec<CalendarEventView>) -> Vec<CalendarEventView> {
    events.sort_by_key(|e| e.start_unix);
    events
}

/// Outlook folder ids to keep. Empty keep drops every Outlook event (iCal is untouched).
#[cfg(test)]
fn prune_keep_ids(selected_folder_ids: &[String]) -> Vec<String> {
    selected_folder_ids.to_vec()
}

pub fn format_event_span(start_unix: i64, end_unix: i64, all_day: bool) -> (String, String) {
    if all_day {
        return (date::format_unix_ymd(&start_unix.to_string()), String::new());
    }
    (format_local_minute(start_unix), format_local_minute(end_unix))
}

pub fn to_view(appt: &OutlookAppointment) -> CalendarEventView {
    let (start, end) = format_event_span(appt.start_unix, appt.end_unix, appt.all_day);
    CalendarEventView {
        start,
        end,
        start_unix: appt.start_unix,
        end_unix: appt.end_unix,
        all_day: appt.all_day,
        subject: appt.subject.clone(),
        location: appt.location.clone(),
        organizer: appt.organizer.clone(),
        attendees: appt.attendees.clone(),
        categories: appt.categories.clone(),
        body: appt.body.clone(),
        calendar_name: appt.calendar_name.clone(),
        private: appt.private,
    }
}

/// Default list window: today through today+14 (inclusive days).
pub fn default_list_filter(now: DateTime<Local>) -> DateFilter {
    let today = now.date_naive();
    let end = today + Duration::days(LIST_DEFAULT_DAYS);
    DateFilter {
        after_unix: Some(naive_midnight_unix(today)),
        before_unix: Some(naive_midnight_unix(end + Duration::days(1)).saturating_sub(1)),
    }
}

pub fn sync_window_filter(days_back: u32, days_ahead: u32, now: DateTime<Local>) -> DateFilter {
    let (start, end_excl) = window_bounds(days_back, days_ahead, now);
    DateFilter {
        after_unix: Some(start),
        before_unix: Some(end_excl.saturating_sub(1)),
    }
}

/// True when the requested range sticks out of the synced window.
pub fn range_outside_sync(requested: DateFilter, synced: DateFilter) -> bool {
    match (requested.after_unix, synced.after_unix) {
        (Some(a), Some(s)) if a < s => return true,
        _ => {}
    }
    match (requested.before_unix, synced.before_unix) {
        (Some(b), Some(s)) if b > s => return true,
        _ => {}
    }
    false
}

pub fn ymd_of_unix(unix: i64) -> String {
    date::format_unix_ymd(&unix.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn appt(subject: &str, private: bool) -> OutlookAppointment {
        OutlookAppointment {
            store_id: "s".into(),
            entry_id: "e".into(),
            folder_entry_id: "f".into(),
            calendar_name: "予定表".into(),
            subject: subject.into(),
            location: "裁判所".into(),
            organizer: "田中".into(),
            attendees: "佐藤".into(),
            categories: "期日".into(),
            body: "準備書面".into(),
            start_unix: 1_700_000_000,
            end_unix: 1_700_003_600,
            all_day: false,
            busy_status: 2,
            private,
        }
    }

    #[test]
    fn restrict_uses_local_clock_not_raw_utc_label_only() {
        let start = Local
            .with_ymd_and_hms(2026, 9, 24, 0, 0, 0)
            .single()
            .unwrap()
            .timestamp();
        let end = Local
            .with_ymd_and_hms(2026, 9, 25, 0, 0, 0)
            .single()
            .unwrap()
            .timestamp();
        let s = restrict_filter(start, end);
        assert!(s.contains("[Start] >="), "{s}");
        assert!(s.contains("AND [Start] <"), "{s}");
        // Japanese Outlook Jet expects the short date (slashes), not ISO hyphens.
        assert!(s.contains("2026/09/24"), "{s}");
        assert!(s.contains("2026/09/25"), "{s}");
        assert!(!s.contains("00:00"), "{s}");
        assert!(!s.contains("2026-09-24"), "{s}");
    }

    #[test]
    fn recurrence_walk_stops_at_window_end() {
        let start = 1_000;
        let end = 2_000;
        assert_eq!(walk_occurrence(0, start, end), WindowWalk::Skip);
        assert_eq!(walk_occurrence(999, start, end), WindowWalk::Skip);
        assert_eq!(walk_occurrence(1_000, start, end), WindowWalk::Keep);
        assert_eq!(walk_occurrence(1_999, start, end), WindowWalk::Keep);
        assert_eq!(walk_occurrence(2_000, start, end), WindowWalk::Stop);
        assert_eq!(walk_occurrence(9_000_000, start, end), WindowWalk::Stop);
    }

    #[test]
    fn private_drops_body() {
        let mut a = appt("非公開会議", true);
        redact_private(&mut a);
        assert_eq!(a.subject, "非公開会議");
        assert!(a.body.is_empty());
        assert!(a.location.is_empty());
        assert!(a.attendees.is_empty());
    }

    #[test]
    fn private_match_ignores_body() {
        let mut a = appt("件名だけ", true);
        redact_private(&mut a);
        let view = to_view(&a);
        assert!(!event_matches_terms(&view, &["準備書面".into()]));
        assert!(event_matches_terms(&view, &["件名".into()]));
    }

    #[test]
    fn whitespace_and() {
        let view = to_view(&appt("弁論 田中", false));
        assert!(event_matches_terms(
            &view,
            &whitespace_terms("弁論 田中")
        ));
        assert!(!event_matches_terms(
            &view,
            &whitespace_terms("弁論 存在しない")
        ));
    }

    #[test]
    fn related_prefers_future_then_caps() {
        let today = Local::now();
        let today_start = naive_midnight_unix(today.date_naive());
        let mut events = Vec::new();
        for i in 0..8 {
            let mut v = to_view(&appt(&format!("f{i}"), false));
            v.start_unix = today_start + 86_400 * (i as i64 + 1);
            events.push(v);
        }
        let mut past = to_view(&appt("past", false));
        past.start_unix = today_start - 86_400;
        events.push(past);
        let out = sort_related(events, today_start);
        assert_eq!(out.len(), RELATED_LIMIT);
        assert!(out[0].start_unix >= today_start);
        assert!(out.iter().all(|e| e.subject != "past"));
    }

    #[test]
    fn prune_empty_keep_means_drop_all_outlook() {
        assert!(prune_keep_ids(&[]).is_empty());
        let keep = prune_keep_ids(&["a".into(), "b".into()]);
        assert_eq!(keep, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn content_terms_cap() {
        let surfaces: Vec<String> = (0..12).map(|i| format!("語{i}")).collect();
        assert_eq!(take_content_terms(&surfaces).len(), CONTENT_TERM_LIMIT);
    }

    #[test]
    fn mail_schema_version_not_bumped_for_calendar() {
        assert_eq!(crate::search::tantivy_backend::MAIL_INDEX_SCHEMA_VERSION, 2);
    }

    #[test]
    fn failed_folder_stays_and_unselected_is_removed() {
        let dir = std::env::temp_dir().join(format!(
            "argos-cal-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db = crate::db::Db::open(&dir.join("argos.db")).unwrap();
        let row = |folder: &str, subject: &str| crate::db::CalendarEventRow {
            path: format!("outlookcal:s/{folder}/{subject}"),
            folder_entry_id: folder.into(),
            store_id: "s".into(),
            entry_id: subject.into(),
            start_unix: 1_700_000_000,
            end_unix: 1_700_003_600,
            all_day: false,
            subject: subject.into(),
            location: String::new(),
            organizer: String::new(),
            attendees: String::new(),
            categories: String::new(),
            body: String::new(),
            calendar_name: folder.into(),
            busy_status: 2,
            private: false,
        };
        db.replace_calendar_folder_events("A", &[row("A", "keep")])
            .unwrap();
        db.replace_calendar_folder_events("B", &[row("B", "fail")])
            .unwrap();
        db.replace_calendar_folder_events("C", &[row("C", "drop")])
            .unwrap();
        // A succeeded (replaced). B failed so it is still selected and not replaced.
        // C was unselected.
        let keep = prune_keep_ids(&["A".into(), "B".into()]);
        db.delete_calendar_events_except(&keep).unwrap();
        let subjects: Vec<String> = db
            .list_calendar_events()
            .unwrap()
            .into_iter()
            .map(|e| e.subject)
            .collect();
        assert!(subjects.contains(&"keep".to_string()));
        assert!(subjects.contains(&"fail".to_string()));
        assert!(!subjects.iter().any(|s| s == "drop"));
        db.delete_calendar_events_except(&prune_keep_ids(&[])).unwrap();
        let subjects: Vec<String> = db
            .list_calendar_events()
            .unwrap()
            .into_iter()
            .map(|e| e.subject)
            .collect();
        assert!(subjects.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
