use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use crate::search::{
    filter_hits_by_exts, filter_hits_by_path_prefix, SearchBackend, SearchHit, TantivyBackend,
};

#[derive(Clone)]
struct ServerState {
    backend: Arc<TantivyBackend>,
    token: Arc<String>,
    pos_filter_enabled: Arc<std::sync::atomic::AtomicBool>,
}

#[derive(Serialize)]
struct HealthResponse {
    ok: bool,
    name: &'static str,
}

#[derive(Deserialize)]
struct SearchRequest {
    query: String,
    limit: Option<usize>,
    #[serde(default)]
    path_prefix: Option<String>,
    #[serde(default)]
    exts: Option<Vec<String>>,
}

#[derive(Serialize)]
struct SearchResponse {
    hits: Vec<SearchHit>,
}

#[derive(Deserialize)]
struct PreviewRequest {
    id: String,
}

#[derive(Serialize)]
struct PreviewResponse {
    hit: Option<SearchHit>,
}

fn unauthorized() -> (StatusCode, String) {
    (StatusCode::UNAUTHORIZED, "unauthorized".into())
}

fn check_bearer(headers: &HeaderMap, expected: &str) -> Result<(), (StatusCode, String)> {
    let Some(value) = headers.get(axum::http::header::AUTHORIZATION) else {
        return Err(unauthorized());
    };
    let Ok(raw) = value.to_str() else {
        return Err(unauthorized());
    };
    let Some(token) = raw.strip_prefix("Bearer ").or_else(|| raw.strip_prefix("bearer ")) else {
        return Err(unauthorized());
    };
    if token != expected || expected.is_empty() {
        return Err(unauthorized());
    }
    Ok(())
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        ok: true,
        name: "argos",
    })
}

async fn search(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(body): Json<SearchRequest>,
) -> Result<Json<SearchResponse>, (StatusCode, String)> {
    check_bearer(&headers, &state.token)?;
    let limit = body.limit.unwrap_or(10).clamp(1, 50);
    let backend = state.backend.clone();
    let query = body.query;
    let path_prefix = body
        .path_prefix
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let exts = crate::search::normalize_exts(body.exts);
    // Over-fetch when scoped so post-filter can still fill limit.
    let fetch_limit = if path_prefix.is_some() || exts.is_some() {
        (limit * 4).clamp(1, 50)
    } else {
        limit
    };
    let prefix_for_filter = path_prefix.clone();
    let exts_for_filter = exts.clone();
    let pos_filter = state
        .pos_filter_enabled
        .load(std::sync::atomic::Ordering::Relaxed);
    let mut hits = tauri::async_runtime::spawn_blocking(move || {
        backend.search(
            &query,
            fetch_limit,
            path_prefix.as_deref(),
            exts.as_deref(),
            pos_filter,
        )
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    // Safety net if backend ignored scope (should not happen for local Tantivy).
    hits = filter_hits_by_path_prefix(hits, prefix_for_filter.as_deref());
    hits = filter_hits_by_exts(hits, exts_for_filter.as_deref());
    hits = crate::search::filter_out_email_hits(hits);
    hits.truncate(limit);
    for hit in &mut hits {
        hit.source = "remote".into();
    }
    Ok(Json(SearchResponse { hits }))
}

async fn preview(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(body): Json<PreviewRequest>,
) -> Result<Json<PreviewResponse>, (StatusCode, String)> {
    check_bearer(&headers, &state.token)?;
    let backend = state.backend.clone();
    let id = body.id;
    let mut hit = tauri::async_runtime::spawn_blocking(move || backend.preview(&id))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    if let Some(ref mut h) = hit {
        if h.doc_kind == "email" || crate::mail::is_outlook_path(&h.path) {
            return Ok(Json(PreviewResponse { hit: None }));
        }
        h.source = "remote".into();
    }
    Ok(Json(PreviewResponse { hit }))
}

/// Manages the lifecycle of the LAN search HTTP server.
pub struct RemoteServerHandle {
    shutdown: Mutex<Option<oneshot::Sender<()>>>,
}

impl RemoteServerHandle {
    pub fn new() -> Self {
        Self {
            shutdown: Mutex::new(None),
        }
    }

    pub fn stop(&self) {
        if let Some(tx) = self.shutdown.lock().take() {
            let _ = tx.send(());
        }
    }

    /// Apply settings: stop existing server, start if enabled.
    pub fn sync(
        &self,
        enabled: bool,
        port: u32,
        token: &str,
        backend: Arc<TantivyBackend>,
        pos_filter_enabled: bool,
    ) {
        self.stop();
        if !enabled {
            return;
        }
        if token.trim().is_empty() {
            eprintln!("argos: remote server not started (token empty)");
            return;
        }
        if !(1..=65535).contains(&port) {
            eprintln!("argos: remote server not started (invalid port {port})");
            return;
        }

        let (tx, rx) = oneshot::channel::<()>();
        *self.shutdown.lock() = Some(tx);

        let state = ServerState {
            backend,
            token: Arc::new(token.to_string()),
            pos_filter_enabled: Arc::new(std::sync::atomic::AtomicBool::new(
                pos_filter_enabled,
            )),
        };
        let app = Router::new()
            .route("/health", get(health))
            .route("/search", post(search))
            .route("/preview", post(preview))
            .with_state(state);

        let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port as u16));
        tauri::async_runtime::spawn(async move {
            let listener = match tokio::net::TcpListener::bind(addr).await {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("argos: remote server bind failed on {addr}: {e}");
                    return;
                }
            };
            eprintln!("argos: remote search server listening on http://0.0.0.0:{port}");
            let server = axum::serve(listener, app).with_graceful_shutdown(async {
                let _ = rx.await;
            });
            if let Err(e) = server.await {
                eprintln!("argos: remote server error: {e}");
            } else {
                eprintln!("argos: remote search server stopped");
            }
        });
    }
}

impl Default for RemoteServerHandle {
    fn default() -> Self {
        Self::new()
    }
}

/// Best-effort LAN IPv4 for connection hints in the settings UI.
pub fn guess_lan_ip() -> Option<String> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    let ip = socket.local_addr().ok()?.ip();
    if ip.is_loopback() {
        return None;
    }
    Some(ip.to_string())
}
