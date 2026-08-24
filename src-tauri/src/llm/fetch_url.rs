//! Fetch a user-specified or tool-requested http(s) page and turn it into text.

use std::io::{ErrorKind, Read};
use std::net::{IpAddr, ToSocketAddrs};
use std::time::Duration;

use reqwest::blocking::Client;
use reqwest::redirect::{Attempt, Policy};
use reqwest::Url;

use crate::db::Settings;
use crate::extractor::{
    decode_html_bytes_with_charset, extract_pdf_pages_from_bytes, html_to_text,
};
use crate::llm::searxng;
use crate::state::AppState;

pub const PASTE_URL_MAX: usize = 3;
pub const FETCH_BODY_CAP: usize = 12_000;
const MAX_BYTES: u64 = 5_000_000;
const MAX_REDIRECTS: usize = 5;
const THIN_BODY_CHARS: usize = 80;
const PAGE_USER_AGENT: &str = "Mozilla/5.0 (compatible; Argos)";
const ACCEPT: &str = "text/html,application/xhtml+xml,application/pdf,text/plain;q=0.9,*/*;q=0.1";
const TRUNCATED_MARK: &str = " […]";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FetchAccess {
    /// URLs the user pasted. RFC1918 is allowed.
    UserPaste,
    /// Model `read_url`. RFC1918 is denied.
    Tool,
}

#[derive(Debug, Clone)]
pub struct FetchedPage {
    pub url: String,
    pub title: String,
    pub body: String,
    pub thin: bool,
}

#[derive(Debug, Default)]
pub struct PasteAttachResult {
    pub attached: usize,
    pub leftover: usize,
    pub failures: Vec<String>,
    pub thin_notes: Vec<String>,
}

impl PasteAttachResult {
    pub fn system_line(&self) -> Option<String> {
        if self.attached == 0 && self.leftover == 0 && self.failures.is_empty() {
            return None;
        }
        let mut s = String::new();
        if self.attached > 0 {
            s.push_str(
                "\nユーザーがメッセージに貼った URL の本文は出典に付いています。read_url で取り直さなくてよいです。",
            );
        }
        if self.leftover > 0 {
            s.push_str(&format!(
                "\n貼られた URL のうち {} 件は件数上限のため読んでいません。",
                self.leftover
            ));
        }
        if !self.failures.is_empty() {
            s.push_str("\n次の URL は本文を取得できませんでした: ");
            s.push_str(&self.failures.join(" / "));
        }
        for note in &self.thin_notes {
            s.push('\n');
            s.push_str(note);
        }
        Some(s)
    }

    pub fn warning_line(&self) -> Option<String> {
        if self.failures.is_empty() {
            return None;
        }
        Some(format!(
            "次の URL は本文を取得できませんでした: {}",
            self.failures.join(" / ")
        ))
    }
}

pub fn message_may_contain_url(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("http://") || lower.contains("https://")
}

/// Unique http(s) URLs in document order, then how many valid extras were dropped.
pub fn extract_http_urls(text: &str, max: usize) -> (Vec<String>, usize) {
    let mut found = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let lower = text.to_ascii_lowercase();
    let mut from = 0usize;
    while from < text.len() {
        let rest = &lower[from..];
        let http = rest.find("http://").map(|i| (i, 7));
        let https = rest.find("https://").map(|i| (i, 8));
        let (rel, scheme_len) = match (http, https) {
            (Some((a, la)), Some((b, lb))) => {
                if a <= b {
                    (a, la)
                } else {
                    (b, lb)
                }
            }
            (Some(p), None) | (None, Some(p)) => p,
            (None, None) => break,
        };
        let start = from + rel;
        if !url_start_ok(text, start) {
            from = start + scheme_len;
            continue;
        }
        let after_scheme = start + scheme_len;
        let end = scan_url_end(text, after_scheme);
        if end <= after_scheme {
            from = after_scheme;
            continue;
        }
        let raw = strip_trailing_punct(&text[start..end]);
        from = end;
        if let Some(url) = normalize_extracted_url(raw) {
            let key = url.to_ascii_lowercase();
            if seen.insert(key) {
                found.push(url);
            }
        }
    }
    if found.len() <= max {
        (found, 0)
    } else {
        let leftover = found.len() - max;
        found.truncate(max);
        (found, leftover)
    }
}

fn url_start_ok(text: &str, start: usize) -> bool {
    if start == 0 {
        return true;
    }
    let Some(prev) = text[..start].chars().next_back() else {
        return true;
    };
    prev.is_whitespace()
        || matches!(
            prev,
            '(' | '[' | '{' | '<' | '"' | '\'' | '「' | '『' | '（' | '【'
        )
}

fn scan_url_end(text: &str, from: usize) -> usize {
    let mut end = from;
    for (i, ch) in text[from..].char_indices() {
        if ch.is_whitespace() || matches!(ch, '<' | '>' | '"' | '\'' | '「' | '『' | '」' | '』') {
            return from + i;
        }
        end = from + i + ch.len_utf8();
    }
    end
}

fn strip_trailing_punct(raw: &str) -> &str {
    raw.trim_end_matches(|c: char| {
        matches!(
            c,
            '。' | '、'
                | '．'
                | '.'
                | ','
                | ';'
                | ':'
                | '!'
                | '?'
                | ')'
                | ']'
                | '}'
                | '>'
                | '」'
                | '』'
                | '）'
                | '】'
        )
    })
}

fn normalize_extracted_url(raw: &str) -> Option<String> {
    let t = raw.trim();
    if !searxng::is_http_url(t) {
        return None;
    }
    let parsed = Url::parse(t).ok()?;
    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return None;
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return None;
    }
    Some(t.to_string())
}

pub fn check_url(url: &Url, access: FetchAccess) -> Result<(), String> {
    let scheme = url.scheme();
    if scheme != "http" && scheme != "https" {
        return Err("http(s) の URL だけ読めます。".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("ユーザー名やパスワード付きの URL は読めません。".into());
    }
    let host = url.host_str().ok_or_else(|| "ホストがありません。".to_string())?;
    check_host(host, access)
}

pub fn check_host(host: &str, access: FetchAccess) -> Result<(), String> {
    let h = host
        .trim()
        .trim_matches(|c| c == '[' || c == ']')
        .to_ascii_lowercase();
    if h.is_empty() {
        return Err("ホストがありません。".into());
    }
    if h == "localhost" || h.ends_with(".localhost") || h == "metadata.google.internal" {
        return Err("このホストは読めません。".into());
    }
    if let Ok(ip) = h.parse::<IpAddr>() {
        return check_ip(ip, access);
    }
    Ok(())
}

fn check_ip(ip: IpAddr, access: FetchAccess) -> Result<(), String> {
    if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() {
        return Err("このホストは読めません。".into());
    }
    match ip {
        IpAddr::V4(v) => {
            if v.is_link_local() || v.is_broadcast() {
                return Err("このホストは読めません。".into());
            }
            if v.octets()[0] == 0 {
                return Err("このホストは読めません。".into());
            }
            if v.is_private() && access == FetchAccess::Tool {
                return Err(
                    "ローカルネットワークの URL は、メッセージに貼ったときだけ読めます。".into(),
                );
            }
        }
        IpAddr::V6(v) => {
            if let Some(v4) = v.to_ipv4_mapped() {
                return check_ip(IpAddr::V4(v4), access);
            }
            if v.is_unicast_link_local() || v.is_unique_local() {
                return Err("このホストは読めません。".into());
            }
        }
    }
    Ok(())
}

fn resolve_and_check(url: &Url, access: FetchAccess) -> Result<(), String> {
    check_url(url, access)?;
    let host = url
        .host_str()
        .ok_or_else(|| "ホストがありません。".to_string())?;
    if host.parse::<IpAddr>().is_ok() {
        return Ok(());
    }
    let port = url.port_or_known_default().unwrap_or(80);
    let addrs = (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("名前を解決できません（{e}）。"))?;
    let mut any = false;
    for addr in addrs {
        any = true;
        check_ip(addr.ip(), access)?;
    }
    if !any {
        return Err("名前を解決できません。".into());
    }
    Ok(())
}

fn cap_chars(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push_str(TRUNCATED_MARK);
    out
}

pub fn charset_from_content_type(ct: &str) -> Option<String> {
    let lower = ct.to_ascii_lowercase();
    let i = lower.find("charset=")?;
    let rest = ct[i + 8..].trim();
    let rest = rest.trim_start_matches(['"', '\'']);
    let end = rest
        .find(|c: char| c == ';' || c == '"' || c == '\'' || c.is_whitespace())
        .unwrap_or(rest.len());
    let label = rest[..end].trim();
    if label.is_empty() {
        None
    } else {
        Some(label.to_string())
    }
}

pub fn looks_like_pdf(bytes: &[u8]) -> bool {
    bytes.starts_with(b"%PDF")
}

pub fn looks_like_html(bytes: &[u8]) -> bool {
    let i = bytes
        .iter()
        .position(|&b| !b.is_ascii_whitespace())
        .unwrap_or(0);
    bytes.get(i) == Some(&b'<')
}

pub fn extract_page_bytes(
    bytes: &[u8],
    content_type: &str,
    url: &str,
    charset: Option<&str>,
) -> Result<(Option<String>, String), String> {
    let ct = content_type.to_ascii_lowercase();
    let path_is_pdf = url
        .split('?')
        .next()
        .unwrap_or(url)
        .to_ascii_lowercase()
        .ends_with(".pdf");
    if ct.starts_with("image/") || ct.starts_with("audio/") || ct.starts_with("video/") {
        return Err("この形式は読めません。".into());
    }
    if looks_like_pdf(bytes) || ct.contains("application/pdf") || (path_is_pdf && !looks_like_html(bytes))
    {
        let pages = extract_pdf_pages_from_bytes(bytes)?;
        return Ok((None, pages.join("\n\n")));
    }
    if ct.contains("text/plain") && !ct.contains("html") {
        let text = String::from_utf8_lossy(bytes).trim().to_string();
        if text.is_empty() {
            return Err("本文がありません。".into());
        }
        return Ok((None, text));
    }
    if looks_like_html(bytes)
        || ct.contains("text/html")
        || ct.contains("application/xhtml")
        || ct.contains("octet-stream")
        || ct.is_empty()
    {
        if !looks_like_html(bytes) && !ct.contains("html") && !ct.contains("xhtml") {
            return Err("この形式は読めません。".into());
        }
        let decoded = decode_html_bytes_with_charset(bytes, charset);
        let (title, text) = html_to_text(&decoded);
        if !text.chars().any(|c| !c.is_whitespace()) {
            return Err("本文がありません。".into());
        }
        return Ok((title, text));
    }
    Err("この形式は読めません。".into())
}

fn timeout_ms(settings: &Settings) -> u64 {
    settings.searxng_timeout_ms.clamp(5_000, 30_000) as u64
}

fn io_denied(msg: String) -> std::io::Error {
    std::io::Error::new(ErrorKind::PermissionDenied, msg)
}

pub fn fetch_page(
    settings: &Settings,
    raw_url: &str,
    access: FetchAccess,
) -> Result<FetchedPage, String> {
    let parsed = Url::parse(raw_url.trim()).map_err(|_| "URL が正しくありません。".to_string())?;
    resolve_and_check(&parsed, access)?;
    let access_copy = access;
    let client = Client::builder()
        .user_agent(PAGE_USER_AGENT)
        .timeout(Duration::from_millis(timeout_ms(settings)))
        .redirect(Policy::custom(move |attempt: Attempt| {
            if attempt.previous().len() >= MAX_REDIRECTS {
                return attempt.error(io_denied("リダイレクトが多すぎます。".into()));
            }
            match resolve_and_check(attempt.url(), access_copy) {
                Ok(()) => attempt.follow(),
                Err(e) => attempt.error(io_denied(e)),
            }
        }))
        .build()
        .map_err(|e| format!("HTTP クライアントを作れません（{e}）。"))?;
    let resp = client
        .get(parsed.clone())
        .header(reqwest::header::ACCEPT, ACCEPT)
        .send()
        .map_err(|e| format!("ページを取得できません（{e}）。"))?;
    let status = resp.status().as_u16();
    if status == 401 || status == 403 {
        return Err("ログインが必要なページか、取得が拒否されました。".into());
    }
    if !(200..300).contains(&status) {
        return Err(format!("HTTP {status} が返りました。"));
    }
    if let Some(len) = resp.content_length() {
        if len > MAX_BYTES {
            return Err("ファイルが大きすぎます。".into());
        }
    }
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let charset = charset_from_content_type(&content_type);
    let mut limited = resp.take(MAX_BYTES + 1);
    let mut bytes = Vec::new();
    limited
        .read_to_end(&mut bytes)
        .map_err(|e| format!("本文を読めません（{e}）。"))?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("ファイルが大きすぎます。".into());
    }
    let (title, text) = extract_page_bytes(
        &bytes,
        &content_type,
        raw_url,
        charset.as_deref(),
    )?;
    let body = cap_chars(text.trim(), FETCH_BODY_CAP);
    if body.is_empty() {
        return Err("本文がありません。".into());
    }
    let thin = body.chars().count() < THIN_BODY_CHARS;
    let title = title
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| raw_url.trim().to_string());
    Ok(FetchedPage {
        url: raw_url.trim().to_string(),
        title,
        body,
        thin,
    })
}

pub fn attach_pasted_urls(
    state: &AppState,
    thread_id: &str,
    message: &str,
) -> PasteAttachResult {
    let (urls, leftover) = extract_http_urls(message, PASTE_URL_MAX);
    let mut out = PasteAttachResult {
        leftover,
        ..PasteAttachResult::default()
    };
    if urls.is_empty() {
        return out;
    }
    let mut to_fetch = Vec::new();
    for url in urls {
        match state.db.find_llm_source_by_path(thread_id, &url) {
            Ok(Some(_)) => {}
            Ok(None) => to_fetch.push(url),
            Err(e) => out.failures.push(format!("{url}: {e}")),
        }
    }
    if to_fetch.is_empty() {
        return out;
    }
    let settings = state.settings.read().clone();
    let handles: Vec<_> = to_fetch
        .into_iter()
        .map(|url| {
            let settings = settings.clone();
            std::thread::spawn(move || {
                let result = fetch_page(&settings, &url, FetchAccess::UserPaste);
                (url, result)
            })
        })
        .collect();
    for handle in handles {
        match handle.join() {
            Ok((url, Ok(page))) => {
                if page.thin {
                    out.thin_notes.push(format!(
                        "{url}: 本文がほとんど取れませんでした（JavaScript で描画されている可能性）。"
                    ));
                }
                match state.db.insert_llm_source_full(
                    thread_id,
                    "attach",
                    &url,
                    &page.title,
                    "",
                    &page.body,
                    "",
                    "unit",
                    "web",
                    "",
                    "",
                    None,
                ) {
                    Ok(_) => out.attached += 1,
                    Err(e) => out.failures.push(format!("{url}: {e}")),
                }
            }
            Ok((url, Err(e))) => out.failures.push(format!("{url}: {e}")),
            Err(_) => out.failures.push("取得スレッドが失敗しました。".into()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(raw: &str) -> Url {
        Url::parse(raw).unwrap()
    }

    #[test]
    fn extract_strips_punct_dedups_and_caps() {
        let text = "見て https://example.com/a。 と https://example.com/a と https://example.com/b, と https://example.com/c と https://example.com/d";
        let (urls, leftover) = extract_http_urls(text, 3);
        assert_eq!(
            urls,
            vec![
                "https://example.com/a".to_string(),
                "https://example.com/b".to_string(),
                "https://example.com/c".to_string()
            ]
        );
        assert_eq!(leftover, 1);
    }

    #[test]
    fn extract_from_markdown_and_rejects_userinfo() {
        let text = "リンクは [x](https://example.com/p) です。 http://user:pass@example.com/secret はダメ。";
        let (urls, leftover) = extract_http_urls(text, 8);
        assert_eq!(urls, vec!["https://example.com/p".to_string()]);
        assert_eq!(leftover, 0);
    }

    #[test]
    fn host_blocks_loopback_link_local_metadata_ula() {
        for raw in [
            "http://localhost/",
            "http://127.0.0.1/",
            "http://[::1]/",
            "http://169.254.1.1/",
            "http://169.254.169.254/",
            "http://[fe80::1]/",
            "http://[fd12:3456:789a::1]/",
            "http://0.0.0.0/",
        ] {
            assert!(
                check_url(&parsed(raw), FetchAccess::UserPaste).is_err(),
                "should block {raw}"
            );
        }
        assert!(check_url(&parsed("https://example.com/a"), FetchAccess::UserPaste).is_ok());
        assert!(check_url(&parsed("http://192.168.1.8/doc"), FetchAccess::UserPaste).is_ok());
        assert!(check_url(&parsed("http://192.168.1.8/doc"), FetchAccess::Tool).is_err());
        assert!(check_url(&parsed("http://10.0.0.2/"), FetchAccess::Tool).is_err());
        assert!(check_url(
            &parsed("http://user:pass@example.com/"),
            FetchAccess::UserPaste
        )
        .is_err());
    }

    #[test]
    fn html_bytes_to_title_and_body() {
        let html = "<html><head><title>判決</title></head><body><p>主文は棄却する。</p></body></html>";
        let (title, body) = extract_page_bytes(
            html.as_bytes(),
            "text/html; charset=utf-8",
            "https://example.jp/a",
            Some("utf-8"),
        )
            .unwrap();
        assert_eq!(title.as_deref(), Some("判決"));
        assert!(body.contains("主文は棄却する"));
    }

    #[test]
    fn pdf_magic_is_detected() {
        assert!(looks_like_pdf(b"%PDF-1.7 rest"));
        assert!(!looks_like_pdf(b"<html>"));
        assert!(looks_like_html(b"  \n<!DOCTYPE html>"));
    }

    #[test]
    fn octet_stream_html_is_extracted() {
        let html = b"<html><head><title>T</title></head><body>hello world body text</body></html>";
        let (title, body) =
            extract_page_bytes(html, "application/octet-stream", "https://example.com/x", None)
                .unwrap();
        assert_eq!(title.as_deref(), Some("T"));
        assert!(body.contains("hello world"));
    }
}
