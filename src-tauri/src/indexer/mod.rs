use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use parking_lot::{Condvar, Mutex};
use walkdir::WalkDir;

use crate::db::{Db, FolderRow};
use crate::extractor::{self, content_hash};
use crate::pathutil;
use crate::search::tantivy_backend::TantivyBackend;

/// A filesystem change observed by the watcher, or replayed after a reindex.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FsEvent {
    /// Path is gone (delete, or the old side of a rename/move). Drops the path and its children.
    Removed(PathBuf),
    /// Content or metadata changed. Directories are ignored; missing paths are removals.
    Changed(PathBuf),
    /// Path was created or is the new side of a rename/move. Directories are crawled.
    Appeared(PathBuf),
}

/// Minimum interval between intermediate `indexing` progress emits (~10 Hz).
const PROGRESS_EMIT_INTERVAL: Duration = Duration::from_millis(100);

/// `reindex_kind` while a startup diff holds the index.
const KIND_STARTUP: u8 = 1;
/// `reindex_kind` while a user reindex, folder load, or relink holds the index.
const KIND_USER: u8 = 2;

pub struct Indexer {
    db: Arc<Db>,
    backend: Arc<TantivyBackend>,
    /// Blocks watcher updates while a full/folder rebuild holds the index.
    reindex_busy: AtomicBool,
    /// `KIND_STARTUP` or `KIND_USER` while `reindex_busy` is set.
    reindex_kind: AtomicU8,
    /// Set when a user operation wants the startup diff to stop and get out of the way.
    cancel_startup: AtomicBool,
    /// Events that arrived while `reindex_busy` was set. Applied in `end_reindex`.
    pending_fs: Mutex<Vec<FsEvent>>,
    /// Held together with the busy-flag update. Waiters sleep on `reindex_cv`.
    reindex_gate: Mutex<()>,
    reindex_cv: Condvar,
    /// A user operation cancelled a startup sync that still had folders left.
    /// Consumed when that user operation finishes, then the app hook runs the rest.
    resync_after_user: AtomicBool,
    /// App hook that starts another startup sync. Empty in tests unless a test sets it.
    schedule_sync: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    /// One full reindex at a time. A second press waits and shares that result.
    reindex_all_flight: Mutex<Flight>,
    reindex_all_cv: Condvar,
    /// In-flight per-folder reindexes, keyed by folder id. Different ids both run.
    folder_flights: Mutex<HashMap<i64, Arc<OnceLock<Result<IndexStats, String>>>>>,
    folder_cv: Condvar,
    /// When set, a startup crawl pauses before it reads files until cancel or clear.
    startup_test_hold: AtomicBool,
    reindex_all_leads: AtomicUsize,
    reindex_all_followers: AtomicUsize,
    reindex_folder_followers: AtomicUsize,
}

struct Flight {
    running: bool,
    result: Arc<OnceLock<Result<IndexStats, String>>>,
}

enum Enter {
    Lead(Arc<OnceLock<Result<IndexStats, String>>>),
    Shared(Result<IndexStats, String>),
}

#[derive(Clone, Copy)]
enum CoalesceSlot {
    All,
    Folder(i64),
}

/// Publishes the leader's result if the crawl panics before it returns.
struct CoalesceGuard<'a> {
    indexer: &'a Indexer,
    slot: CoalesceSlot,
    cell: Arc<OnceLock<Result<IndexStats, String>>>,
    result: Option<Result<IndexStats, String>>,
}

impl Drop for CoalesceGuard<'_> {
    fn drop(&mut self) {
        let result = self
            .result
            .take()
            .unwrap_or_else(|| Err("インデックス再構築が中断されました".into()));
        self.indexer
            .publish_coalesced(self.slot, &self.cell, result);
    }
}

/// Clears the reindex flag on drop, including when the crawl panics, and replays
/// filesystem events that arrived while the flag was set.
struct ReindexGuard<'a> {
    indexer: &'a Indexer,
}

impl Drop for ReindexGuard<'_> {
    fn drop(&mut self) {
        self.indexer.end_reindex();
    }
}

impl Indexer {
    pub fn new(db: Arc<Db>, backend: Arc<TantivyBackend>) -> Self {
        Self {
            db,
            backend,
            reindex_busy: AtomicBool::new(false),
            reindex_kind: AtomicU8::new(0),
            cancel_startup: AtomicBool::new(false),
            pending_fs: Mutex::new(Vec::new()),
            reindex_gate: Mutex::new(()),
            reindex_cv: Condvar::new(),
            resync_after_user: AtomicBool::new(false),
            schedule_sync: Mutex::new(None),
            reindex_all_flight: Mutex::new(Flight {
                running: false,
                result: Arc::new(OnceLock::new()),
            }),
            reindex_all_cv: Condvar::new(),
            folder_flights: Mutex::new(HashMap::new()),
            folder_cv: Condvar::new(),
            startup_test_hold: AtomicBool::new(false),
            reindex_all_leads: AtomicUsize::new(0),
            reindex_all_followers: AtomicUsize::new(0),
            reindex_folder_followers: AtomicUsize::new(0),
        }
    }

    /// Run after a user operation preempts the startup sync, once the index is free.
    pub fn set_schedule_sync(&self, hook: impl Fn() + Send + Sync + 'static) {
        *self.schedule_sync.lock() = Some(Arc::new(hook));
    }

    /// Take the index. A user operation (`KIND_USER`) asks a running startup diff to
    /// stop, then waits until the flag is free. It does not cancel another user operation.
    fn begin_reindex(&self, kind: u8) -> ReindexGuard<'_> {
        loop {
            {
                let _gate = self.reindex_gate.lock();
                let _pending = self.pending_fs.lock();
                if !self.reindex_busy.load(Ordering::SeqCst) {
                    self.cancel_startup.store(false, Ordering::SeqCst);
                    self.reindex_kind.store(kind, Ordering::SeqCst);
                    self.reindex_busy.store(true, Ordering::SeqCst);
                    return ReindexGuard { indexer: self };
                }
                if kind == KIND_USER && self.reindex_kind.load(Ordering::SeqCst) == KIND_STARTUP {
                    self.cancel_startup.store(true, Ordering::SeqCst);
                    self.resync_after_user.store(true, Ordering::SeqCst);
                }
            }
            let mut gate = self.reindex_gate.lock();
            if self.reindex_busy.load(Ordering::SeqCst) {
                self.reindex_cv.wait(&mut gate);
            }
        }
    }

    fn startup_cancelled(&self) -> bool {
        self.reindex_kind.load(Ordering::SeqCst) == KIND_STARTUP
            && self.cancel_startup.load(Ordering::SeqCst)
    }

    fn end_reindex(&self) {
        // Replay must not observe the startup cancel flag and abandon a moved folder.
        self.cancel_startup.store(false, Ordering::SeqCst);
        let kind = self.reindex_kind.load(Ordering::SeqCst);
        loop {
            let batch = {
                let mut pending = self.pending_fs.lock();
                std::mem::take(&mut *pending)
            };
            if batch.is_empty() {
                let _gate = self.reindex_gate.lock();
                let pending = self.pending_fs.lock();
                if pending.is_empty() {
                    self.reindex_busy.store(false, Ordering::SeqCst);
                    self.reindex_kind.store(0, Ordering::SeqCst);
                    drop(pending);
                    drop(_gate);
                    self.reindex_cv.notify_all();
                    // The follow-up sync must start after this operation has released
                    // the gate. Scheduling it from the aborted startup would let the
                    // new sync take the gate and get cancelled again.
                    if kind == KIND_USER && self.resync_after_user.swap(false, Ordering::SeqCst) {
                        let hook = self.schedule_sync.lock().clone();
                        if let Some(hook) = hook {
                            hook();
                        }
                    }
                    return;
                }
                continue;
            }
            for event in batch {
                if let Err(e) = self.apply_fs_event_now(event) {
                    eprintln!("argos: deferred fs event failed: {e}");
                }
            }
        }
    }

    /// Queue `event` when a reindex owns the index. The caller must not apply it.
    fn defer_if_busy(&self, event: FsEvent) -> bool {
        let mut pending = self.pending_fs.lock();
        if self.reindex_busy.load(Ordering::SeqCst) {
            pending.push(event);
            true
        } else {
            false
        }
    }

    pub fn reindex_all<F>(&self, mut on_progress: F) -> Result<IndexStats, String>
    where
        F: FnMut(IndexProgress),
    {
        match self.enter_reindex_all() {
            Enter::Shared(stats) => stats,
            Enter::Lead(cell) => {
                self.reindex_all_leads.fetch_add(1, Ordering::SeqCst);
                let mut guard = CoalesceGuard {
                    indexer: self,
                    slot: CoalesceSlot::All,
                    cell,
                    result: None,
                };
                guard.result = Some(self.run_reindex_all(&mut on_progress));
                guard
                    .result
                    .clone()
                    .unwrap_or_else(|| Err("インデックス再構築が中断されました".into()))
            }
        }
    }

    fn run_reindex_all<F>(&self, on_progress: &mut F) -> Result<IndexStats, String>
    where
        F: FnMut(IndexProgress),
    {
        let _guard = self.begin_reindex(KIND_USER);
        // Full rebuild so removed folders / orphaned chunks disappear
        self.backend.clear_all()?;
        self.db.clear_all_files().map_err(|e| e.to_string())?;

        let folders = self.db.list_folders().map_err(|e| e.to_string())?;
        let excludes = self.load_excludes()?;

        let mut stats = IndexStats::default();
        for folder in folders.into_iter().filter(|f| f.enabled) {
            stats.merge(self.crawl_folder(&folder, &excludes, false, on_progress));
        }
        Ok(stats)
    }

    /// Resume-friendly reindex for one folder: keep existing docs, skip unchanged
    /// files, then drop paths that no longer exist (or are newly excluded).
    pub fn reindex_folder<F>(
        &self,
        folder_id: i64,
        mut on_progress: F,
    ) -> Result<IndexStats, String>
    where
        F: FnMut(IndexProgress),
    {
        match self.enter_reindex_folder(folder_id) {
            Enter::Shared(stats) => stats,
            Enter::Lead(cell) => {
                let mut guard = CoalesceGuard {
                    indexer: self,
                    slot: CoalesceSlot::Folder(folder_id),
                    cell,
                    result: None,
                };
                guard.result = Some(self.run_reindex_folder(folder_id, &mut on_progress));
                guard
                    .result
                    .clone()
                    .unwrap_or_else(|| Err("インデックス再構築が中断されました".into()))
            }
        }
    }

    fn run_reindex_folder<F>(
        &self,
        folder_id: i64,
        on_progress: &mut F,
    ) -> Result<IndexStats, String>
    where
        F: FnMut(IndexProgress),
    {
        let _guard = self.begin_reindex(KIND_USER);
        let folder = self
            .db
            .get_folder(folder_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "フォルダが見つかりません".to_string())?;

        if !folder.enabled {
            return Err("フォルダが無効です".into());
        }

        let excludes = self.load_excludes()?;
        Ok(self.crawl_folder(&folder, &excludes, true, on_progress))
    }

    /// Pick up creates, edits, deletes, and moves made while the watcher was not running.
    /// Same incremental crawl as a per-folder reindex: unchanged files are skipped, paths
    /// that are no longer on disk are dropped. Does not emit UI progress.
    /// A user reindex or relink asks this to stop so it does not hold the index.
    pub fn sync_enabled_folders(&self) -> Result<IndexStats, String> {
        let _guard = self.begin_reindex(KIND_STARTUP);
        let result = (|| {
            let folders = self.db.list_folders().map_err(|e| e.to_string())?;
            let excludes = self.load_excludes()?;
            let mut stats = IndexStats::default();
            for folder in folders.into_iter().filter(|f| f.enabled) {
                self.wait_if_startup_held();
                if self.startup_cancelled() {
                    eprintln!("argos: startup sync preempted by a user reindex or relink");
                    break;
                }
                stats.merge(self.crawl_folder(&folder, &excludes, true, &mut |_| {}));
            }
            Ok(stats)
        })();
        result
    }

    fn load_excludes(&self) -> Result<Vec<String>, String> {
        Ok(self
            .db
            .list_exclude_paths()
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|e| e.path)
            .collect())
    }

    fn crawl_folder<F>(
        &self,
        folder: &FolderRow,
        excludes: &[String],
        purge_orphans: bool,
        on_progress: &mut F,
    ) -> IndexStats
    where
        F: FnMut(IndexProgress),
    {
        let mut stats = IndexStats::default();
        let root = PathBuf::from(&folder.path);
        self.wait_if_startup_held();
        if self.startup_cancelled() {
            return stats;
        }
        // Missing, offline, or not a directory: do not treat "nothing listed" as "delete all".
        if !root.is_dir() {
            stats.errors += 1;
            eprintln!(
                "argos: skip crawl, folder is not a directory: {}",
                root.display()
            );
            return stats;
        }
        if self.startup_cancelled() {
            return stats;
        }

        on_progress(IndexProgress {
            folder_id: folder.id,
            current: 0,
            total: 0,
            phase: IndexPhase::Counting,
        });

        let total = count_indexable(&root, excludes, &|| self.startup_cancelled());
        if self.startup_cancelled() {
            return stats;
        }

        on_progress(IndexProgress {
            folder_id: folder.id,
            current: 0,
            total,
            phase: IndexPhase::Indexing,
        });

        let mut current: u32 = 0;
        let mut last_emit = Instant::now()
            .checked_sub(PROGRESS_EMIT_INTERVAL)
            .unwrap_or_else(Instant::now);
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut blocked: Vec<String> = Vec::new();
        let mut aborted = false;

        for entry in WalkDir::new(&root).into_iter() {
            if self.startup_cancelled() {
                aborted = true;
                break;
            }
            let entry = match entry {
                Ok(entry) => entry,
                Err(err) => {
                    stats.errors += 1;
                    eprintln!("argos: walk error under {}: {err}", root.display());
                    let failed = err.path().unwrap_or(root.as_path());
                    push_blocked(&mut blocked, failed, &folder.path, &folder.public_path);
                    continue;
                }
            };
            let path = entry.path();
            if !is_indexable(path, entry.file_type(), excludes) {
                continue;
            }
            let store_path =
                pathutil::to_indexed_path(&path.to_string_lossy(), &folder.path, &folder.public_path);
            seen.insert(store_path);
            match self.index_one(&folder.path, &folder.public_path, path) {
                Ok(IndexAction::Indexed) => stats.indexed += 1,
                Ok(IndexAction::Skipped) => stats.skipped += 1,
                Err(e) => {
                    eprintln!("index error {}: {e}", path.display());
                    stats.errors += 1;
                }
            }
            current += 1;
            let is_done = current >= total;
            let due = last_emit.elapsed() >= PROGRESS_EMIT_INTERVAL;
            if is_done || due {
                on_progress(IndexProgress {
                    folder_id: folder.id,
                    current,
                    total,
                    phase: IndexPhase::Indexing,
                });
                last_emit = Instant::now();
            }
        }

        // Ensure final n/N even when the last intermediate emit was throttled away
        // and total was 0 (empty folder): still report 0/0 once above.
        if total > 0 && current > 0 {
            on_progress(IndexProgress {
                folder_id: folder.id,
                current,
                total,
                phase: IndexPhase::Indexing,
            });
        }

        if aborted {
            eprintln!(
                "argos: startup sync preempted, skip orphan purge for folder {}",
                folder.id
            );
        } else if purge_orphans {
            match self.purge_folder_orphans(folder.id, &seen, &blocked) {
                Ok(n) => stats.removed += n,
                Err(e) => {
                    eprintln!("argos: purge orphans for folder {}: {e}", folder.id);
                    stats.errors += 1;
                }
            }
        }

        stats
    }

    /// Remove indexed paths for this folder that were not seen in the latest crawl.
    /// Paths under `blocked` were inside a directory the walk could not read, so they stay.
    fn purge_folder_orphans(
        &self,
        folder_id: i64,
        seen: &std::collections::HashSet<String>,
        blocked: &[String],
    ) -> Result<u32, String> {
        let existing = self
            .db
            .list_file_paths_by_folder(folder_id)
            .map_err(|e| e.to_string())?;
        let orphans: Vec<String> = existing
            .into_iter()
            .filter(|p| !seen.contains(p) && !path_is_blocked(p, blocked))
            .collect();
        let n = orphans.len() as u32;
        self.delete_indexed_paths(&orphans)?;
        Ok(n)
    }

    /// Apply one watcher event. Events that arrive during a reindex are queued, not dropped.
    pub fn handle_fs_event(&self, event: FsEvent) -> Result<bool, String> {
        if self.defer_if_busy(event.clone()) {
            return Ok(false);
        }
        self.apply_fs_event_now(event)
    }

    fn apply_fs_event_now(&self, event: FsEvent) -> Result<bool, String> {
        match event {
            FsEvent::Removed(path) => self.remove_tree_now(&path),
            FsEvent::Changed(path) => self.apply_changed(&path),
            FsEvent::Appeared(path) => self.apply_appeared(&path),
        }
    }

    fn apply_changed(&self, path: &Path) -> Result<bool, String> {
        if !path.exists() {
            return self.remove_tree_now(path);
        }
        if !path.is_file() {
            return Ok(false);
        }
        self.index_if_in_folder(path)
    }

    fn apply_appeared(&self, path: &Path) -> Result<bool, String> {
        if !path.exists() {
            return self.remove_tree_now(path);
        }
        if path.is_dir() {
            let Some(folder) = self.folder_for_path(path)? else {
                return Ok(false);
            };
            return self.index_tree(&folder, path);
        }
        self.index_if_in_folder(path)
    }

    fn index_if_in_folder(&self, path: &Path) -> Result<bool, String> {
        if !path.is_file() || !extractor::is_supported(path) {
            return Ok(false);
        }
        let excludes = self.load_excludes()?;
        if is_excluded(path, &excludes) {
            return Ok(false);
        }
        let Some(folder) = self.folder_for_path(path)? else {
            return Ok(false);
        };
        Ok(matches!(
            self.index_one(&folder.path, &folder.public_path, path)?,
            IndexAction::Indexed
        ))
    }

    /// Index every supported file under `dir`, then drop indexed descendants that are gone.
    fn index_tree(&self, folder: &FolderRow, dir: &Path) -> Result<bool, String> {
        let excludes = self.load_excludes()?;
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut blocked: Vec<String> = Vec::new();
        let mut changed = false;
        for entry in WalkDir::new(dir).into_iter() {
            let entry = match entry {
                Ok(entry) => entry,
                Err(err) => {
                    eprintln!("argos: walk error under {}: {err}", dir.display());
                    let failed = err.path().unwrap_or(dir);
                    push_blocked(&mut blocked, failed, &folder.path, &folder.public_path);
                    continue;
                }
            };
            let path = entry.path();
            if !is_indexable(path, entry.file_type(), &excludes) {
                continue;
            }
            let store_path =
                pathutil::to_indexed_path(&path.to_string_lossy(), &folder.path, &folder.public_path);
            seen.insert(store_path);
            match self.index_one(&folder.path, &folder.public_path, path)? {
                IndexAction::Indexed => changed = true,
                IndexAction::Skipped => {}
            }
        }
        let removed = self.purge_under_prefix(folder, dir, &seen, &blocked)?;
        Ok(changed || removed > 0)
    }

    fn purge_under_prefix(
        &self,
        folder: &FolderRow,
        dir: &Path,
        seen: &std::collections::HashSet<String>,
        blocked: &[String],
    ) -> Result<u32, String> {
        let raw = dir.to_string_lossy();
        let fs_str = pathutil::simplify_windows_path(&raw);
        let indexed_prefix =
            pathutil::to_indexed_path(&raw, &folder.path, &folder.public_path);
        let existing = self
            .db
            .list_file_paths_by_folder(folder.id)
            .map_err(|e| e.to_string())?;
        let orphans: Vec<String> = existing
            .into_iter()
            .filter(|stored| {
                !seen.contains(stored)
                    && !path_is_blocked(stored, blocked)
                    && (pathutil::path_starts_with(stored, &indexed_prefix)
                        || pathutil::path_starts_with(stored, &fs_str))
            })
            .collect();
        let n = orphans.len() as u32;
        self.delete_indexed_paths(&orphans)?;
        Ok(n)
    }

    fn folder_for_path(&self, path: &Path) -> Result<Option<FolderRow>, String> {
        let raw = path.to_string_lossy();
        let mut best: Option<FolderRow> = None;
        let mut best_len = 0usize;
        for folder in self.db.list_folders().map_err(|e| e.to_string())? {
            if !folder.enabled {
                continue;
            }
            if !pathutil::path_starts_with(&raw, &folder.path) {
                continue;
            }
            let len = pathutil::simplify_windows_path(&folder.path).len();
            if best.is_none() || len >= best_len {
                best_len = len;
                best = Some(folder);
            }
        }
        Ok(best)
    }

    /// Delete `path` and every indexed file under it (a renamed/removed folder).
    fn remove_tree_now(&self, path: &Path) -> Result<bool, String> {
        let raw = path.to_string_lossy();
        let fs_str = pathutil::simplify_windows_path(&raw);
        let folders = self.db.list_folders().map_err(|e| e.to_string())?;
        let mut doomed = Vec::new();
        push_unique_path(&mut doomed, fs_str.clone());
        let mut from_db = false;
        for folder in &folders {
            let indexed = pathutil::to_indexed_path(&raw, &folder.path, &folder.public_path);
            push_unique_path(&mut doomed, indexed.clone());
            if !(pathutil::path_starts_with(&raw, &folder.path)
                || pathutil::path_starts_with(&fs_str, &folder.path))
            {
                continue;
            }
            let existing = self
                .db
                .list_file_paths_by_folder(folder.id)
                .map_err(|e| e.to_string())?;
            for stored in existing {
                if pathutil::path_starts_with(&stored, &indexed)
                    || pathutil::path_starts_with(&stored, &fs_str)
                {
                    from_db = true;
                    push_unique_path(&mut doomed, stored);
                }
            }
        }
        if doomed.is_empty() {
            return Ok(false);
        }
        self.delete_indexed_paths(&doomed)?;
        Ok(from_db)
    }

    fn delete_indexed_paths(&self, paths: &[String]) -> Result<(), String> {
        if paths.is_empty() {
            return Ok(());
        }
        self.backend.delete_paths(paths)?;
        for path in paths {
            self.db
                .mark_file_deleted(path)
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// Rebind a registered folder to a new filesystem path without re-extracting content.
    pub fn rebind_folder_path(
        &self,
        folder_id: i64,
        new_path: &str,
    ) -> Result<FolderRow, String> {
        let _guard = self.begin_reindex(KIND_USER);
        let result = (|| {
            let folder = self
                .db
                .get_folder(folder_id)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| "フォルダが見つかりません".to_string())?;
            let new_path = pathutil::simplify_windows_path(new_path.trim());
            if new_path.is_empty() {
                return Err("フォルダパスが空です".into());
            }
            if !std::path::Path::new(&new_path).is_dir() {
                return Err("指定されたパスにフォルダがありません".into());
            }
            if folder.path.eq_ignore_ascii_case(&new_path) {
                return Ok(folder);
            }

            let old_indexed =
                pathutil::effective_public_root(&folder.path, &folder.public_path);
            let new_indexed =
                pathutil::effective_public_root(&new_path, &folder.public_path);

            // Tantivy first while docs still key off the old `folder` field.
            self.backend.remap_folder_prefix(
                &folder.path,
                &new_path,
                &old_indexed,
                &new_indexed,
            )?;

            self.db
                .remap_file_paths_prefix(folder_id, &old_indexed, &new_indexed)
                .map_err(|e| {
                    format!(
                        "インデックスのパス更新後に DB 更新へ失敗しました（{e}）。設定から「読込」で復旧を試してください"
                    )
                })?;

            let updated = self
                .db
                .update_folder_path(folder_id, &new_path)
                .map_err(|e| {
                    format!(
                        "インデックスは更新されましたがフォルダ登録の更新に失敗しました（{e}）。設定から「読込」で復旧を試してください"
                    )
                })?
                .ok_or_else(|| "フォルダが見つかりません".to_string())?;

            eprintln!(
                "argos: rebound folder '{}' -> '{}' (indexed root '{}' -> '{}')",
                folder.path, new_path, old_indexed, new_indexed
            );
            Ok(updated)
        })();
        result
    }

    fn index_one(
        &self,
        folder: &str,
        public_path: &str,
        path: &Path,
    ) -> Result<IndexAction, String> {
        let meta = fs::metadata(path).map_err(|e| e.to_string())?;
        let size = meta.len();
        // `None` means the timestamp could not be read. Treating that as 0 would
        // skip a later content change whenever the size stayed the same.
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|d| d.as_secs());
        let store_path =
            pathutil::to_indexed_path(&path.to_string_lossy(), folder, public_path);

        // Size and mtime are enough to skip the read. Startup walks every registered
        // file, including UNC folders, and hashing each one pegs the disk at logon.
        if let Ok(Some(existing)) = self.db.get_file_meta(&store_path) {
            if metadata_unchanged(existing.size, existing.mtime, size, mtime) {
                return Ok(IndexAction::Skipped);
            }
        }
        let mtime = mtime.unwrap_or(0);

        let bytes = fs::read(path).map_err(|e| e.to_string())?;
        let hash = content_hash(&bytes);

        if let Ok(Some(existing)) = self.db.get_file_meta(&store_path) {
            if existing.content_hash == hash {
                let ext = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("");
                let folder_id = self
                    .db
                    .folder_id_by_path(folder)
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| "folder not found".to_string())?;
                self.db
                    .upsert_file(
                        folder_id,
                        &store_path,
                        ext,
                        size as i64,
                        mtime as i64,
                        &hash,
                    )
                    .map_err(|e| e.to_string())?;
                return Ok(IndexAction::Skipped);
            }
        }

        let extracted = match extractor::extract_file(path) {
            Ok(doc) => doc,
            Err(e) if extractor::is_skippable_extract_error(&e) => {
                eprintln!("index skip {}: {e}", path.display());
                return Ok(IndexAction::Skipped);
            }
            Err(e) => return Err(e),
        };
        let _chunks = self
            .backend
            .index_file(path, &store_path, folder, mtime, size, &extracted)?;

        let folder_id = self
            .db
            .folder_id_by_path(folder)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "folder not found".to_string())?;

        self.db
            .upsert_file(
                folder_id,
                &store_path,
                path.extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or(""),
                size as i64,
                mtime as i64,
                &hash,
            )
            .map_err(|e| e.to_string())?;

        Ok(IndexAction::Indexed)
    }

    fn enter_reindex_all(&self) -> Enter {
        let mut flight = self.reindex_all_flight.lock();
        if flight.running {
            let cell = Arc::clone(&flight.result);
            self.reindex_all_followers.fetch_add(1, Ordering::SeqCst);
            while cell.get().is_none() {
                self.reindex_all_cv.wait(&mut flight);
            }
            return Enter::Shared(
                cell.get()
                    .expect("coalesced reindex published a result")
                    .clone(),
            );
        }
        let cell = Arc::new(OnceLock::new());
        flight.running = true;
        flight.result = Arc::clone(&cell);
        Enter::Lead(cell)
    }

    fn enter_reindex_folder(&self, folder_id: i64) -> Enter {
        let mut flights = self.folder_flights.lock();
        if let Some(cell) = flights.get(&folder_id).cloned() {
            self.reindex_folder_followers.fetch_add(1, Ordering::SeqCst);
            while cell.get().is_none() {
                self.folder_cv.wait(&mut flights);
            }
            return Enter::Shared(
                cell.get()
                    .expect("coalesced folder reindex published a result")
                    .clone(),
            );
        }
        let cell = Arc::new(OnceLock::new());
        flights.insert(folder_id, Arc::clone(&cell));
        Enter::Lead(cell)
    }

    fn publish_coalesced(
        &self,
        slot: CoalesceSlot,
        cell: &Arc<OnceLock<Result<IndexStats, String>>>,
        result: Result<IndexStats, String>,
    ) {
        let _ = cell.set(result);
        match slot {
            CoalesceSlot::All => {
                let mut flight = self.reindex_all_flight.lock();
                flight.running = false;
                self.reindex_all_cv.notify_all();
            }
            CoalesceSlot::Folder(folder_id) => {
                let mut flights = self.folder_flights.lock();
                if flights
                    .get(&folder_id)
                    .is_some_and(|current| Arc::ptr_eq(current, cell))
                {
                    flights.remove(&folder_id);
                }
                self.folder_cv.notify_all();
            }
        }
    }

    fn wait_if_startup_held(&self) {
        while self.startup_test_hold.load(Ordering::SeqCst)
            && self.reindex_kind.load(Ordering::SeqCst) == KIND_STARTUP
            && !self.cancel_startup.load(Ordering::SeqCst)
        {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn count_indexable(root: &Path, excludes: &[String], stop: &dyn Fn() -> bool) -> u32 {
    let mut total = 0u32;
    for entry in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
        if stop() {
            break;
        }
        if is_indexable(entry.path(), entry.file_type(), excludes) {
            total = total.saturating_add(1);
        }
    }
    total
}

fn push_blocked(blocked: &mut Vec<String>, raw: &Path, folder: &str, public_path: &str) {
    let text = raw.to_string_lossy();
    let fs = pathutil::simplify_windows_path(&text);
    let indexed = pathutil::to_indexed_path(&text, folder, public_path);
    for path in [fs, indexed] {
        if path.is_empty() || blocked.iter().any(|b| b.eq_ignore_ascii_case(&path)) {
            continue;
        }
        blocked.push(path);
    }
}

fn path_is_blocked(path: &str, blocked: &[String]) -> bool {
    blocked
        .iter()
        .any(|prefix| pathutil::path_starts_with(path, prefix))
}

/// `file_type` comes from the directory listing (`DirEntry::file_type`), which
/// does not stat again. A share that drops after the listing must not make a
/// file that was just listed look deleted.
fn is_indexable(path: &Path, file_type: fs::FileType, excludes: &[String]) -> bool {
    listing_is_file(path, file_type)
        && extractor::is_supported(path)
        && !is_excluded(path, excludes)
}

fn listing_is_file(path: &Path, file_type: fs::FileType) -> bool {
    if file_type.is_file() {
        return true;
    }
    // Symlinks are not files in the listing. Stat only those, so a normal file
    // never depends on a second stat that fails when the share drops.
    file_type.is_symlink() && path.is_file()
}

fn metadata_unchanged(
    existing_size: i64,
    existing_mtime: i64,
    size: u64,
    mtime: Option<u64>,
) -> bool {
    match mtime {
        Some(mtime) => existing_size == size as i64 && existing_mtime == mtime as i64,
        None => false,
    }
}

fn is_excluded(path: &Path, excludes: &[String]) -> bool {
    let s = pathutil::simplify_windows_path(&path.to_string_lossy());
    excludes.iter().any(|ex| {
        pathutil::path_starts_with(&s, &pathutil::simplify_windows_path(ex))
    })
}

fn push_unique_path(paths: &mut Vec<String>, path: String) -> bool {
    if path.is_empty() || paths.iter().any(|existing| existing.eq_ignore_ascii_case(&path)) {
        return false;
    }
    paths.push(path);
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum IndexPhase {
    Counting,
    Indexing,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexProgress {
    pub folder_id: i64,
    pub current: u32,
    pub total: u32,
    pub phase: IndexPhase,
}

#[derive(Debug, Default, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexStats {
    pub indexed: u32,
    pub skipped: u32,
    pub errors: u32,
    /// Paths dropped because they were not on disk at the end of a crawl.
    #[serde(default)]
    pub removed: u32,
}

impl IndexStats {
    fn merge(&mut self, other: IndexStats) {
        self.indexed += other.indexed;
        self.skipped += other.skipped;
        self.errors += other.errors;
        self.removed += other.removed;
    }
}

pub enum IndexAction {
    Indexed,
    Skipped,
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{path_is_blocked, KIND_STARTUP, KIND_USER};
    use super::*;
    use crate::db::Db;
    use crate::search::tantivy_backend::TantivyBackend;
    use crate::search::SearchBackend;

    struct Env {
        indexer: Option<Arc<Indexer>>,
        backend: Option<Arc<TantivyBackend>>,
        db: Option<Arc<Db>>,
        root: PathBuf,
        files: PathBuf,
    }

    impl Env {
        fn new(tag: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = std::env::temp_dir().join(format!("argos-index-{tag}-{nanos}"));
            let files = root.join("files");
            fs::create_dir_all(&files).unwrap();
            let db = Arc::new(Db::open(&root.join("argos.db")).unwrap());
            db.add_folder(files.to_str().unwrap(), "").unwrap();
            let backend = Arc::new(
                TantivyBackend::open(&root.join("index"))
                    .unwrap()
                    .backend,
            );
            let indexer = Arc::new(Indexer::new(db.clone(), backend.clone()));
            Self {
                indexer: Some(indexer),
                backend: Some(backend),
                db: Some(db),
                root,
                files,
            }
        }

        fn indexer(&self) -> &Indexer {
            self.indexer.as_ref().unwrap()
        }

        fn backend(&self) -> &TantivyBackend {
            self.backend.as_ref().unwrap()
        }

        fn write(&self, rel: &str, body: &str) -> PathBuf {
            let path = self.files.join(rel);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&path, body).unwrap();
            path
        }

        fn stored(&self, rel: &str) -> String {
            pathutil::to_indexed_path(
                &self.files.join(rel).to_string_lossy(),
                &self.files.to_string_lossy(),
                "",
            )
        }

        fn index_all(&self) {
            let id = self.db.as_ref().unwrap().list_folders().unwrap()[0].id;
            self.indexer().reindex_folder(id, |_| {}).unwrap();
        }

        fn paths_for(&self, query: &str) -> Vec<String> {
            self.backend()
                .search(query, 20, None, None, false)
                .unwrap()
                .into_iter()
                .map(|hit| hit.path)
                .collect()
        }
    }

    impl Drop for Env {
        fn drop(&mut self) {
            self.indexer.take();
            self.backend.take();
            self.db.take();
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn move_and_rename_drop_old_paths_and_index_new_ones() {
        let env = Env::new("move");
        env.write("keep.txt", "契約のメモ。このファイルは残す。追加の本文。");
        env.write("old.txt", "損害賠償の対象となる記述。契約にも触れる。");
        env.write("sub/inner.txt", "契約の下位フォルダにある文書。本文を足す。");
        env.write("sibling/other.txt", "契約の兄弟フォルダ。これは動かす対象ではない。");
        env.index_all();

        let old = env.files.join("old.txt");
        let renamed = env.files.join("renamed.txt");
        fs::rename(&old, &renamed).unwrap();
        let sub = env.files.join("sub");
        let moved = env.files.join("moved");
        fs::rename(&sub, &moved).unwrap();

        env.indexer()
            .handle_fs_event(FsEvent::Removed(old))
            .unwrap();
        env.indexer()
            .handle_fs_event(FsEvent::Appeared(renamed))
            .unwrap();
        env.indexer()
            .handle_fs_event(FsEvent::Removed(sub))
            .unwrap();
        env.indexer()
            .handle_fs_event(FsEvent::Appeared(moved))
            .unwrap();

        assert!(!env.backend().has_path(&env.stored("old.txt")).unwrap());
        assert!(!env.backend().has_path(&env.stored("sub/inner.txt")).unwrap());
        assert!(env.backend().has_path(&env.stored("renamed.txt")).unwrap());
        assert!(env.backend().has_path(&env.stored("moved/inner.txt")).unwrap());
        assert!(env.backend().has_path(&env.stored("keep.txt")).unwrap());
        assert!(env.backend().has_path(&env.stored("sibling/other.txt")).unwrap());

        let hits = env.paths_for("損害賠償");
        assert!(
            hits.iter().any(|p| p == &env.stored("renamed.txt")),
            "{hits:?}"
        );
        assert!(hits.iter().all(|p| p != &env.stored("old.txt")), "{hits:?}");

        let all = env.paths_for("契約");
        assert!(all.iter().any(|p| p == &env.stored("moved/inner.txt")), "{all:?}");
        assert!(all.iter().all(|p| p != &env.stored("sub/inner.txt")), "{all:?}");
        assert!(all.iter().any(|p| p == &env.stored("sibling/other.txt")), "{all:?}");
        assert!(all.iter().any(|p| p == &env.stored("keep.txt")), "{all:?}");
    }

    #[test]
    fn startup_sync_picks_up_changes_made_while_closed() {
        let env = Env::new("startup");
        env.write("keep.txt", "契約のメモ。このファイルは残す。追加の本文。");
        env.write("gone.txt", "慰謝料の請求について記載したメモ。本文を足す。");
        env.write("move.txt", "損害賠償の対象となる記述。契約にも触れる。");
        env.write("sub/inner.txt", "契約の下位フォルダにある文書。本文を足す。");
        env.index_all();

        fs::remove_file(env.files.join("gone.txt")).unwrap();
        fs::rename(env.files.join("move.txt"), env.files.join("moved.txt")).unwrap();
        fs::rename(env.files.join("sub"), env.files.join("sub2")).unwrap();
        fs::write(
            env.files.join("sub2/inner.txt"),
            "契約の下位フォルダにある文書。更新済の本文をここに書く。",
        )
        .unwrap();
        env.write("fresh.txt", "契約の新規ファイル。閉じていた間に追加した。");

        let stats = env.indexer().sync_enabled_folders().unwrap();
        assert!(stats.removed >= 1, "{stats:?}");
        assert!(stats.indexed >= 1, "{stats:?}");

        assert!(!env.backend().has_path(&env.stored("gone.txt")).unwrap());
        assert!(!env.backend().has_path(&env.stored("move.txt")).unwrap());
        assert!(!env.backend().has_path(&env.stored("sub/inner.txt")).unwrap());
        assert!(env.backend().has_path(&env.stored("moved.txt")).unwrap());
        assert!(env.backend().has_path(&env.stored("sub2/inner.txt")).unwrap());
        assert!(env.backend().has_path(&env.stored("fresh.txt")).unwrap());
        assert!(env.backend().has_path(&env.stored("keep.txt")).unwrap());

        let damages = env.paths_for("損害賠償");
        assert!(
            damages.iter().any(|p| p == &env.stored("moved.txt")),
            "{damages:?}"
        );
        assert!(damages.iter().all(|p| p != &env.stored("move.txt")));

        let condolence = env.paths_for("慰謝料");
        assert!(condolence.is_empty(), "{condolence:?}");

        let updated = env.paths_for("更新済");
        assert!(
            updated.iter().any(|p| p == &env.stored("sub2/inner.txt")),
            "{updated:?}"
        );
    }

    #[test]
    fn events_during_reindex_are_applied_afterwards() {
        let env = Env::new("defer");
        env.write("a.txt", "契約についての最初の文書。本文を足して長さを確保する。");
        env.index_all();
        assert!(env.backend().has_path(&env.stored("a.txt")).unwrap());

        let guard = env.indexer().begin_reindex(KIND_USER);
        let a = env.files.join("a.txt");
        let b = env.write("b.txt", "損害賠償について後から追加した文書。契約にも触れる。");
        fs::remove_file(&a).unwrap();
        env.indexer()
            .handle_fs_event(FsEvent::Removed(a))
            .unwrap();
        env.indexer()
            .handle_fs_event(FsEvent::Appeared(b))
            .unwrap();

        assert!(
            env.backend().has_path(&env.stored("a.txt")).unwrap(),
            "queued remove must wait until the reindex finishes"
        );
        assert!(
            !env.backend().has_path(&env.stored("b.txt")).unwrap(),
            "queued add must wait until the reindex finishes"
        );

        drop(guard);
        assert!(!env.backend().has_path(&env.stored("a.txt")).unwrap());
        assert!(env.backend().has_path(&env.stored("b.txt")).unwrap());
        let hits = env.paths_for("損害賠償");
        assert!(hits.iter().any(|p| p == &env.stored("b.txt")), "{hits:?}");
    }

    #[test]
    fn startup_skips_files_whose_size_and_mtime_match() {
        let env = Env::new("mtime");
        let body = "契約について記載したメモです。追加。";
        let path = env.write("a.txt", body);
        env.index_all();

        let again = env.indexer().sync_enabled_folders().unwrap();
        assert_eq!(again.indexed, 0, "{again:?}");
        assert!(again.skipped >= 1, "{again:?}");

        // Same length, restored mtime: the bytes differ, but startup must not read them.
        let rewritten = "別件について記載したメモです。追加。";
        assert_eq!(body.len(), rewritten.len());
        let mtime = fs::metadata(&path).unwrap().modified().unwrap();
        fs::write(&path, rewritten).unwrap();
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(mtime)
            .unwrap();
        let skipped = env.indexer().sync_enabled_folders().unwrap();
        assert_eq!(skipped.indexed, 0, "{skipped:?}");
        assert!(env.paths_for("契約").iter().any(|p| p == &env.stored("a.txt")));
        assert!(env.paths_for("別件").is_empty(), "same size and mtime must not reindex");

        let later = mtime + Duration::from_secs(10);
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(later)
            .unwrap();
        let updated = env.indexer().sync_enabled_folders().unwrap();
        assert!(updated.indexed >= 1, "{updated:?}");
        assert!(env.paths_for("別件").iter().any(|p| p == &env.stored("a.txt")));
    }

    #[test]
    fn startup_sync_leaves_an_offline_folder_in_the_index() {
        let env = Env::new("offline");
        env.write("keep.txt", "契約のメモ。このファイルは残す。追加の本文。");
        env.index_all();
        let aside = env.root.join("aside");
        fs::rename(&env.files, &aside).unwrap();
        let stats = env.indexer().sync_enabled_folders().unwrap();
        assert_eq!(stats.removed, 0, "{stats:?}");
        assert!(stats.errors >= 1, "{stats:?}");
        assert!(env.backend().has_path(&env.stored("keep.txt")).unwrap());
        fs::rename(&aside, &env.files).unwrap();
    }

    #[test]
    fn blocked_prefix_keeps_only_the_unreadable_subtree() {
        let blocked = vec![r"C:\cases\locked".to_string()];
        assert!(path_is_blocked(r"C:\cases\locked\secret.txt", &blocked));
        assert!(!path_is_blocked(r"C:\cases\gone.txt", &blocked));
        assert!(!path_is_blocked(r"C:\cases\locked2\a.txt", &blocked));
    }

    #[test]
    fn unknown_mtime_is_not_treated_as_unchanged() {
        assert!(metadata_unchanged(4, 10, 4, Some(10)));
        assert!(!metadata_unchanged(4, 10, 4, Some(11)));
        assert!(!metadata_unchanged(4, 10, 5, Some(10)));
        assert!(!metadata_unchanged(4, 0, 4, None));
        assert!(!metadata_unchanged(4, 10, 4, None));
        // A real timestamp of 0 is still a timestamp. Only `None` means unknown.
        assert!(metadata_unchanged(4, 0, 4, Some(0)));
    }

    #[test]
    fn directory_listing_type_is_what_makes_a_path_indexable() {
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("argos-ftype-{nanos}"));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.txt");
        fs::write(&file, "契約").unwrap();
        let file_type = fs::metadata(&file).unwrap().file_type();
        let dir_type = fs::metadata(&dir).unwrap().file_type();
        assert!(is_indexable(&file, file_type, &[]));
        // The path still names a file. A directory type from the listing must not
        // be replaced by a fresh stat, or a dropped share looks like a deletion.
        assert!(!is_indexable(&file, dir_type, &[]));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn in_progress_startup_sync_stops_when_preempted() {
        let env = Env::new("preempt");
        env.write("keep.txt", "契約のメモ。このファイルは残す。追加の本文。");
        env.index_all();
        let indexer = Arc::clone(env.indexer.as_ref().unwrap());
        indexer.startup_test_hold.store(true, Ordering::SeqCst);
        let _release = ReleaseStartupHold(&indexer);

        let worker = Arc::clone(&indexer);
        let sync_thread = std::thread::spawn(move || worker.sync_enabled_folders());
        wait_until(
            Duration::from_secs(5),
            "startup sync did not take the index",
            || indexer.reindex_busy.load(Ordering::SeqCst),
        );

        let id = env.db.as_ref().unwrap().list_folders().unwrap()[0].id;
        env.indexer().reindex_folder(id, |_| {}).unwrap();
        let stats = sync_thread.join().unwrap().unwrap();
        assert_eq!(stats.indexed, 0, "{stats:?}");
        assert_eq!(stats.skipped, 0, "{stats:?}");
        assert_eq!(stats.removed, 0, "{stats:?}");
        assert!(env.backend().has_path(&env.stored("keep.txt")).unwrap());
    }

    #[test]
    fn preempted_startup_sync_runs_again_after_the_user_op() {
        let env = Env::new("resched");
        env.write("a.txt", "契約のメモ。本文を足して長さを確保する。");
        env.index_all();
        let indexer = Arc::clone(env.indexer.as_ref().unwrap());
        indexer.startup_test_hold.store(true, Ordering::SeqCst);
        let _release = ReleaseStartupHold(&indexer);

        let worker = Arc::clone(&indexer);
        let sync_thread = std::thread::spawn(move || worker.sync_enabled_folders());
        wait_until(
            Duration::from_secs(5),
            "startup sync did not take the index",
            || indexer.reindex_busy.load(Ordering::SeqCst),
        );

        let follow = Arc::clone(&indexer);
        let (tx, rx) = std::sync::mpsc::channel();
        indexer.set_schedule_sync(move || {
            follow.startup_test_hold.store(false, Ordering::SeqCst);
            let _ = tx.send(follow.sync_enabled_folders());
        });

        let id = env.db.as_ref().unwrap().list_folders().unwrap()[0].id;
        let user_indexer = Arc::clone(&indexer);
        let user = std::thread::spawn(move || user_indexer.reindex_folder(id, |_| {}));
        let follow_stats = rx
            .recv_timeout(Duration::from_secs(30))
            .expect("startup sync should run again after the user operation releases the index")
            .unwrap();
        assert!(
            follow_stats.skipped >= 1 || follow_stats.indexed >= 1,
            "{follow_stats:?}"
        );
        assert_eq!(follow_stats.removed, 0, "{follow_stats:?}");

        let preempted = sync_thread.join().unwrap().unwrap();
        assert_eq!(
            (preempted.indexed, preempted.skipped, preempted.removed),
            (0, 0, 0),
            "{preempted:?}"
        );
        user.join().unwrap().unwrap();

        let again = env.indexer().reindex_folder(id, |_| {}).unwrap();
        assert_eq!(
            indexer.reindex_all_followers.load(Ordering::SeqCst),
            0,
            "a folder reindex must not be counted as a full-reindex follower"
        );
        assert!(again.skipped >= 1, "{again:?}");
    }

    #[test]
    fn double_press_reindex_all_runs_once() {
        let env = Env::new("coalesce-all");
        env.write("a.txt", "契約のメモ。本文を足して長さを確保する。");
        let indexer = Arc::clone(env.indexer.as_ref().unwrap());
        let hold = indexer.begin_reindex(KIND_USER);
        let first = Arc::clone(&indexer);
        let second = Arc::clone(&indexer);
        let (tx1, rx1) = std::sync::mpsc::channel();
        let (tx2, rx2) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx1.send(first.reindex_all(|_| {}));
        });
        std::thread::spawn(move || {
            let _ = tx2.send(second.reindex_all(|_| {}));
        });
        wait_until(
            Duration::from_secs(5),
            "the second full reindex did not join the first",
            || indexer.reindex_all_followers.load(Ordering::SeqCst) >= 1,
        );
        assert_eq!(indexer.reindex_all_leads.load(Ordering::SeqCst), 1);
        drop(hold);

        let a = rx1.recv_timeout(Duration::from_secs(30)).unwrap().unwrap();
        let b = rx2.recv_timeout(Duration::from_secs(30)).unwrap().unwrap();
        assert_eq!(a.indexed, b.indexed);
        assert!(a.indexed >= 1, "{a:?}");
        assert_eq!(indexer.reindex_all_leads.load(Ordering::SeqCst), 1);

        env.indexer().reindex_all(|_| {}).unwrap();
        assert_eq!(
            indexer.reindex_all_leads.load(Ordering::SeqCst),
            2,
            "a press after the shared run finishes must start a new reindex"
        );
    }

    #[test]
    fn double_press_of_one_folder_shares_one_crawl_and_another_folder_does_not() {
        let env = Env::new("coalesce-folder");
        env.write("a.txt", "契約のメモ。本文を足して長さを確保する。");
        let db = env.db.as_ref().unwrap();
        let id_a = db.list_folders().unwrap()[0].id;
        let other = env.root.join("other");
        fs::create_dir_all(&other).unwrap();
        fs::write(
            other.join("b.txt"),
            "契約の別フォルダ。本文を足して長さを確保する。",
        )
        .unwrap();
        fs::write(
            other.join("c.txt"),
            "契約のさらに別の文書。本文を足して長さを確保する。",
        )
        .unwrap();
        let id_b = db.add_folder(other.to_str().unwrap(), "").unwrap().id;

        let indexer = Arc::clone(env.indexer.as_ref().unwrap());
        let hold = indexer.begin_reindex(KIND_USER);
        let first = Arc::clone(&indexer);
        let second = Arc::clone(&indexer);
        let (tx1, rx1) = std::sync::mpsc::channel();
        let (tx2, rx2) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx1.send(first.reindex_folder(id_a, |_| {}));
        });
        std::thread::spawn(move || {
            let _ = tx2.send(second.reindex_folder(id_a, |_| {}));
        });
        wait_until(
            Duration::from_secs(5),
            "the second folder reindex did not join the first",
            || indexer.reindex_folder_followers.load(Ordering::SeqCst) >= 1,
        );
        drop(hold);
        let a = rx1.recv_timeout(Duration::from_secs(30)).unwrap().unwrap();
        let b = rx2.recv_timeout(Duration::from_secs(30)).unwrap().unwrap();
        assert_eq!(a.indexed, b.indexed);
        assert_eq!(a.indexed, 1, "{a:?}");

        let hold = indexer.begin_reindex(KIND_USER);
        let left = Arc::clone(&indexer);
        let right = Arc::clone(&indexer);
        let (txa, rxa) = std::sync::mpsc::channel();
        let (txb, rxb) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = txa.send(left.reindex_folder(id_a, |_| {}));
        });
        wait_until(Duration::from_secs(5), "folder A did not start", || {
            indexer.folder_flights.lock().contains_key(&id_a)
        });
        std::thread::spawn(move || {
            let _ = txb.send(right.reindex_folder(id_b, |_| {}));
        });
        wait_until(
            Duration::from_secs(5),
            "folder B did not start its own crawl",
            || indexer.folder_flights.lock().contains_key(&id_b),
        );
        let followers = indexer.reindex_folder_followers.load(Ordering::SeqCst);
        drop(hold);
        let stats_a = rxa.recv_timeout(Duration::from_secs(30)).unwrap().unwrap();
        let stats_b = rxb.recv_timeout(Duration::from_secs(30)).unwrap().unwrap();
        assert_eq!(stats_a.skipped, 1, "{stats_a:?}");
        assert_eq!(stats_b.indexed, 2, "{stats_b:?}");
        assert_eq!(
            indexer.reindex_folder_followers.load(Ordering::SeqCst),
            followers,
            "a different folder must not share the in-flight result"
        );
    }

    /// The directory has to be unlistable. Wine can do that with `start /unix`
    /// and chmod; a normal Windows process cannot, so the default run ignores
    /// this test instead of reporting success without checking the purge.
    #[test]
    #[ignore = "needs Wine `cmd /c start /unix /bin/chmod` or a Windows ACL deny"]
    fn unreadable_subdirectory_is_not_purged_and_a_real_delete_still_is() {
        let env = Env::new("walkerr");
        env.write("keep.txt", "契約のメモ。このファイルは残す。追加の本文。");
        env.write("gone.txt", "慰謝料の請求について記載したメモ。本文を足す。");
        env.write(
            "locked/secret.txt",
            "損害賠償は読めないフォルダの中。本文を足す。",
        );
        env.index_all();
        fs::remove_file(env.files.join("gone.txt")).unwrap();

        let locked = env.files.join("locked");
        if !deny_directory_listing(&locked) {
            if wine_can_launch_unix_program() {
                panic!(
                    "Wine started a Unix program but could not revoke listing of {}",
                    locked.display()
                );
            }
            eprintln!(
                "ignored: unreadable_subdirectory_is_not_purged: directory listing could not be revoked (Wine start /unix chmod is unavailable, and this process has no ACL deny)"
            );
            return;
        }
        let _restore = RestoreListing(&locked);
        assert!(
            fs::read_dir(&locked).is_err(),
            "listing the locked directory should fail"
        );

        let stats = env.indexer().sync_enabled_folders().unwrap();
        assert!(stats.removed >= 1, "{stats:?}");
        assert!(stats.errors >= 1, "{stats:?}");
        assert!(!env.backend().has_path(&env.stored("gone.txt")).unwrap());
        assert!(env.backend().has_path(&env.stored("locked/secret.txt")).unwrap());
        assert!(env.backend().has_path(&env.stored("keep.txt")).unwrap());
        let hits = env.paths_for("損害賠償");
        assert!(
            hits.iter().any(|p| p == &env.stored("locked/secret.txt")),
            "{hits:?}"
        );
    }

    #[test]
    fn user_reindex_waits_for_startup_instead_of_failing() {
        let env = Env::new("wait");
        env.write("a.txt", "契約についての最初の文書。本文を足して長さを確保する。");
        env.index_all();
        let indexer = Arc::clone(env.indexer.as_ref().unwrap());
        let guard = indexer.begin_reindex(KIND_STARTUP);
        let id = env.db.as_ref().unwrap().list_folders().unwrap()[0].id;
        let worker = Arc::clone(&indexer);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(worker.reindex_folder(id, |_| {}));
        });
        assert!(
            rx.recv_timeout(Duration::from_millis(400)).is_err(),
            "a user reindex must wait while the startup diff holds the index"
        );
        drop(guard);
        rx.recv_timeout(Duration::from_secs(30))
            .expect("user reindex should run after startup releases the index")
            .expect("user reindex should succeed");
    }

    #[test]
    fn panic_during_reindex_releases_the_index() {
        let env = Env::new("panic");
        env.write("a.txt", "契約のメモ。本文を足して長さを確保する。");
        let indexer = Arc::clone(env.indexer.as_ref().unwrap());
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = indexer.begin_reindex(KIND_USER);
            panic!("boom");
        }));
        assert!(caught.is_err());
        let id = env.db.as_ref().unwrap().list_folders().unwrap()[0].id;
        env.indexer().reindex_folder(id, |_| {}).unwrap();
    }

    #[test]
    fn case_only_rename_drops_the_old_spelling() {
        let env = Env::new("case");
        let path = env.write("Note.txt", "契約の大文字ファイル。本文を足して長さを確保する。");
        env.index_all();
        let old = env.stored("Note.txt");
        let renamed = env.files.join("note.txt");
        let tmp = env.files.join("note-tmp.txt");
        fs::rename(&path, &tmp).unwrap();
        fs::rename(&tmp, &renamed).unwrap();
        env.indexer()
            .handle_fs_event(FsEvent::Removed(path))
            .unwrap();
        env.indexer()
            .handle_fs_event(FsEvent::Appeared(renamed))
            .unwrap();
        let new_path = env.stored("note.txt");
        assert_ne!(old, new_path);
        assert!(!env.backend().has_path(&old).unwrap());
        assert!(env.backend().has_path(&new_path).unwrap());
        let hits = env.paths_for("契約");
        assert!(hits.iter().any(|p| p == &new_path), "{hits:?}");
        assert!(hits.iter().all(|p| p != &old), "{hits:?}");
    }

    #[test]
    fn public_path_is_what_a_scoped_and_remote_search_sees() {
        let env = Env::new("public");
        let db = env.db.as_ref().unwrap();
        let folder = db.list_folders().unwrap().into_iter().next().unwrap();
        db.update_folder_public_path(folder.id, r"\\server\share\cases")
            .unwrap();
        env.write("memo.txt", "契約の公開パスにある文書。本文を足して長さを確保する。");
        let private_dir = env.root.join("private");
        fs::create_dir_all(private_dir.join("nested")).unwrap();
        fs::write(
            private_dir.join("nested").join("secret.txt"),
            "契約の非共有文書。本文を足して長さを確保する。",
        )
        .unwrap();
        let private = db.add_folder(private_dir.to_str().unwrap(), "").unwrap();
        env.indexer().reindex_folder(folder.id, |_| {}).unwrap();
        env.indexer().reindex_folder(private.id, |_| {}).unwrap();

        let stored = pathutil::to_indexed_path(
            &env.files.join("memo.txt").to_string_lossy(),
            &folder.path,
            r"\\server\share\cases",
        );
        assert!(
            stored.starts_with(r"\\server\share\cases\"),
            "{stored}"
        );
        assert!(env.backend().has_path(&stored).unwrap());

        for prefix in [
            r"\\server\share\cases",
            r"\\?\UNC\server\share\cases",
            r"//server/share/Cases",
        ] {
            let hits = env
                .backend()
                .search("契約", 10, Some(prefix), None, false)
                .unwrap();
            assert!(
                hits.iter().any(|h| h.path == stored),
                "scope {prefix}: {:?}",
                hits.iter().map(|h| &h.path).collect::<Vec<_>>()
            );
            assert!(
                hits.iter().all(|h| !h.path.contains("secret")),
                "scope {prefix} leaked the private file: {:?}",
                hits.iter().map(|h| &h.path).collect::<Vec<_>>()
            );
        }

        let drive = env
            .backend()
            .search("契約", 10, Some(&folder.path), None, false)
            .unwrap();
        assert!(
            drive.iter().all(|h| h.path != stored),
            "a drive-letter scope does not match the public UNC path: {drive:?}"
        );

        let snap = crate::search::RemoteShareSnapshot {
            folders: vec![crate::search::remote_share::RemoteShareFolder {
                id: folder.id,
                path: pathutil::simplify_windows_path(&folder.path),
                public_path: r"\\server\share\cases".into(),
            }],
            shared_ids: std::iter::once(folder.id).collect(),
        };
        let remote = env
            .backend()
            .search_for_remote(
                "契約",
                10,
                Some(r"\\server\share\cases"),
                None,
                false,
                crate::search::SearchOpts::default(),
                None,
                &snap,
            )
            .unwrap();
        assert!(remote.iter().any(|h| h.path == stored), "{remote:?}");
        assert!(remote.iter().all(|h| !h.path.contains("secret")), "{remote:?}");

        let hidden = env
            .backend()
            .search_for_remote(
                "契約",
                10,
                None,
                None,
                false,
                crate::search::SearchOpts::default(),
                None,
                &snap,
            )
            .unwrap();
        assert!(hidden.iter().any(|h| h.path == stored), "{hidden:?}");
        assert!(
            hidden.iter().all(|h| !h.path.contains("secret")),
            "unscoped remote search must hide the unshared folder: {hidden:?}"
        );
    }

    #[test]
    fn rename_and_relink_do_not_delete_notes_or_chat_sources() {
        let env = Env::new("notes");
        let path = env.write("a.txt", "契約のメモ。本文を足して長さを確保する。");
        env.index_all();
        let db = env.db.as_ref().unwrap();
        let stored = env.stored("a.txt");
        let note = db.create_note("検討メモ").unwrap();
        db.insert_note_item(&note.id, "契約", "p1", &format!("{{\"path\":\"{stored}\"}}"))
            .unwrap();
        let thread = db.create_llm_thread("会話", true).unwrap();
        db.insert_llm_source(&thread.id, "attach", &stored, "a", "p1", "引用本文", "契約")
            .unwrap();

        let renamed = env.files.join("b.txt");
        fs::rename(&path, &renamed).unwrap();
        env.indexer()
            .handle_fs_event(FsEvent::Removed(path))
            .unwrap();
        env.indexer()
            .handle_fs_event(FsEvent::Appeared(renamed))
            .unwrap();
        env.indexer().sync_enabled_folders().unwrap();

        assert_eq!(db.list_note_items(&note.id).unwrap().len(), 1);
        let sources = db.list_llm_sources(&thread.id).unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].body, "引用本文");
        assert_eq!(sources[0].path, stored);

        let dest = env.root.join("relinked");
        fs::create_dir_all(&dest).unwrap();
        fs::copy(env.files.join("b.txt"), dest.join("b.txt")).unwrap();
        let id = db.list_folders().unwrap()[0].id;
        env.indexer()
            .rebind_folder_path(id, dest.to_str().unwrap())
            .unwrap();
        assert_eq!(db.list_note_items(&note.id).unwrap().len(), 1);
        assert_eq!(db.list_llm_sources(&thread.id).unwrap()[0].body, "引用本文");
    }

    fn wait_until(limit: Duration, message: &str, mut ready: impl FnMut() -> bool) {
        let deadline = Instant::now() + limit;
        while !ready() {
            assert!(Instant::now() < deadline, "{message}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    struct ReleaseStartupHold<'a>(&'a Indexer);

    impl Drop for ReleaseStartupHold<'_> {
        fn drop(&mut self) {
            self.0.startup_test_hold.store(false, Ordering::SeqCst);
            self.0.cancel_startup.store(true, Ordering::SeqCst);
        }
    }

    fn deny_directory_listing(dir: &Path) -> bool {
        wine_chmod(dir, "000")
    }

    fn wine_chmod(dir: &Path, mode: &str) -> bool {
        for unix in wine_drive_c_candidates(dir) {
            let launched = std::process::Command::new("cmd")
                .args(["/c", "start", "/unix", "/bin/chmod", mode, &unix])
                .status()
                .map(|status| status.success())
                .unwrap_or(false);
            if !launched {
                continue;
            }
            std::thread::sleep(Duration::from_millis(300));
            if mode == "000" {
                if fs::read_dir(dir).is_err() {
                    return true;
                }
                continue;
            }
            return true;
        }
        false
    }

    fn wine_drive_c_candidates(dir: &Path) -> Vec<String> {
        let text = dir.to_string_lossy().replace('/', "\\");
        let Some(rest) = text
            .strip_prefix(r"C:\")
            .or_else(|| text.strip_prefix(r"c:\"))
        else {
            return Vec::new();
        };
        let rel = rest.replace('\\', "/");
        let mut out = Vec::new();
        if let Some(prefix) = std::env::var("WINEPREFIX")
            .ok()
            .filter(|prefix| !prefix.is_empty())
        {
            out.push(format!("{prefix}/drive_c/{rel}"));
        }
        if let Some(home) = std::env::var("HOME").ok().filter(|home| home.starts_with('/')) {
            out.push(format!("{home}/.wine/drive_c/{rel}"));
        }
        if let Ok(user) = std::env::var("USER").or_else(|_| std::env::var("USERNAME")) {
            if !user.is_empty() {
                out.push(format!("/home/{user}/.wine/drive_c/{rel}"));
            }
        }
        out
    }

    fn wine_can_launch_unix_program() -> bool {
        std::process::Command::new("cmd")
            .args(["/c", "start", "/unix", "/bin/true"])
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    struct RestoreListing<'a>(&'a Path);

    impl Drop for RestoreListing<'_> {
        fn drop(&mut self) {
            let _ = wine_chmod(self.0, "755");
        }
    }
}
