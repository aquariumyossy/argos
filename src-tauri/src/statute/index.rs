//! Statute rows in a separate Tantivy index, plus an exact citation lookup
//! that works before that index has been built.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::Mutex;

use crate::search::legal_ref::has_legal_ref;
use crate::search::{SearchHit, TantivyBackend};
use crate::statute::store::{ArticleRow, StatuteDb};
pub const PATH_PREFIX: &str = "statute:";

pub struct StatuteIndex {
    dir: PathBuf,
    gate: Mutex<()>,
    inner: Mutex<Option<Arc<TantivyBackend>>>,
}

impl StatuteIndex {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            gate: Mutex::new(()),
            inner: Mutex::new(None),
        }
    }

    pub fn get(&self) -> Result<Arc<TantivyBackend>, String> {
        let mut slot = self.inner.lock();
        if let Some(backend) = slot.as_ref() {
            return Ok(backend.clone());
        }
        let opened = TantivyBackend::open(&self.dir)?;
        let backend = Arc::new(opened.backend);
        *slot = Some(backend.clone());
        Ok(backend)
    }

    fn token_path(&self) -> PathBuf {
        self.dir.join("statute_index_token")
    }

    pub fn invalidate(&self) {
        let _ = std::fs::remove_file(self.token_path());
    }
}

pub fn ensure_index(db: &StatuteDb, index: &StatuteIndex) -> Result<(), String> {
    let _gate = index.gate.lock();
    let want = db.index_token()?;
    let count = db.article_count()?;
    let backend = index.get()?;
    let have = std::fs::read_to_string(index.token_path()).unwrap_or_default();
    if have.trim() == want && backend.num_docs() == count as u64 {
        return Ok(());
    }
    backend.clear_all()?;
    let rows = db.list_articles()?;
    for chunk in rows.chunks(400) {
        for (name, row) in chunk {
            backend.stage_plain_unit(
                &hit_title(name, row),
                &hit_body(name, row),
                &statute_path(&row.law_id, &row.key),
                &hit_label(row),
            )?;
        }
        backend.publish_staged()?;
    }
    if let Some(parent) = index.token_path().parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(index.token_path(), want).map_err(|e| e.to_string())?;
    Ok(())
}

pub fn index_verified(
    db: &StatuteDb,
    index: &StatuteIndex,
    _articles: &[crate::statute::cite::ToolArticle],
) {
    let _ = sync_dirty(db, index);
}

/// Rewrite every article of a law that just changed, and drop keys that are gone.
/// The freshness token is written only when the index document count matches SQLite.
pub fn sync_dirty(db: &StatuteDb, index: &StatuteIndex) -> Result<(), String> {
    let note = db.take_index_dirty();
    if note.laws.is_empty() && note.removed.is_empty() {
        return Ok(());
    }
    let _gate = index.gate.lock();
    let backend = index.get()?;
    for (law_id, key) in &note.removed {
        backend.stage_delete_path(&statute_path(law_id, key))?;
    }
    for law_id in &note.laws {
        let name = db
            .law_ref(law_id)?
            .map(|law| law.name)
            .unwrap_or_default();
        for row in db.articles_for(law_id)? {
            backend.stage_plain_unit(
                &hit_title(&name, &row),
                &hit_body(&name, &row),
                &statute_path(&row.law_id, &row.key),
                &hit_label(&row),
            )?;
        }
    }
    backend.publish_staged()?;
    let count = db.article_count()?;
    if backend.num_docs() == count as u64 {
        if let Some(parent) = index.token_path().parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(index.token_path(), db.index_token()?).map_err(|e| e.to_string())?;
    } else {
        let _ = std::fs::remove_file(index.token_path());
    }
    Ok(())
}

pub fn statute_path(law_id: &str, key: &str) -> String {
    format!("{PATH_PREFIX}{law_id}:{key}")
}

pub fn citation_hits(db: &StatuteDb, query: &str) -> Vec<SearchHit> {
    if !has_legal_ref(query) {
        return Vec::new();
    }
    let Some((key, _)) = first_article(query) else {
        return Vec::new();
    };
    let Ok(Some(law)) = db.match_law_in_text(query) else {
        return Vec::new();
    };
    let Ok(Some(row)) = db.lookup_article(&law.law_id, &key) else {
        return Vec::new();
    };
    vec![to_hit(&law.name, &row, 1_000_000.0)]
}

pub fn merge_citation_hits(
    hits: Vec<SearchHit>,
    citations: Vec<SearchHit>,
    limit: usize,
) -> Vec<SearchHit> {
    if citations.is_empty() {
        return hits;
    }
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for hit in citations.into_iter().chain(hits) {
        if seen.insert(hit.path.to_ascii_lowercase()) {
            out.push(hit);
        }
        if limit > 0 && out.len() >= limit {
            break;
        }
    }
    out
}

fn first_article(query: &str) -> Option<(String, String)> {
    let span = crate::statute::xmlparse::article_spans(query)
        .into_iter()
        .next()?;
    Some((span.key, span.display))
}

fn hit_title(name: &str, row: &ArticleRow) -> String {
    if row.heading.is_empty() {
        format!("{name} {}", row.num)
    } else {
        format!("{name} {}（{}）", row.num, row.heading)
    }
}

fn hit_label(row: &ArticleRow) -> String {
    if row.heading.is_empty() {
        row.num.clone()
    } else {
        format!("{}（{}）", row.num, row.heading)
    }
}

fn hit_body(name: &str, row: &ArticleRow) -> String {
    format!("{name}\n{}\n{}", row.num, row.body)
}

fn to_hit(name: &str, row: &ArticleRow, score: f32) -> SearchHit {
    let path = statute_path(&row.law_id, &row.key);
    let body = hit_body(name, row);
    let snippet: String = body.chars().take(120).collect();
    SearchHit {
        id: path.clone(),
        title: hit_title(name, row),
        snippet,
        path,
        page: None,
        chunk_id: None,
        score,
        source: "statute".into(),
        preview_text: row.body.clone(),
        highlight_terms: Vec::new(),
        match_count: 1,
        paragraphs: Vec::new(),
        unit_label: hit_label(row),
        mail_from: String::new(),
        mail_date: String::new(),
        mail_conversation_id: String::new(),
        mail_folder: String::new(),
        doc_kind: "statute".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::SearchBackend;
    use crate::statute::xmlparse::ParsedArticle;

    #[test]
    fn citation_lookup_finds_a_saved_article_by_law_name_and_number() {
        let path = std::env::temp_dir().join(format!(
            "argos-cite-hit-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_file(&path);
        let db = StatuteDb::open_empty(&path).unwrap();
        db.replace_law(
            "129AC0000000089",
            "民法",
            "",
            "2026-04-01",
            "2026-04-01",
            "bundle",
            &["民法".into()],
            &[ParsedArticle {
                key: "555".into(),
                num: "第555条".into(),
                heading: "売買".into(),
                body: "売買は、当事者の一方がある財産権を相手方に移転することを約する。".into(),
                suppl: false,
            }],
        )
        .unwrap();
        let hits = citation_hits(&db, "民法 第555条");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].doc_kind, "statute");
        assert!(hits[0].path.starts_with(PATH_PREFIX));
        assert!(hits[0].title.contains("民法"));
        assert!(hits[0].title.contains("第555条"));
        assert!(hits[0].preview_text.contains("財産権"));
        assert!(citation_hits(&db, "売買契約").is_empty());
        let merged = merge_citation_hits(
            vec![SearchHit {
                id: "file".into(),
                title: "メモ".into(),
                snippet: String::new(),
                path: r"C:\memo.txt".into(),
                page: None,
                chunk_id: None,
                score: 1.0,
                source: "local".into(),
                preview_text: String::new(),
                highlight_terms: Vec::new(),
                match_count: 1,
                paragraphs: Vec::new(),
                unit_label: String::new(),
                mail_from: String::new(),
                mail_date: String::new(),
                mail_conversation_id: String::new(),
                mail_folder: String::new(),
                doc_kind: "file".into(),
            }],
            hits,
            10,
        );
        assert_eq!(merged[0].doc_kind, "statute");
        assert_eq!(merged.len(), 2);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn replacing_a_law_refreshes_the_index_even_when_the_count_is_unchanged() {
        let dir = std::env::temp_dir().join(format!(
            "argos-statute-index-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = StatuteDb::open_empty(&dir.join("statutes.db")).unwrap();
        let index = StatuteIndex::new(dir.join("index-statute"));
        let article = |body: &str| ParsedArticle {
            key: "555".into(),
            num: "第555条".into(),
            heading: "売買".into(),
            body: body.into(),
            suppl: false,
        };
        db.replace_law(
            "129AC0000000089",
            "民法",
            "",
            "2026-04-01",
            "2026-04-01",
            "bundle",
            &["民法".into()],
            &[article("OLDBODYTOKEN")],
        )
        .unwrap();
        sync_dirty(&db, &index).unwrap();
        let token = std::fs::read_to_string(index.token_path()).unwrap();
        assert_eq!(index.get().unwrap().num_docs(), 1);
        db.replace_law(
            "129AC0000000089",
            "民法",
            "",
            "2026-04-01",
            "2026-04-01",
            "egov",
            &["民法".into()],
            &[article("NEWBODYTOKEN")],
        )
        .unwrap();
        assert_eq!(db.article_count().unwrap(), 1);
        sync_dirty(&db, &index).unwrap();
        let token2 = std::fs::read_to_string(index.token_path()).unwrap();
        assert_ne!(token, token2, "same count must still change the index token");
        let backend = index.get().unwrap();
        assert_eq!(backend.num_docs(), 1);
        let fresh = backend
            .search_opts(
                "NEWBODYTOKEN",
                5,
                None,
                None,
                false,
                crate::search::SearchOpts::default(),
            )
            .unwrap();
        assert!(
            fresh.iter().any(|hit| {
                hit.preview_text.contains("NEWBODYTOKEN") || hit.snippet.contains("NEWBODYTOKEN")
            }),
            "{fresh:?}"
        );
        let stale = backend
            .search_opts(
                "OLDBODYTOKEN",
                5,
                None,
                None,
                false,
                crate::search::SearchOpts::default(),
            )
            .unwrap();
        assert!(
            stale
                .iter()
                .all(|hit| !hit.preview_text.contains("OLDBODYTOKEN")),
            "{stale:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
