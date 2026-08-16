use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};

use crate::db::{LlmMessageRow, LlmSourceRow, LlmThreadRow};
use crate::llm::{self, LlmModelInfo};
use crate::llm::context::{assemble_turns, ChatTurn};
use crate::llm::tools::{self, MAX_TOOL_ROUNDS, ToolExec};
use crate::state::AppState;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmChatDelta {
    pub request_id: String,
    pub thread_id: String,
    pub text: String,
    pub kind: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmChatError {
    pub request_id: String,
    pub thread_id: String,
    pub message: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmSendResult {
    pub thread: LlmThreadRow,
    pub user_message: LlmMessageRow,
    pub assistant_message: Option<LlmMessageRow>,
    pub cancelled: bool,
    pub error: Option<String>,
    pub truncated: bool,
    pub context_chars: usize,
    pub warning: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmTestResult {
    pub message: String,
    pub loopback: bool,
    pub models: Vec<LlmModelInfo>,
}

fn title_from_content(content: &str) -> String {
    let line = content.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let t: String = line.chars().take(40).collect();
    let t = t.trim();
    if t.is_empty() {
        "新しい会話".into()
    } else {
        t.to_string()
    }
}

/// Clears the LLM busy flag even if `llm_send` panics (e.g. dropping a blocking HTTP client).
struct LlmBusyGuard {
    state: Arc<AppState>,
    request_id: String,
    done: bool,
}

impl LlmBusyGuard {
    fn new(state: Arc<AppState>, request_id: String) -> Self {
        Self {
            state,
            request_id,
            done: false,
        }
    }

    fn finish(&mut self) {
        if !self.done {
            self.state.finish_llm(&self.request_id);
            self.done = true;
        }
    }
}

impl Drop for LlmBusyGuard {
    fn drop(&mut self) {
        self.finish();
    }
}

async fn execute_tool_off_runtime(
    state: Arc<AppState>,
    thread_id: String,
    name: String,
    arguments: String,
    thread_scope: Option<String>,
    next_cite: i64,
) -> (ToolExec, i64) {
    match tokio::task::spawn_blocking(move || {
        let mut n = next_cite;
        let exec = tools::execute_tool(
            &state,
            &thread_id,
            &name,
            &arguments,
            thread_scope.as_deref(),
            &mut n,
        );
        (exec, n)
    })
    .await
    {
        Ok(pair) => pair,
        Err(e) => (
            ToolExec {
                content: format!("ツール実行に失敗しました: {e}"),
                consumed: Vec::new(),
            },
            next_cite,
        ),
    }
}

/// Trim one tool result to what is left of the reserve, keeping the head (the highest
/// scoring hits) and telling the model the rest was cut.
fn fit_tool_content(content: String, budget: usize, used: usize) -> String {
    let left = budget.saturating_sub(used);
    if left == 0 {
        return "（文脈の上限に達したため、この結果は省略しました）".into();
    }
    if content.chars().count() <= left {
        return content;
    }
    let mut out: String = content.chars().take(left).collect();
    out.push_str("\n（以下、文脈の上限により省略）");
    out
}

#[tauri::command]
pub fn show_chat_window(app: AppHandle) {
    crate::show_chat(&app);
}

#[tauri::command]
pub fn llm_list_threads(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<LlmThreadRow>, String> {
    state.db.list_llm_threads().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn llm_create_thread(
    state: State<'_, Arc<AppState>>,
    title: Option<String>,
) -> Result<LlmThreadRow, String> {
    let title = title.unwrap_or_default();
    let thread = state
        .db
        .create_llm_thread(title.trim(), true)
        .map_err(|e| e.to_string())?;
    state
        .db
        .set_active_llm_thread_id(Some(&thread.id))
        .map_err(|e| e.to_string())?;
    Ok(thread)
}

#[tauri::command]
pub fn llm_rename_thread(
    state: State<'_, Arc<AppState>>,
    id: String,
    title: String,
) -> Result<LlmThreadRow, String> {
    state
        .db
        .rename_llm_thread(&id, title.trim())
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "会話が見つかりません".into())
}

/// Restrict this thread's index searches to the given folders (or `mailfolder:<name>`).
/// An empty list clears the scope. Multiple paths are stored newline-separated.
#[tauri::command]
pub fn llm_set_thread_scope(
    state: State<'_, Arc<AppState>>,
    id: String,
    path_prefixes: Vec<String>,
) -> Result<LlmThreadRow, String> {
    let joined = tools::join_thread_scopes(&path_prefixes);
    state
        .db
        .set_llm_thread_scope(&id, &joined)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "会話が見つかりません".into())
}

#[tauri::command]
pub fn llm_delete_thread(state: State<'_, Arc<AppState>>, id: String) -> Result<(), String> {
    if !state.db.delete_llm_thread(&id).map_err(|e| e.to_string())? {
        return Err("会話が見つかりません".into());
    }
    if state.db.get_active_llm_thread_id().as_deref() == Some(id.as_str()) {
        let next = state
            .db
            .list_llm_threads()
            .map_err(|e| e.to_string())?
            .into_iter()
            .next();
        state
            .db
            .set_active_llm_thread_id(next.as_ref().map(|t| t.id.as_str()))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub fn llm_get_active_thread(
    state: State<'_, Arc<AppState>>,
) -> Result<Option<LlmThreadRow>, String> {
    let Some(id) = state.db.get_active_llm_thread_id() else {
        return Ok(None);
    };
    state.db.get_llm_thread(&id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn llm_set_active_thread(
    state: State<'_, Arc<AppState>>,
    id: String,
) -> Result<LlmThreadRow, String> {
    let thread = state
        .db
        .get_llm_thread(&id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "会話が見つかりません".to_string())?;
    state
        .db
        .set_active_llm_thread_id(Some(&thread.id))
        .map_err(|e| e.to_string())?;
    Ok(thread)
}

#[tauri::command]
pub fn llm_list_messages(
    state: State<'_, Arc<AppState>>,
    thread_id: String,
) -> Result<Vec<LlmMessageRow>, String> {
    state
        .db
        .list_llm_messages(&thread_id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn llm_test_connection(
    state: State<'_, Arc<AppState>>,
) -> Result<LlmTestResult, String> {
    let settings = state.settings.read().clone();
    let loopback = llm::is_loopback_url(&settings.llm_base_url);
    let (message, models) = llm::test_connection(&settings).await?;
    Ok(LlmTestResult {
        message,
        loopback,
        models,
    })
}

#[tauri::command]
pub async fn llm_list_models(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<LlmModelInfo>, String> {
    let settings = state.settings.read().clone();
    llm::list_models(&settings).await
}

#[tauri::command]
pub fn llm_cancel(state: State<'_, Arc<AppState>>) {
    state.cancel_llm();
}

#[tauri::command]
pub async fn llm_send(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    thread_id: Option<String>,
    content: String,
) -> Result<LlmSendResult, String> {
    let content = content.trim().to_string();
    if content.is_empty() {
        return Err("メッセージが空です。".into());
    }
    if state.is_llm_busy() {
        return Err("生成中です。停止してから送信してください。".into());
    }

    let thread = match thread_id.filter(|s| !s.trim().is_empty()) {
        Some(id) => state
            .db
            .get_llm_thread(&id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "会話が見つかりません".to_string())?,
        None => {
            let title = title_from_content(&content);
            state
                .db
                .create_llm_thread(&title, true)
                .map_err(|e| e.to_string())?
        }
    };
    state
        .db
        .set_active_llm_thread_id(Some(&thread.id))
        .map_err(|e| e.to_string())?;

    if thread.title.trim().is_empty() {
        let _ = state
            .db
            .rename_llm_thread(&thread.id, &title_from_content(&content));
    }

    let user_message = state
        .db
        .insert_llm_message(&thread.id, "user", &content)
        .map_err(|e| e.to_string())?;

    let history = state
        .db
        .list_llm_messages(&thread.id)
        .map_err(|e| e.to_string())?;
    let sources = state
        .db
        .list_llm_sources(&thread.id)
        .map_err(|e| e.to_string())?;
    let settings = state.settings.read().clone();
    let max_chars = settings.llm_max_context_chars as usize;
    let thread_scope = Some(thread.path_prefix.trim().to_string()).filter(|s| !s.is_empty());
    let mut system = llm::system_for_request(&settings);
    llm::append_diagram_hint(&mut system, &content);
    // Without this the model reads an empty result as "nothing exists" and keeps
    // rephrasing, instead of reporting that the folder does not contain the answer.
    if let Some(line) = tools::format_thread_scope_system_line(&thread.path_prefix) {
        system.push_str(&line);
    }
    let (assembled, stats) = assemble_turns(&system, &sources, &history, max_chars);
    let turns_for_plain = llm::apply_thinking_to_turns(assembled.clone(), &settings);
    let truncated = stats.truncated;
    let context_chars = stats.total_chars;
    let mut consumed: Vec<(String, i64)> = stats
        .consumed
        .iter()
        .map(|c| (c.id.clone(), c.cite_no))
        .collect();
    let mut next_cite = state
        .db
        .max_llm_cite_no(&thread.id)
        .unwrap_or(0)
        .max(consumed.iter().map(|(_, n)| *n).max().unwrap_or(0));
    let request_id = uuid::Uuid::new_v4().to_string();
    let cancel = Arc::new(AtomicBool::new(false));
    let state_arc = state.inner().clone();
    state.start_llm(request_id.clone(), thread.id.clone(), cancel.clone());
    let mut busy = LlmBusyGuard::new(state_arc.clone(), request_id.clone());

    let tools_schema = tools::tools_schema();
    let mut use_tools = true;
    let mut retried_without_tools = false;
    let mut extra_final = false;
    let mut warning: Option<String> = None;
    let mut rounds = 0usize;
    let mut current_turns = assembled;
    // Tool output is appended after the context was already sized, so it needs its own
    // allowance. A quarter of the window leaves room for the history and the answer.
    let tool_budget = (max_chars / 4).max(2_000);
    let mut tool_used = 0usize;
    let result_text;

    loop {
        let with_tools = use_tools;
        let disable_thinking = with_tools;
        let turns_now = if with_tools {
            llm::turns_for_tool_round(current_turns.clone())
        } else {
            llm::apply_thinking_to_turns(current_turns.clone(), &settings)
        };
        let outcome = {
            let app2 = app.clone();
            let req = request_id.clone();
            let tid = thread.id.clone();
            llm::stream_chat(
                &settings,
                &turns_now,
                cancel.clone(),
                if with_tools {
                    Some(&tools_schema)
                } else {
                    None
                },
                disable_thinking,
                move |kind, delta| {
                    let _ = app2.emit(
                        "llm-chat-delta",
                        LlmChatDelta {
                            request_id: req.clone(),
                            thread_id: tid.clone(),
                            text: delta.to_string(),
                            kind: kind.to_string(),
                        },
                    );
                },
            )
            .await
        };

        match outcome {
            Err(e) if llm::is_cancelled_error(&e) => {
                busy.finish();
                let _ = state.db.delete_uncited_tool_sources(&thread.id);
                emit_sources_updated(&app, &thread.id);
                let thread = state
                    .db
                    .get_llm_thread(&thread.id)
                    .map_err(|e| e.to_string())?
                    .unwrap_or(thread);
                return Ok(LlmSendResult {
                    thread,
                    user_message,
                    assistant_message: None,
                    cancelled: true,
                    error: None,
                    truncated,
                    context_chars,
                    warning,
                });
            }
            Err(e) if use_tools && !retried_without_tools && llm::is_tools_unsupported_error(&e) => {
                use_tools = false;
                retried_without_tools = true;
                warning = Some(
                    "このモデルはインデックス検索ツールに対応していません。検索窓から出典を送ってください。"
                        .into(),
                );
                current_turns = turns_for_plain.clone();
                continue;
            }
            Err(e) => {
                busy.finish();
                let _ = state.db.delete_uncited_tool_sources(&thread.id);
                emit_sources_updated(&app, &thread.id);
                let _ = app.emit(
                    "llm-chat-error",
                    LlmChatError {
                        request_id,
                        thread_id: thread.id.clone(),
                        message: e.clone(),
                    },
                );
                let thread = state
                    .db
                    .get_llm_thread(&thread.id)
                    .map_err(|err| err.to_string())?
                    .unwrap_or(thread);
                return Ok(LlmSendResult {
                    thread,
                    user_message,
                    assistant_message: None,
                    cancelled: false,
                    error: Some(e),
                    truncated,
                    context_chars,
                    warning,
                });
            }
            Ok(out) => {
                if use_tools && !out.tool_calls.is_empty() && rounds < MAX_TOOL_ROUNDS {
                    rounds += 1;
                    let tool_calls_json = serde_json::json!(out
                        .tool_calls
                        .iter()
                        .enumerate()
                        .map(|(i, c)| serde_json::json!({
                            "id": c.id,
                            "type": "function",
                            "index": i,
                            "function": { "name": c.name, "arguments": c.arguments }
                        }))
                        .collect::<Vec<_>>());
                    current_turns.push(ChatTurn {
                        role: "assistant".into(),
                        content: out.content,
                        name: None,
                        tool_call_id: None,
                        tool_calls: Some(tool_calls_json),
                    });
                    for tc in out.tool_calls {
                        let _ = app.emit(
                            "llm-chat-delta",
                            LlmChatDelta {
                                request_id: request_id.clone(),
                                thread_id: thread.id.clone(),
                                text: format!("{}…", tc.name),
                                kind: "tool".into(),
                            },
                        );
                        let (exec, new_cite) = execute_tool_off_runtime(
                            state_arc.clone(),
                            thread.id.clone(),
                            tc.name.clone(),
                            tc.arguments.clone(),
                            thread_scope.clone(),
                            next_cite,
                        )
                        .await;
                        next_cite = new_cite;
                        consumed.extend(exec.consumed);
                        emit_sources_updated(&app, &thread.id);
                        // `assemble_turns` sized the context before any tool ran, so tool
                        // output is pure overflow. Trim it to the reserve instead of
                        // letting the request exceed the window and get truncated by the
                        // server, which would drop the question itself.
                        let content = fit_tool_content(exec.content, tool_budget, tool_used);
                        tool_used += content.chars().count();
                        current_turns.push(ChatTurn {
                            role: "tool".into(),
                            content,
                            name: Some(tc.name),
                            tool_call_id: Some(tc.id),
                            tool_calls: None,
                        });
                    }
                    // The reserve is spent; answer from what the rounds already produced.
                    if tool_used >= tool_budget {
                        use_tools = false;
                        extra_final = true;
                        current_turns.push(ChatTurn::text(
                            "user",
                            "ツールはこれ以上使わず、これまでに得た出典だけで答えてください。",
                        ));
                    }
                    continue;
                }
                if use_tools
                    && !out.tool_calls.is_empty()
                    && rounds >= MAX_TOOL_ROUNDS
                    && !extra_final
                {
                    extra_final = true;
                    use_tools = false;
                    current_turns.push(ChatTurn::text(
                        "user",
                        "ツールはこれ以上使わず、これまでに得た出典だけで答えてください。",
                    ));
                    continue;
                }
                result_text = out.content;
                break;
            }
        }
    }

    busy.finish();

    let assistant_message = if result_text.is_empty() {
        None
    } else {
        Some(
            state
                .db
                .insert_llm_message(&thread.id, "assistant", &result_text)
                .map_err(|e| e.to_string())?,
        )
    };
    if let Some(assistant) = assistant_message.as_ref() {
        if !consumed.is_empty() {
            state
                .db
                .consume_llm_sources(&consumed, &user_message.id, &assistant.id)
                .map_err(|e| e.to_string())?;
            emit_sources_updated(&app, &thread.id);
        }
    } else {
        let _ = state.db.delete_uncited_tool_sources(&thread.id);
        emit_sources_updated(&app, &thread.id);
    }
    let thread = state
        .db
        .get_llm_thread(&thread.id)
        .map_err(|e| e.to_string())?
        .unwrap_or(thread);
    Ok(LlmSendResult {
        thread,
        user_message,
        assistant_message,
        cancelled: false,
        error: if result_text.trim().is_empty() {
            Some(
                "モデルが本文を返しませんでした。思考を「オフ」にするか、検索窓から出典を送ってください。"
                    .into(),
            )
        } else {
            None
        },
        truncated,
        context_chars,
        warning,
    })
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmSourcesUpdated {
    pub thread_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmAttachItem {
    pub path: String,
    pub title: Option<String>,
    pub paragraph_id: Option<String>,
    pub body: String,
    pub query: Option<String>,
    pub origin: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmAttachResult {
    pub thread: LlmThreadRow,
    pub sources: Vec<LlmSourceRow>,
    pub added: usize,
    pub skipped: usize,
    pub created_thread: bool,
}

fn emit_sources_updated(app: &AppHandle, thread_id: &str) {
    let _ = app.emit(
        "llm-sources-updated",
        LlmSourcesUpdated {
            thread_id: thread_id.to_string(),
        },
    );
}

fn attach_thread_title(explicit: Option<&str>, items: &[LlmAttachItem]) -> String {
    let from_arg = explicit.map(str::trim).filter(|s| !s.is_empty());
    if let Some(t) = from_arg {
        return t.to_string();
    }
    for item in items {
        if let Some(t) = item.title.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            return t.to_string();
        }
    }
    "新しい会話".into()
}

#[tauri::command]
pub fn llm_list_sources(
    state: State<'_, Arc<AppState>>,
    thread_id: String,
) -> Result<Vec<LlmSourceRow>, String> {
    state
        .db
        .list_llm_sources(&thread_id)
        .map_err(|e| e.to_string())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmFilePreview {
    pub chars: usize,
    pub units: usize,
    pub hard_cap: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmGrainResult {
    pub source: LlmSourceRow,
    pub sources: Vec<LlmSourceRow>,
    pub removed: usize,
}

#[tauri::command]
pub fn llm_remove_source(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    id: String,
) -> Result<(), String> {
    let thread_id = state
        .db
        .delete_llm_source(&id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "出典が見つかりません".to_string())?;
    emit_sources_updated(&app, &thread_id);
    Ok(())
}

fn load_file_body(state: &AppState, path: &str) -> Result<(String, usize), String> {
    let units = llm::grain::collect_path_units(&state.backend, &state.mail_backend, path)?;
    let n = units.len();
    let body = llm::grain::file_body_from_units(&units)?;
    Ok((body, n))
}

#[tauri::command]
pub fn llm_preview_source_file(
    state: State<'_, Arc<AppState>>,
    id: String,
) -> Result<LlmFilePreview, String> {
    let row = state
        .db
        .get_llm_source(&id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "出典が見つかりません".to_string())?;
    let path = row.path.trim();
    if path.is_empty() {
        return Err("パスが無い出典はファイル全体にできません。".into());
    }
    let (body, units) = load_file_body(state.inner(), path)?;
    Ok(LlmFilePreview {
        chars: llm::grain::char_len(&body),
        units,
        hard_cap: llm::grain::FILE_GRAIN_HARD_CAP,
    })
}

#[tauri::command]
pub fn llm_set_source_grain(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    id: String,
    grain: String,
) -> Result<LlmGrainResult, String> {
    let row = state
        .db
        .get_llm_source(&id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "出典が見つかりません".to_string())?;
    if !row.is_pending() {
        return Err("読み込み済みの出典は段落／全文を切り替えられません。".into());
    }
    let want_file = llm::grain::is_file_grain(&grain);
    let path = row.path.trim();
    if want_file && path.is_empty() {
        return Err("パスが無い出典はファイル全体にできません。".into());
    }

    let body = if want_file {
        let (body, _) = load_file_body(state.inner(), path)?;
        body
    } else if let Some(saved) = llm::grain::saved_unit_body(&row.unit_body) {
        saved.to_string()
    } else {
        let settings = state.settings.read().clone();
        llm::grain::unit_body_from_index(
            &settings,
            &state.backend,
            &state.mail_backend,
            &row.paragraph_id,
        )?
    };

    let source = if want_file {
        let snapshot = if row.unit_body.trim().is_empty() {
            row.body.as_str()
        } else {
            row.unit_body.as_str()
        };
        state
            .db
            .update_llm_source_grain(&id, "file", &body, Some(snapshot))
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "出典が見つかりません".to_string())?
    } else {
        state
            .db
            .update_llm_source_grain(&id, "unit", &body, None)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "出典が見つかりません".to_string())?
    };
    let removed = if want_file {
        state
            .db
            .delete_other_llm_sources_for_path(&row.thread_id, &row.id, path)
            .map_err(|e| e.to_string())?
    } else {
        0
    };
    let sources = state
        .db
        .list_llm_sources(&row.thread_id)
        .map_err(|e| e.to_string())?;
    emit_sources_updated(&app, &row.thread_id);
    Ok(LlmGrainResult {
        source,
        sources,
        removed,
    })
}

#[tauri::command]
pub fn llm_attach_sources(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    items: Vec<LlmAttachItem>,
    title: Option<String>,
    thread_id: Option<String>,
) -> Result<LlmAttachResult, String> {
    if items
        .iter()
        .all(|it| it.body.trim().is_empty() && it.path.trim().is_empty())
    {
        return Err("添付する出典がありません。".into());
    }

    let want_new = match thread_id.as_deref().map(str::trim) {
        None | Some("") | Some("new") => true,
        Some(_) => false,
    };
    let created_thread;
    let thread = if want_new {
        let title = attach_thread_title(title.as_deref(), &items);
        created_thread = true;
        state
            .db
            .create_llm_thread(&title, false)
            .map_err(|e| e.to_string())?
    } else {
        created_thread = false;
        let id = thread_id.as_deref().unwrap_or("").trim();
        state
            .db
            .get_llm_thread(id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "会話が見つかりません".to_string())?
    };

    if !state.is_llm_busy() {
        state
            .db
            .set_active_llm_thread_id(Some(&thread.id))
            .map_err(|e| e.to_string())?;
    }

    let mut added = 0usize;
    let mut skipped = 0usize;
    for item in &items {
        let body = item.body.trim();
        if body.is_empty() {
            skipped += 1;
            continue;
        }
        let origin = item.origin.as_deref().unwrap_or("attach");
        let path = item.path.trim();
        let src_title = item
            .title
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(path);
        let paragraph_id = item.paragraph_id.as_deref().unwrap_or("");
        let query = item.query.as_deref().unwrap_or("");
        let (_row, created) = state
            .db
            .insert_llm_source(
                &thread.id,
                origin,
                path,
                src_title,
                paragraph_id,
                body,
                query,
            )
            .map_err(|e| e.to_string())?;
        if created {
            added += 1;
        } else {
            skipped += 1;
        }
    }

    if added == 0 && skipped == items.len() && items.iter().all(|it| it.body.trim().is_empty()) {
        return Err("本文が空の出典は添付できません。".into());
    }

    let sources = state
        .db
        .list_llm_sources(&thread.id)
        .map_err(|e| e.to_string())?;
    let thread = state
        .db
        .get_llm_thread(&thread.id)
        .map_err(|e| e.to_string())?
        .unwrap_or(thread);

    crate::show_chat(&app);
    emit_sources_updated(&app, &thread.id);

    Ok(LlmAttachResult {
        thread,
        sources,
        added,
        skipped,
        created_thread,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tool output is appended after `assemble_turns` already sized the context, so it has
    /// to be trimmed here or the request overflows the window.
    #[test]
    fn tool_results_are_trimmed_to_the_reserve() {
        let body = "あ".repeat(100);
        assert_eq!(
            fit_tool_content(body.clone(), 500, 0),
            body,
            "content within the reserve passes through untouched"
        );

        let trimmed = fit_tool_content(body.clone(), 60, 0);
        assert!(trimmed.chars().count() < body.chars().count());
        assert!(
            trimmed.contains("省略"),
            "the model must be told the result was cut: {trimmed}"
        );

        let exhausted = fit_tool_content(body, 60, 60);
        assert!(
            exhausted.contains("省略"),
            "a spent reserve yields a notice, not silence: {exhausted}"
        );
        assert!(exhausted.chars().count() < 40);
    }
}

