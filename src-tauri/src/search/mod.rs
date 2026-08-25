use std::collections::{BTreeSet, HashSet};

use serde::{Deserialize, Serialize};

pub mod date;
pub mod history;
pub mod legal_ref;
pub mod morph;
pub mod remote_backend;
pub mod remote_share;
pub mod tantivy_backend;

pub use date::{format_unix_ymd, parse_date_range, today_ymd, DateFilter};
pub use history::{extract_search_terms, suggest_from_history, SearchTermSuggestion};
pub use morph::{apply_user_dictionary, is_noise_highlight_term, MorphAnalyzer, UserDictMatcher};
pub use remote_backend::{hybrid_search, RemoteArgosBackend};
pub use remote_share::{
    filter_hits_by_share, path_is_remotely_shared, RemoteShareSnapshot,
};
pub use tantivy_backend::{parse_query_syntax, TantivyBackend};

use crate::db::{Db, Settings};
use crate::pathutil;

/// Max paths fetched from SQLite for a date/from allowlist.
const SQL_PATH_CAP: usize = 10_000;

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

/// Retrieval tuning that differs between the popup and the LLM tool.
///
/// The popup searches a sentence the user selected and wants recall: one matching noun
/// is a useful hit. The LLM tool asks a question and wants precision, and it needs
/// paragraph-level results because a statute file holds hundreds of articles.
#[derive(Debug, Clone, Copy, Default)]
pub struct SearchOpts {
    /// Require a share of the query's search units to match, and drop question boilerplate.
    pub precision: bool,
    /// `Some(n)`: return units directly, at most `n` per file. `None`: one best unit per file.
    pub per_file_units: Option<usize>,
    /// Score a hit up when its parent folders carry a query unit. Directory names are the
    /// one signal the query side cannot supply: two cases match `解除 通知` equally well,
    /// and only the folder says which case is meant. Off for the popup, whose ranking is
    /// covered by existing tests.
    pub path_boost: bool,
}

impl SearchOpts {
    /// Settings used by the chat tool.
    pub fn for_llm(per_file_units: usize) -> Self {
        Self {
            precision: true,
            per_file_units: Some(per_file_units.max(1)),
            path_boost: true,
        }
    }
}

/// Optional hard filters that sit beside [`SearchOpts`] (which stays `Copy`).
#[derive(Debug, Clone, Default)]
pub struct SearchFilter {
    pub date: DateFilter,
    pub mail_from: Option<String>,
    pub sort_date: bool,
    /// `None` = unconstrained. `Some(empty)` = no matching paths (skip that index).
    pub mail_paths: Option<Vec<String>>,
    pub file_paths: Option<Vec<String>>,
}

impl SearchFilter {
    pub fn skip_files(&self) -> bool {
        self.mail_from
            .as_deref()
            .map(str::trim)
            .is_some_and(|s| !s.is_empty())
    }

    pub fn skip_remote(&self) -> bool {
        self.date.is_active() || self.skip_files()
    }

    pub fn is_constrained(&self) -> bool {
        self.date.is_active() || self.skip_files()
    }
}

/// Resolve SQLite path allowlists for a date / sender filter.
pub fn build_search_filter(
    db: &Db,
    date: DateFilter,
    mail_from: Option<&str>,
    path_prefix: Option<&str>,
    exts: Option<&[String]>,
    sort_date: bool,
) -> Result<SearchFilter, String> {
    let from = mail_from.map(str::trim).filter(|s| !s.is_empty());
    let prefix = path_prefix.map(str::trim).filter(|s| !s.is_empty());
    let skip_files = from.is_some();
    let need_paths = date.is_active() || from.is_some();
    if !need_paths {
        return Ok(SearchFilter {
            date,
            mail_from: from.map(|s| s.to_string()),
            sort_date,
            mail_paths: None,
            file_paths: None,
        });
    }

    let mail_folder = prefix.and_then(|p| p.strip_prefix("mailfolder:"));
    let fs_prefix = prefix.filter(|p| !p.starts_with("mailfolder:"));

    let mail_paths = if skip_files || mail_folder.is_some() || fs_prefix.is_none() {
        let paths = db
            .list_indexed_email_paths_in_range(
                date.after_unix,
                date.before_unix,
                from,
                mail_folder,
                SQL_PATH_CAP,
            )
            .map_err(|e| e.to_string())?;
        Some(paths)
    } else {
        // Filesystem folder scope: mail index is not queried.
        Some(Vec::new())
    };

    let file_paths = if skip_files || mail_folder.is_some() {
        Some(Vec::new())
    } else if date.is_active() {
        let mut paths = db
            .list_ok_file_paths_in_range(date.after_unix, date.before_unix, SQL_PATH_CAP)
            .map_err(|e| e.to_string())?;
        if let Some(p) = fs_prefix {
            paths.retain(|path| pathutil::path_starts_with(path, p));
        }
        if let Some(list) = exts.filter(|e| !e.is_empty()) {
            paths.retain(|path| {
                let ext = path_extension(path);
                list.iter().any(|e| e == &ext)
            });
        }
        Some(paths)
    } else {
        None
    };

    Ok(SearchFilter {
        date,
        mail_from: from.map(|s| s.to_string()),
        sort_date,
        mail_paths,
        file_paths,
    })
}

/// Narrow an existing filter to a preferred folder, for the scoped half of a folder-biased
/// search.
///
/// A path allowlist reaches the retrieval query as a `Must` clause, so the strict rung of
/// the ladder can find in-folder paragraphs directly. The prefix post-filter cannot: it
/// runs after retrieval, so a folder whose paragraphs miss the global top-N only surfaces
/// once the ladder has relaxed the query, which returns *looser* matches than an unscoped
/// search would.
///
/// Returns the filter unchanged when the folder holds more files than
/// [`tantivy_backend::PATH_OR_CAP`]; a `Must` clause of that many terms costs more than it
/// buys, and the caller still passes the prefix for post-filtering.
pub fn narrow_filter_to_prefix(
    db: &Db,
    base: &SearchFilter,
    prefix: &str,
) -> Result<SearchFilter, String> {
    let prefix = prefix.trim();
    let mut out = base.clone();
    if prefix.is_empty() || prefix.starts_with("mailfolder:") || base.skip_files() {
        return Ok(out);
    }
    match base.file_paths.as_ref() {
        // A date filter already produced an allowlist. Intersect it: two path `Must`
        // clauses in one query would require a path to equal two different values.
        Some(list) => {
            out.file_paths = Some(
                list.iter()
                    .filter(|p| pathutil::path_starts_with(p, prefix))
                    .cloned()
                    .collect(),
            );
        }
        None => {
            let paths = db
                .list_ok_file_paths_under_prefix(prefix, tantivy_backend::PATH_OR_CAP + 1)
                .map_err(|e| e.to_string())?;
            if paths.len() <= tantivy_backend::PATH_OR_CAP {
                out.file_paths = Some(paths);
            }
        }
    }
    Ok(out)
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

    /// Backends that cannot honour `opts` fall back to the default retrieval.
    fn search_opts(
        &self,
        query: &str,
        limit: usize,
        path_prefix: Option<&str>,
        exts: Option<&[String]>,
        pos_filter_enabled: bool,
        opts: SearchOpts,
    ) -> Result<Vec<SearchHit>, String> {
        let _ = opts;
        self.search(query, limit, path_prefix, exts, pos_filter_enabled)
    }

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
                merged.match_count = out[idx]
                    .match_count
                    .saturating_add(merged.match_count.max(1));
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
    run_search_with_opts(
        settings,
        local,
        mail,
        query,
        limit,
        path_prefix,
        exts,
        user_dict,
        SearchOpts::default(),
        &SearchFilter::default(),
    )
}

/// `run_search` with retrieval tuning.
///
/// `opts` only reaches backends this process owns. Remote and hybrid file search keeps
/// the default retrieval, because mixing paragraph-level local hits with file-level
/// remote hits would make the merged ranking meaningless.
#[allow(clippy::too_many_arguments)]
pub fn run_search_with_opts(
    settings: &Settings,
    local: &TantivyBackend,
    mail: Option<&TantivyBackend>,
    query: &str,
    limit: usize,
    path_prefix: Option<&str>,
    exts: Option<&[String]>,
    user_dict: &UserDictMatcher,
    opts: SearchOpts,
    filter: &SearchFilter,
) -> Result<Vec<SearchHit>, String> {
    let rewritten = apply_user_dictionary(query, user_dict);
    let pos_filter = settings.pos_filter_enabled;
    let local_only = filter.skip_remote()
        || path_prefix
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .is_some_and(|p| p.starts_with("mailfolder:"));
    match settings.search_mode.as_str() {
        "remote" if !local_only => {
            let remote = RemoteArgosBackend::from_settings(settings)?;
            let mut hits = remote.search(&rewritten, limit, path_prefix, exts, pos_filter)?;
            if should_query_mail(settings, path_prefix, exts, filter) {
                if let Some(mail_be) = mail {
                    let mail_hits = search_mail(
                        mail_be,
                        &rewritten,
                        limit,
                        None,
                        pos_filter,
                        mail_opts(opts),
                        filter,
                    )?;
                    hits = merge_hits_by_score(hits, mail_hits, limit);
                }
            }
            Ok(hits)
        }
        "hybrid" if !local_only => {
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
            if should_query_mail(settings, path_prefix, exts, filter) {
                if let Some(mail_be) = mail {
                    let mail_hits = search_mail(
                        mail_be,
                        &rewritten,
                        limit,
                        None,
                        pos_filter,
                        mail_opts(opts),
                        filter,
                    )?;
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
            opts,
            filter,
        ),
    }
}

/// One email is one unit, so paragraph-level fan-out only duplicates long messages.
fn mail_opts(opts: SearchOpts) -> SearchOpts {
    SearchOpts {
        per_file_units: None,
        ..opts
    }
}

fn should_query_mail(
    settings: &Settings,
    path_prefix: Option<&str>,
    exts: Option<&[String]>,
    filter: &SearchFilter,
) -> bool {
    if !settings.mail_enabled {
        return false;
    }
    if let Some(paths) = filter.mail_paths.as_ref() {
        if paths.is_empty() && filter.is_constrained() {
            return false;
        }
    }
    if let Some(p) = path_prefix.map(str::trim).filter(|s| !s.is_empty()) {
        return p.starts_with("mailfolder:");
    }
    if filter.skip_files() {
        return true;
    }
    // Extension filters are file-oriented; skip mail when an ext filter is set.
    exts.map(|e| e.is_empty()).unwrap_or(true)
}

fn search_mail(
    mail: &TantivyBackend,
    query: &str,
    limit: usize,
    path_prefix: Option<&str>,
    pos_filter: bool,
    opts: SearchOpts,
    filter: &SearchFilter,
) -> Result<Vec<SearchHit>, String> {
    let allow = filter.mail_paths.as_deref();
    if let Some(paths) = allow {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
    }
    mail.search_filtered(
        query,
        limit,
        path_prefix,
        None,
        pos_filter,
        opts,
        allow,
    )
}

fn search_files(
    local: &TantivyBackend,
    query: &str,
    limit: usize,
    path_prefix: Option<&str>,
    exts: Option<&[String]>,
    pos_filter: bool,
    opts: SearchOpts,
    filter: &SearchFilter,
) -> Result<Vec<SearchHit>, String> {
    if filter.skip_files() {
        return Ok(Vec::new());
    }
    if let Some(paths) = filter.file_paths.as_ref() {
        if paths.is_empty() && filter.date.is_active() {
            return Ok(Vec::new());
        }
    }
    local.search_filtered(
        query,
        limit,
        path_prefix,
        exts,
        pos_filter,
        opts,
        filter.file_paths.as_deref(),
    )
}

#[allow(clippy::too_many_arguments)]
fn search_local_with_mail(
    settings: &Settings,
    local: &TantivyBackend,
    mail: Option<&TantivyBackend>,
    query: &str,
    limit: usize,
    path_prefix: Option<&str>,
    exts: Option<&[String]>,
    pos_filter: bool,
    opts: SearchOpts,
    filter: &SearchFilter,
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
            return search_mail(
                mail_be,
                query,
                limit,
                Some(p),
                pos_filter,
                mail_opts(opts),
                filter,
            );
        }
        if filter.skip_files() {
            // Filesystem folder scope does not apply to Outlook paths.
            if !should_query_mail(settings, None, exts, filter) {
                return Ok(Vec::new());
            }
            let Some(mail_be) = mail else {
                return Ok(Vec::new());
            };
            return search_mail(
                mail_be,
                query,
                limit,
                None,
                pos_filter,
                mail_opts(opts),
                filter,
            );
        }
        return search_files(local, query, limit, Some(p), exts, pos_filter, opts, filter);
    }

    let file_hits = search_files(local, query, limit, None, exts, pos_filter, opts, filter)?;
    if !should_query_mail(settings, None, exts, filter) {
        return Ok(file_hits);
    }
    let Some(mail_be) = mail else {
        return Ok(file_hits);
    };
    let mail_hits = search_mail(mail_be, query, limit, None, pos_filter, mail_opts(opts), filter)?;
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
    filter: &SearchFilter,
) -> Result<Vec<SearchHit>, String> {
    run_search_with_mail_opts(
        settings,
        local,
        mail,
        query,
        limit,
        path_prefix,
        exts,
        user_dict,
        SearchOpts::default(),
        filter,
    )
}

/// Retrieval for the chat tool: precision-first, paragraph-level.
#[allow(clippy::too_many_arguments)]
pub fn run_search_precise(
    settings: &Settings,
    local: &TantivyBackend,
    mail: Option<&TantivyBackend>,
    query: &str,
    limit: usize,
    path_prefix: Option<&str>,
    exts: Option<&[String]>,
    user_dict: &UserDictMatcher,
    per_file_units: usize,
    filter: &SearchFilter,
) -> Result<Vec<SearchHit>, String> {
    run_search_with_mail_opts(
        settings,
        local,
        mail,
        query,
        limit,
        path_prefix,
        exts,
        user_dict,
        SearchOpts::for_llm(per_file_units),
        filter,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn run_search_with_mail_opts(
    settings: &Settings,
    local: &TantivyBackend,
    mail: Option<&TantivyBackend>,
    query: &str,
    limit: usize,
    path_prefix: Option<&str>,
    exts: Option<&[String]>,
    user_dict: &UserDictMatcher,
    opts: SearchOpts,
    filter: &SearchFilter,
) -> Result<Vec<SearchHit>, String> {
    let fetch = if settings.mail_thread_collapse {
        (limit * 3).max(limit)
    } else {
        limit
    };
    let mut hits = run_search_with_opts(
        settings,
        local,
        mail,
        query,
        fetch,
        path_prefix,
        exts,
        user_dict,
        opts,
        filter,
    )?;
    if settings.mail_thread_collapse {
        hits = collapse_email_threads(hits);
    }
    if filter.sort_date {
        sort_hits_by_mail_date(&mut hits);
    }
    hits.truncate(limit);
    Ok(hits)
}

/// Load mail documents for already-filtered paths (sender listing).
pub fn list_mail_hits(
    mail: &TantivyBackend,
    paths: &[String],
    limit: usize,
    collapse: bool,
) -> Result<Vec<SearchHit>, String> {
    let fetch = if collapse {
        (limit * 3).max(limit)
    } else {
        limit
    };
    let mut hits = mail.hits_for_paths(paths, fetch)?;
    if collapse {
        hits = collapse_email_threads(hits);
    }
    sort_hits_by_mail_date(&mut hits);
    hits.truncate(limit);
    Ok(hits)
}

fn sort_hits_by_mail_date(hits: &mut [SearchHit]) {
    hits.sort_by(|a, b| {
        let da: i64 = a.mail_date.parse().unwrap_or(0);
        let db: i64 = b.mail_date.parse().unwrap_or(0);
        db.cmp(&da).then_with(|| b.score.total_cmp(&a.score))
    });
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
/// Full bodies are loaded only when the file has at most this many units.
const PREVIEW_FILE_UNIT_CAP: usize = 200;
const PREVIEW_FILE_CHAR_CAP: usize = 50_000;
const PREVIEW_FILE_CONTEXT: usize = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewFile {
    pub units: Vec<SearchHit>,
    /// True when only a window around matches is returned.
    pub excerpt: bool,
    pub match_ids: Vec<String>,
}

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

/// List "show more": remote hits go to the host; local/mail stay on this machine.
pub fn run_list_path_matches(
    settings: &Settings,
    local: &TantivyBackend,
    mail: Option<&TantivyBackend>,
    query: &str,
    path: &str,
    source: Option<&str>,
    user_dict: &UserDictMatcher,
) -> Result<Vec<SearchHit>, String> {
    if source.is_some_and(|s| s.eq_ignore_ascii_case("remote")) {
        let rewritten = apply_user_dictionary(query, user_dict);
        let remote = RemoteArgosBackend::from_settings(settings)?;
        return remote.path_matches(&rewritten, path, PATH_MATCHES_LIMIT);
    }
    run_path_matches(settings, local, mail, query, path, user_dict)
}

/// Indexed units for one file in document order, optionally trimmed to a match window.
pub fn run_preview_file(
    settings: &Settings,
    local: &TantivyBackend,
    mail: Option<&TantivyBackend>,
    query: &str,
    path: &str,
    user_dict: &UserDictMatcher,
) -> Result<PreviewFile, String> {
    let matches = run_path_matches(settings, local, mail, query, path, user_dict)?;
    let backend = if crate::mail::is_outlook_path(path) {
        mail.ok_or_else(|| "メールインデックスがありません".to_string())?
    } else {
        local
    };
    let addrs = backend.unit_addrs_for_path(path)?;
    if addrs.len() <= PREVIEW_FILE_UNIT_CAP {
        let all = backend.hits_from_addrs(addrs)?;
        return Ok(assemble_preview_file(all, matches));
    }
    let chunk_ids =
        preview_chunk_window(&matches, PREVIEW_FILE_CONTEXT, PREVIEW_FILE_UNIT_CAP as u32);
    let window = backend.units_for_path_chunk_ids(path, &chunk_ids)?;
    let mut preview = assemble_preview_file(window, matches);
    preview.excerpt = true;
    Ok(preview)
}

/// chunk_id ± context around matches. If none have a chunk_id, take the file head.
fn preview_chunk_window(matches: &[SearchHit], context: usize, fallback_cap: u32) -> Vec<u32> {
    let ctx = context as u32;
    let mut ids = BTreeSet::new();
    for m in matches {
        let Some(c) = m.chunk_id else {
            continue;
        };
        let from = c.saturating_sub(ctx);
        let to = c.saturating_add(ctx);
        for id in from..=to {
            ids.insert(id);
        }
    }
    if ids.is_empty() {
        return (0..fallback_cap).collect();
    }
    ids.into_iter().collect()
}

/// Insert match hits that `units_for_path` missed, then keep document order.
fn merge_missing_match_units(all: &mut Vec<SearchHit>, matches: &[SearchHit]) {
    let have: HashSet<String> = all.iter().map(|h| h.id.clone()).collect();
    for m in matches {
        if !have.contains(&m.id) {
            all.push(m.clone());
        }
    }
    all.sort_by(|a, b| {
        a.chunk_id
            .unwrap_or(0)
            .cmp(&b.chunk_id.unwrap_or(0))
            .then_with(|| a.page.unwrap_or(0).cmp(&b.page.unwrap_or(0)))
            .then_with(|| a.id.cmp(&b.id))
    });
}

fn union_highlight_terms(hits: &[SearchHit]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for h in hits {
        for t in &h.highlight_terms {
            if !t.is_empty() && seen.insert(t.clone()) {
                out.push(t.clone());
            }
        }
    }
    out
}

fn stamp_highlight_terms(units: &mut [SearchHit], terms: &[String]) {
    if terms.is_empty() {
        return;
    }
    for unit in units {
        let mut seen: HashSet<String> = unit.highlight_terms.iter().cloned().collect();
        for t in terms {
            if seen.insert(t.clone()) {
                unit.highlight_terms.push(t.clone());
            }
        }
    }
}

fn assemble_preview_file(mut all: Vec<SearchHit>, matches: Vec<SearchHit>) -> PreviewFile {
    let match_ids: Vec<String> = matches.iter().map(|h| h.id.clone()).collect();
    let highlight_terms = union_highlight_terms(&matches);
    merge_missing_match_units(&mut all, &matches);
    let total_chars: usize = all.iter().map(|h| h.preview_text.chars().count()).sum();
    let within_cap = all.len() <= PREVIEW_FILE_UNIT_CAP && total_chars <= PREVIEW_FILE_CHAR_CAP;
    if within_cap {
        stamp_highlight_terms(&mut all, &highlight_terms);
        return PreviewFile {
            units: all,
            excerpt: false,
            match_ids,
        };
    }
    if match_ids.is_empty() {
        let mut units: Vec<SearchHit> = all.into_iter().take(PREVIEW_FILE_UNIT_CAP).collect();
        stamp_highlight_terms(&mut units, &highlight_terms);
        return PreviewFile {
            units,
            excerpt: true,
            match_ids,
        };
    }
    let match_set: HashSet<&str> = match_ids.iter().map(|s| s.as_str()).collect();
    let match_indices: Vec<usize> = all
        .iter()
        .enumerate()
        .filter_map(|(i, unit)| match_set.contains(unit.id.as_str()).then_some(i))
        .collect();
    let keep = preview_keep_mask(all.len(), &match_indices, PREVIEW_FILE_CONTEXT);
    let mut units: Vec<SearchHit> = all
        .into_iter()
        .zip(keep)
        .filter_map(|(u, k)| k.then_some(u))
        .collect();
    stamp_highlight_terms(&mut units, &highlight_terms);
    PreviewFile {
        units,
        excerpt: true,
        match_ids,
    }
}

fn preview_keep_mask(len: usize, match_indices: &[usize], context: usize) -> Vec<bool> {
    let mut keep = vec![false; len];
    for &i in match_indices {
        if i >= len {
            continue;
        }
        let from = i.saturating_sub(context);
        let to = (i + context + 1).min(len);
        for slot in keep.iter_mut().take(to).skip(from) {
            *slot = true;
        }
    }
    keep
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

#[cfg(test)]
mod tests {
    use super::{
        assemble_preview_file, build_search_filter, collapse_email_threads, list_mail_hits,
        narrow_filter_to_prefix, preview_chunk_window, preview_keep_mask, run_preview_file,
        sort_hits_by_mail_date, DateFilter, SearchFilter, SearchHit, TantivyBackend,
        UserDictMatcher,
    };
    use crate::db::{Db, Settings};
    use crate::extractor::ExtractedDoc;
    use crate::extractor::segment::SearchUnit;
    use crate::search::tantivy_backend::EmailDocMeta;

    fn dummy_hit(id: &str, chunk: u32, text: &str) -> SearchHit {
        SearchHit {
            id: id.into(),
            title: "t".into(),
            snippet: text.chars().take(40).collect(),
            path: r"C:\a.md".into(),
            page: Some(1),
            chunk_id: Some(chunk),
            score: 1.0,
            source: "local".into(),
            preview_text: text.into(),
            highlight_terms: vec![],
            match_count: 1,
            paragraphs: vec![],
            unit_label: String::new(),
            mail_from: String::new(),
            mail_date: String::new(),
            mail_conversation_id: String::new(),
            mail_folder: String::new(),
            doc_kind: "file".into(),
        }
    }

    #[test]
    fn preview_keep_mask_includes_match_neighbors() {
        let keep = preview_keep_mask(8, &[3], 2);
        assert_eq!(
            keep,
            vec![false, true, true, true, true, true, false, false]
        );
    }

    #[test]
    fn preview_keep_mask_merges_nearby_windows() {
        let keep = preview_keep_mask(6, &[0, 5], 1);
        assert_eq!(keep, vec![true, true, false, false, true, true]);
    }

    #[test]
    fn assemble_keeps_matches_missing_from_fetched_units() {
        let filler = "あ".repeat(50);
        let all: Vec<SearchHit> = (0..210)
            .map(|i| dummy_hit(&format!("f#{i}"), i, &filler))
            .collect();
        let late = dummy_hit("f#500", 500, "第499条（弁済による代位の要件）");
        let preview = assemble_preview_file(all, vec![late.clone()]);
        assert!(preview.excerpt);
        assert_eq!(preview.match_ids, vec!["f#500".to_string()]);
        assert!(
            preview.units.iter().any(|u| u.id == "f#500"),
            "match outside the fetch window must still appear in the excerpt"
        );
    }

    #[test]
    fn assemble_under_cap_returns_whole_file() {
        let all: Vec<SearchHit> = (0..10)
            .map(|i| dummy_hit(&format!("f#{i}"), i, "短い"))
            .collect();
        let hit = all[3].clone();
        let preview = assemble_preview_file(all, vec![hit]);
        assert!(!preview.excerpt);
        assert_eq!(preview.units.len(), 10);
        assert_eq!(preview.match_ids, vec!["f#3".to_string()]);
    }

    #[test]
    fn assemble_stamps_match_highlight_terms_on_units() {
        let all: Vec<SearchHit> = (0..5)
            .map(|i| dummy_hit(&format!("f#{i}"), i, "短い"))
            .collect();
        let mut hit = all[2].clone();
        hit.highlight_terms = vec!["反社会的勢力".into(), "排除".into()];
        let preview = assemble_preview_file(all, vec![hit]);
        assert!(preview
            .units
            .iter()
            .all(|u| u.highlight_terms.iter().any(|t| t == "反社会的勢力")
                && u.highlight_terms.iter().any(|t| t == "排除")));
    }

    #[test]
    fn preview_chunk_window_expands_neighbors() {
        let hit = dummy_hit("f#10", 10, "x");
        assert_eq!(
            preview_chunk_window(&[hit], 2, 200),
            vec![8, 9, 10, 11, 12]
        );
    }

    #[test]
    fn preview_chunk_window_falls_back_to_file_head() {
        let mut hit = dummy_hit("f#x", 0, "x");
        hit.chunk_id = None;
        assert_eq!(preview_chunk_window(&[hit], 2, 4), vec![0, 1, 2, 3]);
    }

    #[test]
    fn run_preview_file_large_file_returns_late_match_window_only() {
        let dir = std::env::temp_dir().join(format!(
            "argos-preview-window-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let backend = TantivyBackend::open(&dir).expect("open index").backend;

        let mut body = String::new();
        for i in 0..220 {
            body.push_str(&format!(
                "第{i}条 これはプレビュー用のダミー本文を十分長くした段落です。さらに文字数を稼ぐための追記。番号{i:04}。\n\n"
            ));
        }
        body.push_str(
            "第999条 弁済による代位の要件をここに置く。これは末尾ユニットとして独立させる。\n\n",
        );
        let path = dir.join("civil.md");
        let path_str = path.to_str().unwrap().to_string();
        std::fs::write(&path, &body).unwrap();
        let n = backend
            .index_file(
                &path,
                &path_str,
                dir.to_str().unwrap(),
                1,
                body.len() as u64,
                &ExtractedDoc {
                    title: "civil".into(),
                    pages: vec![body],
                },
            )
            .expect("index");
        assert!(
            n > 200,
            "fixture must exceed PREVIEW_FILE_UNIT_CAP, got {n}"
        );

        let preview = run_preview_file(
            &Settings::default(),
            &backend,
            None,
            "弁済による代位",
            &path_str,
            &UserDictMatcher::from_words(Vec::<String>::new()),
        )
        .expect("preview");

        assert!(preview.excerpt, "large file must be an excerpt");
        assert!(
            preview.units.len() < n,
            "must not return every unit: got {} of {n}",
            preview.units.len()
        );
        assert!(
            preview
                .units
                .iter()
                .any(|u| u.preview_text.contains("弁済による代位")),
            "late match must be present: {:?}",
            preview.units.iter().map(|u| &u.id).collect::<Vec<_>>()
        );
        assert!(
            !preview.units.iter().any(|u| u.chunk_id == Some(0)),
            "file head must not be loaded for a late-only match"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn mail_hit(
        id: &str,
        path: &str,
        from: &str,
        date: &str,
        conv: &str,
        score: f32,
    ) -> SearchHit {
        SearchHit {
            id: id.into(),
            title: "件名".into(),
            snippet: String::new(),
            path: path.into(),
            page: None,
            chunk_id: Some(0),
            score,
            source: "outlook".into(),
            preview_text: String::new(),
            highlight_terms: vec![],
            match_count: 1,
            paragraphs: vec![],
            unit_label: String::new(),
            mail_from: from.into(),
            mail_date: date.into(),
            mail_conversation_id: conv.into(),
            mail_folder: "受信トレイ".into(),
            doc_kind: "email".into(),
        }
    }

    fn temp_db() -> (std::path::PathBuf, Db) {
        let dir = std::env::temp_dir().join(format!(
            "argos-search-filter-db-{}",
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
    fn prefix_allowlist_stops_at_the_folder_boundary() {
        let (dir, db) = temp_db();
        let folder = db.add_folder(r"C:\cases", "").unwrap();
        for path in [
            r"C:\cases\alpha\a.txt",
            r"C:\cases\alpha\sub\b.txt",
            // A sibling that shares the prefix as raw text but not as a folder.
            r"C:\cases\alpha2\c.txt",
            r"C:\cases\beta\d.txt",
        ] {
            db.upsert_file(folder.id, path, "txt", 1, 1_700_000_000, "h")
                .unwrap();
        }
        let base = SearchFilter::default();
        let narrowed = narrow_filter_to_prefix(&db, &base, r"C:\cases\alpha").unwrap();
        let mut paths = narrowed.file_paths.expect("allowlist");
        paths.sort();
        assert_eq!(
            paths,
            vec![
                r"C:\cases\alpha\a.txt".to_string(),
                r"C:\cases\alpha\sub\b.txt".to_string(),
            ],
            "alpha2 shares the characters but is a different folder"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prefix_intersects_the_date_allowlist_instead_of_adding_a_second_one() {
        // Two path constraints in one query would ask a path to equal two values at once.
        let (dir, db) = temp_db();
        let folder = db.add_folder(r"C:\cases", "").unwrap();
        db.upsert_file(folder.id, r"C:\cases\alpha\new.txt", "txt", 1, 1_700_000_100, "h")
            .unwrap();
        db.upsert_file(folder.id, r"C:\cases\alpha\old.txt", "txt", 1, 1_600_000_000, "h")
            .unwrap();
        db.upsert_file(folder.id, r"C:\cases\beta\new.txt", "txt", 1, 1_700_000_100, "h")
            .unwrap();

        let date = DateFilter {
            after_unix: Some(1_700_000_000),
            before_unix: Some(1_700_000_200),
        };
        let base = build_search_filter(&db, date, None, None, None, false).unwrap();
        assert_eq!(base.file_paths.as_ref().map(Vec::len), Some(2));

        let narrowed = narrow_filter_to_prefix(&db, &base, r"C:\cases\alpha").unwrap();
        assert_eq!(
            narrowed.file_paths.as_deref(),
            Some(&[r"C:\cases\alpha\new.txt".to_string()][..]),
            "the date range and the folder must end up as one list"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prefix_allowlist_is_empty_when_the_folder_holds_nothing() {
        // The scoped half legitimately finds nothing; the caller still runs the unscoped
        // half, so this must not read as "no results anywhere".
        let (dir, db) = temp_db();
        let folder = db.add_folder(r"C:\cases", "").unwrap();
        db.upsert_file(folder.id, r"C:\cases\beta\d.txt", "txt", 1, 1_700_000_000, "h")
            .unwrap();
        let narrowed =
            narrow_filter_to_prefix(&db, &SearchFilter::default(), r"C:\cases\empty").unwrap();
        assert_eq!(narrowed.file_paths.as_deref(), Some(&[][..]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prefix_allowlist_treats_like_wildcards_literally() {
        let (dir, db) = temp_db();
        let folder = db.add_folder(r"C:\cases", "").unwrap();
        db.upsert_file(folder.id, r"C:\cases\100%\a.txt", "txt", 1, 1_700_000_000, "h")
            .unwrap();
        db.upsert_file(folder.id, r"C:\cases\100X\b.txt", "txt", 1, 1_700_000_000, "h")
            .unwrap();
        let narrowed =
            narrow_filter_to_prefix(&db, &SearchFilter::default(), r"C:\cases\100%").unwrap();
        assert_eq!(
            narrowed.file_paths.as_deref(),
            Some(&[r"C:\cases\100%\a.txt".to_string()][..]),
            "a folder named 100% is not a wildcard"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mail_folder_scope_leaves_the_file_allowlist_alone() {
        let (dir, db) = temp_db();
        let base = SearchFilter::default();
        let narrowed = narrow_filter_to_prefix(&db, &base, "mailfolder:受信トレイ").unwrap();
        assert!(narrowed.file_paths.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn from_filter_skips_files_and_remote() {
        let f = SearchFilter {
            mail_from: Some("Aさん".into()),
            ..SearchFilter::default()
        };
        assert!(f.skip_files());
        assert!(f.skip_remote());
        assert!(f.is_constrained());
        let dated = SearchFilter {
            date: DateFilter {
                after_unix: Some(1),
                before_unix: None,
            },
            ..SearchFilter::default()
        };
        assert!(!dated.skip_files());
        assert!(dated.skip_remote());
    }

    #[test]
    fn sort_hits_by_mail_date_newest_first() {
        let mut hits = vec![
            mail_hit("a", "outlook:a", "A", "100", "", 9.0),
            mail_hit("b", "outlook:b", "B", "300", "", 1.0),
            mail_hit("c", "outlook:c", "C", "200", "", 5.0),
        ];
        sort_hits_by_mail_date(&mut hits);
        assert_eq!(
            hits.iter().map(|h| h.id.as_str()).collect::<Vec<_>>(),
            vec!["b", "c", "a"]
        );
    }

    #[test]
    fn collapse_keeps_newest_in_thread() {
        let hits = vec![
            mail_hit("old", "outlook:old", "A", "100", "conv", 9.0),
            mail_hit("new", "outlook:new", "A", "300", "conv", 1.0),
        ];
        let collapsed = collapse_email_threads(hits);
        assert_eq!(collapsed.len(), 1);
        assert_eq!(collapsed[0].id, "new");
    }

    #[test]
    fn sqlite_from_filter_skips_file_paths() {
        let (dir, db) = temp_db();
        let folder = db.add_folder(r"C:\docs", "").unwrap();
        db.upsert_file(folder.id, r"C:\docs\a.txt", "txt", 1, 1_700_000_000, "h")
            .unwrap();
        db.upsert_email_message(
            "outlook:a",
            "s",
            "e",
            "",
            "受信トレイ",
            "Aさん",
            "件名",
            1_700_000_000,
            "h",
            "indexed",
        )
        .unwrap();
        let filter = build_search_filter(&db, DateFilter::default(), Some("Aさん"), None, None, false)
            .unwrap();
        assert!(filter.skip_files());
        assert_eq!(filter.file_paths.as_deref(), Some(&[][..]));
        assert_eq!(filter.mail_paths.as_deref(), Some(&["outlook:a".to_string()][..]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn from_listing_keeps_mail_without_sender_in_body() {
        let dir = std::env::temp_dir().join(format!(
            "argos-mail-from-list-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mail = TantivyBackend::open_mail(&dir).expect("open mail").backend;
        let body = "打ち合わせの日程を調整いたします。契約書の確認をお願いします。";
        mail.index_email(
            "outlook:from-a",
            "日程",
            1_700_000_000,
            body.len() as u64,
            &[SearchUnit {
                text: body.into(),
                page: None,
                unit_id: 0,
                label: "日程".into(),
                kind: "message".into(),
            }],
            &EmailDocMeta {
                from: "Aさん".into(),
                date_unix: 1_700_000_000,
                conversation_id: String::new(),
                folder: "受信トレイ".into(),
            },
        )
        .expect("index");

        let by_name = mail
            .search_filtered(
                "Aさん",
                8,
                None,
                None,
                false,
                super::SearchOpts::default(),
                None,
            )
            .expect("search name");
        assert!(
            by_name.is_empty(),
            "sender name is not in the body, so BM25 must not be required: {:?}",
            by_name.iter().map(|h| &h.path).collect::<Vec<_>>()
        );

        let listed = list_mail_hits(&mail, &["outlook:from-a".into()], 8, false).expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].path, "outlook:from-a");
        assert_eq!(listed[0].mail_from, "Aさん");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
