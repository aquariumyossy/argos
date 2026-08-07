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
pub fn filter_hits_by_path_prefix(
    hits: Vec<SearchHit>,
    path_prefix: Option<&str>,
) -> Vec<SearchHit> {
    let Some(prefix) = path_prefix.map(str::trim).filter(|s| !s.is_empty()) else {
        return hits;
    };
    hits.into_iter()
        .filter(|h| pathutil::path_starts_with(&h.path, prefix))
        .collect()
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
pub fn run_search(
    settings: &Settings,
    local: &TantivyBackend,
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
            let remote = RemoteArgosBackend::from_settings(settings)?;
            remote.search(&rewritten, limit, path_prefix, exts, pos_filter)
        }
        "hybrid" => {
            let remote = RemoteArgosBackend::from_settings(settings)?;
            hybrid_search(
                local,
                &remote,
                &rewritten,
                limit,
                path_prefix,
                exts,
                pos_filter,
            )
        }
        _ => local.search(&rewritten, limit, path_prefix, exts, pos_filter),
    }
}

/// Route preview; for hybrid, try local then remote.
pub fn run_preview(
    settings: &Settings,
    local: &TantivyBackend,
    hit_id: &str,
) -> Result<Option<SearchHit>, String> {
    match settings.search_mode.as_str() {
        "remote" => {
            let remote = RemoteArgosBackend::from_settings(settings)?;
            remote.preview(hit_id)
        }
        "hybrid" => {
            if let Some(hit) = local.preview(hit_id)? {
                return Ok(Some(hit));
            }
            let remote = RemoteArgosBackend::from_settings(settings)?;
            remote.preview(hit_id)
        }
        _ => local.preview(hit_id),
    }
}

const PATH_MATCHES_LIMIT: usize = 50;

/// Matching chunks for one file (local index only). Used by preview occurrence navigation.
pub fn run_path_matches(
    settings: &Settings,
    local: &TantivyBackend,
    query: &str,
    path: &str,
    user_dict: &UserDictMatcher,
) -> Result<Vec<SearchHit>, String> {
    let rewritten = apply_user_dictionary(query, user_dict);
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
