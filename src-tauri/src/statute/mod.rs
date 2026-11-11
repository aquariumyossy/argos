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
    format_tool_result, mark_unverified, summary_lines, verified_from_tool_output, VerifiedCite,
    LLM_STATUTE_HINT, LLM_STATUTE_SENTINEL,
};
pub use egov::{EgovSource, LiveEgov, OfflineEgov};
pub use index::{
    citation_hits, ensure_index, index_verified, merge_citation_hits, StatuteIndex, PATH_PREFIX,
};
pub use lookup::{resolve_articles, ArticleQuery};
pub use store::{StatuteDb, BUNDLE_VERSION};

use std::path::Path;
use std::sync::Arc;

pub struct StatuteService {
    pub db: StatuteDb,
    pub index: StatuteIndex,
    pub client: Arc<dyn EgovSource>,
}

impl StatuteService {
    pub fn open(data_dir: &Path) -> Result<Self, String> {
        let db = StatuteDb::open(&data_dir.join("statutes.db"))?;
        let index = StatuteIndex::new(data_dir.join("index-statute"));
        let client = Arc::new(LiveEgov::new()?);
        Ok(Self { db, index, client })
    }

    /// Tests and offline callers. Does not import the bundled seed and never dials out.
    pub fn open_offline(data_dir: &Path) -> Result<Self, String> {
        let db = StatuteDb::open_empty(&data_dir.join("statutes.db"))?;
        let index = StatuteIndex::new(data_dir.join("index-statute"));
        Ok(Self {
            db,
            index,
            client: Arc::new(OfflineEgov),
        })
    }
}

pub fn ensure_service_index(service: &StatuteService) -> Result<(), String> {
    ensure_index(&service.db, &service.index)
}
