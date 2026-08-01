use std::time::Duration;

use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};

use crate::db::Settings;

use super::{SearchBackend, SearchHit};

#[derive(Serialize)]
struct SearchRequest<'a> {
    query: &'a str,
    limit: usize,
}

#[derive(Deserialize)]
struct SearchResponse {
    hits: Vec<SearchHit>,
}

#[derive(Serialize)]
struct PreviewRequest<'a> {
    id: &'a str,
}

#[derive(Deserialize)]
struct PreviewResponse {
    hit: Option<SearchHit>,
}

#[derive(Deserialize)]
struct HealthResponse {
    ok: bool,
    #[allow(dead_code)]
    name: Option<String>,
}

pub struct RemoteArgosBackend {
    base_url: String,
    token: String,
    client: Client,
}

impl RemoteArgosBackend {
    pub fn from_settings(settings: &Settings) -> Result<Self, String> {
        let base = settings.remote_url.trim().trim_end_matches('/').to_string();
        if base.is_empty() {
            return Err("リモート URL が未設定です".into());
        }
        if settings.remote_token.trim().is_empty() {
            return Err("リモートトークンが未設定です".into());
        }
        let timeout = Duration::from_millis(settings.remote_timeout_ms.max(500) as u64);
        let client = Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self {
            base_url: base,
            token: settings.remote_token.trim().to_string(),
            client,
        })
    }

    pub fn test_connection(settings: &Settings) -> Result<String, String> {
        let backend = Self::from_settings(settings)?;
        let url = format!("{}/health", backend.base_url);
        let resp = backend
            .client
            .get(&url)
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {}", backend.token),
            )
            .send()
            .map_err(|e| format!("接続失敗: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("接続失敗: HTTP {}", resp.status()));
        }
        let body: HealthResponse = resp
            .json()
            .map_err(|e| format!("応答の解析に失敗: {e}"))?;
        if !body.ok {
            return Err("ヘルスチェックが失敗しました".into());
        }
        Ok(format!("{} に接続できました", backend.base_url))
    }

    fn auth_header(&self) -> String {
        format!("Bearer {}", self.token)
    }
}

impl SearchBackend for RemoteArgosBackend {
    fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>, String> {
        let url = format!("{}/search", self.base_url);
        let resp = self
            .client
            .post(&url)
            .header(reqwest::header::AUTHORIZATION, self.auth_header())
            .json(&SearchRequest { query, limit })
            .send()
            .map_err(|e| format!("リモート検索に失敗: {e}"))?;
        if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err("リモート認証に失敗しました（トークンを確認）".into());
        }
        if !resp.status().is_success() {
            return Err(format!("リモート検索に失敗: HTTP {}", resp.status()));
        }
        let body: SearchResponse = resp
            .json()
            .map_err(|e| format!("リモート検索結果の解析に失敗: {e}"))?;
        let mut hits = body.hits;
        for hit in &mut hits {
            hit.source = "remote".into();
        }
        Ok(hits)
    }

    fn preview(&self, hit_id: &str) -> Result<Option<SearchHit>, String> {
        let url = format!("{}/preview", self.base_url);
        let resp = self
            .client
            .post(&url)
            .header(reqwest::header::AUTHORIZATION, self.auth_header())
            .json(&PreviewRequest { id: hit_id })
            .send()
            .map_err(|e| format!("リモートプレビューに失敗: {e}"))?;
        if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err("リモート認証に失敗しました（トークンを確認）".into());
        }
        if !resp.status().is_success() {
            return Err(format!("リモートプレビューに失敗: HTTP {}", resp.status()));
        }
        let body: PreviewResponse = resp
            .json()
            .map_err(|e| format!("リモートプレビューの解析に失敗: {e}"))?;
        let mut hit = body.hit;
        if let Some(ref mut h) = hit {
            h.source = "remote".into();
        }
        Ok(hit)
    }
}

/// Merge local and remote results: alternate by rank within each source, then trim to limit.
pub fn hybrid_search(
    local: &dyn SearchBackend,
    remote: &RemoteArgosBackend,
    query: &str,
    limit: usize,
) -> Result<Vec<SearchHit>, String> {
    let (local_res, remote_res) = std::thread::scope(|scope| {
        let local_handle = scope.spawn(|| local.search(query, limit));
        let remote_handle = scope.spawn(|| remote.search(query, limit));
        (local_handle.join(), remote_handle.join())
    });

    let local_res = local_res.map_err(|_| "ローカル検索スレッドが失敗しました".to_string())?;
    let remote_res = remote_res.map_err(|_| "リモート検索スレッドが失敗しました".to_string())?;

    let (local_hits, local_err) = match local_res {
        Ok(h) => (h, None),
        Err(e) => {
            eprintln!("argos: hybrid local search failed: {e}");
            (Vec::new(), Some(e))
        }
    };
    let (remote_hits, remote_err) = match remote_res {
        Ok(h) => (h, None),
        Err(e) => {
            eprintln!("argos: hybrid remote search failed: {e}");
            (Vec::new(), Some(e))
        }
    };

    if local_hits.is_empty() && remote_hits.is_empty() {
        if let (Some(le), Some(re)) = (&local_err, &remote_err) {
            return Err(format!("ローカル・リモートとも失敗: {le} / {re}"));
        }
        if let Some(e) = local_err.or(remote_err) {
            return Err(e);
        }
        return Ok(Vec::new());
    }

    let mut out = Vec::with_capacity(limit);
    let mut li = 0usize;
    let mut ri = 0usize;
    while out.len() < limit && (li < local_hits.len() || ri < remote_hits.len()) {
        if li < local_hits.len() {
            out.push(local_hits[li].clone());
            li += 1;
            if out.len() >= limit {
                break;
            }
        }
        if ri < remote_hits.len() {
            out.push(remote_hits[ri].clone());
            ri += 1;
        }
    }
    Ok(out)
}
