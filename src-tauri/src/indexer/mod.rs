use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use walkdir::WalkDir;

use crate::db::{Db, FolderRow};
use crate::extractor::{self, content_hash};
use crate::pathutil;
use crate::search::tantivy_backend::TantivyBackend;

/// Minimum interval between intermediate `indexing` progress emits (~10 Hz).
const PROGRESS_EMIT_INTERVAL: Duration = Duration::from_millis(100);

pub struct Indexer {
    db: Arc<Db>,
    backend: Arc<TantivyBackend>,
    /// Blocks watcher updates while a full/folder rebuild holds the index.
    reindex_busy: AtomicBool,
}

impl Indexer {
    pub fn new(db: Arc<Db>, backend: Arc<TantivyBackend>) -> Self {
        Self {
            db,
            backend,
            reindex_busy: AtomicBool::new(false),
        }
    }

    fn begin_reindex(&self) -> Result<(), String> {
        if self
            .reindex_busy
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err("インデックス再構築が既に実行中です".into());
        }
        Ok(())
    }

    fn end_reindex(&self) {
        self.reindex_busy.store(false, Ordering::SeqCst);
    }

    pub fn reindex_all<F>(&self, mut on_progress: F) -> Result<IndexStats, String>
    where
        F: FnMut(IndexProgress),
    {
        self.begin_reindex()?;
        let result = (|| {
            // Full rebuild so removed folders / orphaned chunks disappear
            self.backend.clear_all()?;
            self.db.clear_all_files().map_err(|e| e.to_string())?;

            let folders = self.db.list_folders().map_err(|e| e.to_string())?;
            let excludes = self.load_excludes()?;

            let mut stats = IndexStats::default();
            for folder in folders.into_iter().filter(|f| f.enabled) {
                stats.merge(self.crawl_folder(&folder, &excludes, false, &mut on_progress));
            }
            Ok(stats)
        })();
        self.end_reindex();
        result
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
        self.begin_reindex()?;
        let result = (|| {
            let folder = self
                .db
                .get_folder(folder_id)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| "フォルダが見つかりません".to_string())?;

            if !folder.enabled {
                return Err("フォルダが無効です".into());
            }

            let excludes = self.load_excludes()?;
            Ok(self.crawl_folder(&folder, &excludes, true, &mut on_progress))
        })();
        self.end_reindex();
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
        if !root.exists() {
            stats.errors += 1;
            return stats;
        }

        on_progress(IndexProgress {
            folder_id: folder.id,
            current: 0,
            total: 0,
            phase: IndexPhase::Counting,
        });

        let total = count_indexable(&root, excludes);

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

        for entry in WalkDir::new(&root).into_iter().filter_map(|e| e.ok()) {
            let path = entry.path();
            if !is_indexable(path, excludes) {
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

        if purge_orphans {
            if let Err(e) = self.purge_folder_orphans(folder.id, &seen) {
                eprintln!("argos: purge orphans for folder {}: {e}", folder.id);
                stats.errors += 1;
            }
        }

        stats
    }

    /// Remove indexed paths for this folder that were not seen in the latest crawl.
    fn purge_folder_orphans(
        &self,
        folder_id: i64,
        seen: &std::collections::HashSet<String>,
    ) -> Result<(), String> {
        let existing = self
            .db
            .list_file_paths_by_folder(folder_id)
            .map_err(|e| e.to_string())?;
        let orphans: Vec<String> = existing
            .into_iter()
            .filter(|p| !seen.contains(p))
            .collect();
        if orphans.is_empty() {
            return Ok(());
        }
        self.backend.delete_paths(&orphans)?;
        for path in &orphans {
            self.db
                .mark_file_deleted(path)
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    pub fn index_path(&self, folder: &str, path: &Path) -> Result<IndexAction, String> {
        if self.reindex_busy.load(Ordering::SeqCst) {
            return Ok(IndexAction::Skipped);
        }
        if !path.is_file() || !extractor::is_supported(path) {
            return Ok(IndexAction::Skipped);
        }
        let excludes = self.load_excludes()?;
        if is_excluded(path, &excludes) {
            return Ok(IndexAction::Skipped);
        }
        let public_path = self
            .db
            .get_folder_by_path(folder)
            .map_err(|e| e.to_string())?
            .map(|f| f.public_path)
            .unwrap_or_default();
        self.index_one(folder, &public_path, path)
    }

    pub fn remove_path(&self, path: &Path) -> Result<(), String> {
        if self.reindex_busy.load(Ordering::SeqCst) {
            return Ok(());
        }
        let fs_str = pathutil::simplify_windows_path(&path.to_string_lossy());
        // Delete both filesystem form and any public-path rewritten form.
        let mut candidates = vec![fs_str.clone()];
        if let Ok(folders) = self.db.list_folders() {
            for folder in folders {
                let indexed = pathutil::to_indexed_path(&fs_str, &folder.path, &folder.public_path);
                if indexed != fs_str {
                    candidates.push(indexed);
                }
            }
        }
        for path_str in candidates {
            self.backend.delete_by_path(&path_str)?;
            self.db
                .mark_file_deleted(&path_str)
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
        self.begin_reindex()?;
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
        self.end_reindex();
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
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let bytes = fs::read(path).map_err(|e| e.to_string())?;
        let hash = content_hash(&bytes);

        let store_path =
            pathutil::to_indexed_path(&path.to_string_lossy(), folder, public_path);

        if let Ok(Some(existing)) = self.db.get_file_meta(&store_path) {
            if existing.size == size as i64
                && existing.mtime == mtime as i64
                && existing.content_hash == hash
            {
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
}

fn count_indexable(root: &Path, excludes: &[String]) -> u32 {
    let mut total = 0u32;
    for entry in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
        if is_indexable(entry.path(), excludes) {
            total = total.saturating_add(1);
        }
    }
    total
}

fn is_indexable(path: &Path, excludes: &[String]) -> bool {
    path.is_file() && extractor::is_supported(path) && !is_excluded(path, excludes)
}

fn is_excluded(path: &Path, excludes: &[String]) -> bool {
    let s = pathutil::simplify_windows_path(&path.to_string_lossy());
    excludes.iter().any(|ex| {
        pathutil::path_starts_with(&s, &pathutil::simplify_windows_path(ex))
    })
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

#[derive(Default, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexStats {
    pub indexed: u32,
    pub skipped: u32,
    pub errors: u32,
}

impl IndexStats {
    fn merge(&mut self, other: IndexStats) {
        self.indexed += other.indexed;
        self.skipped += other.skipped;
        self.errors += other.errors;
    }
}

pub enum IndexAction {
    Indexed,
    Skipped,
}
