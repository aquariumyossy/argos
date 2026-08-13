//! Search-term history extraction and suggestion ranking.

use serde::{Deserialize, Serialize};

use crate::db::SearchTermHistory;

use super::tantivy_backend::parse_query_syntax;

const SUGGEST_LIMIT: usize = 8;
/// Recency half-life in seconds (~7 days).
const RECENCY_HALF_LIFE_SECS: f64 = 7.0 * 24.0 * 3600.0;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchTermSuggestion {
    /// Full term to apply (replace last token or append).
    pub term: String,
    /// Matched prefix of `term` (may be empty for co-occurrence).
    pub display_prefix: String,
    /// Remainder after prefix (highlighted in UI).
    pub display_rest: String,
    /// `"prefix"` or `"cooccur"`.
    pub kind: String,
    pub score: f32,
    /// Present in search-term history.
    #[serde(default)]
    pub from_history: bool,
    /// Present in registered search words (dictionary).
    #[serde(default)]
    pub from_registered: bool,
}

/// Extract history terms from a raw query (phrases kept whole; includes via morph).
pub fn extract_search_terms<F>(query: &str, mut content_surfaces: F) -> Result<Vec<String>, String>
where
    F: FnMut(&str) -> Result<Vec<String>, String>,
{
    let parsed = parse_query_syntax(query);
    let mut out: Vec<String> = Vec::new();
    let mut push = |s: String| {
        let t = s.trim();
        if t.is_empty() {
            return;
        }
        if out.iter().any(|x| x == t) {
            return;
        }
        out.push(t.to_string());
    };

    for phrase in &parsed.phrases {
        push(phrase.clone());
    }

    for inc in &parsed.includes {
        let surfaces = content_surfaces(inc)?;
        for s in surfaces {
            push(s);
        }
        let trimmed = inc.trim().to_string();
        // Keep the raw include surface for compound prefix completion (損害賠償).
        if trimmed.chars().count() >= 2 {
            push(trimmed);
        }
    }

    Ok(out)
}

fn is_query_delimiter(c: char) -> bool {
    matches!(
        c,
        ' ' | '\t' | '\n' | '\r' | '\u{3000}' | ',' | '\u{FF0C}' | '\u{3001}'
    )
}

/// Last whitespace/comma-separated token. Empty if inside an unclosed `"phrase`.
pub fn last_query_token(query: &str) -> Option<String> {
    let chars: Vec<char> = query.chars().collect();
    let mut i = 0usize;
    let mut in_quote = false;
    let mut last_start = None::<usize>;
    let mut last_end = None::<usize>;

    while i < chars.len() {
        if !in_quote && is_query_delimiter(chars[i]) {
            i += 1;
            continue;
        }
        if chars[i] == '"' {
            in_quote = !in_quote;
            i += 1;
            if in_quote {
                last_start = Some(i.saturating_sub(1));
                last_end = Some(i);
            }
            continue;
        }
        if in_quote {
            last_end = Some(i + 1);
            i += 1;
            continue;
        }
        // optional leading `-` on a free term — still part of token for exclusion typing
        let start = i;
        while i < chars.len() && !is_query_delimiter(chars[i]) && chars[i] != '"' {
            i += 1;
        }
        last_start = Some(start);
        last_end = Some(i);
    }

    if in_quote {
        // Unclosed phrase: suppress suggestions.
        return None;
    }
    let (Some(s), Some(e)) = (last_start, last_end) else {
        return Some(String::new());
    };
    if s >= e {
        return Some(String::new());
    }
    let raw: String = chars[s..e].iter().collect();
    // Strip a single leading `-` for matching history (excludes aren't suggested anyway).
    let token = raw.strip_prefix('-').unwrap_or(&raw).trim();
    // Strip wrapping quotes if somehow complete
    let token = token.trim_matches('"');
    Some(token.to_string())
}

/// Tokens already fully present in the query (for duplicate / co-occur anchors).
pub fn committed_query_terms(query: &str) -> Vec<String> {
    let parsed = parse_query_syntax(query);
    let mut out = Vec::new();
    for p in parsed.phrases {
        let t = p.trim();
        if !t.is_empty() && !out.iter().any(|x| x == t) {
            out.push(t.to_string());
        }
    }
    for inc in parsed.includes {
        let t = inc.trim();
        if !t.is_empty() && !out.iter().any(|x| x == t) {
            out.push(t.to_string());
        }
    }
    out
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn recency_score(last: i64, now: i64) -> f32 {
    if last <= 0 {
        return 0.0;
    }
    let age = (now - last).max(0) as f64;
    let score = (-age / RECENCY_HALF_LIFE_SECS * std::f64::consts::LN_2).exp();
    score as f32
}

fn base_score(count: u32, last: i64, now: i64) -> f32 {
    2.0 * (1.0 + count as f32).ln() + recency_score(last, now)
}

/// Ranked suggestions from history + registered words for the current query.
///
/// Ranking uses frequency + recency from history when available. Registered-only
/// terms get a modest base score (count=1, no recency), so frequently / recently
/// used history terms naturally rank above unused dictionary words. Terms in both
/// sources keep the history score and are flagged with both badges.
pub fn suggest_from_history(
    history: &SearchTermHistory,
    registered: &[String],
    query: &str,
) -> Vec<SearchTermSuggestion> {
    let Some(token) = last_query_token(query) else {
        return Vec::new();
    };
    if token.is_empty() && query.trim().is_empty() {
        return recent_history_suggestions(history, registered);
    }
    let committed = committed_query_terms(query);
    let now = now_secs();
    let mut scored: Vec<SearchTermSuggestion> = Vec::new();

    let registered_set: std::collections::HashSet<&str> = registered
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();

    let already = |term: &str| {
        committed.iter().any(|c| c == term) || (!token.is_empty() && term == token.as_str())
    };

    let push_or_merge = |scored: &mut Vec<SearchTermSuggestion>, sug: SearchTermSuggestion| {
        if let Some(existing) = scored.iter_mut().find(|s| s.term == sug.term) {
            if sug.score > existing.score {
                existing.score = sug.score;
                if existing.kind != "prefix" || sug.kind == "prefix" {
                    existing.kind = sug.kind.clone();
                    existing.display_prefix = sug.display_prefix.clone();
                    existing.display_rest = sug.display_rest.clone();
                }
            }
            existing.from_history |= sug.from_history;
            existing.from_registered |= sug.from_registered;
        } else {
            scored.push(sug);
        }
    };

    // A: prefix completions (history stats + registered words)
    if !token.is_empty() {
        let mut prefix_terms: std::collections::HashSet<String> = std::collections::HashSet::new();
        for term in history.stats.keys() {
            if term.starts_with(&token) && term.len() > token.len() {
                prefix_terms.insert(term.clone());
            }
        }
        for term in &registered_set {
            if term.starts_with(&token) && term.len() > token.len() {
                prefix_terms.insert((*term).to_string());
            }
        }

        for term in prefix_terms {
            if already(&term) {
                continue;
            }
            let rest = term[token.len()..].to_string();
            if rest.is_empty() {
                continue;
            }
            let from_history = history.stats.contains_key(&term);
            let from_registered = registered_set.contains(term.as_str());
            let prefix_bonus = (token.chars().count() as f32 * 0.05).min(0.5);
            let score = if let Some(st) = history.stats.get(&term) {
                base_score(st.count, st.last, now) + prefix_bonus
            } else {
                // Registered-only: modest base, no recency → history usually wins.
                base_score(1, 0, now) + prefix_bonus
            };
            push_or_merge(
                &mut scored,
                SearchTermSuggestion {
                    term,
                    display_prefix: token.clone(),
                    display_rest: rest,
                    kind: "prefix".into(),
                    score,
                    from_history,
                    from_registered,
                },
            );
        }
    }

    // B: co-occurrence from recent events
    let anchors: Vec<String> = {
        let mut a = committed.clone();
        if !token.is_empty() && !a.iter().any(|x| x == &token) {
            a.push(token.clone());
        }
        a
    };

    if !anchors.is_empty() {
        for (event_idx, event) in history.events.iter().enumerate() {
            let event_has_anchor = event.terms.iter().any(|t| {
                anchors.iter().any(|a| {
                    t == a
                        || (!a.is_empty() && t.starts_with(a.as_str()) && t.as_str() != a.as_str())
                })
            });
            if !event_has_anchor {
                continue;
            }
            let recency_event = 1.0 / (1.0 + event_idx as f32);
            for c in &event.terms {
                if already(c) {
                    continue;
                }
                if !token.is_empty() && c.starts_with(&token) && c.len() > token.len() {
                    continue;
                }
                if !token.is_empty() {
                    let linked_via_other = event.terms.iter().any(|t| {
                        committed.iter().any(|c0| c0 == t)
                            || (t.starts_with(&token) && t.as_str() != token.as_str())
                    });
                    if !linked_via_other
                        && !committed
                            .iter()
                            .any(|c0| event.terms.iter().any(|t| t == c0))
                    {
                        let token_prefixes_event = event
                            .terms
                            .iter()
                            .any(|t| t.starts_with(&token) && t.as_str() != token);
                        if !token_prefixes_event {
                            continue;
                        }
                    }
                }

                let st = history.stats.get(c);
                let count = st.map(|s| s.count).unwrap_or(1);
                let last = st.map(|s| s.last).unwrap_or(event.t);
                let score = base_score(count, last, now) + 1.0 * recency_event;
                let from_registered = registered_set.contains(c.as_str());
                push_or_merge(
                    &mut scored,
                    SearchTermSuggestion {
                        term: c.clone(),
                        display_prefix: String::new(),
                        display_rest: c.clone(),
                        kind: "cooccur".into(),
                        score,
                        from_history: true,
                        from_registered,
                    },
                );
            }
        }
    }

    scored.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.term.cmp(&b.term))
    });
    scored.dedup_by(|a, b| a.term == b.term);
    scored.truncate(SUGGEST_LIMIT);
    scored
}

/// Newest-first unique terms for an empty query (address-bar style).
fn recent_history_suggestions(
    history: &SearchTermHistory,
    registered: &[String],
) -> Vec<SearchTermSuggestion> {
    let registered_set: std::collections::HashSet<&str> = registered
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    let mut out: Vec<SearchTermSuggestion> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for event in &history.events {
        for term in &event.terms {
            let t = term.trim();
            if t.is_empty() || !seen.insert(t.to_string()) {
                continue;
            }
            out.push(SearchTermSuggestion {
                term: t.to_string(),
                display_prefix: String::new(),
                display_rest: t.to_string(),
                kind: "recent".into(),
                score: (SUGGEST_LIMIT - out.len()) as f32,
                from_history: true,
                from_registered: registered_set.contains(t),
            });
            if out.len() >= SUGGEST_LIMIT {
                return out;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{SearchTermEvent, SearchTermStat};
    use std::collections::HashMap;

    #[test]
    fn last_token_basic() {
        assert_eq!(last_query_token("契約 損害").as_deref(), Some("損害"));
        assert_eq!(last_query_token("損害").as_deref(), Some("損害"));
        assert_eq!(last_query_token("").as_deref(), Some(""));
        assert_eq!(last_query_token("契約 \""), None);
    }

    #[test]
    fn prefix_suggests_compound() {
        let mut stats = HashMap::new();
        stats.insert(
            "損害賠償".into(),
            SearchTermStat {
                count: 3,
                last: now_secs(),
            },
        );
        let history = SearchTermHistory {
            events: vec![SearchTermEvent {
                terms: vec!["損害賠償".into()],
                t: now_secs(),
            }],
            stats,
        };
        let sug = suggest_from_history(&history, &[], "損害");
        assert!(
            sug.iter()
                .any(|s| s.term == "損害賠償" && s.kind == "prefix"),
            "{sug:?}"
        );
        let hit = sug.iter().find(|s| s.term == "損害賠償").unwrap();
        assert_eq!(hit.display_rest, "賠償");
        assert!(hit.from_history);
    }

    #[test]
    fn registered_prefix_mixed() {
        let history = SearchTermHistory::default();
        let sug = suggest_from_history(&history, &["損害賠償".into()], "損害");
        assert!(
            sug.iter()
                .any(|s| s.term == "損害賠償" && s.from_registered && !s.from_history),
            "{sug:?}"
        );
    }

    #[test]
    fn empty_query_suggests_recent_history() {
        let now = now_secs();
        let mut stats = HashMap::new();
        stats.insert(
            "後の語".into(),
            SearchTermStat {
                count: 1,
                last: now,
            },
        );
        stats.insert(
            "先の語".into(),
            SearchTermStat {
                count: 1,
                last: now - 10,
            },
        );
        let history = SearchTermHistory {
            events: vec![
                SearchTermEvent {
                    terms: vec!["後の語".into()],
                    t: now,
                },
                SearchTermEvent {
                    terms: vec!["先の語".into(), "後の語".into()],
                    t: now - 10,
                },
            ],
            stats,
        };
        let sug = suggest_from_history(&history, &["後の語".into()], "");
        assert_eq!(
            sug.iter().map(|s| s.term.as_str()).collect::<Vec<_>>(),
            vec!["後の語", "先の語"]
        );
        assert_eq!(sug[0].kind, "recent");
        assert!(sug[0].from_history && sug[0].from_registered);
        assert!(suggest_from_history(&SearchTermHistory::default(), &[], "").is_empty());
    }
}
