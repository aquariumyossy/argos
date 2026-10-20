use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use notify::event::{ModifyKind, RenameMode};
use notify::{Config, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tauri::{AppHandle, Emitter};

use crate::indexer::{FsEvent, Indexer};
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
    app: AppHandle,
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

        // Debounce buffer. Rename/move is a remove of the old path plus an appear of the new one.
        let mut pending: Vec<FsEvent> = Vec::new();
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
                    pending.extend(events_from_notify(&event.kind, &event.paths));
                }
                Ok(Err(e)) => eprintln!("watch error: {e}"),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if pending.is_empty() {
                        continue;
                    }
                    let batch = std::mem::take(&mut pending);
                    let mut folders_updated = false;
                    for event in batch {
                        match indexer.handle_fs_event(event) {
                            Ok(true) => folders_updated = true,
                            Ok(false) => {}
                            Err(e) => eprintln!("index error: {e}"),
                        }
                    }
                    if folders_updated {
                        let _ = app.emit("folders-updated", ());
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    });
    Ok(handle)
}

/// Map a notify event onto index updates.
///
/// Deletes and the old side of a rename drop the path (and, later, everything under it).
/// Creates and the new side of a rename index a file or crawl a moved folder. A plain
/// content change does not crawl directories — child files arrive as their own events.
pub(crate) fn events_from_notify(kind: &EventKind, paths: &[PathBuf]) -> Vec<FsEvent> {
    if paths.is_empty() {
        return Vec::new();
    }
    match kind {
        EventKind::Remove(_) => map_paths(paths, FsEvent::Removed),
        EventKind::Create(_) => map_paths(paths, FsEvent::Appeared),
        EventKind::Modify(ModifyKind::Name(mode)) => match mode {
            RenameMode::From => map_paths(paths, FsEvent::Removed),
            RenameMode::To => map_paths(paths, FsEvent::Appeared),
            RenameMode::Both if paths.len() >= 2 => {
                let mut out = vec![FsEvent::Removed(paths[0].clone())];
                for to in paths.iter().skip(1) {
                    out.push(FsEvent::Appeared(to.clone()));
                }
                out
            }
            // Unpaired rename: the indexer checks whether the path is still on disk.
            RenameMode::Both | RenameMode::Any => map_paths(paths, FsEvent::Appeared),
            _ => map_paths(paths, FsEvent::Appeared),
        },
        EventKind::Modify(_) | EventKind::Any => map_paths(paths, FsEvent::Changed),
        EventKind::Access(_) | EventKind::Other => Vec::new(),
    }
}

fn map_paths(paths: &[PathBuf], kind: fn(PathBuf) -> FsEvent) -> Vec<FsEvent> {
    paths.iter().cloned().map(kind).collect()
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use notify::event::{CreateKind, DataChange, ModifyKind, RemoveKind, RenameMode};
    use notify::EventKind;

    use super::events_from_notify;
    use crate::indexer::FsEvent;

    #[test]
    fn rename_both_drops_old_path_and_adds_new_path() {
        let events = events_from_notify(
            &EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
            &[PathBuf::from(r"C:\docs\old.txt"), PathBuf::from(r"C:\docs\new.txt")],
        );
        assert_eq!(
            events,
            vec![
                FsEvent::Removed(PathBuf::from(r"C:\docs\old.txt")),
                FsEvent::Appeared(PathBuf::from(r"C:\docs\new.txt")),
            ]
        );
    }

    #[test]
    fn rename_from_and_to_are_not_treated_as_edits() {
        let from = events_from_notify(
            &EventKind::Modify(ModifyKind::Name(RenameMode::From)),
            &[PathBuf::from(r"C:\docs\sub")],
        );
        let to = events_from_notify(
            &EventKind::Modify(ModifyKind::Name(RenameMode::To)),
            &[PathBuf::from(r"C:\docs\sub2")],
        );
        assert_eq!(from, vec![FsEvent::Removed(PathBuf::from(r"C:\docs\sub"))]);
        assert_eq!(to, vec![FsEvent::Appeared(PathBuf::from(r"C:\docs\sub2"))]);
    }

    #[test]
    fn remove_is_a_deletion_and_create_is_an_appear() {
        assert_eq!(
            events_from_notify(
                &EventKind::Remove(RemoveKind::File),
                &[PathBuf::from("a.txt")]
            ),
            vec![FsEvent::Removed(PathBuf::from("a.txt"))]
        );
        assert_eq!(
            events_from_notify(
                &EventKind::Create(CreateKind::Folder),
                &[PathBuf::from("dir")]
            ),
            vec![FsEvent::Appeared(PathBuf::from("dir"))]
        );
    }

    #[test]
    fn content_change_does_not_crawl_as_a_new_folder() {
        let events = events_from_notify(
            &EventKind::Modify(ModifyKind::Data(DataChange::Any)),
            &[PathBuf::from("note.txt")],
        );
        assert_eq!(events, vec![FsEvent::Changed(PathBuf::from("note.txt"))]);
    }
}
