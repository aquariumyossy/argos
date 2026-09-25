use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_opener::OpenerExt;

use crate::db::{
    EmailFolderRow, ExcludePathRow, FolderRow, NoteItemRow, NoteRow, SearchHistoryTermRow,
    SearchWordImport, SearchWordImportResult, SearchWordRow, Settings,
};
use crate::indexer::{IndexProgress, IndexStats};
use crate::mail::{self, MailSyncProgress, MailSyncStats, OutlookFolderInfo};
use crate::pathutil;
use crate::remote_server;
use crate::search::{self, SearchHit};
use crate::selection;
use crate::state::{AppState, PreviewTarget};
use crate::{
    hide_popup_window, hide_preview_window as do_hide_preview_window, show_main, show_notes,
    show_popup, show_preview,
};

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchPayload {
    pub query: String,
    pub hits: Vec<SearchHit>,
    /// True while shortcut search is still running (popup shown early).
    #[serde(default)]
    pub searching: bool,
}

pub use crate::search::{SearchScopeRow, SearchScopesResult};

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct NoteUpdatedPayload {
    note_id: String,
    kind: String,
    source: String,
}

fn emit_note_updated(app: &AppHandle, note_id: &str, kind: &str) {
    emit_note_updated_from(app, note_id, kind, "");
}

pub(crate) fn emit_note_updated_from(app: &AppHandle, note_id: &str, kind: &str, source: &str) {
    let _ = app.emit(
        "note-updated",
        NoteUpdatedPayload {
            note_id: note_id.to_string(),
            kind: kind.to_string(),
            source: source.to_string(),
        },
    );
}

pub async fn trigger_search(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<Arc<AppState>>();
    let limit = state.settings.read().max_results;

    let query = tauri::async_runtime::spawn_blocking(|| selection::capture_selection(800))
        .await
        .map_err(|e| e.to_string())?
        .unwrap_or_default();

    eprintln!(
        "argos: captured query len={} preview={:?}",
        query.chars().count(),
        query.chars().take(40).collect::<String>()
    );

    // Show popup immediately with the query; search fills results after.
    show_popup(app);
    app.emit(
        "search-results",
        SearchPayload {
            query: query.clone(),
            hits: Vec::new(),
            searching: true,
        },
    )
    .map_err(|e| e.to_string())?;

    let settings = state.settings.read().clone();
    let backend = state.backend.clone();
    let mail_backend = state.mail_backend.clone();
    let user_dict = state.user_dict.read().clone();
    let q = query.clone();
    let pos_filter = settings.pos_filter_enabled;
    let search_result = tauri::async_runtime::spawn_blocking(move || {
        let result = search::run_search(
            &settings,
            backend.as_ref(),
            Some(mail_backend.as_ref()),
            &q,
            limit,
            None,
            None,
            &user_dict,
        );
        let terms = search::extract_search_terms(&q, |text| {
            backend.morph_content_surfaces(text, pos_filter)
        })
        .unwrap_or_default();
        (result, terms)
    })
    .await
    .map_err(|e| e.to_string());

    let (hits, terms) = match search_result {
        Ok((Ok(hits), terms)) => (hits, terms),
        Ok((Err(e), _)) => {
            let _ = app.emit(
                "search-results",
                SearchPayload {
                    query: query.clone(),
                    hits: Vec::new(),
                    searching: false,
                },
            );
            return Err(e);
        }
        Err(e) => {
            let _ = app.emit(
                "search-results",
                SearchPayload {
                    query: query.clone(),
                    hits: Vec::new(),
                    searching: false,
                },
            );
            return Err(e);
        }
    };
    if !terms.is_empty() {
        let _ = state.db.record_search_terms(&terms);
    }

    app.emit(
        "search-results",
        SearchPayload {
            query,
            hits,
            searching: false,
        },
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn get_settings(state: State<'_, Arc<AppState>>) -> Settings {
    state.settings.read().clone()
}

#[tauri::command]
pub fn is_app_ready() -> bool {
    crate::is_app_ready()
}

#[tauri::command]
pub fn update_settings(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    mut settings: Settings,
) -> Result<Settings, String> {
    settings.popup_width = settings.popup_width.clamp(320, 1200);
    settings.popup_height = settings.popup_height.clamp(280, 1000);
    if !matches!(
        settings.popup_position.as_str(),
        "left" | "center" | "right"
    ) {
        settings.popup_position = "center".into();
    }
    settings.search_mode = search::normalize_search_mode(&settings.search_mode);
    settings.remote_server_port = settings.remote_server_port.clamp(1, 65535);
    settings.remote_timeout_ms = settings.remote_timeout_ms.clamp(500, 60_000);
    settings.mail_days_back = settings.mail_days_back.clamp(1, 3650);
    settings.mail_sync_interval_secs = settings.mail_sync_interval_secs.min(7 * 24 * 3600);
    settings.calendar_days_back = settings.calendar_days_back.clamp(0, 365);
    settings.calendar_days_ahead = settings.calendar_days_ahead.clamp(1, 365);
    settings.calendar_sync_interval_secs = settings.calendar_sync_interval_secs.min(7 * 24 * 3600);
    settings.calendar_ical_feeds =
        crate::mail::ical::normalize_ical_feeds(settings.calendar_ical_feeds)?;
    settings.llm_base_url = crate::llm::normalize_base_url(&settings.llm_base_url);
    if settings.llm_base_url.is_empty() {
        settings.llm_base_url = crate::db::DEFAULT_LLM_BASE_URL.into();
    }
    settings.llm_timeout_ms = settings.llm_timeout_ms.clamp(5_000, 600_000);
    settings.llm_max_context_chars = settings.llm_max_context_chars.clamp(4_000, 200_000);
    settings.llm_search_top_k = settings.llm_search_top_k.clamp(1, 16);
    settings.searxng_url = crate::llm::searxng::normalize_base_url(&settings.searxng_url);
    settings.searxng_timeout_ms = settings.searxng_timeout_ms.clamp(5_000, 30_000);
    settings.llm_web_search_top_k = settings.llm_web_search_top_k.clamp(1, 8);
    settings.llm_thinking = match settings.llm_thinking.trim() {
        "auto" | "brief" | "off" => settings.llm_thinking.trim().to_string(),
        _ => crate::db::DEFAULT_LLM_THINKING.into(),
    };
    settings.llm_thinking_budget = settings.llm_thinking_budget.min(32_000);
    if settings.llm_thinking == "brief" && settings.llm_thinking_budget == 0 {
        settings.llm_thinking_budget = crate::db::DEFAULT_LLM_THINKING_BUDGET;
    }
    if settings.llm_system_prompt.trim().is_empty() {
        settings.llm_system_prompt = crate::db::DEFAULT_LLM_SYSTEM_PROMPT.into();
    }
    search::ensure_server_token(&mut settings);

    if settings.notes_shortcut.trim().is_empty() {
        settings.notes_shortcut = "Ctrl+Alt+N".into();
    }
    if settings.notes_shortcut == settings.shortcut {
        return Err("ノート用ショートカットは検索用と同じにできません".into());
    }
    let prev_search = state.settings.read().shortcut.clone();
    let prev_notes = state.settings.read().notes_shortcut.clone();

    let ical_keep: Vec<String> = crate::mail::ical::enabled_ical_feeds(&settings.calendar_ical_feeds)
        .into_iter()
        .map(|f| crate::mail::ical::ical_folder_id(&f.id))
        .collect();

    state
        .db
        .save_settings(&settings)
        .map_err(|e| e.to_string())?;
    state
        .db
        .delete_ical_events_except(&ical_keep)
        .map_err(|e| e.to_string())?;
    // Apply autostart
    use tauri_plugin_autostart::ManagerExt;
    let launcher = app.autolaunch();
    if settings.autostart {
        let _ = launcher.enable();
    } else {
        let _ = launcher.disable();
    }
    *state.settings.write() = settings.clone();
    state.sync_remote_server();
    if settings.shortcut != prev_search || settings.notes_shortcut != prev_notes {
        if let Err(e) =
            crate::register_app_shortcuts(&app, &settings.shortcut, &settings.notes_shortcut)
        {
            eprintln!("argos: shortcut re-register failed: {e}");
        }
    }
    if let Some(w) = app.get_webview_window("popup") {
        crate::apply_popup_initial_size(&app, &w);
        if !w.is_visible().unwrap_or(false) {
            crate::apply_popup_initial_position(&app, &w);
        }
    }
    let _ = app.emit("settings-updated", ());
    Ok(settings)
}

#[tauri::command]
pub fn list_folders(state: State<'_, Arc<AppState>>) -> Result<Vec<FolderRow>, String> {
    state.db.list_folders().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn add_folder(state: State<'_, Arc<AppState>>, path: String) -> Result<FolderRow, String> {
    let path = crate::pathutil::simplify_windows_path(path.trim());
    if path.is_empty() {
        return Err("フォルダパスが空です".into());
    }
    let public_path = crate::pathutil::suggest_public_path(&path).unwrap_or_default();
    let row = state
        .db
        .add_folder(&path, &public_path)
        .map_err(|e| e.to_string())?;
    state.watch_folder(&row.path);
    state.refresh_remote_share();
    Ok(row)
}

#[tauri::command]
pub fn update_folder_public_path(
    state: State<'_, Arc<AppState>>,
    id: i64,
    public_path: String,
) -> Result<FolderRow, String> {
    let public_path = crate::pathutil::simplify_windows_path(public_path.trim());
    let row = state
        .db
        .update_folder_public_path(id, &public_path)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "フォルダが見つかりません".to_string())?;
    state.refresh_remote_share();
    Ok(row)
}

/// Rebind a registered folder to a new path and remap the index (no content re-extract).
#[tauri::command]
pub fn update_folder_path(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    id: i64,
    path: String,
) -> Result<FolderRow, String> {
    let new_path = crate::pathutil::simplify_windows_path(path.trim());
    if new_path.is_empty() {
        return Err("フォルダパスが空です".into());
    }
    if !std::path::Path::new(&new_path).is_dir() {
        return Err("指定されたパスにフォルダがありません".into());
    }

    let folder = state
        .db
        .get_folder(id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "フォルダが見つかりません".to_string())?;

    if folder.path.eq_ignore_ascii_case(&new_path) {
        return Ok(folder);
    }

    // Reject collision with another registered folder (case-insensitive).
    let folders = state.db.list_folders().map_err(|e| e.to_string())?;
    if folders
        .iter()
        .any(|f| f.id != id && f.path.eq_ignore_ascii_case(&new_path))
    {
        return Err("このパスは既に検索対象として登録されています".into());
    }

    let old_path = folder.path.clone();
    state.unwatch_folder(&old_path);

    match state.indexer.rebind_folder_path(id, &new_path) {
        Ok(row) => {
            state.watch_folder(&row.path);
            let _ = app.emit("folders-updated", ());
            state.refresh_remote_share();
            Ok(row)
        }
        Err(e) => {
            if std::path::Path::new(&old_path).is_dir() {
                state.watch_folder(&old_path);
            }
            Err(e)
        }
    }
}

#[tauri::command]
pub fn remove_folder(state: State<'_, Arc<AppState>>, id: i64) -> Result<(), String> {
    // Snapshot paths before DB delete so we can purge Tantivy
    let folder = state
        .db
        .get_folder(id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "フォルダが見つかりません".to_string())?;
    let file_paths = state
        .db
        .list_file_paths_by_folder(id)
        .map_err(|e| e.to_string())?;

    state.unwatch_folder(&folder.path);

    // Purge search index first
    state.backend.delete_by_folder(&folder.path)?;
    state.backend.delete_paths(&file_paths)?;

    state.db.remove_folder(id).map_err(|e| e.to_string())?;
    state.refresh_remote_share();
    eprintln!(
        "argos: removed folder '{}' (purged {} file paths from index)",
        folder.path,
        file_paths.len()
    );
    Ok(())
}

#[tauri::command]
pub fn set_folder_share_remote(
    state: State<'_, Arc<AppState>>,
    id: i64,
    share_remote: bool,
) -> Result<FolderRow, String> {
    let row = state
        .db
        .set_folder_share_remote(id, share_remote)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "フォルダが見つかりません".to_string())?;
    state.refresh_remote_share();
    Ok(row)
}

#[tauri::command]
pub fn list_exclude_paths(state: State<'_, Arc<AppState>>) -> Result<Vec<ExcludePathRow>, String> {
    state.db.list_exclude_paths().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn add_exclude_path(
    state: State<'_, Arc<AppState>>,
    path: String,
) -> Result<ExcludePathRow, String> {
    state.db.add_exclude_path(&path).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn remove_exclude_path(state: State<'_, Arc<AppState>>, id: i64) -> Result<(), String> {
    state.db.remove_exclude_path(id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn list_search_words(state: State<'_, Arc<AppState>>) -> Result<Vec<SearchWordRow>, String> {
    state.db.list_search_words().map_err(|e| e.to_string())
}

fn emit_search_words_updated(app: &AppHandle) {
    let _ = app.emit("search-words-updated", ());
}

#[tauri::command]
pub fn add_search_word(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    word: String,
    reading: Option<String>,
    pos_label: Option<String>,
) -> Result<SearchWordRow, String> {
    let word = word.trim().to_string();
    if word.is_empty() {
        return Err("検索ワードが空です".into());
    }
    let reading = reading.unwrap_or_default();
    let pos_label = pos_label.unwrap_or_default();
    let row = state
        .db
        .add_search_word(&word, &reading, &pos_label)
        .map_err(|e| e.to_string())?;
    state.refresh_user_dict();
    emit_search_words_updated(&app);
    Ok(row)
}

#[tauri::command]
pub fn update_search_word(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    id: i64,
    word: String,
    reading: Option<String>,
    pos_label: Option<String>,
) -> Result<SearchWordRow, String> {
    let word = word.trim().to_string();
    if word.is_empty() {
        return Err("検索ワードが空です".into());
    }
    let row = state
        .db
        .update_search_word(id, &word, reading.as_deref(), pos_label.as_deref())
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "検索ワードが見つかりません".to_string())?;
    state.refresh_user_dict();
    emit_search_words_updated(&app);
    Ok(row)
}

#[tauri::command]
pub fn remove_search_word(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    id: i64,
) -> Result<(), String> {
    state.db.remove_search_word(id).map_err(|e| e.to_string())?;
    state.refresh_user_dict();
    emit_search_words_updated(&app);
    Ok(())
}

#[tauri::command]
pub fn clear_search_words(app: AppHandle, state: State<'_, Arc<AppState>>) -> Result<u64, String> {
    let n = state.db.clear_search_words().map_err(|e| e.to_string())?;
    state.refresh_user_dict();
    emit_search_words_updated(&app);
    Ok(n)
}

#[tauri::command]
pub fn import_search_words(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    entries: Vec<SearchWordImport>,
) -> Result<SearchWordImportResult, String> {
    let result = state
        .db
        .import_search_words(&entries)
        .map_err(|e| e.to_string())?;
    state.refresh_user_dict();
    emit_search_words_updated(&app);
    Ok(result)
}

#[tauri::command]
pub async fn search_query(
    state: State<'_, Arc<AppState>>,
    query: String,
    path_prefix: Option<String>,
    exts: Option<Vec<String>>,
    date_after: Option<String>,
    date_before: Option<String>,
) -> Result<Vec<SearchHit>, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let settings = state.settings.read().clone();
        let limit = settings.max_results;
        let prefix = path_prefix
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let exts = search::normalize_exts(exts);
        let user_dict = state.user_dict.read().clone();
        let date = search::parse_date_range(date_after.as_deref(), date_before.as_deref())?;
        let filter =
            search::build_search_filter(&state.db, date, None, prefix, exts.as_deref(), false)?;
        search::run_search_with_mail_options(
            &settings,
            state.backend.as_ref(),
            Some(state.mail_backend.as_ref()),
            &query,
            limit,
            prefix,
            exts.as_deref(),
            &user_dict,
            &filter,
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn search_path_matches(
    state: State<'_, Arc<AppState>>,
    query: String,
    path: String,
    source: Option<String>,
) -> Result<Vec<SearchHit>, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let settings = state.settings.read().clone();
        let user_dict = state.user_dict.read().clone();
        search::run_list_path_matches(
            &settings,
            state.backend.as_ref(),
            Some(state.mail_backend.as_ref()),
            &query,
            &path,
            source.as_deref(),
            &user_dict,
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewFileResult {
    pub units: Vec<SearchHit>,
    pub excerpt: bool,
    pub match_ids: Vec<String>,
}

#[tauri::command]
pub async fn preview_file(
    state: State<'_, Arc<AppState>>,
    query: String,
    path: String,
) -> Result<PreviewFileResult, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let settings = state.settings.read().clone();
        let user_dict = state.user_dict.read().clone();
        search::run_preview_file(
            &settings,
            state.backend.as_ref(),
            Some(state.mail_backend.as_ref()),
            &query,
            &path,
            &user_dict,
        )
        .map(|p| PreviewFileResult {
            units: p.units,
            excerpt: p.excerpt,
            match_ids: p.match_ids,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn list_search_scopes(
    state: State<'_, Arc<AppState>>,
    query: Option<String>,
) -> Result<SearchScopesResult, String> {
    let settings = state.settings.read().clone();
    let user_dict = state.user_dict.read().clone();
    search::assemble_search_scopes(
        &state.db,
        query.as_deref(),
        &settings,
        state.backend.as_ref(),
        Some(state.mail_backend.as_ref()),
        &user_dict,
        search::ScopeListOpts {
            include_mail: false,
            share: None,
        },
    )
}

#[tauri::command]
pub fn push_recent_search_scope(
    state: State<'_, Arc<AppState>>,
    path: String,
    label: String,
) -> Result<Vec<SearchScopeRow>, String> {
    let rows = state
        .db
        .push_recent_search_scope(&path, &label)
        .map_err(|e| e.to_string())?;
    Ok(rows
        .into_iter()
        .map(|s| SearchScopeRow {
            path: s.path,
            label: s.label,
            is_root: false,
        })
        .collect())
}

#[tauri::command]
pub fn list_search_history_terms(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<SearchHistoryTermRow>, String> {
    Ok(state.db.list_search_history_terms())
}

#[tauri::command]
pub fn record_search_query(state: State<'_, Arc<AppState>>, query: String) -> Result<(), String> {
    let q = query.trim();
    if q.is_empty() {
        return Ok(());
    }
    let pos_filter = state.settings.read().pos_filter_enabled;
    let backend = state.backend.clone();
    let terms =
        search::extract_search_terms(q, |text| backend.morph_content_surfaces(text, pos_filter))?;
    state
        .db
        .record_search_terms(&terms)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn clear_search_term_history(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    state
        .db
        .clear_search_term_history()
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn suggest_search_terms(
    state: State<'_, Arc<AppState>>,
    query: String,
) -> Result<Vec<search::SearchTermSuggestion>, String> {
    let history = state.db.get_search_term_history();
    let registered: Vec<String> = state
        .db
        .list_search_words()
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|w| w.word)
        .collect();
    Ok(search::suggest_from_history(&history, &registered, &query))
}

#[tauri::command]
pub fn hide_popup(app: AppHandle) {
    hide_popup_window(&app);
}

#[tauri::command]
pub fn show_popup_window(app: AppHandle) {
    let _ = show_popup(&app);
}

#[tauri::command]
pub fn show_preview_window(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    target: PreviewTarget,
) -> Result<(), String> {
    if target.path.trim().is_empty() {
        return Err("プレビューするパスがありません".into());
    }
    *state.preview_target.write() = Some(target);
    show_preview(&app);
    Ok(())
}

#[tauri::command]
pub fn get_preview_target(state: State<'_, Arc<AppState>>) -> Option<PreviewTarget> {
    state.preview_target.read().clone()
}

#[tauri::command]
pub fn hide_preview_window(app: AppHandle) {
    do_hide_preview_window(&app);
}

#[tauri::command]
pub async fn open_hit(app: AppHandle, path: String) -> Result<(), String> {
    if mail::is_outlook_path(&path) {
        let (store_id, entry_id) = mail::parse_outlook_path(&path)
            .ok_or_else(|| "Outlook メールのパスが不正です".to_string())?;
        let state = app.state::<Arc<AppState>>();
        let mail_h = state.mail.clone();
        let opened =
            tauri::async_runtime::spawn_blocking(move || mail_h.open_item(&store_id, &entry_id))
                .await
                .map_err(|e| e.to_string())?;
        opened?;
        hide_popup_window(&app);
        return Ok(());
    }
    let p = std::path::Path::new(&path);
    if !p.exists() {
        return Err(missing_open_path_message(&app, &path, true));
    }
    app.opener()
        .open_path(&path, None::<&str>)
        .map_err(|e| {
            format!(
                "ファイルを開けません（{e}）。リモート上のパスの場合はホスト PC で開くか、共有パスで再インデックスしてください。"
            )
        })?;
    Ok(())
}

/// Open the folder that contains the file (on Windows, select the file in Explorer).
#[tauri::command]
pub async fn open_containing_folder(app: AppHandle, path: String) -> Result<(), String> {
    if mail::is_outlook_path(&path) {
        // Opening the message in Outlook is the closest equivalent.
        return open_hit(app, path).await;
    }
    let p = std::path::Path::new(&path);
    if !p.exists() {
        return Err(missing_open_path_message(&app, &path, false));
    }

    #[cfg(windows)]
    {
        // Do NOT use `explorer /select,...` for UNC: when it fails, Explorer opens Documents.
        // Also Rust Command::arg re-quotes the argument and breaks /select parsing.
        match crate::pathutil::open_folder_and_select(&path) {
            Ok(()) => {}
            Err(shell_err) => {
                // Fallback: open the parent folder (no file selection).
                let folder = p
                    .parent()
                    .ok_or_else(|| format!("親フォルダがありません（{shell_err}）"))?;
                app.opener()
                    .open_path(folder.to_string_lossy().as_ref(), None::<&str>)
                    .map_err(|e| format!("{shell_err} / 親フォルダも開けません（{e}）"))?;
            }
        }
    }

    #[cfg(not(windows))]
    {
        let folder = p
            .parent()
            .ok_or_else(|| "親フォルダがありません".to_string())?;
        app.opener()
            .open_path(folder.to_string_lossy().as_ref(), None::<&str>)
            .map_err(|e| e.to_string())?;
    }

    Ok(())
}

fn missing_open_path_message(app: &AppHandle, path: &str, is_file: bool) -> String {
    let remote_hint = if is_file {
        "ファイルを開けません。リモート上のパスの可能性があります。ホスト PC で開くか、共有パス（UNC）で再インデックスしてください。"
    } else {
        "パスが見つかりません。リモート上のパスの可能性があります。ホスト PC で開くか、共有パス（UNC）で再インデックスしてください。"
    };

    let Some(state) = app.try_state::<Arc<AppState>>() else {
        return remote_hint.to_string();
    };
    let Ok(folders) = state.db.list_folders() else {
        return remote_hint.to_string();
    };

    let simplified = pathutil::simplify_windows_path(path);
    let parent_missing = folders.iter().any(|f| {
        !f.exists
            && (pathutil::path_starts_with(&simplified, &f.path)
                || pathutil::path_starts_with(
                    &simplified,
                    &pathutil::effective_public_root(&f.path, &f.public_path),
                ))
    });
    let any_missing = folders.iter().any(|f| !f.exists);

    if parent_missing || any_missing {
        "パスが見つかりません。登録フォルダの場所が変わった可能性があります。設定で「パス変更」してください。"
            .into()
    } else {
        remote_hint.to_string()
    }
}

#[tauri::command]
pub async fn get_preview(
    state: State<'_, Arc<AppState>>,
    hit_id: String,
) -> Result<Option<SearchHit>, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let settings = state.settings.read().clone();
        search::run_preview(
            &settings,
            state.backend.as_ref(),
            Some(state.mail_backend.as_ref()),
            &hit_id,
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Read a local `.json` file as UTF-8 text for full-file preview.
#[tauri::command]
pub fn read_text_file(path: String) -> Result<String, String> {
    let p = std::path::Path::new(&path);
    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    if ext != "json" {
        return Err(format!("unsupported preview extension: {ext}"));
    }
    if !p.is_file() {
        return Err("ファイルが見つかりません".into());
    }
    std::fs::read_to_string(p).map_err(|e| e.to_string())
}

/// Write UTF-8 text to a path chosen by the user (e.g. note Markdown export).
#[tauri::command]
pub fn write_text_file(path: String, contents: String) -> Result<(), String> {
    let p = std::path::Path::new(&path);
    if let Some(parent) = p.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            return Err("保存先フォルダが存在しません".into());
        }
    }
    std::fs::write(p, contents.as_bytes()).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn test_remote_connection(state: State<'_, Arc<AppState>>) -> Result<String, String> {
    let settings = state.settings.read().clone();
    search::RemoteArgosBackend::test_connection(&settings)
}

#[tauri::command]
pub fn test_searxng_connection(state: State<'_, Arc<AppState>>) -> Result<String, String> {
    let settings = state.settings.read().clone();
    crate::llm::searxng::test_connection(&settings)
}

#[tauri::command]
pub fn open_web_url(app: AppHandle, url: String) -> Result<(), String> {
    let url = url.trim();
    if !crate::llm::searxng::is_http_url(url) {
        return Err("http(s) の URL だけ開けます。".into());
    }
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|e| format!("URL を開けません（{e}）。"))
}

#[tauri::command]
pub fn get_lan_ip_hint() -> Option<String> {
    remote_server::guess_lan_ip()
}

#[tauri::command]
pub fn show_settings_window(app: AppHandle) {
    show_main(&app);
}

#[tauri::command]
pub fn set_popup_dragging(app: AppHandle, dragging: bool) {
    crate::set_popup_dragging(dragging);
    if !dragging {
        if let Some(w) = app.get_webview_window("popup") {
            let _ = w.set_focus();
        }
    }
}

#[tauri::command]
pub async fn run_reindex(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<IndexStats, String> {
    let indexer = state.indexer.clone();
    let app_progress = app.clone();
    let stats = tauri::async_runtime::spawn_blocking(move || {
        indexer.reindex_all(|p: IndexProgress| {
            let _ = app_progress.emit("index-progress", &p);
        })
    })
    .await
    .map_err(|e| e.to_string())??;

    Ok(stats)
}

#[tauri::command]
pub async fn run_reindex_folder(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    id: i64,
) -> Result<IndexStats, String> {
    let indexer = state.indexer.clone();
    tauri::async_runtime::spawn_blocking(move || {
        indexer.reindex_folder(id, |p: IndexProgress| {
            let _ = app.emit("index-progress", &p);
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MailFolderKey {
    pub store_id: String,
    pub entry_id: String,
}

#[tauri::command]
pub async fn mail_detect_outlook(state: State<'_, Arc<AppState>>) -> Result<String, String> {
    let mail = state.mail.clone();
    tauri::async_runtime::spawn_blocking(move || mail.detect())
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn mail_outlook_running(state: State<'_, Arc<AppState>>) -> Result<bool, String> {
    let mail = state.mail.clone();
    tauri::async_runtime::spawn_blocking(move || mail.is_running())
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn mail_list_folders(state: State<'_, Arc<AppState>>) -> Result<Vec<EmailFolderRow>, String> {
    state.db.list_email_folders().map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn mail_refresh_folder_catalog(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<EmailFolderRow>, String> {
    let mail = state.mail.clone();
    let listed = tauri::async_runtime::spawn_blocking(move || mail.list_folders())
        .await
        .map_err(|e| e.to_string())??;
    let rows: Vec<EmailFolderRow> = listed
        .into_iter()
        .map(|f: OutlookFolderInfo| EmailFolderRow {
            id: 0,
            store_id: f.store_id,
            entry_id: f.entry_id,
            name: f.name,
            path_label: f.path_label,
            selected: false,
            item_count: f.item_count,
            indexed_count: 0,
        })
        .collect();
    state
        .db
        .replace_email_folder_catalog(&rows)
        .map_err(|e| e.to_string())?;
    state.db.list_email_folders().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn mail_set_selected_folders(
    state: State<'_, Arc<AppState>>,
    folders: Vec<MailFolderKey>,
) -> Result<(), String> {
    let keys: Vec<(String, String)> = folders
        .into_iter()
        .map(|f| (f.store_id, f.entry_id))
        .collect();
    state
        .db
        .set_email_folders_selected(&keys)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn mail_list_selected_folder_names(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<String>, String> {
    state
        .db
        .list_indexed_email_folder_names()
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn mail_run_sync(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<MailSyncStats, String> {
    if !state.settings.read().mail_enabled {
        return Err("Outlook メールインデックスが無効です。設定で有効にしてください。".into());
    }
    let mail = state.mail.clone();
    let app2 = app.clone();
    let stats = tauri::async_runtime::spawn_blocking(move || {
        mail.sync_all(true, move |p: MailSyncProgress| {
            let _ = app2.emit("mail-sync-progress", &p);
        })
    })
    .await
    .map_err(|e| e.to_string())??;
    let refreshed = state.db.load_settings();
    *state.settings.write() = refreshed;
    Ok(stats)
}

#[tauri::command]
pub fn mail_indexed_count(state: State<'_, Arc<AppState>>) -> Result<u32, String> {
    state.db.count_indexed_emails().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn calendar_list_folders(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<crate::db::CalendarFolderRow>, String> {
    state
        .db
        .prune_outlook_events_to_selected()
        .map_err(|e| e.to_string())?;
    state.db.list_calendar_folders().map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn calendar_refresh_folder_catalog(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<crate::db::CalendarFolderRow>, String> {
    let mail = state.mail.clone();
    let listed = tauri::async_runtime::spawn_blocking(move || mail.list_calendars())
        .await
        .map_err(|e| e.to_string())??;
    let rows: Vec<crate::db::CalendarFolderRow> = listed
        .into_iter()
        .map(|f| crate::db::CalendarFolderRow {
            id: 0,
            store_id: f.store_id,
            entry_id: f.entry_id,
            name: f.name,
            path_label: f.path_label,
            selected: false,
            is_default: f.is_default,
            item_count: f.item_count,
            event_count: 0,
        })
        .collect();
    state
        .db
        .replace_calendar_folder_catalog(&rows)
        .map_err(|e| e.to_string())?;
    state.db.list_calendar_folders().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn calendar_set_selected_folders(
    state: State<'_, Arc<AppState>>,
    folders: Vec<MailFolderKey>,
) -> Result<(), String> {
    let keys: Vec<(String, String)> = folders
        .into_iter()
        .map(|f| (f.store_id, f.entry_id))
        .collect();
    state
        .db
        .set_calendar_folders_selected(&keys)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn calendar_run_sync(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<crate::mail::calendar::CalendarSyncStats, String> {
    if !state.settings.read().calendar_enabled {
        return Err("予定表が無効です。設定で有効にしてください。".into());
    }
    let st = (*state).clone();
    let app2 = app.clone();
    let stats = tauri::async_runtime::spawn_blocking(move || {
        let _guard = st.calendar_sync.lock();
        crate::mail::ical::sync_calendar_sources(&st.db, &st.mail, true, move |p| {
            let _ = app2.emit("calendar-sync-progress", &p);
        })
    })
    .await
    .map_err(|e| e.to_string())??;
    let refreshed = state.db.load_settings();
    *state.settings.write() = refreshed;
    Ok(stats)
}

#[tauri::command]
pub fn calendar_event_count(state: State<'_, Arc<AppState>>) -> Result<u32, String> {
    state.db.count_calendar_events().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn calendar_related(
    state: State<'_, Arc<AppState>>,
    query: String,
    date_after: Option<String>,
    date_before: Option<String>,
) -> Result<Vec<crate::mail::calendar::CalendarEventView>, String> {
    if !state.settings.read().calendar_enabled {
        return Ok(Vec::new());
    }
    let settings = state.settings.read().clone();
    let morph = crate::search::morph::MorphAnalyzer::new()?;
    let surfaces = morph
        .content_surfaces(&query, settings.pos_filter_enabled)
        .unwrap_or_default();
    let terms = crate::mail::calendar::take_content_terms(&surfaces);
    if terms.is_empty() {
        return Ok(Vec::new());
    }
    let date = crate::search::parse_date_range(date_after.as_deref(), date_before.as_deref())?;
    let rows = state.db.list_calendar_events().map_err(|e| e.to_string())?;
    let mut views: Vec<_> = rows
        .iter()
        .map(calendar_row_view)
        .filter(|v| crate::mail::calendar::in_date_filter(v.start_unix, date))
        .filter(|v| crate::mail::calendar::event_matches_terms(v, &terms))
        .collect();
    let today = crate::mail::calendar::window_bounds(0, 0, chrono::Local::now()).0;
    views = crate::mail::calendar::sort_related(views, today);
    Ok(views)
}

fn calendar_row_view(row: &crate::db::CalendarEventRow) -> crate::mail::calendar::CalendarEventView {
    let appt = crate::mail::calendar::OutlookAppointment {
        store_id: row.store_id.clone(),
        entry_id: row.entry_id.clone(),
        folder_entry_id: row.folder_entry_id.clone(),
        calendar_name: row.calendar_name.clone(),
        subject: row.subject.clone(),
        location: row.location.clone(),
        organizer: row.organizer.clone(),
        attendees: row.attendees.clone(),
        categories: row.categories.clone(),
        body: row.body.clone(),
        start_unix: row.start_unix,
        end_unix: row.end_unix,
        all_day: row.all_day,
        busy_status: row.busy_status,
        private: row.private,
    };
    crate::mail::calendar::to_view(&appt)
}

// --- Notes ---

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteItemSnapshot {
    pub path: String,
    pub title: String,
    pub source: String,
    pub doc_kind: String,
    pub paragraph_id: String,
    pub label: String,
    pub page: Option<u32>,
    pub body: String,
    #[serde(default)]
    pub highlight_terms: Vec<String>,
    #[serde(default)]
    pub mail_from: String,
    #[serde(default)]
    pub mail_date: String,
    #[serde(default)]
    pub mail_folder: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeepToNotePayload {
    pub query: String,
    /// Prefer full body when available (e.g. from preview).
    #[serde(default)]
    pub body: Option<String>,
    /// Short snippet fallback when body/preview unavailable.
    #[serde(default)]
    pub snippet: Option<String>,
    pub path: String,
    pub title: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub doc_kind: String,
    /// Paragraph / unit id for preview lookup and dedupe.
    #[serde(default)]
    pub paragraph_id: String,
    #[serde(default)]
    pub label: String,
    pub page: Option<u32>,
    #[serde(default)]
    pub highlight_terms: Vec<String>,
    #[serde(default)]
    pub mail_from: String,
    #[serde(default)]
    pub mail_date: String,
    #[serde(default)]
    pub mail_folder: String,
    /// When true, do not bring the notes window to the front.
    #[serde(default)]
    pub silent: bool,
    /// `"new"` creates a note. A note id keeps to that note. Empty uses the active note.
    #[serde(default)]
    pub note_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeepToNoteResult {
    pub note: NoteRow,
    pub item: NoteItemRow,
    pub created: bool,
    pub created_note: bool,
}

#[tauri::command]
pub fn show_notes_window(app: AppHandle) {
    show_notes(&app);
}

#[tauri::command]
pub fn list_notes(state: State<'_, Arc<AppState>>) -> Result<Vec<NoteRow>, String> {
    state.db.list_notes().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn search_notes(state: State<'_, Arc<AppState>>, query: String) -> Result<Vec<String>, String> {
    state.db.search_note_ids(&query).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn create_note(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    title: Option<String>,
) -> Result<NoteRow, String> {
    let note = state
        .db
        .create_note(title.as_deref().unwrap_or(""))
        .map_err(|e| e.to_string())?;
    state
        .db
        .set_active_note_id(Some(&note.id))
        .map_err(|e| e.to_string())?;
    emit_note_updated(&app, &note.id, "list");
    Ok(note)
}

#[tauri::command]
pub fn rename_note(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    id: String,
    title: String,
) -> Result<NoteRow, String> {
    state
        .db
        .rename_note(&id, &title)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "ノートが見つかりません".into())
        .map(|n| {
            emit_note_updated(&app, &n.id, "list");
            n
        })
}

#[tauri::command]
pub fn delete_note(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    id: String,
) -> Result<(), String> {
    let active = state.db.get_active_note_id();
    state.db.delete_note(&id).map_err(|e| e.to_string())?;
    if active.as_deref() == Some(id.as_str()) {
        state
            .db
            .set_active_note_id(None)
            .map_err(|e| e.to_string())?;
    }
    emit_note_updated(&app, &id, "list");
    Ok(())
}

#[tauri::command]
pub fn get_active_note(state: State<'_, Arc<AppState>>) -> Result<Option<NoteRow>, String> {
    let Some(id) = state.db.get_active_note_id() else {
        return Ok(None);
    };
    state.db.get_note(&id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn set_active_note(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    id: String,
) -> Result<NoteRow, String> {
    let note = state
        .db
        .get_note(&id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "ノートが見つかりません".to_string())?;
    state
        .db
        .set_active_note_id(Some(&note.id))
        .map_err(|e| e.to_string())?;
    emit_note_updated(&app, &note.id, "active");
    Ok(note)
}

#[tauri::command]
pub fn update_note_memo(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    id: String,
    memo: String,
) -> Result<NoteRow, String> {
    state
        .db
        .update_note_memo(&id, &memo)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "ノートが見つかりません".into())
        .map(|n| {
            emit_note_updated(&app, &n.id, "memo");
            n
        })
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppendNoteMemoResult {
    pub note: NoteRow,
    pub chunk: String,
}

#[tauri::command]
pub fn append_note_memo(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    id: String,
    chunk: String,
    heading: Option<String>,
) -> Result<AppendNoteMemoResult, String> {
    let chunk = chunk.trim_end().to_string();
    if chunk.trim().is_empty() {
        return Err("追記する本文が空です。".into());
    }
    let note = if id.trim() == "new" {
        let created = state
            .db
            .create_note("無題のノート")
            .map_err(|e| e.to_string())?;
        state
            .db
            .set_active_note_id(Some(&created.id))
            .map_err(|e| e.to_string())?;
        created
    } else {
        state
            .db
            .get_note(&id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "ノートが見つかりません".to_string())?
    };
    let heading_ref = heading
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let next = crate::notes_md::append_chunk(&note.memo, heading_ref, &chunk)
        .map_err(|e| e.message(heading_ref.unwrap_or("")))?;
    let note = state
        .db
        .update_note_memo(&note.id, &next)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "ノートが見つかりません".to_string())?;
    emit_note_updated(&app, &note.id, "memo");
    Ok(AppendNoteMemoResult { note, chunk })
}

#[tauri::command]
pub fn undo_append_note_memo(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    id: String,
    chunk: String,
) -> Result<NoteRow, String> {
    let note = state
        .db
        .get_note(&id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "ノートが見つかりません".to_string())?;
    let next = crate::notes_md::undo_append(&note.memo, &chunk)?;
    let note = state
        .db
        .update_note_memo(&note.id, &next)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "ノートが見つかりません".to_string())?;
    emit_note_updated(&app, &note.id, "memo");
    Ok(note)
}

#[tauri::command]
pub fn set_note_view_mode(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    id: String,
    view_mode: String,
) -> Result<NoteRow, String> {
    state
        .db
        .set_note_view_mode(&id, &view_mode)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "ノートが見つかりません".into())
        .map(|n| {
            emit_note_updated(&app, &n.id, "list");
            n
        })
}

#[tauri::command]
pub fn list_note_items(
    state: State<'_, Arc<AppState>>,
    note_id: String,
) -> Result<Vec<NoteItemRow>, String> {
    state
        .db
        .list_note_items(&note_id)
        .map_err(|e| e.to_string())
}

fn insert_keep_item(
    state: &AppState,
    payload: KeepToNotePayload,
) -> Result<KeepToNoteResult, String> {
    let specified = payload
        .note_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let (note, created_note) = match specified {
        Some("new") => {
            let created = state
                .db
                .create_note("無題のノート")
                .map_err(|e| e.to_string())?;
            state
                .db
                .set_active_note_id(Some(&created.id))
                .map_err(|e| e.to_string())?;
            (created, true)
        }
        Some(id) => {
            let note = state
                .db
                .get_note(id)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| "ノートが見つかりません".to_string())?;
            state
                .db
                .set_active_note_id(Some(&note.id))
                .map_err(|e| e.to_string())?;
            (note, false)
        }
        None => {
            let mut note_id = state.db.get_active_note_id();
            if let Some(ref id) = note_id {
                if state.db.get_note(id).map_err(|e| e.to_string())?.is_none() {
                    note_id = None;
                }
            }
            if let Some(id) = note_id {
                let note = state
                    .db
                    .get_note(&id)
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| "ノートが見つかりません".to_string())?;
                (note, false)
            } else {
                let created = state
                    .db
                    .create_note("無題のノート")
                    .map_err(|e| e.to_string())?;
                state
                    .db
                    .set_active_note_id(Some(&created.id))
                    .map_err(|e| e.to_string())?;
                (created, true)
            }
        }
    };

    let paragraph_id = if payload.paragraph_id.trim().is_empty() {
        String::new()
    } else {
        payload.paragraph_id.trim().to_string()
    };

    if !paragraph_id.is_empty() {
        if let Some(existing) = state
            .db
            .find_note_item_by_paragraph(&note.id, &paragraph_id)
            .map_err(|e| e.to_string())?
        {
            return Ok(KeepToNoteResult {
                note,
                item: existing,
                created: false,
                created_note,
            });
        }
    }

    let mut body = payload
        .body
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    let mut highlight_terms = payload.highlight_terms.clone();

    if !paragraph_id.is_empty() {
        let settings = state.settings.read().clone();
        if let Ok(Some(hit)) = search::run_preview(
            &settings,
            state.backend.as_ref(),
            Some(state.mail_backend.as_ref()),
            &paragraph_id,
        ) {
            if body.is_none() {
                let text = hit.preview_text.trim();
                if !text.is_empty() {
                    body = Some(text.to_string());
                }
            }
            if highlight_terms.is_empty() && !hit.highlight_terms.is_empty() {
                highlight_terms = hit.highlight_terms;
            }
        }
    }

    let body = body
        .or_else(|| {
            payload
                .snippet
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
        })
        .unwrap_or_default();

    let snapshot = NoteItemSnapshot {
        path: payload.path,
        title: payload.title,
        source: payload.source,
        doc_kind: payload.doc_kind,
        paragraph_id: paragraph_id.clone(),
        label: payload.label,
        page: payload.page,
        body,
        highlight_terms,
        mail_from: payload.mail_from,
        mail_date: payload.mail_date,
        mail_folder: payload.mail_folder,
    };
    let item_json = serde_json::to_string(&snapshot).map_err(|e| e.to_string())?;
    let item = state
        .db
        .insert_note_item(&note.id, payload.query.trim(), &paragraph_id, &item_json)
        .map_err(|e| e.to_string())?;
    let note = state
        .db
        .get_note(&note.id)
        .map_err(|e| e.to_string())?
        .unwrap_or(note);
    Ok(KeepToNoteResult {
        note,
        item,
        created: true,
        created_note,
    })
}

#[tauri::command]
pub fn keep_to_note(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    payload: KeepToNotePayload,
) -> Result<KeepToNoteResult, String> {
    let silent = payload.silent;
    let result = insert_keep_item(state.inner().as_ref(), payload)?;
    if !silent {
        show_notes(&app);
    }
    emit_note_updated(&app, &result.note.id, "active");
    Ok(result)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeepPathMatchesResult {
    pub created: u32,
    pub skipped: u32,
}

#[tauri::command]
pub async fn keep_path_matches(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    query: String,
    path: String,
    title: String,
    source: String,
    doc_kind: String,
    mail_from: String,
    mail_date: String,
    mail_folder: String,
) -> Result<KeepPathMatchesResult, String> {
    let state = state.inner().clone();
    let result =
        tauri::async_runtime::spawn_blocking(move || -> Result<KeepPathMatchesResult, String> {
            let settings = state.settings.read().clone();
            let user_dict = state.user_dict.read().clone();
            let hits = search::run_path_matches(
                &settings,
                state.backend.as_ref(),
                Some(state.mail_backend.as_ref()),
                &query,
                &path,
                &user_dict,
            )?;
            let mut created = 0u32;
            let mut skipped = 0u32;
            for hit in hits {
                let result = insert_keep_item(
                    state.as_ref(),
                    KeepToNotePayload {
                        query: query.clone(),
                        body: Some(hit.preview_text),
                        snippet: Some(hit.snippet),
                        path: path.clone(),
                        title: if title.is_empty() {
                            hit.title
                        } else {
                            title.clone()
                        },
                        source: if source.is_empty() {
                            hit.source
                        } else {
                            source.clone()
                        },
                        doc_kind: if doc_kind.is_empty() {
                            hit.doc_kind
                        } else {
                            doc_kind.clone()
                        },
                        paragraph_id: hit.id,
                        label: hit.unit_label,
                        page: hit.page,
                        highlight_terms: hit.highlight_terms,
                        mail_from: if mail_from.is_empty() {
                            hit.mail_from
                        } else {
                            mail_from.clone()
                        },
                        mail_date: if mail_date.is_empty() {
                            hit.mail_date
                        } else {
                            mail_date.clone()
                        },
                        mail_folder: if mail_folder.is_empty() {
                            hit.mail_folder
                        } else {
                            mail_folder.clone()
                        },
                        silent: true,
                        note_id: None,
                    },
                )?;
                if result.created {
                    created += 1;
                } else {
                    skipped += 1;
                }
            }
            Ok(KeepPathMatchesResult { created, skipped })
        })
        .await
        .map_err(|e| e.to_string())??;
    emit_note_updated(&app, "", "list");
    Ok(result)
}

#[tauri::command]
pub fn remove_note_item(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    id: String,
) -> Result<(), String> {
    state.db.remove_note_item(&id).map_err(|e| e.to_string())?;
    emit_note_updated(&app, "", "items");
    Ok(())
}

#[tauri::command]
pub fn update_note_item_memo(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    id: String,
    memo: String,
) -> Result<NoteItemRow, String> {
    state
        .db
        .update_note_item_memo(&id, &memo)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "キープ項目が見つかりません".into())
        .map(|n| {
            emit_note_updated(&app, &n.note_id, "items");
            n
        })
}

#[tauri::command]
pub fn reorder_note_items(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    note_id: String,
    ordered_ids: Vec<String>,
) -> Result<(), String> {
    state
        .db
        .reorder_note_items(&note_id, &ordered_ids)
        .map_err(|e| e.to_string())?;
    emit_note_updated(&app, &note_id, "items");
    Ok(())
}

#[tauri::command]
pub fn reorder_notes(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    ordered_ids: Vec<String>,
) -> Result<(), String> {
    state
        .db
        .reorder_notes(&ordered_ids)
        .map_err(|e| e.to_string())?;
    emit_note_updated(&app, "", "list");
    Ok(())
}
