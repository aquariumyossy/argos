use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{ConnectInfo, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use tower_http::cors::{AllowOrigin, CorsLayer};

use crate::db::{Db, Settings};
use crate::search::{
    assemble_search_scopes, filter_hits_by_exts, filter_hits_by_path_prefix, filter_out_email_hits,
    run_local_search_multi, run_path_matches, run_preview, RemoteShareSnapshot, ScopeListOpts,
    SearchBackend, SearchHit, SearchScopesResult, TantivyBackend, UserDictMatcher,
    MAX_SEARCH_PREFIXES,
};

#[derive(Clone)]
struct ServerState {
    backend: Arc<TantivyBackend>,
    mail_backend: Arc<TantivyBackend>,
    db: Arc<Db>,
    token: Arc<String>,
    share: Arc<Mutex<RemoteShareSnapshot>>,
    settings: Arc<RwLock<Settings>>,
    user_dict: Arc<RwLock<UserDictMatcher>>,
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
    #[serde(default, rename = "pathPrefixes", alias = "path_prefixes")]
    path_prefixes: Option<Vec<String>>,
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

#[derive(Deserialize)]
struct ScopesQuery {
    query: Option<String>,
}

fn unauthorized() -> (StatusCode, String) {
    (StatusCode::UNAUTHORIZED, "unauthorized".into())
}

fn check_bearer(headers: &HeaderMap, expected: &str) -> Result<(), (StatusCode, String)> {
    let Some(value) = headers.get(header::AUTHORIZATION) else {
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

fn is_loopback_addr(addr: SocketAddr) -> bool {
    addr.ip().is_loopback()
}

fn require_auth(
    headers: &HeaderMap,
    token: &str,
    loopback: bool,
) -> Result<(), (StatusCode, String)> {
    if loopback {
        return Ok(());
    }
    check_bearer(headers, token)
}

fn local_settings(settings: &Settings) -> Settings {
    let mut s = settings.clone();
    s.search_mode = "local".into();
    s
}

fn origin_host_is_loopback(origin: &HeaderValue) -> bool {
    let Ok(raw) = origin.to_str() else {
        return false;
    };
    let Ok(uri) = raw.parse::<axum::http::Uri>() else {
        return false;
    };
    match uri.host() {
        Some("127.0.0.1") | Some("localhost") | Some("::1") => true,
        Some(h) => h.eq_ignore_ascii_case("localhost"),
        None => false,
    }
}

fn loopback_cors() -> CorsLayer {
    CorsLayer::new()
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE])
        .allow_origin(AllowOrigin::predicate(|origin, _parts| {
            origin_host_is_loopback(origin)
        }))
}

fn request_prefixes(body: &SearchRequest) -> Vec<String> {
    let raw = match &body.path_prefixes {
        Some(list) => list.clone(),
        None => body
            .path_prefix
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| vec![s.to_string()])
            .unwrap_or_default(),
    };
    crate::pathutil::collapse_path_prefixes(&raw)
        .into_iter()
        .take(MAX_SEARCH_PREFIXES)
        .collect()
}

fn merge_lan_hits(mut hits: Vec<SearchHit>, limit: usize) -> Vec<SearchHit> {
    hits.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut seen = std::collections::HashSet::new();
    hits.retain(|h| seen.insert(h.path.to_ascii_lowercase()));
    hits.truncate(limit);
    hits
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        ok: true,
        name: "argos",
    })
}

async fn scopes(
    State(state): State<ServerState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(q): Query<ScopesQuery>,
) -> Result<Json<SearchScopesResult>, (StatusCode, String)> {
    let loopback = is_loopback_addr(addr);
    require_auth(&headers, &state.token, loopback)?;
    let settings = local_settings(&state.settings.read());
    let user_dict = state.user_dict.read().clone();
    let db = state.db.clone();
    let backend = state.backend.clone();
    let mail_backend = state.mail_backend.clone();
    let share = if loopback {
        None
    } else {
        Some(state.share.lock().clone())
    };
    let query = q.query.clone();
    let include_mail = loopback;
    let result = tokio::task::spawn_blocking(move || {
        assemble_search_scopes(
            &db,
            query.as_deref(),
            &settings,
            backend.as_ref(),
            Some(mail_backend.as_ref()),
            &user_dict,
            ScopeListOpts {
                include_mail,
                share,
            },
        )
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(result))
}

async fn search(
    State(state): State<ServerState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<SearchRequest>,
) -> Result<Json<SearchResponse>, (StatusCode, String)> {
    let loopback = is_loopback_addr(addr);
    require_auth(&headers, &state.token, loopback)?;
    let prefixes = request_prefixes(&body);
    let limit = body.limit.unwrap_or(10).clamp(1, 50);
    let query = body.query;
    let exts = crate::search::normalize_exts(body.exts);
    let backend = state.backend.clone();
    let mail_backend = state.mail_backend.clone();
    let settings = local_settings(&state.settings.read());
    let user_dict = state.user_dict.read().clone();

    if loopback {
        let hits = tokio::task::spawn_blocking(move || {
            run_local_search_multi(
                &settings,
                backend.as_ref(),
                Some(mail_backend.as_ref()),
                &query,
                &prefixes,
                limit,
                exts.as_deref(),
                &user_dict,
            )
        })
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
        return Ok(Json(SearchResponse { hits }));
    }

    let share = state.share.lock().clone();
    if !share.has_shared_folders() {
        return Ok(Json(SearchResponse { hits: Vec::new() }));
    }
    let fetch_limit = (limit * 4).clamp(1, 50);
    let prefix_for_filter = if prefixes.len() == 1 {
        prefixes.first().cloned()
    } else {
        None
    };
    let searches: Vec<Option<String>> = if prefixes.is_empty() {
        vec![None]
    } else {
        prefixes.into_iter().map(Some).collect()
    };
    let pos_filter = settings.pos_filter_enabled;
    let share_for_search = share.clone();
    let exts_for_search = exts.clone();
    let mut hits = tokio::task::spawn_blocking(move || {
        let mut all = Vec::new();
        for prefix in searches {
            all.extend(backend.search_for_remote(
                &query,
                fetch_limit,
                prefix.as_deref(),
                exts_for_search.as_deref(),
                pos_filter,
                &share_for_search,
            )?);
        }
        Ok::<Vec<SearchHit>, String>(all)
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    hits = filter_hits_by_path_prefix(hits, prefix_for_filter.as_deref());
    hits = filter_hits_by_exts(hits, exts.as_deref());
    hits = filter_out_email_hits(hits);
    hits = share.filter_hits(hits);
    hits = merge_lan_hits(hits, limit);
    for hit in &mut hits {
        hit.source = "remote".into();
    }
    Ok(Json(SearchResponse { hits }))
}

async fn preview(
    State(state): State<ServerState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<PreviewRequest>,
) -> Result<Json<PreviewResponse>, (StatusCode, String)> {
    let loopback = is_loopback_addr(addr);
    require_auth(&headers, &state.token, loopback)?;
    let backend = state.backend.clone();
    let mail_backend = state.mail_backend.clone();
    let id = body.id;
    if loopback {
        let settings = local_settings(&state.settings.read());
        let hit = tokio::task::spawn_blocking(move || {
            run_preview(
                &settings,
                backend.as_ref(),
                Some(mail_backend.as_ref()),
                &id,
            )
        })
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
        return Ok(Json(PreviewResponse { hit }));
    }

    let share = state.share.lock().clone();
    let mut hit = tokio::task::spawn_blocking(move || backend.as_ref().preview(&id))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    if let Some(ref mut h) = hit {
        if h.doc_kind == "email"
            || crate::mail::is_outlook_path(&h.path)
            || !share.path_is_shared(&h.path)
        {
            return Ok(Json(PreviewResponse { hit: None }));
        }
        h.source = "remote".into();
    }
    Ok(Json(PreviewResponse { hit }))
}

#[derive(Deserialize)]
struct PathMatchesRequest {
    query: String,
    path: String,
    limit: Option<usize>,
}

/// Matching units for one file (not aggregated). Used by the client's "show more".
async fn path_matches(
    State(state): State<ServerState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<PathMatchesRequest>,
) -> Result<Json<SearchResponse>, (StatusCode, String)> {
    let loopback = is_loopback_addr(addr);
    require_auth(&headers, &state.token, loopback)?;
    let path = body.path.trim().to_string();
    if path.is_empty() {
        return Ok(Json(SearchResponse { hits: Vec::new() }));
    }

    if loopback {
        let backend = state.backend.clone();
        let mail_backend = state.mail_backend.clone();
        let settings = local_settings(&state.settings.read());
        let user_dict = state.user_dict.read().clone();
        let query = body.query;
        let mut hits = tokio::task::spawn_blocking(move || {
            run_path_matches(
                &settings,
                backend.as_ref(),
                Some(mail_backend.as_ref()),
                &query,
                &path,
                &user_dict,
            )
        })
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
        let limit = body.limit.unwrap_or(50).clamp(1, 50);
        hits.truncate(limit);
        return Ok(Json(SearchResponse { hits }));
    }

    if crate::mail::is_outlook_path(&path) {
        return Ok(Json(SearchResponse { hits: Vec::new() }));
    }
    let share = state.share.lock().clone();
    if !share.path_is_shared(&path) {
        return Ok(Json(SearchResponse { hits: Vec::new() }));
    }
    let limit = body.limit.unwrap_or(50).clamp(1, 50);
    let backend = state.backend.clone();
    let query = body.query;
    let pos_filter = state.settings.read().pos_filter_enabled;
    let mut hits = tokio::task::spawn_blocking(move || {
        backend.matches_for_path(&query, &path, limit, pos_filter)
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    hits = filter_out_email_hits(hits);
    hits = share.filter_hits(hits);
    for hit in &mut hits {
        hit.source = "remote".into();
    }
    Ok(Json(SearchResponse { hits }))
}

fn router(state: ServerState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/scopes", get(scopes))
        .route("/search", post(search))
        .route("/path_matches", post(path_matches))
        .route("/preview", post(preview))
        .layer(loopback_cors())
        .with_state(state)
}

/// Manages the lifecycle of the LAN / local-search HTTP server.
pub struct RemoteServerHandle {
    shutdown: Mutex<Option<oneshot::Sender<()>>>,
    /// Active server identity so unrelated settings saves do not bounce the bind.
    running: Arc<Mutex<Option<RunningServer>>>,
    share: Arc<Mutex<RemoteShareSnapshot>>,
}

struct RunningServer {
    port: u32,
    token: String,
    lan: bool,
}

impl RemoteServerHandle {
    pub fn new() -> Self {
        Self {
            shutdown: Mutex::new(None),
            running: Arc::new(Mutex::new(None)),
            share: Arc::new(Mutex::new(RemoteShareSnapshot::default())),
        }
    }

    pub fn set_share(&self, snap: RemoteShareSnapshot) {
        *self.share.lock() = snap;
    }

    pub fn stop(&self) {
        if let Some(tx) = self.shutdown.lock().take() {
            let _ = tx.send(());
        }
        *self.running.lock() = None;
    }

    /// Apply settings: restart only when LAN/port/token change.
    pub fn sync(
        &self,
        enabled: bool,
        port: u32,
        token: &str,
        backend: Arc<TantivyBackend>,
        mail_backend: Arc<TantivyBackend>,
        db: Arc<Db>,
        settings: Arc<RwLock<Settings>>,
        user_dict: Arc<RwLock<UserDictMatcher>>,
    ) {
        if !(1..=65535).contains(&port) {
            eprintln!("argos: remote server not started (invalid port {port})");
            self.stop();
            return;
        }

        let lan = enabled && !token.trim().is_empty();
        if enabled && token.trim().is_empty() {
            eprintln!("argos: LAN share not started (token empty); local search API on 127.0.0.1");
        }

        {
            let running = self.running.lock();
            if let Some(cfg) = running.as_ref() {
                if cfg.port == port && cfg.token == token && cfg.lan == lan {
                    return;
                }
            }
        }

        self.stop();

        let (tx, rx) = oneshot::channel::<()>();
        *self.shutdown.lock() = Some(tx);
        *self.running.lock() = Some(RunningServer {
            port,
            token: token.to_string(),
            lan,
        });

        let state = ServerState {
            backend,
            mail_backend,
            db,
            token: Arc::new(token.to_string()),
            share: self.share.clone(),
            settings,
            user_dict,
        };
        let app = router(state).into_make_service_with_connect_info::<SocketAddr>();
        let addr = if lan {
            SocketAddr::from(([0, 0, 0, 0], port as u16))
        } else {
            SocketAddr::from(([127, 0, 0, 1], port as u16))
        };
        let running = self.running.clone();
        let expected_port = port;
        let expected_token = token.to_string();
        let expected_lan = lan;
        tauri::async_runtime::spawn(async move {
            let listener = match tokio::net::TcpListener::bind(addr).await {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("argos: remote server bind failed on {addr}: {e}");
                    let mut slot = running.lock();
                    if let Some(cfg) = slot.as_ref() {
                        if cfg.port == expected_port
                            && cfg.token == expected_token
                            && cfg.lan == expected_lan
                        {
                            *slot = None;
                        }
                    }
                    return;
                }
            };
            if lan {
                eprintln!("argos: remote search server listening on http://0.0.0.0:{port}");
            } else {
                eprintln!("argos: local search API listening on http://127.0.0.1:{port}");
            }
            let server = axum::serve(listener, app).with_graceful_shutdown(async {
                let _ = rx.await;
            });
            if let Err(e) = server.await {
                eprintln!("argos: remote server error: {e}");
            } else if lan {
                eprintln!("argos: remote search server stopped");
            } else {
                eprintln!("argos: local search API stopped");
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::extract::ConnectInfo;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    fn loopback_info() -> ConnectInfo<SocketAddr> {
        ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 1)))
    }

    fn lan_info() -> ConnectInfo<SocketAddr> {
        ConnectInfo(SocketAddr::from(([192, 168, 1, 10], 1)))
    }

    fn temp_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "argos-remote-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn test_state(token: &str) -> (std::path::PathBuf, ServerState) {
        let dir = temp_dir();
        let backend = Arc::new(
            TantivyBackend::open(&dir.join("index"))
                .expect("open index")
                .backend,
        );
        let mail_backend = Arc::new(
            TantivyBackend::open_mail(&dir.join("mail"))
                .expect("open mail")
                .backend,
        );
        let db = Arc::new(Db::open(&dir.join("argos.db")).unwrap());
        let state = ServerState {
            backend,
            mail_backend,
            db,
            token: Arc::new(token.to_string()),
            share: Arc::new(Mutex::new(RemoteShareSnapshot::default())),
            settings: Arc::new(RwLock::new(Settings::default())),
            user_dict: Arc::new(RwLock::new(UserDictMatcher::from_words(Vec::<String>::new()))),
        };
        (dir, state)
    }

    async fn oneshot(
        app: Router,
        mut req: Request<Body>,
        info: ConnectInfo<SocketAddr>,
    ) -> axum::http::Response<axum::body::Body> {
        req.extensions_mut().insert(info);
        ServiceExt::oneshot(app, req).await.unwrap()
    }

    async fn body_json(resp: axum::http::Response<axum::body::Body>) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn health_ok_without_auth() {
        let (dir, state) = test_state("secret");
        let app = router(state);
        let resp = oneshot(
            app,
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
            loopback_info(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["ok"], true);
        assert_eq!(v["name"], "argos");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn loopback_scopes_ok_without_auth() {
        let (dir, state) = test_state("");
        let app = router(state);
        let resp = oneshot(
            app,
            Request::builder()
                .uri("/scopes")
                .body(Body::empty())
                .unwrap(),
            loopback_info(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert!(v["recent"].is_array());
        assert!(v["scopes"].is_array());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn loopback_search_path_prefixes_without_auth() {
        let (dir, state) = test_state("");
        let app = router(state);
        let body = serde_json::json!({
            "query": "民法 555条",
            "limit": 8,
            "pathPrefixes": [r"C:\案件A", r"C:\案件B"]
        });
        let resp = oneshot(
            app,
            Request::builder()
                .method("POST")
                .uri("/search")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
            loopback_info(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert!(v["hits"].is_array());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn loopback_search_single_path_prefix_ok() {
        let (dir, state) = test_state("secret");
        let app = router(state);
        let body = serde_json::json!({
            "query": "契約",
            "limit": 5,
            "path_prefix": r"C:\案件A"
        });
        let resp = oneshot(
            app,
            Request::builder()
                .method("POST")
                .uri("/search")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
            loopback_info(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert!(v["hits"].is_array());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn non_loopback_search_without_token_is_401() {
        let (dir, state) = test_state("secret");
        let app = router(state);
        let body = serde_json::json!({ "query": "契約", "limit": 5 });
        let resp = oneshot(
            app,
            Request::builder()
                .method("POST")
                .uri("/search")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
            lan_info(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn non_loopback_scopes_without_token_is_401() {
        let (dir, state) = test_state("secret");
        let app = router(state);
        let resp = oneshot(
            app,
            Request::builder()
                .uri("/scopes")
                .body(Body::empty())
                .unwrap(),
            lan_info(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn lan_disabled_still_serves_loopback_health() {
        let dir = temp_dir();
        let backend = Arc::new(
            TantivyBackend::open(&dir.join("index"))
                .expect("open index")
                .backend,
        );
        let mail_backend = Arc::new(
            TantivyBackend::open_mail(&dir.join("mail"))
                .expect("open mail")
                .backend,
        );
        let db = Arc::new(Db::open(&dir.join("argos.db")).unwrap());
        let settings = Arc::new(RwLock::new(Settings::default()));
        let user_dict = Arc::new(RwLock::new(UserDictMatcher::from_words(Vec::<String>::new())));

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port() as u32;
        drop(listener);

        let handle = RemoteServerHandle::new();
        handle.sync(
            false,
            port,
            "",
            backend,
            mail_backend,
            db,
            settings,
            user_dict,
        );

        let url = format!("http://127.0.0.1:{port}/health");
        let mut last_status = None;
        for _ in 0..40 {
            let url = url.clone();
            let result = tokio::task::spawn_blocking(move || reqwest::blocking::get(&url)).await;
            if let Ok(Ok(resp)) = result {
                last_status = Some(resp.status());
                if resp.status().is_success() {
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        handle.stop();
        assert_eq!(
            last_status.map(|s| s.as_u16()),
            Some(200),
            "loopback /health should be 200 when LAN is off"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
