use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use crate::db::{Db, Settings};
use crate::indexer::Indexer;
use crate::mail::MailStaHandle;
use crate::remote_server::RemoteServerHandle;
use crate::search::tantivy_backend::TantivyBackend;
use crate::search::UserDictMatcher;
use crate::watcher::WatcherHandle;

pub struct LlmJob {
    pub request_id: String,
    pub thread_id: String,
    pub kind: String,
    pub cancel: Arc<AtomicBool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewTarget {
    pub origin: String,
    pub path: String,
    #[serde(default)]
    pub paragraph_id: Option<String>,
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub highlight_terms: Option<Vec<String>>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub fallback_body: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub source_id: Option<String>,
}

pub struct AppState {
    pub db: Arc<Db>,
    pub settings: RwLock<Settings>,
    pub data_dir: PathBuf,
    pub backend: Arc<TantivyBackend>,
    /// Dedicated Outlook mail index (`index-mail/`). Independent of file reindex.
    pub mail_backend: Arc<TantivyBackend>,
    pub indexer: Arc<Indexer>,
    pub remote_server: RemoteServerHandle,
    /// Set during app setup after the FS watcher thread starts.
    pub watcher: RwLock<Option<WatcherHandle>>,
    /// Client-side user dictionary for query phrase forcing.
    pub user_dict: RwLock<UserDictMatcher>,
    /// Outlook Classic COM worker (STA). Always present; errors if Outlook missing.
    pub mail: MailStaHandle,
    pub llm_job: RwLock<Option<LlmJob>>,
    pub preview_target: RwLock<Option<PreviewTarget>>,
}

impl AppState {
    /// Returns `(state, needs_full_reindex)` when the on-disk *file* index schema was wiped.
    pub fn open() -> Result<(Self, bool), String> {
        let data_dir = dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Argos");
        std::fs::create_dir_all(&data_dir).map_err(|e| e.to_string())?;
        let db_path = data_dir.join("argos.db");
        let db = Arc::new(Db::open(&db_path).map_err(|e| e.to_string())?);
        let mut settings = db.load_settings();
        crate::search::ensure_server_token(&mut settings);
        let _ = db.save_settings(&settings);

        let index_dir = data_dir.join("index");
        let opened = TantivyBackend::open(&index_dir)?;
        let backend = Arc::new(opened.backend);

        let mail_index_dir = data_dir.join("index-mail");
        let mail_opened = TantivyBackend::open_mail(&mail_index_dir)?;
        let mail_backend = Arc::new(mail_opened.backend);
        // Schema wipe, or migration to a fresh empty index-mail while SQLite still
        // thinks messages are indexed (would cause sync to skip everything).
        let mail_db_indexed = db.count_indexed_emails().unwrap_or(0);
        let mail_needs_resync =
            mail_opened.needs_full_reindex || (mail_db_indexed > 0 && mail_backend.num_docs() == 0);
        if mail_needs_resync {
            eprintln!(
                "argos: mail index needs rebuild (schema_wipe={}, sqlite_indexed={}, tantivy_docs={}); clearing mail sqlite",
                mail_opened.needs_full_reindex,
                mail_db_indexed,
                mail_backend.num_docs()
            );
            let _ = db.clear_all_email_messages();
        }

        let indexer = Arc::new(Indexer::new(db.clone(), backend.clone()));
        let remote_server = RemoteServerHandle::new();
        let user_dict = Self::build_user_dict(&db);
        let mail = MailStaHandle::start(db.clone(), mail_backend.clone())?;

        Ok((
            Self {
                db,
                settings: RwLock::new(settings),
                data_dir,
                backend,
                mail_backend,
                indexer,
                remote_server,
                watcher: RwLock::new(None),
                user_dict: RwLock::new(user_dict),
                mail,
                llm_job: RwLock::new(None),
                preview_target: RwLock::new(None),
            },
            opened.needs_full_reindex,
        ))
    }

    pub fn build_user_dict(db: &Db) -> UserDictMatcher {
        let words = db
            .list_search_words()
            .unwrap_or_default()
            .into_iter()
            .map(|w| w.word);
        UserDictMatcher::from_words(words)
    }

    pub fn refresh_user_dict(&self) {
        let matcher = Self::build_user_dict(&self.db);
        *self.user_dict.write() = matcher;
    }

    pub fn set_watcher(&self, handle: WatcherHandle) {
        *self.watcher.write() = Some(handle);
    }

    pub fn watch_folder(&self, path: &str) {
        if let Some(w) = self.watcher.read().as_ref() {
            w.watch_folder(path);
        }
    }

    pub fn unwatch_folder(&self, path: &str) {
        if let Some(w) = self.watcher.read().as_ref() {
            w.unwatch_folder(path);
        }
    }

    pub fn refresh_remote_share(&self) {
        let folders = self.db.list_folders().unwrap_or_default();
        self.remote_server
            .set_share(crate::search::RemoteShareSnapshot::from_folders(&folders));
    }

    pub fn sync_remote_server(&self) {
        self.refresh_remote_share();
        let (enabled, port, token, pos_filter) = {
            let s = self.settings.read();
            (
                s.remote_server_enabled,
                s.remote_server_port,
                s.remote_server_token.clone(),
                s.pos_filter_enabled,
            )
        };
        self.remote_server
            .sync(enabled, port, &token, self.backend.clone(), pos_filter);
    }

    pub fn is_llm_busy(&self) -> bool {
        self.llm_job.read().is_some()
    }

    pub fn start_llm(
        &self,
        request_id: String,
        thread_id: String,
        kind: &str,
        cancel: Arc<AtomicBool>,
    ) -> bool {
        let mut job = self.llm_job.write();
        if job.is_some() {
            return false;
        }
        *job = Some(LlmJob {
            request_id,
            thread_id,
            kind: kind.to_string(),
            cancel,
        });
        true
    }

    pub fn finish_llm(&self, request_id: &str) {
        let mut job = self.llm_job.write();
        if job.as_ref().is_some_and(|j| j.request_id == request_id) {
            *job = None;
        }
    }

    pub fn cancel_llm(&self) -> Option<LlmJob> {
        if let Some(job) = self.llm_job.write().take() {
            job.cancel.store(true, Ordering::SeqCst);
            Some(job)
        } else {
            None
        }
    }
}
