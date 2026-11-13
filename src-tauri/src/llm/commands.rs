use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Runtime, State};
use tauri_plugin_opener::OpenerExt;

use crate::db::{LlmMessageRow, LlmSourceRow, LlmThreadRow, NoteReview};
use crate::llm::context::{
    answer_has_phantom_cites, assemble_turns, cap_chars, consumed_cited_in_answer, context_budget,
    estimate_tokens, fit_injected_turn, fit_tool_text, is_synthetic_user_turn, meter_question,
    meter_request_tokens, meter_usage_tokens, parse_cited_nos, sent_usage_tokens,
    silent_end_notice, sources_for_consumed, summary_trigger_reached, tool_schema_tokens, ChatTurn,
    ContextBudget, PHANTOM_CITE_HINT, PHANTOM_CITE_HINT_NO_TOOLS, REASONING_CARRY_CAP,
    STOP_TOOLS_HINT,
};
use crate::llm::summary::{self, SummaryAttempt};
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
    pub context_tokens: usize,
    pub thinking_budget_state: String,
    pub thinking_budget_tokens: Option<u32>,
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

enum ToolRun {
    Done(ToolExec),
    /// The model cannot read images. The text is the existing vision error, and
    /// later calls in this turn must not send the image again.
    VisionUnsupported(ToolExec),
    Cancelled,
}

async fn run_look_image(
    state: Arc<AppState>,
    thread_id: String,
    arguments: String,
    cancel: Arc<AtomicBool>,
) -> ToolRun {
    let args: serde_json::Value =
        serde_json::from_str(&arguments).unwrap_or_else(|_| serde_json::json!({}));
    let sources = state.db.list_llm_sources(&thread_id).unwrap_or_default();
    let target = match tools::resolve_look_image(&sources, &args) {
        Ok(target) => target,
        Err(message) => return ToolRun::Done(ToolExec::text(message)),
    };
    let Some(row) = sources.into_iter().find(|row| row.id == target.source_id) else {
        return ToolRun::Done(ToolExec::text("見返せる添付画像がありません。"));
    };
    let path = match files::resolve_stored(&state.data_dir, &row.stored_relpath) {
        Ok(path) => path,
        Err(message) => return ToolRun::Done(ToolExec::text(message)),
    };
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(err) => {
            return ToolRun::Done(ToolExec::text(format!("画像を読めません（{err}）。")));
        }
    };
    let mime = files::mime_for_ext(&files::file_ext(&path));
    let settings = state.settings.read().clone();
    let prompt = llm::look_image_prompt(&target.focus);
    match llm::ask_vision(&settings, mime, &bytes, &prompt, cancel).await {
        Ok(text) => ToolRun::Done(ToolExec::text(tools::format_look_image_answer(
            &target.label,
            &text,
        ))),
        Err(err) if llm::is_cancelled_error(&err) => ToolRun::Cancelled,
        Err(err) if err == crate::llm::http_error::VISION_UNSUPPORTED_ERROR => {
            ToolRun::VisionUnsupported(ToolExec::text(tools::look_image_failure(&err)))
        }
        Err(err) => ToolRun::Done(ToolExec::text(tools::look_image_failure(&err))),
    }
}

fn push_tool_turn(
    db: &crate::db::Db,
    thread_id: &str,
    turns: &mut Vec<ChatTurn>,
    name: &str,
    tool_call_id: &str,
    content: String,
) -> Result<(), String> {
    let tool_turn = ChatTurn {
        role: "tool".into(),
        content,
        name: Some(name.to_string()),
        tool_call_id: Some(tool_call_id.to_string()),
        tool_calls: None,
        reasoning: None,
    };
    store_turn(db, thread_id, &tool_turn)?;
    turns.push(tool_turn);
    Ok(())
}

async fn execute_tool_off_runtime(
    state: Arc<AppState>,
    thread_id: String,
    name: String,
    arguments: String,
    thread_scope: Option<String>,
    web_search: bool,
    request_id: String,
) -> ToolExec {
    match tokio::task::spawn_blocking(move || {
        tools::execute_tool(
            &state,
            &thread_id,
            &name,
            &arguments,
            thread_scope.as_deref(),
            web_search,
            &request_id,
        )
    })
    .await
    {
        Ok(exec) => exec,
        Err(e) => ToolExec {
            content: format!("ツール実行に失敗しました: {e}"),
            consumed: Vec::new(),
            saved_file: None,
            wrote_note_id: None,
        },
    }
}

fn emit_chat_reset<R: Runtime>(app: &AppHandle<R>, request_id: &str, thread_id: &str) {
    let _ = app.emit(
        "llm-chat-delta",
        LlmChatDelta {
            request_id: request_id.to_string(),
            thread_id: thread_id.to_string(),
            text: String::new(),
            kind: "reset".into(),
        },
    );
}

fn store_turn(db: &crate::db::Db, thread_id: &str, turn: &ChatTurn) -> Result<(), String> {
    let calls = turn
        .tool_calls
        .as_ref()
        .map(|value| value.to_string())
        .unwrap_or_default();
    db.insert_llm_message_full(
        thread_id,
        &turn.role,
        &turn.content,
        turn.reasoning.as_deref().unwrap_or(""),
        &calls,
        turn.tool_call_id.as_deref().unwrap_or(""),
        turn.name.as_deref().unwrap_or(""),
        "",
    )
    .map(|_| ())
    .map_err(|e| e.to_string())
}

fn budget_fields(base: &str) -> (String, Option<u32>) {
    match llm::thinking::known_budget_for(&llm::normalize_base_url(base)) {
        llm::thinking::KnownBudget::Tokens(n) => ("set".into(), Some(n)),
        llm::thinking::KnownBudget::Unset => ("unset".into(), None),
        llm::thinking::KnownBudget::Unknown => ("unknown".into(), None),
    }
}

/// Put this-turn hits on a user turn so the model sees a real 出典 block (not only `role: tool`).
fn inject_final_source_turn(
    turns: &mut Vec<ChatTurn>,
    sources: &[LlmSourceRow],
    consumed: &[(String, i64)],
    stop_tools: bool,
    message_budget: usize,
) -> bool {
    let rows = sources_for_consumed(sources, consumed);
    // Tool hits stay where the tool result put them, including ones fit_tool_text
    // cut out entirely. Only a non-tool source that is not already present is added.
    let cite_hint = rows
        .iter()
        .any(|row| row.cite_no > 0 || row.is_injectable());
    let fresh: Vec<LlmSourceRow> = rows
        .into_iter()
        .filter(|row| !row.is_tool_origin() && !source_already_in_request(turns, row))
        .collect();
    if fresh.is_empty() && !cite_hint && !stop_tools {
        return false;
    }
    if let Some(turn) = crate::llm::context::compose_final_turn(&fresh, stop_tools, cite_hint) {
        crate::llm::context::omit_source_bodies_from_tool_turns(turns, &fresh);
        turns.push(turn);
        fit_injected_turn(turns, message_budget);
        return true;
    }
    false
}

fn required_prompt_tokens(system: &str, question: &str, sources: &[LlmSourceRow]) -> usize {
    let has_sources = sources.iter().any(|source| source.is_injectable());
    let mut sys = system.trim().to_string();
    if has_sources && !sys.contains("[n]") {
        sys.push_str(crate::llm::context::CITATION_GUIDE);
    }
    let sys_tokens = if sys.is_empty() {
        0
    } else {
        estimate_tokens(&sys) + 4
    };
    let question_text = if has_sources {
        format!("【質問】\n{question}")
    } else {
        question.to_string()
    };
    sys_tokens + estimate_tokens(&question_text) + 4
}

fn send_error(
    thread: LlmThreadRow,
    user_message: LlmMessageRow,
    base: &str,
    message: &str,
) -> LlmSendResult {
    let (state, tokens) = budget_fields(base);
    LlmSendResult {
        thread,
        user_message,
        assistant_message: None,
        cancelled: false,
        error: Some(message.to_string()),
        truncated: false,
        context_tokens: 0,
        thinking_budget_state: state,
        thinking_budget_tokens: tokens,
        warning: None,
        note_writes: Vec::new(),
    }
}

/// A non-tool source already rendered in this request is not sent again.
fn source_already_in_request(turns: &[ChatTurn], source: &LlmSourceRow) -> bool {
    if source.cite_no > 0 {
        let headers = crate::llm::context::source_header_lines(source);
        if turns.iter().any(|turn| {
            turn.content.lines().any(|line| {
                let line = line.trim_end_matches('\r');
                headers.iter().any(|header| line == header)
            })
        }) {
            return true;
        }
    }
    let id = source.id.trim();
    if id.is_empty() {
        return false;
    }
    turns.iter().any(|turn| {
        turn.role == "user" && !is_synthetic_user_turn(&turn.content) && turn.content.contains(id)
    })
}

fn discard_refused_user_message(db: &crate::db::Db, message_id: &str) {
    let _ = db.delete_llm_message(message_id);
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
    state
        .db
        .get_note_review(&note_id)
        .map_err(|e| e.to_string())
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteReviewEvent {
    pub thread_id: String,
    pub note_id: String,
}

fn emit_note_review<R: Runtime>(app: &AppHandle<R>, thread_id: &str, note_id: &str) {
    let _ = app.emit(
        "llm-note-review",
        NoteReviewEvent {
            thread_id: thread_id.to_string(),
            note_id: note_id.to_string(),
        },
    );
}

fn after_llm_note_write<R: Runtime>(
    app: &AppHandle<R>,
    state: &AppState,
    thread_id: &str,
    note_id: &str,
) {
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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingBudgetStatus {
    /// "set" | "unset" | "unknown"
    pub state: String,
    pub tokens: Option<u32>,
}

/// Read MTPLX `GET /metrics` and return the remembered thinking budget.
/// Does not count or cut the budget; that stays on the server.
#[tauri::command]
pub async fn llm_thinking_budget(
    state: State<'_, Arc<AppState>>,
) -> Result<ThinkingBudgetStatus, String> {
    let settings = state.settings.read().clone();
    let _ = llm::refresh_thinking_budget(&settings, None).await;
    let (state_name, tokens) = budget_fields(&settings.llm_base_url);
    Ok(ThinkingBudgetStatus {
        state: state_name,
        tokens,
    })
}

#[tauri::command]
pub async fn llm_list_models(state: State<'_, Arc<AppState>>) -> Result<Vec<LlmModelInfo>, String> {
    let settings = state.settings.read().clone();
    llm::list_models(&settings).await
}

/// Open a saved `.docx`, `.xlsx`, or `.html` with the OS association.
/// HTML uses the same path as Word and Excel, so the browser opens it.
#[tauri::command]
pub fn open_output_file(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    name: String,
) -> Result<(), String> {
    let path = crate::llm::create_office::output_file_path(&state.data_dir, &name)?;
    let path = crate::llm::create_office::strip_verbatim_prefix(&path);
    let display = path.to_string_lossy().to_string();
    app.opener()
        .open_path(display.as_str(), None::<&str>)
        .map_err(|e| format!("ファイルを開けません（{e}）。"))?;
    Ok(())
}

/// Open Argos's output folder in the file manager.
/// The path is always `<data dir>/output`. The caller cannot choose another one.
/// Directory creation and the shell call run off the UI thread.
#[tauri::command]
pub async fn open_output_folder(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    let data_dir = state.data_dir.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let path = crate::llm::create_office::prepare_output_dir(&data_dir)?;
        let display = path.to_string_lossy().to_string();
        app.opener()
            .open_path(display.as_str(), None::<&str>)
            .map_err(|err| format!("出力フォルダを開けません（{err}）。"))?;
        Ok(())
    })
    .await
    .map_err(|err| format!("出力フォルダを開けません（{err}）。"))?
}

/// Open the large HTML preview.
///
/// Async so the body is not the WebView2 IPC callback. `spawn_blocking` then
/// builds the window off the UI thread; wry posts creation to the event loop.
/// A synchronous command deadlocks WebView2 on Windows: the window is blank,
/// its close button does nothing, and the chat and tray quit freeze with it.
#[tauri::command]
pub async fn show_html_preview(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    name: String,
) -> Result<(), String> {
    let data_dir = state.data_dir.clone();
    tauri::async_runtime::spawn_blocking(move || {
        crate::html_preview::show_window(&app, &data_dir, &name)
    })
    .await
    .map_err(|err| format!("プレビューを開けません（{err}）。"))?
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

struct ChatRequestPlan {
    system: String,
    web_search: bool,
    tools_schema: serde_json::Value,
    schema_tokens: usize,
    split: Option<ContextBudget>,
    prompt_budget: usize,
    tool_budget: usize,
}

/// System text, tool schema, and the message budget `llm_send` passes to `assemble_turns`.
fn plan_chat_request(
    state: &AppState,
    settings: &crate::db::Settings,
    thread: &LlmThreadRow,
    content: &str,
    web_search: bool,
    sources: &[LlmSourceRow],
    extra_system: Option<&str>,
) -> ChatRequestPlan {
    let mut system = llm::system_for_request(settings);
    llm::append_diagram_hint(&mut system, content);
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
    if let Some(note) = &bound_note {
        system.push_str(&tools::format_note_target_system_line(
            &note.title,
            &note.memo,
        ));
    }
    system.push_str(&tools::format_search_date_system_line(
        settings.mail_days_back,
    ));
    system.push_str(&tools::format_calendar_system_line());
    let web_search = web_search && !settings.searxng_url.trim().is_empty();
    if web_search {
        system.push_str(&tools::format_web_search_system_line());
    }
    let remote_index =
        tools::include_hybrid_remote_system_line(&settings.search_mode, &thread.path_prefix);
    if remote_index {
        system.push_str(&tools::format_remote_index_system_line());
    }
    if let Some(line) = extra_system {
        system.push_str(line);
    }
    let remote_only = tools::is_remote_only_thread(&thread.path_prefix);
    let look_image = tools::thread_has_relook_image(sources);
    if look_image {
        system.push_str(&tools::format_look_image_system_line());
    }
    let tools_schema = tools::tools_schema(web_search, remote_index, remote_only, look_image);
    let schema_tokens = tool_schema_tokens(&tools_schema);
    let full_limit = settings.llm_max_context_tokens as usize;
    let required = required_prompt_tokens(&system, content, sources);
    let Some(split) = context_budget(full_limit, schema_tokens, required) else {
        return ChatRequestPlan {
            system,
            web_search,
            tools_schema,
            schema_tokens,
            split: None,
            prompt_budget: full_limit.max(1),
            tool_budget: 0,
        };
    };
    // Reserves shrink when this question needs the room. The schema stays counted.
    let mut prompt_budget = split.messages;
    let mut tool_budget = split.tool_allowance;
    let excerpt =
        crate::llm::context::pending_attachment_borrow(&system, content, sources, prompt_budget)
            .min(tool_budget);
    prompt_budget = prompt_budget.saturating_add(excerpt);
    tool_budget = tool_budget.saturating_sub(excerpt);
    ChatRequestPlan {
        system,
        web_search,
        tools_schema,
        schema_tokens,
        split: Some(split),
        prompt_budget,
        tool_budget,
    }
}

fn blank_llm_thread() -> LlmThreadRow {
    LlmThreadRow {
        id: String::new(),
        title: String::new(),
        search_enabled: true,
        path_prefix: String::new(),
        note_id: String::new(),
        sort_order: 0,
        created_at: 0,
        updated_at: 0,
        summary: String::new(),
        summary_through_id: String::new(),
    }
}

fn push_warning(warning: &mut Option<String>, line: &str) {
    *warning = Some(match warning.take() {
        Some(prev) if !prev.is_empty() => format!("{prev} {line}"),
        _ => line.to_string(),
    });
}

#[tauri::command]
pub async fn llm_send(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    thread_id: Option<String>,
    content: String,
    web_search: Option<bool>,
) -> Result<LlmSendResult, String> {
    llm_send_inner(&app, state.inner(), thread_id, content, web_search).await
}

async fn llm_send_inner<R: Runtime>(
    app: &AppHandle<R>,
    state: &Arc<AppState>,
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

    let mut thread = match thread_id.filter(|s| !s.trim().is_empty()) {
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
        let state_for_fetch = Arc::clone(state);
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
    let thread_scope = Some(thread.path_prefix.trim().to_string()).filter(|s| !s.is_empty());
    let paste_line = paste.system_line();
    let plan = plan_chat_request(
        &state,
        &settings,
        &thread,
        &content,
        web_search,
        &sources,
        paste_line.as_deref(),
    );
    let system = plan.system;
    let web_search = plan.web_search;
    let tools_schema = plan.tools_schema;
    let Some(split) = plan.split else {
        discard_refused_user_message(&state.db, &user_message.id);
        return Ok(send_error(
            thread,
            user_message,
            &settings.llm_base_url,
            crate::llm::http_error::QUESTION_DOES_NOT_FIT_ERROR,
        ));
    };
    let prompt_budget = plan.prompt_budget;
    let tool_budget = plan.tool_budget;
    let schema_tokens = plan.schema_tokens;
    // Messages plus the tool allowance leave the reply reserve untouched.
    let inject_budget = split.messages.saturating_add(split.tool_allowance).max(1);
    let request_id = uuid::Uuid::new_v4().to_string();
    let cancel = Arc::new(AtomicBool::new(false));
    let state_arc = Arc::clone(state);
    let mut busy: Option<LlmBusyGuard> = None;
    // Summary keys off replayed turns and the tool schema, against 80% of the
    // meter denominator. The system prompt is on the meter only, so a longer
    // instruction does not by itself fold the thread.
    let baseline =
        summary::prepare_model_history(&history, &thread.summary, &thread.summary_through_id);
    let usage = sent_usage_tokens(&system, &sources, &baseline, schema_tokens, prompt_budget);
    let mut summary_now = thread.summary.clone();
    let mut through_now = thread.summary_through_id.clone();
    let mut attempt = SummaryAttempt::NotRun;
    if summary_trigger_reached(usage, settings.llm_max_context_tokens as usize) {
        let chunk_budget =
            summary::summary_request_budget(settings.llm_max_context_tokens as usize);
        let mut announced = false;
        for _ in 0..summary::SUMMARY_CHUNKS_PER_SEND {
            let Some(chunk) = summary::next_summary_chunk(
                &history,
                &sources,
                &summary_now,
                &through_now,
                chunk_budget,
            ) else {
                break;
            };
            if !announced {
                if !state.start_llm(
                    request_id.clone(),
                    thread.id.clone(),
                    "send",
                    cancel.clone(),
                ) {
                    return Err("生成中です。停止してから送信してください。".into());
                }
                busy = Some(LlmBusyGuard::new(state_arc.clone(), request_id.clone()));
                let _ = app.emit(
                    "llm-chat-delta",
                    LlmChatDelta {
                        request_id: request_id.clone(),
                        thread_id: thread.id.clone(),
                        text: summary::SUMMARY_STATUS_TEXT.into(),
                        kind: "status".into(),
                    },
                );
                announced = true;
            }
            // Thinking off and tools off. The closure drops summary tokens.
            let call = summary::summary_call(&chunk.transcript);
            let summarized = llm::stream_chat(
                &settings,
                &call.turns,
                cancel.clone(),
                None,
                llm::runaway_chars_for(&settings.llm_base_url),
                call.thinking,
                Some(call.max_tokens),
                |_, _| {},
            )
            .await;
            match summarized {
                Err(e) if llm::is_cancelled_error(&e) => {
                    if let Some(guard) = busy.as_mut() {
                        guard.finish();
                    }
                    kick_pending_ocr(&app, state_arc.clone());
                    let _ = state.db.delete_uncited_tool_sources(&thread.id);
                    emit_sources_updated(&app, &thread.id);
                    let thread = state
                        .db
                        .get_llm_thread(&thread.id)
                        .map_err(|err| err.to_string())?
                        .unwrap_or(thread);
                    return Ok(LlmSendResult {
                        thread,
                        user_message,
                        assistant_message: None,
                        cancelled: true,
                        error: None,
                        truncated: false,
                        context_tokens: 0,
                        thinking_budget_state: budget_fields(&settings.llm_base_url).0,
                        thinking_budget_tokens: budget_fields(&settings.llm_base_url).1,
                        warning,
                        note_writes: Vec::new(),
                    });
                }
                Ok(out) => match summary::finalize_summary(&out.content) {
                    Ok(text) => {
                        if state
                            .db
                            .set_llm_thread_summary(&thread.id, &text, &chunk.through_id)
                            .is_ok()
                        {
                            summary_now = text;
                            through_now = chunk.through_id;
                            attempt = SummaryAttempt::Saved {
                                summary: summary_now.clone(),
                                through_id: through_now.clone(),
                            };
                        } else {
                            attempt = SummaryAttempt::Failed;
                            break;
                        }
                    }
                    Err(_) => {
                        attempt = SummaryAttempt::Failed;
                        break;
                    }
                },
                Err(_) => {
                    attempt = SummaryAttempt::Failed;
                    break;
                }
            }
        }
    }
    let folded = summary::fold_summary(&history, &summary_now, &through_now, attempt);
    if folded.failed {
        push_warning(&mut warning, summary::SUMMARY_FAILURE_WARNING);
    }
    thread.summary = folded.summary;
    thread.summary_through_id = folded.through_id;
    let (assembled, stats) = assemble_turns(&system, &sources, &folded.messages, prompt_budget);
    // A long question may clip the summary inside this request. That clip is not stored.
    if stats.question_exceeds {
        discard_refused_user_message(&state.db, &user_message.id);
        return Ok(send_error(
            thread,
            user_message,
            &settings.llm_base_url,
            crate::llm::http_error::QUESTION_DOES_NOT_FIT_ERROR,
        ));
    }
    let turns_for_plain = assembled.clone();
    let truncated = stats.truncated;
    let context_tokens = stats.total_tokens;
    let mut consumed: Vec<(String, i64)> = stats
        .consumed
        .iter()
        .map(|c| (c.id.clone(), c.cite_no))
        .collect();
    persist_image_group_cites(&state.db, &sources, &consumed);
    if busy.is_none() {
        if !state.start_llm(
            request_id.clone(),
            thread.id.clone(),
            "send",
            cancel.clone(),
        ) {
            return Err("生成中です。停止してから送信してください。".into());
        }
        busy = Some(LlmBusyGuard::new(state_arc.clone(), request_id.clone()));
    }
    let mut busy = busy.expect("llm send owns the busy guard");

    let mut use_tools = true;
    let mut retried_without_tools = false;
    let mut phantom_retried = false;
    let mut rounds = 0usize;
    let mut model_rounds = 0usize;
    let mut law_calls = 0usize;
    let mut verified_cites: Vec<crate::statute::VerifiedCite> = Vec::new();
    let max_rounds = tools::max_tool_rounds(settings.llm_max_tool_rounds);
    let unlimited_rounds = max_rounds == 0;
    let mut current_turns = assembled;
    let mut source_turn_injected = false;
    // In-turn tool results spend the allowance left after the attachment excerpt.
    let mut tool_used = 0usize;
    let mut read_note_called = false;
    let mut wrote_note_ids: Vec<String> = Vec::new();
    // Set after a vision model rejects the image. Further look_image calls
    // return the same text and do not send the pixels again.
    let mut vision_blocked = false;
    let mut final_reasoning = String::new();
    let mut round_limit_final = false;
    let mut round_limit_aside: Option<String> = None;
    let mut empty_reply = false;
    // Once a cut answers, later passes in this turn stay thinking-off.
    let mut cut_hold = llm::thinking::CutHold::default();
    // First answer kept when a phantom retry (hint or source-block) comes back empty.
    let mut phantom_fallback: Option<PhantomOriginal> = None;
    // Cut answer kept when a later phantom or citation retry fails.
    let mut recovered_cut: Option<RecoveredCut> = None;
    let mut kept_source_ids: Option<std::collections::HashSet<String>> = None;
    let mut result_text;

    loop {
        if cut_hold.tools_off {
            use_tools = false;
        }
        if use_tools && !unlimited_rounds && rounds >= max_rounds {
            // One answer from what this turn already collected. No further tool calls.
            use_tools = false;
            round_limit_final = true;
            if !source_turn_injected {
                let all = state.db.list_llm_sources(&thread.id).unwrap_or_default();
                source_turn_injected = inject_final_source_turn(
                    &mut current_turns,
                    &all,
                    &consumed,
                    true,
                    inject_budget,
                );
            }
            if !source_turn_injected {
                current_turns.push(ChatTurn::text("user", STOP_TOOLS_HINT));
                source_turn_injected = true;
            }
            emit_chat_reset(&app, &request_id, &thread.id);
            continue;
        }
        let with_tools = use_tools;
        if model_rounds > 0 {
            crate::llm::context::fit_followup_request(&mut current_turns, inject_budget);
        }
        let turns_now = current_turns.clone();
        let mut bridge_retried = false;
        let thinking_on = llm::thinking::thinking_enabled(&settings) && !cut_hold.thinking_off;
        let mut cut = if cut_hold.thinking_off {
            llm::thinking::ThinkingCut::held(with_tools)
        } else {
            llm::thinking::ThinkingCut::start(thinking_on, with_tools)
        };
        let mut thinking_off_settings = None;
        // The round-start flag stays true when stage 2 drops tools mid-round.
        // This follows the request that actually produced the response.
        let mut tools_sent;
        let outcome = loop {
            let app2 = app.clone();
            let req = request_id.clone();
            let tid = thread.id.clone();
            let request = cut.request();
            tools_sent = request.tools;
            let call_settings: &crate::db::Settings = if request.thinking_off {
                thinking_off_settings.get_or_insert_with(|| {
                    let mut off = settings.clone();
                    off.llm_thinking = "off".into();
                    off
                })
            } else {
                &settings
            };
            let streamed = llm::stream_chat(
                call_settings,
                &turns_now,
                cancel.clone(),
                if request.tools {
                    Some(&tools_schema)
                } else {
                    None
                },
                llm::runaway_chars_for(&settings.llm_base_url),
                None,
                None,
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
            .await;
            match &streamed {
                Err(e) if e == llm::thinking::THINKING_BRIDGE_ERROR && !bridge_retried => {
                    bridge_retried = true;
                    emit_chat_reset(&app, &request_id, &thread.id);
                    continue;
                }
                Err(e) if e == llm::thinking::THINKING_BRIDGE_ERROR => {
                    break Err(llm::thinking::thinking_bridge_error());
                }
                _ => {}
            }
            let reply = cut_reply(&streamed);
            let step = if cut.in_recovery() || matches!(reply, llm::thinking::CutReply::OverCap) {
                cut.observe(reply)
            } else {
                llm::thinking::CutStep::Pass
            };
            match step {
                llm::thinking::CutStep::Pass => break streamed,
                llm::thinking::CutStep::Again(_) => {
                    // The cap stopped this thinking stream. Ask again from the
                    // materials already in the turn. This is not a tool round.
                    emit_chat_reset(&app, &request_id, &thread.id);
                    continue;
                }
                llm::thinking::CutStep::Answer { tools } => {
                    cut_hold.observe_answer(tools);
                    if !tools {
                        use_tools = false;
                    }
                    break streamed;
                }
                llm::thinking::CutStep::Stop { over_cap: true } => {
                    let err = streamed.as_ref().err().map(String::as_str).unwrap_or("");
                    break Err(llm::thinking::finish_runaway(err));
                }
                llm::thinking::CutStep::Stop { over_cap: false } => break streamed,
            }
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
                    context_tokens,
                    thinking_budget_state: budget_fields(&settings.llm_base_url).0,
                    thinking_budget_tokens: budget_fields(&settings.llm_base_url).1,
                    warning,
                    note_writes: note_write_notices(state.as_ref(), &wrote_note_ids),
                });
            }
            Err(e) if round_limit_final => {
                if let Some(kept) = take_kept_answer(&mut phantom_fallback, &mut recovered_cut) {
                    final_reasoning = kept.reasoning;
                    result_text = kept.content;
                    kept_source_ids = Some(kept.source_ids);
                    round_limit_aside = Some(tools::tool_round_limit_short_notice(max_rounds));
                } else {
                    let finished = tools::round_limit_failure(max_rounds, &e);
                    result_text = finished.text;
                    round_limit_aside = finished.notice;
                }
                break;
            }
            Err(_)
                if recovered_cut_to_keep(&recovered_cut).is_some()
                    || phantom_fallback.is_some() =>
            {
                // A later phantom or citation retry failed. Keep the cut answer,
                // and only the sources that existed when it was written.
                if let Some(kept) = take_kept_answer(&mut phantom_fallback, &mut recovered_cut) {
                    final_reasoning = kept.reasoning;
                    result_text = kept.content;
                    kept_source_ids = Some(kept.source_ids);
                } else {
                    final_reasoning = String::new();
                    result_text = String::new();
                }
                break;
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
                    context_tokens,
                    thinking_budget_state: budget_fields(&settings.llm_base_url).0,
                    thinking_budget_tokens: budget_fields(&settings.llm_base_url).1,
                    warning,
                    note_writes: note_write_notices(state.as_ref(), &wrote_note_ids),
                });
            }
            Ok(out) => {
                let source_ids = state
                    .db
                    .list_llm_sources(&thread.id)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|row| row.id)
                    .collect();
                // Reasoning-only calls on a request that sent no tools are not a tool turn.
                let acts_as_tool_call = !out.tool_calls.is_empty()
                    && (tools_sent || out.tool_calls_from_content_or_structured);
                remember_recovered_cut(
                    &mut recovered_cut,
                    cut_hold.thinking_off,
                    &out.content,
                    &out.reasoning,
                    acts_as_tool_call,
                    source_ids,
                );
                let stripped_without_tools = acts_as_tool_call && !tools_sent;
                let answer_body = if stripped_without_tools {
                    ""
                } else {
                    out.content.trim()
                };
                if round_limit_final {
                    if answer_body.is_empty() {
                        if let Some(kept) =
                            take_kept_answer(&mut phantom_fallback, &mut recovered_cut)
                        {
                            final_reasoning = kept.reasoning;
                            result_text = kept.content;
                            kept_source_ids = Some(kept.source_ids);
                            round_limit_aside =
                                Some(tools::tool_round_limit_short_notice(max_rounds));
                        } else {
                            let finished = tools::round_limit_answer(None, max_rounds);
                            result_text = finished.text;
                            round_limit_aside = finished.notice;
                        }
                    } else {
                        let finished = tools::round_limit_answer(Some(answer_body), max_rounds);
                        if finished.notice.is_some() {
                            final_reasoning = out.reasoning;
                        }
                        result_text = finished.text;
                        round_limit_aside = finished.notice;
                    }
                    break;
                }
                if stripped_without_tools {
                    if let Some(kept) = take_kept_answer(&mut phantom_fallback, &mut recovered_cut)
                    {
                        final_reasoning = kept.reasoning;
                        result_text = kept.content;
                        kept_source_ids = Some(kept.source_ids);
                    } else {
                        empty_reply = true;
                        final_reasoning = out.reasoning;
                        result_text = silent_end_notice(&out.finish_reason);
                    }
                    break;
                }
                if tools_sent
                    && llm::thinking::thinking_enabled(&settings)
                    && !out.request_id.is_empty()
                {
                    let _ = llm::refresh_thinking_budget(&settings, Some(&out.request_id)).await;
                }
                if use_tools && !out.tool_calls.is_empty() {
                    let names: Vec<&str> = out
                        .tool_calls
                        .iter()
                        .map(|call| call.name.as_str())
                        .collect();
                    model_rounds += 1;
                    if tools::counts_toward_tool_round(&names) {
                        rounds += 1;
                    }
                    let law_only =
                        !names.is_empty() && names.iter().all(|name| *name == tools::TOOL_LAW);
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
                    let reasoning = {
                        let r = out.reasoning.trim();
                        if r.is_empty() {
                            None
                        } else {
                            Some(cap_chars(r, REASONING_CARRY_CAP))
                        }
                    };
                    let assistant_turn = ChatTurn {
                        role: "assistant".into(),
                        content: out.content,
                        name: None,
                        tool_call_id: None,
                        tool_calls: Some(tool_calls_json),
                        reasoning,
                    };
                    store_turn(&state.db, &thread.id, &assistant_turn)?;
                    current_turns.push(assistant_turn);
                    let mut read_url_in_round = 0usize;
                    let mut cancel_tools = false;
                    for tc in out.tool_calls {
                        if cancel_tools {
                            let content =
                                fit_tool_text("中断しました。".into(), tool_budget, tool_used);
                            tool_used += estimate_tokens(&content);
                            push_tool_turn(
                                &state.db,
                                &thread.id,
                                &mut current_turns,
                                &tc.name,
                                &tc.id,
                                content,
                            )?;
                            continue;
                        }
                        if tc.name == tools::TOOL_LOOK_IMAGE && vision_blocked {
                            let content = fit_tool_text(
                                tools::look_image_failure(
                                    crate::llm::http_error::VISION_UNSUPPORTED_ERROR,
                                ),
                                tool_budget,
                                tool_used,
                            );
                            tool_used += estimate_tokens(&content);
                            push_tool_turn(
                                &state.db,
                                &thread.id,
                                &mut current_turns,
                                &tc.name,
                                &tc.id,
                                content,
                            )?;
                            continue;
                        }
                        if tc.name == tools::TOOL_LAW && law_calls >= tools::MAX_LAW_CALLS {
                            let content = fit_tool_text(
                                tools::law_call_limit_message().to_string(),
                                tool_budget,
                                tool_used,
                            );
                            tool_used += estimate_tokens(&content);
                            let tool_turn = ChatTurn {
                                role: "tool".into(),
                                content,
                                name: Some(tc.name),
                                tool_call_id: Some(tc.id),
                                tool_calls: None,
                                reasoning: None,
                            };
                            store_turn(&state.db, &thread.id, &tool_turn)?;
                            current_turns.push(tool_turn);
                            continue;
                        }
                        if tc.name == tools::TOOL_LAW {
                            law_calls += 1;
                        }
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
                            let content = fit_tool_text(
                                "一度に読める URL は 2 件までです。必要なものから順に read_url してください。"
                                    .into(),
                                tool_budget,
                                tool_used,
                            );
                            tool_used += estimate_tokens(&content);
                            let tool_turn = ChatTurn {
                                role: "tool".into(),
                                content,
                                name: Some(tc.name),
                                tool_call_id: Some(tc.id),
                                tool_calls: None,
                                reasoning: None,
                            };
                            store_turn(&state.db, &thread.id, &tool_turn)?;
                            current_turns.push(tool_turn);
                            continue;
                        }
                        if tc.name == tools::TOOL_READ_URL {
                            read_url_in_round += 1;
                        }
                        if tc.name == tools::TOOL_READ_NOTE {
                            read_note_called = true;
                        }
                        let run = if tc.name == tools::TOOL_LOOK_IMAGE {
                            run_look_image(
                                state_arc.clone(),
                                thread.id.clone(),
                                tc.arguments.clone(),
                                cancel.clone(),
                            )
                            .await
                        } else {
                            ToolRun::Done(
                                execute_tool_off_runtime(
                                    state_arc.clone(),
                                    thread.id.clone(),
                                    tc.name.clone(),
                                    tc.arguments.clone(),
                                    thread_scope.clone(),
                                    web_search,
                                    request_id.clone(),
                                )
                                .await,
                            )
                        };
                        let exec = match run {
                            ToolRun::Cancelled => {
                                let content =
                                    fit_tool_text("中断しました。".into(), tool_budget, tool_used);
                                tool_used += estimate_tokens(&content);
                                push_tool_turn(
                                    &state.db,
                                    &thread.id,
                                    &mut current_turns,
                                    &tc.name,
                                    &tc.id,
                                    content,
                                )?;
                                cancel_tools = true;
                                continue;
                            }
                            ToolRun::VisionUnsupported(exec) => {
                                vision_blocked = true;
                                exec
                            }
                            ToolRun::Done(exec) => exec,
                        };
                        consumed.extend(exec.consumed);
                        emit_sources_updated(&app, &thread.id);
                        if let Some(nid) = exec.wrote_note_id.clone() {
                            after_llm_note_write(&app, state.as_ref(), &thread.id, &nid);
                            if !wrote_note_ids.iter().any(|x| x == &nid) {
                                wrote_note_ids.push(nid);
                            }
                        }
                        let saved_file = exec.saved_file.clone();
                        let mut content = exec.content;
                        if tc.name == tools::TOOL_WRITE_NOTE && !read_note_called {
                            content.push_str(
                                "\n（警告: この請求では read_note の前に書きました。読んでいない部分を消していないか確認してください。）",
                            );
                        }
                        let content = if tc.name == tools::TOOL_READ_NOTE {
                            content
                        } else {
                            fit_tool_text(content, tool_budget, tool_used)
                        };
                        if tc.name == tools::TOOL_LAW {
                            verified_cites
                                .extend(crate::statute::verified_from_delivered(&content));
                        }
                        tool_used += estimate_tokens(&content);
                        let tool_turn = ChatTurn {
                            role: "tool".into(),
                            content,
                            name: Some(tc.name.clone()),
                            tool_call_id: Some(tc.id),
                            tool_calls: None,
                            reasoning: None,
                        };
                        store_turn(&state.db, &thread.id, &tool_turn)?;
                        current_turns.push(tool_turn);
                        if let Some(notice) = crate::llm::create_office::notice_for_tool(
                            &tc.name,
                            saved_file.as_ref(),
                        ) {
                            let _ = state.db.insert_llm_notice(&thread.id, &notice);
                        }
                    }
                    if cancel_tools {
                        busy.finish();
                        kick_pending_ocr(&app, state_arc.clone());
                        let _ = state.db.delete_uncited_tool_sources(&thread.id);
                        emit_sources_updated(&app, &thread.id);
                        let thread = state
                            .db
                            .get_llm_thread(&thread.id)
                            .map_err(|err| err.to_string())?
                            .unwrap_or(thread);
                        return Ok(LlmSendResult {
                            thread,
                            user_message,
                            assistant_message: None,
                            cancelled: true,
                            error: None,
                            truncated,
                            context_tokens,
                            thinking_budget_state: budget_fields(&settings.llm_base_url).0,
                            thinking_budget_tokens: budget_fields(&settings.llm_base_url).1,
                            warning,
                            note_writes: note_write_notices(state.as_ref(), &wrote_note_ids),
                        });
                    }
                    // A law-only round does not spend the web-search budget, but it must
                    // not keep requesting the model after law_article is exhausted.
                    if law_only && law_calls >= tools::MAX_LAW_CALLS {
                        use_tools = false;
                    }
                    // The reserve is spent; answer from what the rounds already produced.
                    if tool_used >= tool_budget {
                        use_tools = false;
                        if !source_turn_injected {
                            let all = state.db.list_llm_sources(&thread.id).unwrap_or_default();
                            source_turn_injected = inject_final_source_turn(
                                &mut current_turns,
                                &all,
                                &consumed,
                                true,
                                inject_budget,
                            );
                        }
                        if !source_turn_injected {
                            current_turns.push(ChatTurn::text("user", STOP_TOOLS_HINT));
                            source_turn_injected = true;
                        }
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
                        if inject_final_source_turn(
                            &mut current_turns,
                            &all,
                            &consumed,
                            true,
                            inject_budget,
                        ) {
                            source_turn_injected = true;
                            use_tools = false;
                            // This continue is itself a retry. Keep a phantom answer so an
                            // empty follow-up does not throw it away, and so a source fetched
                            // on the retry cannot bind to a number in that answer.
                            remember_phantom_answer(
                                &mut phantom_fallback,
                                &out.content,
                                &out.reasoning,
                                &all,
                            );
                            emit_chat_reset(&app, &request_id, &thread.id);
                            continue;
                        }
                    }
                }
                if !phantom_retried {
                    let all = state.db.list_llm_sources(&thread.id).unwrap_or_default();
                    if answer_has_phantom_cites(&out.content, &all) {
                        phantom_retried = true;
                        // At the cap, turning tools back on makes the next pass the
                        // round-limit final answer (notice and stream reset).
                        // A server that already rejected tools stays tool-less too.
                        let tools_on = phantom_retry_tools_on(
                            unlimited_rounds,
                            rounds,
                            max_rounds,
                            cut_hold.tools_off,
                            retried_without_tools,
                        );
                        current_turns.push(ChatTurn::text("user", phantom_cite_hint(tools_on)));
                        use_tools = tools_on;
                        remember_phantom_answer(
                            &mut phantom_fallback,
                            &out.content,
                            &out.reasoning,
                            &all,
                        );
                        emit_chat_reset(&app, &request_id, &thread.id);
                        continue;
                    }
                }
                match answer_after_phantom_retry(
                    out.content.trim().is_empty(),
                    phantom_fallback.is_some(),
                ) {
                    PhantomRetryAnswer::Original => {
                        let original = phantom_fallback
                            .take()
                            .expect("empty phantom retry keeps the first answer");
                        final_reasoning = original.reasoning;
                        result_text = original.content;
                        kept_source_ids = Some(original.source_ids);
                    }
                    PhantomRetryAnswer::EmptyNotice => {
                        if let Some(original) = phantom_fallback.take() {
                            final_reasoning = original.reasoning;
                            result_text = original.content;
                            kept_source_ids = Some(original.source_ids);
                        } else if let Some(kept) = recovered_cut_to_keep(&recovered_cut) {
                            final_reasoning = kept.reasoning.clone();
                            result_text = kept.content.clone();
                            kept_source_ids = Some(kept.source_ids.clone());
                        } else {
                            empty_reply = true;
                            final_reasoning = out.reasoning;
                            result_text = silent_end_notice(&out.finish_reason);
                        }
                    }
                    PhantomRetryAnswer::Retry => {
                        final_reasoning = out.reasoning;
                        result_text = out.content;
                    }
                }
                break;
            }
        }
    }

    result_text = crate::statute::mark_unverified(&result_text, &verified_cites);

    busy.finish();
    kick_pending_ocr(&app, state_arc.clone());

    let result_kind = if (round_limit_final && round_limit_aside.is_none()) || empty_reply {
        crate::db::LLM_MESSAGE_NOTICE
    } else {
        ""
    };
    let assistant_message = if result_text.is_empty() {
        None
    } else {
        Some(save_assistant_answer(
            &state.db,
            &thread.id,
            &result_text,
            &final_reasoning,
            result_kind,
        )?)
    };
    if let Some(notice) = round_limit_aside.as_deref() {
        let _ = state.db.insert_llm_notice(&thread.id, notice);
    }
    if let Some(notice) =
        cut_hold.notice(result_kind == crate::db::LLM_MESSAGE_NOTICE, &result_text)
    {
        let _ = state.db.insert_llm_notice(&thread.id, notice);
    }
    let unset_notice = llm::thinking::thinking_budget_unset_notice();
    let already_told = history.iter().any(|message| {
        message.kind == crate::db::LLM_MESSAGE_NOTICE && message.content == unset_notice
    }) || result_text == unset_notice;
    if llm::thinking::known_budget_for(&llm::normalize_base_url(&settings.llm_base_url))
        == llm::thinking::KnownBudget::Unset
        && !already_told
    {
        let _ = state.db.insert_llm_notice(&thread.id, &unset_notice);
    }
    if let Some(assistant) = assistant_message.as_ref() {
        let cited = citations_for_saved_answer(&consumed, &result_text, kept_source_ids.as_ref());
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
        error: None,
        truncated,
        context_tokens,
        thinking_budget_state: budget_fields(&settings.llm_base_url).0,
        thinking_budget_tokens: budget_fields(&settings.llm_base_url).1,
        warning,
        note_writes: note_write_notices(state.as_ref(), &wrote_note_ids),
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

fn emit_sources_updated<R: Runtime>(app: &AppHandle<R>, thread_id: &str) {
    let _ = app.emit(
        "llm-sources-updated",
        LlmSourcesUpdated {
            thread_id: thread_id.to_string(),
        },
    );
}

fn emit_ocr_status<R: Runtime>(app: &AppHandle<R>, thread_id: &str, active: bool) {
    let _ = app.emit(
        "llm-ocr-status",
        LlmOcrStatus {
            thread_id: thread_id.to_string(),
            active,
        },
    );
}

fn kick_pending_ocr<R: Runtime>(app: &AppHandle<R>, state: Arc<AppState>) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        run_ocr_queue(app, state).await;
    });
}

async fn run_ocr_queue<R: Runtime>(app: AppHandle<R>, state: Arc<AppState>) {
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

async fn run_ocr_for_thread<R: Runtime>(
    app: &AppHandle<R>,
    state: Arc<AppState>,
    thread_id: String,
) {
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

/// Size of the next request: assembled turns, including the system prompt, plus the tool schema.
/// Runs off the UI thread. An empty draft is measured as one new question.
#[tauri::command]
pub async fn llm_context_meter(
    state: State<'_, Arc<AppState>>,
    thread_id: Option<String>,
    draft: String,
    web_search: Option<bool>,
) -> Result<usize, String> {
    let state = state.inner().clone();
    let thread_id = thread_id.unwrap_or_default();
    let web_search = web_search.unwrap_or(false);
    let (tokens, _) = tauri::async_runtime::spawn_blocking(move || {
        measure_context_meter(state, thread_id, draft, web_search, None)
    })
    .await
    .map_err(|err| err.to_string())??;
    Ok(tokens)
}

/// Next request if `source_id` were the whole file, assembled with the same
/// budget `llm_send` passes to `assemble_turns`. `tokens` is that request's
/// size, the meter numerator after the switch. `body_trimmed` is set only when
/// this file's own body is cut. Old exchanges are dropped before that.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmFileGrainMeter {
    pub tokens: usize,
    pub body_trimmed: bool,
}

#[tauri::command]
pub async fn llm_file_grain_meter(
    state: State<'_, Arc<AppState>>,
    thread_id: String,
    source_id: String,
    draft: String,
    web_search: Option<bool>,
) -> Result<LlmFileGrainMeter, String> {
    let state = state.inner().clone();
    let web_search = web_search.unwrap_or(false);
    let (tokens, body_trimmed) = tauri::async_runtime::spawn_blocking(move || {
        measure_context_meter(state, thread_id, draft, web_search, Some(source_id))
    })
    .await
    .map_err(|err| err.to_string())??;
    Ok(LlmFileGrainMeter {
        tokens,
        body_trimmed,
    })
}

struct FileGrainReading {
    tokens: usize,
    body_trimmed: bool,
}

/// Same extra user row `meter_usage_tokens` appends. Kept in step with
/// `history_including_draft` so this count is the meter numerator.
fn history_with_meter_draft(history: &[LlmMessageRow], draft: &str) -> Vec<LlmMessageRow> {
    let mut history = history.to_vec();
    history.push(LlmMessageRow {
        id: "meter-draft".into(),
        thread_id: history
            .last()
            .map(|row| row.thread_id.clone())
            .unwrap_or_default(),
        role: "user".into(),
        content: meter_question(draft).to_string(),
        created_at: 0,
        reasoning: String::new(),
        tool_calls: String::new(),
        tool_call_id: String::new(),
        tool_name: String::new(),
        kind: String::new(),
        carries_summary: false,
    });
    history
}

fn file_grain_reading(
    system: &str,
    sources: &[LlmSourceRow],
    history: &[LlmMessageRow],
    draft: &str,
    schema_tokens: usize,
    message_budget: usize,
    source_id: &str,
) -> FileGrainReading {
    let history = history_with_meter_draft(history, draft);
    let (turns, _) = assemble_turns(system, sources, &history, message_budget.max(1));
    let full = sources
        .iter()
        .find(|source| source.id == source_id)
        .map(|source| source.body.trim().to_string())
        .unwrap_or_default();
    let latest = turns
        .iter()
        .rev()
        .find(|turn| turn.role == "user")
        .map(|turn| turn.content.as_str())
        .unwrap_or("");
    FileGrainReading {
        tokens: meter_request_tokens(&turns, schema_tokens),
        body_trimmed: !full.is_empty() && !latest.contains(full.as_str()),
    }
}

/// Pending rows of the same path are dropped, matching `llm_set_source_grain`.
fn sources_with_file_body(
    mut sources: Vec<LlmSourceRow>,
    id: &str,
    file_body: String,
) -> Result<Vec<LlmSourceRow>, String> {
    let idx = sources
        .iter()
        .position(|source| source.id == id)
        .ok_or_else(|| "出典が見つかりません".to_string())?;
    let path = sources[idx].path.trim().to_string();
    sources[idx].body = file_body;
    sources[idx].grain = "file".into();
    if !path.is_empty() {
        sources.retain(|source| {
            source.id == id || !source.is_pending() || !source.path.eq_ignore_ascii_case(&path)
        });
    }
    Ok(sources)
}

fn reject_file_grain(row: &LlmSourceRow) -> Result<(), String> {
    if row.is_image() {
        return Err("画像出典は段落／全文を切り替えられません。".into());
    }
    if row.is_web() {
        return Err("ウェブ出典は段落／全文を切り替えられません。".into());
    }
    if row.is_remote() {
        return Err("リモート出典は段落／全文を切り替えられません。".into());
    }
    if !row.is_pending() {
        return Err("読み込み済みの出典は段落／全文を切り替えられません。".into());
    }
    if row.path.trim().is_empty() {
        return Err("パスが無い出典はファイル全体にできません。".into());
    }
    Ok(())
}

fn expand_pending_file(
    state: &AppState,
    sources: Vec<LlmSourceRow>,
    id: &str,
) -> Result<Vec<LlmSourceRow>, String> {
    let path = {
        let row = sources
            .iter()
            .find(|source| source.id == id)
            .ok_or_else(|| "出典が見つかりません".to_string())?;
        reject_file_grain(row)?;
        row.path.trim().to_string()
    };
    let (body, _) = load_file_body(state, &path)?;
    sources_with_file_body(sources, id, body)
}

fn measure_context_meter(
    state: Arc<AppState>,
    thread_id: String,
    draft: String,
    web_search: bool,
    expand_source_id: Option<String>,
) -> Result<(usize, bool), String> {
    let question = meter_question(&draft).to_string();
    let settings = state.settings.read().clone();
    let (thread, history, mut sources) = if thread_id.trim().is_empty() {
        (blank_llm_thread(), Vec::new(), Vec::new())
    } else {
        let thread = state
            .db
            .get_llm_thread(thread_id.trim())
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "会話が見つかりません".to_string())?;
        let history = state
            .db
            .list_llm_messages(&thread.id)
            .map_err(|e| e.to_string())?;
        let sources = state
            .db
            .list_llm_sources(&thread.id)
            .map_err(|e| e.to_string())?;
        (thread, history, sources)
    };
    let expand_id = expand_source_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string);
    if let Some(id) = expand_id.as_deref() {
        sources = expand_pending_file(&state, sources, id)?;
    }
    let plan = plan_chat_request(
        &state, &settings, &thread, &question, web_search, &sources, None,
    );
    let history =
        summary::prepare_model_history(&history, &thread.summary, &thread.summary_through_id);
    if let Some(id) = expand_id.as_deref() {
        let reading = file_grain_reading(
            &plan.system,
            &sources,
            &history,
            &draft,
            plan.schema_tokens,
            plan.prompt_budget,
            id,
        );
        return Ok((reading.tokens, reading.body_trimmed));
    }
    Ok((
        meter_usage_tokens(
            &plan.system,
            &sources,
            &history,
            &draft,
            plan.schema_tokens,
            plan.prompt_budget,
        ),
        false,
    ))
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
    if row.is_remote() {
        return Err("リモート出典は段落／全文を切り替えられません。".into());
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

    finish_attach(
        &app,
        state.inner(),
        thread,
        added,
        skipped,
        created_thread,
        errors,
        warnings,
        wants_ocr,
    )
}

fn decode_pasted_image(data_base64: &str) -> Result<Vec<u8>, String> {
    use base64::Engine;
    let cleaned: String = data_base64.chars().filter(|c| !c.is_whitespace()).collect();
    if cleaned.is_empty() {
        return Err("画像ファイルが空です。".into());
    }
    base64::engine::general_purpose::STANDARD
        .decode(cleaned)
        .map_err(|_| "画像データが不正です。".to_string())
}

fn prepare_pasted_image(data_base64: &str, mime: &str) -> Result<(Vec<u8>, &'static str), String> {
    let bytes = decode_pasted_image(data_base64)?;
    let ext = files::pasted_image_ext(mime)?;
    if let Some(err) = files::image_len_error(bytes.len() as u64) {
        return Err(err);
    }
    Ok((bytes, ext))
}

fn unique_paste_label(db: &crate::db::Db, thread_id: &str, base: &str) -> Result<String, String> {
    for n in 1..50u32 {
        let label = files::disambiguate_paste_label(base, n);
        let exists = db
            .find_llm_source_by_path(thread_id, &label)
            .map_err(|e| e.to_string())?;
        if exists.is_none() {
            return Ok(label);
        }
    }
    Ok(format!(
        "{}-{}",
        files::disambiguate_paste_label(base, 50),
        uuid::Uuid::new_v4()
    ))
}

fn attach_pasted_image_bytes(
    db: &crate::db::Db,
    data_dir: &Path,
    thread_id: &str,
    bytes: &[u8],
    ext: &str,
    base_title: &str,
) -> Result<AttachOutcome, String> {
    let label = unique_paste_label(db, thread_id, base_title)?;
    let id = uuid::Uuid::new_v4().to_string();
    let rel = files::stored_relpath(thread_id, &id, ext);
    files::write_bytes_into_store(data_dir, &rel, bytes)?;
    match db.insert_llm_source_full(
        thread_id,
        "attach",
        &label,
        &label,
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
            if row.id != id && row.stored_relpath != rel {
                files::remove_stored(data_dir, &rel);
            }
            Ok(AttachOutcome {
                created: usize::from(created),
                skipped: usize::from(!created),
                needs_ocr: row.is_image() && !row.is_injectable(),
                warning: None,
            })
        }
        Err(err) => {
            files::remove_stored(data_dir, &rel);
            Err(err.to_string())
        }
    }
}

fn finish_attach(
    app: &AppHandle,
    state: &Arc<AppState>,
    thread: LlmThreadRow,
    added: usize,
    skipped: usize,
    created_thread: bool,
    errors: Vec<LlmAttachFileError>,
    warnings: Vec<String>,
    wants_ocr: bool,
) -> Result<LlmAttachFilesResult, String> {
    let sources = state
        .db
        .list_llm_sources(&thread.id)
        .map_err(|e| e.to_string())?;
    let thread = state
        .db
        .get_llm_thread(&thread.id)
        .map_err(|e| e.to_string())?
        .unwrap_or(thread);
    crate::show_chat(app);
    emit_sources_updated(app, &thread.id);
    let settings = state.settings.read().clone();
    let remote_ocr = wants_ocr && !llm::is_loopback_url(&settings.llm_base_url);
    let ocr_started = wants_ocr && !state.is_llm_busy();
    if ocr_started {
        kick_pending_ocr(app, Arc::clone(state));
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
pub fn llm_attach_pasted_image(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    data_base64: String,
    mime: String,
    thread_id: Option<String>,
) -> Result<LlmAttachFilesResult, String> {
    let (bytes, ext) = prepare_pasted_image(&data_base64, &mime)?;
    let title = files::pasted_image_title(chrono::Local::now().naive_local());
    let (thread, created_thread) =
        resolve_attach_thread(state.inner(), thread_id.as_deref(), &title)?;
    if !state.is_llm_busy() {
        state
            .db
            .set_active_llm_thread_id(Some(&thread.id))
            .map_err(|e| e.to_string())?;
    }
    let outcome = match attach_pasted_image_bytes(
        &state.db,
        &state.data_dir,
        &thread.id,
        &bytes,
        ext,
        &title,
    ) {
        Ok(outcome) => outcome,
        Err(err) => {
            if created_thread {
                let _ = state.db.delete_llm_thread(&thread.id);
                files::remove_thread_store(&state.data_dir, &thread.id);
            }
            return Err(err);
        }
    };
    if created_thread && outcome.created == 0 {
        let _ = state.db.delete_llm_thread(&thread.id);
        files::remove_thread_store(&state.data_dir, &thread.id);
        return Err("同じ出典がすでに読込前にあります。".into());
    }
    let warnings = outcome.warning.into_iter().collect();
    finish_attach(
        &app,
        state.inner(),
        thread,
        outcome.created,
        outcome.skipped,
        created_thread,
        Vec::new(),
        warnings,
        outcome.needs_ocr,
    )
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

fn cut_reply(result: &Result<llm::StreamOutcome, String>) -> llm::thinking::CutReply {
    match result {
        Err(e) if e.starts_with(llm::thinking::THINKING_RUNAWAY_PREFIX) => {
            llm::thinking::CutReply::OverCap
        }
        Err(e) if llm::is_tools_unsupported_error(e) => llm::thinking::CutReply::ToolsRejected,
        Err(_) => llm::thinking::CutReply::Other,
        Ok(out) if !out.tool_calls.is_empty() || out.finish_reason == "tool_calls" => {
            llm::thinking::CutReply::NeedsTools
        }
        Ok(_) => llm::thinking::CutReply::Answer,
    }
}

/// A phantom [n] may call tools again only while rounds remain.
/// At the cap the retry stays tool-free, so it is not the round-limit final answer.
fn phantom_retry_enables_tools(unlimited_rounds: bool, rounds: usize, max_rounds: usize) -> bool {
    unlimited_rounds || rounds < max_rounds
}

/// Tools stay off when this turn already learned the server rejects them, and
/// when cut recovery itself removed them. A tool-less citation re-request on a
/// tool-capable server does not set `tools_locked_off`, so rounds may restore tools.
fn phantom_retry_tools_on(
    unlimited_rounds: bool,
    rounds: usize,
    max_rounds: usize,
    tools_locked_off: bool,
    retried_without_tools: bool,
) -> bool {
    phantom_retry_enables_tools(unlimited_rounds, rounds, max_rounds)
        && !tools_locked_off
        && !retried_without_tools
}

fn phantom_cite_hint(tools_on: bool) -> &'static str {
    if tools_on {
        PHANTOM_CITE_HINT
    } else {
        PHANTOM_CITE_HINT_NO_TOOLS
    }
}

enum PhantomRetryAnswer {
    Retry,
    Original,
    EmptyNotice,
}

/// An empty phantom-cite retry keeps the first answer. An empty first reply is the notice.
fn answer_after_phantom_retry(retry_empty: bool, had_original: bool) -> PhantomRetryAnswer {
    if !retry_empty {
        PhantomRetryAnswer::Retry
    } else if had_original {
        PhantomRetryAnswer::Original
    } else {
        PhantomRetryAnswer::EmptyNotice
    }
}

struct PhantomOriginal {
    content: String,
    reasoning: String,
    source_ids: std::collections::HashSet<String>,
}

struct RecoveredCut {
    content: String,
    reasoning: String,
    /// Sources that already existed when this answer was produced.
    source_ids: std::collections::HashSet<String>,
}

/// The cut already answered. A later retry must not replace this with a failure.
/// Text that rides along with a tool call is a preamble, not a new answer.
fn remember_recovered_cut(
    slot: &mut Option<RecoveredCut>,
    thinking_was_cut: bool,
    content: &str,
    reasoning: &str,
    has_tool_calls: bool,
    source_ids: std::collections::HashSet<String>,
) {
    if thinking_was_cut && !has_tool_calls && !content.trim().is_empty() {
        *slot = Some(RecoveredCut {
            content: content.to_string(),
            reasoning: reasoning.to_string(),
            source_ids,
        });
    }
}

fn recovered_cut_to_keep(slot: &Option<RecoveredCut>) -> Option<&RecoveredCut> {
    slot.as_ref().filter(|row| !row.content.trim().is_empty())
}

/// Phantom text wins, then the cut answer. Sources are those from that moment.
fn take_kept_answer(
    phantom: &mut Option<PhantomOriginal>,
    recovered: &mut Option<RecoveredCut>,
) -> Option<RecoveredCut> {
    if let Some(original) = phantom.take() {
        return Some(RecoveredCut {
            content: original.content,
            reasoning: original.reasoning,
            source_ids: original.source_ids,
        });
    }
    let kept = recovered.take()?;
    if kept.content.trim().is_empty() {
        None
    } else {
        Some(kept)
    }
}

/// Remember the first phantom answer and the source ids that existed then.
/// A later retry must not replace it, and must not bind a newly fetched row to it.
fn remember_phantom_answer(
    slot: &mut Option<PhantomOriginal>,
    content: &str,
    reasoning: &str,
    sources: &[LlmSourceRow],
) {
    if slot.is_some() || !answer_has_phantom_cites(content, sources) {
        return;
    }
    slot.replace(PhantomOriginal {
        content: content.to_string(),
        reasoning: reasoning.to_string(),
        source_ids: sources.iter().map(|source| source.id.clone()).collect(),
    });
}

/// A saved answer already used `[n]`. Later sources must start above that.
/// Notices are instructions to the user, not citations the model wrote.
fn raise_ledger_for_kept_answer(db: &crate::db::Db, thread_id: &str, content: &str) {
    let max_n = parse_cited_nos(content)
        .into_iter()
        .filter(|n| *n > 0 && *n <= crate::db::LLM_CITE_SCAN_MAX)
        .max()
        .unwrap_or(0);
    if max_n > 0 {
        let _ = db.raise_llm_cite_counter(thread_id, max_n);
    }
}

/// The one place a finished assistant answer is stored. Every non-notice answer,
/// including a phantom retry and a round-limit final answer, advances the ledger here.
fn save_assistant_answer(
    db: &crate::db::Db,
    thread_id: &str,
    content: &str,
    reasoning: &str,
    kind: &str,
) -> Result<crate::db::LlmMessageRow, String> {
    if kind != crate::db::LLM_MESSAGE_NOTICE {
        raise_ledger_for_kept_answer(db, thread_id, content);
    }
    db.insert_llm_message_full(thread_id, "assistant", content, reasoning, "", "", "", kind)
        .map_err(|e| e.to_string())
}

/// A kept answer may cite only sources that existed when it was written.
fn citations_for_saved_answer(
    consumed: &[(String, i64)],
    answer: &str,
    existed: Option<&std::collections::HashSet<String>>,
) -> Vec<(String, i64)> {
    let mut cited = consumed_cited_in_answer(consumed, answer);
    if let Some(ids) = existed {
        cited.retain(|(id, _)| ids.contains(id));
    }
    cited
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
            fit_tool_text(body.clone(), 500, 0),
            body,
            "content within the reserve passes through untouched"
        );

        let trimmed = fit_tool_text(body.clone(), 20, 0);
        assert!(trimmed.chars().count() < body.chars().count());
        assert!(
            trimmed.contains("省略"),
            "the model must be told the result was cut: {trimmed}"
        );

        let exhausted = fit_tool_text(body, 20, 20);
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
        let mut turns = vec![ChatTurn {
            role: "tool".into(),
            content: crate::llm::context::format_sources(std::slice::from_ref(&s)),
            name: Some("search_index".into()),
            tool_call_id: Some("c1".into()),
            tool_calls: None,
            reasoning: None,
        }];
        assert!(inject_final_source_turn(
            &mut turns,
            std::slice::from_ref(&s),
            &[("hit".into(), 2)],
            true,
            usize::MAX,
        ));
        let blob = turns
            .iter()
            .map(|turn| turn.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(blob.matches("第936条").count(), 1, "{blob}");
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[1].role, "user");
        assert!(turns[0].content.contains("第936条"), "{}", turns[0].content);
        assert!(
            !turns[1].content.contains("第936条"),
            "a cite already in the tool result is not copied again: {}",
            turns[1].content
        );
        assert!(
            !turns[1].content.contains("【出典】"),
            "the tool body is not copied onto the hint turn: {}",
            turns[1].content
        );
        assert!(
            turns[1].content.contains("[n]"),
            "the cite instruction stays when the body is already in the tool result: {}",
            turns[1].content
        );
        assert!(turns[1]
            .content
            .contains(crate::llm::context::FINAL_CITE_HINT));
        assert!(turns[1].content.contains("ツールはこれ以上"));
        s.id = "other".into();
        let mut empty = Vec::new();
        assert!(!inject_final_source_turn(
            &mut empty,
            std::slice::from_ref(&s),
            &[("hit".into(), 2)],
            false,
            usize::MAX,
        ));
    }

    #[test]
    fn injected_turn_does_not_repeat_an_attachment_already_on_the_question() {
        let body = "添付の本文です。二度送らない。".repeat(40);
        let mut attached = LlmSourceRow {
            id: "file".into(),
            thread_id: "t".into(),
            sort_order: 0,
            origin: "attach".into(),
            path: r"C:\docs\memo.txt".into(),
            title: "memo.txt".into(),
            paragraph_id: String::new(),
            body: body.clone(),
            query: String::new(),
            created_at: 0,
            grain: "file".into(),
            unit_body: String::new(),
            injected_user_message_id: String::new(),
            cited_assistant_message_id: String::new(),
            cite_no: 3,
            kind: "text".into(),
            stored_relpath: String::new(),
            ocr_status: String::new(),
        };
        let header = crate::llm::context::source_header_lines(&attached)[0].clone();
        let truncated = format!("【出典】\n{header}\n{}\n\n【質問】\n見て", &body[..12]);
        let mut turns = vec![ChatTurn::text("user", truncated)];
        assert!(inject_final_source_turn(
            &mut turns,
            std::slice::from_ref(&attached),
            &[("file".into(), 3)],
            true,
            32_768,
        ));
        let blob = turns
            .iter()
            .map(|turn| turn.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            blob.matches(body.as_str()).count(),
            0,
            "a truncated copy on the question is still that source: {blob}"
        );
        assert!(turns
            .iter()
            .any(|turn| turn.content.contains(STOP_TOOLS_HINT)));
        let full = "あ".repeat(40_000);
        attached.body = full.clone();
        let stored = fit_tool_text(
            crate::llm::context::format_sources(std::slice::from_ref(&attached)),
            2_000,
            0,
        );
        assert!(stored.contains("[3]"));
        assert!(!stored.contains(&full));
        let tool = ChatTurn {
            role: "tool".into(),
            content: stored.clone(),
            name: Some("read_unit".into()),
            tool_call_id: Some("c".into()),
            tool_calls: None,
            reasoning: None,
        };
        let mut wide = vec![ChatTurn::text("user", "質問"), tool];
        assert!(
            inject_final_source_turn(
                &mut wide,
                std::slice::from_ref(&attached),
                &[("file".into(), 3)],
                false,
                8_000,
            ),
            "the cite instruction is still added"
        );
        assert_eq!(wide[1].content, stored);
        assert!(wide
            .last()
            .unwrap()
            .content
            .contains(crate::llm::context::FINAL_CITE_HINT));
        assert!(!wide.iter().any(|turn| turn.content.contains(&full)));
    }

    #[test]
    fn injection_does_not_restore_tool_hits_cut_out_of_the_result() {
        let mut rows = Vec::new();
        for n in 1..=8 {
            let body = format!("HIT{n}-{}", "あ".repeat(1_200));
            rows.push(LlmSourceRow {
                id: format!("h{n}"),
                thread_id: "t".into(),
                sort_order: n,
                origin: "tool".into(),
                path: format!(r"C:\law\{n}.txt"),
                title: format!("{n}.txt"),
                paragraph_id: format!("p{n}"),
                body,
                query: String::new(),
                created_at: 0,
                grain: "unit".into(),
                unit_body: String::new(),
                injected_user_message_id: String::new(),
                cited_assistant_message_id: String::new(),
                cite_no: n,
                kind: "text".into(),
                stored_relpath: String::new(),
                ocr_status: String::new(),
            });
        }
        let stored = fit_tool_text(crate::llm::context::format_sources(&rows), 3_000, 0);
        assert!(stored.contains("[1]"), "{stored}");
        let cut: Vec<_> = rows
            .iter()
            .filter(|row| !stored.contains(&row.body))
            .map(|row| row.cite_no)
            .collect();
        assert!(
            cut.len() >= 2,
            "the fixture must cut later hits, stored tokens {}",
            estimate_tokens(&stored)
        );
        let consumed: Vec<_> = rows
            .iter()
            .map(|row| (row.id.clone(), row.cite_no))
            .collect();
        let mut turns = vec![ChatTurn {
            role: "tool".into(),
            content: stored,
            name: Some("search_index".into()),
            tool_call_id: Some("c".into()),
            tool_calls: None,
            reasoning: None,
        }];
        assert!(inject_final_source_turn(
            &mut turns, &rows, &consumed, true, 16_384,
        ));
        let blob = turns
            .iter()
            .map(|turn| turn.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        for row in rows.iter().filter(|row| cut.contains(&row.cite_no)) {
            assert!(
                !blob.contains(&row.body),
                "hit {} was cut from the tool result and must not come back",
                row.cite_no
            );
        }
        assert!(blob.contains(crate::llm::context::FINAL_CITE_HINT));
    }

    #[test]
    fn injected_turn_stays_inside_the_reply_reserve() {
        let split = context_budget(16_384, 2_105, 200).expect("room for a short question");
        let budget = split.messages + split.tool_allowance;
        assert_eq!(
            16_384 - 2_105 - budget,
            crate::llm::context::reply_headroom_tokens(16_384)
        );
        let full = "あ".repeat(40_000);
        let hit = LlmSourceRow {
            id: "hit".into(),
            thread_id: "t".into(),
            sort_order: 0,
            origin: "attach".into(),
            path: r"C:\law\民法.txt".into(),
            title: "民法.txt".into(),
            paragraph_id: "p9".into(),
            body: full.clone(),
            query: String::new(),
            created_at: 0,
            grain: "unit".into(),
            unit_body: String::new(),
            injected_user_message_id: String::new(),
            cited_assistant_message_id: String::new(),
            cite_no: 9,
            kind: "text".into(),
            stored_relpath: String::new(),
            ocr_status: String::new(),
        };
        let mut turns = vec![ChatTurn::text("user", "質問")];
        assert!(inject_final_source_turn(
            &mut turns,
            std::slice::from_ref(&hit),
            &[("hit".into(), 9)],
            false,
            budget,
        ));
        let tokens: usize = turns.iter().map(crate::llm::context::turn_tokens).sum();
        assert!(tokens <= budget, "{tokens} > {budget}");
        let injected = turns.last().unwrap();
        assert!(
            injected
                .content
                .contains(crate::llm::context::FINAL_CITE_HINT),
            "{}",
            injected.content
        );
        assert!(!injected.content.contains(&full));
    }

    #[test]
    fn refused_question_is_removed_from_replayed_history() {
        let (dir, db) = temp_db("refuse");
        let thread = db.create_llm_thread("会話", true).unwrap();
        let row = db
            .insert_llm_message(&thread.id, "user", &"あ".repeat(40_000))
            .unwrap();
        discard_refused_user_message(&db, &row.id);
        let history = db.list_llm_messages(&thread.id).unwrap();
        assert!(history.is_empty());
        let (turns, _) = assemble_turns("役割", &[], &history, 500);
        assert!(turns.iter().all(|turn| turn.role != "user"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn required_prompt_ignores_a_pending_attachment() {
        let body = "添".repeat(20_000);
        let mut file = LlmSourceRow {
            id: "file".into(),
            thread_id: "t".into(),
            sort_order: 0,
            origin: "attach".into(),
            path: r"C:\docs\添付.txt".into(),
            title: "添付.txt".into(),
            paragraph_id: String::new(),
            body: body.clone(),
            query: String::new(),
            created_at: 0,
            grain: "file".into(),
            unit_body: String::new(),
            injected_user_message_id: String::new(),
            cited_assistant_message_id: String::new(),
            cite_no: 1,
            kind: "text".into(),
            stored_relpath: String::new(),
            ocr_status: String::new(),
        };
        let with_file = required_prompt_tokens("役割", "見て", std::slice::from_ref(&file));
        assert!(with_file < estimate_tokens(&body), "{with_file}");
        assert!(context_budget(16_384, 2_105, with_file).is_some());
        file.body.clear();
        let question = "あ".repeat(40_000);
        let too_big = required_prompt_tokens("役割", &question, &[]);
        assert!(context_budget(16_384, 2_105, too_big).is_none());
    }

    #[test]
    fn phantom_retry_at_the_round_cap_stays_tool_free() {
        assert!(phantom_retry_enables_tools(false, 7, 8));
        assert!(
            !phantom_retry_enables_tools(false, 8, 8),
            "the cap is already used, so the retry must not turn tools back on"
        );
        assert!(!phantom_retry_enables_tools(false, 9, 8));
        assert!(
            phantom_retry_enables_tools(true, 8, 0),
            "unlimited rounds keep tools on"
        );
    }

    #[test]
    fn phantom_retry_at_the_cap_does_not_ask_for_tools_and_keeps_an_empty_retry() {
        let hint = phantom_cite_hint(false);
        assert_ne!(hint, PHANTOM_CITE_HINT);
        assert!(
            !hint.contains("ツールで"),
            "a tool-free retry must not tell the model to call a tool: {hint}"
        );
        assert!(phantom_cite_hint(true).contains("ツールで"));
        assert!(matches!(
            answer_after_phantom_retry(true, true),
            PhantomRetryAnswer::Original
        ));
        assert!(matches!(
            answer_after_phantom_retry(false, true),
            PhantomRetryAnswer::Retry
        ));
        assert!(matches!(
            answer_after_phantom_retry(true, false),
            PhantomRetryAnswer::EmptyNotice
        ));
    }

    #[test]
    fn pasted_image_is_stored_for_the_same_pre_read_as_a_file() {
        let (dir, db) = temp_db("paste-image");
        let thread = db.create_llm_thread("会話", false).unwrap();
        let png = [0x89u8, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
        let title = "貼り付け画像（2026-11-12 11時59分01秒）";
        let outcome = attach_pasted_image_bytes(&db, &dir, &thread.id, &png, "png", title).unwrap();
        assert_eq!(outcome.created, 1);
        assert!(outcome.needs_ocr);
        let sources = db.list_llm_sources(&thread.id).unwrap();
        assert_eq!(sources.len(), 1);
        let row = &sources[0];
        assert_eq!(row.kind, "image");
        assert_eq!(row.ocr_status, "pending");
        assert_eq!(row.title, title);
        assert_eq!(row.path, title);
        assert!(row.body.is_empty());
        assert!(row.cite_no > 0);
        assert!(crate::llm::tools::thread_has_relook_image(&sources));
        let stored = files::resolve_stored(&dir, &row.stored_relpath).unwrap();
        assert_eq!(std::fs::read(&stored).unwrap(), png);
        let again = attach_pasted_image_bytes(&db, &dir, &thread.id, &png, "png", title).unwrap();
        assert_eq!(again.created, 1);
        let sources = db.list_llm_sources(&thread.id).unwrap();
        assert_eq!(sources.len(), 2);
        assert_ne!(sources[0].path, sources[1].path);
        assert!(sources[1].title.contains('・'), "{}", sources[1].title);
        assert_ne!(sources[0].cite_no, sources[1].cite_no);
        use base64::Engine;
        let encoded = base64::engine::general_purpose::STANDARD.encode(png);
        let prepared = prepare_pasted_image(&encoded, "image/png").unwrap();
        assert_eq!(prepared.0, png);
        assert_eq!(prepared.1, "png");
        assert!(prepare_pasted_image("@@@@", "image/png").is_err());
        assert!(prepare_pasted_image(&encoded, "image/bmp")
            .unwrap_err()
            .contains("対応していない形式"));
        let _ = std::fs::remove_dir_all(dir);
    }

    fn temp_db(label: &str) -> (std::path::PathBuf, crate::db::Db) {
        let dir = std::env::temp_dir().join(format!(
            "argos-{label}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db = crate::db::Db::open(&dir.join("argos.db")).unwrap();
        (dir, db)
    }

    /// Turn 1 searches [1]–[4], the answer cites only [1], and the rest are deleted.
    fn thread_after_partial_citation(db: &crate::db::Db) -> crate::db::LlmThreadRow {
        let thread = db.create_llm_thread("調査", true).unwrap();
        let user = db
            .insert_llm_message(&thread.id, "user", "Aを調べて")
            .unwrap();
        let assistant = db
            .insert_llm_message(&thread.id, "assistant", "Aです [1]")
            .unwrap();
        let titles = ["A", "B", "C", "D"];
        let mut rows = Vec::new();
        for (index, title) in titles.iter().enumerate() {
            let n = (index + 1) as i64;
            let (row, created) = db
                .insert_llm_source(
                    &thread.id,
                    "tool",
                    &format!("path-{title}"),
                    title,
                    &format!("p{n}"),
                    title,
                    "query",
                )
                .unwrap();
            assert!(created);
            assert_eq!(row.cite_no, n);
            rows.push(row);
        }
        let tool_text = crate::llm::context::format_sources(&rows);
        assert!(tool_text.contains("[1]"));
        assert!(tool_text.contains("[4]"));
        db.insert_llm_message_full(
            &thread.id,
            "tool",
            &tool_text,
            "",
            "",
            "call-1",
            "search_index",
            "",
        )
        .unwrap();
        db.consume_llm_sources(&[(rows[0].id.clone(), 1)], &user.id, &assistant.id)
            .unwrap();
        assert_eq!(db.delete_uncited_tool_sources(&thread.id).unwrap(), 3);
        db.insert_llm_message(&thread.id, "user", "これも見て")
            .unwrap();
        thread
    }

    fn user_block_for(db: &crate::db::Db, thread_id: &str) -> String {
        let sources = db.list_llm_sources(thread_id).unwrap();
        let history = db.list_llm_messages(thread_id).unwrap();
        let (turns, _) = assemble_turns("役割", &sources, &history, 80_000);
        turns
            .iter()
            .rev()
            .find(|turn| turn.role == "user")
            .map(|turn| turn.content.clone())
            .unwrap_or_default()
    }

    #[test]
    fn attach_after_deleted_uncited_hits_gets_the_next_ledger_number() {
        let (dir, db) = temp_db("cite-attach");
        let thread = thread_after_partial_citation(&db);
        let (file, _) = db
            .insert_llm_source(
                &thread.id,
                "attach",
                r"C:\memo.txt",
                "memo.txt",
                "file",
                "添付本文",
                "",
            )
            .unwrap();
        assert_eq!(file.cite_no, 5, "the deleted [2] must not be reused");
        let block = user_block_for(&db, &thread.id);
        assert!(block.contains("[5]"), "{block}");
        assert!(block.contains("添付本文"), "{block}");
        assert!(!block.contains("[2]"), "{block}");
        let sources = db.list_llm_sources(&thread.id).unwrap();
        assert!(!answer_has_phantom_cites("添付です [5]", &sources));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pasted_url_shown_as_six_is_accepted_and_a_stale_three_is_rejected() {
        let (dir, db) = temp_db("cite-url");
        let thread = thread_after_partial_citation(&db);
        let (file, _) = db
            .insert_llm_source(
                &thread.id,
                "attach",
                r"C:\memo.txt",
                "memo.txt",
                "file",
                "添付本文",
                "",
            )
            .unwrap();
        assert_eq!(file.cite_no, 5);
        let (page, created) = db
            .insert_llm_source_full(
                &thread.id,
                "attach",
                "https://example.com/case",
                "裁判例",
                "",
                "ページ本文",
                "",
                "unit",
                "web",
                "",
                "",
                None,
            )
            .unwrap();
        assert!(created);
        assert_eq!(page.cite_no, 6);
        let block = user_block_for(&db, &thread.id);
        assert!(block.contains("[6]"), "{block}");
        assert!(block.contains("https://example.com/case"), "{block}");
        let sources = db.list_llm_sources(&thread.id).unwrap();
        assert!(
            !answer_has_phantom_cites("ページです [6]", &sources),
            "the number the prompt shows must be a real source"
        );
        assert!(
            answer_has_phantom_cites("古い [3]", &sources),
            "a deleted search hit must stay rejected"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn next_search_after_a_pasted_url_continues_the_ledger() {
        let (dir, db) = temp_db("cite-search");
        let thread = thread_after_partial_citation(&db);
        db.insert_llm_source(
            &thread.id,
            "attach",
            r"C:\memo.txt",
            "memo.txt",
            "file",
            "添付本文",
            "",
        )
        .unwrap();
        db.insert_llm_source_full(
            &thread.id,
            "attach",
            "https://example.com/case",
            "裁判例",
            "",
            "ページ本文",
            "",
            "unit",
            "web",
            "",
            "",
            None,
        )
        .unwrap();
        let (hit, _) = db
            .insert_llm_source(&thread.id, "tool", "path-E", "E", "p7", "次の検索", "query")
            .unwrap();
        assert_eq!(hit.cite_no, 7);
        let block = user_block_for(&db, &thread.id);
        assert!(block.contains("[7]"), "{block}");
        assert!(block.contains("次の検索"), "{block}");
        let sources = db.list_llm_sources(&thread.id).unwrap();
        assert!(!answer_has_phantom_cites("続き [7]", &sources));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn footnotes_in_stored_answers_do_not_move_the_citation_counter() {
        let (dir, db) = temp_db("cite-footnote");
        let thread = db.create_llm_thread("調査", true).unwrap();
        db.insert_llm_message(
            &thread.id,
            "assistant",
            "脚注 [180] と [4096] と [2026]。リンクは [1](https://example.com) です。",
        )
        .unwrap();
        db.insert_llm_message(&thread.id, "user", "添付").unwrap();
        let (row, _) = db
            .insert_llm_source(&thread.id, "attach", "path-a", "A", "p", "本文", "")
            .unwrap();
        assert_eq!(row.cite_no, 1);
        assert_eq!(db.llm_cite_counter(&thread.id).unwrap(), 1);
        let block = user_block_for(&db, &thread.id);
        assert!(block.contains("[1]"), "{block}");
        assert!(!block.contains("[180]"), "{block}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn kept_answer_binds_only_sources_that_existed_when_it_was_written() {
        let existed = std::collections::HashSet::from(["a".to_string()]);
        let bound = citations_for_saved_answer(
            &[("a".into(), 1), ("d".into(), 3)],
            "Aです [1]。幻の出典 [3]。",
            Some(&existed),
        );
        assert_eq!(bound, vec![("a".to_string(), 1)]);
    }

    #[test]
    fn phantom_number_in_a_kept_answer_is_not_reused_by_a_later_source() {
        let (dir, db) = temp_db("cite-phantom");
        let thread = db.create_llm_thread("調査", true).unwrap();
        let (first, _) = db
            .insert_llm_source(&thread.id, "tool", "path-a", "A", "p1", "A", "q")
            .unwrap();
        assert_eq!(first.cite_no, 1);
        let sources_then = db.list_llm_sources(&thread.id).unwrap();
        let answer = "Aです [1]。裏が取れない [3]。";
        assert!(answer_has_phantom_cites(answer, &sources_then));
        let mut slot = None;
        remember_phantom_answer(&mut slot, answer, "", &sources_then);
        let kept = slot.expect("the first phantom answer is kept");
        let saved = save_assistant_answer(&db, &thread.id, &kept.content, "", "").unwrap();
        assert_eq!(saved.content, kept.content);
        assert_ne!(saved.kind, crate::db::LLM_MESSAGE_NOTICE);
        let (later, _) = db
            .insert_llm_source(&thread.id, "tool", "path-d", "D", "p2", "D", "q")
            .unwrap();
        assert_ne!(
            later.cite_no, 3,
            "the ledger must not hand [3] to a later source"
        );
        assert_eq!(later.cite_no, 4);
        let bound = citations_for_saved_answer(
            &[(first.id.clone(), first.cite_no), (later.id, later.cite_no)],
            &kept.content,
            Some(&kept.source_ids),
        );
        assert_eq!(bound, vec![(first.id, 1)]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn kept_phantom_answer_advances_the_counter_past_two_later_sources() {
        let (dir, db) = temp_db("cite-phantom-two");
        let thread = db.create_llm_thread("調査", true).unwrap();
        let (first, _) = db
            .insert_llm_source(&thread.id, "tool", "path-a", "A", "p1", "A", "q")
            .unwrap();
        assert_eq!(first.cite_no, 1);
        let sources_then = db.list_llm_sources(&thread.id).unwrap();
        let answer = "根拠は [5] です。巨大な脚注 [9223372036854775807] は進めません。";
        assert!(answer_has_phantom_cites(answer, &sources_then));
        let mut slot = None;
        remember_phantom_answer(&mut slot, answer, "", &sources_then);
        let kept = slot.expect("the first phantom answer is kept");
        let saved = save_assistant_answer(&db, &thread.id, &kept.content, "", "").unwrap();
        assert_eq!(saved.content, kept.content);
        assert_eq!(db.llm_cite_counter(&thread.id).unwrap(), 5);
        let (memo, _) = db
            .insert_llm_source(&thread.id, "attach", "memo.txt", "memo", "p", "本文", "")
            .unwrap();
        assert_ne!(
            memo.cite_no, 5,
            "memo.txt must not reuse the kept phantom [5]"
        );
        assert_eq!(memo.cite_no, 6);
        let (second, _) = db
            .insert_llm_source(&thread.id, "attach", "later.txt", "later", "p2", "続き", "")
            .unwrap();
        assert_ne!(second.cite_no, 5);
        assert_eq!(second.cite_no, 7);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A non-empty phantom retry is the answer that gets saved, so its `[n]` owns the number.
    /// A later notice that mentions `[n]` does not.
    #[test]
    fn phantom_retry_answer_advances_the_counter_and_a_notice_does_not() {
        let (dir, db) = temp_db("cite-phantom-retry");
        let thread = db.create_llm_thread("調査", true).unwrap();
        let (first, _) = db
            .insert_llm_source(&thread.id, "tool", "path-a", "A", "p1", "A", "q")
            .unwrap();
        assert_eq!(first.cite_no, 1);
        let sources_then = db.list_llm_sources(&thread.id).unwrap();
        let original = "根拠は [5] です。";
        assert!(answer_has_phantom_cites(original, &sources_then));
        let mut slot = None;
        remember_phantom_answer(&mut slot, original, "", &sources_then);
        assert!(slot.is_some(), "the first phantom answer is remembered");
        assert!(matches!(
            answer_after_phantom_retry(false, true),
            PhantomRetryAnswer::Retry
        ));
        let retry = "調べ直しても [5] のままです。巨大な脚注 [9223372036854775807] は進めません。";
        let saved = save_assistant_answer(&db, &thread.id, retry, "考え", "").unwrap();
        assert_eq!(saved.role, "assistant");
        assert_eq!(saved.content, retry);
        assert_ne!(saved.kind, crate::db::LLM_MESSAGE_NOTICE);
        assert_eq!(db.llm_cite_counter(&thread.id).unwrap(), 5);
        let (memo, _) = db
            .insert_llm_source(&thread.id, "attach", "memo.txt", "memo", "p", "本文", "")
            .unwrap();
        assert_ne!(
            memo.cite_no, 5,
            "memo.txt must not reuse the retried phantom [5]"
        );
        assert_eq!(memo.cite_no, 6);
        let notice = save_assistant_answer(
            &db,
            &thread.id,
            "案内 [9]",
            "",
            crate::db::LLM_MESSAGE_NOTICE,
        )
        .unwrap();
        assert_eq!(notice.kind, crate::db::LLM_MESSAGE_NOTICE);
        assert_eq!(
            db.llm_cite_counter(&thread.id).unwrap(),
            6,
            "a notice must not advance the ledger"
        );
        let (second, _) = db
            .insert_llm_source(&thread.id, "attach", "later.txt", "later", "p2", "続き", "")
            .unwrap();
        assert_eq!(second.cite_no, 7);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cut_reply_notices_a_cap_a_tool_call_and_a_tools_rejection() {
        let runaway = Err(format!("{}12", llm::thinking::THINKING_RUNAWAY_PREFIX));
        assert_eq!(cut_reply(&runaway), llm::thinking::CutReply::OverCap);
        let rejected = Err(crate::llm::http_error::TOOLS_UNSUPPORTED_ERROR.to_string());
        assert_eq!(cut_reply(&rejected), llm::thinking::CutReply::ToolsRejected);
        assert_eq!(
            cut_reply(&Err("cancelled".into())),
            llm::thinking::CutReply::Other
        );
        let answer = llm::StreamOutcome {
            content: "答え".into(),
            ..llm::StreamOutcome::default()
        };
        assert_eq!(cut_reply(&Ok(answer)), llm::thinking::CutReply::Answer);
        let tools = llm::StreamOutcome {
            content: String::new(),
            tool_calls: vec![llm::ToolCall {
                id: "c".into(),
                name: "search_index".into(),
                arguments: "{}".into(),
            }],
            ..llm::StreamOutcome::default()
        };
        assert_eq!(cut_reply(&Ok(tools)), llm::thinking::CutReply::NeedsTools);
        let finish = llm::StreamOutcome {
            finish_reason: "tool_calls".into(),
            ..llm::StreamOutcome::default()
        };
        assert_eq!(cut_reply(&Ok(finish)), llm::thinking::CutReply::NeedsTools);
    }

    #[test]
    fn thinking_cut_notice_is_stored_as_a_chat_notice() {
        let (dir, db) = temp_db("think-cut");
        let thread = db.create_llm_thread("会話", true).unwrap();
        let row = db
            .insert_llm_notice(&thread.id, llm::thinking::THINKING_CUT_NOTICE)
            .unwrap();
        assert_eq!(row.kind, crate::db::LLM_MESSAGE_NOTICE);
        assert_eq!(row.role, "assistant");
        assert_eq!(
            row.content,
            "考えが長くなったため途中で打ち切り、集めた資料から答えています"
        );
        let history = db.list_llm_messages(&thread.id).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].kind, "notice");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn phantom_tools_after(answer_tools: bool, rounds: usize, max_rounds: usize) -> bool {
        let mut hold = llm::thinking::CutHold::default();
        hold.observe_answer(answer_tools);
        phantom_retry_tools_on(false, rounds, max_rounds, hold.tools_off, false)
    }

    #[test]
    fn tool_less_cut_keeps_the_phantom_tools_hint_while_rounds_remain() {
        let mut cut = llm::thinking::ThinkingCut::start(true, false);
        assert!(matches!(
            cut.observe(llm::thinking::CutReply::OverCap),
            llm::thinking::CutStep::Again(_)
        ));
        let llm::thinking::CutStep::Answer { tools } = cut.observe(llm::thinking::CutReply::Answer)
        else {
            panic!("the tool-less re-request should answer");
        };
        assert!(tools);
        assert!(
            phantom_tools_after(tools, 1, 8),
            "rounds remain, so the phantom retry still has tools"
        );
        assert_eq!(phantom_cite_hint(true), PHANTOM_CITE_HINT);

        let mut stripped = llm::thinking::ThinkingCut::start(true, true);
        stripped.observe(llm::thinking::CutReply::OverCap);
        stripped.observe(llm::thinking::CutReply::ToolsRejected);
        let llm::thinking::CutStep::Answer { tools: kept } =
            stripped.observe(llm::thinking::CutReply::Answer)
        else {
            panic!("the tools-off fallback should answer");
        };
        assert!(!kept);
        assert!(
            !phantom_tools_after(kept, 1, 8),
            "recovery that stripped tools keeps the phantom retry tool-free"
        );
        assert_eq!(phantom_cite_hint(false), PHANTOM_CITE_HINT_NO_TOOLS);
    }

    /// A vLLM-style 400 (`"auto" tool choice requires`) falls back once. The
    /// phantom-cite retry must not send tools again, or the second rejection
    /// discards the answer because `retried_without_tools` is already set.
    #[test]
    fn tools_rejecting_server_keeps_the_phantom_answer_without_resending_tools() {
        let rejected = crate::llm::http_error::chat_failure(
            400,
            r#"{"error":{"message":"\"auto\" tool choice requires --enable-auto-tool-choice"}}"#,
        );
        assert_eq!(rejected, crate::llm::http_error::TOOLS_UNSUPPORTED_ERROR);
        assert!(llm::is_tools_unsupported_error(&rejected));

        let mut use_tools = true;
        let mut retried_without_tools = false;
        assert!(use_tools && !retried_without_tools);
        use_tools = false;
        retried_without_tools = true;

        let answer = "根拠は [9] です。";
        assert!(answer_has_phantom_cites(answer, &[]));

        // Plain call after the fallback hits the cap, then answers with [9].
        let mut cut = llm::thinking::ThinkingCut::start(true, use_tools);
        assert!(matches!(
            cut.observe(llm::thinking::CutReply::OverCap),
            llm::thinking::CutStep::Again(req) if req.thinking_off && !req.tools
        ));
        let llm::thinking::CutStep::Answer { tools } = cut.observe(llm::thinking::CutReply::Answer)
        else {
            panic!("the thinking-off resend should answer");
        };
        assert!(
            tools,
            "a tool-capable citation re-request must still be allowed to restore tools"
        );
        let mut hold = llm::thinking::CutHold::default();
        hold.observe_answer(tools);
        assert!(!hold.tools_off);
        let tools_on = phantom_retry_tools_on(false, 0, 8, hold.tools_off, retried_without_tools);
        assert!(
            !tools_on,
            "do not re-send tools after the server rejected them"
        );
        assert_eq!(phantom_cite_hint(tools_on), PHANTOM_CITE_HINT_NO_TOOLS);
        use_tools = tools_on;
        assert!(
            !(use_tools && !retried_without_tools && llm::is_tools_unsupported_error(&rejected)),
            "a second rejection is not the one-time fallback, so it would drop the answer"
        );
        let mut kept = None;
        remember_phantom_answer(&mut kept, answer, "", &[]);
        assert!(matches!(
            answer_after_phantom_retry(true, kept.is_some()),
            PhantomRetryAnswer::Original
        ));
        let kept = kept.expect("the phantom answer is kept");
        assert_eq!(kept.content, answer);
        assert_eq!(
            hold.notice(false, &kept.content),
            Some(llm::thinking::THINKING_CUT_NOTICE)
        );

        // The same gate without a cut: a short phantom answer stays tool-less
        // and does not store the cut notice.
        let mut plain = llm::thinking::ThinkingCut::start(true, false);
        assert_eq!(
            plain.observe(llm::thinking::CutReply::Answer),
            llm::thinking::CutStep::Pass
        );
        let hold_plain = llm::thinking::CutHold::default();
        let tools_plain = phantom_retry_tools_on(false, 0, 8, hold_plain.tools_off, true);
        assert!(!tools_plain);
        assert_eq!(phantom_cite_hint(tools_plain), PHANTOM_CITE_HINT_NO_TOOLS);
        assert!(hold_plain.notice(false, answer).is_none());
        assert!(matches!(
            answer_after_phantom_retry(true, true),
            PhantomRetryAnswer::Original
        ));
    }

    fn pending_contract(body: &str) -> LlmSourceRow {
        LlmSourceRow {
            id: "f1".into(),
            thread_id: "t".into(),
            sort_order: 0,
            origin: "attach".into(),
            path: r"C:\docs\契約.txt".into(),
            title: String::new(),
            paragraph_id: "f1".into(),
            body: body.into(),
            query: String::new(),
            created_at: 0,
            grain: "unit".into(),
            unit_body: String::new(),
            injected_user_message_id: String::new(),
            cited_assistant_message_id: String::new(),
            cite_no: 0,
            kind: "text".into(),
            stored_relpath: String::new(),
            ocr_status: String::new(),
        }
    }

    fn chat_message(id: &str, role: &str, content: &str) -> LlmMessageRow {
        LlmMessageRow {
            id: id.into(),
            thread_id: "t".into(),
            role: role.into(),
            content: content.into(),
            created_at: 0,
            reasoning: String::new(),
            tool_calls: String::new(),
            tool_call_id: String::new(),
            tool_name: String::new(),
            kind: String::new(),
            carries_summary: false,
        }
    }

    #[test]
    fn file_grain_meter_counts_the_pending_attachment_on_the_meter_basis() {
        let budget = 200_000;
        let expanded =
            sources_with_file_body(vec![pending_contract("段落")], "f1", "あ".repeat(100_000))
                .unwrap();
        assert_eq!(expanded[0].grain, "file");
        let reading = file_grain_reading("役割", &expanded, &[], "", 2_410, budget, "f1");
        assert!(!reading.body_trimmed);
        assert_eq!(
            reading.tokens, 64_983,
            "same count as the chat meter for a pending attachment"
        );
        assert_eq!(
            reading.tokens,
            meter_usage_tokens("役割", &expanded, &[], "", 2_410, budget)
        );

        let mut sibling = pending_contract("別段落");
        sibling.id = "f2".into();
        sibling.path = r"c:\docs\契約.txt".into();
        let mut other = pending_contract("他のファイル");
        other.id = "other".into();
        other.path = r"C:\docs\other.txt".into();
        let mut consumed = pending_contract("既読");
        consumed.id = "kept".into();
        consumed.injected_user_message_id = "u1".into();
        let kept = sources_with_file_body(
            vec![pending_contract("段落"), sibling, other, consumed],
            "f1",
            "本文".into(),
        )
        .unwrap();
        assert_eq!(
            kept.iter()
                .map(|source| source.id.as_str())
                .collect::<Vec<_>>(),
            ["f1", "other", "kept"]
        );
        assert_eq!(kept[0].body, "本文");
        assert_eq!(kept[0].grain, "file");
    }

    #[test]
    fn file_grain_confirm_asks_only_when_the_file_body_is_cut() {
        let budget = 40_000usize;
        let exchange = "あ".repeat(2_000);
        let mut history = Vec::new();
        for i in 0..80 {
            history.push(chat_message(&format!("u{i}"), "user", &exchange));
            history.push(chat_message(&format!("a{i}"), "assistant", &exchange));
        }

        let small =
            sources_with_file_body(vec![pending_contract("段落")], "f1", "あ".repeat(3_000))
                .unwrap();
        let small_reading = file_grain_reading("役割", &small, &history, "", 2_410, budget, "f1");
        assert!(
            !small_reading.body_trimmed,
            "a short file stays whole after old exchanges are dropped"
        );
        assert_eq!(
            small_reading.tokens,
            meter_usage_tokens("役割", &small, &history, "", 2_410, budget)
        );
        let whole_history = meter_usage_tokens("役割", &small, &history, "", 2_410, 8_000_000);
        assert!(
            whole_history > small_reading.tokens,
            "the untrimmed history is larger than the send: {whole_history} vs {}",
            small_reading.tokens
        );

        let large =
            sources_with_file_body(vec![pending_contract("段落")], "f1", "あ".repeat(100_000))
                .unwrap();
        let large_reading = file_grain_reading("役割", &large, &history, "", 2_410, budget, "f1");
        assert!(
            large_reading.body_trimmed,
            "a file that does not fit is cut"
        );
        assert_eq!(
            large_reading.tokens,
            meter_usage_tokens("役割", &large, &history, "", 2_410, budget)
        );
    }

    struct MeterParts {
        tokens: usize,
        system_tokens: usize,
        schema_tokens: usize,
        system: String,
    }

    fn replay_tokens(parts: &MeterParts) -> usize {
        parts.tokens - parts.system_tokens - parts.schema_tokens
    }

    fn delta(later: usize, earlier: usize) -> i64 {
        later as i64 - earlier as i64
    }

    /// What `llm_context_meter` reports for this thread, and the system turn it counted.
    fn meter_parts(
        state: &std::sync::Arc<crate::state::AppState>,
        thread_id: &str,
        draft: &str,
        web_search: bool,
    ) -> MeterParts {
        let (tokens, trimmed) = measure_context_meter(
            std::sync::Arc::clone(state),
            thread_id.to_string(),
            draft.to_string(),
            web_search,
            None,
        )
        .unwrap();
        assert!(!trimmed);
        let settings = state.settings.read().clone();
        let (thread, history, sources) = if thread_id.trim().is_empty() {
            (blank_llm_thread(), Vec::new(), Vec::new())
        } else {
            let thread = state.db.get_llm_thread(thread_id).unwrap().unwrap();
            let history = state.db.list_llm_messages(thread_id).unwrap();
            let sources = state.db.list_llm_sources(thread_id).unwrap();
            (thread, history, sources)
        };
        let history =
            summary::prepare_model_history(&history, &thread.summary, &thread.summary_through_id);
        let plan = plan_chat_request(
            state,
            &settings,
            &thread,
            meter_question(draft),
            web_search,
            &sources,
            None,
        );
        assert_eq!(
            tokens,
            meter_usage_tokens(
                &plan.system,
                &sources,
                &history,
                draft,
                plan.schema_tokens,
                plan.prompt_budget,
            )
        );
        let folded = history_with_meter_draft(&history, draft);
        let (turns, _) = assemble_turns(&plan.system, &sources, &folded, plan.prompt_budget.max(1));
        assert_eq!(tokens, meter_request_tokens(&turns, plan.schema_tokens));
        assert_eq!(turns.iter().filter(|turn| turn.role == "system").count(), 1);
        let system_tokens: usize = turns
            .iter()
            .filter(|turn| turn.role == "system")
            .map(crate::llm::context::turn_tokens)
            .sum();
        let gate = sent_usage_tokens(
            &plan.system,
            &sources,
            &folded,
            plan.schema_tokens,
            plan.prompt_budget,
        );
        assert_eq!(tokens, gate + system_tokens);
        assert!(system_tokens > 0);
        MeterParts {
            tokens,
            system_tokens,
            schema_tokens: plan.schema_tokens,
            system: turns
                .into_iter()
                .find(|turn| turn.role == "system")
                .unwrap()
                .content,
        }
    }

    #[test]
    fn context_meter_counts_the_system_prompt_this_chat_would_send() {
        let dir = std::env::temp_dir().join(format!(
            "argos-meter-system-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let state = test_state(&dir, 9, 8);
        {
            let mut settings = state.settings.write();
            settings.llm_system_prompt = "事務所の独自ルールだけを守ってください。".into();
            settings.searxng_url = "http://127.0.0.1:8888".into();
            settings.search_mode = "local".into();
            settings.mail_days_back = 45;
            settings.llm_max_context_tokens = 131_072;
        }
        let created = state.db.create_llm_thread("調査", true).unwrap();
        state
            .db
            .set_llm_thread_scope(&created.id, r"C:\docs\案件")
            .unwrap();
        let note = state.db.create_note("準備書面").unwrap();
        state
            .db
            .update_note_memo(&note.id, "## 請求原因\n売買代金")
            .unwrap();
        state.db.set_llm_thread_note(&created.id, &note.id).unwrap();
        state
            .db
            .insert_llm_message(&created.id, "user", "前回の質問")
            .unwrap();
        state
            .db
            .insert_llm_source(
                &created.id,
                "attach",
                r"C:\docs\契約.txt",
                "契約.txt",
                "p1",
                "条項本文",
                "",
            )
            .unwrap();

        let draft = "質問です";
        let baseline = meter_parts(&state, &created.id, draft, false);
        let thread = state.db.get_llm_thread(&created.id).unwrap().unwrap();
        let note = state.db.get_note(&note.id).unwrap().unwrap();
        let scope = tools::format_thread_scope_system_line(&thread.path_prefix).unwrap();
        let note_line = tools::format_note_target_system_line(&note.title, &note.memo);
        assert!(baseline
            .system
            .contains("事務所の独自ルールだけを守ってください。"));
        assert!(!baseline.system.contains("言語タグ mermaid"));
        assert!(baseline.system.contains(scope.trim()));
        assert!(baseline.system.contains(note_line.trim()));
        assert!(baseline
            .system
            .contains(tools::format_search_date_system_line(45).trim()));
        assert!(baseline
            .system
            .contains(tools::format_calendar_system_line().trim()));
        assert!(baseline.system.contains("生のHTMLは書かないでください"));
        assert!(baseline
            .system
            .contains("根拠にする条を read_unit で読み直して"));
        assert!(baseline
            .system
            .contains("書いた内容はすぐメモに反映されます"));
        assert!(baseline
            .system
            .contains(crate::statute::LLM_STATUTE_SENTINEL));
        assert!(
            baseline.system.contains("出典ブロックがあるときは"),
            "a pending source adds the citation guide to the counted prompt"
        );
        assert!(!baseline.system.contains("ウェブ検索が有効です"));
        assert!(!baseline.system.contains("別PCの共有フォルダも検索対象です"));

        let diagram = meter_parts(&state, &created.id, "契約を図にして", false);
        assert!(diagram.system.contains(llm::LLM_DIAGRAM_HINT));
        assert!(
            !diagram.system.contains("出典ブロックがあるときは"),
            "the diagram hint already shows [n], so the citation guide is not added again"
        );
        assert!(diagram.system_tokens > baseline.system_tokens);
        assert_eq!(diagram.schema_tokens, baseline.schema_tokens);

        let fresh = meter_parts(&state, "", draft, false);
        assert!(fresh
            .system
            .contains("事務所の独自ルールだけを守ってください。"));
        assert!(!fresh.system.contains("言語タグ mermaid"));
        assert!(!fresh.system.contains("配下に限定"));
        assert!(!fresh.system.contains("出典ブロックがあるときは"));
        assert!(baseline.system_tokens > fresh.system_tokens);

        {
            let mut settings = state.settings.write();
            settings.llm_system_prompt = "短く。".into();
        }
        let shorter = meter_parts(&state, &created.id, draft, false);
        assert!(shorter.system.contains("短く。"));
        assert!(!shorter.system.contains("事務所の独自ルール"));
        assert_eq!(shorter.schema_tokens, baseline.schema_tokens);
        assert_eq!(replay_tokens(&shorter), replay_tokens(&baseline));
        assert!(baseline.tokens > shorter.tokens);
        assert_eq!(
            baseline.tokens - shorter.tokens,
            baseline.system_tokens - shorter.system_tokens
        );

        {
            let mut settings = state.settings.write();
            settings.llm_system_prompt = "事務所の独自ルールだけを守ってください。".into();
        }
        let restored = meter_parts(&state, &created.id, draft, false);
        assert_eq!(restored.tokens, baseline.tokens);

        let web = meter_parts(&state, &created.id, draft, true);
        assert!(web.system.contains("ウェブ検索が有効です"));
        assert_ne!(web.schema_tokens, restored.schema_tokens);
        assert_eq!(replay_tokens(&web), replay_tokens(&restored));
        assert_eq!(
            delta(web.tokens, restored.tokens),
            delta(web.system_tokens, restored.system_tokens)
                + delta(web.schema_tokens, restored.schema_tokens)
        );

        {
            let mut settings = state.settings.write();
            settings.search_mode = "hybrid".into();
        }
        let hybrid = meter_parts(&state, &created.id, draft, true);
        assert!(hybrid.system.contains("別PCの共有フォルダも検索対象です"));
        assert_eq!(replay_tokens(&hybrid), replay_tokens(&web));
        assert_eq!(
            delta(hybrid.tokens, web.tokens),
            delta(hybrid.system_tokens, web.system_tokens)
                + delta(hybrid.schema_tokens, web.schema_tokens)
        );
        assert!(hybrid.tokens > web.tokens);

        state.db.set_llm_thread_scope(&created.id, "").unwrap();
        let unscoped = meter_parts(&state, &created.id, draft, true);
        assert!(!unscoped.system.contains("配下に限定"));
        assert_eq!(unscoped.schema_tokens, hybrid.schema_tokens);
        assert_eq!(replay_tokens(&unscoped), replay_tokens(&hybrid));
        assert_eq!(
            delta(hybrid.tokens, unscoped.tokens),
            delta(hybrid.system_tokens, unscoped.system_tokens)
        );

        let plain = meter_parts(&state, &created.id, "見て", true);
        assert!(!plain.system.contains("言語タグ mermaid"));
        assert!(plain.system.contains("ウェブ検索が有効です"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn cut_then_answer(tools: bool, body: &str) -> (llm::thinking::CutHold, Option<RecoveredCut>) {
        let mut turn = llm::thinking::ThinkingCut::start(true, tools);
        assert!(matches!(
            turn.observe(llm::thinking::CutReply::OverCap),
            llm::thinking::CutStep::Again(_)
        ));
        let llm::thinking::CutStep::Answer { tools: still } =
            turn.observe(llm::thinking::CutReply::Answer)
        else {
            panic!("the thinking-off resend should answer");
        };
        let mut hold = llm::thinking::CutHold::default();
        hold.observe_answer(still);
        let mut recovered = None;
        remember_recovered_cut(
            &mut recovered,
            hold.thinking_off,
            body,
            "",
            false,
            std::collections::HashSet::new(),
        );
        (hold, recovered)
    }

    #[test]
    fn phantom_retry_http_500_keeps_the_cut_answer() {
        let body = "根拠は [9] です。";
        let (hold, recovered) = cut_then_answer(true, body);
        let err = crate::llm::http_error::chat_failure(500, "internal");
        assert!(!llm::is_cancelled_error(&err));
        let kept = recovered_cut_to_keep(&recovered).expect("the cut answer is saved");
        assert_eq!(kept.content, body);
        assert_eq!(
            hold.notice(false, &kept.content),
            Some(llm::thinking::THINKING_CUT_NOTICE)
        );
        let mut phantom = None;
        remember_phantom_answer(&mut phantom, body, "", &[]);
        assert!(phantom.is_some());
        assert!(matches!(
            answer_after_phantom_retry(true, true),
            PhantomRetryAnswer::Original
        ));
    }

    #[test]
    fn second_cap_on_the_phantom_retry_keeps_the_cut_answer() {
        let body = "根拠は [9] です。";
        let (hold, recovered) = cut_then_answer(false, body);
        assert!(hold.thinking_off);
        let tools_on = phantom_retry_tools_on(false, 0, 8, hold.tools_off, true);
        assert!(!tools_on, "a tools-rejecting server does not resend tools");
        let mut retry = llm::thinking::ThinkingCut::held(tools_on);
        assert_eq!(
            retry.observe(llm::thinking::CutReply::OverCap),
            llm::thinking::CutStep::Stop { over_cap: true }
        );
        let err = llm::thinking::finish_runaway("thinking-runaway:32768");
        assert!(!llm::is_cancelled_error(&err));
        let kept =
            recovered_cut_to_keep(&recovered).expect("the cut answer survives the second cap");
        assert_eq!(kept.content, body);
        assert_eq!(
            hold.notice(false, &kept.content),
            Some(llm::thinking::THINKING_CUT_NOTICE)
        );

        let mut first = llm::thinking::ThinkingCut::start(true, true);
        assert!(matches!(
            first.observe(llm::thinking::CutReply::OverCap),
            llm::thinking::CutStep::Again(_)
        ));
        assert_eq!(
            first.observe(llm::thinking::CutReply::OverCap),
            llm::thinking::CutStep::Stop { over_cap: true }
        );
        let mut none = None;
        remember_recovered_cut(
            &mut none,
            false,
            body,
            "",
            false,
            std::collections::HashSet::new(),
        );
        assert!(
            recovered_cut_to_keep(&none).is_none(),
            "the first thinking-off resend still fails when it hits the cap"
        );
    }

    #[test]
    fn citation_rerequest_cap_keeps_the_uncited_cut_body() {
        let body = "三か月以内です。";
        let (hold, recovered) = cut_then_answer(true, body);
        let mut phantom = None;
        remember_phantom_answer(&mut phantom, body, "", &[]);
        assert!(
            phantom.is_none(),
            "this body has no [n], so the phantom slot does not keep it"
        );
        let mut retry = llm::thinking::ThinkingCut::held(false);
        assert_eq!(
            retry.observe(llm::thinking::CutReply::OverCap),
            llm::thinking::CutStep::Stop { over_cap: true }
        );
        let kept = recovered_cut_to_keep(&recovered).expect("the uncited body is saved");
        assert_eq!(kept.content, body);
        assert_eq!(
            hold.notice(false, &kept.content),
            Some(llm::thinking::THINKING_CUT_NOTICE)
        );
    }

    #[test]
    fn tool_call_preamble_does_not_overwrite_the_recovered_answer() {
        let mut slot = None;
        remember_recovered_cut(
            &mut slot,
            true,
            "根拠は [9] です。",
            "",
            false,
            std::collections::HashSet::new(),
        );
        remember_recovered_cut(
            &mut slot,
            true,
            "確認します。",
            "",
            true,
            std::collections::HashSet::new(),
        );
        assert_eq!(
            recovered_cut_to_keep(&slot).unwrap().content,
            "根拠は [9] です。"
        );
    }

    fn sse_message(payload: serde_json::Value) -> Vec<u8> {
        let data = format!("data: {payload}\n\ndata: [DONE]\n\n");
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n{data}"
        )
        .into_bytes()
    }

    fn http_status(code: u16, reason: &str, body: &str) -> Vec<u8> {
        format!(
            "HTTP/1.1 {code} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .into_bytes()
    }

    fn serve_script(script: Vec<Vec<u8>>) -> u16 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for response in script {
                let Ok((mut sock, _)) = listener.accept() else {
                    break;
                };
                let _ = sock.set_read_timeout(Some(std::time::Duration::from_secs(20)));
                let mut buf = Vec::new();
                let mut tmp = [0u8; 4096];
                loop {
                    match std::io::Read::read(&mut sock, &mut tmp) {
                        Ok(0) => break,
                        Ok(n) => {
                            buf.extend_from_slice(&tmp[..n]);
                            if let Some(header_end) = buf.windows(4).position(|w| w == b"\r\n\r\n")
                            {
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
                            if buf.len() > 4_000_000 {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
                let _ = std::io::Write::write_all(&mut sock, &response);
                let _ = std::io::Write::flush(&mut sock);
            }
        });
        port
    }

    fn reasoning_over_cap() -> Vec<u8> {
        let payload = serde_json::json!({
            "choices": [{ "delta": { "reasoning_content": "あ".repeat(100_000) } }]
        });
        sse_message(payload)
    }

    fn content_stop(text: &str) -> Vec<u8> {
        sse_message(serde_json::json!({
            "choices": [{ "delta": { "content": text }, "finish_reason": "stop" }]
        }))
    }

    fn content_with_reasoning(content: &str, reasoning: &str) -> Vec<u8> {
        sse_message(serde_json::json!({
            "choices": [{
                "delta": { "content": content, "reasoning_content": reasoning },
                "finish_reason": "stop"
            }]
        }))
    }

    fn reasoning_only_tool_call(body: &str) -> Vec<u8> {
        content_with_reasoning(
            body,
            r#"<tool_call>{"name":"search_index","arguments":{"query":"OrionContract"}}</tool_call>"#,
        )
    }

    fn content_and_search(text: &str, query: &str) -> Vec<u8> {
        sse_message(serde_json::json!({
            "choices": [{
                "delta": {
                    "content": text,
                    "tool_calls": [{
                        "index": 0,
                        "id": "c1",
                        "function": {
                            "name": "search_index",
                            "arguments": serde_json::json!({ "query": query }).to_string()
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        }))
    }

    fn test_state(
        dir: &std::path::Path,
        port: u16,
        max_rounds: u32,
    ) -> std::sync::Arc<crate::state::AppState> {
        let db = std::sync::Arc::new(crate::db::Db::open(&dir.join("argos.db")).unwrap());
        let mut settings = crate::db::Settings::default();
        settings.llm_base_url = format!("http://127.0.0.1:{port}/v1");
        settings.llm_model = "test-model".into();
        settings.llm_timeout_ms = 20_000;
        settings.llm_thinking = "on".into();
        settings.llm_max_tool_rounds = max_rounds;
        settings.search_mode = "local".into();
        let backend = std::sync::Arc::new(
            crate::search::tantivy_backend::TantivyBackend::open(&dir.join("index"))
                .unwrap()
                .backend,
        );
        let mail_backend = std::sync::Arc::new(
            crate::search::tantivy_backend::TantivyBackend::open_mail(&dir.join("index-mail"))
                .unwrap()
                .backend,
        );
        let indexer =
            std::sync::Arc::new(crate::indexer::Indexer::new(db.clone(), backend.clone()));
        let mail = crate::mail::MailStaHandle::start(db.clone(), mail_backend.clone()).unwrap();
        let statute = std::sync::Arc::new(
            crate::statute::StatuteService::open_offline(dir).expect("statute db"),
        );
        std::sync::Arc::new(crate::state::AppState {
            db,
            settings: std::sync::Arc::new(parking_lot::RwLock::new(settings)),
            data_dir: dir.to_path_buf(),
            backend,
            mail_backend,
            indexer,
            remote_server: crate::remote_server::RemoteServerHandle::new(),
            watcher: parking_lot::RwLock::new(None),
            user_dict: std::sync::Arc::new(parking_lot::RwLock::new(
                crate::search::UserDictMatcher::from_words(std::iter::empty::<String>()),
            )),
            mail,
            statute,
            calendar_sync: parking_lot::Mutex::new(()),
            llm_job: parking_lot::RwLock::new(None),
            preview_target: parking_lot::RwLock::new(None),
        })
    }

    fn final_answer(messages: &[crate::db::LlmMessageRow]) -> &crate::db::LlmMessageRow {
        messages
            .iter()
            .rev()
            .find(|message| {
                message.role == "assistant"
                    && message.tool_calls.trim().is_empty()
                    && message.kind != crate::db::LLM_MESSAGE_NOTICE
            })
            .expect("a final assistant answer")
    }

    async fn send_script(
        label: &str,
        max_rounds: u32,
        script: Vec<Vec<u8>>,
        index: bool,
    ) -> (
        std::path::PathBuf,
        crate::db::Db,
        crate::db::LlmThreadRow,
        LlmSendResult,
    ) {
        let dir = std::env::temp_dir().join(format!(
            "argos-send-{label}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let port = serve_script(script);
        let state = test_state(&dir, port, max_rounds);
        if index {
            let path = dir.join("contract.txt");
            std::fs::write(&path, "OrionContract の期間は三か月です。").unwrap();
            state
                .backend
                .index_file(
                    &path,
                    &path.to_string_lossy(),
                    "",
                    1,
                    20,
                    &crate::extractor::ExtractedDoc {
                        title: "contract.txt".into(),
                        pages: vec!["OrionContract の期間は三か月です。".into()],
                    },
                )
                .unwrap();
        }
        let app = tauri::test::mock_app();
        let result = llm_send_inner(app.handle(), &state, None, "調べて".into(), Some(false))
            .await
            .expect("llm_send");
        let db = crate::db::Db::open(&dir.join("argos.db")).unwrap();
        (dir, db, result.thread.clone(), result)
    }

    fn content_and_law() -> Vec<u8> {
        sse_message(serde_json::json!({
            "choices": [{
                "delta": {
                    "content": "",
                    "tool_calls": [{
                        "index": 0,
                        "id": "law1",
                        "function": {
                            "name": "law_article",
                            "arguments": serde_json::json!({ "law": "民法", "article": "1" }).to_string()
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        }))
    }

    #[tokio::test]
    async fn law_article_only_rounds_stop_after_the_call_cap() {
        let mut script = Vec::new();
        for _ in 0..16 {
            script.push(content_and_law());
        }
        script.push(content_stop("確認できた範囲で答えます。"));
        let (dir, db, thread, result) = send_script("law-cap", 8, script, false).await;
        assert!(result.error.is_none(), "{:?}", result.error);
        let messages = db.list_llm_messages(&thread.id).unwrap();
        let law_rows = messages
            .iter()
            .filter(|message| message.tool_name == "law_article")
            .count();
        assert!(
            (10..=11).contains(&law_rows),
            "law_article rounds did not stop at the cap: {law_rows}"
        );
        drop(db);
        let _ = std::fs::remove_dir_all(dir);
    }

    fn seven_searches() -> Vec<Vec<u8>> {
        (0..7)
            .map(|_| content_and_search("", "OrionContract"))
            .collect()
    }

    fn assert_kept_round_limit_answer(
        messages: &[crate::db::LlmMessageRow],
        body: &str,
        cut_notice: bool,
    ) {
        let answer = messages
            .iter()
            .find(|message| message.content == body)
            .expect("the recovered answer is saved");
        assert_eq!(
            answer.kind, "",
            "a kept answer is a normal turn, not a notice"
        );
        assert!(messages.iter().any(|message| {
            message.kind == crate::db::LLM_MESSAGE_NOTICE
                && message.content.contains("上限の 8 回")
                && message.content.contains("ここまでの資料で答えています")
        }));
        assert_eq!(
            messages
                .iter()
                .any(|message| message.content == llm::thinking::THINKING_CUT_NOTICE),
            cut_notice
        );
        assert!(
            !messages
                .iter()
                .any(|message| message.content.contains("続きの回答は失敗しました")),
            "the round-limit failure text must not replace the kept answer"
        );
    }

    #[tokio::test]
    async fn llm_send_keeps_the_cut_answer_when_the_round_limit_final_fails() {
        let mut script = seven_searches();
        script.push(reasoning_over_cap());
        script.push(content_stop("根拠は [9] です。"));
        script.push(content_and_search("調べます。", "OrionContract"));
        script.push(http_status(
            500,
            "Internal Server Error",
            r#"{"error":"failed"}"#,
        ));
        let (dir, db, thread, result) = send_script("round-fail", 8, script, false).await;
        assert!(result.error.is_none(), "{:?}", result.error);
        let messages = db.list_llm_messages(&thread.id).unwrap();
        assert_kept_round_limit_answer(&messages, "根拠は [9] です。", true);
        assert_eq!(
            messages
                .iter()
                .filter(|message| message.role == "assistant" && message.content == "調べます。")
                .count(),
            1,
            "the preamble stays on the tool-call row only"
        );
        drop(db);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn llm_send_keeps_a_phantom_answer_when_the_round_limit_final_fails_without_a_cut() {
        let mut script = seven_searches();
        script.push(content_stop("根拠は [9] です。"));
        script.push(content_and_search("調べます。", "OrionContract"));
        script.push(http_status(
            500,
            "Internal Server Error",
            r#"{"error":"failed"}"#,
        ));
        let (dir, db, thread, result) = send_script("round-plain", 8, script, false).await;
        assert!(result.error.is_none(), "{:?}", result.error);
        let messages = db.list_llm_messages(&thread.id).unwrap();
        assert_kept_round_limit_answer(&messages, "根拠は [9] です。", false);
        drop(db);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn llm_send_keeps_the_cut_answer_when_the_round_limit_final_is_empty() {
        let mut script = seven_searches();
        script.push(reasoning_over_cap());
        script.push(content_stop("根拠は [9] です。"));
        script.push(content_and_search("調べます。", "OrionContract"));
        script.push(content_stop(""));
        let (dir, db, thread, result) = send_script("round-empty", 8, script, false).await;
        assert!(result.error.is_none(), "{:?}", result.error);
        let messages = db.list_llm_messages(&thread.id).unwrap();
        assert_kept_round_limit_answer(&messages, "根拠は [9] です。", true);
        assert!(
            !messages
                .iter()
                .any(|message| message.content.contains("続きはもう一度指示するか")),
            "an empty final must not replace the kept answer with the long limit notice"
        );
        drop(db);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn llm_send_keeps_the_cut_answer_when_the_round_limit_final_embeds_a_tool_call() {
        let mut script = seven_searches();
        script.push(reasoning_over_cap());
        script.push(content_stop("根拠は [9] です。"));
        script.push(content_and_search("調べます。", "OrionContract"));
        script.push(content_stop(
            "まだ調べます。<tool_call>{\"name\":\"search_index\",\"arguments\":{\"query\":\"OrionContract\"}}</tool_call>",
        ));
        let (dir, db, thread, result) = send_script("round-embed", 8, script, false).await;
        assert!(result.error.is_none(), "{:?}", result.error);
        let messages = db.list_llm_messages(&thread.id).unwrap();
        assert_kept_round_limit_answer(&messages, "根拠は [9] です。", true);
        assert!(
            !messages
                .iter()
                .any(|message| message.content.contains("まだ調べます。")),
            "a stripped tool call on the final no-tools call must not replace the answer"
        );
        drop(db);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn llm_send_ignores_an_embedded_tool_call_when_tools_were_not_sent() {
        let rejected =
            r#"{"error":{"message":"\"auto\" tool choice requires --enable-auto-tool-choice"}}"#;
        let embedded = "まだ調べます。<tool_call>{\"name\":\"search_index\",\"arguments\":{\"query\":\"OrionContract\"}}</tool_call>";
        let (dir, db, thread, result) = send_script(
            "embedded",
            8,
            vec![
                http_status(400, "Bad Request", rejected),
                http_status(400, "Bad Request", rejected),
                reasoning_over_cap(),
                content_stop("根拠は [9] です。"),
                content_stop(embedded),
            ],
            false,
        )
        .await;
        assert!(result.error.is_none(), "{:?}", result.error);
        let messages = db.list_llm_messages(&thread.id).unwrap();
        let answer = messages
            .iter()
            .find(|message| message.content == "根拠は [9] です。")
            .expect("the cut answer is kept");
        assert_eq!(answer.kind, "");
        assert!(messages.iter().any(|message| {
            message.kind == crate::db::LLM_MESSAGE_NOTICE
                && message.content == llm::thinking::THINKING_CUT_NOTICE
        }));
        assert!(
            !messages
                .iter()
                .any(|message| message.content.contains("まだ調べます。")),
            "a stripped tool call must not become the answer"
        );
        drop(db);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn llm_send_keeps_the_body_when_the_round_limit_final_calls_a_tool_only_in_reasoning() {
        let mut script = seven_searches();
        script.push(content_and_search("", "OrionContract"));
        script.push(reasoning_only_tool_call("結論は三か月です。"));
        let (dir, db, thread, result) = send_script("round-reason", 8, script, false).await;
        assert!(result.error.is_none(), "{:?}", result.error);
        let messages = db.list_llm_messages(&thread.id).unwrap();
        let answer = messages
            .iter()
            .find(|message| message.content == "結論は三か月です。")
            .expect("the body is the answer");
        assert_eq!(answer.kind, "");
        assert!(messages.iter().any(|message| {
            message.kind == crate::db::LLM_MESSAGE_NOTICE
                && message.content.contains("上限の 8 回")
                && message.content.contains("ここまでの資料で答えています")
        }));
        assert!(!messages.iter().any(|message| {
            message.content.contains("続きはもう一度指示するか")
                || message.content.contains("本文も操作も返さず")
        }));
        drop(db);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn llm_send_keeps_the_body_when_a_tool_refused_phantom_retry_calls_a_tool_only_in_reasoning(
    ) {
        let rejected =
            r#"{"error":{"message":"\"auto\" tool choice requires --enable-auto-tool-choice"}}"#;
        let (dir, db, thread, result) = send_script(
            "phantom-reason",
            8,
            vec![
                http_status(400, "Bad Request", rejected),
                http_status(400, "Bad Request", rejected),
                content_stop("根拠は [9] です。"),
                reasoning_only_tool_call("結論は三か月です。"),
            ],
            false,
        )
        .await;
        assert!(result.error.is_none(), "{:?}", result.error);
        let messages = db.list_llm_messages(&thread.id).unwrap();
        let answer = messages
            .iter()
            .find(|message| message.content == "結論は三か月です。")
            .expect("the phantom retry body is the answer");
        assert_eq!(answer.kind, "");
        assert!(!messages
            .iter()
            .any(|message| message.content == "根拠は [9] です。"));
        assert!(!messages
            .iter()
            .any(|message| message.content.contains("本文も操作も返さず")));
        drop(db);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn llm_send_keeps_the_earlier_answer_when_stage2_embeds_a_tool_call() {
        let embedded = "まだ調べます。[1]<tool_call>{\"name\":\"search_index\",\"arguments\":{\"query\":\"OrionContract\"}}</tool_call>";
        let (dir, db, thread, result) = send_script(
            "stage2-kept",
            8,
            vec![
                content_stop("根拠は [9] です。"),
                reasoning_over_cap(),
                content_and_search("確認します。", "OrionContract"),
                content_stop(embedded),
            ],
            false,
        )
        .await;
        assert!(result.error.is_none(), "{:?}", result.error);
        let messages = db.list_llm_messages(&thread.id).unwrap();
        let answer = messages
            .iter()
            .find(|message| message.content == "根拠は [9] です。")
            .expect("the earlier answer is kept");
        assert_eq!(answer.kind, "");
        assert!(messages.iter().any(|message| {
            message.kind == crate::db::LLM_MESSAGE_NOTICE
                && message.content == llm::thinking::THINKING_CUT_NOTICE
        }));
        assert!(!messages
            .iter()
            .any(|message| message.content.contains("まだ調べます。")));
        drop(db);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn llm_send_does_not_save_a_stage2_tool_call_preamble_when_nothing_is_kept() {
        let embedded = "まだ調べます。[1]<tool_call>{\"name\":\"search_index\",\"arguments\":{\"query\":\"OrionContract\"}}</tool_call>";
        let (dir, db, thread, result) = send_script(
            "stage2-empty",
            8,
            vec![
                reasoning_over_cap(),
                content_and_search("確認します。", "OrionContract"),
                content_stop(embedded),
            ],
            false,
        )
        .await;
        assert!(result.error.is_none(), "{:?}", result.error);
        let messages = db.list_llm_messages(&thread.id).unwrap();
        assert!(messages.iter().any(|message| {
            message.kind == crate::db::LLM_MESSAGE_NOTICE
                && message.content.contains("本文も操作も返さず")
        }));
        assert!(!messages
            .iter()
            .any(|message| message.content.contains("まだ調べます。")));
        assert!(!messages
            .iter()
            .any(|message| message.content == llm::thinking::THINKING_CUT_NOTICE));
        drop(db);
        let _ = std::fs::remove_dir_all(dir);
    }

    fn stage2_reasoning_body_then(last: Vec<u8>) -> Vec<Vec<u8>> {
        vec![
            content_and_search("", "OrionContract"),
            reasoning_over_cap(),
            content_and_search("確認します。", "OrionContract"),
            reasoning_only_tool_call("結論は三か月です。"),
            last,
        ]
    }

    fn assert_stage2_reasoning_body_kept(messages: &[crate::db::LlmMessageRow]) {
        let answer = messages
            .iter()
            .find(|message| message.content == "結論は三か月です。")
            .expect("the stage-2 body is kept");
        assert_eq!(answer.kind, "");
        assert!(messages.iter().any(|message| {
            message.kind == crate::db::LLM_MESSAGE_NOTICE
                && message.content == llm::thinking::THINKING_CUT_NOTICE
        }));
        assert!(messages.iter().any(|message| {
            message.role == "tool" && message.content.contains("OrionContract")
        }));
        assert!(!messages.iter().any(|message| {
            message.content.contains("本文も操作も返さず")
                || message.content.contains("まだ調べます。")
                || message.content.contains("続きの回答は失敗しました")
        }));
    }

    #[tokio::test]
    async fn llm_send_keeps_a_stage2_reasoning_body_when_the_source_retry_fails() {
        let (dir, db, thread, result) = send_script(
            "stage2-reason-500",
            8,
            stage2_reasoning_body_then(http_status(
                500,
                "Internal Server Error",
                r#"{"error":"failed"}"#,
            )),
            true,
        )
        .await;
        assert!(result.error.is_none(), "{:?}", result.error);
        let messages = db.list_llm_messages(&thread.id).unwrap();
        assert_stage2_reasoning_body_kept(&messages);
        drop(db);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn llm_send_keeps_a_stage2_reasoning_body_when_the_source_retry_is_empty() {
        let (dir, db, thread, result) = send_script(
            "stage2-reason-empty",
            8,
            stage2_reasoning_body_then(content_stop("")),
            true,
        )
        .await;
        assert!(result.error.is_none(), "{:?}", result.error);
        let messages = db.list_llm_messages(&thread.id).unwrap();
        assert_stage2_reasoning_body_kept(&messages);
        drop(db);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn llm_send_keeps_a_stage2_reasoning_body_when_the_source_retry_embeds_a_tool_call() {
        let (dir, db, thread, result) = send_script(
            "stage2-reason-embed",
            8,
            stage2_reasoning_body_then(content_stop(
                "まだ調べます。<tool_call>{\"name\":\"search_index\",\"arguments\":{\"query\":\"OrionContract\"}}</tool_call>",
            )),
            true,
        )
        .await;
        assert!(result.error.is_none(), "{:?}", result.error);
        let messages = db.list_llm_messages(&thread.id).unwrap();
        assert_stage2_reasoning_body_kept(&messages);
        drop(db);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn llm_send_keeps_the_cut_answer_when_the_round_limit_final_has_structured_tool_calls() {
        let mut script = seven_searches();
        script.push(reasoning_over_cap());
        script.push(content_stop("根拠は [9] です。"));
        script.push(content_and_search("", "OrionContract"));
        script.push(content_and_search("まだ調べます。", "OrionContract"));
        let (dir, db, thread, result) = send_script("round-structured", 8, script, false).await;
        assert!(result.error.is_none(), "{:?}", result.error);
        let messages = db.list_llm_messages(&thread.id).unwrap();
        assert_kept_round_limit_answer(&messages, "根拠は [9] です。", true);
        assert!(!messages
            .iter()
            .any(|message| message.content.contains("まだ調べます。")));
        drop(db);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn llm_send_keeps_the_earlier_answer_when_stage2_returns_structured_tool_calls() {
        let (dir, db, thread, result) = send_script(
            "stage2-structured-kept",
            8,
            vec![
                content_stop("根拠は [9] です。"),
                reasoning_over_cap(),
                content_and_search("確認します。", "OrionContract"),
                content_and_search("まだ調べます。", "OrionContract"),
            ],
            false,
        )
        .await;
        assert!(result.error.is_none(), "{:?}", result.error);
        let messages = db.list_llm_messages(&thread.id).unwrap();
        let answer = messages
            .iter()
            .find(|message| message.content == "根拠は [9] です。")
            .expect("the earlier answer is kept");
        assert_eq!(answer.kind, "");
        assert!(messages.iter().any(|message| {
            message.kind == crate::db::LLM_MESSAGE_NOTICE
                && message.content == llm::thinking::THINKING_CUT_NOTICE
        }));
        assert!(!messages
            .iter()
            .any(|message| message.content.contains("まだ調べます。")));
        drop(db);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn llm_send_shows_the_empty_notice_when_stage2_structured_tool_calls_have_nothing_kept() {
        let (dir, db, thread, result) = send_script(
            "stage2-structured-empty",
            8,
            vec![
                reasoning_over_cap(),
                content_and_search("確認します。", "OrionContract"),
                content_and_search("まだ調べます。", "OrionContract"),
            ],
            false,
        )
        .await;
        assert!(result.error.is_none(), "{:?}", result.error);
        let messages = db.list_llm_messages(&thread.id).unwrap();
        assert!(messages.iter().any(|message| {
            message.kind == crate::db::LLM_MESSAGE_NOTICE
                && message.content.contains("本文も操作も返さず")
        }));
        assert!(!messages
            .iter()
            .any(|message| message.content.contains("まだ調べます。")));
        assert!(!messages
            .iter()
            .any(|message| message.content == llm::thinking::THINKING_CUT_NOTICE));
        drop(db);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn llm_send_does_not_bind_a_source_fetched_after_the_cut_answer() {
        let (dir, db, thread, result) = send_script(
            "bind",
            8,
            vec![
                reasoning_over_cap(),
                content_stop("根拠は [1] です。"),
                content_and_search("確認します。", "OrionContract"),
                http_status(500, "Internal Server Error", r#"{"error":"failed"}"#),
            ],
            true,
        )
        .await;
        assert!(result.error.is_none(), "{:?}", result.error);
        let messages = db.list_llm_messages(&thread.id).unwrap();
        let answer = final_answer(&messages);
        assert_eq!(answer.content, "根拠は [1] です。");
        let tool = messages
            .iter()
            .find(|message| message.role == "tool")
            .expect("search_index ran");
        assert!(
            tool.content.contains("OrionContract"),
            "the retry must have registered a hit: {}",
            tool.content
        );
        let sources = db.list_llm_sources(&thread.id).unwrap();
        assert!(
            sources
                .iter()
                .all(|source| source.cited_assistant_message_id != answer.id),
            "a source fetched during the retry must not bind to the kept answer: {sources:?}"
        );
        drop(db);
        let _ = std::fs::remove_dir_all(dir);
    }
}
