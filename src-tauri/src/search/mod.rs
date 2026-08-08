use serde::{Deserialize, Serialize};

pub mod history;
pub mod morph;
pub mod remote_backend;
pub mod tantivy_backend;

pub use history::{extract_search_terms, suggest_from_history, SearchTermSuggestion};
pub use morph::{apply_user_dictionary, is_noise_highlight_term, MorphAnalyzer, UserDictMatcher};
pub use remote_backend::{hybrid_search, RemoteArgosBackend};
pub use tantivy_backend::{parse_query_syntax, TantivyBackend};

use crate::db::Settings;
use crate::pathutil;

/// Normalized search hit shared by all backends (Tantivy / remote Argos).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    pub id: String,
    pub title: String,
    pub snippet: String,
    pub path: String,
    pub page: Option<u32>,
    pub chunk_id: Option<u32>,
    pub score: f32,
    pub source: String,
    pub preview_text: String,
    /// Morphological / matched terms to highlight in UI.
    pub highlight_terms: Vec<String>,
    /// Number of matching paragraph units in this file (list aggregation).
    #[serde(default)]
    pub match_count: u32,
    /// Nested matching paragraphs (top N for list UI). Empty when unknown/compat.
    #[serde(default)]
    pub paragraphs: Vec<ParagraphHit>,
    /// Best-effort unit label for the primary (best) paragraph.
    #[serde(default)]
    pub unit_label: String,
    /// Email sender display (empty for files).
    #[serde(default)]
    pub mail_from: String,
    /// Email date as unix seconds string (empty for files).
    #[serde(default)]
    pub mail_date: String,
    /// Outlook ConversationID (empty when unknown / files).
    #[serde(default)]
    pub mail_conversation_id: String,
    /// Outlook folder display name.
    #[serde(default)]
    pub mail_folder: String,
    /// `file` | `email` (empty treated as file for older hits).
    #[serde(default)]
    pub doc_kind: String,
}

/// One matching paragraph nested under a file hit.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParagraphHit {
    pub id: String,
    pub label: String,
    pub snippet: String,
    pub score: f32,
    pub page: Option<u32>,
}

pub trait SearchBackend: Send + Sync {
    fn search(
        &self,
        query: &str,
        limit: usize,
        path_prefix: Option<&str>,
        exts: Option<&[String]>,
        pos_filter_enabled: bool,
    ) -> Result<Vec<SearchHit>, String>;
    fn preview(&self, hit_id: &str) -> Result<Option<SearchHit>, String>;
}

/// Keep hits whose path is under `path_prefix` (Windows-aware). Empty/None = no filter.
/// Outlook virtual paths are never matched by filesystem prefixes.
pub fn filter_hits_by_path_prefix(
    hits: Vec<SearchHit>,
    path_prefix: Option<&str>,
) -> Vec<SearchHit> {
    let Some(prefix) = path_prefix.map(str::trim).filter(|s| !s.is_empty()) else {
        return hits;
    };
    // Mail-folder scope uses a special prefix.
    if let Some(folder) = prefix.strip_prefix("mailfolder:") {
        let folder = folder.trim();
        return hits
            .into_iter()
            .filter(|h| h.doc_kind == "email" && h.mail_folder.eq_ignore_ascii_case(folder))
            .collect();
    }
    hits.into_iter()
        .filter(|h| {
            if crate::mail::is_outlook_path(&h.path) {
                return false;
            }
            pathutil::path_starts_with(&h.path, prefix)
        })
        .collect()
}

/// Drop email documents (for LAN remote responses).
pub fn filter_out_email_hits(hits: Vec<SearchHit>) -> Vec<SearchHit> {
    hits.into_iter()
        .filter(|h| h.doc_kind != "email" && !crate::mail::is_outlook_path(&h.path))
        .collect()
}

/// Collapse email hits that share a conversation id, keeping the newest by mail_date.
pub fn collapse_email_threads(hits: Vec<SearchHit>) -> Vec<SearchHit> {
    let mut out: Vec<SearchHit> = Vec::new();
    let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for hit in hits {
        if hit.doc_kind != "email" || hit.mail_conversation_id.is_empty() {
            out.push(hit);
            continue;
        }
        let key = hit.mail_conversation_id.clone();
        if let Some(&idx) = seen.get(&key) {
            let old_date: i64 = out[idx].mail_date.parse().unwrap_or(0);
            let new_date: i64 = hit.mail_date.parse().unwrap_or(0);
            if new_date >= old_date {
                let mut merged = hit;
                merged.match_count = out[idx].match_count.saturating_add(merged.match_count.max(1));
                out[idx] = merged;
            } else {
                out[idx].match_count = out[idx].match_count.saturating_add(1);
            }
        } else {
            seen.insert(key, out.len());
            out.push(hit);
        }
    }
    out
}

/// Extension from a file path (lowercase, no leading dot). Empty if none.
pub fn path_extension(path: &str) -> String {
    let base = path.replace('\\', "/");
    let file = base.rsplit('/').next().unwrap_or("");
    let Some(i) = file.rfind('.') else {
        return String::new();
    };
    if i == 0 || i + 1 >= file.len() {
        return String::new();
    }
    file[i + 1..].to_lowercase()
}

/// Keep hits whose path extension is in `exts` (lowercase). Empty/None = no filter.
pub fn filter_hits_by_exts(hits: Vec<SearchHit>, exts: Option<&[String]>) -> Vec<SearchHit> {
    let Some(list) = exts.filter(|e| !e.is_empty()) else {
        return hits;
    };
    hits.into_iter()
        .filter(|h| {
            let ext = path_extension(&h.path);
            list.iter().any(|e| e == &ext)
        })
        .collect()
}

/// Trim, strip leading dots, lowercase, drop empties. None if nothing left.
pub fn normalize_exts(exts: Option<Vec<String>>) -> Option<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for raw in exts.unwrap_or_default() {
        let e = raw.trim().trim_start_matches('.').to_lowercase();
        if e.is_empty() {
            continue;
        }
        if !out.iter().any(|x| x == &e) {
            out.push(e);
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Apply client user-dictionary quoting, then route by `search_mode`.
///
/// Dictionary rewrite runs on the client so remote hosts receive `"phrase"` syntax
/// without needing the client's word list. POS filtering uses each side's local index
/// tokenizer (host settings for remote hits).
///
/// When `mail` is provided and mail indexing is enabled, local/hybrid searches may
/// also query the dedicated mail index (never exposed on remote-only mode).
pub fn run_search(
    settings: &Settings,
    local: &TantivyBackend,
    mail: Option<&TantivyBackend>,
    query: &str,
    limit: usize,
    path_prefix: Option<&str>,
    exts: Option<&[String]>,
    user_dict: &UserDictMatcher,
) -> Result<Vec<SearchHit>, String> {
    let rewritten = apply_user_dictionary(query, user_dict);
    let pos_filter = settings.pos_filter_enabled;
    match settings.search_mode.as_str() {
        "remote" => {
            // Mail folder scopes never leave this machine.
            if path_prefix
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .is_some_and(|p| p.starts_with("mailfolder:"))
            {
                return search_local_with_mail(
                    settings,
                    local,
                    mail,
                    &rewritten,
                    limit,
                    path_prefix,
                    exts,
                    pos_filter,
                );
            }
            let remote = RemoteArgosBackend::from_settings(settings)?;
            let mut hits = remote.search(&rewritten, limit, path_prefix, exts, pos_filter)?;
            // Mail index is local-only; merge when unscoped (or mailfolder handled above).
            if should_query_mail(settings, path_prefix, exts) {
                if let Some(mail_be) = mail {
                    let mail_hits =
                        mail_be.search(&rewritten, limit, None, None, pos_filter)?;
                    hits = merge_hits_by_score(hits, mail_hits, limit);
                }
            }
            Ok(hits)
        }
        "hybrid" => {
            // Mail folder scopes stay on the local mail index only.
            if path_prefix
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .is_some_and(|p| p.starts_with("mailfolder:"))
            {
                return search_local_with_mail(
                    settings,
                    local,
                    mail,
                    &rewritten,
                    limit,
                    path_prefix,
                    exts,
                    pos_filter,
                );
            }
            let remote = RemoteArgosBackend::from_settings(settings)?;
            let mut hits = hybrid_search(
                local,
                &remote,
                &rewritten,
                limit,
                path_prefix,
                exts,
                pos_filter,
            )?;
            if should_query_mail(settings, path_prefix, exts) {
                if let Some(mail_be) = mail {
                    let mail_hits =
                        mail_be.search(&rewritten, limit, None, None, pos_filter)?;
                    hits = merge_hits_by_score(hits, mail_hits, limit);
                }
            }
            Ok(hits)
        }
        _ => search_local_with_mail(
            settings,
            local,
            mail,
            &rewritten,
            limit,
            path_prefix,
            exts,
            pos_filter,
        ),
    }
}

fn should_query_mail(
    settings: &Settings,
    path_prefix: Option<&str>,
    exts: Option<&[String]>,
) -> bool {
    if !settings.mail_enabled {
        return false;
    }
    if let Some(p) = path_prefix.map(str::trim).filter(|s| !s.is_empty()) {
        return p.starts_with("mailfolder:");
    }
    // Extension filters are file-oriented; skip mail when an ext filter is set.
    exts.map(|e| e.is_empty()).unwrap_or(true)
}

fn search_local_with_mail(
    settings: &Settings,
    local: &TantivyBackend,
    mail: Option<&TantivyBackend>,
    query: &str,
    limit: usize,
    path_prefix: Option<&str>,
    exts: Option<&[String]>,
    pos_filter: bool,
) -> Result<Vec<SearchHit>, String> {
    let prefix = path_prefix.map(str::trim).filter(|s| !s.is_empty());
    if let Some(p) = prefix {
        if p.starts_with("mailfolder:") {
            if !settings.mail_enabled {
                return Ok(Vec::new());
            }
            let Some(mail_be) = mail else {
                return Ok(Vec::new());
            };
            return mail_be.search(query, limit, Some(p), None, pos_filter);
        }
        // Filesystem scope: files only.
        return local.search(query, limit, Some(p), exts, pos_filter);
    }

    let file_hits = local.search(query, limit, None, exts, pos_filter)?;
    if !should_query_mail(settings, None, exts) {
        return Ok(file_hits);
    }
    let Some(mail_be) = mail else {
        return Ok(file_hits);
    };
    let mail_hits = mail_be.search(query, limit, None, None, pos_filter)?;
    Ok(merge_hits_by_score(file_hits, mail_hits, limit))
}

fn merge_hits_by_score(
    mut a: Vec<SearchHit>,
    mut b: Vec<SearchHit>,
    limit: usize,
) -> Vec<SearchHit> {
    a.append(&mut b);
    a.sort_by(|x, y| y.score.total_cmp(&x.score));
    a.truncate(limit);
    a
}

/// Local search with optional email thread collapse from settings.
pub fn run_search_with_mail_options(
    settings: &Settings,
    local: &TantivyBackend,
    mail: Option<&TantivyBackend>,
    query: &str,
    limit: usize,
    path_prefix: Option<&str>,
    exts: Option<&[String]>,
    user_dict: &UserDictMatcher,
) -> Result<Vec<SearchHit>, String> {
    let fetch = if settings.mail_thread_collapse {
        (limit * 3).max(limit)
    } else {
        limit
    };
    let mut hits = run_search(
        settings,
        local,
        mail,
        query,
        fetch,
        path_prefix,
        exts,
        user_dict,
    )?;
    if settings.mail_thread_collapse {
        hits = collapse_email_threads(hits);
        hits.truncate(limit);
    }
    Ok(hits)
}

/// Route preview; for hybrid, try local then remote. Outlook ids use the mail index.
pub fn run_preview(
    settings: &Settings,
    local: &TantivyBackend,
    mail: Option<&TantivyBackend>,
    hit_id: &str,
) -> Result<Option<SearchHit>, String> {
    let prefer_mail = hit_id.starts_with("outlook:") || hit_id.contains("outlook:");
    if prefer_mail {
        if let Some(mail_be) = mail {
            if let Some(hit) = mail_be.preview(hit_id)? {
                return Ok(Some(hit));
            }
        }
    }
    match settings.search_mode.as_str() {
        "remote" => {
            let remote = RemoteArgosBackend::from_settings(settings)?;
            remote.preview(hit_id)
        }
        "hybrid" => {
            if let Some(hit) = local.preview(hit_id)? {
                return Ok(Some(hit));
            }
            if let Some(mail_be) = mail {
                if let Some(hit) = mail_be.preview(hit_id)? {
                    return Ok(Some(hit));
                }
            }
            let remote = RemoteArgosBackend::from_settings(settings)?;
            remote.preview(hit_id)
        }
        _ => {
            if let Some(hit) = local.preview(hit_id)? {
                return Ok(Some(hit));
            }
            if let Some(mail_be) = mail {
                return mail_be.preview(hit_id);
            }
            Ok(None)
        }
    }
}

const PATH_MATCHES_LIMIT: usize = 50;

/// Matching chunks for one file (local index only). Used by preview occurrence navigation.
pub fn run_path_matches(
    settings: &Settings,
    local: &TantivyBackend,
    mail: Option<&TantivyBackend>,
    query: &str,
    path: &str,
    user_dict: &UserDictMatcher,
) -> Result<Vec<SearchHit>, String> {
    let rewritten = apply_user_dictionary(query, user_dict);
    if crate::mail::is_outlook_path(path) {
        let Some(mail_be) = mail else {
            return Ok(Vec::new());
        };
        return mail_be.matches_for_path(
            &rewritten,
            path,
            PATH_MATCHES_LIMIT,
            settings.pos_filter_enabled,
        );
    }
    local.matches_for_path(
        &rewritten,
        path,
        PATH_MATCHES_LIMIT,
        settings.pos_filter_enabled,
    )
}

pub fn normalize_search_mode(mode: &str) -> String {
    match mode {
        "remote" | "hybrid" => mode.into(),
        _ => "local".into(),
    }
}

pub fn ensure_server_token(settings: &mut Settings) {
    if settings.remote_server_enabled && settings.remote_server_token.trim().is_empty() {
        settings.remote_server_token = uuid::Uuid::new_v4().to_string();
    }
}
