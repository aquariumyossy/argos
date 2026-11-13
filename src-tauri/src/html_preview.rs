//! Isolated HTML preview.
//!
//! The file is read from the office output folder and served by the `htmlpreview`
//! protocol. Chat embeds that URL in a sandboxed iframe. 「大きく表示」 opens a
//! separate window whose top document is a shell we control; the calculator runs
//! only inside the same sandboxed frame.
//!
//! Network is refused by one Content-Security-Policy (response header, iframe
//! `csp` attribute, and a meta tag). The frame's sandbox does not include
//! `allow-same-origin`, so its script cannot read the app window.
//!
//! On Windows, WebView2 runs Tauri's IPC script in every frame. A document-start
//! script therefore drops `chrome.webview.postMessage` in child frames and on
//! this preview origin, before the page's own script runs.

use std::path::Path;

use tauri::webview::WebviewWindowBuilder;
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindow, WindowEvent};

use crate::llm::create_office::output_file_path;
use crate::state::AppState;

pub const WINDOW_LABEL: &str = "html-preview";
pub const PREVIEW_SANDBOX: &str = "allow-scripts";

/// Kept in step with `PREVIEW_CSP` in `src/features/chat/htmlPreviewPolicy.ts`.
pub const PREVIEW_CSP: &str = concat!(
    "default-src 'none'; script-src 'unsafe-inline' 'unsafe-eval'; style-src 'unsafe-inline'; ",
    "img-src data:; font-src data:; media-src data:; connect-src 'none'; ",
    "form-action 'none'; frame-src 'none'; child-src 'none'; object-src 'none'; ",
    "base-uri 'none'; worker-src 'none'; manifest-src 'none'; sandbox allow-scripts"
);

const SHELL_CSP: &str = concat!(
    "default-src 'none'; script-src 'none'; style-src 'unsafe-inline'; ",
    "frame-src 'self'; child-src 'self'; object-src 'none'; base-uri 'none'; ",
    "form-action 'none'; connect-src 'none'; img-src 'none'; font-src 'none'; ",
    "worker-src 'none'"
);

const MAX_PREVIEW_BYTES: u64 = 512 * 1024;

const LOCK_JS: &str = r#"
function lockHtmlPreview() {
  function deny() { throw new Error("blocked"); }
  function denyPromise() { return Promise.reject(new Error("blocked")); }
  function noop() {}
  function lock(obj, key, value) {
    if (!obj) return;
    try {
      Object.defineProperty(obj, key, { configurable: false, writable: false, value: value });
    } catch (e) {
      try { obj[key] = value; } catch (e2) {}
    }
  }
  lock(window, "fetch", denyPromise);
  lock(window, "XMLHttpRequest", deny);
  lock(window, "WebSocket", deny);
  lock(window, "EventSource", deny);
  lock(window, "Worker", deny);
  lock(window, "SharedWorker", deny);
  lock(window, "open", deny);
  lock(window, "RTCPeerConnection", deny);
  lock(window, "webkitRTCPeerConnection", deny);
  lock(window, "RTCDataChannel", deny);
  lock(window, "RTCSessionDescription", deny);
  lock(window, "RTCIceCandidate", deny);
  try { lock(navigator, "sendBeacon", function () { return false; }); } catch (e) {}
  var deadView = { postMessage: noop, addEventListener: noop, removeEventListener: noop };
  try { lock(window, "chrome", { webview: deadView }); } catch (e) {}
  try {
    var chromeObj = window.chrome;
    if (chromeObj) {
      lock(chromeObj, "webview", deadView);
      if (chromeObj.webview) {
        lock(chromeObj.webview, "postMessage", noop);
        lock(chromeObj.webview, "addEventListener", noop);
        lock(chromeObj.webview, "removeEventListener", noop);
      }
    }
  } catch (e) {}
  try { lock(window, "ipc", Object.freeze({ postMessage: noop })); } catch (e) {}
  try {
    var internals = window.__TAURI_INTERNALS__;
    if (internals) {
      lock(internals, "invoke", denyPromise);
      lock(internals, "ipc", noop);
      lock(internals, "transformCallback", function () { return 0; });
      lock(internals, "convertFileSrc", function () { return ""; });
    }
  } catch (e) {}
  try {
    if (window.navigation && window.navigation.addEventListener) {
      window.navigation.addEventListener("navigate", function (event) {
        try {
          var here = String(window.location.href);
          var next = event.destination && String(event.destination.url || "");
          if (next && next !== here && event.cancelable) event.preventDefault();
        } catch (err) {}
      });
    }
  } catch (e) {}
}
lockHtmlPreview();
"#;

/// Runs in every frame. The app's own top document is left alone.
pub fn isolation_script() -> String {
    format!(
        r#"
try {{
  var href = "";
  try {{ href = String(window.location.href || ""); }} catch (e) {{}}
  var preview = href.indexOf("htmlpreview.localhost") !== -1 || href.indexOf("htmlpreview://") === 0;
  if (window.top === window && !preview) return;
}} catch (e) {{ return; }}
{LOCK_JS}
"#
    )
}

pub fn isolation_plugin<R: tauri::Runtime>() -> tauri::plugin::TauriPlugin<R> {
    tauri::plugin::Builder::new("html-preview-isolation")
        .js_init_script_on_all_frames(isolation_script())
        // WebView2 NavigationStarting (and WebKit's navigation policy) fires for the
        // top document and every child frame. Cancelling here stops the chat iframe
        // from following location changes, link clicks, meta refresh, or data: URLs.
        .on_navigation(|_webview, url| app_navigation_allowed(url))
        .build()
}

pub fn respond(app: &AppHandle, uri: &http::Uri) -> http::Response<Vec<u8>> {
    let data_dir = app.state::<std::sync::Arc<AppState>>().data_dir.clone();
    match render(&data_dir, &uri.to_string()) {
        Ok((csp, body)) => html_response(200, csp, body),
        Err(()) => html_response(
            404,
            PREVIEW_CSP,
            "<!DOCTYPE html><meta charset=\"utf-8\"><p>表示できません。</p>".into(),
        ),
    }
}

pub fn show_window(app: &AppHandle, data_dir: &Path, name: &str) -> Result<(), String> {
    let _ = read_html(data_dir, name)?;
    let url = shell_url(name)?;
    let title = format!("Argos　{name}");
    if let Some(window) = app.get_webview_window(WINDOW_LABEL) {
        let _ = window.set_title(&title);
        window
            .navigate(url)
            .map_err(|err| format!("プレビューを開けません（{err}）。"))?;
        let _ = window.unminimize();
        window
            .show()
            .map_err(|err| format!("プレビューを開けません（{err}）。"))?;
        let _ = window.set_focus();
        return Ok(());
    }
    let window = WebviewWindowBuilder::new(app, WINDOW_LABEL, WebviewUrl::External(url))
        .title(&title)
        .inner_size(960.0, 720.0)
        .min_inner_size(480.0, 360.0)
        .center()
        .resizable(true)
        .focused(true)
        .disable_drag_drop_handler()
        .on_navigation(|url| preview_navigation_allowed(url))
        .on_new_window(|_url, _features| tauri::webview::NewWindowResponse::Deny)
        .build()
        .map_err(|err| format!("プレビューを開けません（{err}）。"))?;
    hide_on_close(&window);
    Ok(())
}

fn hide_on_close(window: &WebviewWindow) {
    let handle = window.clone();
    window.on_window_event(move |event| {
        if let WindowEvent::CloseRequested { api, .. } = event {
            api.prevent_close();
            let _ = handle.hide();
        }
    });
}

const DEV_SERVER_PORT: u16 = 1420;

/// Document navigations the app webviews may complete.
///
/// Allowed: the dev server, the production Tauri origin, the html preview
/// protocol, and `about:blank` / `about:srcdoc`. Everything else, including
/// `https:`, `data:`, and `file:`, is cancelled before a request is made.
pub fn app_navigation_allowed(url: &url::Url) -> bool {
    if preview_navigation_allowed(url) {
        return true;
    }
    match (url.scheme(), url.host_str()) {
        ("tauri", Some("localhost")) => true,
        ("http" | "https", Some("tauri.localhost")) => url.port().is_none(),
        ("http", Some("localhost" | "127.0.0.1" | "::1" | "[::1]")) => {
            url.port() == Some(DEV_SERVER_PORT)
        }
        _ => false,
    }
}

pub fn preview_navigation_allowed(url: &url::Url) -> bool {
    match url.scheme() {
        "about" => {
            let text = url.as_str();
            text == "about:blank" || text.starts_with("about:srcdoc")
        }
        "htmlpreview" => matches!(url.host_str(), Some("localhost") | None),
        "http" | "https" => url.host_str() == Some("htmlpreview.localhost"),
        _ => false,
    }
}

pub fn shell_url(name: &str) -> Result<url::Url, String> {
    let query = format!("name={}", encode_uri_component(name));
    let raw = if cfg!(any(windows, target_os = "android")) {
        format!("http://htmlpreview.localhost/shell?{query}")
    } else {
        format!("htmlpreview://localhost/shell?{query}")
    };
    url::Url::parse(&raw).map_err(|err| format!("プレビューを開けません（{err}）。"))
}

enum Page {
    Preview,
    Shell,
}

fn render(data_dir: &Path, uri: &str) -> Result<(&'static str, String), ()> {
    let (page, name) = page_for_request(uri)?;
    let html = read_html(data_dir, &name).map_err(|_| ())?;
    match page {
        Page::Preview => Ok((PREVIEW_CSP, guard_html(&html))),
        Page::Shell => Ok((SHELL_CSP, shell_html(&name))),
    }
}

fn page_for_request(uri: &str) -> Result<(Page, String), ()> {
    let parsed = if uri.contains("://") {
        url::Url::parse(uri).map_err(|_| ())?
    } else {
        let path = if uri.starts_with('/') {
            uri.to_string()
        } else {
            format!("/{uri}")
        };
        url::Url::parse(&format!("htmlpreview://localhost{path}")).map_err(|_| ())?
    };
    if !request_origin_allowed(&parsed) && uri.contains("://") {
        return Err(());
    }
    let name = query_name(parsed.query().unwrap_or(""))?;
    let path = parsed.path().trim_end_matches('/');
    let page = match path {
        "/preview" => Page::Preview,
        "/shell" => Page::Shell,
        _ => return Err(()),
    };
    Ok((page, name))
}

fn request_origin_allowed(url: &url::Url) -> bool {
    match url.scheme() {
        "htmlpreview" => matches!(url.host_str(), Some("localhost") | None),
        "http" | "https" => url.host_str() == Some("htmlpreview.localhost"),
        _ => false,
    }
}

fn query_name(query: &str) -> Result<String, ()> {
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if key == "name" {
            let name = percent_decode(value)?;
            if name.is_empty() {
                return Err(());
            }
            return Ok(name);
        }
    }
    Err(())
}

fn read_html(data_dir: &Path, name: &str) -> Result<String, String> {
    if !name.to_ascii_lowercase().ends_with(".html") {
        return Err("HTML のファイル名を指定してください。".into());
    }
    let path = output_file_path(data_dir, name)?;
    let meta = std::fs::metadata(&path).map_err(|_| "ファイルが見つかりません。".to_string())?;
    if !meta.is_file() {
        return Err("ファイルが見つかりません。".into());
    }
    if meta.len() > MAX_PREVIEW_BYTES {
        return Err("HTML が大きすぎます。".into());
    }
    let bytes = std::fs::read(&path).map_err(|_| "ファイルを読めません。".to_string())?;
    String::from_utf8(bytes).map_err(|_| "HTML を読めません。".to_string())
}

pub fn guard_html(html: &str) -> String {
    let html = strip_hostile_tags(html);
    let guard = format!(
        "<meta charset=\"utf-8\"><meta name=\"referrer\" content=\"no-referrer\"><meta http-equiv=\"Content-Security-Policy\" content=\"{PREVIEW_CSP}\"><script>{LOCK_JS}</script>"
    );
    inject_guard(&html, &guard)
}

/// Keep a leading doctype, then put the lock in `<head>` so it runs before page scripts.
fn inject_guard(html: &str, guard: &str) -> String {
    let lower = html.to_ascii_lowercase();
    if let Some(end) = find_tag_end_ci(&lower, "<head") {
        let mut out = String::with_capacity(html.len() + guard.len());
        out.push_str(&html[..end]);
        out.push_str(guard);
        out.push_str(&html[end..]);
        return out;
    }
    if let Some(end) = find_tag_end_ci(&lower, "<html") {
        let mut out = String::with_capacity(html.len() + guard.len() + 13);
        out.push_str(&html[..end]);
        out.push_str("<head>");
        out.push_str(guard);
        out.push_str("</head>");
        out.push_str(&html[end..]);
        return out;
    }
    format!("<!DOCTYPE html><head>{guard}</head>{html}")
}

fn find_tag_end_ci(lower: &str, prefix: &str) -> Option<usize> {
    let bytes = lower.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'<' && lower[index..].starts_with(prefix) {
            let next = lower[index + prefix.len()..].chars().next();
            let boundary = matches!(next, None | Some('>' | '/' | ' ' | '\t' | '\n' | '\r'));
            if boundary {
                return tag_end(bytes, index);
            }
        }
        let step = lower[index..]
            .chars()
            .next()
            .map(|ch| ch.len_utf8())
            .unwrap_or(1);
        index += step;
    }
    None
}

fn shell_html(name: &str) -> String {
    let src = format!("/preview?name={}", encode_uri_component(name));
    format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><meta name=\"referrer\" content=\"no-referrer\"><meta http-equiv=\"Content-Security-Policy\" content=\"{SHELL_CSP}\"><title>{}</title><style>html,body{{margin:0;height:100%;background:#fff}}iframe{{border:0;width:100%;height:100%;display:block}}</style></head><body><iframe sandbox=\"{PREVIEW_SANDBOX}\" referrerpolicy=\"no-referrer\" csp=\"{PREVIEW_CSP}\" src=\"{}\"></iframe></body></html>",
        html_escape(name),
        html_escape(&src)
    )
}

fn html_response(status: u16, csp: &str, body: String) -> http::Response<Vec<u8>> {
    http::Response::builder()
        .status(status)
        .header(http::header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(http::header::CONTENT_SECURITY_POLICY, csp)
        .header("referrer-policy", "no-referrer")
        .header(http::header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(http::header::CACHE_CONTROL, "no-store")
        .body(body.into_bytes())
        .unwrap_or_else(|_| {
            http::Response::builder()
                .status(500)
                .body(Vec::new())
                .unwrap_or_else(|_| http::Response::new(Vec::new()))
        })
}

fn strip_hostile_tags(html: &str) -> String {
    let bytes = html.as_bytes();
    let mut out = String::with_capacity(html.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'<' {
            if let Some(end) = tag_end(bytes, index) {
                let tag = &html[index..end];
                if !hostile_tag(tag) {
                    out.push_str(tag);
                }
                index = end;
                continue;
            }
        }
        let ch = html[index..].chars().next().unwrap_or('\u{FFFD}');
        out.push(ch);
        index += ch.len_utf8();
    }
    out
}

fn tag_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut index = start + 1;
    let mut quote = None;
    while index < bytes.len() {
        let byte = bytes[index];
        if let Some(q) = quote {
            if byte == q {
                quote = None;
            }
        } else if byte == b'"' || byte == b'\'' {
            quote = Some(byte);
        } else if byte == b'>' {
            return Some(index + 1);
        }
        index += 1;
    }
    None
}

fn hostile_tag(tag: &str) -> bool {
    let lower = tag.to_ascii_lowercase();
    let head = lower.trim_start_matches('<').trim_start();
    if starts_tag(head, "base") {
        return true;
    }
    if !starts_tag(head, "meta") {
        return false;
    }
    matches!(
        http_equiv(&lower).as_deref(),
        Some("content-security-policy")
            | Some("content-security-policy-report-only")
            | Some("refresh")
    )
}

fn starts_tag(head: &str, name: &str) -> bool {
    let rest = head.strip_prefix(name);
    match rest {
        Some(rest) => {
            rest.is_empty()
                || rest.starts_with(|ch: char| ch.is_whitespace() || ch == '/' || ch == '>')
        }
        None => false,
    }
}

fn http_equiv(lower_tag: &str) -> Option<String> {
    let key = "http-equiv";
    let index = lower_tag.find(key)? + key.len();
    let rest = lower_tag[index..]
        .trim_start()
        .strip_prefix('=')?
        .trim_start();
    let mut chars = rest.chars();
    let quote = chars.next()?;
    if quote == '"' || quote == '\'' {
        let rest = &rest[quote.len_utf8()..];
        let end = rest.find(quote)?;
        Some(rest[..end].trim().to_string())
    } else {
        let end = rest
            .find(|ch: char| ch.is_whitespace() || ch == '>')
            .unwrap_or(rest.len());
        Some(rest[..end].trim().to_string())
    }
}

fn html_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
    out
}

fn encode_uri_component(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'!'
            | b'~'
            | b'*'
            | b'\''
            | b'('
            | b')' => out.push(byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn percent_decode(value: &str) -> Result<String, ()> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' if index + 2 < bytes.len() => {
                let high = hex_val(bytes[index + 1]).ok_or(())?;
                let low = hex_val(bytes[index + 2]).ok_or(())?;
                out.push((high << 4) | low);
                index += 3;
            }
            b'%' => return Err(()),
            byte if byte.is_ascii() => {
                out.push(byte);
                index += 1;
            }
            _ => return Err(()),
        }
    }
    String::from_utf8(out).map_err(|_| ())
}

fn hex_val(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct Tmp(PathBuf);

    impl Tmp {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "argos-html-preview-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn preview_policy_blocks_network_and_same_origin() {
        assert!(PREVIEW_CSP.contains("default-src 'none'"));
        assert!(PREVIEW_CSP.contains("connect-src 'none'"));
        assert!(PREVIEW_CSP.contains("script-src 'unsafe-inline' 'unsafe-eval'"));
        assert!(PREVIEW_CSP.contains("style-src 'unsafe-inline'"));
        assert!(PREVIEW_CSP.contains("frame-src 'none'"));
        assert!(PREVIEW_CSP.contains("object-src 'none'"));
        assert!(PREVIEW_CSP.contains("base-uri 'none'"));
        assert!(PREVIEW_CSP.contains("worker-src 'none'"));
        assert!(PREVIEW_CSP.contains("sandbox allow-scripts"));
        assert!(!PREVIEW_CSP.contains("allow-same-origin"));
        assert!(!PREVIEW_CSP.contains("http:") && !PREVIEW_CSP.contains("https:"));
        assert_eq!(PREVIEW_SANDBOX, "allow-scripts");
        let script = isolation_script();
        assert!(script.contains("window.top === window"));
        assert!(script.contains("htmlpreview.localhost"));
        assert!(script.contains("postMessage"));
        assert!(script.contains("__TAURI_INTERNALS__"));
        assert!(!LOCK_JS.contains("</script"));
        for name in [
            "RTCPeerConnection",
            "webkitRTCPeerConnection",
            "RTCDataChannel",
            "RTCSessionDescription",
            "RTCIceCandidate",
        ] {
            assert!(
                LOCK_JS.contains(&format!("lock(window, \"{name}\", deny)")),
                "{name}"
            );
        }
    }

    #[test]
    fn chat_navigation_blocks_preview_escapes_and_keeps_the_app() {
        for raw in [
            "http://localhost:1420/",
            "http://localhost:1420/index.html",
            "http://127.0.0.1:1420/",
            "http://[::1]:1420/",
            "tauri://localhost/",
            "tauri://localhost/index.html",
            "http://tauri.localhost/",
            "https://tauri.localhost/index.html",
            "htmlpreview://localhost/preview?name=a.html",
            "http://htmlpreview.localhost/preview?name=a.html",
            "https://htmlpreview.localhost/shell?name=a.html",
            "about:blank",
        ] {
            assert!(app_navigation_allowed(&raw.parse().unwrap()), "{raw}");
        }
        // location.href, location.assign, an <a> click, a script-injected meta
        // refresh, and an entity-encoded refresh that the HTML guard does not
        // strip all arrive as a navigation of the chat webview or its iframe.
        for raw in [
            "https://evil.test/steal",
            "http://evil.test/",
            "http://localhost/",
            "http://localhost:1421/",
            "https://localhost:1420/",
            "http://tauri.localhost.evil.test/",
            "http://htmlpreview.localhost.evil.test/",
            "data:text/html,hello",
            "javascript:alert(1)",
            "file:///C:/Windows/system32/cmd.exe",
            "blob:http://localhost:1420/11111111-1111-1111-1111-111111111111",
        ] {
            let url = raw.parse().unwrap();
            assert!(!app_navigation_allowed(&url), "{raw}");
        }
        let encoded = "<meta http-equiv=\"&#114;efresh\" content=\"0;url=https://evil.test/x\">";
        let guarded = guard_html(encoded);
        assert!(guarded.contains("evil.test"), "{guarded}");
        assert!(!app_navigation_allowed(
            &"https://evil.test/x".parse().unwrap()
        ));
    }

    #[test]
    fn navigation_stays_on_the_preview_origin() {
        assert!(preview_navigation_allowed(
            &"htmlpreview://localhost/preview?name=a.html"
                .parse()
                .unwrap()
        ));
        assert!(preview_navigation_allowed(
            &"http://htmlpreview.localhost/shell?name=a.html"
                .parse()
                .unwrap()
        ));
        assert!(preview_navigation_allowed(&"about:blank".parse().unwrap()));
        assert!(!preview_navigation_allowed(
            &"https://example.com/".parse().unwrap()
        ));
        assert!(!preview_navigation_allowed(
            &"http://htmlpreview.localhost.evil.test/".parse().unwrap()
        ));
        assert!(!preview_navigation_allowed(
            &"file:///etc/passwd".parse().unwrap()
        ));
        assert!(!preview_navigation_allowed(
            &"htmlpreview://evil/preview".parse().unwrap()
        ));
    }

    #[test]
    fn request_names_are_decoded_and_other_urls_are_refused() {
        let (_page, name) = page_for_request(
            "http://htmlpreview.localhost/preview?name=%E6%89%8B%E6%95%B0%E6%96%99.html",
        )
        .unwrap();
        assert_eq!(name, "手数料.html");
        let (page, name) = page_for_request("/shell?name=a%26b.html").unwrap();
        assert!(matches!(page, Page::Shell));
        assert_eq!(name, "a&b.html");
        assert!(page_for_request("https://example.com/preview?name=a.html").is_err());
        assert!(page_for_request("/preview?name=").is_err());
        assert!(page_for_request("/other?name=a.html").is_err());
        assert_eq!(
            encode_uri_component("手数料.html"),
            "%E6%89%8B%E6%95%B0%E6%96%99.html"
        );
    }

    #[test]
    fn guarded_document_strips_network_hooks_and_runs_the_lock_first() {
        let raw = "\
<meta http-equiv=\"Content-Security-Policy\" content=\"default-src https:\">\
<base href=\"https://evil.test/\">\
<meta http-equiv=\"refresh\" content=\"0;url=https://evil.test/\">\
<script>window.__user = 1</script>";
        let guarded = guard_html(raw);
        let lock_at = guarded.find("lockHtmlPreview").unwrap();
        let user_at = guarded.find("window.__user").unwrap();
        assert!(lock_at < user_at);
        assert!(guarded.contains(PREVIEW_CSP));
        assert!(!guarded.contains("evil.test"));
        assert!(!guarded.contains("default-src https:"));
        assert!(guarded.contains("<script>window.__user = 1</script>"));
    }

    #[test]
    fn preview_read_stays_inside_the_output_folder() {
        let tmp = Tmp::new();
        let dir = crate::llm::create_office::output_dir(&tmp.0);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("calc.html"), "<p>計算</p>").unwrap();
        let (csp, body) = render(&tmp.0, "/preview?name=calc.html").unwrap();
        assert_eq!(csp, PREVIEW_CSP);
        assert!(body.contains("<p>計算</p>"));
        assert!(body.contains("lockHtmlPreview"));
        let (_csp, shell) = render(&tmp.0, "/shell?name=calc.html").unwrap();
        assert!(shell.contains("sandbox=\"allow-scripts\""));
        assert!(!shell.contains("allow-same-origin"));
        assert!(shell.contains("/preview?name=calc.html"));
        assert!(render(&tmp.0, "/preview?name=..%2Fcalc.html").is_err());
        assert!(render(&tmp.0, "/preview?name=C:%5CWindows%5Ccalc.html").is_err());
        assert!(render(&tmp.0, "/preview?name=notes.txt").is_err());
    }
}
