use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use walkdir::WalkDir;

use crate::db::{Db, FolderRow};
use crate::extractor::{self, content_hash};
use crate::pathutil;
use crate::search::tantivy_backend::TantivyBackend;

pub struct Indexer {
    db: Arc<Db>,
    backend: Arc<TantivyBackend>,
}

impl Indexer {
    pub fn new(db: Arc<Db>, backend: Arc<TantivyBackend>) -> Self {
        Self { db, backend }
    }

    pub fn reindex_all(&self) -> Result<IndexStats, String> {
        // Full rebuild so removed folders / orphaned chunks disappear
        self.backend.clear_all()?;
        self.db.clear_all_files().map_err(|e| e.to_string())?;

        let folders = self.db.list_folders().map_err(|e| e.to_string())?;
        let excludes = self.load_excludes()?;

        let mut stats = IndexStats::default();
        for folder in folders.into_iter().filter(|f| f.enabled) {
            stats.merge(self.crawl_folder(&folder, &excludes));
        }
        Ok(stats)
    }

    /// Rebuild index for a single folder without touching other folders.
    pub fn reindex_folder(&self, folder_id: i64) -> Result<IndexStats, String> {
        let folder = self
            .db
            .get_folder(folder_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "フォルダが見つかりません".to_string())?;

        if !folder.enabled {
            return Err("フォルダが無効です".into());
        }

        // Purge only this folder's entries, then crawl
        let file_paths = self
            .db
            .list_file_paths_by_folder(folder_id)
            .map_err(|e| e.to_string())?;
        self.backend.delete_by_folder(&folder.path)?;
        self.backend.delete_paths(&file_paths)?;
        self.db
            .clear_files_by_folder(folder_id)
            .map_err(|e| e.to_string())?;

        let excludes = self.load_excludes()?;
        Ok(self.crawl_folder(&folder, &excludes))
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

    fn crawl_folder(&self, folder: &FolderRow, excludes: &[String]) -> IndexStats {
        let mut stats = IndexStats::default();
        let root = PathBuf::from(&folder.path);
        if !root.exists() {
            stats.errors += 1;
            return stats;
        }
        for entry in WalkDir::new(&root).into_iter().filter_map(|e| e.ok()) {
            let path = entry.path();
            if !path.is_file() || !extractor::is_supported(path) {
                continue;
            }
            if is_excluded(path, excludes) {
                continue;
            }
            match self.index_one(&folder.path, &folder.public_path, path) {
                Ok(IndexAction::Indexed) => stats.indexed += 1,
                Ok(IndexAction::Skipped) => stats.skipped += 1,
                Err(e) => {
                    eprintln!("index error {}: {e}", path.display());
                    stats.errors += 1;
                }
            }
        }
        stats
    }

    pub fn index_path(&self, folder: &str, path: &Path) -> Result<(), String> {
        let public_path = self
            .db
            .get_folder_by_path(folder)
            .map_err(|e| e.to_string())?
            .map(|f| f.public_path)
            .unwrap_or_default();
        self.index_one(folder, &public_path, path).map(|_| ())
    }

    pub fn remove_path(&self, path: &Path) -> Result<(), String> {
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

fn is_excluded(path: &Path, excludes: &[String]) -> bool {
    let s = pathutil::simplify_windows_path(&path.to_string_lossy());
    excludes.iter().any(|ex| {
        pathutil::path_starts_with(&s, &pathutil::simplify_windows_path(ex))
    })
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

enum IndexAction {
    Indexed,
    Skipped,
}
