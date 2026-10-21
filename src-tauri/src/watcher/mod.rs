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
    /// When `online`, drop the current watch and create it again.
    /// When `online` is false the root is not a directory we could see. Keep any
    /// watch that already exists and do not call `watch`.
    Rearm { path: String, online: bool },
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

    /// Recreate the filesystem watch for one enabled root that stat confirmed is online.
    ///
    /// `online == false` leaves an existing watch in place and does not call `watch`,
    /// so a root that is merely unconfirmed cannot drop live updates. A failure to
    /// watch this path does not remove it from event routing.
    pub fn rearm_folder(&self, path: &str, online: bool) -> Result<(), String> {
        let path = pathutil::simplify_windows_path(path);
        if path.is_empty() {
            return Ok(());
        }
        self.cmd_tx
            .send(WatcherCmd::Rearm { path, online })
            .map_err(|_| "watcher stopped".to_string())
    }
}

trait RootWatch {
    fn watch_root(&mut self, path: &std::path::Path) -> Result<(), String>;
    fn unwatch_root(&mut self, path: &std::path::Path) -> Result<(), String>;
}

impl RootWatch for RecommendedWatcher {
    fn watch_root(&mut self, path: &std::path::Path) -> Result<(), String> {
        Watcher::watch(self, path, RecursiveMode::Recursive).map_err(|e| e.to_string())
    }

    fn unwatch_root(&mut self, path: &std::path::Path) -> Result<(), String> {
        Watcher::unwatch(self, path).map_err(|e| e.to_string())
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
        let mut armed = Vec::with_capacity(folders.len());

        for folder in &folders {
            let path = PathBuf::from(folder);
            let mut is_armed = false;
            if path.exists() {
                match watcher.watch_root(&path) {
                    Ok(()) => is_armed = true,
                    Err(e) => eprintln!("watch {}: {e}", path.display()),
                }
            }
            armed.push(is_armed);
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
                    &mut armed,
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
            // Unpaired rename, or a mode we cannot tell apart: the indexer checks
            // whether the path is still on disk.
            RenameMode::Both | RenameMode::Any | RenameMode::Other => {
                map_paths(paths, FsEvent::Appeared)
            }
        },
        EventKind::Modify(_) | EventKind::Any => map_paths(paths, FsEvent::Changed),
        EventKind::Access(_) | EventKind::Other => Vec::new(),
    }
}

fn map_paths(paths: &[PathBuf], kind: fn(PathBuf) -> FsEvent) -> Vec<FsEvent> {
    paths.iter().cloned().map(kind).collect()
}

fn apply_cmd(
    watcher: &mut dyn RootWatch,
    folders: &mut Vec<String>,
    normalized_folders: &mut Vec<String>,
    armed: &mut Vec<bool>,
    cmd: WatcherCmd,
) {
    debug_assert_eq!(folders.len(), normalized_folders.len());
    debug_assert_eq!(folders.len(), armed.len());
    match cmd {
        WatcherCmd::Add(path) => {
            let norm = pathutil::normalize_for_compare(&path);
            if normalized_folders.iter().any(|n| n == &norm) {
                return;
            }
            let pb = PathBuf::from(&path);
            let mut is_armed = false;
            if pb.exists() {
                match watcher.watch_root(&pb) {
                    Ok(()) => is_armed = true,
                    Err(e) => eprintln!("watch {}: {e}", pb.display()),
                }
            } else {
                eprintln!("watch skip (missing): {path}");
            }
            folders.push(path);
            normalized_folders.push(norm);
            armed.push(is_armed);
        }
        WatcherCmd::Remove(path) => {
            let norm = pathutil::normalize_for_compare(&path);
            let Some(idx) = normalized_folders.iter().position(|n| n == &norm) else {
                return;
            };
            let stored = folders[idx].clone();
            let pb = PathBuf::from(&stored);
            if let Err(e) = watcher.unwatch_root(&pb) {
                eprintln!("unwatch {stored}: {e}");
            }
            folders.remove(idx);
            normalized_folders.remove(idx);
            armed.remove(idx);
        }
        WatcherCmd::Rearm { path, online } => {
            let norm = pathutil::normalize_for_compare(&path);
            let idx = if let Some(idx) = normalized_folders.iter().position(|n| n == &norm) {
                idx
            } else {
                folders.push(path.clone());
                normalized_folders.push(norm);
                armed.push(false);
                folders.len() - 1
            };
            let stored = folders[idx].clone();
            let pb = PathBuf::from(&stored);
            if !online {
                eprintln!("argos: watch rearm skip (not confirmed online): {stored}");
                return;
            }
            if armed[idx] {
                if let Err(e) = watcher.unwatch_root(&pb) {
                    eprintln!("argos: unwatch {stored}: {e}");
                }
                armed[idx] = false;
            }
            match watcher.watch_root(&pb) {
                Ok(()) => armed[idx] = true,
                Err(e) => {
                    eprintln!("argos: watch rearm {stored}: {e}");
                    armed[idx] = false;
                }
            }
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

    #[derive(Default)]
    struct FakeWatch {
        calls: Vec<(&'static str, String)>,
        fail_watch: Vec<String>,
    }

    impl super::RootWatch for FakeWatch {
        fn watch_root(&mut self, path: &std::path::Path) -> Result<(), String> {
            let path = path.to_string_lossy().to_string();
            self.calls.push(("watch", path.clone()));
            if self.fail_watch.iter().any(|fail| fail == &path) {
                Err("share disconnected".into())
            } else {
                Ok(())
            }
        }

        fn unwatch_root(&mut self, path: &std::path::Path) -> Result<(), String> {
            self.calls.push(("unwatch", path.to_string_lossy().to_string()));
            Ok(())
        }
    }

    #[test]
    fn rearm_refreshes_each_root_and_an_offline_or_failed_root_does_not_stop_the_next() {
        let mut watcher = FakeWatch {
            fail_watch: vec!["flaky".into()],
            ..FakeWatch::default()
        };
        let mut folders = vec![
            "online-a".to_string(),
            "offline".to_string(),
            "online-b".to_string(),
            "flaky".to_string(),
        ];
        let mut norms = folders
            .iter()
            .map(|p| crate::pathutil::normalize_for_compare(p))
            .collect();
        // online-b was offline at startup, so it has no OS watch yet.
        let mut armed = vec![true, true, false, true];

        super::apply_cmd(
            &mut watcher,
            &mut folders,
            &mut norms,
            &mut armed,
            super::WatcherCmd::Add("online-a".into()),
        );
        assert!(
            watcher.calls.is_empty(),
            "adding a root that is already routed must not refresh it: {:?}",
            watcher.calls
        );

        super::apply_cmd(
            &mut watcher,
            &mut folders,
            &mut norms,
            &mut armed,
            super::WatcherCmd::Rearm {
                path: "offline".into(),
                online: false,
            },
        );
        super::apply_cmd(
            &mut watcher,
            &mut folders,
            &mut norms,
            &mut armed,
            super::WatcherCmd::Rearm {
                path: "flaky".into(),
                online: true,
            },
        );
        super::apply_cmd(
            &mut watcher,
            &mut folders,
            &mut norms,
            &mut armed,
            super::WatcherCmd::Rearm {
                path: "online-a".into(),
                online: true,
            },
        );
        super::apply_cmd(
            &mut watcher,
            &mut folders,
            &mut norms,
            &mut armed,
            super::WatcherCmd::Rearm {
                path: "online-b".into(),
                online: true,
            },
        );
        super::apply_cmd(
            &mut watcher,
            &mut folders,
            &mut norms,
            &mut armed,
            super::WatcherCmd::Rearm {
                path: "fresh".into(),
                online: true,
            },
        );

        assert_eq!(
            watcher.calls,
            vec![
                ("unwatch", "flaky".into()),
                ("watch", "flaky".into()),
                ("unwatch", "online-a".into()),
                ("watch", "online-a".into()),
                ("watch", "online-b".into()),
                ("watch", "fresh".into()),
            ]
        );
        assert!(
            armed[1],
            "a root that is not confirmed online keeps the watch it already has"
        );
        assert!(!armed[3], "a failed watch must not count as armed");
        assert!(armed[0] && armed[2]);
        assert_eq!(folders.last().map(String::as_str), Some("fresh"));
        assert!(armed.last().copied().unwrap_or(false));
        assert!(
            folders.iter().any(|p| p == "offline"),
            "an offline root stays registered so the next re-arm can try it"
        );
        assert!(folders.iter().any(|p| p == "flaky"));
    }
}
