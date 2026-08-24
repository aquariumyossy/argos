//! SearXNG JSON search for the chat web-search sidecar.

use std::collections::HashSet;
use std::time::Duration;

use reqwest::blocking::Client;
use serde::Deserialize;

use crate::db::Settings;

pub const DEFAULT_TIMEOUT_MS: u32 = 8_000;
pub const DEFAULT_TOP_K: u32 = 5;
pub const TOP_K_MAX: u32 = 8;
const QUERY_CHAR_CAP: usize = 400;
const USER_AGENT: &str = "Argos";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebHit {
    pub title: String,
    pub url: String,
    pub content: String,
}

#[derive(Deserialize)]
struct SearchResponse {
    #[serde(default)]
    results: Vec<RawResult>,
}

#[derive(Deserialize)]
struct RawResult {
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    content: String,
}

/// Strip `/search`, query string, fragment, and a trailing slash.
pub fn normalize_base_url(raw: &str) -> String {
    let mut s = raw.trim().to_string();
    if s.is_empty() {
        return s;
    }
    if let Some(i) = s.find('#') {
        s.truncate(i);
    }
    if let Some(scheme) = s.find("://") {
        let after = scheme + 3;
        if let Some(rel) = s[after..].find(['?', '#']) {
            s.truncate(after + rel);
        }
    } else if let Some(i) = s.find('?') {
        s.truncate(i);
    }
    s = s.trim().trim_end_matches('/').to_string();
    let lower = s.to_ascii_lowercase();
    if lower.ends_with("/search") {
        s.truncate(s.len() - "/search".len());
        s = s.trim_end_matches('/').to_string();
    }
    s
}

pub fn is_http_url(raw: &str) -> bool {
    let t = raw.trim();
    if t.is_empty() || t.chars().any(char::is_whitespace) {
        return false;
    }
    let lower = t.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

pub fn cap_query(q: &str) -> String {
    let t = q.trim();
    if t.chars().count() <= QUERY_CHAR_CAP {
        return t.to_string();
    }
    t.chars().take(QUERY_CHAR_CAP).collect()
}

pub fn parse_results_json(text: &str) -> Result<Vec<WebHit>, String> {
    let parsed: SearchResponse = serde_json::from_str(text).map_err(|_| {
        "SearXNG の応答が JSON ではありません。format=json が有効か確認してください。".to_string()
    })?;
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for raw in parsed.results {
        let url = raw.url.trim().to_string();
        if !is_http_url(&url) {
            continue;
        }
        let key = url.to_ascii_lowercase();
        if !seen.insert(key) {
            continue;
        }
        let title = raw.title.trim().to_string();
        out.push(WebHit {
            title: if title.is_empty() {
                url.clone()
            } else {
                title
            },
            url,
            content: raw.content.trim().to_string(),
        });
    }
    Ok(out)
}

pub fn interpret_http_body(status: u16, content_type: &str, body: &str) -> Result<Vec<WebHit>, String> {
    if status == 403 {
        return Err(
            "SearXNG が JSON を拒否しました。サーバ側の search.formats に json を許可してください。"
                .into(),
        );
    }
    if !(200..300).contains(&status) {
        return Err(format!("SearXNG が HTTP {status} を返しました。"));
    }
    let ct = content_type.to_ascii_lowercase();
    let trimmed = body.trim_start();
    if ct.contains("text/html") || trimmed.starts_with('<') {
        return Err(
            "HTML が返りました。URL に format=json が付いているか、JSON が有効か確認してください。"
                .into(),
        );
    }
    parse_results_json(body)
}

fn client(settings: &Settings) -> Result<Client, String> {
    let timeout = Duration::from_millis(
        settings
            .searxng_timeout_ms
            .clamp(5_000, 30_000) as u64,
    );
    Client::builder()
        .timeout(timeout)
        .user_agent(USER_AGENT)
        .build()
        .map_err(|e| e.to_string())
}

fn get_json(settings: &Settings, query: &str) -> Result<Vec<WebHit>, String> {
    let base = normalize_base_url(&settings.searxng_url);
    if base.is_empty() {
        return Err("SearXNG の URL が未設定です。設定のローカルLLMで入れてください。".into());
    }
    let q = cap_query(query);
    if q.is_empty() {
        return Ok(Vec::new());
    }
    let url = format!("{}/search", base.trim_end_matches('/'));
    let resp = client(settings)?
        .get(&url)
        .query(&[("q", q.as_str()), ("format", "json"), ("language", "ja-JP")])
        .send()
        .map_err(|e| format!("SearXNG に接続できません（{e}）。"))?;
    let status = resp.status().as_u16();
    let ct = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let body = resp
        .text()
        .map_err(|e| format!("SearXNG の応答を読めません（{e}）。"))?;
    interpret_http_body(status, &ct, &body)
}

pub fn search(settings: &Settings, query: &str) -> Result<Vec<WebHit>, String> {
    let k = settings.llm_web_search_top_k.clamp(1, TOP_K_MAX) as usize;
    let mut hits = get_json(settings, query)?;
    if hits.len() > k {
        hits.truncate(k);
    }
    Ok(hits)
}

pub fn test_connection(settings: &Settings) -> Result<String, String> {
    let hits = get_json(settings, "argos")?;
    Ok(format!(
        "SearXNG に接続できました（JSON {}件）。",
        hits.len()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_search_path_and_query() {
        assert_eq!(
            normalize_base_url("http://192.168.0.1:8080/search?q=調べたい語&format=json"),
            "http://192.168.0.1:8080"
        );
        assert_eq!(
            normalize_base_url("http://192.168.0.1:8080/search/"),
            "http://192.168.0.1:8080"
        );
        assert_eq!(
            normalize_base_url("http://192.168.0.1:8080/"),
            "http://192.168.0.1:8080"
        );
        assert_eq!(normalize_base_url("  "), "");
    }

    #[test]
    fn parse_title_url_content_and_dedup() {
        let json = r#"{
            "results": [
                {"title": "A", "url": "https://example.com/a", "content": "alpha"},
                {"title": "A2", "url": "https://example.com/a", "content": "dup"},
                {"title": "", "url": "https://example.com/b", "content": "beta"},
                {"title": "bad", "url": "javascript:alert(1)", "content": "no"},
                {"title": "ftp", "url": "ftp://files.example/x", "content": "no"}
            ]
        }"#;
        let hits = parse_results_json(json).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].title, "A");
        assert_eq!(hits[0].url, "https://example.com/a");
        assert_eq!(hits[0].content, "alpha");
        assert_eq!(hits[1].title, "https://example.com/b");
        assert_eq!(hits[1].content, "beta");
    }

    #[test]
    fn html_and_403_have_actionable_errors() {
        let html = interpret_http_body(200, "text/html", "<html>nope</html>").unwrap_err();
        assert!(html.contains("HTML"), "{html}");
        assert!(html.contains("format=json"), "{html}");
        let forbidden = interpret_http_body(403, "application/json", "{}").unwrap_err();
        assert!(forbidden.contains("search.formats"), "{forbidden}");
        let tagged = interpret_http_body(200, "application/json", "<!doctype html>").unwrap_err();
        assert!(tagged.contains("HTML"), "{tagged}");
    }

    #[test]
    fn cap_query_truncates() {
        let long: String = "あ".repeat(QUERY_CHAR_CAP + 10);
        assert_eq!(cap_query(&long).chars().count(), QUERY_CHAR_CAP);
        assert_eq!(cap_query("  民法 555  "), "民法 555");
    }
}
