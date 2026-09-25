//! Google Calendar (and other) iCal feeds. COM-free so parse rules can be tested
//! without Outlook or the network.

use std::collections::{HashMap, HashSet};
use std::io::{ErrorKind, Read};
use std::str::FromStr;
use std::time::Duration;

use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, TimeZone, Utc};
use icalendar::{
    Calendar, CalendarComponent, CalendarDateTime, Class, Component, DatePerhapsTime, Event,
    EventLike, EventStatus, Property,
};
use reqwest::blocking::Client;
use reqwest::redirect::{Attempt, Policy};
use reqwest::Url;
use rrule::{RRuleSet, Tz as RruleTz};
use xxhash_rust::xxh64::xxh64;

use crate::db::{CalendarEventRow, CalendarIcalFeed, Db};
use crate::llm::fetch_url::{self, FetchAccess};
use crate::mail::calendar::{
    self, cap_chars, redact_private, walk_occurrence, CalendarSyncProgress, CalendarSyncStats,
    OutlookAppointment, WindowWalk, BODY_CAP_CHARS, MAX_APPOINTMENTS_PER_FOLDER,
};
use crate::mail::MailStaHandle;

pub const ICAL_STORE_ID: &str = "ical";
pub const MAX_ICAL_FEEDS: usize = 8;
pub const ICAL_FETCH_TIMEOUT: Duration = Duration::from_secs(30);
const ICAL_MAX_BYTES: u64 = 5_000_000;
const ICAL_MAX_REDIRECTS: usize = 5;
const RRULE_SCAN_CAP: u16 = 1_500;
const DEFAULT_CALENDAR_NAME: &str = "Googleカレンダー";
const USER_AGENT: &str = "Mozilla/5.0 (compatible; Argos)";

pub fn ical_folder_id(feed_id: &str) -> String {
    format!("{ICAL_STORE_ID}:{feed_id}")
}

pub fn make_ical_path(feed_id: &str, uid: &str, start_unix: i64) -> String {
    format!("ical:{feed_id}/{uid}/{start_unix}")
}

pub fn feed_label(feed: &CalendarIcalFeed) -> String {
    let name = feed.name.trim();
    if name.is_empty() {
        DEFAULT_CALENDAR_NAME.to_string()
    } else {
        name.to_string()
    }
}

pub fn enabled_ical_feeds(feeds: &[CalendarIcalFeed]) -> Vec<CalendarIcalFeed> {
    feeds
        .iter()
        .filter(|f| f.enabled && !f.url.trim().is_empty())
        .cloned()
        .collect()
}

/// Drop empty rows, convert `webcal://` to https, assign ids, reject http / dupes / overflow.
pub fn normalize_ical_feeds(feeds: Vec<CalendarIcalFeed>) -> Result<Vec<CalendarIcalFeed>, String> {
    let mut out: Vec<CalendarIcalFeed> = Vec::new();
    let mut seen_urls: HashSet<String> = HashSet::new();
    let mut seen_ids: HashSet<String> = HashSet::new();
    for mut feed in feeds {
        let url = match normalize_ical_url(&feed.url)? {
            None => continue,
            Some(u) => u,
        };
        if !seen_urls.insert(url.to_ascii_lowercase()) {
            return Err("同じ iCal URL が重複しています。".into());
        }
        let mut id = feed.id.trim().to_string();
        if id.is_empty() || !seen_ids.insert(id.clone()) {
            id = uuid::Uuid::new_v4().to_string();
            seen_ids.insert(id.clone());
        }
        feed.id = id;
        feed.url = url;
        feed.name = feed.name.trim().to_string();
        out.push(feed);
    }
    if out.len() > MAX_ICAL_FEEDS {
        return Err(format!(
            "Google カレンダーは{MAX_ICAL_FEEDS}件までです。"
        ));
    }
    Ok(out)
}

pub fn normalize_ical_url(raw: &str) -> Result<Option<String>, String> {
    let t = raw.trim();
    if t.is_empty() {
        return Ok(None);
    }
    let mut s = t.to_string();
    if let Some(rest) = s.strip_prefix("webcal://") {
        s = format!("https://{rest}");
    } else if let Some(rest) = s.strip_prefix("WEBCAL://") {
        s = format!("https://{rest}");
    }
    let parsed = Url::parse(&s).map_err(|_| "iCal URL が正しくありません。".to_string())?;
    if parsed.scheme() == "http" {
        return Err("http の URL は使えません。https の非公開URLを貼ってください。".into());
    }
    if parsed.scheme() != "https" {
        return Err("https の iCal URL を貼ってください。".into());
    }
    fetch_url::check_url(&parsed, FetchAccess::Tool)?;
    Ok(Some(s))
}

pub fn redact_ical_url(raw: &str) -> String {
    let mut s = raw.to_string();
    while let Some(i) = s.to_ascii_lowercase().find("private-") {
        let rest = &s[i + "private-".len()..];
        let take = rest
            .find(|c: char| c == '/' || c == '?' || c == '#')
            .unwrap_or(rest.len());
        s.replace_range(i + "private-".len()..i + "private-".len() + take, "…");
        break;
    }
    s
}

pub fn appointment_to_row(appt: OutlookAppointment) -> CalendarEventRow {
    CalendarEventRow {
        path: make_ical_path(
            appt.folder_entry_id
                .strip_prefix("ical:")
                .unwrap_or(&appt.folder_entry_id),
            &appt.entry_id,
            appt.start_unix,
        ),
        folder_entry_id: appt.folder_entry_id,
        store_id: appt.store_id,
        entry_id: appt.entry_id,
        start_unix: appt.start_unix,
        end_unix: appt.end_unix,
        all_day: appt.all_day,
        subject: appt.subject,
        location: appt.location,
        organizer: appt.organizer,
        attendees: appt.attendees,
        categories: appt.categories,
        body: appt.body,
        calendar_name: appt.calendar_name,
        busy_status: appt.busy_status,
        private: appt.private,
    }
}

pub fn parse_ics(
    ics: &str,
    feed_id: &str,
    display_name: &str,
    window_start: i64,
    end_exclusive: i64,
) -> Result<(Vec<OutlookAppointment>, bool), String> {
    let text = prepare_ics_text(ics.as_bytes(), "")?;
    parse_ics_prepared(&text, feed_id, display_name, window_start, end_exclusive)
}

fn parse_ics_prepared(
    text: &str,
    feed_id: &str,
    display_name: &str,
    window_start: i64,
    end_exclusive: i64,
) -> Result<(Vec<OutlookAppointment>, bool), String> {
    let calendar: Calendar = match text.parse() {
        Ok(c) => c,
        Err(_) => {
            let stripped = strip_vtimezone(text);
            stripped
                .parse()
                .map_err(|_| "予定表を解析できません。".to_string())?
        }
    };
    let cal_name = {
        let user = display_name.trim();
        if !user.is_empty() {
            user.to_string()
        } else {
            calendar
                .get_name()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(DEFAULT_CALENDAR_NAME)
                .to_string()
        }
    };
    let default_tz = calendar.get_timezone().map(str::trim).filter(|s| !s.is_empty());

    let mut masters: Vec<RawEvent> = Vec::new();
    let mut overrides: HashMap<(String, i64), RawEvent> = HashMap::new();
    for component in &calendar.components {
        let CalendarComponent::Event(event) = component else {
            continue;
        };
        let Some(raw) = raw_event(event, default_tz) else {
            continue;
        };
        if let Some(rid) = raw.recurrence_id {
            overrides.insert((raw.uid.clone(), rid), raw);
        } else {
            masters.push(raw);
        }
    }

    let folder_id = ical_folder_id(feed_id);
    let mut out: Vec<OutlookAppointment> = Vec::new();
    let mut truncated = false;
    let mut seen: HashSet<String> = HashSet::new();

    for master in &masters {
        if master.cancelled && master.rrule.is_some() {
            continue;
        }
        if master.cancelled && master.rrule.is_none() {
            continue;
        }
        let occurrences = match expand_event(master, window_start, end_exclusive) {
            Some(v) => v,
            None => continue,
        };
        for (start, end, all_day) in occurrences {
            if let Some(over) = overrides.get(&(master.uid.clone(), start)) {
                if over.cancelled {
                    continue;
                }
                if !push_raw(
                    over,
                    over.start_unix,
                    over.end_unix,
                    over.all_day,
                    &folder_id,
                    &cal_name,
                    feed_id,
                    window_start,
                    end_exclusive,
                    &mut out,
                    &mut seen,
                    &mut truncated,
                ) {
                    break;
                }
                continue;
            }
            if !push_occurrence(
                master,
                start,
                end,
                all_day,
                &folder_id,
                &cal_name,
                feed_id,
                &mut out,
                &mut seen,
                &mut truncated,
            ) {
                break;
            }
        }
        if truncated {
            break;
        }
    }

    if !truncated {
        for over in overrides.values() {
            if over.cancelled {
                continue;
            }
            let key = format!("{}/{}", over.uid, over.start_unix);
            if seen.contains(&key) {
                continue;
            }
            let _ = push_raw(
                over,
                over.start_unix,
                over.end_unix,
                over.all_day,
                &folder_id,
                &cal_name,
                feed_id,
                window_start,
                end_exclusive,
                &mut out,
                &mut seen,
                &mut truncated,
            );
        }
    }

    out.sort_by_key(|a| a.start_unix);
    Ok((out, truncated))
}

fn push_occurrence(
    raw: &RawEvent,
    start: i64,
    end: i64,
    all_day: bool,
    folder_id: &str,
    cal_name: &str,
    feed_id: &str,
    out: &mut Vec<OutlookAppointment>,
    seen: &mut HashSet<String>,
    truncated: &mut bool,
) -> bool {
    match walk_occurrence(start, i64::MIN / 2, i64::MAX / 2) {
        WindowWalk::Keep => {}
        _ => return true,
    }
    if out.len() >= MAX_APPOINTMENTS_PER_FOLDER {
        *truncated = true;
        return false;
    }
    let key = format!("{}/{}", raw.uid, start);
    if !seen.insert(key) {
        return true;
    }
    let mut appt = appointment_from_raw(raw, start, end, all_day, folder_id, cal_name, feed_id);
    redact_private(&mut appt);
    out.push(appt);
    true
}

fn push_raw(
    raw: &RawEvent,
    start: i64,
    end: i64,
    all_day: bool,
    folder_id: &str,
    cal_name: &str,
    feed_id: &str,
    window_start: i64,
    end_exclusive: i64,
    out: &mut Vec<OutlookAppointment>,
    seen: &mut HashSet<String>,
    truncated: &mut bool,
) -> bool {
    match walk_occurrence(start, window_start, end_exclusive) {
        WindowWalk::Keep => {}
        _ => return true,
    }
    push_occurrence(
        raw, start, end, all_day, folder_id, cal_name, feed_id, out, seen, truncated,
    )
}

fn appointment_from_raw(
    raw: &RawEvent,
    start: i64,
    end: i64,
    all_day: bool,
    folder_id: &str,
    cal_name: &str,
    feed_id: &str,
) -> OutlookAppointment {
    let uid = sanitize_uid(&raw.uid, start, feed_id);
    OutlookAppointment {
        store_id: ICAL_STORE_ID.into(),
        entry_id: uid,
        folder_entry_id: folder_id.to_string(),
        calendar_name: cal_name.to_string(),
        subject: raw.summary.clone(),
        location: raw.location.clone(),
        organizer: raw.organizer.clone(),
        attendees: raw.attendees.clone(),
        categories: String::new(),
        body: cap_chars(&raw.description, BODY_CAP_CHARS),
        start_unix: start,
        end_unix: end,
        all_day,
        busy_status: raw.busy_status,
        private: raw.private,
    }
}

#[derive(Clone)]
struct RawEvent {
    uid: String,
    summary: String,
    location: String,
    description: String,
    organizer: String,
    attendees: String,
    private: bool,
    cancelled: bool,
    start_unix: i64,
    end_unix: i64,
    all_day: bool,
    rrule: Option<String>,
    dtstart_line: String,
    exdates: Vec<i64>,
    rdates: Vec<i64>,
    recurrence_id: Option<i64>,
    busy_status: i32,
}

fn raw_event(event: &Event, default_tz: Option<&str>) -> Option<RawEvent> {
    let start_prop = event.properties().get("DTSTART")?;
    let (start_unix, all_day, dtstart_line) = datetime_from_property(start_prop, default_tz)?;
    let end_unix = if let Some(end_prop) = event.properties().get("DTEND") {
        let (end, end_all_day, _) = datetime_from_property(end_prop, default_tz)?;
        if all_day && end_all_day {
            end
        } else {
            end
        }
    } else if let Some(dur) = event.property_value("DURATION") {
        start_unix + parse_ics_duration(dur).unwrap_or(if all_day { 86_400 } else { 3_600 })
    } else if all_day {
        start_unix + 86_400
    } else {
        start_unix + 3_600
    };
    let uid = event
        .get_uid()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            format!(
                "{:x}",
                xxh64(
                    format!("{}|{}|{}", event.get_summary().unwrap_or(""), start_unix, end_unix)
                        .as_bytes(),
                    0
                )
            )
        });
    let cancelled = matches!(event.get_status(), Some(EventStatus::Cancelled));
    let private = matches!(event.get_class(), Some(Class::Private | Class::Confidential));
    let recurrence_id = event
        .properties()
        .get("RECURRENCE-ID")
        .and_then(|p| datetime_from_property(p, default_tz).map(|(ts, _, _)| ts));
    let rrule = event
        .property_value("RRULE")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let mut exdates = Vec::new();
    let mut rdates = Vec::new();
    for p in props_named(event, "EXDATE") {
        exdates.extend(datetime_list_from_property(p, default_tz));
    }
    for p in props_named(event, "RDATE") {
        rdates.extend(datetime_list_from_property(p, default_tz));
    }
    let busy_status = match event.property_value("TRANSP").map(|s| s.to_ascii_uppercase()) {
        Some(s) if s == "TRANSPARENT" => 0,
        _ => 2,
    };
    Some(RawEvent {
        uid,
        summary: event.get_summary().unwrap_or("").to_string(),
        location: event.get_location().unwrap_or("").to_string(),
        description: event.get_description().unwrap_or("").to_string(),
        organizer: cal_address_label(event.properties().get("ORGANIZER")),
        attendees: attendee_labels(event),
        private,
        cancelled,
        start_unix,
        end_unix,
        all_day,
        rrule,
        dtstart_line,
        exdates,
        rdates,
        recurrence_id,
        busy_status,
    })
}

fn props_named<'a>(event: &'a Event, key: &str) -> Vec<&'a Property> {
    if let Some(list) = event.multi_properties().get(key) {
        if !list.is_empty() {
            return list.iter().collect();
        }
    }
    event.properties().get(key).into_iter().collect()
}

fn cal_address_label(prop: Option<&Property>) -> String {
    let Some(p) = prop else {
        return String::new();
    };
    if let Some(cn) = p.params().get("CN") {
        let v = cn.value().trim();
        if !v.is_empty() {
            return v.to_string();
        }
    }
    let v = p.value().trim();
    v.strip_prefix("mailto:")
        .or_else(|| v.strip_prefix("MAILTO:"))
        .unwrap_or(v)
        .to_string()
}

fn attendee_labels(event: &Event) -> String {
    let mut names: Vec<String> = Vec::new();
    for p in props_named(event, "ATTENDEE") {
        let n = cal_address_label(Some(p));
        if !n.is_empty() && !names.iter().any(|x| x == &n) {
            names.push(n);
        }
    }
    names.join("; ")
}

fn datetime_list_from_property(prop: &Property, default_tz: Option<&str>) -> Vec<i64> {
    let mut out = Vec::new();
    for part in prop.value().split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let mut p = Property::new(prop.key(), part);
        for (k, v) in prop.params() {
            p.add_parameter(k, v.value());
        }
        if let Some((ts, _, _)) = datetime_from_property(&p, default_tz) {
            out.push(ts);
        }
    }
    out
}

fn datetime_from_property(
    prop: &Property,
    default_tz: Option<&str>,
) -> Option<(i64, bool, String)> {
    if let Some(dpt) = DatePerhapsTime::from_property(prop) {
        return Some(dpt_to_unix(&dpt, default_tz));
    }
    let tzid = prop.params().get("TZID").map(|p| p.value().to_string());
    let value = prop.value().trim();
    if value.len() == 8 && value.chars().all(|c| c.is_ascii_digit()) {
        let date = NaiveDate::parse_from_str(value, "%Y%m%d").ok()?;
        let ts = date_midnight_unix(date, tzid.as_deref().or(default_tz));
        return Some((ts, true, format!("DTSTART;VALUE=DATE:{value}")));
    }
    let (naive, utc) = parse_ics_naive(value)?;
    if utc {
        let ts = Utc.from_utc_datetime(&naive).timestamp();
        return Some((ts, false, format!("DTSTART:{}Z", naive.format("%Y%m%dT%H%M%S"))));
    }
    let tz_name = tzid.as_deref().or(default_tz);
    let ts = naive_in_tz(naive, tz_name);
    let line = if let Some(tz) = tz_name {
        format!("DTSTART;TZID={tz}:{}", naive.format("%Y%m%dT%H%M%S"))
    } else {
        format!("DTSTART:{}", naive.format("%Y%m%dT%H%M%S"))
    };
    Some((ts, false, line))
}

fn dpt_to_unix(dpt: &DatePerhapsTime, default_tz: Option<&str>) -> (i64, bool, String) {
    match dpt {
        DatePerhapsTime::Date(date) => {
            let ts = date_midnight_unix(*date, default_tz);
            (
                ts,
                true,
                format!("DTSTART;VALUE=DATE:{}", date.format("%Y%m%d")),
            )
        }
        DatePerhapsTime::DateTime(CalendarDateTime::Utc(dt)) => {
            (dt.timestamp(), false, format!("DTSTART:{}", dt.format("%Y%m%dT%H%M%SZ")))
        }
        DatePerhapsTime::DateTime(CalendarDateTime::Floating(naive)) => {
            let ts = naive_in_tz(*naive, default_tz);
            let line = if let Some(tz) = default_tz {
                format!("DTSTART;TZID={tz}:{}", naive.format("%Y%m%dT%H%M%S"))
            } else {
                format!("DTSTART:{}", naive.format("%Y%m%dT%H%M%S"))
            };
            (ts, false, line)
        }
        DatePerhapsTime::DateTime(CalendarDateTime::WithTimezone { date_time, tzid }) => {
            let ts = naive_in_tz(*date_time, Some(tzid));
            (
                ts,
                false,
                format!("DTSTART;TZID={tzid}:{}", date_time.format("%Y%m%dT%H%M%S")),
            )
        }
    }
}

fn parse_ics_naive(value: &str) -> Option<(NaiveDateTime, bool)> {
    let utc = value.ends_with('Z');
    let v = value.trim_end_matches('Z');
    let fmt = if v.contains('T') {
        if v.len() == 15 {
            "%Y%m%dT%H%M%S"
        } else if v.len() == 13 {
            "%Y%m%dT%H%M"
        } else {
            return None;
        }
    } else {
        return None;
    };
    NaiveDateTime::parse_from_str(v, fmt)
        .ok()
        .map(|n| (n, utc))
}

fn date_midnight_unix(date: NaiveDate, tz_name: Option<&str>) -> i64 {
    let naive = date.and_hms_opt(0, 0, 0).expect("midnight");
    naive_in_tz(naive, tz_name)
}

fn naive_in_tz(naive: NaiveDateTime, tz_name: Option<&str>) -> i64 {
    if let Some(name) = tz_name {
        if let Ok(tz) = chrono_tz::Tz::from_str(name) {
            return match tz.from_local_datetime(&naive) {
                chrono::LocalResult::Single(dt) | chrono::LocalResult::Ambiguous(dt, _) => {
                    dt.timestamp()
                }
                chrono::LocalResult::None => naive.and_utc().timestamp(),
            };
        }
    }
    match Local.from_local_datetime(&naive) {
        chrono::LocalResult::Single(dt) | chrono::LocalResult::Ambiguous(dt, _) => dt.timestamp(),
        chrono::LocalResult::None => naive.and_utc().timestamp(),
    }
}

fn expand_event(
    raw: &RawEvent,
    window_start: i64,
    end_exclusive: i64,
) -> Option<Vec<(i64, i64, bool)>> {
    let duration = (raw.end_unix - raw.start_unix).max(0);
    if raw.rrule.is_none() && raw.rdates.is_empty() {
        return match walk_occurrence(raw.start_unix, window_start, end_exclusive) {
            WindowWalk::Keep => Some(vec![(raw.start_unix, raw.end_unix, raw.all_day)]),
            _ => Some(Vec::new()),
        };
    }
    let mut starts: Vec<i64> = Vec::new();
    if let Some(rrule) = &raw.rrule {
        match expand_rrule(&raw.dtstart_line, rrule, &raw.exdates, window_start, end_exclusive)
        {
            Some(v) => starts.extend(v),
            None => {
                if walk_occurrence(raw.start_unix, window_start, end_exclusive) == WindowWalk::Keep
                    && !raw.exdates.contains(&raw.start_unix)
                {
                    starts.push(raw.start_unix);
                }
            }
        }
    } else if walk_occurrence(raw.start_unix, window_start, end_exclusive) == WindowWalk::Keep
        && !raw.exdates.contains(&raw.start_unix)
    {
        starts.push(raw.start_unix);
    }
    for rdate in &raw.rdates {
        if !raw.exdates.contains(rdate)
            && walk_occurrence(*rdate, window_start, end_exclusive) == WindowWalk::Keep
        {
            starts.push(*rdate);
        }
    }
    starts.sort_unstable();
    starts.dedup();
    Some(
        starts
            .into_iter()
            .map(|s| (s, s + duration, raw.all_day))
            .collect(),
    )
}

fn expand_rrule(
    dtstart_line: &str,
    rrule: &str,
    exdates: &[i64],
    window_start: i64,
    end_exclusive: i64,
) -> Option<Vec<i64>> {
    let blob = format!("{dtstart_line}\nRRULE:{rrule}");
    let parsed: RRuleSet = blob.parse().ok()?;
    let after = unix_to_rrule(window_start.saturating_sub(1))?;
    let before = unix_to_rrule(end_exclusive)?;
    let result = parsed.after(after).before(before).all(RRULE_SCAN_CAP);
    let mut out = Vec::new();
    for dt in result.dates {
        let ts = dt.timestamp();
        if exdates.contains(&ts) {
            continue;
        }
        match walk_occurrence(ts, window_start, end_exclusive) {
            WindowWalk::Keep => out.push(ts),
            WindowWalk::Skip => {}
            WindowWalk::Stop => break,
        }
    }
    Some(out)
}

fn unix_to_rrule(ts: i64) -> Option<DateTime<RruleTz>> {
    let utc = Utc.timestamp_opt(ts, 0).single()?;
    Some(utc.with_timezone(&RruleTz::UTC))
}

fn sanitize_uid(uid: &str, start_unix: i64, feed_id: &str) -> String {
    let cleaned: String = uid
        .chars()
        .map(|c| if c == '/' || c == '\\' { '_' } else { c })
        .collect();
    if cleaned.is_empty() {
        format!("{:x}-{start_unix}", xxh64(feed_id.as_bytes(), 0))
    } else {
        cleaned
    }
}

fn parse_ics_duration(raw: &str) -> Option<i64> {
    let s = raw.trim();
    let (neg, s) = if let Some(rest) = s.strip_prefix('-') {
        (true, rest)
    } else {
        (false, s)
    };
    let s = s.strip_prefix('P').or_else(|| s.strip_prefix('p'))?;
    let mut secs: i64 = 0;
    let mut num = String::new();
    let mut in_time = false;
    for c in s.chars() {
        if c.is_ascii_digit() {
            num.push(c);
            continue;
        }
        let n: i64 = if num.is_empty() { 0 } else { num.parse().ok()? };
        num.clear();
        match c {
            'W' | 'w' => secs += n * 7 * 86_400,
            'D' | 'd' if !in_time => secs += n * 86_400,
            'T' | 't' => in_time = true,
            'H' | 'h' => secs += n * 3_600,
            'M' | 'm' => secs += n * 60,
            'S' | 's' => secs += n,
            _ => return None,
        }
    }
    if !num.is_empty() {
        return None;
    }
    Some(if neg { -secs } else { secs })
}

fn strip_vtimezone(ics: &str) -> String {
    let mut out = String::with_capacity(ics.len());
    let mut skipping = false;
    for line in ics.lines() {
        let t = line.trim();
        if t.eq_ignore_ascii_case("BEGIN:VTIMEZONE") {
            skipping = true;
            continue;
        }
        if skipping {
            if t.eq_ignore_ascii_case("END:VTIMEZONE") {
                skipping = false;
            }
            continue;
        }
        out.push_str(line);
        out.push('\r');
        out.push('\n');
    }
    out
}

pub fn prepare_ics_text(bytes: &[u8], _content_type: &str) -> Result<String, String> {
    if fetch_url::looks_like_html(bytes) {
        return Err("URL が ICS ではありません。".into());
    }
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    let text = String::from_utf8_lossy(bytes);
    let trimmed = text.trim_start_matches('\u{feff}').trim();
    if trimmed.is_empty() {
        return Err("予定表が空です。".into());
    }
    if !trimmed.to_ascii_uppercase().contains("BEGIN:VCALENDAR") {
        return Err("URL が ICS ではありません。".into());
    }
    Ok(trimmed.to_string())
}

pub fn fetch_ics(url: &str) -> Result<String, String> {
    let parsed = Url::parse(url).map_err(|_| "iCal URL が正しくありません。".to_string())?;
    if parsed.scheme() != "https" {
        return Err("https の iCal URL を貼ってください。".into());
    }
    fetch_url::resolve_and_check(&parsed, FetchAccess::Tool)?;
    let client = Client::builder()
        .user_agent(USER_AGENT)
        .timeout(ICAL_FETCH_TIMEOUT)
        .redirect(Policy::custom(move |attempt: Attempt| {
            if attempt.previous().len() >= ICAL_MAX_REDIRECTS {
                return attempt.error(std::io::Error::new(
                    ErrorKind::PermissionDenied,
                    "リダイレクトが多すぎます。",
                ));
            }
            match fetch_url::resolve_and_check(attempt.url(), FetchAccess::Tool) {
                Ok(()) => {
                    if attempt.url().scheme() != "https" {
                        return attempt.error(std::io::Error::new(
                            ErrorKind::PermissionDenied,
                            "https の iCal URL を貼ってください。",
                        ));
                    }
                    attempt.follow()
                }
                Err(_) => attempt.error(std::io::Error::new(
                    ErrorKind::PermissionDenied,
                    "このホストは読めません。",
                )),
            }
        }))
        .build()
        .map_err(|_| "HTTP クライアントを作れません。".to_string())?;
    let resp = client
        .get(parsed)
        .header(
            reqwest::header::ACCEPT,
            "text/calendar, text/plain;q=0.9, */*;q=0.1",
        )
        .send()
        .map_err(|_| "カレンダーを取得できません。".to_string())?;
    let status = resp.status().as_u16();
    if status == 401 || status == 403 {
        return Err("非公開URLを確認してください。リセットした URL は使えません。".into());
    }
    if !(200..300).contains(&status) {
        return Err(format!("カレンダーを取得できません（HTTP {status}）。"));
    }
    if let Some(len) = resp.content_length() {
        if len > ICAL_MAX_BYTES {
            return Err("カレンダーが大きすぎます。".into());
        }
    }
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let mut limited = resp.take(ICAL_MAX_BYTES + 1);
    let mut bytes = Vec::new();
    limited
        .read_to_end(&mut bytes)
        .map_err(|_| "カレンダーを取得できません。".to_string())?;
    if bytes.len() as u64 > ICAL_MAX_BYTES {
        return Err("カレンダーが大きすぎます。".into());
    }
    prepare_ics_text(&bytes, &content_type)
}

fn progress(
    db: &Db,
    phase: &str,
    folder_label: impl Into<String>,
    current: u32,
    total: u32,
    message: impl Into<String>,
) -> CalendarSyncProgress {
    CalendarSyncProgress {
        phase: phase.into(),
        folder_label: folder_label.into(),
        current,
        total,
        message: message.into(),
        indexed_total: db.count_calendar_events().unwrap_or(0),
    }
}

/// Outlook COM stays in the STA worker. ICS fetch runs here so HTTP cannot block mail open.
pub fn sync_calendar_sources<F>(
    db: &Db,
    mail: &MailStaHandle,
    allow_launch: bool,
    on_progress: F,
) -> Result<CalendarSyncStats, String>
where
    F: Fn(CalendarSyncProgress) + Send + Sync + Clone + 'static,
{
    let settings = db.load_settings();
    if !settings.calendar_enabled {
        return Err("予定表が無効です。設定で有効にしてください。".into());
    }
    let ical_feeds = enabled_ical_feeds(&settings.calendar_ical_feeds);
    let outlook_folders = db
        .list_selected_calendar_folders()
        .map_err(|e| e.to_string())?;
    if ical_feeds.is_empty() && outlook_folders.is_empty() {
        return Err(
            "同期するものがありません。Google の iCal URL を追加するか、Outlook 予定表を選んでください。"
                .into(),
        );
    }

    let (window_start, end_exclusive) = calendar::window_bounds(
        settings.calendar_days_back,
        settings.calendar_days_ahead,
        Local::now(),
    );

    let mut stats = CalendarSyncStats::default();
    let mut any_ok = false;
    let ical_total = ical_feeds.len() as u32;

    for (i, feed) in ical_feeds.iter().enumerate() {
        let label = feed_label(feed);
        on_progress(progress(
            db,
            "ical",
            label.clone(),
            i as u32 + 1,
            ical_total.max(1),
            format!("Google 取得中: {label} ({}/{})", i + 1, ical_feeds.len()),
        ));
        match fetch_and_expand(feed, window_start, end_exclusive) {
            Ok((rows, truncated)) => {
                if let Err(e) = db.replace_calendar_folder_events(&ical_folder_id(&feed.id), &rows) {
                    eprintln!("argos: ical replace failed: {e}");
                    stats.errors += 1;
                    continue;
                }
                stats.indexed += rows.len() as u32;
                stats.truncated |= truncated;
                any_ok = true;
            }
            Err(e) => {
                eprintln!("argos: ical feed failed ({label}): {e}");
                stats.errors += 1;
            }
        }
    }

    let keep_ical: Vec<String> = ical_feeds.iter().map(|f| ical_folder_id(&f.id)).collect();
    let _ = db.delete_ical_events_except(&keep_ical);
    let _ = db.prune_outlook_events_to_selected();

    if !outlook_folders.is_empty() {
        match mail.sync_calendar(allow_launch, {
            let on_progress = on_progress.clone();
            move |p| on_progress(p)
        }) {
            Ok(o) => {
                stats.indexed += o.indexed;
                stats.errors += o.errors;
                stats.folders += o.folders;
                stats.truncated |= o.truncated;
                if o.indexed > 0 || o.errors == 0 {
                    any_ok = true;
                }
            }
            Err(e) => {
                stats.errors += 1;
                if !any_ok {
                    return Err(e);
                }
            }
        }
    }

    if !any_ok {
        return Err("予定表を取得できませんでした。".into());
    }
    let _ = db.set_calendar_last_sync_now(stats.truncated);
    on_progress(progress(
        db,
        "done",
        "",
        ical_total + stats.folders,
        ical_total + stats.folders,
        format!("完了: 予定 {} / エラー {}", stats.indexed, stats.errors),
    ));
    Ok(stats)
}

fn fetch_and_expand(
    feed: &CalendarIcalFeed,
    window_start: i64,
    end_exclusive: i64,
) -> Result<(Vec<CalendarEventRow>, bool), String> {
    let ics = fetch_ics(&feed.url)?;
    let (appts, truncated) = parse_ics_prepared(
        &ics,
        &feed.id,
        feed.name.trim(),
        window_start,
        end_exclusive,
    )?;
    let rows = appts.into_iter().map(appointment_to_row).collect();
    Ok((rows, truncated))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use crate::db::CalendarFolderRow;

    fn tokyo_ts(y: i32, m: u32, d: u32, h: u32, min: u32) -> i64 {
        chrono_tz::Asia::Tokyo
            .with_ymd_and_hms(y, m, d, h, min, 0)
            .single()
            .unwrap()
            .timestamp()
    }

    fn window_around_march_2026() -> (i64, i64) {
        (
            tokyo_ts(2026, 3, 1, 0, 0),
            tokyo_ts(2026, 3, 29, 0, 0),
        )
    }

    #[test]
    fn weekly_rrule_stays_in_window() {
        let ics = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nX-WR-CALNAME:仕事\r\nX-WR-TIMEZONE:Asia/Tokyo\r\nBEGIN:VEVENT\r\nUID:weekly-1\r\nDTSTART;TZID=Asia/Tokyo:20260302T090000\r\nDTEND;TZID=Asia/Tokyo:20260302T100000\r\nRRULE:FREQ=WEEKLY;COUNT=10\r\nSUMMARY:週次\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let (start, end) = window_around_march_2026();
        let (events, truncated) = parse_ics(ics, "feed-a", "", start, end).unwrap();
        assert!(!truncated);
        assert_eq!(events.len(), 4);
        assert!(events.iter().all(|e| e.subject == "週次"));
        assert!(events.iter().all(|e| e.calendar_name == "仕事"));
        assert_eq!(events[0].start_unix, tokyo_ts(2026, 3, 2, 9, 0));
        assert_eq!(events[3].start_unix, tokyo_ts(2026, 3, 23, 9, 0));
    }

    #[test]
    fn until_less_daily_stops_at_window() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:daily-1\r\nDTSTART:20260301T000000Z\r\nDTEND:20260301T010000Z\r\nRRULE:FREQ=DAILY\r\nSUMMARY:毎日\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let start = Utc.with_ymd_and_hms(2026, 3, 1, 0, 0, 0).single().unwrap().timestamp();
        let end = Utc.with_ymd_and_hms(2026, 3, 8, 0, 0, 0).single().unwrap().timestamp();
        let (events, _) = parse_ics(ics, "f", "", start, end).unwrap();
        assert_eq!(events.len(), 7);
    }

    #[test]
    fn all_day_exclusive_dtend() {
        let ics = "BEGIN:VCALENDAR\r\nX-WR-TIMEZONE:Asia/Tokyo\r\nBEGIN:VEVENT\r\nUID:allday-1\r\nDTSTART;VALUE=DATE:20260310\r\nDTEND;VALUE=DATE:20260311\r\nSUMMARY:終日\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let (start, end) = window_around_march_2026();
        let (events, _) = parse_ics(ics, "f", "祝日", start, end).unwrap();
        assert_eq!(events.len(), 1);
        assert!(events[0].all_day);
        assert_eq!(events[0].start_unix, tokyo_ts(2026, 3, 10, 0, 0));
        assert_eq!(events[0].end_unix, tokyo_ts(2026, 3, 11, 0, 0));
        assert_eq!(events[0].calendar_name, "祝日");
    }

    #[test]
    fn duration_sets_end() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:dur-1\r\nDTSTART:20260310T010000Z\r\nDURATION:PT2H\r\nSUMMARY:duration\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let (start, end) = window_around_march_2026();
        let (events, _) = parse_ics(ics, "f", "", start, end).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].end_unix - events[0].start_unix, 2 * 3600);
    }

    #[test]
    fn private_class_redacts_body() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:priv-1\r\nDTSTART:20260310T010000Z\r\nDTEND:20260310T020000Z\r\nCLASS:PRIVATE\r\nSUMMARY:秘密\r\nDESCRIPTION:本文は残さない\r\nLOCATION:会議室\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let (start, end) = window_around_march_2026();
        let (events, _) = parse_ics(ics, "f", "", start, end).unwrap();
        assert_eq!(events.len(), 1);
        assert!(events[0].private);
        assert!(events[0].body.is_empty());
        assert!(events[0].location.is_empty());
        assert_eq!(events[0].subject, "秘密");
    }

    #[test]
    fn cancelled_master_dropped() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:cx-1\r\nDTSTART:20260310T010000Z\r\nDTEND:20260310T020000Z\r\nSTATUS:CANCELLED\r\nSUMMARY:中止\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let (start, end) = window_around_march_2026();
        let (events, _) = parse_ics(ics, "f", "", start, end).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn cancelled_exception_skips_one() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:series-1\r\nDTSTART:20260302T000000Z\r\nDTEND:20260302T010000Z\r\nRRULE:FREQ=WEEKLY;COUNT=4\r\nSUMMARY:系列\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:series-1\r\nRECURRENCE-ID:20260309T000000Z\r\nDTSTART:20260309T000000Z\r\nDTEND:20260309T010000Z\r\nSTATUS:CANCELLED\r\nSUMMARY:系列\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let start = Utc.with_ymd_and_hms(2026, 3, 1, 0, 0, 0).single().unwrap().timestamp();
        let end = Utc.with_ymd_and_hms(2026, 3, 30, 0, 0, 0).single().unwrap().timestamp();
        let (events, _) = parse_ics(ics, "f", "", start, end).unwrap();
        assert_eq!(events.len(), 3);
        assert!(!events.iter().any(|e| e.start_unix
            == Utc.with_ymd_and_hms(2026, 3, 9, 0, 0, 0).single().unwrap().timestamp()));
    }

    #[test]
    fn two_feeds_use_distinct_folders() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:same-uid\r\nDTSTART:20260310T010000Z\r\nDTEND:20260310T020000Z\r\nSUMMARY:A\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let (start, end) = window_around_march_2026();
        let (a, _) = parse_ics(ics, "one", "甲", start, end).unwrap();
        let (b, _) = parse_ics(ics, "two", "乙", start, end).unwrap();
        assert_eq!(a[0].folder_entry_id, "ical:one");
        assert_eq!(b[0].folder_entry_id, "ical:two");
        assert_ne!(a[0].store_id, "outlook");
        assert_eq!(a[0].calendar_name, "甲");
        assert_eq!(b[0].calendar_name, "乙");
    }

    #[test]
    fn redact_hides_private_token() {
        let raw = "https://calendar.google.com/calendar/ical/x%40gmail.com/private-SECRETTOKEN/basic.ics";
        let red = redact_ical_url(raw);
        assert!(!red.contains("SECRETTOKEN"));
        assert!(red.contains("private-"));
        let err = format!("failed {raw}");
        assert!(err.contains("SECRETTOKEN"));
        assert!(!redact_ical_url(&err).contains("SECRETTOKEN"));
    }

    #[test]
    fn http_url_rejected() {
        let err = normalize_ical_url("http://calendar.google.com/calendar/ical/x/private-abc/basic.ics")
            .unwrap_err();
        assert!(err.contains("https"));
        assert!(!err.contains("private-abc"));
    }

    #[test]
    fn html_body_rejected() {
        let html = b"<!DOCTYPE html><html><body>login</body></html>";
        let err = prepare_ics_text(html, "text/html").unwrap_err();
        assert!(err.contains("ICS"));
    }

    #[test]
    fn empty_and_duplicate_feeds() {
        let a = CalendarIcalFeed {
            id: "1".into(),
            url: "https://calendar.google.com/calendar/ical/a/private-aaa/basic.ics".into(),
            name: "A".into(),
            enabled: true,
        };
        let dup = CalendarIcalFeed {
            id: "2".into(),
            url: a.url.clone(),
            name: "B".into(),
            enabled: true,
        };
        let empty = CalendarIcalFeed {
            id: "3".into(),
            url: "  ".into(),
            name: "".into(),
            enabled: true,
        };
        let webcal = CalendarIcalFeed {
            id: "4".into(),
            url: "webcal://calendar.google.com/calendar/ical/b/private-bbb/basic.ics".into(),
            name: "".into(),
            enabled: true,
        };
        let out = normalize_ical_feeds(vec![empty, a.clone(), webcal]).unwrap();
        assert_eq!(out.len(), 2);
        assert!(out[1].url.starts_with("https://"));
        let err = normalize_ical_feeds(vec![a, dup]).unwrap_err();
        assert!(err.contains("重複"));
        let too_many: Vec<_> = (0..9)
            .map(|i| CalendarIcalFeed {
                id: format!("{i}"),
                url: format!("https://calendar.google.com/calendar/ical/{i}/private-x/basic.ics"),
                name: String::new(),
                enabled: true,
            })
            .collect();
        assert!(normalize_ical_feeds(too_many).unwrap_err().contains("8件"));
    }

    #[test]
    fn ical_rows_survive_outlook_prune() {
        let dir = std::env::temp_dir().join(format!(
            "argos-ical-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("argos.db")).unwrap();
        let ical = CalendarEventRow {
            path: "ical:f1/uid/1".into(),
            folder_entry_id: "ical:f1".into(),
            store_id: ICAL_STORE_ID.into(),
            entry_id: "uid".into(),
            start_unix: 1,
            end_unix: 2,
            all_day: false,
            subject: "gcal".into(),
            location: String::new(),
            organizer: String::new(),
            attendees: String::new(),
            categories: String::new(),
            body: String::new(),
            calendar_name: "仕事".into(),
            busy_status: 2,
            private: false,
        };
        let outlook = CalendarEventRow {
            path: "outlookcal:s/A/keep".into(),
            folder_entry_id: "A".into(),
            store_id: "s".into(),
            entry_id: "keep".into(),
            start_unix: 1,
            end_unix: 2,
            all_day: false,
            subject: "keep".into(),
            location: String::new(),
            organizer: String::new(),
            attendees: String::new(),
            categories: String::new(),
            body: String::new(),
            calendar_name: "A".into(),
            busy_status: 2,
            private: false,
        };
        db.replace_calendar_folder_events("ical:f1", &[ical]).unwrap();
        db.replace_calendar_folder_events("A", &[outlook.clone()]).unwrap();
        db.replace_calendar_folder_events(
            "C",
            &[CalendarEventRow {
                folder_entry_id: "C".into(),
                path: "outlookcal:s/C/drop".into(),
                entry_id: "drop".into(),
                subject: "drop".into(),
                ..outlook
            }],
        )
        .unwrap();
        db.delete_calendar_events_except(&["A".into()]).unwrap();
        let subjects: Vec<String> = db
            .list_calendar_events()
            .unwrap()
            .into_iter()
            .map(|e| e.subject)
            .collect();
        assert!(subjects.contains(&"gcal".into()));
        assert!(subjects.contains(&"keep".into()));
        assert!(!subjects.iter().any(|s| s == "drop"));

        db.delete_ical_events_except(&[]).unwrap();
        let subjects: Vec<String> = db
            .list_calendar_events()
            .unwrap()
            .into_iter()
            .map(|e| e.subject)
            .collect();
        assert!(!subjects.contains(&"gcal".into()));
        assert!(subjects.contains(&"keep".into()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn failed_fetch_keeps_existing_row() {
        let dir = std::env::temp_dir().join(format!(
            "argos-ical-keep-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("argos.db")).unwrap();
        let row = CalendarEventRow {
            path: "ical:keepme/uid/1".into(),
            folder_entry_id: "ical:keepme".into(),
            store_id: ICAL_STORE_ID.into(),
            entry_id: "uid".into(),
            start_unix: 1,
            end_unix: 2,
            all_day: false,
            subject: "stale".into(),
            location: String::new(),
            organizer: String::new(),
            attendees: String::new(),
            categories: String::new(),
            body: String::new(),
            calendar_name: "仕事".into(),
            busy_status: 2,
            private: false,
        };
        db.replace_calendar_folder_events("ical:keepme", &[row]).unwrap();
        db.delete_ical_events_except(&["ical:keepme".into()]).unwrap();
        assert_eq!(db.list_calendar_events().unwrap()[0].subject, "stale");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn sample_row(folder: &str, store: &str, subject: &str) -> CalendarEventRow {
        CalendarEventRow {
            path: format!("{store}:{folder}/{subject}"),
            folder_entry_id: folder.into(),
            store_id: store.into(),
            entry_id: subject.into(),
            start_unix: 1,
            end_unix: 2,
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
        }
    }

    fn sample_folder(entry: &str, _selected: bool) -> CalendarFolderRow {
        CalendarFolderRow {
            id: 0,
            store_id: "s".into(),
            entry_id: entry.into(),
            name: entry.into(),
            path_label: entry.into(),
            selected: false,
            is_default: false,
            item_count: 0,
            event_count: 0,
        }
    }

    fn open_tmp(prefix: &str) -> (std::path::PathBuf, Db) {
        let dir = std::env::temp_dir().join(format!(
            "{prefix}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("argos.db")).unwrap();
        (dir, db)
    }

    #[test]
    fn unchecking_all_outlook_folders_drops_outlook_keeps_ical() {
        let (dir, db) = open_tmp("argos-uncheck-outlook");
        db.replace_calendar_folder_catalog(&[sample_folder("A", true), sample_folder("B", true)])
            .unwrap();
        db.set_calendar_folders_selected(&[("s".into(), "A".into())])
            .unwrap();
        db.replace_calendar_folder_events("ical:f1", &[sample_row("ical:f1", ICAL_STORE_ID, "gcal")])
            .unwrap();
        db.replace_calendar_folder_events("A", &[sample_row("A", "s", "via-ol")])
            .unwrap();
        db.set_calendar_folders_selected(&[]).unwrap();
        let subjects: Vec<String> = db
            .list_calendar_events()
            .unwrap()
            .into_iter()
            .map(|e| e.subject)
            .collect();
        assert_eq!(subjects, vec!["gcal".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prune_to_selected_keeps_checked_outlook_and_ical() {
        let (dir, db) = open_tmp("argos-prune-selected");
        db.replace_calendar_folder_catalog(&[sample_folder("A", true), sample_folder("B", false)])
            .unwrap();
        db.set_calendar_folders_selected(&[("s".into(), "A".into())])
            .unwrap();
        db.replace_calendar_folder_events("ical:f1", &[sample_row("ical:f1", ICAL_STORE_ID, "gcal")])
            .unwrap();
        db.replace_calendar_folder_events("A", &[sample_row("A", "s", "keep-ol")])
            .unwrap();
        db.replace_calendar_folder_events("B", &[sample_row("B", "s", "drop-ol")])
            .unwrap();
        db.prune_outlook_events_to_selected().unwrap();
        let subjects: Vec<String> = db
            .list_calendar_events()
            .unwrap()
            .into_iter()
            .map(|e| e.subject)
            .collect();
        assert!(subjects.contains(&"gcal".into()));
        assert!(subjects.contains(&"keep-ol".into()));
        assert!(!subjects.iter().any(|s| s == "drop-ol"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prune_without_catalog_leaves_outlook_rows() {
        let (dir, db) = open_tmp("argos-prune-nocatalog");
        db.replace_calendar_folder_events("A", &[sample_row("A", "s", "keep")])
            .unwrap();
        db.prune_outlook_events_to_selected().unwrap();
        assert_eq!(db.list_calendar_events().unwrap()[0].subject, "keep");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
