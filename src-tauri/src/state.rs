use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::RwLock;

use crate::db::{Db, Settings};
use crate::indexer::Indexer;
use crate::remote_server::RemoteServerHandle;
use crate::search::tantivy_backend::TantivyBackend;
use crate::search::UserDictMatcher;
use crate::watcher::WatcherHandle;

pub struct AppState {
    pub db: Arc<Db>,
    pub settings: RwLock<Settings>,
    pub data_dir: PathBuf,
    pub backend: Arc<TantivyBackend>,
    pub indexer: Arc<Indexer>,
    pub remote_server: RemoteServerHandle,
    /// Set during app setup after the FS watcher thread starts.
    pub watcher: RwLock<Option<WatcherHandle>>,
    /// Client-side user dictionary for query phrase forcing.
    pub user_dict: RwLock<UserDictMatcher>,
}

impl AppState {
    pub fn open() -> Result<Self, String> {
        let data_dir = dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Argos");
        std::fs::create_dir_all(&data_dir).map_err(|e| e.to_string())?;
        let db_path = data_dir.join("argos.db");
        let db = Arc::new(Db::open(&db_path).map_err(|e| e.to_string())?);
        let settings = db.load_settings();
        let _ = db.save_settings(&settings);

        let index_dir = data_dir.join("index");
        let backend = Arc::new(TantivyBackend::open(&index_dir)?);
        let indexer = Arc::new(Indexer::new(db.clone(), backend.clone()));
        let remote_server = RemoteServerHandle::new();
        let user_dict = Self::build_user_dict(&db);

        Ok(Self {
            db,
            settings: RwLock::new(settings),
            data_dir,
            backend,
            indexer,
            remote_server,
            watcher: RwLock::new(None),
            user_dict: RwLock::new(user_dict),
        })
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

    pub fn sync_remote_server(&self) {
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
}
