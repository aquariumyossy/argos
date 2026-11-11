//! Decide whether a saved statute is still the current revision.
//!
//! Dates come from the e-Gov update list (`EnforcementDate`, otherwise
//! `AmendPromulgationDate`). A walk that does not reach back to the saved
//! 時点 probes the article text instead of assuming it is current.

use chrono::{Duration, NaiveDate};

/// How many update-list days one lookup will download. Older gaps are probed
/// per article so a long offline stretch does not issue hundreds of requests.
pub const UPDATE_WALK_DAYS: i64 = 14;

pub fn parse_ymd(raw: &str) -> Option<NaiveDate> {
    let digits: String = raw.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() != 8 {
        return None;
    }
    let year: i32 = digits[0..4].parse().ok()?;
    let month: u32 = digits[4..6].parse().ok()?;
    let day: u32 = digits[6..8].parse().ok()?;
    // 21171231 shows up as a placeholder for an unenforced future date.
    if !(1868..=2099).contains(&year) {
        return None;
    }
    NaiveDate::from_ymd_opt(year, month, day)
}

pub fn format_ymd(date: NaiveDate) -> String {
    date.format("%Y-%m-%d").to_string()
}

pub fn compact_ymd(date: NaiveDate) -> String {
    date.format("%Y%m%d").to_string()
}

/// `remote` is newer than the text we stored. Missing or unparseable remote
/// dates are not newer. A known remote date replaces an empty stored date.
pub fn revision_is_newer(remote: &str, stored: &str) -> bool {
    match (parse_ymd(remote), parse_ymd(stored)) {
        (Some(remote), Some(stored)) => remote > stored,
        (Some(_), None) => true,
        _ => false,
    }
}

/// True when every day after `stored_as_of` through `today` was downloaded.
pub fn updates_cover(
    stored_as_of: &str,
    today: &str,
    walked_from: Option<&str>,
    walked_through: Option<&str>,
    day_count: i64,
) -> bool {
    let Some(today) = parse_ymd(today) else {
        return false;
    };
    let Some(stored) = parse_ymd(stored_as_of) else {
        return false;
    };
    if stored >= today {
        return true;
    }
    let (Some(from), Some(through)) = (
        walked_from.and_then(parse_ymd),
        walked_through.and_then(parse_ymd),
    ) else {
        return false;
    };
    if through < today || from > through {
        return false;
    }
    let span = through.signed_duration_since(from).num_days() + 1;
    if day_count < span {
        return false;
    }
    let start = stored.succ_opt().unwrap_or(stored);
    from <= start
}

/// First day to request, and whether this call can finish the walk.
pub fn walk_start(updates_through: &str, today: &str) -> Option<NaiveDate> {
    let today = parse_ymd(today)?;
    let next = match parse_ymd(updates_through) {
        Some(done) if done >= today => return None,
        Some(done) => done.succ_opt().unwrap_or(done),
        None => today - Duration::days(UPDATE_WALK_DAYS - 1),
    };
    if next > today {
        None
    } else {
        Some(next)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RevisionAction {
    /// Use the saved text. `offline` means a refresh was not attempted.
    Keep { offline: bool },
    /// Download this article and compare it with the saved body.
    ProbeArticle,
    /// The update list names a newer revision and more than one article is saved.
    ReloadLaw { as_of: String },
}

pub struct RevisionInput<'a> {
    pub stored_as_of: &'a str,
    pub today: &'a str,
    pub remote_as_of: Option<&'a str>,
    pub covered: bool,
    pub offline: bool,
    pub cached_articles: usize,
    pub probed_on: &'a str,
    /// A saved amendment whose enforcement date has arrived.
    pub pending_due: bool,
}

pub fn revision_action(input: &RevisionInput<'_>) -> RevisionAction {
    if input.offline {
        return RevisionAction::Keep { offline: true };
    }
    // A future date is not the text we stored. Recheck once it can be in force.
    if let (Some(stored), Some(today)) = (parse_ymd(input.stored_as_of), parse_ymd(input.today)) {
        if stored > today {
            return reload_or_probe(input.cached_articles, input.today);
        }
    }
    if input.pending_due {
        return reload_or_probe(input.cached_articles, input.today);
    }
    if let Some(remote) = input.remote_as_of {
        if let (Some(remote_date), Some(today)) = (parse_ymd(remote), parse_ymd(input.today)) {
            if remote_date > today {
                // Not in force yet. Do not treat the law as updated.
            } else if revision_is_newer(remote, input.stored_as_of) {
                if input.cached_articles > 1 {
                    return RevisionAction::ReloadLaw {
                        as_of: format_ymd(remote_date),
                    };
                }
                return RevisionAction::ProbeArticle;
            }
        } else if revision_is_newer(remote, input.stored_as_of) {
            if input.cached_articles > 1 {
                if let Some(date) = parse_ymd(remote) {
                    return RevisionAction::ReloadLaw {
                        as_of: format_ymd(date),
                    };
                }
            }
            return RevisionAction::ProbeArticle;
        }
    }
    if !input.covered && input.probed_on != input.today {
        return RevisionAction::ProbeArticle;
    }
    RevisionAction::Keep { offline: false }
}

fn reload_or_probe(cached_articles: usize, today: &str) -> RevisionAction {
    if cached_articles > 1 {
        RevisionAction::ReloadLaw {
            as_of: today.to_string(),
        }
    } else {
        RevisionAction::ProbeArticle
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_compact_and_dashed_dates_and_rejects_placeholders() {
        assert_eq!(parse_ymd("20260401").unwrap().to_string(), "2026-04-01");
        assert_eq!(parse_ymd("2026-04-01").unwrap().to_string(), "2026-04-01");
        assert!(parse_ymd("").is_none());
        assert!(parse_ymd("21171231").is_none());
        assert!(parse_ymd("18960427").is_some());
    }

    #[test]
    fn newer_revision_compares_dates() {
        assert!(revision_is_newer("2026-04-01", "2024-04-01"));
        assert!(!revision_is_newer("2020-01-01", "2024-04-01"));
        assert!(!revision_is_newer("2024-04-01", "2024-04-01"));
        assert!(!revision_is_newer("", "2024-04-01"));
        assert!(revision_is_newer("20260401", ""));
        assert!(!revision_is_newer("not-a-date", "2024-04-01"));
    }

    #[test]
    fn coverage_requires_every_day_since_the_saved_date() {
        assert!(updates_cover(
            "2026-11-09",
            "2026-11-11",
            Some("2026-11-10"),
            Some("2026-11-11"),
            2
        ));
        assert!(updates_cover("2026-11-11", "2026-11-11", None, None, 0));
        assert!(!updates_cover(
            "2026-10-01",
            "2026-11-11",
            Some("2026-10-29"),
            Some("2026-11-11"),
            14
        ));
        assert!(!updates_cover(
            "2026-11-09",
            "2026-11-11",
            Some("2026-11-10"),
            Some("2026-11-11"),
            1
        ));
    }

    #[test]
    fn walk_start_is_the_next_missing_day_and_caps_a_cold_start() {
        assert_eq!(walk_start("2026-11-11", "2026-11-11"), None);
        assert_eq!(
            walk_start("2026-11-10", "2026-11-11").unwrap().to_string(),
            "2026-11-11"
        );
        assert_eq!(
            walk_start("", "2026-11-11").unwrap().to_string(),
            "2026-10-29"
        );
    }

    fn input<'a>(
        stored: &'a str,
        remote: Option<&'a str>,
        covered: bool,
        offline: bool,
        count: usize,
        probed: &'a str,
    ) -> RevisionInput<'a> {
        RevisionInput {
            stored_as_of: stored,
            today: "2026-11-11",
            remote_as_of: remote,
            covered,
            offline,
            cached_articles: count,
            probed_on: probed,
            pending_due: false,
        }
    }

    #[test]
    fn refresh_follows_the_revision_date_and_fails_soft_offline() {
        assert_eq!(
            revision_action(&input(
                "2024-04-01",
                Some("2026-04-01"),
                true,
                false,
                40,
                ""
            )),
            RevisionAction::ReloadLaw {
                as_of: "2026-04-01".into()
            }
        );
        assert_eq!(
            revision_action(&input("2024-04-01", Some("2026-04-01"), true, false, 1, "")),
            RevisionAction::ProbeArticle
        );
        assert_eq!(
            revision_action(&input(
                "2026-04-01",
                Some("2020-01-01"),
                true,
                false,
                40,
                ""
            )),
            RevisionAction::Keep { offline: false }
        );
        assert_eq!(
            revision_action(&input("2024-04-01", None, true, true, 40, "")),
            RevisionAction::Keep { offline: true }
        );
        assert_eq!(
            revision_action(&input("2024-04-01", None, false, false, 40, "")),
            RevisionAction::ProbeArticle
        );
        assert_eq!(
            revision_action(&input("2024-04-01", None, false, false, 40, "2026-11-11")),
            RevisionAction::Keep { offline: false }
        );
    }

    #[test]
    fn a_future_enforcement_date_is_not_treated_as_current_text() {
        assert_eq!(
            revision_action(&input(
                "2020-04-01",
                Some("2027-04-01"),
                true,
                false,
                40,
                ""
            )),
            RevisionAction::Keep { offline: false }
        );
        let mut due = input("2020-04-01", None, true, false, 40, "2026-11-11");
        due.pending_due = true;
        assert_eq!(
            revision_action(&due),
            RevisionAction::ReloadLaw {
                as_of: "2026-11-11".into()
            }
        );
        assert_eq!(
            revision_action(&input(
                "2027-04-01",
                None,
                true,
                false,
                40,
                "2026-11-11"
            )),
            RevisionAction::ReloadLaw {
                as_of: "2026-11-11".into()
            }
        );
    }
}
