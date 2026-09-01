use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};

use crate::db::{LlmMessageRow, LlmSourceRow, LlmThreadRow, NoteReview};
use crate::llm::context::{
    assemble_turns, consumed_cited_in_answer, final_source_turn, sources_for_consumed, ChatTurn,
    STOP_TOOLS_HINT,
};
use crate::llm::tools::{self, ToolExec};
use crate::llm::{self, files, LlmModelInfo};
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

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmOcrStatus {
    pub thread_id: String,
    pub active: bool,
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
    #[serde(default)]
    pub note_writes: Vec<NoteWriteNotice>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteWriteNotice {
    pub note_id: String,
    pub title: String,
    pub has_review: bool,
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
    web_search: bool,
    request_id: String,
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
            web_search,
            &request_id,
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
                wrote_note_id: None,
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

/// Put this-turn hits on a user turn so the model sees a real 出典 block (not only `role: tool`).
fn inject_final_source_turn(
    turns: &mut Vec<ChatTurn>,
    sources: &[LlmSourceRow],
    consumed: &[(String, i64)],
    stop_tools: bool,
) -> bool {
    let rows = sources_for_consumed(sources, consumed);
    if let Some(turn) = final_source_turn(&rows, stop_tools) {
        turns.push(turn);
        return true;
    }
    false
}

#[tauri::command]
pub fn show_chat_window(app: AppHandle) {
    crate::show_chat(&app);
}

#[tauri::command]
pub fn llm_list_threads(state: State<'_, Arc<AppState>>) -> Result<Vec<LlmThreadRow>, String> {
    state.db.list_llm_threads().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn llm_search_threads(
    state: State<'_, Arc<AppState>>,
    query: String,
) -> Result<Vec<String>, String> {
    state
        .db
        .search_llm_thread_ids(&query)
        .map_err(|e| e.to_string())
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

/// Bind this conversation to a note. Empty `note_id` clears the binding.
#[tauri::command]
pub fn llm_set_thread_note(
    state: State<'_, Arc<AppState>>,
    id: String,
    note_id: String,
) -> Result<LlmThreadRow, String> {
    let note_id = note_id.trim();
    if !note_id.is_empty()
        && state
            .db
            .get_note(note_id)
            .map_err(|e| e.to_string())?
            .is_none()
    {
        return Err("ノートが見つかりません".into());
    }
    state
        .db
        .set_llm_thread_note(&id, note_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "会話が見つかりません".into())
}

#[tauri::command]
pub fn get_note_review(
    state: State<'_, Arc<AppState>>,
    note_id: String,
) -> Result<Option<NoteReview>, String> {
    state.db.get_note_review(&note_id).map_err(|e| e.to_string())
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteReviewEvent {
    pub thread_id: String,
    pub note_id: String,
}

fn emit_note_review(app: &AppHandle, thread_id: &str, note_id: &str) {
    let _ = app.emit(
        "llm-note-review",
        NoteReviewEvent {
            thread_id: thread_id.to_string(),
            note_id: note_id.to_string(),
        },
    );
}

fn after_llm_note_write(app: &AppHandle, state: &AppState, thread_id: &str, note_id: &str) {
    crate::commands::emit_note_updated_from(app, note_id, "memo", "llm");
    emit_note_review(app, thread_id, note_id);
    if !crate::notes_window_visible(app) {
        let _ = state.db.set_active_note_id(Some(note_id));
    }
}

fn note_write_notices(state: &AppState, ids: &[String]) -> Vec<NoteWriteNotice> {
    let mut out = Vec::new();
    for id in ids {
        let review = state.db.get_note_review(id).ok().flatten();
        let (title, has_review) = match &review {
            Some(r) => (r.note_title.clone(), r.has_review),
            None => (String::new(), false),
        };
        out.push(NoteWriteNotice {
            note_id: id.clone(),
            title,
            has_review,
        });
    }
    out
}

fn review_event_after(app: &AppHandle, review: NoteReview, memo_changed: bool) -> NoteReview {
    if memo_changed {
        crate::commands::emit_note_updated_from(app, &review.note_id, "memo", "");
    }
    emit_note_review(app, "", &review.note_id);
    review
}

#[tauri::command]
pub fn ack_note_review(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    note_id: String,
    memo_len: u32,
) -> Result<NoteReview, String> {
    let review = state.db.ack_note_review(&note_id, memo_len)?;
    Ok(review_event_after(&app, review, false))
}

#[tauri::command]
pub fn revert_note_review(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    note_id: String,
    memo_len: u32,
) -> Result<NoteReview, String> {
    let review = state.db.revert_note_review(&note_id, memo_len)?;
    Ok(review_event_after(&app, review, true))
}

#[tauri::command]
pub fn keep_note_hunk(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    note_id: String,
    hunk_index: u32,
    base_len: u32,
    memo_len: u32,
) -> Result<NoteReview, String> {
    let review = state
        .db
        .keep_note_hunk(&note_id, hunk_index, base_len, memo_len)?;
    Ok(review_event_after(&app, review, false))
}

#[tauri::command]
pub fn revert_note_hunk(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    note_id: String,
    hunk_index: u32,
    base_len: u32,
    memo_len: u32,
) -> Result<NoteReview, String> {
    let review = state
        .db
        .revert_note_hunk(&note_id, hunk_index, base_len, memo_len)?;
    Ok(review_event_after(&app, review, true))
}

#[tauri::command]
pub fn llm_reorder_threads(
    state: State<'_, Arc<AppState>>,
    ordered_ids: Vec<String>,
) -> Result<(), String> {
    state
        .db
        .reorder_llm_threads(&ordered_ids)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn llm_delete_thread(state: State<'_, Arc<AppState>>, id: String) -> Result<(), String> {
    if !state.db.delete_llm_thread(&id).map_err(|e| e.to_string())? {
        return Err("会話が見つかりません".into());
    }
    files::remove_thread_store(&state.data_dir, &id);
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
pub async fn llm_test_connection(state: State<'_, Arc<AppState>>) -> Result<LlmTestResult, String> {
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
pub async fn llm_list_models(state: State<'_, Arc<AppState>>) -> Result<Vec<LlmModelInfo>, String> {
    let settings = state.settings.read().clone();
    llm::list_models(&settings).await
}

#[tauri::command]
pub fn llm_cancel(app: AppHandle, state: State<'_, Arc<AppState>>) {
    if let Some(job) = state.cancel_llm() {
        if job.kind == "ocr" {
            let _ = state.db.fail_pending_ocr(&job.thread_id);
            emit_sources_updated(&app, &job.thread_id);
            emit_ocr_status(&app, &job.thread_id, false);
        }
    }
}

#[tauri::command]
pub async fn llm_send(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    thread_id: Option<String>,
    content: String,
    web_search: Option<bool>,
) -> Result<LlmSendResult, String> {
    let web_search = web_search.unwrap_or(false);
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

    let paste = if crate::llm::fetch_url::message_may_contain_url(&content) {
        let state_for_fetch = state.inner().clone();
        let tid = thread.id.clone();
        let msg = content.clone();
        tokio::task::spawn_blocking(move || {
            crate::llm::fetch_url::attach_pasted_urls(&state_for_fetch, &tid, &msg)
        })
        .await
        .unwrap_or_else(|_| crate::llm::fetch_url::PasteAttachResult::default())
    } else {
        crate::llm::fetch_url::PasteAttachResult::default()
    };
    if paste.attached > 0 || !paste.failures.is_empty() {
        emit_sources_updated(&app, &thread.id);
    }
    let mut warning = paste.warning_line();

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
    let bound_note = {
        let nid = thread.note_id.trim();
        if nid.is_empty() {
            None
        } else {
            state.db.get_note(nid).ok().flatten()
        }
    };
    if let Some(n) = &bound_note {
        system.push_str(&tools::format_note_target_system_line(&n.title, &n.memo));
    }
    system.push_str(&tools::format_search_date_system_line(
        settings.mail_days_back,
    ));
    let web_search = web_search && !settings.searxng_url.trim().is_empty();
    if web_search {
        system.push_str(&tools::format_web_search_system_line());
    }
    if let Some(line) = paste.system_line() {
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
    persist_image_group_cites(&state.db, &sources, &consumed);
    let mut next_cite = state
        .db
        .max_llm_cite_no(&thread.id)
        .unwrap_or(0)
        .max(consumed.iter().map(|(_, n)| *n).max().unwrap_or(0));
    let request_id = uuid::Uuid::new_v4().to_string();
    let cancel = Arc::new(AtomicBool::new(false));
    let state_arc = state.inner().clone();
    if !state.start_llm(
        request_id.clone(),
        thread.id.clone(),
        "send",
        cancel.clone(),
    ) {
        return Err("生成中です。停止してから送信してください。".into());
    }
    let mut busy = LlmBusyGuard::new(state_arc.clone(), request_id.clone());

    let tools_schema = tools::tools_schema(web_search);
    let mut use_tools = true;
    let mut retried_without_tools = false;
    let mut extra_final = false;
    let mut rounds = 0usize;
    let max_rounds = tools::max_tool_rounds(web_search);
    let mut current_turns = assembled;
    let mut source_turn_injected = false;
    // Tool output is appended after the context was already sized, so it needs its own
    // allowance. A quarter of the window leaves room for the history and the answer.
    let tool_budget = (max_chars / 4).max(2_000);
    let mut tool_used = 0usize;
    let mut read_note_called = false;
    let mut wrote_note_ids: Vec<String> = Vec::new();
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
                kick_pending_ocr(&app, state_arc.clone());
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
                    note_writes: note_write_notices(state.inner().as_ref(), &wrote_note_ids),
                });
            }
            Err(e)
                if use_tools && !retried_without_tools && llm::is_tools_unsupported_error(&e) =>
            {
                use_tools = false;
                retried_without_tools = true;
                warning = Some(match warning.take() {
                    Some(prev) => format!(
                        "{prev} このモデルはインデックス検索とノート編集のツールに対応していません。検索窓から出典を送ってください。"
                    ),
                    None => {
                        "このモデルはインデックス検索とノート編集のツールに対応していません。検索窓から出典を送ってください。"
                            .into()
                    }
                });
                current_turns = turns_for_plain.clone();
                continue;
            }
            Err(e) => {
                busy.finish();
                kick_pending_ocr(&app, state_arc.clone());
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
                    note_writes: note_write_notices(state.inner().as_ref(), &wrote_note_ids),
                });
            }
            Ok(out) => {
                if use_tools && !out.tool_calls.is_empty() && rounds < max_rounds {
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
                    let mut read_url_in_round = 0usize;
                    for tc in out.tool_calls {
                        let hint = if web_search && tc.name == tools::TOOL_SEARCH {
                            "search_index / search_web…".to_string()
                        } else {
                            format!("{}…", tc.name)
                        };
                        let _ = app.emit(
                            "llm-chat-delta",
                            LlmChatDelta {
                                request_id: request_id.clone(),
                                thread_id: thread.id.clone(),
                                text: hint,
                                kind: "tool".into(),
                            },
                        );
                        if tc.name == tools::TOOL_READ_URL
                            && read_url_in_round >= tools::MAX_READ_URL_PER_ROUND
                        {
                            let content = fit_tool_content(
                                "一度に読める URL は 2 件までです。必要なものから順に read_url してください。"
                                    .into(),
                                tool_budget,
                                tool_used,
                            );
                            tool_used += content.chars().count();
                            current_turns.push(ChatTurn {
                                role: "tool".into(),
                                content,
                                name: Some(tc.name),
                                tool_call_id: Some(tc.id),
                                tool_calls: None,
                            });
                            continue;
                        }
                        if tc.name == tools::TOOL_READ_URL {
                            read_url_in_round += 1;
                        }
                        if tc.name == tools::TOOL_READ_NOTE {
                            read_note_called = true;
                        }
                        let (exec, new_cite) = execute_tool_off_runtime(
                            state_arc.clone(),
                            thread.id.clone(),
                            tc.name.clone(),
                            tc.arguments.clone(),
                            thread_scope.clone(),
                            next_cite,
                            web_search,
                            request_id.clone(),
                        )
                        .await;
                        next_cite = new_cite;
                        consumed.extend(exec.consumed);
                        emit_sources_updated(&app, &thread.id);
                        if let Some(nid) = exec.wrote_note_id.clone() {
                            after_llm_note_write(&app, state.inner().as_ref(), &thread.id, &nid);
                            if !wrote_note_ids.iter().any(|x| x == &nid) {
                                wrote_note_ids.push(nid);
                            }
                        }
                        let mut content = exec.content;
                        if tc.name == tools::TOOL_WRITE_NOTE && !read_note_called {
                            content.push_str(
                                "\n（警告: この請求では read_note の前に書きました。読んでいない部分を消していないか確認してください。）",
                            );
                        }
                        let content = if tc.name == tools::TOOL_READ_NOTE {
                            content
                        } else {
                            fit_tool_content(content, tool_budget, tool_used)
                        };
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
                        if !source_turn_injected {
                            let all = state.db.list_llm_sources(&thread.id).unwrap_or_default();
                            source_turn_injected =
                                inject_final_source_turn(&mut current_turns, &all, &consumed, true);
                        }
                        if !source_turn_injected {
                            current_turns.push(ChatTurn::text("user", STOP_TOOLS_HINT));
                            source_turn_injected = true;
                        }
                    }
                    continue;
                }
                if use_tools
                    && !out.tool_calls.is_empty()
                    && rounds >= max_rounds
                    && !extra_final
                {
                    extra_final = true;
                    use_tools = false;
                    if !source_turn_injected {
                        let all = state.db.list_llm_sources(&thread.id).unwrap_or_default();
                        source_turn_injected =
                            inject_final_source_turn(&mut current_turns, &all, &consumed, true);
                    }
                    if !source_turn_injected {
                        current_turns.push(ChatTurn::text("user", STOP_TOOLS_HINT));
                        source_turn_injected = true;
                    }
                    continue;
                }
                // Search hits live in `role: tool`. The citation guide only fires for a
                // user-turn 出典 block, so inject one before accepting a tool-only answer
                // that did not copy [n].
                if rounds > 0 && !consumed.is_empty() && !source_turn_injected {
                    let already_cited = consumed_cited_in_answer(&consumed, &out.content);
                    if already_cited.is_empty() {
                        let all = state.db.list_llm_sources(&thread.id).unwrap_or_default();
                        if inject_final_source_turn(&mut current_turns, &all, &consumed, true) {
                            source_turn_injected = true;
                            use_tools = false;
                            extra_final = true;
                            continue;
                        }
                    }
                }
                result_text = out.content;
                break;
            }
        }
    }

    busy.finish();
    kick_pending_ocr(&app, state_arc.clone());

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
        let cited = consumed_cited_in_answer(&consumed, &result_text);
        if !cited.is_empty() {
            state
                .db
                .consume_llm_sources(&cited, &user_message.id, &assistant.id)
                .map_err(|e| e.to_string())?;
        }
    }
    let _ = state.db.delete_uncited_tool_sources(&thread.id);
    emit_sources_updated(&app, &thread.id);
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
        note_writes: note_write_notices(state.inner().as_ref(), &wrote_note_ids),
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

fn emit_ocr_status(app: &AppHandle, thread_id: &str, active: bool) {
    let _ = app.emit(
        "llm-ocr-status",
        LlmOcrStatus {
            thread_id: thread_id.to_string(),
            active,
        },
    );
}

fn kick_pending_ocr(app: &AppHandle, state: Arc<AppState>) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        run_ocr_queue(app, state).await;
    });
}

async fn run_ocr_queue(app: AppHandle, state: Arc<AppState>) {
    loop {
        if state.is_llm_busy() {
            return;
        }
        let next = match state.db.next_pending_ocr_source() {
            Ok(row) => row,
            Err(_) => return,
        };
        let Some(first) = next else {
            return;
        };
        run_ocr_for_thread(&app, state.clone(), first.thread_id.clone()).await;
    }
}

async fn run_ocr_for_thread(app: &AppHandle, state: Arc<AppState>, thread_id: String) {
    let request_id = uuid::Uuid::new_v4().to_string();
    let cancel = Arc::new(AtomicBool::new(false));
    if !state.start_llm(request_id.clone(), thread_id.clone(), "ocr", cancel.clone()) {
        return;
    }
    emit_ocr_status(app, &thread_id, true);
    let mut guard = LlmBusyGuard::new(state.clone(), request_id.clone());

    loop {
        if cancel.load(std::sync::atomic::Ordering::SeqCst) {
            break;
        }
        let next = match state.db.next_pending_ocr_in_thread(&thread_id) {
            Ok(row) => row,
            Err(_) => break,
        };
        let Some(row) = next else {
            break;
        };
        let result = transcribe_source(&state, &row, cancel.clone()).await;
        if cancel.load(std::sync::atomic::Ordering::SeqCst)
            || matches!(&result, Err(e) if llm::is_cancelled_error(e))
        {
            let _ = state.db.fail_pending_ocr(&thread_id);
            break;
        }
        match result {
            Ok(text) => {
                let _ = state.db.update_llm_source_ocr(&row.id, &text, "");
                let _ = state.db.inherit_image_group_meta(&row.id);
            }
            Err(e) => {
                let _ = state.db.update_llm_source_ocr(&row.id, "", "error");
                let _ = app.emit(
                    "llm-chat-error",
                    LlmChatError {
                        request_id: request_id.clone(),
                        thread_id: thread_id.clone(),
                        message: e,
                    },
                );
            }
        }
        emit_sources_updated(app, &thread_id);
    }

    guard.finish();
    emit_ocr_status(app, &thread_id, false);
    emit_sources_updated(app, &thread_id);
}

async fn transcribe_source(
    state: &AppState,
    row: &LlmSourceRow,
    cancel: Arc<AtomicBool>,
) -> Result<String, String> {
    let path = files::resolve_stored(&state.data_dir, &row.stored_relpath)?;
    let bytes = std::fs::read(&path).map_err(|e| format!("画像を読めません（{e}）。"))?;
    let ext = files::file_ext(&path);
    let mime = files::mime_for_ext(&ext);
    let settings = state.settings.read().clone();
    llm::transcribe_image(&settings, mime, &bytes, cancel).await
}

fn attach_thread_title(explicit: Option<&str>, items: &[LlmAttachItem]) -> String {
    let from_arg = explicit.map(str::trim).filter(|s| !s.is_empty());
    if let Some(t) = from_arg {
        return t.to_string();
    }
    for item in items {
        if let Some(t) = item
            .title
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
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

fn persist_image_group_cites(db: &crate::db::Db, all: &[LlmSourceRow], consumed: &[(String, i64)]) {
    let mut seen = std::collections::HashSet::new();
    for (id, n) in consumed {
        let Some(row) = all.iter().find(|s| s.id == *id) else {
            continue;
        };
        let Some(key) = crate::llm::transcript::image_group_key(row) else {
            continue;
        };
        if !seen.insert(key) {
            continue;
        }
        let _ = db.apply_image_group_cite(&row.thread_id, &row.path, *n);
    }
}

fn delete_image_group_files(data_dir: &Path, members: &[LlmSourceRow]) {
    for m in members {
        files::remove_stored(data_dir, &m.stored_relpath);
    }
}

#[tauri::command]
pub fn llm_remove_source(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    id: String,
) -> Result<(), String> {
    let row = state
        .db
        .get_llm_source(&id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "出典が見つかりません".to_string())?;
    let thread_id = row.thread_id.clone();
    if crate::llm::transcript::image_group_key(&row).is_some() {
        let members = state
            .db
            .list_llm_image_group(&thread_id, &row.path)
            .map_err(|e| e.to_string())?;
        delete_image_group_files(&state.data_dir, &members);
        for m in members {
            let _ = state.db.delete_llm_source(&m.id);
        }
        emit_sources_updated(&app, &thread_id);
        return Ok(());
    }
    let stored = row.stored_relpath.clone();
    state
        .db
        .delete_llm_source(&id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "出典が見つかりません".to_string())?;
    files::remove_stored(&state.data_dir, &stored);
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
    if row.is_image() {
        return Err("画像出典は段落／全文を切り替えられません。".into());
    }
    if row.is_web() {
        return Err("ウェブ出典は段落／全文を切り替えられません。".into());
    }
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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmAttachFileError {
    pub path: String,
    pub message: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmAttachFilesResult {
    pub thread: LlmThreadRow,
    pub sources: Vec<LlmSourceRow>,
    pub added: usize,
    pub skipped: usize,
    pub created_thread: bool,
    pub errors: Vec<LlmAttachFileError>,
    pub remote_ocr: bool,
    pub ocr_started: bool,
    pub warnings: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmSourceImage {
    pub mime: String,
    pub data_url: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmImageSourceGroup {
    pub title: String,
    pub path: String,
    pub pages: Vec<LlmSourceRow>,
    pub can_save: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmSaveTranscript {
    pub path: String,
    pub existed: bool,
    pub written: bool,
}

fn resolve_attach_thread(
    state: &AppState,
    thread_id: Option<&str>,
    title: &str,
) -> Result<(LlmThreadRow, bool), String> {
    let want_new = match thread_id.map(str::trim) {
        None | Some("") | Some("new") => true,
        Some(_) => false,
    };
    if want_new {
        let t = if title.trim().is_empty() {
            "新しい会話"
        } else {
            title.trim()
        };
        let thread = state
            .db
            .create_llm_thread(t, false)
            .map_err(|e| e.to_string())?;
        Ok((thread, true))
    } else {
        let id = thread_id.unwrap_or("").trim();
        let thread = state
            .db
            .get_llm_thread(id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "会話が見つかりません".to_string())?;
        Ok((thread, false))
    }
}

struct AttachOutcome {
    created: usize,
    skipped: usize,
    needs_ocr: bool,
    warning: Option<String>,
}

fn attach_one_os_file(
    state: &AppState,
    thread_id: &str,
    raw_path: &str,
) -> Result<AttachOutcome, String> {
    let path = files::normalize_os_path(raw_path);
    if path.is_empty() {
        return Err("パスが空です。".into());
    }
    let p = Path::new(&path);
    let kind = files::classify_path(p)?;
    if let Some(existing) = state
        .db
        .find_any_pending_llm_source_by_path(thread_id, &path)
        .map_err(|e| e.to_string())?
    {
        let needs_ocr = existing.is_image() && !existing.is_injectable();
        return Ok(AttachOutcome {
            created: 0,
            skipped: 1,
            needs_ocr,
            warning: None,
        });
    }
    let title = files::file_title(p);
    match kind {
        files::AttachKind::Text => {
            if files::file_ext(p) == "pdf" {
                match files::extract_attach_doc(p)? {
                    files::AttachDoc::Text { title, body } => {
                        insert_text_source(state, thread_id, &path, &title, &body)
                    }
                    files::AttachDoc::EmptyPdf => {
                        attach_scanned_pdf(state, thread_id, p, &path, &title)
                    }
                }
            } else {
                let (title, body) = files::extract_text_body(p)?;
                insert_text_source(state, thread_id, &path, &title, &body)
            }
        }
        files::AttachKind::Image => attach_image_file(state, thread_id, p, &path, &title),
    }
}

fn insert_text_source(
    state: &AppState,
    thread_id: &str,
    path: &str,
    title: &str,
    body: &str,
) -> Result<AttachOutcome, String> {
    let (_row, created) = state
        .db
        .insert_llm_source_full(
            thread_id, "attach", path, title, "", body, "", "file", "text", "", "", None,
        )
        .map_err(|e| e.to_string())?;
    Ok(AttachOutcome {
        created: if created { 1 } else { 0 },
        skipped: if created { 0 } else { 1 },
        needs_ocr: false,
        warning: None,
    })
}

fn attach_image_file(
    state: &AppState,
    thread_id: &str,
    p: &Path,
    path: &str,
    title: &str,
) -> Result<AttachOutcome, String> {
    files::check_image_size(p)?;
    let id = uuid::Uuid::new_v4().to_string();
    let ext = files::file_ext(p);
    let rel = files::stored_relpath(thread_id, &id, &ext);
    if let Err(e) = files::copy_into_store(&state.data_dir, p, &rel) {
        return Err(e);
    }
    match state.db.insert_llm_source_full(
        thread_id,
        "attach",
        path,
        title,
        "",
        "",
        "",
        "file",
        "image",
        &rel,
        "pending",
        Some(&id),
    ) {
        Ok((row, created)) => {
            if row.id != id {
                files::remove_stored(&state.data_dir, &rel);
            }
            Ok(AttachOutcome {
                created: if created { 1 } else { 0 },
                skipped: if created { 0 } else { 1 },
                needs_ocr: row.is_image() && !row.is_injectable(),
                warning: None,
            })
        }
        Err(e) => {
            files::remove_stored(&state.data_dir, &rel);
            Err(e.to_string())
        }
    }
}

fn attach_scanned_pdf(
    state: &AppState,
    thread_id: &str,
    p: &Path,
    path: &str,
    filename: &str,
) -> Result<AttachOutcome, String> {
    let raster = llm::pdf_win::rasterize_pdf(p)?;
    if raster.pages.is_empty() {
        return Err("ページがありません。".into());
    }
    let mut rels: Vec<String> = Vec::new();
    let mut created = 0usize;
    let mut needs_ocr = false;
    for page in &raster.pages {
        let id = uuid::Uuid::new_v4().to_string();
        let rel = files::stored_relpath(thread_id, &id, "jpg");
        if let Err(e) = files::write_bytes_into_store(&state.data_dir, &rel, &page.jpeg) {
            for r in &rels {
                files::remove_stored(&state.data_dir, r);
            }
            return Err(e);
        }
        rels.push(rel.clone());
        let title = llm::pdf_win::pdf_page_title(filename, page.page_no);
        let pid = llm::pdf_win::pdf_page_paragraph_id(page.page_no);
        match state.db.insert_llm_source_full(
            thread_id,
            "attach",
            path,
            &title,
            &pid,
            "",
            "",
            "file",
            "image",
            &rel,
            "pending",
            Some(&id),
        ) {
            Ok((row, was_created)) => {
                if row.id != id {
                    files::remove_stored(&state.data_dir, &rel);
                }
                if was_created {
                    created += 1;
                }
                if row.is_image() && !row.is_injectable() {
                    needs_ocr = true;
                }
            }
            Err(e) => {
                for r in &rels {
                    files::remove_stored(&state.data_dir, r);
                }
                return Err(e.to_string());
            }
        }
    }
    let warning = if raster.truncated {
        Some(llm::pdf_win::truncation_warning(
            raster.pages.len() as u32,
            raster.total_pages,
        ))
    } else {
        None
    };
    Ok(AttachOutcome {
        created,
        skipped: 0,
        needs_ocr,
        warning,
    })
}

#[tauri::command]
pub fn llm_attach_files(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    paths: Vec<String>,
    thread_id: Option<String>,
) -> Result<LlmAttachFilesResult, String> {
    if paths.iter().all(|p| p.trim().is_empty()) {
        return Err("添付するファイルがありません。".into());
    }
    let first_title = paths
        .iter()
        .map(|p| files::file_title(Path::new(p.trim())))
        .find(|t| !t.trim().is_empty())
        .unwrap_or_else(|| "新しい会話".into());
    let (thread, created_thread) =
        resolve_attach_thread(state.inner(), thread_id.as_deref(), &first_title)?;

    if !state.is_llm_busy() {
        state
            .db
            .set_active_llm_thread_id(Some(&thread.id))
            .map_err(|e| e.to_string())?;
    }

    let mut added = 0usize;
    let mut skipped = 0usize;
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let mut wants_ocr = false;
    for raw in &paths {
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }
        match attach_one_os_file(state.inner(), &thread.id, raw) {
            Ok(outcome) => {
                added += outcome.created;
                skipped += outcome.skipped;
                if outcome.needs_ocr {
                    wants_ocr = true;
                }
                if let Some(w) = outcome.warning {
                    warnings.push(w);
                }
            }
            Err(message) => {
                errors.push(LlmAttachFileError {
                    path: raw.to_string(),
                    message,
                });
            }
        }
    }

    if created_thread && added == 0 {
        let _ = state.db.delete_llm_thread(&thread.id);
        files::remove_thread_store(&state.data_dir, &thread.id);
        if errors.is_empty() && skipped > 0 {
            return Err("同じ出典がすでに読込前にあります。".into());
        }
        let msg = errors
            .first()
            .map(|e| e.message.clone())
            .unwrap_or_else(|| "添付するファイルがありません。".into());
        return Err(msg);
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

    let settings = state.settings.read().clone();
    let remote_ocr = wants_ocr && !llm::is_loopback_url(&settings.llm_base_url);
    let ocr_started = wants_ocr && !state.is_llm_busy();
    if ocr_started {
        kick_pending_ocr(&app, state.inner().clone());
    }

    Ok(LlmAttachFilesResult {
        thread,
        sources,
        added,
        skipped,
        created_thread,
        errors,
        remote_ocr,
        ocr_started,
        warnings,
    })
}

#[tauri::command]
pub fn llm_retry_ocr(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    id: String,
) -> Result<Vec<LlmSourceRow>, String> {
    let row = state
        .db
        .get_llm_source(&id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "出典が見つかりません".to_string())?;
    if !row.is_image() {
        return Err("画像出典ではありません。".into());
    }
    let members = if crate::llm::transcript::image_group_key(&row).is_some() {
        state
            .db
            .list_llm_image_group(&row.thread_id, &row.path)
            .map_err(|e| e.to_string())?
    } else {
        vec![row.clone()]
    };
    let mut reset = 0usize;
    for m in &members {
        if !m.is_pending() {
            continue;
        }
        if !m.ocr_status.eq_ignore_ascii_case("error") {
            continue;
        }
        state
            .db
            .update_llm_source_ocr(&m.id, "", "pending")
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "出典が見つかりません".to_string())?;
        reset += 1;
    }
    if reset == 0 && row.is_pending() && row.ocr_status.eq_ignore_ascii_case("error") {
        return Err("再読み取りできるページがありません。".into());
    }
    let sources = state
        .db
        .list_llm_sources(&row.thread_id)
        .map_err(|e| e.to_string())?;
    emit_sources_updated(&app, &row.thread_id);
    if !state.is_llm_busy() {
        kick_pending_ocr(&app, state.inner().clone());
    }
    Ok(sources)
}

#[tauri::command]
pub fn llm_image_source_group(
    state: State<'_, Arc<AppState>>,
    id: String,
) -> Result<LlmImageSourceGroup, String> {
    let row = state
        .db
        .get_llm_source(&id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "出典が見つかりません".to_string())?;
    if !row.is_image() {
        return Err("画像出典ではありません。".into());
    }
    let mut pages = if crate::llm::transcript::image_group_key(&row).is_some() {
        state
            .db
            .list_llm_image_group(&row.thread_id, &row.path)
            .map_err(|e| e.to_string())?
    } else {
        vec![row.clone()]
    };
    if pages.is_empty() {
        pages.push(row.clone());
    }
    crate::llm::transcript::sort_image_pages(&mut pages);
    let title = crate::llm::transcript::file_label(&row.path, &row.title);
    let can_save = pages.iter().any(|s| s.is_injectable());
    Ok(LlmImageSourceGroup {
        title,
        path: row.path.clone(),
        pages,
        can_save,
    })
}

#[tauri::command]
pub fn llm_save_source_transcript(
    state: State<'_, Arc<AppState>>,
    source_id: String,
    overwrite: bool,
) -> Result<LlmSaveTranscript, String> {
    let row = state
        .db
        .get_llm_source(&source_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "出典が見つかりません".to_string())?;
    if !row.is_image() {
        return Err("画像出典ではありません。".into());
    }
    let mut members = if crate::llm::transcript::image_group_key(&row).is_some() {
        state
            .db
            .list_llm_image_group(&row.thread_id, &row.path)
            .map_err(|e| e.to_string())?
    } else {
        vec![row.clone()]
    };
    if members.is_empty() {
        members.push(row);
    }
    crate::llm::transcript::sort_image_pages(&mut members);
    let plan = crate::llm::transcript::prepare_transcript_save(&members)?;
    let (existed, written) =
        crate::llm::transcript::write_md_file(&plan.dest, &plan.markdown, overwrite)?;
    Ok(LlmSaveTranscript {
        path: plan.dest.to_string_lossy().into_owned(),
        existed,
        written,
    })
}

#[tauri::command]
pub fn llm_source_image(
    state: State<'_, Arc<AppState>>,
    id: String,
) -> Result<LlmSourceImage, String> {
    let row = state
        .db
        .get_llm_source(&id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "出典が見つかりません".to_string())?;
    if !row.is_image() {
        return Err("画像出典ではありません。".into());
    }
    let path = files::resolve_stored(&state.data_dir, &row.stored_relpath)?;
    if !path.is_file() {
        return Err("保存した画像が見つかりません。".into());
    }
    let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    let mime = files::mime_for_ext(&files::file_ext(&path)).to_string();
    Ok(LlmSourceImage {
        mime: mime.clone(),
        data_url: format!("data:{mime};base64,{b64}"),
    })
}

#[tauri::command]
pub fn llm_attached_file_path(
    state: State<'_, Arc<AppState>>,
    id: String,
) -> Result<String, String> {
    let row = state
        .db
        .get_llm_source(&id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "出典が見つかりません".to_string())?;
    let orig = Path::new(row.path.trim());
    if orig.is_file() {
        return Ok(row.path.trim().to_string());
    }
    if !row.stored_relpath.trim().is_empty() {
        let stored = files::resolve_stored(&state.data_dir, &row.stored_relpath)?;
        if stored.is_file() {
            return Ok(stored.to_string_lossy().into_owned());
        }
    }
    Err("ファイルが見つかりません。".into())
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

    #[test]
    fn inject_final_source_turn_appends_user_block() {
        let mut s = LlmSourceRow {
            id: "hit".into(),
            thread_id: "t".into(),
            sort_order: 0,
            origin: "tool".into(),
            path: "C:\\民法.md".into(),
            title: "民法.md".into(),
            paragraph_id: "p1".into(),
            body: "第936条".into(),
            query: String::new(),
            created_at: 0,
            grain: "unit".into(),
            unit_body: String::new(),
            injected_user_message_id: String::new(),
            cited_assistant_message_id: String::new(),
            cite_no: 2,
            kind: "text".into(),
            stored_relpath: String::new(),
            ocr_status: String::new(),
        };
        let mut turns = Vec::new();
        assert!(inject_final_source_turn(
            &mut turns,
            std::slice::from_ref(&s),
            &[("hit".into(), 2)],
            true,
        ));
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].role, "user");
        assert!(turns[0].content.contains("【出典】"));
        assert!(turns[0].content.contains("[2]"));
        assert!(turns[0].content.contains("[n]"));
        assert!(turns[0].content.contains("ツールはこれ以上"));
        s.id = "other".into();
        let mut empty = Vec::new();
        assert!(!inject_final_source_turn(
            &mut empty,
            std::slice::from_ref(&s),
            &[("hit".into(), 2)],
            false,
        ));
    }
}
