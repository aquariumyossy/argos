use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_opener::OpenerExt;

use crate::db::{
    ExcludePathRow, FolderRow, SearchWordImport, SearchWordImportResult, SearchWordRow, Settings,
};
use crate::indexer::IndexStats;
use crate::pathutil;
use crate::remote_server;
use crate::search::{self, SearchHit};
use crate::selection;
use crate::state::AppState;
use crate::{hide_popup_window, show_main, show_popup};

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchPayload {
    pub query: String,
    pub hits: Vec<SearchHit>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchScopeRow {
    pub path: String,
    pub label: String,
    pub is_root: bool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchScopesResult {
    pub recent: Vec<SearchScopeRow>,
    pub scopes: Vec<SearchScopeRow>,
}

const MAX_SUBFOLDERS_PER_ROOT: usize = 400;
/// Wider than typical UI `max_results` so scope picker can see more matching folders.
const SCOPE_QUERY_HIT_LIMIT: usize = 200;

fn folder_display_name(path: &str) -> String {
    let simplified = pathutil::simplify_windows_path(path);
    simplified
        .rsplit('\\')
        .find(|s| !s.is_empty())
        .unwrap_or(path)
        .to_string()
}

fn parent_dir(path: &str) -> Option<String> {
    let simplified = pathutil::simplify_windows_path(path);
    let truncated = simplified.trim_end_matches('\\');
    let (parent, name) = match truncated.rfind('\\') {
        Some(i) => (&truncated[..i], &truncated[i + 1..]),
        None => return None,
    };
    if name.is_empty() {
        return None;
    }
    // Keep drive root like `C:` as `C:\`
    if parent.len() == 2 && parent.as_bytes()[1] == b':' {
        return Some(format!("{parent}\\"));
    }
    if parent.is_empty() {
        // UNC `\\server\share\file` → parent `\\server\share` handled by rfind;
        // bare `\\server` shouldn't appear as a file parent we care about.
        return None;
    }
    Some(parent.to_string())
}

fn relative_label(root: &str, dir: &str) -> String {
    let root = pathutil::simplify_windows_path(root);
    let dir = pathutil::simplify_windows_path(dir);
    if dir.eq_ignore_ascii_case(&root) {
        return folder_display_name(&root);
    }
    if !pathutil::path_starts_with(&dir, &root) {
        return folder_display_name(&dir);
    }
    let rest = dir[root.len()..].trim_start_matches('\\');
    if rest.is_empty() {
        folder_display_name(&root)
    } else {
        rest.replace('\\', "/")
    }
}

fn collect_search_scopes(
    folders: &[FolderRow],
    list_paths: impl Fn(i64) -> Result<Vec<String>, String>,
) -> Result<Vec<SearchScopeRow>, String> {
    use std::collections::BTreeMap;

    let mut out: Vec<SearchScopeRow> = Vec::new();
    for folder in folders.iter().filter(|f| f.enabled) {
        let root = pathutil::effective_public_root(&folder.path, &folder.public_path);
        out.push(SearchScopeRow {
            path: root.clone(),
            label: folder_display_name(&root),
            is_root: true,
        });

        let paths = list_paths(folder.id)?;
        // BTreeMap keeps labels sorted for stable UI.
        let mut subdirs: BTreeMap<String, String> = BTreeMap::new();
        for file_path in paths {
            let mut current = parent_dir(&file_path);
            while let Some(dir) = current {
                if !pathutil::path_starts_with(&dir, &root) {
                    break;
                }
                if dir.eq_ignore_ascii_case(&root) {
                    break;
                }
                let key = dir.to_ascii_lowercase();
                subdirs.entry(key).or_insert_with(|| dir.clone());
                if subdirs.len() >= MAX_SUBFOLDERS_PER_ROOT {
                    break;
                }
                current = parent_dir(&dir);
            }
            if subdirs.len() >= MAX_SUBFOLDERS_PER_ROOT {
                break;
            }
        }

        let mut subs: Vec<SearchScopeRow> = subdirs
            .into_values()
            .map(|path| SearchScopeRow {
                label: relative_label(&root, &path),
                path,
                is_root: false,
            })
            .collect();
        subs.sort_by(|a, b| a.label.to_ascii_lowercase().cmp(&b.label.to_ascii_lowercase()));
        out.extend(subs);
    }
    Ok(out)
}

pub async fn trigger_search(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<Arc<AppState>>();
    let limit = state.settings.read().max_results;

    let query = tauri::async_runtime::spawn_blocking(|| selection::capture_selection(800))
        .await
        .map_err(|e| e.to_string())?
        .unwrap_or_default();

    eprintln!("argos: captured query len={} preview={:?}", query.chars().count(), query.chars().take(40).collect::<String>());

    let settings = state.settings.read().clone();
    let backend = state.backend.clone();
    let user_dict = state.user_dict.read().clone();
    let q = query.clone();
    let hits = tauri::async_runtime::spawn_blocking(move || {
        search::run_search(&settings, backend.as_ref(), &q, limit, None, &user_dict)
    })
    .await
    .map_err(|e| e.to_string())??;

    show_popup(app);
    app.emit("search-results", SearchPayload { query, hits })
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn get_settings(state: State<'_, Arc<AppState>>) -> Settings {
    state.settings.read().clone()
}

#[tauri::command]
pub fn update_settings(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    mut settings: Settings,
) -> Result<Settings, String> {
    settings.popup_width = settings.popup_width.clamp(320, 1200);
    settings.popup_height = settings.popup_height.clamp(280, 1000);
    if !matches!(settings.popup_position.as_str(), "left" | "center" | "right") {
        settings.popup_position = "center".into();
    }
    settings.search_mode = search::normalize_search_mode(&settings.search_mode);
    settings.remote_server_port = settings.remote_server_port.clamp(1, 65535);
    settings.remote_timeout_ms = settings.remote_timeout_ms.clamp(500, 60_000);
    search::ensure_server_token(&mut settings);

    state.db.save_settings(&settings).map_err(|e| e.to_string())?;
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
    if let Some(w) = app.get_webview_window("popup") {
        crate::apply_popup_initial_size(&app, &w);
        if !w.is_visible().unwrap_or(false) {
            crate::apply_popup_initial_position(&app, &w);
        }
    }
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
    Ok(row)
}

#[tauri::command]
pub fn update_folder_public_path(
    state: State<'_, Arc<AppState>>,
    id: i64,
    public_path: String,
) -> Result<FolderRow, String> {
    let public_path = crate::pathutil::simplify_windows_path(public_path.trim());
    state
        .db
        .update_folder_public_path(id, &public_path)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "フォルダが見つかりません".to_string())
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

    state
        .db
        .remove_folder(id)
        .map_err(|e| e.to_string())?;
    eprintln!(
        "argos: removed folder '{}' (purged {} file paths from index)",
        folder.path,
        file_paths.len()
    );
    Ok(())
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
        .update_search_word(
            id,
            &word,
            reading.as_deref(),
            pos_label.as_deref(),
        )
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
pub fn search_query(
    state: State<'_, Arc<AppState>>,
    query: String,
    path_prefix: Option<String>,
) -> Result<Vec<SearchHit>, String> {
    let settings = state.settings.read().clone();
    let limit = settings.max_results;
    let prefix = path_prefix
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let user_dict = state.user_dict.read().clone();
    search::run_search(
        &settings,
        state.backend.as_ref(),
        &query,
        limit,
        prefix,
        &user_dict,
    )
}

#[tauri::command]
pub fn list_search_scopes(
    state: State<'_, Arc<AppState>>,
    query: Option<String>,
) -> Result<SearchScopesResult, String> {
    let folders = state.db.list_folders().map_err(|e| e.to_string())?;
    let db = state.db.clone();
    let all = collect_search_scopes(&folders, |folder_id| {
        db.list_file_paths_by_folder(folder_id)
            .map_err(|e| e.to_string())
    })?;

    let recent: Vec<SearchScopeRow> = state
        .db
        .list_recent_search_scopes()
        .into_iter()
        .map(|s| SearchScopeRow {
            path: s.path,
            label: s.label,
            is_root: false,
        })
        .collect();

    let filtered = match query
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        None => all,
        Some(q) => {
            let settings = state.settings.read().clone();
            let user_dict = state.user_dict.read().clone();
            let hits = search::run_search(
                &settings,
                state.backend.as_ref(),
                q,
                SCOPE_QUERY_HIT_LIMIT,
                None,
                &user_dict,
            )?;
            if hits.is_empty() {
                Vec::new()
            } else {
                all.into_iter()
                    .filter(|scope| {
                        hits.iter()
                            .any(|h| pathutil::path_starts_with(&h.path, &scope.path))
                    })
                    .collect()
            }
        }
    };

    // Prefer recent at the top; drop duplicates from the main list.
    let scopes = filtered
        .into_iter()
        .filter(|scope| {
            !recent
                .iter()
                .any(|r| r.path.eq_ignore_ascii_case(&scope.path))
        })
        .collect();

    Ok(SearchScopesResult { recent, scopes })
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
pub fn hide_popup(app: AppHandle) {
    hide_popup_window(&app);
}

#[tauri::command]
pub fn open_hit(app: AppHandle, path: String) -> Result<(), String> {
    let p = std::path::Path::new(&path);
    if !p.exists() {
        return Err(
            "ファイルを開けません。リモート上のパスの可能性があります。ホスト PC で開くか、共有パス（UNC）で再インデックスしてください。"
                .into(),
        );
    }
    app.opener()
        .open_path(&path, None::<&str>)
        .map_err(|e| {
            format!(
                "ファイルを開けません（{e}）。リモート上のパスの場合はホスト PC で開くか、共有パスで再インデックスしてください。"
            )
        })?;
    hide_popup_window(&app);
    Ok(())
}

/// Open the folder that contains the file (on Windows, select the file in Explorer).
#[tauri::command]
pub fn open_containing_folder(app: AppHandle, path: String) -> Result<(), String> {
    let p = std::path::Path::new(&path);
    if !p.exists() {
        return Err(
            "パスが見つかりません。リモート上のパスの可能性があります。ホスト PC で開くか、共有パス（UNC）で再インデックスしてください。"
                .into(),
        );
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

    let _ = &app;
    hide_popup_window(&app);
    Ok(())
}

#[tauri::command]
pub fn get_preview(
    state: State<'_, Arc<AppState>>,
    hit_id: String,
) -> Result<Option<SearchHit>, String> {
    let settings = state.settings.read().clone();
    search::run_preview(&settings, state.backend.as_ref(), &hit_id)
}

#[tauri::command]
pub fn test_remote_connection(state: State<'_, Arc<AppState>>) -> Result<String, String> {
    let settings = state.settings.read().clone();
    search::RemoteArgosBackend::test_connection(&settings)
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
pub async fn run_reindex(state: State<'_, Arc<AppState>>) -> Result<IndexStats, String> {
    let indexer = state.indexer.clone();
    tauri::async_runtime::spawn_blocking(move || indexer.reindex_all())
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn run_reindex_folder(
    state: State<'_, Arc<AppState>>,
    id: i64,
) -> Result<IndexStats, String> {
    let indexer = state.indexer.clone();
    tauri::async_runtime::spawn_blocking(move || indexer.reindex_folder(id))
        .await
        .map_err(|e| e.to_string())?
}
