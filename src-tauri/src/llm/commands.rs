use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};

use crate::db::{LlmMessageRow, LlmSourceRow, LlmThreadRow, NoteReview};
use crate::llm::context::{
    answer_has_phantom_cites, assemble_turns, cap_chars, consumed_cited_in_answer, context_budget,
    estimate_tokens, final_source_turn, fit_injected_turn, fit_tool_text, is_synthetic_user_turn,
    parse_cited_nos, silent_end_notice, sources_for_consumed, tool_schema_tokens, ChatTurn,
    PHANTOM_CITE_HINT, PHANTOM_CITE_HINT_NO_TOOLS, REASONING_CARRY_CAP, STOP_TOOLS_HINT,
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
            wrote_note_id: None,
        },
    }
}

fn emit_chat_reset(app: &AppHandle, request_id: &str, thread_id: &str) {
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
    // A cite already in this request, truncated or full, is not copied again.
    let fresh: Vec<LlmSourceRow> = rows
        .into_iter()
        .filter(|row| !source_already_in_request(turns, row))
        .collect();
    if let Some(turn) = final_source_turn(&fresh, stop_tools) {
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

/// A cite already in this request, including a truncated tool entry, is not sent again.
fn source_already_in_request(turns: &[ChatTurn], source: &LlmSourceRow) -> bool {
    if source.cite_no > 0
        && turns.iter().any(|turn| {
            crate::llm::context::citation_header_nos(&turn.content).contains(&source.cite_no)
        })
    {
        return true;
    }
    let id = source.id.trim();
    if id.is_empty() {
        return false;
    }
    turns.iter().any(|turn| {
        turn.role == "user"
            && !is_synthetic_user_turn(&turn.content)
            && turn.content.contains(id)
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
    let max_tokens = settings.llm_max_context_tokens as usize;
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
    if let Some(line) = paste.system_line() {
        system.push_str(&line);
    }
    let remote_only_scope = tools::is_remote_only_thread(&thread.path_prefix);
    let tools_schema = tools::tools_schema(web_search, remote_index, remote_only_scope);
    let schema_tokens = tool_schema_tokens(&tools_schema);
    let full_limit = max_tokens;
    let required = required_prompt_tokens(&system, &content, &sources);
    let Some(split) = context_budget(full_limit, schema_tokens, required) else {
        discard_refused_user_message(&state.db, &user_message.id);
        return Ok(send_error(
            thread,
            user_message,
            &settings.llm_base_url,
            crate::llm::http_error::QUESTION_DOES_NOT_FIT_ERROR,
        ));
    };
    // Reserves shrink when this question needs the room. The schema stays counted.
    let prompt_budget = split.messages;
    // Messages plus the tool allowance leave the reply reserve untouched.
    let inject_budget = split.messages.saturating_add(split.tool_allowance).max(1);
    let (assembled, stats) = assemble_turns(&system, &sources, &history, prompt_budget);
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

    let mut use_tools = true;
    let mut retried_without_tools = false;
    let mut phantom_retried = false;
    let mut rounds = 0usize;
    let max_rounds = tools::max_tool_rounds(settings.llm_max_tool_rounds);
    let unlimited_rounds = max_rounds == 0;
    let mut current_turns = assembled;
    let mut source_turn_injected = false;
    // In-turn tool results spend the allowance left after the question and the schema.
    let tool_budget = split.tool_allowance;
    let mut tool_used = 0usize;
    let mut read_note_called = false;
    let mut wrote_note_ids: Vec<String> = Vec::new();
    let mut final_reasoning = String::new();
    let mut round_limit_final = false;
    let mut round_limit_aside: Option<String> = None;
    let mut empty_reply = false;
    // First answer kept when a phantom retry (hint or source-block) comes back empty.
    let mut phantom_fallback: Option<PhantomOriginal> = None;
    let mut kept_source_ids: Option<std::collections::HashSet<String>> = None;
    let result_text;

    loop {
        if use_tools && !unlimited_rounds && rounds >= max_rounds {
            // One answer from what this turn already collected. No further tool calls.
            use_tools = false;
            round_limit_final = true;
            if !source_turn_injected {
                let all = state.db.list_llm_sources(&thread.id).unwrap_or_default();
                source_turn_injected =
                    inject_final_source_turn(&mut current_turns, &all, &consumed, true, inject_budget);
            }
            if !source_turn_injected {
                current_turns.push(ChatTurn::text("user", STOP_TOOLS_HINT));
                source_turn_injected = true;
            }
            emit_chat_reset(&app, &request_id, &thread.id);
            continue;
        }
        let with_tools = use_tools;
        let turns_now = current_turns.clone();
        let mut bridge_retried = false;
        let outcome = loop {
            let app2 = app.clone();
            let req = request_id.clone();
            let tid = thread.id.clone();
            let streamed = llm::stream_chat(
                &settings,
                &turns_now,
                cancel.clone(),
                if with_tools {
                    Some(&tools_schema)
                } else {
                    None
                },
                llm::runaway_chars_for(&settings.llm_base_url),
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
            match streamed {
                Err(e) if e == llm::thinking::THINKING_BRIDGE_ERROR && !bridge_retried => {
                    bridge_retried = true;
                    emit_chat_reset(&app, &request_id, &thread.id);
                    continue;
                }
                Err(e) if e == llm::thinking::THINKING_BRIDGE_ERROR => {
                    break Err(llm::thinking::thinking_bridge_error());
                }
                Err(e) if e.starts_with(llm::thinking::THINKING_RUNAWAY_PREFIX) => {
                    let chars = e
                        .strip_prefix(llm::thinking::THINKING_RUNAWAY_PREFIX)
                        .unwrap_or("");
                    let n: usize = chars.parse().unwrap_or(0);
                    break Err(llm::thinking::runaway_thinking_error(n));
                }
                other => break other,
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
                    note_writes: note_write_notices(state.inner().as_ref(), &wrote_note_ids),
                });
            }
            Err(e) if round_limit_final => {
                let finished = tools::round_limit_failure(max_rounds, &e);
                result_text = finished.text;
                round_limit_aside = finished.notice;
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
                    note_writes: note_write_notices(state.inner().as_ref(), &wrote_note_ids),
                });
            }
            Ok(out) => {
                if round_limit_final {
                    let finished = tools::round_limit_answer(Some(&out.content), max_rounds);
                    if finished.notice.is_some() {
                        final_reasoning = out.reasoning;
                    }
                    result_text = finished.text;
                    round_limit_aside = finished.notice;
                    break;
                }
                if with_tools
                    && llm::thinking::thinking_enabled(&settings)
                    && !out.request_id.is_empty()
                {
                    let _ = llm::refresh_thinking_budget(&settings, Some(&out.request_id)).await;
                }
                if use_tools && !out.tool_calls.is_empty() {
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
                        let exec = execute_tool_off_runtime(
                            state_arc.clone(),
                            thread.id.clone(),
                            tc.name.clone(),
                            tc.arguments.clone(),
                            thread_scope.clone(),
                            web_search,
                            request_id.clone(),
                        )
                        .await;
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
                            fit_tool_text(content, tool_budget, tool_used)
                        };
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
                        let tools_on =
                            phantom_retry_enables_tools(unlimited_rounds, rounds, max_rounds);
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
                        empty_reply = true;
                        final_reasoning = out.reasoning;
                        result_text = silent_end_notice(&out.finish_reason);
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

/// A phantom [n] may call tools again only while rounds remain.
/// At the cap the retry stays tool-free, so it is not the round-limit final answer.
fn phantom_retry_enables_tools(unlimited_rounds: bool, rounds: usize, max_rounds: usize) -> bool {
    unlimited_rounds || rounds < max_rounds
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
        let truncated = format!("【出典】\n[3] memo.txt\n{}\n\n【質問】\n見て", &body[..12]);
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
        assert!(turns.iter().any(|turn| turn.content.contains(STOP_TOOLS_HINT)));
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
            !inject_final_source_turn(
                &mut wide,
                std::slice::from_ref(&attached),
                &[("file".into(), 3)],
                false,
                8_000,
            ),
            "a truncated tool entry is already this source"
        );
        assert_eq!(wide.len(), 2);
        assert_eq!(wide[1].content, stored);
        assert!(!wide.iter().any(|turn| turn.content.contains(&full)));
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
            origin: "tool".into(),
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
        db.insert_llm_message(&thread.id, "user", "これも見て").unwrap();
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
            .insert_llm_source(
                &thread.id,
                "tool",
                "path-E",
                "E",
                "p7",
                "次の検索",
                "query",
            )
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
            &[
                ("a".into(), 1),
                ("d".into(), 3),
            ],
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
        assert_ne!(later.cite_no, 3, "the ledger must not hand [3] to a later source");
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
        assert_ne!(memo.cite_no, 5, "memo.txt must not reuse the kept phantom [5]");
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
        assert_ne!(memo.cite_no, 5, "memo.txt must not reuse the retried phantom [5]");
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
}
