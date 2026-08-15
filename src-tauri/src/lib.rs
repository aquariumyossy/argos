pub mod commands;
pub mod db;
pub mod extractor;
pub mod indexer;
pub mod llm;
pub mod mail;
pub mod pathutil;
pub mod remote_server;
pub mod search;
pub mod selection;
pub mod state;
pub mod watcher;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, LogicalSize, Manager, PhysicalPosition, WebviewWindow, WebviewWindowBuilder,
    WindowEvent,
};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

use crate::state::AppState;

/// True while the user is dragging the popup (set from the frontend).
static POPUP_DRAGGING: AtomicBool = AtomicBool::new(false);
/// Set after `AppState` is managed so frontends can poll without needing State.
static APP_READY: AtomicBool = AtomicBool::new(false);

pub fn set_popup_dragging(dragging: bool) {
    POPUP_DRAGGING.store(dragging, Ordering::SeqCst);
}

pub fn is_app_ready() -> bool {
    APP_READY.load(Ordering::SeqCst)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_autostart::Builder::new().build())
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main(app);
        }))
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, shortcut, event| {
                    if event.state != ShortcutState::Pressed {
                        return;
                    }
                    let Some(state) = app.try_state::<Arc<AppState>>() else {
                        return;
                    };
                    let (search_sc, notes_sc) = {
                        let s = state.settings.read();
                        (s.shortcut.clone(), s.notes_shortcut.clone())
                    };
                    let is_notes = parse_shortcut(&notes_sc)
                        .map(|sc| &sc == shortcut)
                        .unwrap_or(false);
                    let is_search = parse_shortcut(&search_sc)
                        .map(|sc| &sc == shortcut)
                        .unwrap_or(false);
                    let app = app.clone();
                    if is_notes && !is_search {
                        tauri::async_runtime::spawn(async move {
                            show_notes(&app);
                        });
                        return;
                    }
                    // Default / search shortcut (also if both resolve equal — prefer search).
                    if is_search || !is_notes {
                        tauri::async_runtime::spawn(async move {
                            if let Err(e) = commands::trigger_search(&app).await {
                                eprintln!("search trigger failed: {e}");
                            }
                        });
                    }
                })
                .build(),
        )
        .setup(|app| {
            let (state, needs_full_reindex) = AppState::open().map_err(|e| {
                eprintln!("failed to open app state: {e}");
                Box::<dyn std::error::Error>::from(e)
            })?;
            let shortcut = state.settings.read().shortcut.clone();
            let notes_shortcut = state.settings.read().notes_shortcut.clone();
            let folders = state
                .db
                .list_folders()
                .unwrap_or_default()
                .into_iter()
                .filter(|f| f.enabled)
                .map(|f| f.path)
                .collect::<Vec<_>>();
            let indexer = state.indexer.clone();
            app.manage(Arc::new(state));
            APP_READY.store(true, Ordering::SeqCst);
            // Frontend may have mounted before AppState was ready; wake retrying loaders.
            let _ = app.emit("argos-ready", ());

            if let Err(e) = setup_tray(app.handle()) {
                eprintln!("argos: tray setup failed (continuing): {e}");
            }

            if let Err(e) = register_app_shortcuts(app.handle(), &shortcut, &notes_shortcut) {
                eprintln!("argos: shortcut registration failed (continuing): {e}");
            }

            if let Some(main) = app.get_webview_window("main") {
                attach_main_window_handlers(&main);
                let _ = main.hide();
            }
            if let Some(popup) = app.get_webview_window("popup") {
                let _ = popup.hide();
                attach_popup_window_handlers(&popup);
            }
            if let Some(notes) = app.get_webview_window("notes") {
                let _ = notes.hide();
                attach_notes_window_handlers(&notes);
            }
            if let Some(chat) = app.get_webview_window("chat") {
                let _ = chat.hide();
                attach_chat_window_handlers(&chat);
            }

            match watcher::start_watcher(folders, indexer.clone(), app.handle().clone()) {
                Ok(handle) => {
                    app.state::<Arc<AppState>>().set_watcher(handle);
                }
                Err(e) => eprintln!("watcher start failed: {e}"),
            }

            // Start LAN search server if enabled in settings
            {
                let state = app.state::<Arc<AppState>>();
                state.sync_remote_server();
            }

            if needs_full_reindex {
                eprintln!(
                    "argos: schema migration — starting automatic full reindex (indexes are incompatible)"
                );
                let indexer = indexer.clone();
                tauri::async_runtime::spawn(async move {
                    match tauri::async_runtime::spawn_blocking(move || indexer.reindex_all(|_| {})).await
                    {
                        Ok(Ok(stats)) => eprintln!(
                            "argos: schema reindex done: indexed={} skipped={} errors={}",
                            stats.indexed, stats.skipped, stats.errors
                        ),
                        Ok(Err(e)) => eprintln!("argos: schema reindex failed: {e}"),
                        Err(e) => eprintln!("argos: schema reindex join failed: {e}"),
                    }
                });
            }

            // Periodic Outlook mail sync (dedicated interval; 0 = manual only).
            {
                let app_handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    loop {
                        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                        let state = app_handle.state::<Arc<AppState>>();
                        let (enabled, interval) = {
                            let s = state.settings.read();
                            (s.mail_enabled, s.mail_sync_interval_secs)
                        };
                        if !enabled || interval == 0 {
                            continue;
                        }
                        let due = {
                            let last = state.settings.read().mail_last_sync_at.clone();
                            if last.is_empty() {
                                true
                            } else if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&last) {
                                let elapsed = chrono::Utc::now().timestamp() - dt.timestamp();
                                elapsed >= interval as i64
                            } else {
                                true
                            }
                        };
                        if !due {
                            continue;
                        }
                        let mail = state.mail.clone();
                        let app2 = app_handle.clone();
                        let _ = tauri::async_runtime::spawn_blocking(move || {
                            // Do not launch Outlook from background sync.
                            mail.sync_all(false, move |p| {
                                let _ = app2.emit("mail-sync-progress", &p);
                            })
                        })
                        .await;
                        // Refresh cached settings (last sync timestamp).
                        let state = app_handle.state::<Arc<AppState>>();
                        let refreshed = state.db.load_settings();
                        *state.settings.write() = refreshed;
                    }
                });
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_settings,
            commands::is_app_ready,
            commands::update_settings,
            commands::list_folders,
            commands::add_folder,
            commands::update_folder_public_path,
            commands::update_folder_path,
            commands::remove_folder,
            commands::list_exclude_paths,
            commands::add_exclude_path,
            commands::remove_exclude_path,
            commands::list_search_words,
            commands::add_search_word,
            commands::update_search_word,
            commands::remove_search_word,
            commands::clear_search_words,
            commands::import_search_words,
            commands::search_query,
            commands::search_path_matches,
            commands::preview_file,
            commands::list_search_scopes,
            commands::push_recent_search_scope,
            commands::list_search_history_terms,
            commands::record_search_query,
            commands::clear_search_term_history,
            commands::suggest_search_terms,
            commands::hide_popup,
            commands::open_hit,
            commands::open_containing_folder,
            commands::get_preview,
            commands::read_text_file,
            commands::write_text_file,
            commands::test_remote_connection,
            commands::get_lan_ip_hint,
            commands::show_settings_window,
            commands::run_reindex,
            commands::run_reindex_folder,
            commands::set_popup_dragging,
            commands::mail_detect_outlook,
            commands::mail_outlook_running,
            commands::mail_list_folders,
            commands::mail_refresh_folder_catalog,
            commands::mail_set_selected_folders,
            commands::mail_list_selected_folder_names,
            commands::mail_run_sync,
            commands::mail_indexed_count,
            commands::show_notes_window,
            commands::list_notes,
            commands::create_note,
            commands::rename_note,
            commands::delete_note,
            commands::get_active_note,
            commands::set_active_note,
            commands::update_note_memo,
            commands::set_note_view_mode,
            commands::list_note_items,
            commands::keep_to_note,
            commands::keep_path_matches,
            commands::remove_note_item,
            commands::update_note_item_memo,
            commands::reorder_note_items,
            commands::reorder_notes,
            llm::commands::show_chat_window,
            llm::commands::llm_list_threads,
            llm::commands::llm_create_thread,
            llm::commands::llm_rename_thread,
            llm::commands::llm_set_thread_scope,
            llm::commands::llm_delete_thread,
            llm::commands::llm_get_active_thread,
            llm::commands::llm_set_active_thread,
            llm::commands::llm_list_messages,
            llm::commands::llm_list_sources,
            llm::commands::llm_attach_sources,
            llm::commands::llm_remove_source,
            llm::commands::llm_preview_source_file,
            llm::commands::llm_set_source_grain,
            llm::commands::llm_send,
            llm::commands::llm_cancel,
            llm::commands::llm_test_connection,
            llm::commands::llm_list_models,
        ])
        .run(tauri::generate_context!())
        .unwrap_or_else(|e| {
            eprintln!("error while running Argos: {e}");
        });
}

fn setup_tray(app: &AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    let show_settings = MenuItem::with_id(app, "settings", "設定を開く", true, None::<&str>)?;
    let chat_item = MenuItem::with_id(app, "chat", "チャットを開く", true, None::<&str>)?;
    let reindex = MenuItem::with_id(app, "reindex", "インデックス再構築", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "終了", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show_settings, &chat_item, &reindex, &quit])?;

    let icon = app
        .default_window_icon()
        .cloned()
        .ok_or("default window icon missing")?;
    let _tray = TrayIconBuilder::new()
        .icon(icon)
        .menu(&menu)
        .tooltip("Argos")
        .on_menu_event(|app, event| match event.id.as_ref() {
            "settings" => {
                show_main(app);
            }
            "chat" => {
                show_chat(app);
            }
            "reindex" => {
                let state = app.state::<Arc<AppState>>();
                let indexer = state.indexer.clone();
                tauri::async_runtime::spawn(async move {
                    match tauri::async_runtime::spawn_blocking(move || indexer.reindex_all(|_| {}))
                        .await
                    {
                        Ok(Ok(stats)) => eprintln!(
                            "reindex done: indexed={} skipped={} errors={}",
                            stats.indexed, stats.skipped, stats.errors
                        ),
                        Ok(Err(e)) => eprintln!("reindex failed: {e}"),
                        Err(e) => eprintln!("reindex join failed: {e}"),
                    }
                });
            }
            "quit" => {
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main(tray.app_handle());
            }
        })
        .build(app)?;

    Ok(())
}

fn attach_main_window_handlers(window: &WebviewWindow) {
    let handle = window.clone();
    window.clone().on_window_event(move |event| {
        if let WindowEvent::CloseRequested { api, .. } = event {
            api.prevent_close();
            if let Err(e) = handle.hide() {
                eprintln!("argos: hide main window failed: {e}");
            }
        }
    });
}

fn attach_popup_window_handlers(window: &WebviewWindow) {
    let handle = window.clone();
    window.clone().on_window_event(move |event| {
        if let WindowEvent::CloseRequested { api, .. } = event {
            api.prevent_close();
            if let Err(e) = handle.hide() {
                eprintln!("argos: hide popup window failed: {e}");
            }
        }
    });
}

fn attach_notes_window_handlers(window: &WebviewWindow) {
    let handle = window.clone();
    window.clone().on_window_event(move |event| {
        if let WindowEvent::CloseRequested { api, .. } = event {
            api.prevent_close();
            if let Err(e) = handle.hide() {
                eprintln!("argos: hide notes window failed: {e}");
            }
        }
    });
}

fn attach_chat_window_handlers(window: &WebviewWindow) {
    let handle = window.clone();
    window.clone().on_window_event(move |event| {
        if let WindowEvent::CloseRequested { api, .. } = event {
            api.prevent_close();
            if let Err(e) = handle.hide() {
                eprintln!("argos: hide chat window failed: {e}");
            }
        }
    });
}

fn create_notes_window(app: &AppHandle) -> Option<WebviewWindow> {
    let config = app
        .config()
        .app
        .windows
        .iter()
        .find(|w| w.label == "notes")?;
    let window = WebviewWindowBuilder::from_config(app, config)
        .ok()?
        .build()
        .map_err(|e| {
            eprintln!("argos: failed to recreate notes window: {e}");
            e
        })
        .ok()?;
    attach_notes_window_handlers(&window);
    Some(window)
}

fn ensure_notes_window(app: &AppHandle) -> Option<WebviewWindow> {
    if let Some(w) = app.get_webview_window("notes") {
        return Some(w);
    }
    eprintln!("argos: notes window missing; recreating");
    create_notes_window(app)
}

pub fn show_notes(app: &AppHandle) {
    let Some(w) = ensure_notes_window(app) else {
        eprintln!("argos: notes window unavailable");
        return;
    };
    if let Err(e) = w.unminimize() {
        eprintln!("argos: unminimize notes window failed: {e}");
    }
    if let Err(e) = w.show() {
        eprintln!("argos: show notes window failed: {e}");
    }
    if let Err(e) = w.set_focus() {
        eprintln!("argos: focus notes window failed: {e}");
    }
}

fn create_chat_window(app: &AppHandle) -> Option<WebviewWindow> {
    let config = app
        .config()
        .app
        .windows
        .iter()
        .find(|w| w.label == "chat")?;
    let window = WebviewWindowBuilder::from_config(app, config)
        .ok()?
        .build()
        .map_err(|e| {
            eprintln!("argos: failed to recreate chat window: {e}");
            e
        })
        .ok()?;
    attach_chat_window_handlers(&window);
    Some(window)
}

fn ensure_chat_window(app: &AppHandle) -> Option<WebviewWindow> {
    if let Some(w) = app.get_webview_window("chat") {
        return Some(w);
    }
    eprintln!("argos: chat window missing; recreating");
    create_chat_window(app)
}

pub fn show_chat(app: &AppHandle) {
    let Some(w) = ensure_chat_window(app) else {
        eprintln!("argos: chat window unavailable");
        return;
    };
    if let Err(e) = w.unminimize() {
        eprintln!("argos: unminimize chat window failed: {e}");
    }
    if let Err(e) = w.show() {
        eprintln!("argos: show chat window failed: {e}");
    }
    if let Err(e) = w.set_focus() {
        eprintln!("argos: focus chat window failed: {e}");
    }
}

pub fn register_app_shortcuts(
    app: &AppHandle,
    search: &str,
    notes: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    // Unregister known previous combos best-effort, then register both.
    let _ = app.global_shortcut().unregister_all();
    let search_parsed = parse_shortcut(search).unwrap_or_else(|| {
        Shortcut::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::KeyA)
    });
    app.global_shortcut().register(search_parsed)?;
    if let Some(notes_parsed) = parse_shortcut(notes) {
        if notes_parsed != search_parsed {
            if let Err(e) = app.global_shortcut().register(notes_parsed) {
                eprintln!("argos: notes shortcut register failed: {e}");
            }
        }
    }
    Ok(())
}

fn create_main_window(app: &AppHandle) -> Option<WebviewWindow> {
    let config = app
        .config()
        .app
        .windows
        .iter()
        .find(|w| w.label == "main")?;
    let window = WebviewWindowBuilder::from_config(app, config)
        .ok()?
        .build()
        .map_err(|e| {
            eprintln!("argos: failed to recreate main window: {e}");
            e
        })
        .ok()?;
    attach_main_window_handlers(&window);
    Some(window)
}

fn ensure_main_window(app: &AppHandle) -> Option<WebviewWindow> {
    if let Some(w) = app.get_webview_window("main") {
        return Some(w);
    }
    eprintln!("argos: main window missing; recreating");
    create_main_window(app)
}

pub fn show_main(app: &AppHandle) {
    let Some(w) = ensure_main_window(app) else {
        eprintln!("argos: settings window unavailable");
        return;
    };
    if let Err(e) = w.unminimize() {
        eprintln!("argos: unminimize main window failed: {e}");
    }
    if let Err(e) = w.show() {
        eprintln!("argos: show main window failed: {e}");
    }
    if let Err(e) = w.set_focus() {
        eprintln!("argos: focus main window failed: {e}");
    }
}

pub fn show_popup(app: &AppHandle) -> Option<WebviewWindow> {
    let w = ensure_popup_window(app)?;
    // Keep user-dragged position / resized size while the popup stays open
    let visible = w.is_visible().unwrap_or(false);
    if !visible {
        apply_popup_initial_size(app, &w);
        apply_popup_initial_position(app, &w);
    }
    let _ = w.unminimize();
    let _ = w.show();
    let _ = w.set_focus();
    Some(w)
}

fn create_popup_window(app: &AppHandle) -> Option<WebviewWindow> {
    let config = app
        .config()
        .app
        .windows
        .iter()
        .find(|w| w.label == "popup")?;
    let window = WebviewWindowBuilder::from_config(app, config)
        .ok()?
        .build()
        .map_err(|e| {
            eprintln!("argos: failed to recreate popup window: {e}");
            e
        })
        .ok()?;
    attach_popup_window_handlers(&window);
    Some(window)
}

fn ensure_popup_window(app: &AppHandle) -> Option<WebviewWindow> {
    if let Some(w) = app.get_webview_window("popup") {
        return Some(w);
    }
    eprintln!("argos: popup window missing; recreating");
    create_popup_window(app)
}

pub fn apply_popup_initial_size(app: &AppHandle, w: &WebviewWindow) {
    let Some(state) = app.try_state::<Arc<AppState>>() else {
        return;
    };
    let (width, height) = {
        let s = state.settings.read();
        (s.popup_width.max(320), s.popup_height.max(280))
    };
    let _ = w.set_size(LogicalSize::new(width as f64, height as f64));
}

pub fn apply_popup_initial_position(app: &AppHandle, w: &WebviewWindow) {
    let Some(state) = app.try_state::<Arc<AppState>>() else {
        return;
    };
    let position = state.settings.read().popup_position.clone();
    if position == "center" {
        let _ = w.center();
        return;
    }

    let monitor = w
        .current_monitor()
        .ok()
        .flatten()
        .or_else(|| w.primary_monitor().ok().flatten());
    let Some(monitor) = monitor else {
        let _ = w.center();
        return;
    };

    let work = monitor.work_area();
    let Ok(size) = w.outer_size() else {
        let _ = w.center();
        return;
    };
    let margin = (20.0 * monitor.scale_factor()).round() as i32;
    let y = work.position.y
        + ((work.size.height as i32 - size.height as i32) / 2).max(margin);
    let x = if position == "left" {
        work.position.x + margin
    } else {
        // right
        work.position.x + work.size.width as i32 - size.width as i32 - margin
    };
    let _ = w.set_position(PhysicalPosition::new(x, y));
}

pub fn hide_popup_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("popup") {
        let _ = w.hide();
    }
}

fn parse_shortcut(s: &str) -> Option<Shortcut> {
    let lower = s.to_lowercase();
    let mut mods = Modifiers::empty();
    if lower.contains("ctrl") || lower.contains("control") {
        mods |= Modifiers::CONTROL;
    }
    if lower.contains("shift") {
        mods |= Modifiers::SHIFT;
    }
    if lower.contains("alt") {
        mods |= Modifiers::ALT;
    }
    if lower.contains("super") || lower.contains("meta") || lower.contains("win") {
        mods |= Modifiers::SUPER;
    }
    if mods.is_empty() {
        return None;
    }
    let key = lower
        .split('+')
        .map(str::trim)
        .filter(|p| {
            !matches!(
                *p,
                "ctrl"
                    | "control"
                    | "shift"
                    | "alt"
                    | "super"
                    | "meta"
                    | "win"
                    | "windows"
                    | ""
            )
        })
        .next_back()?;
    let code = match key {
        "space" => Code::Space,
        "q" => Code::KeyQ,
        "w" => Code::KeyW,
        "e" => Code::KeyE,
        "a" => Code::KeyA,
        "s" => Code::KeyS,
        "d" => Code::KeyD,
        "z" => Code::KeyZ,
        "x" => Code::KeyX,
        "c" => Code::KeyC,
        "f" => Code::KeyF,
        "g" => Code::KeyG,
        "r" => Code::KeyR,
        "t" => Code::KeyT,
        "b" => Code::KeyB,
        "v" => Code::KeyV,
        "n" => Code::KeyN,
        "m" => Code::KeyM,
        "h" => Code::KeyH,
        "j" => Code::KeyJ,
        "k" => Code::KeyK,
        "l" => Code::KeyL,
        "y" => Code::KeyY,
        "u" => Code::KeyU,
        "i" => Code::KeyI,
        "o" => Code::KeyO,
        "p" => Code::KeyP,
        _ => return None,
    };
    Some(Shortcut::new(Some(mods), code))
}
