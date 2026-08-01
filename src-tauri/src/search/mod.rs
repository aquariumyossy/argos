use serde::{Deserialize, Serialize};

pub mod remote_backend;
pub mod tantivy_backend;

pub use remote_backend::{hybrid_search, RemoteArgosBackend};
pub use tantivy_backend::TantivyBackend;

use crate::db::Settings;

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
}

pub trait SearchBackend: Send + Sync {
    fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>, String>;
    fn preview(&self, hit_id: &str) -> Result<Option<SearchHit>, String>;
}

/// Route search according to `search_mode` in settings.
pub fn run_search(
    settings: &Settings,
    local: &TantivyBackend,
    query: &str,
    limit: usize,
) -> Result<Vec<SearchHit>, String> {
    match settings.search_mode.as_str() {
        "remote" => {
            let remote = RemoteArgosBackend::from_settings(settings)?;
            remote.search(query, limit)
        }
        "hybrid" => {
            let remote = RemoteArgosBackend::from_settings(settings)?;
            hybrid_search(local, &remote, query, limit)
        }
        _ => local.search(query, limit),
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
