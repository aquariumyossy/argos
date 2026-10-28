//! OpenAI-compatible local LLM client (chat completions + /models).

pub mod commands;
pub mod context;
pub mod fetch_url;
pub mod files;
pub mod grain;
pub mod http_error;
pub mod pdf_win;
pub mod searxng;
pub mod thinking;
pub mod tools;
pub mod transcript;

pub use http_error::{is_context_too_long_error, is_tools_unsupported_error};

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::db::{Settings, LLM_FORMAT_HINT, LLM_FORMAT_SENTINEL, LLM_NOTE_HINT, LLM_NOTE_SENTINEL};
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

fn map_reqwest_error(e: &reqwest::Error) -> String {
    http_error::transport_failure(&error_chain(e))
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
    if !s.contains(LLM_FORMAT_SENTINEL) && !s.contains(LLM_FORMAT_HINT) {
        if !s.is_empty() {
            s.push('\n');
        }
        s.push_str(LLM_FORMAT_HINT);
    }
    if !s.contains(LLM_NOTE_SENTINEL) && !s.contains(LLM_NOTE_HINT) {
        if !s.is_empty() {
            s.push('\n');
        }
        s.push_str(LLM_NOTE_HINT);
    }
    s
}

fn apply_thinking_params(body: &mut serde_json::Value, settings: &Settings) {
    body["chat_template_kwargs"] =
        serde_json::json!({ "enable_thinking": thinking::thinking_enabled(settings) });
}

pub(crate) fn build_chat_body(
    model: &str,
    messages: &[Value],
    tools: Option<&Value>,
    thinking: Option<bool>,
) -> Value {
    let mut body = serde_json::json!({
        "model": model,
        "messages": messages,
        "stream": true
    });
    if let Some(tools) = tools {
        body["tools"] = tools.clone();
        body["tool_choice"] = json!("auto");
    }
    if let Some(enabled) = thinking {
        body["chat_template_kwargs"] = json!({ "enable_thinking": enabled });
    }
    body
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
        return Err(http_error::models_failure(status.as_u16()));
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
            let mut message = if models.is_empty() {
                "接続できました（モデル一覧は空です）。".to_string()
            } else {
                let names: Vec<&str> = models.iter().map(|m| m.id.as_str()).take(8).collect();
                format!(
                    "接続できました。モデル {} 件（例: {}）",
                    models.len(),
                    names.join(", ")
                )
            };
            message.push_str(&thinking_budget_sentence(refresh_thinking_budget(settings, None).await));
            Ok((message, models))
        }
        Err(e) => {
            if http_error::is_connection_message(&e) || settings.llm_model.trim().is_empty() {
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
        return Err(http_error::chat_failure(status.as_u16(), &text));
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
        if let Some(r) = t
            .reasoning
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            m["reasoning_content"] = json!(crate::llm::context::cap_chars(
                r,
                crate::llm::context::REASONING_CARRY_CAP
            ));
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
    finish_reason: String,
    request_id: String,
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
    pub reasoning: String,
    pub tool_calls: Vec<ToolCall>,
    pub finish_reason: String,
    pub request_id: String,
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
            reasoning: self.reasoning,
            finish_reason: self.finish_reason,
            request_id: self.request_id,
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

pub async fn stream_chat(
    settings: &Settings,
    turns: &[ChatTurn],
    cancel: Arc<AtomicBool>,
    tools: Option<&Value>,
    runaway_chars: usize,
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
    let thinking_on = thinking::thinking_enabled(settings);
    let mut include_thinking = true;
    let resp = loop {
        let thinking = if include_thinking {
            Some(thinking_on)
        } else {
            None
        };
        let body = build_chat_body(model, &messages, tools, thinking);
        let req = apply_auth(
            client
                .post(&url)
                .header(reqwest::header::ACCEPT, "text/event-stream")
                .header(reqwest::header::CACHE_CONTROL, "no-cache")
                .json(&body),
            &settings.llm_api_key,
        );
        let resp = req.send().await.map_err(|e| map_reqwest_error(&e))?;
        if resp.status() == reqwest::StatusCode::BAD_REQUEST && include_thinking {
            // Servers that do not understand the thinking fields reject them.
            let _ = resp.text().await;
            include_thinking = false;
            continue;
        }
        break resp;
    };
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(http_error::chat_failure(status.as_u16(), &text));
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
        ingest_payload(&text, &mut parse, runaway_chars, &mut on_delta)?;
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
                "応答が止まりました（タイムアウト）。指示を分けるか、設定の「思考」をオフにしてください。"
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
                drain_sse_lines(&mut buf, &mut parse, runaway_chars, &mut on_delta)?;
                last = Instant::now();
            }
        }
    }
    if !buf.trim().is_empty() {
        let rest = buf.trim();
        if rest.starts_with('{') {
            ingest_payload(rest, &mut parse, runaway_chars, &mut on_delta)?;
        } else {
            for line in buf.split('\n') {
                take_sse_data_line(line, &mut parse, runaway_chars, &mut on_delta)?;
            }
        }
    }
    Ok(parse.into_outcome())
}

fn drain_sse_lines(
    buf: &mut String,
    parse: &mut StreamParse,
    runaway_chars: usize,
    on_delta: &mut impl FnMut(&str, &str),
) -> Result<(), String> {
    while let Some(idx) = buf.find('\n') {
        let line = buf[..idx].to_string();
        buf.drain(..=idx);
        take_sse_data_line(&line, parse, runaway_chars, on_delta)?;
    }
    Ok(())
}

fn take_sse_data_line(
    line: &str,
    parse: &mut StreamParse,
    runaway_chars: usize,
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
    ingest_payload(data, parse, runaway_chars, on_delta)
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
    runaway_chars: usize,
    on_delta: &mut impl FnMut(&str, &str),
) -> Result<(), String> {
    let v: Value = serde_json::from_str(data)
        .map_err(|_| "応答の解釈に失敗しました。".to_string())?;
    if let Some(id) = v.get("id").and_then(|value| value.as_str()) {
        if !id.is_empty() {
            parse.request_id = id.to_string();
        }
    }
    if let Some(reason) = v.pointer("/choices/0/finish_reason").and_then(|value| value.as_str()) {
        if !reason.is_empty() {
            parse.finish_reason = reason.to_string();
        }
    }
    if v.get("error").is_some() {
        return Err("LLM の呼び出しに失敗しました。".into());
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
    guard_stream(parse, runaway_chars)
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

fn guard_stream(parse: &StreamParse, runaway_chars: usize) -> Result<(), String> {
    if parse.content.contains(thinking::THINKING_BRIDGE) {
        return Err(thinking::THINKING_BRIDGE_ERROR.into());
    }
    let thought = parse.reasoning.chars().count();
    if runaway_chars > 0 && thought > runaway_chars {
        return Err(format!("{}{thought}", thinking::THINKING_RUNAWAY_PREFIX));
    }
    Ok(())
}

pub fn is_cancelled_error(e: &str) -> bool {
    e == "cancelled"
}

pub fn metrics_origin(base: &str) -> String {
    let base = normalize_base_url(base);
    if let Some(rest) = base.strip_suffix("/v1") {
        rest.trim_end_matches('/').to_string()
    } else {
        base
    }
}

pub fn runaway_chars_for(base: &str) -> usize {
    let budget = match thinking::known_budget_for(&normalize_base_url(base)) {
        thinking::KnownBudget::Tokens(n) => Some(n),
        thinking::KnownBudget::Unset | thinking::KnownBudget::Unknown => None,
    };
    thinking::runaway_thinking_chars(budget)
}

pub fn thinking_budget_sentence(budget: thinking::KnownBudget) -> String {
    match budget {
        thinking::KnownBudget::Tokens(n) => format!(
            " 思考の上限は MTPLX の記録で {} トークンです。",
            thinking::format_ja_int(n as u64)
        ),
        thinking::KnownBudget::Unset => {
            " MTPLX に思考の上限（--agent-thinking-budget）が設定されていません。思考が長引くことがあります。"
                .into()
        }
        thinking::KnownBudget::Unknown => {
            " 思考の上限は、最初のツール呼び出しのあとに MTPLX の記録から確かめます。".into()
        }
    }
}

fn known_from_parsed(parsed: Option<Option<u32>>) -> Option<thinking::KnownBudget> {
    match parsed {
        Some(Some(n)) => Some(thinking::KnownBudget::Tokens(n)),
        Some(None) => Some(thinking::KnownBudget::Unset),
        None => None,
    }
}

async fn fetch_metrics(settings: &Settings) -> Option<Value> {
    let origin = metrics_origin(&settings.llm_base_url);
    if origin.is_empty() {
        return None;
    }
    let client = client_for(5_000).ok()?;
    let url = format!("{origin}/metrics");
    let resp = apply_auth(client.get(&url), &settings.llm_api_key)
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let text = resp.text().await.ok()?;
    serde_json::from_str(&text).ok()
}

/// Read `GET /metrics`. `None` from the parser means the records do not say,
/// so a previously remembered budget is kept.
pub async fn refresh_thinking_budget(
    settings: &Settings,
    request_id: Option<&str>,
) -> thinking::KnownBudget {
    let base = normalize_base_url(&settings.llm_base_url);
    let apply = |parsed: Option<Option<u32>>| -> Option<thinking::KnownBudget> {
        let budget = known_from_parsed(parsed)?;
        thinking::remember_budget(&base, budget);
        Some(budget)
    };
    if let Some(value) = fetch_metrics(settings).await {
        if let Some(budget) = apply(thinking::budget_from_metrics(&value, request_id)) {
            return budget;
        }
        if request_id.is_some() {
            tokio::time::sleep(Duration::from_millis(300)).await;
            if let Some(value) = fetch_metrics(settings).await {
                if let Some(budget) = apply(thinking::budget_from_metrics(&value, request_id)) {
                    return budget;
                }
            }
        }
    }
    thinking::known_budget_for(&base)
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
        return Err(http_error::ocr_failure(status.as_u16(), &text));
    }
    let v: Value = serde_json::from_str(&text).map_err(|_| "応答の解釈に失敗しました。".to_string())?;
    if v.get("error").is_some() {
        return Err("モデルが書き起こしを返しませんでした。画像に対応したモデルか確認してください。".into());
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
    fn transport_failures_stay_connection_messages() {
        let conn = http_error::transport_failure("error sending request for url: connection refused");
        assert!(conn.contains("接続できません"), "{conn}");
        let lan = http_error::transport_failure(
            "error sending request for url (http://100.65.231.111:8000/v1/models)",
        );
        assert!(lan.contains("127.0.0.1") || lan.contains("届きません"), "{lan}");
        let refused = http_error::transport_failure("tcp connect error: os error 10061");
        assert!(refused.contains("接続できません"), "{refused}");
        let timed_out = http_error::transport_failure("os error 10060");
        assert!(timed_out.contains("接続できません"), "{timed_out}");
        let unknown = http_error::transport_failure("invalid api key sk-live-secret");
        assert!(!unknown.contains("sk-live-secret"), "{unknown}");
        assert!(!unknown.contains("invalid api key"), "{unknown}");
    }

    #[test]
    fn http_status_chooses_a_fixed_message_and_drops_the_body() {
        let tokens = http_error::chat_failure(500, r#"{"message":"prompt is 14017 tokens"}"#);
        assert!(tokens.contains("HTTP 500"), "{tokens}");
        assert!(!tokens.contains("API キーが拒否"), "{tokens}");
        assert!(!tokens.contains("14017"), "{tokens}");
        let request = http_error::chat_failure(500, r#"{"request_id":"req_a401f9"}"#);
        assert!(request.contains("HTTP 500"), "{request}");
        assert!(!request.contains("req_a401f9"), "{request}");
        assert!(!request.contains("API キーが拒否"), "{request}");
        let auth = http_error::chat_failure(401, r#"{"request_id":"req_10061"}"#);
        assert!(auth.contains("API キーが拒否"), "{auth}");
        assert!(!auth.contains("req_10061"), "{auth}");
        assert!(!auth.contains("接続できません"), "{auth}");
        assert!(http_error::chat_failure(403, "Forbidden").contains("API キーが拒否"));
        let tokens_10061 = http_error::chat_failure(500, "prompt is 10061 tokens");
        assert!(tokens_10061.contains("HTTP 500"), "{tokens_10061}");
        assert!(!tokens_10061.contains("10061"), "{tokens_10061}");
        assert!(!tokens_10061.contains("接続できません"), "{tokens_10061}");
        let count = http_error::chat_failure(500, "prompt_tokens 14049");
        assert!(count.contains("HTTP 500"), "{count}");
        assert!(!count.contains("14049"), "{count}");
        let nested = http_error::chat_failure(500, "HTTP 404 Not Found");
        assert!(nested.contains("HTTP 500"), "{nested}");
        assert!(!nested.contains("404"), "{nested}");
        assert!(!nested.contains("モデルが見つかりません"), "{nested}");
        let phrase = http_error::chat_failure(500, "Unauthorized for url");
        assert!(phrase.contains("HTTP 500"), "{phrase}");
        assert!(!phrase.contains("API キーが拒否"), "{phrase}");
        assert!(!phrase.contains("Unauthorized"), "{phrase}");
        let missing = http_error::chat_failure(404, "model 'llama3.2-vision:7b' not found");
        assert!(missing.contains("指定したモデルが見つかりません"), "{missing}");
        assert!(!missing.contains("llama3"), "{missing}");
        assert!(!is_tools_unsupported_error(&missing), "{missing}");
        let tool_name = http_error::chat_failure(404, "llama3-groq-tool-use");
        assert!(!is_tools_unsupported_error(&tool_name), "{tool_name}");
        assert!(!tool_name.contains("llama3"), "{tool_name}");
        let big = http_error::chat_failure(413, "secret-body");
        assert!(big.contains("HTTP 413"), "{big}");
        assert!(!big.contains("secret-body"), "{big}");
        let limited = http_error::chat_failure(429, "secret-body");
        assert!(limited.contains("HTTP 429"), "{limited}");
        assert!(!limited.contains("secret-body"), "{limited}");
        let other = http_error::chat_failure(422, "secret-body");
        assert!(other.contains("HTTP 422"), "{other}");
        assert!(!other.contains("secret-body"), "{other}");
        let tools = http_error::chat_failure(400, "registry.ollama.ai/library/llama3:latest does not support tools");
        assert_eq!(tools, http_error::TOOLS_UNSUPPORTED_ERROR);
        let choice = http_error::chat_failure(400, r#""auto" tool choice requires --enable-auto-tool-choice"#);
        assert!(is_tools_unsupported_error(&choice), "{choice}");
        let choice_json = http_error::chat_failure(
            400,
            r#"{"error":{"message":"\"auto\" tool choice requires --enable-auto-tool-choice"}}"#,
        );
        assert!(is_tools_unsupported_error(&choice_json), "{choice_json}");
        let parsing = http_error::chat_failure(500, "error parsing tool call secret-user-text");
        assert!(parsing.contains("HTTP 500"), "{parsing}");
        assert!(!is_tools_unsupported_error(&parsing), "{parsing}");
        assert!(!parsing.contains("secret-user-text"), "{parsing}");
        assert!(!parsing.contains("tool"), "{parsing}");
        let vision = http_error::ocr_failure(400, "this model does not support image input");
        assert!(vision.contains("画像を読めません"), "{vision}");
        assert!(!vision.contains("image input"), "{vision}");
        let vision_404 = http_error::ocr_failure(404, "llama3.2-vision:7b does not support image input");
        assert!(vision_404.contains("指定したモデルが見つかりません"), "{vision_404}");
        assert!(!vision_404.contains("画像を読めません"), "{vision_404}");
        assert!(!vision_404.contains("llama3"), "{vision_404}");
        let list = http_error::models_failure(404);
        assert!(list.contains("HTTP 404"), "{list}");
        assert!(!list.contains("見つかりません"), "{list}");
        let image_on_chat = http_error::chat_failure(400, "this model does not support image input");
        assert!(image_on_chat.contains("HTTP 400"), "{image_on_chat}");
        assert!(!image_on_chat.contains("画像を読めません"), "{image_on_chat}");
        let generic_words = http_error::chat_failure(400, "tool image vision unsupported");
        assert!(!is_tools_unsupported_error(&generic_words), "{generic_words}");
        assert!(generic_words.contains("HTTP 400"), "{generic_words}");
        let too_long = http_error::chat_failure(
            400,
            r#"{"error":{"message":"the request exceeds the available context size, try increasing it","type":"exceed_context_size"}}"#,
        );
        assert_eq!(too_long, http_error::CONTEXT_TOO_LONG_ERROR);
        assert!(!too_long.contains("exceed"), "{too_long}");
        assert!(!too_long.contains("context size"), "{too_long}");
        assert!(is_context_too_long_error(&too_long));
        for phrase in [
            "maximum context length is 32768",
            "context_length_exceeded",
            "prompt is too long",
            "n_ctx exceeded",
            "too many tokens",
        ] {
            let mapped = http_error::chat_failure(400, phrase);
            assert_eq!(mapped, http_error::CONTEXT_TOO_LONG_ERROR, "{phrase}");
            assert!(!mapped.contains(phrase), "{phrase} -> {mapped}");
        }
        let context_on_500 = http_error::chat_failure(500, "context length exceeded secret-body");
        assert!(context_on_500.contains("HTTP 500"), "{context_on_500}");
        assert!(!is_context_too_long_error(&context_on_500), "{context_on_500}");
        assert!(!context_on_500.contains("secret-body"), "{context_on_500}");
        assert!(!context_on_500.contains("context length"), "{context_on_500}");
        let tools_on_404 = http_error::chat_failure(404, "model does not support tools");
        assert!(!is_tools_unsupported_error(&tools_on_404), "{tools_on_404}");
        assert!(tools_on_404.contains("指定したモデルが見つかりません"), "{tools_on_404}");
        let tools_on_500 = http_error::chat_failure(500, "does not support tools");
        assert!(!is_tools_unsupported_error(&tools_on_500), "{tools_on_500}");
        assert!(tools_on_500.contains("HTTP 500"), "{tools_on_500}");
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
        drain_sse_lines(&mut buf, &mut parse, 0, &mut |kind, d| {
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
        drain_sse_lines(&mut buf, &mut parse, 0, &mut |kind, d| {
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
        drain_sse_lines(&mut buf, &mut parse, 0, &mut |_, _| {}).unwrap();
        let out = parse.into_outcome();
        assert_eq!(out.tool_calls.len(), 1);
        assert_eq!(out.tool_calls[0].name, "search_index");
        assert!(out.tool_calls[0].arguments.contains("民法"));
    }

    fn local_llm_settings(port: u16) -> Settings {
        let mut settings = Settings::default();
        settings.llm_base_url = format!("http://127.0.0.1:{port}/v1");
        settings.llm_model = "test-model".into();
        settings.llm_timeout_ms = 5_000;
        settings
    }

    fn serve_http(status: u16, reason: &str, body: &str, times: usize) -> u16 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let reason = reason.to_string();
        let body = body.to_string();
        std::thread::spawn(move || {
            for _ in 0..times {
                let Ok((mut sock, _)) = listener.accept() else {
                    break;
                };
                let _ = sock.set_read_timeout(Some(std::time::Duration::from_secs(5)));
                let mut buf = Vec::new();
                let mut tmp = [0u8; 4096];
                loop {
                    match std::io::Read::read(&mut sock, &mut tmp) {
                        Ok(0) => break,
                        Ok(n) => {
                            buf.extend_from_slice(&tmp[..n]);
                            if let Some(header_end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                                let headers = String::from_utf8_lossy(&buf[..header_end]);
                                let len = headers
                                    .lines()
                                    .find_map(|line| {
                                        let (name, value) = line.split_once(':')?;
                                        if name.eq_ignore_ascii_case("content-length") {
                                            value.trim().parse::<usize>().ok()
                                        } else {
                                            None
                                        }
                                    })
                                    .unwrap_or(0);
                                if buf.len() - (header_end + 4) >= len {
                                    break;
                                }
                            }
                            if buf.len() > 2_000_000 {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
                let resp = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = std::io::Write::write_all(&mut sock, resp.as_bytes());
                let _ = std::io::Write::flush(&mut sock);
            }
        });
        port
    }

    async fn stream_chat_err(port: u16, tools: bool) -> String {
        stream_chat_err_model(port, "test-model", tools).await
    }

    async fn stream_chat_err_model(port: u16, model: &str, tools: bool) -> String {
        let mut settings = local_llm_settings(port);
        settings.llm_model = model.into();
        let turns = [ChatTurn::text("user", "こんにちは")];
        let schema = json!([{ "type": "function", "function": { "name": "search_index" } }]);
        stream_chat(
            &settings,
            &turns,
            Arc::new(AtomicBool::new(false)),
            if tools { Some(&schema) } else { None },
            0,
            |_, _| {},
        )
        .await
        .expect_err("the local server returns an error status")
    }

    #[tokio::test]
    async fn stream_chat_keeps_tools_unsupported_errors_detectable() {
        let ollama = r#"{"error":{"message":"registry.ollama.ai/library/llama3:latest does not support tools"}}"#;
        let ollama_port = serve_http(400, "Bad Request", ollama, 2);
        let ollama_err = stream_chat_err(ollama_port, true).await;
        assert!(
            is_tools_unsupported_error(&ollama_err),
            "ollama tools error must still reach the tool-free fallback: {ollama_err}"
        );
        assert!(!ollama_err.contains("does not support tools"), "{ollama_err}");
        assert!(!ollama_err.contains("registry.ollama.ai"), "{ollama_err}");

        let vllm = r#"{"error":{"message":"\"auto\" tool choice requires --enable-auto-tool-choice and --tool-call-parser to be set"}}"#;
        let vllm_port = serve_http(400, "Bad Request", vllm, 2);
        let vllm_err = stream_chat_err(vllm_port, true).await;
        assert!(
            is_tools_unsupported_error(&vllm_err),
            "vllm tool-choice error must still reach the tool-free fallback: {vllm_err}"
        );
        assert!(!vllm_err.contains("enable-auto-tool-choice"), "{vllm_err}");
        assert!(!vllm_err.contains("tool choice"), "{vllm_err}");
    }

    #[tokio::test]
    async fn stream_chat_maps_client_and_server_errors_from_the_response() {
        let secret_body = r#"{"error":"marker-safe-detail","api_key":"sk-live-secret","authorization":"Bearer sk-live-secret"}"#;
        // The first HTTP 400 is the thinking-field retry, so a 400 is served twice.
        let port_400 = serve_http(400, "Bad Request", secret_body, 2);
        let err_400 = stream_chat_err(port_400, false).await;
        assert!(err_400.contains("HTTP 400"), "{err_400}");
        assert!(!err_400.contains("marker-safe-detail"), "{err_400}");
        assert!(!err_400.contains("sk-live-secret"), "{err_400}");
        assert!(!err_400.to_lowercase().contains("api_key"), "{err_400}");
        assert!(!err_400.contains("Bearer"), "{err_400}");
        assert!(
            !is_tools_unsupported_error(&err_400),
            "a generic 400 must not look like a tools fallback: {err_400}"
        );

        for (code, reason) in [
            (413u16, "Payload Too Large"),
            (422, "Unprocessable Entity"),
            (429, "Too Many Requests"),
        ] {
            let body = format!(r#"{{"error":"marker-{code}","api_key":"sk-live-secret"}}"#);
            let port = serve_http(code, reason, &body, 1);
            let err = stream_chat_err(port, false).await;
            assert!(err.contains(&format!("HTTP {code}")), "{err}");
            assert!(!err.contains(&format!("marker-{code}")), "{err}");
            assert!(!err.contains("sk-live-secret"), "{err}");
            assert!(!err.to_lowercase().contains("api_key"), "{err}");
        }

        let port_500 = serve_http(
            500,
            "Internal Server Error",
            r#"{"error":"error parsing tool call","user":"秘密の依頼文をそのまま返して"}"#,
            1,
        );
        let err_500 = stream_chat_err(port_500, true).await;
        assert!(err_500.contains("HTTP 500"), "{err_500}");
        assert!(!err_500.contains("秘密の依頼文"), "{err_500}");
        assert!(!err_500.contains("error parsing tool call"), "{err_500}");
        assert!(!err_500.contains("tool"), "{err_500}");
        assert!(!is_tools_unsupported_error(&err_500), "{err_500}");

        let port_401 = serve_http(401, "Unauthorized", r#"{"request_id":"req_10061"}"#, 1);
        let err_401 = stream_chat_err(port_401, false).await;
        assert!(err_401.contains("API キーが拒否"), "{err_401}");
        assert!(!err_401.contains("req_10061"), "{err_401}");
        assert!(!err_401.contains("接続"), "{err_401}");

        for model in ["llama3.2-vision:7b", "llama3-groq-tool-use"] {
            let body = format!(r#"{{"error":"model '{model}' not found, try pulling it first"}}"#);
            let port = serve_http(404, "Not Found", &body, 1);
            let err = stream_chat_err_model(port, model, true).await;
            assert!(err.contains("指定したモデルが見つかりません"), "{model}: {err}");
            assert!(!is_tools_unsupported_error(&err), "{model}: {err}");
            assert!(!err.contains("画像を読めません"), "{model}: {err}");
            assert!(!err.contains(model), "{model}: {err}");
            assert!(!err.to_lowercase().contains("not found"), "{model}: {err}");
        }
    }

    #[tokio::test]
    async fn transcribe_image_reads_vision_unsupported_from_the_raw_status() {
        let body = r#"{"error":{"message":"this model does not support image input"}}"#;
        let port = serve_http(400, "Bad Request", body, 1);
        let err = transcribe_image(
            &local_llm_settings(port),
            "image/png",
            &[0x89, 0x50, 0x4e, 0x47],
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .expect_err("vision models reject image input with HTTP 400");
        assert!(err.contains("このモデルは画像を読めません"), "{err}");
        assert!(!err.contains("image input"), "{err}");

        let missing = r#"{"error":"model 'llama3.2-vision:7b' not found"}"#;
        let missing_port = serve_http(404, "Not Found", missing, 1);
        let mut settings = local_llm_settings(missing_port);
        settings.llm_model = "llama3.2-vision:7b".into();
        let missing_err = transcribe_image(
            &settings,
            "image/png",
            &[0x89, 0x50, 0x4e, 0x47],
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .expect_err("an unpulled vision model is HTTP 404");
        assert!(
            missing_err.contains("指定したモデルが見つかりません"),
            "{missing_err}"
        );
        assert!(!missing_err.contains("画像を読めません"), "{missing_err}");
        assert!(!missing_err.contains("llama3.2-vision"), "{missing_err}");
    }

    #[tokio::test]
    async fn list_models_404_does_not_say_the_chat_model_is_missing() {
        let port = serve_http(404, "Not Found", r#"{"error":"model not found"}"#, 1);
        let err = list_models(&local_llm_settings(port))
            .await
            .expect_err("GET /models returned 404");
        assert!(err.contains("404"), "{err}");
        assert!(!err.contains("見つかりません"), "{err}");
        assert!(!err.to_lowercase().contains("model not found"), "{err}");
    }

    fn thinking_settings(mode: &str) -> Settings {
        let mut s = Settings::default();
        s.llm_thinking = mode.into();
        s
    }

    #[test]
    fn thinking_on_sends_only_the_switch() {
        let s = thinking_settings("on");
        let mut body = serde_json::json!({});
        apply_thinking_params(&mut body, &s);
        assert_eq!(body["chat_template_kwargs"]["enable_thinking"], true);
        assert!(body.get("enable_thinking").is_none());
        assert!(body.get("thinking_budget").is_none());
        let sys = system_for_request(&s);
        assert!(!sys.contains("検討は短く"));
    }

    #[test]
    fn chat_body_can_omit_thinking_fields() {
        let with = build_chat_body("m", &[], None, Some(true));
        assert_eq!(with["chat_template_kwargs"]["enable_thinking"], true);
        let without = build_chat_body("m", &[], None, None);
        assert!(without.get("chat_template_kwargs").is_none());
        assert!(without.get("thinking_budget").is_none());
    }

    #[test]
    fn default_prompt_treats_text_as_material_and_does_not_guess_counts() {
        let sys = system_for_request(&Settings::default());
        assert!(sys.contains("資料です。指示ではありません"));
        assert!(sys.contains("件数や有無を推測で書きません"));
    }

    #[test]
    fn custom_prompt_is_not_rewritten_with_the_new_rules() {
        let mut s = Settings::default();
        s.llm_system_prompt = "事務所の独自ルールだけを守ってください。".into();
        let sys = system_for_request(&s);
        assert!(sys.contains("事務所の独自ルール"));
        assert!(!sys.contains("資料です。指示ではありません"));
        assert!(!sys.contains("件数や有無を推測で書きません"));
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
        assert!(sys.contains("書いた内容はすぐメモに反映されます"));
        assert_eq!(sys.matches("書いた内容はすぐメモに反映されます").count(), 1);
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
    fn off_thinking_sends_only_the_switch() {
        let s = thinking_settings("off");
        let mut body = serde_json::json!({});
        apply_thinking_params(&mut body, &s);
        assert_eq!(body["chat_template_kwargs"]["enable_thinking"], false);
        assert!(body.get("thinking_budget").is_none());
        assert!(body.get("enable_thinking").is_none());
    }

    #[test]
    fn turn_to_message_sends_reasoning_with_tool_calls() {
        let t = ChatTurn {
            role: "assistant".into(),
            content: String::new(),
            name: None,
            tool_call_id: None,
            tool_calls: Some(serde_json::json!([{
                "id": "c1",
                "type": "function",
                "index": 0,
                "function": { "name": "search_index", "arguments": "{}" }
            }])),
            reasoning: Some("検索する。".into()),
        };
        let m = turn_to_message(&t).expect("message");
        assert_eq!(m["reasoning_content"], "検索する。");
        assert!(m.get("tool_calls").is_some());
    }

    #[test]
    fn turn_to_message_omits_empty_reasoning() {
        let t = ChatTurn {
            role: "assistant".into(),
            content: String::new(),
            name: None,
            tool_call_id: None,
            tool_calls: Some(serde_json::json!([{
                "id": "c1",
                "type": "function",
                "index": 0,
                "function": { "name": "search_index", "arguments": "{}" }
            }])),
            reasoning: Some("  ".into()),
        };
        let m = turn_to_message(&t).expect("message");
        assert!(m.get("reasoning_content").is_none());
    }

    #[test]
    fn turn_to_message_caps_long_reasoning() {
        let long = "あ".repeat(crate::llm::context::REASONING_CARRY_CAP + 10);
        let t = ChatTurn {
            role: "assistant".into(),
            content: String::new(),
            name: None,
            tool_call_id: None,
            tool_calls: Some(serde_json::json!([{
                "id": "c1",
                "type": "function",
                "index": 0,
                "function": { "name": "search_index", "arguments": "{}" }
            }])),
            reasoning: Some(long),
        };
        let m = turn_to_message(&t).expect("message");
        let sent = m["reasoning_content"].as_str().unwrap();
        assert_eq!(
            sent.chars().count(),
            crate::llm::context::REASONING_CARRY_CAP
        );
        assert!(sent.ends_with('…'));
    }

    #[test]
    fn into_outcome_keeps_reasoning() {
        let parse = StreamParse {
            content: "答え".into(),
            reasoning: "検討".into(),
            tool_calls: Vec::new(),
            finish_reason: String::new(),
            request_id: String::new(),
        };
        let out = parse.into_outcome();
        assert_eq!(out.reasoning, "検討");
        assert_eq!(out.content, "答え");
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
            finish_reason: String::new(),
            request_id: String::new(),
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
        drain_sse_lines(&mut buf, &mut parse, 0, &mut |_, _| {}).unwrap();
        let out = parse.into_outcome();
        assert_eq!(out.tool_calls.len(), 1);
        assert_eq!(out.tool_calls[0].name, "read_unit");
        assert!(out.tool_calls[0].arguments.contains("u1"));
    }
}
