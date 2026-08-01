use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use notify::{Config, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::indexer::Indexer;
use crate::pathutil;

enum WatcherCmd {
    Add(String),
    Remove(String),
}

/// Handle for adding/removing watch roots after the watcher thread has started.
#[derive(Clone)]
pub struct WatcherHandle {
    cmd_tx: mpsc::Sender<WatcherCmd>,
}

impl WatcherHandle {
    pub fn watch_folder(&self, path: &str) {
        let path = pathutil::simplify_windows_path(path);
        if path.is_empty() {
            return;
        }
        let _ = self.cmd_tx.send(WatcherCmd::Add(path));
    }

    pub fn unwatch_folder(&self, path: &str) {
        let path = pathutil::simplify_windows_path(path);
        if path.is_empty() {
            return;
        }
        let _ = self.cmd_tx.send(WatcherCmd::Remove(path));
    }
}

pub fn start_watcher(
    folders: Vec<String>,
    indexer: Arc<Indexer>,
) -> Result<WatcherHandle, String> {
    let (cmd_tx, cmd_rx) = mpsc::channel::<WatcherCmd>();
    let handle = WatcherHandle { cmd_tx };

    thread::spawn(move || {
        let (tx, rx) = mpsc::channel();
        let mut watcher = match RecommendedWatcher::new(tx, Config::default()) {
            Ok(w) => w,
            Err(e) => {
                eprintln!("watcher init failed: {e}");
                return;
            }
        };

        let mut folders = folders;
        let mut normalized_folders: Vec<String> = folders
            .iter()
            .map(|f| pathutil::normalize_for_compare(f))
            .collect();

        for folder in &folders {
            let path = PathBuf::from(folder);
            if path.exists() {
                if let Err(e) = watcher.watch(&path, RecursiveMode::Recursive) {
                    eprintln!("watch {}: {e}", path.display());
                }
            }
        }

        // Debounce buffer
        let mut pending: Vec<(PathBuf, bool)> = Vec::new(); // path, removed
        loop {
            // Apply watch/unwatch commands without waiting for FS events
            while let Ok(cmd) = cmd_rx.try_recv() {
                apply_cmd(
                    &mut watcher,
                    &mut folders,
                    &mut normalized_folders,
                    cmd,
                );
            }

            match rx.recv_timeout(Duration::from_millis(500)) {
                Ok(Ok(event)) => {
                    let removed = matches!(
                        event.kind,
                        EventKind::Remove(_) | EventKind::Any
                    );
                    for path in event.paths {
                        pending.push((path, removed));
                    }
                }
                Ok(Err(e)) => eprintln!("watch error: {e}"),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if pending.is_empty() {
                        continue;
                    }
                    let batch = std::mem::take(&mut pending);
                    for (path, removed) in batch {
                        if removed {
                            let _ = indexer.remove_path(&path);
                            continue;
                        }
                        if !path.is_file() {
                            continue;
                        }
                        let event_norm =
                            pathutil::normalize_for_compare(&path.to_string_lossy());
                        // Find owning folder prefix (normalized)
                        let folder = normalized_folders
                            .iter()
                            .zip(folders.iter())
                            .find(|(norm, _)| pathutil::path_starts_with(&event_norm, norm))
                            .map(|(_, original)| original.clone())
                            .unwrap_or_default();
                        if folder.is_empty() {
                            continue;
                        }
                        let _ = indexer.index_path(&folder, &path);
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    });
    Ok(handle)
}

fn apply_cmd(
    watcher: &mut RecommendedWatcher,
    folders: &mut Vec<String>,
    normalized_folders: &mut Vec<String>,
    cmd: WatcherCmd,
) {
    match cmd {
        WatcherCmd::Add(path) => {
            let norm = pathutil::normalize_for_compare(&path);
            if normalized_folders.iter().any(|n| n == &norm) {
                return;
            }
            let pb = PathBuf::from(&path);
            if pb.exists() {
                if let Err(e) = watcher.watch(&pb, RecursiveMode::Recursive) {
                    eprintln!("watch {}: {e}", pb.display());
                }
            } else {
                eprintln!("watch skip (missing): {path}");
            }
            folders.push(path);
            normalized_folders.push(norm);
        }
        WatcherCmd::Remove(path) => {
            let norm = pathutil::normalize_for_compare(&path);
            let Some(idx) = normalized_folders.iter().position(|n| n == &norm) else {
                return;
            };
            let pb = PathBuf::from(&path);
            if let Err(e) = watcher.unwatch(&pb) {
                eprintln!("unwatch {}: {e}", pb.display());
            }
            folders.remove(idx);
            normalized_folders.remove(idx);
        }
    }
}
