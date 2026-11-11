//! Built-in statute database. The user's statute folders stay on the file index.

mod bundle;
mod catalog;
mod cite;
mod egov;
mod index;
mod lookup;
mod refresh;
mod store;
mod xmlparse;

pub use bundle::bundle_main;
pub use cite::{
    format_tool_result, mark_unverified, summary_lines, verified_from_delivered,
    verified_from_tool_output, VerifiedCite,
    LLM_STATUTE_HINT, LLM_STATUTE_SENTINEL,
};
pub use egov::{EgovSource, LiveEgov, OfflineEgov};
pub use index::{
    citation_hits, ensure_index, index_verified, merge_citation_hits, StatuteIndex, PATH_PREFIX,
};
pub use lookup::{resolve_articles, ArticleQuery};
pub use store::{StatuteDb, BUNDLE_VERSION};

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub struct StatuteService {
    pub db: StatuteDb,
    pub index: StatuteIndex,
    pub client: Arc<dyn EgovSource>,
    pub enabled: AtomicBool,
}

impl StatuteService {
    /// Opens the statute database without importing the seed. A broken file
    /// disables statute features and does not stop the rest of the app.
    pub fn open(data_dir: &Path) -> Self {
        match Self::try_open(data_dir) {
            Ok(service) => service,
            Err(err) => {
                eprintln!("argos: statute db disabled: {err}");
                Self::disabled()
            }
        }
    }

    fn try_open(data_dir: &Path) -> Result<Self, String> {
        let db = StatuteDb::open_empty(&data_dir.join("statutes.db"))?;
        let _ = db.article_count()?;
        let index = StatuteIndex::new(data_dir.join("index-statute"));
        let client: Arc<dyn EgovSource> = match LiveEgov::new() {
            Ok(client) => Arc::new(client),
            Err(err) => {
                eprintln!("argos: e-Gov client disabled: {err}");
                Arc::new(OfflineEgov)
            }
        };
        Ok(Self {
            db,
            index,
            client,
            enabled: AtomicBool::new(true),
        })
    }

    pub fn disabled() -> Self {
        let db = StatuteDb::open_memory().unwrap_or_else(|err| {
            eprintln!("argos: in-memory statute db failed: {err}");
            StatuteDb::open_empty(&std::env::temp_dir().join("argos-statute-disabled.db"))
                .expect("statute fallback")
        });
        Self {
            db,
            index: StatuteIndex::new(std::env::temp_dir().join("argos-statute-disabled-index")),
            client: Arc::new(OfflineEgov),
            enabled: AtomicBool::new(false),
        }
    }

    /// Tests and offline callers. Does not import the bundled seed and never dials out.
    pub fn open_offline(data_dir: &Path) -> Result<Self, String> {
        let db = StatuteDb::open_empty(&data_dir.join("statutes.db"))?;
        let index = StatuteIndex::new(data_dir.join("index-statute"));
        Ok(Self {
            db,
            index,
            client: Arc::new(OfflineEgov),
            enabled: AtomicBool::new(true),
        })
    }
}

/// Import the bundled seed, then build the statute index. Not part of startup.
pub fn prepare_service(service: &StatuteService) -> Result<(), String> {
    if !service.enabled.load(Ordering::Relaxed) {
        return Ok(());
    }
    if let Err(err) = service.db.import_seed() {
        service.enabled.store(false, Ordering::Relaxed);
        return Err(err);
    }
    ensure_index(&service.db, &service.index)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_corrupt_statute_file_does_not_stop_open() {
        let dir = std::env::temp_dir().join(format!(
            "argos-statute-corrupt-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("statutes.db"), "this is not sqlite").unwrap();
        let service = StatuteService::open(&dir);
        assert!(!service.enabled.load(Ordering::Relaxed));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
