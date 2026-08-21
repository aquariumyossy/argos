//! OpenAI-compatible local LLM client (chat completions + /models).

pub mod commands;
pub mod context;
pub mod files;
pub mod grain;
pub mod pdf_win;
pub mod tools;
pub mod transcript;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::db::{Settings, LLM_FORMAT_HINT, LLM_FORMAT_SENTINEL};
use crate::llm::context::ChatTurn;

pub fn normalize_base_url(raw: &str) -> String {
    let s = raw.trim().trim_end_matches('/').to_string();
    if s.is_empty() {
        return s;
    }
    // OpenAI-compat servers (Ollama / LM Studio / llama.cpp / MTPLX) expose
    // /v1/models. Users often paste host:port only (e.g. MTPLX on :8000).
    if openai_url_has_no_path(&s) {
        format!("{s}/v1")
    } else {
        s
    }
}

fn openai_url_has_no_path(url: &str) -> bool {
    let rest = url.split("://").nth(1).unwrap_or(url);
    !rest.contains('/')
}

/// Bases to try for /models and /chat/completions.
fn api_base_candidates(raw: &str) -> Vec<String> {
    let base = normalize_base_url(raw);
    if base.is_empty() {
        return Vec::new();
    }
    let mut out = vec![base.clone()];
    if !base.ends_with("/v1") {
        out.push(format!("{base}/v1"));
    }
    out
}

pub fn is_loopback_url(raw: &str) -> bool {
    let s = raw.trim();
    if s.is_empty() {
        return false;
    }
    let rest = s.split("://").nth(1).unwrap_or(s);
    let hostport = rest.split('/').next().unwrap_or(rest);
    let hostport = hostport.rsplit('@').next().unwrap_or(hostport);
    let host = if hostport.starts_with('[') {
        hostport
            .split(']')
            .next()
            .unwrap_or("")
            .trim_start_matches('[')
    } else {
        hostport.split(':').next().unwrap_or(hostport)
    };
    is_loopback_host(host)
}

fn is_loopback_host(host: &str) -> bool {
    let h = host.trim().to_ascii_lowercase();
    if h == "localhost" {
        return true;
    }
    if let Ok(ip) = h.parse::<std::net::IpAddr>() {
        return ip.is_loopback();
    }
    false
}

fn join_url(base: &str, path: &str) -> String {
    let base = normalize_base_url(base);
    let path = path.trim_start_matches('/');
    format!("{base}/{path}")
}

fn error_chain(err: &dyn std::error::Error) -> String {
    let mut parts = vec![err.to_string()];
    let mut cur = err.source();
    while let Some(e) = cur {
        let s = e.to_string();
        if !parts.iter().any(|p| p.contains(&s)) {
            parts.push(s);
        }
        cur = e.source();
    }
    parts.join(" | ")
}

fn is_transport_error(l: &str) -> bool {
    l.contains("connection refused")
        || l.contains("connect error")
        || l.contains("tcp connect error")
        || l.contains("failed to connect")
        || l.contains("could not connect")
        || l.contains("error sending request")
        || l.contains("dns error")
        || l.contains("timed out")
        || l.contains("timeout")
        || l.contains("network is unreachable")
        || l.contains("os error 10060")
        || l.contains("os error 10061")
        || l.contains("os error 10051")
        || l.contains("os error 10065")
        || l.contains("10060")
        || l.contains("10061")
        || l.contains("リモート")
        || l.contains("接続できません")
}

fn looks_non_loopback_http(raw: &str) -> bool {
    raw.contains("100.")
        || raw.contains("192.168.")
        || raw.contains("10.")
        || raw.contains("172.16.")
        || raw.contains("172.17.")
        || raw.contains("172.18.")
        || raw.contains("172.19.")
        || raw.contains("172.2")
        || raw.contains("172.30.")
        || raw.contains("172.31.")
}

fn map_reqwest_error(e: &reqwest::Error) -> String {
    map_llm_error(&error_chain(e))
}

fn map_llm_error(raw: &str) -> String {
    let l = raw.to_lowercase();
    if l.contains("n_ctx")
        || l.contains("context length")
        || l.contains("context window")
        || l.contains("too many tokens")
        || l.contains("maximum context")
        || (l.contains("max_tokens") && l.contains("exceed"))
        || l.contains("prompt is too long")
    {
        return "コンテキスト長が足りません。Ollama の num_ctx などを 64k 以上にしてください。"
            .into();
    }
    if is_transport_error(&l) {
        if looks_non_loopback_http(raw) {
            return "LLM サーバのポートに届きません。Mac の MTPLX は既定で 127.0.0.1:8000 だけ待ち受けるため、Tailscale の 100.x からは繋がりません。Mac 側で LAN 公開するか、Tailscale Serve で 8000 を出してください（例: mtplx serve --host 0.0.0.0 --port 8000 --api-key 任意のキー）。Argos の API キー欄に同じキーを入れます。".into();
        }
        return "LLM サーバに接続できません。起動と URL を確認してください。".into();
    }
    if l.contains("unauthorized") || l.contains("401") || l.contains("invalid api key") {
        return "API キーが拒否されました。MTPLX を 0.0.0.0 で起動している場合は、同じキーを Argos の API キー欄に入れてください。".into();
    }
    if l.contains("model")
        && (l.contains("not found") || l.contains("does not exist") || l.contains("404"))
    {
        return "指定したモデルが見つかりません。モデル名を確認してください。".into();
    }
    let trimmed = raw.trim();
    if trimmed.chars().count() > 400 {
        format!("{}…", trimmed.chars().take(400).collect::<String>())
    } else if trimmed.is_empty() {
        "LLM の呼び出しに失敗しました。".into()
    } else {
        trimmed.to_string()
    }
}

fn apply_auth(mut req: reqwest::RequestBuilder, api_key: &str) -> reqwest::RequestBuilder {
    let key = api_key.trim();
    if !key.is_empty() {
        req = req.bearer_auth(key).header("X-API-Key", key);
    }
    req
}

fn client_for(timeout_ms: u32) -> Result<reqwest::Client, String> {
    let timeout = Duration::from_millis(timeout_ms.max(1_000) as u64);
    reqwest::Client::builder()
        .timeout(timeout)
        .connect_timeout(Duration::from_secs(10))
        .tcp_nodelay(true)
        .build()
        .map_err(|e| map_reqwest_error(&e))
}

/// Streaming chat: no overall timeout. Idle gaps are enforced while reading.
fn client_for_stream() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .tcp_nodelay(true)
        .build()
        .map_err(|e| map_reqwest_error(&e))
}

pub const THINKING_BRIEF_HINT: &str = "内部の検討は短く切り上げ、すぐに結論を書いてください。";

/// Injected only when the user asks for a diagram, not on every turn.
pub const LLM_DIAGRAM_HINT: &str =
    "図は mermaid フェンス（言語タグ mermaid）で1本出してください。要件事実は flowchart LR（左が請求原因、右へ抗弁・再抗弁）。請求原因ノードは1つだけ。時系列は flowchart LR または timeline。ノードは短く、出典にない事実は書かない。ラベルは A[\"請求原因 [n]\"] のように二重引用符で囲み、矢印は --> または -.->（途中に空白を入れない）。枠線や矢印のASCIIアート、言語タグなしのコードブロックで図を描かないでください。";
const LLM_DIAGRAM_SENTINEL: &str = "言語タグ mermaid";

fn user_asks_for_diagram(content: &str) -> bool {
    [
        "ダイアグラム",
        "mermaid",
        "フローチャート",
        "flowchart",
        "構造図",
        "要件事実図",
        "時系列図",
        "図にして",
        "図を描",
        "図を作",
        "図で示",
        "図で整理",
    ]
    .iter()
    .any(|k| content.contains(k))
}

pub fn append_diagram_hint(system: &mut String, user_content: &str) {
    if !user_asks_for_diagram(user_content) {
        return;
    }
    if system.contains(LLM_DIAGRAM_SENTINEL) {
        return;
    }
    if !system.is_empty() {
        system.push('\n');
    }
    system.push_str(LLM_DIAGRAM_HINT);
}

pub fn system_for_request(settings: &Settings) -> String {
    let mut s = settings.llm_system_prompt.trim().to_string();
    if settings.llm_thinking.trim() == "brief"
        && !s.contains("検討は短く")
        && !s.contains(THINKING_BRIEF_HINT)
    {
        if !s.is_empty() {
            s.push('\n');
        }
        s.push_str(THINKING_BRIEF_HINT);
    }
    if !s.contains(LLM_FORMAT_SENTINEL) && !s.contains(LLM_FORMAT_HINT) {
        if !s.is_empty() {
            s.push('\n');
        }
        s.push_str(LLM_FORMAT_HINT);
    }
    s
}

pub fn apply_thinking_to_turns(mut turns: Vec<ChatTurn>, settings: &Settings) -> Vec<ChatTurn> {
    if settings.llm_thinking.trim() != "off" {
        return turns;
    }
    if let Some(t) = turns.iter_mut().rev().find(|t| t.role == "user") {
        if !t.content.contains("/no_think") {
            t.content.push_str("\n/no_think");
        }
    }
    turns
}

/// Tool-calling rounds: turn thinking off in both JSON params and the last user turn.
pub fn turns_for_tool_round(turns: Vec<ChatTurn>) -> Vec<ChatTurn> {
    let mut off = Settings::default();
    off.llm_thinking = "off".into();
    apply_thinking_to_turns(turns, &off)
}

fn apply_thinking_params(body: &mut serde_json::Value, settings: &Settings) {
    match settings.llm_thinking.trim() {
        "off" => {
            body["enable_thinking"] = serde_json::json!(false);
            body["chat_template_kwargs"] = serde_json::json!({ "enable_thinking": false });
        }
        "brief" => {
            let n = if settings.llm_thinking_budget == 0 {
                crate::db::DEFAULT_LLM_THINKING_BUDGET
            } else {
                settings.llm_thinking_budget.max(64)
            };
            body["enable_thinking"] = serde_json::json!(true);
            body["thinking_budget"] = serde_json::json!(n);
            body["chat_template_kwargs"] = serde_json::json!({
                "enable_thinking": true,
                "thinking_budget": n
            });
        }
        _ => {
            if settings.llm_thinking_budget > 0 {
                body["thinking_budget"] = serde_json::json!(settings.llm_thinking_budget);
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmModelInfo {
    pub id: String,
}

pub async fn list_models(settings: &Settings) -> Result<Vec<LlmModelInfo>, String> {
    let bases = api_base_candidates(&settings.llm_base_url);
    if bases.is_empty() {
        return Err("LLM の URL が空です。".into());
    }
    let client = client_for(settings.llm_timeout_ms)?;
    let mut last_err = String::new();
    for base in bases {
        let url = join_url(&base, "models");
        match fetch_models_url(&client, &url, &settings.llm_api_key).await {
            Ok(models) => return Ok(models),
            Err(e) => last_err = e,
        }
    }
    Err(last_err)
}

async fn fetch_models_url(
    client: &reqwest::Client,
    url: &str,
    api_key: &str,
) -> Result<Vec<LlmModelInfo>, String> {
    let req = apply_auth(client.get(url), api_key);
    let resp = req.send().await.map_err(|e| map_reqwest_error(&e))?;
    let status = resp.status();
    let text = resp.text().await.map_err(|e| map_reqwest_error(&e))?;
    if !status.is_success() {
        return Err(map_llm_error(&format!("{status} {text}")));
    }
    parse_models_json(&text)
}

fn parse_models_json(text: &str) -> Result<Vec<LlmModelInfo>, String> {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return Err("モデル一覧の形式を解釈できませんでした。".into());
    };
    let mut ids: Vec<String> = Vec::new();
    let mut arrays: Vec<&Vec<Value>> = Vec::new();
    if let Some(arr) = v.get("data").and_then(|x| x.as_array()) {
        arrays.push(arr);
    }
    if let Some(arr) = v.get("models").and_then(|x| x.as_array()) {
        arrays.push(arr);
    }
    if let Some(arr) = v.as_array() {
        arrays.push(arr);
    }
    for arr in arrays {
        for item in arr {
            let id = item
                .get("id")
                .and_then(|x| x.as_str())
                .or_else(|| item.get("name").and_then(|x| x.as_str()))
                .unwrap_or("")
                .trim();
            if !id.is_empty() {
                ids.push(id.to_string());
            }
        }
    }
    if ids.is_empty() && v.get("data").is_none() && v.get("models").is_none() && !v.is_array() {
        return Err("モデル一覧の形式を解釈できませんでした。".into());
    }
    ids.sort();
    ids.dedup();
    Ok(ids.into_iter().map(|id| LlmModelInfo { id }).collect())
}

pub async fn test_connection(settings: &Settings) -> Result<(String, Vec<LlmModelInfo>), String> {
    match list_models(settings).await {
        Ok(models) => {
            let message = if models.is_empty() {
                "接続できました（モデル一覧は空です）。".into()
            } else {
                let names: Vec<&str> = models.iter().map(|m| m.id.as_str()).take(8).collect();
                format!(
                    "接続できました。モデル {} 件（例: {}）",
                    models.len(),
                    names.join(", ")
                )
            };
            Ok((message, models))
        }
        Err(e) => {
            if is_transport_error(&e.to_lowercase()) || settings.llm_model.trim().is_empty() {
                return Err(e);
            }
            ping_chat(settings).await?;
            Ok((
                format!("接続できました（/models は失敗: {e}）。"),
                Vec::new(),
            ))
        }
    }
}

async fn ping_chat(settings: &Settings) -> Result<(), String> {
    let base = normalize_base_url(&settings.llm_base_url);
    if base.is_empty() {
        return Err("LLM の URL が空です。".into());
    }
    let model = settings.llm_model.trim();
    if model.is_empty() {
        return Err(
            "モデル名が空です。接続テストでモデル一覧を取得するか、モデル名を入力してください。"
                .into(),
        );
    }
    let url = join_url(&base, "chat/completions");
    let client = client_for(settings.llm_timeout_ms.min(20_000).max(5_000))?;
    let body = serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": "ping"}],
        "max_tokens": 1,
        "stream": false
    });
    let req = apply_auth(client.post(&url).json(&body), &settings.llm_api_key);
    let resp = req.send().await.map_err(|e| map_reqwest_error(&e))?;
    let status = resp.status();
    let text = resp.text().await.map_err(|e| map_reqwest_error(&e))?;
    if !status.is_success() {
        return Err(map_llm_error(&format!("{status} {text}")));
    }
    Ok(())
}

fn turn_to_message(t: &ChatTurn) -> Option<Value> {
    let role = t.role.as_str();
    if role != "user" && role != "assistant" && role != "system" && role != "tool" {
        return None;
    }
    let mut m = serde_json::json!({ "role": role });
    if let Some(tcs) = &t.tool_calls {
        m["tool_calls"] = tcs.clone();
        if t.content.is_empty() {
            m["content"] = Value::Null;
        } else {
            m["content"] = json!(t.content);
        }
        return Some(m);
    }
    if role == "tool" {
        m["content"] = json!(t.content);
        if let Some(id) = &t.tool_call_id {
            m["tool_call_id"] = json!(id);
        }
        if let Some(n) = &t.name {
            m["name"] = json!(n);
        }
        return Some(m);
    }
    if t.content.is_empty() {
        return None;
    }
    m["content"] = json!(t.content);
    Some(m)
}

#[derive(Debug, Clone, Default)]
struct ToolCallAcc {
    index: i64,
    id: String,
    name: String,
    arguments: String,
}

#[derive(Debug, Clone, Default)]
struct StreamParse {
    content: String,
    reasoning: String,
    tool_calls: Vec<ToolCallAcc>,
}

#[derive(Debug, Clone)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Default)]
pub struct StreamOutcome {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
}

impl StreamParse {
    fn into_outcome(self) -> StreamOutcome {
        let mut calls = self.tool_calls;
        let mut content = self.content;
        if calls.is_empty() {
            let (stripped, embedded) = pull_embedded_tool_calls(&content);
            if !embedded.is_empty() {
                content = stripped;
                calls.extend(embedded);
            } else {
                let (_, from_think) = pull_embedded_tool_calls(&self.reasoning);
                calls.extend(from_think);
            }
        }
        calls.sort_by_key(|c| c.index);
        StreamOutcome {
            content,
            tool_calls: calls
                .into_iter()
                .filter(|c| !c.name.is_empty() || !c.arguments.is_empty())
                .map(|c| ToolCall {
                    id: if c.id.is_empty() {
                        format!("call-{}", c.index)
                    } else {
                        c.id
                    },
                    name: c.name,
                    arguments: c.arguments,
                })
                .collect(),
        }
    }
}

fn tool_call_from_value(v: &Value, index: i64) -> Option<ToolCallAcc> {
    let name = v
        .get("name")
        .and_then(|x| x.as_str())
        .or_else(|| v.pointer("/function/name").and_then(|x| x.as_str()))
        .unwrap_or("")
        .trim()
        .to_string();
    if name.is_empty() {
        return None;
    }
    let args = v
        .get("arguments")
        .or_else(|| v.get("parameters"))
        .or_else(|| v.pointer("/function/arguments"));
    let arguments = match args {
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => "{}".into(),
    };
    Some(ToolCallAcc {
        index,
        id: format!("embed-{index}"),
        name,
        arguments,
    })
}

/// Qwen / Hermes sometimes write `<tool_call>{...}</tool_call>` in content instead of `tool_calls`.
fn pull_embedded_tool_calls(text: &str) -> (String, Vec<ToolCallAcc>) {
    let mut calls = Vec::new();
    let mut stripped = String::new();
    let mut rest = text;
    let open = "<tool_call>";
    let close = "</tool_call>";
    while let Some(start) = rest.find(open) {
        stripped.push_str(&rest[..start]);
        let after = &rest[start + open.len()..];
        let Some(end) = after.find(close) else {
            stripped.push_str(&rest[start..]);
            rest = "";
            break;
        };
        let inner = after[..end].trim();
        rest = &after[end + close.len()..];
        if let Ok(v) = serde_json::from_str::<Value>(inner) {
            if let Some(call) = tool_call_from_value(&v, calls.len() as i64) {
                calls.push(call);
                continue;
            }
        }
        stripped.push_str(open);
        stripped.push_str(inner);
        stripped.push_str(close);
    }
    stripped.push_str(rest);
    (stripped, calls)
}

pub fn is_tools_unsupported_error(e: &str) -> bool {
    let l = e.to_lowercase();
    let mentions_tools = l.contains("tool")
        || l.contains("function_call")
        || l.contains("function calling")
        || l.contains("function call");
    let looks_invalid = l.contains("400")
        || l.contains("404")
        || l.contains("invalid")
        || l.contains("unknown")
        || l.contains("not support")
        || l.contains("unsupported")
        || l.contains("unexpected")
        || l.contains("does not support");
    mentions_tools && looks_invalid
}

pub async fn stream_chat(
    settings: &Settings,
    turns: &[ChatTurn],
    cancel: Arc<AtomicBool>,
    tools: Option<&Value>,
    disable_thinking: bool,
    mut on_delta: impl FnMut(&str, &str),
) -> Result<StreamOutcome, String> {
    let base = normalize_base_url(&settings.llm_base_url);
    if base.is_empty() {
        return Err("LLM の URL が空です。".into());
    }
    let model = settings.llm_model.trim();
    if model.is_empty() {
        return Err("モデル名が空です。設定の「ローカルLLM」でモデルを指定してください。".into());
    }
    let messages: Vec<Value> = turns.iter().filter_map(turn_to_message).collect();
    if !messages
        .iter()
        .any(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
    {
        return Err("送信するユーザーメッセージがありません。".into());
    }

    let url = join_url(&base, "chat/completions");
    let client = client_for_stream()?;
    let mut body = serde_json::json!({
        "model": model,
        "messages": messages,
        "stream": true
    });
    if let Some(tools) = tools {
        body["tools"] = tools.clone();
        body["tool_choice"] = json!("auto");
    }
    if disable_thinking {
        let mut off = settings.clone();
        off.llm_thinking = "off".into();
        apply_thinking_params(&mut body, &off);
    } else {
        apply_thinking_params(&mut body, settings);
    }
    let req = apply_auth(
        client
            .post(&url)
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .header(reqwest::header::CACHE_CONTROL, "no-cache")
            .json(&body),
        &settings.llm_api_key,
    );
    let resp = req.send().await.map_err(|e| map_reqwest_error(&e))?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(map_llm_error(&format!("{status} {text}")));
    }

    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    let mut parse = StreamParse::default();
    if content_type.contains("application/json") && !content_type.contains("event-stream") {
        let text = resp.text().await.map_err(|e| map_reqwest_error(&e))?;
        ingest_payload(&text, &mut parse, &mut on_delta)?;
        return Ok(parse.into_outcome());
    }

    let mut stream = resp.bytes_stream();
    let mut buf = String::new();
    let idle = Duration::from_millis(settings.llm_timeout_ms.max(5_000) as u64);
    let mut last = Instant::now();
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Err("cancelled".into());
        }
        let remaining = idle.saturating_sub(last.elapsed());
        if remaining.is_zero() {
            return Err(
                "応答が止まりました（タイムアウト）。思考が長いときは設定の「思考」を「短くする」か「オフ」にしてください。"
                    .into(),
            );
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(200).min(remaining)) => {}
            chunk = stream.next() => {
                let Some(chunk) = chunk else {
                    break;
                };
                let bytes = chunk.map_err(|e| map_reqwest_error(&e))?;
                buf.push_str(&String::from_utf8_lossy(&bytes));
                drain_sse_lines(&mut buf, &mut parse, &mut on_delta)?;
                last = Instant::now();
            }
        }
    }
    if !buf.trim().is_empty() {
        let rest = buf.trim();
        if rest.starts_with('{') {
            ingest_payload(rest, &mut parse, &mut on_delta)?;
        } else {
            for line in buf.split('\n') {
                take_sse_data_line(line, &mut parse, &mut on_delta)?;
            }
        }
    }
    Ok(parse.into_outcome())
}

fn drain_sse_lines(
    buf: &mut String,
    parse: &mut StreamParse,
    on_delta: &mut impl FnMut(&str, &str),
) -> Result<(), String> {
    while let Some(idx) = buf.find('\n') {
        let line = buf[..idx].to_string();
        buf.drain(..=idx);
        take_sse_data_line(&line, parse, on_delta)?;
    }
    Ok(())
}

fn take_sse_data_line(
    line: &str,
    parse: &mut StreamParse,
    on_delta: &mut impl FnMut(&str, &str),
) -> Result<(), String> {
    let line = line.trim_end_matches('\r').trim();
    let Some(data) = line.strip_prefix("data:") else {
        return Ok(());
    };
    let data = data.trim();
    if data.is_empty() || data == "[DONE]" {
        return Ok(());
    }
    ingest_payload(data, parse, on_delta)
}

fn merge_tool_calls(parse: &mut StreamParse, arr: &[Value]) {
    for item in arr {
        let index = item
            .get("index")
            .and_then(|v| v.as_i64())
            .unwrap_or_else(|| parse.tool_calls.last().map(|c| c.index).unwrap_or(0));
        if let Some(slot) = parse.tool_calls.iter_mut().find(|c| c.index == index) {
            if let Some(id) = item.get("id").and_then(|v| v.as_str()) {
                if !id.is_empty() {
                    slot.id = id.to_string();
                }
            }
            if let Some(name) = item.pointer("/function/name").and_then(|v| v.as_str()) {
                if !name.is_empty() {
                    slot.name = name.to_string();
                }
            }
            if let Some(args) = item.pointer("/function/arguments") {
                if let Some(s) = args.as_str() {
                    slot.arguments.push_str(s);
                } else {
                    slot.arguments.push_str(&args.to_string());
                }
            }
        } else {
            let mut acc = ToolCallAcc {
                index,
                id: item
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                name: item
                    .pointer("/function/name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                arguments: String::new(),
            };
            if let Some(args) = item.pointer("/function/arguments") {
                if let Some(s) = args.as_str() {
                    acc.arguments.push_str(s);
                } else if !args.is_null() {
                    acc.arguments.push_str(&args.to_string());
                }
            }
            parse.tool_calls.push(acc);
        }
    }
}

fn ingest_payload(
    data: &str,
    parse: &mut StreamParse,
    on_delta: &mut impl FnMut(&str, &str),
) -> Result<(), String> {
    let v: Value = serde_json::from_str(data)
        .map_err(|_| map_llm_error(&format!("ストリームの解釈に失敗しました: {data}")))?;
    if let Some(err) = v.get("error") {
        let msg = err
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or_else(|| err.as_str().unwrap_or("error"));
        return Err(map_llm_error(msg));
    }
    if let Some(arr) = v
        .pointer("/choices/0/delta/tool_calls")
        .or_else(|| v.pointer("/choices/0/message/tool_calls"))
        .and_then(|x| x.as_array())
    {
        merge_tool_calls(parse, arr);
    }
    if let Some(fc) = v
        .pointer("/choices/0/delta/function_call")
        .or_else(|| v.pointer("/choices/0/message/function_call"))
    {
        if fc.is_object() {
            merge_tool_calls(
                parse,
                &[json!({
                    "index": 0,
                    "id": "call-0",
                    "function": fc
                })],
            );
        }
    }
    for (kind, text) in pieces_from_payload(&v) {
        if kind == "content" {
            parse.content.push_str(&text);
        } else if kind == "reasoning" {
            parse.reasoning.push_str(&text);
        }
        on_delta(kind, &text);
    }
    Ok(())
}

fn value_text(v: Option<&Value>) -> String {
    let Some(v) = v else {
        return String::new();
    };
    if let Some(s) = v.as_str() {
        return s.to_string();
    }
    if let Some(arr) = v.as_array() {
        return arr
            .iter()
            .map(|item| {
                item.as_str()
                    .map(str::to_string)
                    .or_else(|| {
                        item.get("text")
                            .and_then(|t| t.as_str())
                            .map(str::to_string)
                    })
                    .unwrap_or_default()
            })
            .collect();
    }
    String::new()
}

fn first_text(candidates: &[Option<&Value>]) -> String {
    for c in candidates {
        let t = value_text(*c);
        if !t.is_empty() {
            return t;
        }
    }
    String::new()
}

fn pieces_from_payload(v: &Value) -> Vec<(&'static str, String)> {
    let reasoning = first_text(&[
        v.pointer("/choices/0/delta/reasoning_content"),
        v.pointer("/choices/0/delta/reasoning"),
        v.pointer("/choices/0/message/reasoning_content"),
    ]);
    let content = first_text(&[
        v.pointer("/choices/0/delta/content"),
        v.pointer("/choices/0/delta/text"),
        v.pointer("/choices/0/text"),
        v.pointer("/choices/0/message/content"),
    ]);
    let mut pieces = Vec::new();
    if !reasoning.is_empty() {
        pieces.push(("reasoning", reasoning));
    }
    if !content.is_empty() {
        pieces.push(("content", content));
    }
    pieces
}

pub fn is_cancelled_error(e: &str) -> bool {
    e == "cancelled"
}

const OCR_PROMPT: &str =
    "この画像に書かれている文字を、見えるとおり書き起こしてください。表や図は簡潔に説明してください。読み取れない箇所は「（判読不能）」としてください。画像にない条文・事実・日付を推測で補わないでください。";

async fn wait_cancel_flag(cancel: Arc<AtomicBool>) {
    loop {
        if cancel.load(Ordering::SeqCst) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

/// One-shot vision call: transcribe an image to text. Does not use tools or history.
pub async fn transcribe_image(
    settings: &Settings,
    mime: &str,
    bytes: &[u8],
    cancel: Arc<AtomicBool>,
) -> Result<String, String> {
    if cancel.load(Ordering::SeqCst) {
        return Err("cancelled".into());
    }
    let base = normalize_base_url(&settings.llm_base_url);
    if base.is_empty() {
        return Err("LLM の URL が空です。".into());
    }
    let model = settings.llm_model.trim();
    if model.is_empty() {
        return Err("モデル名が空です。設定の「ローカルLLM」でモデルを指定してください。".into());
    }
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
    let data_url = format!("data:{mime};base64,{b64}");
    let url = join_url(&base, "chat/completions");
    let timeout_ms = settings.llm_timeout_ms.max(120_000);
    let client = client_for(timeout_ms)?;
    let mut body = serde_json::json!({
        "model": model,
        "stream": false,
        "messages": [{
            "role": "user",
            "content": [
                { "type": "text", "text": OCR_PROMPT },
                { "type": "image_url", "image_url": { "url": data_url } }
            ]
        }]
    });
    let mut off = settings.clone();
    off.llm_thinking = "off".into();
    apply_thinking_params(&mut body, &off);

    let req = apply_auth(client.post(&url).json(&body), &settings.llm_api_key);
    let resp = tokio::select! {
        _ = wait_cancel_flag(cancel.clone()) => {
            return Err("cancelled".into());
        }
        sent = req.send() => sent.map_err(|e| map_reqwest_error(&e))?,
    };
    if cancel.load(Ordering::SeqCst) {
        return Err("cancelled".into());
    }
    let status = resp.status();
    let text = resp.text().await.map_err(|e| map_reqwest_error(&e))?;
    if !status.is_success() {
        return Err(map_vision_error(&map_llm_error(&format!(
            "{status} {text}"
        ))));
    }
    let v: Value = serde_json::from_str(&text)
        .map_err(|_| map_llm_error(&format!("応答の解釈に失敗しました: {text}")))?;
    if let Some(err) = v.get("error") {
        let msg = err
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or_else(|| err.as_str().unwrap_or("error"));
        return Err(map_vision_error(&map_llm_error(msg)));
    }
    let content = first_text(&[
        v.pointer("/choices/0/message/content"),
        v.pointer("/choices/0/text"),
        v.pointer("/choices/0/delta/content"),
    ]);
    let content = content.trim();
    if content.is_empty() {
        return Err(
            "モデルが書き起こしを返しませんでした。画像に対応したモデルか確認してください。".into(),
        );
    }
    let capped: String = content
        .chars()
        .take(crate::llm::context::PER_SOURCE_CAP)
        .collect();
    Ok(capped)
}

fn map_vision_error(msg: &str) -> String {
    let l = msg.to_lowercase();
    if l.contains("image")
        || l.contains("vision")
        || l.contains("multimodal")
        || l.contains("does not support")
        || l.contains("unsupported")
    {
        return "このモデルは画像を読めません。vision 対応のモデルを設定してください。".into();
    }
    msg.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds_v1_when_path_missing() {
        assert_eq!(
            normalize_base_url("http://100.65.231.111:8000"),
            "http://100.65.231.111:8000/v1"
        );
        assert_eq!(
            normalize_base_url("http://127.0.0.1:11434/v1"),
            "http://127.0.0.1:11434/v1"
        );
        assert_eq!(
            normalize_base_url("http://127.0.0.1:8000/"),
            "http://127.0.0.1:8000/v1"
        );
    }

    #[test]
    fn loopback_hosts() {
        assert!(is_loopback_url("http://127.0.0.1:11434/v1"));
        assert!(is_loopback_url("http://localhost:1234/v1"));
        assert!(is_loopback_url("http://[::1]:8080/v1"));
        assert!(is_loopback_url("http://127.0.0.2/v1"));
        assert!(!is_loopback_url("http://192.168.1.8:11434/v1"));
        assert!(!is_loopback_url("https://api.openai.com/v1"));
        assert!(!is_loopback_url(""));
    }

    #[test]
    fn maps_context_errors() {
        let msg = map_llm_error("prompt is too long: n_ctx=4096");
        assert!(msg.contains("コンテキスト長"));
        let conn = map_llm_error("error sending request for url: connection refused");
        assert!(conn.contains("接続できません"));
        let lan =
            map_llm_error("error sending request for url (http://100.65.231.111:8000/v1/models)");
        assert!(lan.contains("127.0.0.1") || lan.contains("届きません"));
    }

    #[test]
    fn parses_models_payload() {
        let json = r#"{"data":[{"id":"qwen3.6:27b"},{"id":"gemma4"}]}"#;
        let models = parse_models_json(json).unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "gemma4");
    }

    #[test]
    fn parses_name_and_empty_data() {
        let named = parse_models_json(r#"{"data":[{"name":"mtplx"}]}"#).unwrap();
        assert_eq!(named[0].id, "mtplx");
        let empty = parse_models_json(r#"{"object":"list","data":[]}"#).unwrap();
        assert!(empty.is_empty());
    }

    #[test]
    fn drains_sse_content() {
        let mut buf = "data: {\"choices\":[{\"delta\":{\"content\":\"あ\"}}]}\n".to_string();
        let mut parse = StreamParse::default();
        let mut got = String::new();
        drain_sse_lines(&mut buf, &mut parse, &mut |kind, d| {
            assert_eq!(kind, "content");
            got.push_str(d);
        })
        .unwrap();
        assert_eq!(parse.content, "あ");
        assert_eq!(got, "あ");
    }

    #[test]
    fn drains_sse_reasoning_then_content() {
        let mut buf = concat!(
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"考\"}}]}\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"え\"}}]}\n"
        )
        .to_string();
        let mut parse = StreamParse::default();
        let mut reasoning = String::new();
        let mut content = String::new();
        drain_sse_lines(&mut buf, &mut parse, &mut |kind, d| {
            if kind == "reasoning" {
                reasoning.push_str(d);
            } else {
                content.push_str(d);
            }
        })
        .unwrap();
        assert_eq!(reasoning, "考");
        assert_eq!(content, "え");
        assert_eq!(parse.content, "え");
    }

    #[test]
    fn drains_sse_tool_calls() {
        let mut buf = concat!(
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"search_index","arguments":"{\"q"}}]}}]}"#,
            "\n",
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"uery\":\"民法\"}"}}]}}]}"#,
            "\n"
        )
        .to_string();
        let mut parse = StreamParse::default();
        drain_sse_lines(&mut buf, &mut parse, &mut |_, _| {}).unwrap();
        let out = parse.into_outcome();
        assert_eq!(out.tool_calls.len(), 1);
        assert_eq!(out.tool_calls[0].name, "search_index");
        assert!(out.tool_calls[0].arguments.contains("民法"));
    }

    #[test]
    fn tools_unsupported_detects_400() {
        assert!(is_tools_unsupported_error(
            "400 invalid tools: unknown field"
        ));
        assert!(!is_tools_unsupported_error("connection refused"));
    }

    fn thinking_settings(mode: &str, budget: u32) -> Settings {
        let mut s = Settings::default();
        s.llm_thinking = mode.into();
        s.llm_thinking_budget = budget;
        s
    }

    #[test]
    fn brief_thinking_sets_budget() {
        let s = thinking_settings("brief", 1024);
        let mut body = serde_json::json!({});
        apply_thinking_params(&mut body, &s);
        assert_eq!(body["enable_thinking"], true);
        assert_eq!(body["thinking_budget"], 1024);
        assert_eq!(body["chat_template_kwargs"]["thinking_budget"], 1024);
        let sys = system_for_request(&s);
        assert!(sys.contains("検討は短く"));
    }

    #[test]
    fn format_hint_appended_when_missing() {
        let mut s = Settings::default();
        s.llm_thinking = "off".into();
        s.llm_system_prompt = "custom".into();
        let sys = system_for_request(&s);
        assert!(sys.contains("custom"));
        assert!(sys.contains("生のHTMLは書かないでください"));
        assert_eq!(sys.matches("生のHTMLは書かないでください").count(), 1);
    }

    #[test]
    fn diagram_hint_only_when_user_asks() {
        let mut sys = String::from("base");
        append_diagram_hint(&mut sys, "争点を整理して");
        assert!(!sys.contains("言語タグ mermaid"));
        append_diagram_hint(&mut sys, "要件事実ダイアグラムを作って");
        assert!(sys.contains("言語タグ mermaid"));
        let once = sys.matches("言語タグ mermaid").count();
        append_diagram_hint(&mut sys, "構造図も出して");
        assert_eq!(sys.matches("言語タグ mermaid").count(), once);
    }

    #[test]
    fn format_hint_not_duplicated_on_default() {
        let mut s = Settings::default();
        s.llm_thinking = "off".into();
        let sys = system_for_request(&s);
        assert_eq!(sys.matches("生のHTMLは書かないでください").count(), 1);
    }

    #[test]
    fn off_thinking_adds_no_think() {
        let s = thinking_settings("off", 0);
        let mut body = serde_json::json!({});
        apply_thinking_params(&mut body, &s);
        assert_eq!(body["enable_thinking"], false);
        let turns = apply_thinking_to_turns(vec![ChatTurn::text("user", "要約して")], &s);
        assert!(turns[0].content.ends_with("/no_think"));
    }

    #[test]
    fn tool_round_appends_no_think() {
        let turns = turns_for_tool_round(vec![ChatTurn::text("user", "民法555条")]);
        assert!(turns[0].content.ends_with("/no_think"));
    }

    #[test]
    fn pulls_qwen_xml_tool_call_from_content() {
        let parse = StreamParse {
            content: concat!(
                "調べます。\n",
                r#"<tool_call>{"name":"search_index","arguments":{"query":"民法555条"}}</tool_call>"#,
            )
            .into(),
            reasoning: String::new(),
            tool_calls: Vec::new(),
        };
        let out = parse.into_outcome();
        assert_eq!(out.tool_calls.len(), 1);
        assert_eq!(out.tool_calls[0].name, "search_index");
        assert!(out.tool_calls[0].arguments.contains("民法555条"));
        assert!(!out.content.contains("<tool_call>"));
        assert!(out.content.contains("調べます。"));
    }

    #[test]
    fn drains_legacy_function_call() {
        let mut buf =
            r#"data: {"choices":[{"delta":{"function_call":{"name":"read_unit","arguments":"{\"paragraph_id\":\"u1\"}"}}}]}"#
                .to_string()
                + "\n";
        let mut parse = StreamParse::default();
        drain_sse_lines(&mut buf, &mut parse, &mut |_, _| {}).unwrap();
        let out = parse.into_outcome();
        assert_eq!(out.tool_calls.len(), 1);
        assert_eq!(out.tool_calls[0].name, "read_unit");
        assert!(out.tool_calls[0].arguments.contains("u1"));
    }
}
