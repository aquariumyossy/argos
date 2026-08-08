//! Outlook Classic COM mail sync (separate from FS indexer).

pub mod path;
pub mod sync;

#[cfg(windows)]
pub mod outlook_com;
#[cfg(windows)]
pub mod sta_worker;

#[cfg(not(windows))]
pub mod outlook_com {
    use super::sync::{OutlookFolderInfo, OutlookMessage};

    pub fn detect_outlook() -> Result<String, String> {
        Err("Outlook 連携は Windows のみ対応です".into())
    }

    pub fn list_mail_folders() -> Result<Vec<OutlookFolderInfo>, String> {
        Err("Outlook 連携は Windows のみ対応です".into())
    }

    pub fn fetch_messages_in_folder(
        _folder_entry_id: &str,
        _store_id: &str,
        _since_unix: i64,
    ) -> Result<Vec<OutlookMessage>, String> {
        Err("Outlook 連携は Windows のみ対応です".into())
    }

    pub fn open_mail_item(_store_id: &str, _entry_id: &str) -> Result<(), String> {
        Err("Outlook 連携は Windows のみ対応です".into())
    }
}

#[cfg(not(windows))]
pub mod sta_worker {
    use std::sync::Arc;

    use crate::db::Db;
    use crate::search::tantivy_backend::TantivyBackend;

    use super::sync::{MailSyncProgress, MailSyncStats, OutlookFolderInfo};

    #[derive(Clone)]
    pub struct MailStaHandle {
        _db: Arc<Db>,
        _backend: Arc<TantivyBackend>,
    }

    impl MailStaHandle {
        pub fn start(db: Arc<Db>, backend: Arc<TantivyBackend>) -> Self {
            Self {
                _db: db,
                _backend: backend,
            }
        }

        pub fn detect(&self) -> Result<String, String> {
            Err("Outlook 連携は Windows のみ対応です".into())
        }

        pub fn list_folders(&self) -> Result<Vec<OutlookFolderInfo>, String> {
            Err("Outlook 連携は Windows のみ対応です".into())
        }

        pub fn sync_all<F>(&self, _on_progress: F) -> Result<MailSyncStats, String>
        where
            F: FnMut(MailSyncProgress) + Send + 'static,
        {
            Err("Outlook 連携は Windows のみ対応です".into())
        }

        pub fn open_item(&self, _store_id: &str, _entry_id: &str) -> Result<(), String> {
            Err("Outlook 連携は Windows のみ対応です".into())
        }
    }
}

pub use path::{is_outlook_path, make_outlook_path, parse_outlook_path, OUTLOOK_SCHEME};
pub use sta_worker::MailStaHandle;
pub use sync::{MailSyncProgress, MailSyncStats, OutlookFolderInfo};
