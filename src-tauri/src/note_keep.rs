//! Badge keeps for an index hit or a web page.
//!
//! Paragraph keeps from the search popup stay as cards. A badge stores the
//! same note item row, with `badge: true`, and opens the file or the page.

use std::collections::HashSet;

use crate::db::{Db, LlmSourceRow, NoteItemRow};
use serde_json::{json, Value};

pub struct KeepBadge {
    pub path: String,
    pub title: String,
    pub source: String,
    pub paragraph_id: String,
    pub body: String,
    pub query: String,
}

pub struct KeepInsert {
    pub item: NoteItemRow,
    pub created: bool,
}

pub fn badge_json(badge: &KeepBadge) -> Value {
    json!({
        "path": badge.path,
        "title": badge.title,
        "source": badge.source,
        "docKind": if badge.source == "web" { "web" } else { "" },
        "paragraphId": badge.paragraph_id,
        "label": "",
        "page": null,
        "body": badge.body,
        "highlightTerms": [],
        "mailFrom": "",
        "mailDate": "",
        "mailFolder": "",
        "badge": true,
    })
}

/// A cited web page or index hit, ready to keep. Notes, pasted images, and empty
/// targets are skipped. Scanned PDF pages share one badge per PDF path.
pub fn badge_for_cited_source(row: &LlmSourceRow) -> Option<KeepBadge> {
    if row.is_note() || is_pasted_image(row) {
        return None;
    }
    if row.is_image() && crate::llm::transcript::parse_pdf_page_no(&row.paragraph_id).is_some() {
        return pdf_document_badge(row);
    }
    if row.is_web() {
        let url = row.path.trim();
        if !is_http_url(url) {
            return None;
        }
        let title = row.title.trim();
        let title = if title.is_empty() {
            url.to_string()
        } else {
            title.to_string()
        };
        return Some(KeepBadge {
            path: url.to_string(),
            title,
            source: "web".into(),
            paragraph_id: url.to_string(),
            body: String::new(),
            query: row.query.clone(),
        });
    }
    let paragraph_id = row.paragraph_id.trim();
    let path = row.path.trim();
    let path = if path.is_empty() {
        path_before_unit(paragraph_id)
    } else {
        path.to_string()
    };
    if path.trim().is_empty() {
        return None;
    }
    let paragraph_id = if paragraph_id.is_empty() {
        path.clone()
    } else {
        paragraph_id.to_string()
    };
    let title = row.title.trim();
    let title = if title.is_empty() {
        file_title(&path)
    } else {
        title.to_string()
    };
    let source = if row.is_remote() {
        "remote".to_string()
    } else {
        "index".to_string()
    };
    Some(KeepBadge {
        path,
        title,
        source,
        paragraph_id,
        body: cap_keep_body(&row.body),
        query: row.query.clone(),
    })
}

fn cap_keep_body(body: &str) -> String {
    body.trim().chars().take(400).collect()
}

/// A paste has no user-facing file. Its path and title are the label, such as
/// `貼り付け画像1` or `貼り付け画像（日時）`.
fn is_pasted_image(row: &LlmSourceRow) -> bool {
    if !row.is_image() {
        return false;
    }
    if crate::llm::transcript::parse_pdf_page_no(&row.paragraph_id).is_some() {
        return false;
    }
    let path = row.path.trim();
    let title = row.title.trim();
    path.starts_with("貼り付け画像")
        || title.starts_with("貼り付け画像")
        || !looks_like_user_file(path)
}

fn looks_like_user_file(path: &str) -> bool {
    let path = path.trim();
    if path.is_empty() || is_http_url(path) {
        return false;
    }
    path.contains('\\') || path.contains('/') || path.contains(':')
}

/// Dedup key is the PDF path, not `pdf-page:N`, so page 1 of another PDF is new.
fn pdf_document_badge(row: &LlmSourceRow) -> Option<KeepBadge> {
    let display = crate::pathutil::simplify_windows_path(row.path.trim());
    if display.is_empty() || !looks_like_user_file(&display) {
        return None;
    }
    let paragraph_id = format!("{}#pdf", display.to_ascii_lowercase());
    let source = if row.is_remote() {
        "remote".to_string()
    } else {
        "index".to_string()
    };
    Some(KeepBadge {
        path: display.clone(),
        title: file_title(&display),
        source,
        paragraph_id,
        body: String::new(),
        query: row.query.clone(),
    })
}

/// Add the sources cited in the final answer to each note written in that turn.
/// Search hits that were not cited are left out.
pub fn keep_cited_on_notes(
    db: &Db,
    note_ids: &[String],
    sources: &[LlmSourceRow],
    cited: &[(String, i64)],
) -> usize {
    let wanted: HashSet<&str> = cited.iter().map(|(id, _)| id.as_str()).collect();
    let mut added = 0usize;
    for note_id in note_ids {
        if note_id.trim().is_empty() {
            continue;
        }
        for source in sources {
            if !wanted.contains(source.id.as_str()) {
                continue;
            }
            let Some(badge) = badge_for_cited_source(source) else {
                continue;
            };
            if let Ok(insert) = add_badge(db, note_id, &badge) {
                if insert.created {
                    added += 1;
                }
            }
        }
    }
    added
}

pub fn add_badge(db: &Db, note_id: &str, badge: &KeepBadge) -> Result<KeepInsert, String> {
    let paragraph_id = badge.paragraph_id.trim();
    if paragraph_id.is_empty() || badge.path.trim().is_empty() {
        return Err("キープする対象がありません。".into());
    }
    let json = serde_json::to_string(&badge_json(badge)).map_err(|e| e.to_string())?;
    db.insert_keep_json(note_id, badge.query.trim(), paragraph_id, &json)
        .map_err(|e| e.to_string())
}

/// Path stored in a paragraph id. Windows paths may contain `#`, so the unit
/// number is the part after the last `#`.
pub fn path_before_unit(paragraph_id: &str) -> String {
    match paragraph_id.rfind('#') {
        Some(i) if i > 0 => paragraph_id[..i].to_string(),
        _ => paragraph_id.to_string(),
    }
}

pub fn file_title(path: &str) -> String {
    let trimmed = path.trim().trim_end_matches(['\\', '/']);
    let name = trimmed
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(trimmed)
        .trim();
    if name.is_empty() {
        "出典".into()
    } else {
        name.to_string()
    }
}

pub fn is_http_url(raw: &str) -> bool {
    let t = raw.trim();
    if t.is_empty() || t.chars().any(char::is_whitespace) {
        return false;
    }
    let lower = t.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db() -> (std::path::PathBuf, Db) {
        let dir = std::env::temp_dir().join(format!(
            "argos-keep-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("argos.db")).unwrap();
        (dir, db)
    }

    fn index_badge(id: &str) -> KeepBadge {
        KeepBadge {
            path: path_before_unit(id),
            title: "契約書.txt".into(),
            source: "index".into(),
            paragraph_id: id.into(),
            body: "解除できる".into(),
            query: "解除".into(),
        }
    }

    #[test]
    fn index_and_web_badges_dedup_and_keep_the_open_target() {
        let (dir, db) = temp_db();
        let note = db.create_note("事件").unwrap();
        let id = r"C:\docs\契約書.txt#3";
        let first = add_badge(&db, &note.id, &index_badge(id)).unwrap();
        assert!(first.created);
        let again = add_badge(&db, &note.id, &index_badge(id)).unwrap();
        assert!(!again.created);
        assert_eq!(again.item.id, first.item.id);

        let web = KeepBadge {
            path: "https://example.com/a".into(),
            title: "解説".into(),
            source: "web".into(),
            paragraph_id: "https://example.com/a".into(),
            body: String::new(),
            query: String::new(),
        };
        let page = add_badge(&db, &note.id, &web).unwrap();
        assert!(page.created);
        let items = db.list_note_items(&note.id).unwrap();
        assert_eq!(items.len(), 2);
        let index_json: serde_json::Value = serde_json::from_str(&items[0].item_json).unwrap();
        assert_eq!(index_json["badge"], true);
        assert_eq!(index_json["path"], r"C:\docs\契約書.txt");
        assert_eq!(index_json["paragraphId"], id);
        let web_json: serde_json::Value = serde_json::from_str(&items[1].item_json).unwrap();
        assert_eq!(web_json["badge"], true);
        assert_eq!(web_json["path"], "https://example.com/a");
        assert_eq!(web_json["title"], "解説");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn llm_source(
        id: &str,
        kind: &str,
        path: &str,
        paragraph_id: &str,
        title: &str,
        body: &str,
    ) -> LlmSourceRow {
        LlmSourceRow {
            id: id.into(),
            thread_id: "t".into(),
            sort_order: 0,
            origin: "tool".into(),
            path: path.into(),
            title: title.into(),
            paragraph_id: paragraph_id.into(),
            body: body.into(),
            query: "解除".into(),
            created_at: 1,
            grain: "unit".into(),
            unit_body: String::new(),
            injected_user_message_id: String::new(),
            cited_assistant_message_id: String::new(),
            cite_no: 1,
            kind: kind.into(),
            stored_relpath: String::new(),
            ocr_status: String::new(),
        }
    }

    #[test]
    fn cited_web_and_index_are_kept_and_uncited_hits_are_not() {
        let (dir, db) = temp_db();
        let note = db.create_note("事件").unwrap();
        let web = llm_source(
            "w1",
            "web",
            "https://example.com/law",
            "",
            "解説",
            "ページ本文",
        );
        let index = llm_source(
            "h1",
            "text",
            r"C:\docs\契約書.txt",
            r"C:\docs\契約書.txt#3",
            "契約書.txt",
            "解除できる",
        );
        let unused = llm_source(
            "h2",
            "text",
            r"C:\docs\別.txt",
            r"C:\docs\別.txt#1",
            "別.txt",
            "関係ない",
        );
        let note_src = llm_source("n1", "note", "note:abc", "note:abc", "別ノート", "本文");
        let cited = vec![("w1".into(), 1i64), ("h1".into(), 2), ("n1".into(), 3)];
        let added = keep_cited_on_notes(
            &db,
            &[note.id.clone()],
            &[web, index, unused, note_src],
            &cited,
        );
        assert_eq!(added, 2);
        let items = db.list_note_items(&note.id).unwrap();
        assert_eq!(items.len(), 2);
        let web_json: serde_json::Value = serde_json::from_str(&items[0].item_json).unwrap();
        assert_eq!(web_json["badge"], true);
        assert_eq!(web_json["path"], "https://example.com/law");
        assert_eq!(web_json["source"], "web");
        let index_json: serde_json::Value = serde_json::from_str(&items[1].item_json).unwrap();
        assert_eq!(index_json["badge"], true);
        assert_eq!(index_json["path"], r"C:\docs\契約書.txt");
        assert_eq!(index_json["paragraphId"], r"C:\docs\契約書.txt#3");
        assert!(keep_cited_on_notes(&db, &[note.id], &[], &cited) == 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_later_note_answer_keeps_a_source_cited_on_an_earlier_turn() {
        let (dir, db) = temp_db();
        let note = db.create_note("事件").unwrap();
        let mut earlier = llm_source(
            "h1",
            "text",
            r"C:\docs\契約書.txt",
            r"C:\docs\契約書.txt#3",
            "契約書.txt",
            "解除できる",
        );
        earlier.cite_no = 1;
        earlier.injected_user_message_id = "u1".into();
        let mut unused = llm_source(
            "w2",
            "web",
            "https://example.com/other",
            "",
            "別ページ",
            "本文",
        );
        unused.cite_no = 2;
        let sources = [earlier, unused];
        let this_turn_consumed: Vec<(String, i64)> = Vec::new();
        let answer = "まとめました [1]";
        assert!(crate::llm::context::consumed_cited_in_answer(&this_turn_consumed, answer).is_empty());
        let cited = crate::llm::context::thread_sources_cited_in_answer(&sources, answer);
        assert_eq!(cited, vec![("h1".into(), 1)]);
        let added = keep_cited_on_notes(&db, &[note.id.clone()], &sources, &cited);
        assert_eq!(added, 1);
        let items = db.list_note_items(&note.id).unwrap();
        assert_eq!(items.len(), 1);
        let json: serde_json::Value = serde_json::from_str(&items[0].item_json).unwrap();
        assert_eq!(json["path"], r"C:\docs\契約書.txt");
        assert_eq!(json["paragraphId"], r"C:\docs\契約書.txt#3");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pasted_images_are_not_auto_kept() {
        let (dir, db) = temp_db();
        let note = db.create_note("事件").unwrap();
        let short = llm_source("p1", "image", "貼り付け画像1", "", "貼り付け画像1", "書き起こし");
        let stamped = llm_source(
            "p2",
            "image",
            "貼り付け画像（2026-11-12 11時59分01秒）",
            "",
            "貼り付け画像（2026-11-12 11時59分01秒）",
            "書き起こし",
        );
        assert!(badge_for_cited_source(&short).is_none());
        assert!(badge_for_cited_source(&stamped).is_none());
        let added = keep_cited_on_notes(
            &db,
            &[note.id.clone()],
            &[short, stamped],
            &[("p1".into(), 1), ("p2".into(), 2)],
        );
        assert_eq!(added, 0);
        assert!(db.list_note_items(&note.id).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scanned_pdf_pages_collapse_to_one_badge_per_file() {
        let (dir, db) = temp_db();
        let note = db.create_note("事件").unwrap();
        let page1 = llm_source(
            "a1",
            "image",
            r"C:\docs\scan.pdf",
            "pdf-page:1",
            "scan.pdf（1ページ目）",
            "一枚目",
        );
        let page2 = llm_source(
            "a2",
            "image",
            r"C:\docs\scan.pdf",
            "pdf-page:2",
            "scan.pdf（2ページ目）",
            "二枚目",
        );
        let other = llm_source(
            "b1",
            "image",
            r"C:\other\scan.pdf",
            "pdf-page:1",
            "scan.pdf（1ページ目）",
            "別の原本",
        );
        let added = keep_cited_on_notes(
            &db,
            &[note.id.clone()],
            &[page1, page2, other],
            &[("a1".into(), 1), ("a2".into(), 2), ("b1".into(), 3)],
        );
        assert_eq!(added, 2);
        let items = db.list_note_items(&note.id).unwrap();
        assert_eq!(items.len(), 2);
        let first: serde_json::Value = serde_json::from_str(&items[0].item_json).unwrap();
        let second: serde_json::Value = serde_json::from_str(&items[1].item_json).unwrap();
        assert_eq!(first["path"], r"C:\docs\scan.pdf");
        assert_eq!(first["title"], "scan.pdf");
        assert_eq!(first["paragraphId"], r"c:\docs\scan.pdf#pdf");
        assert_eq!(items[0].paragraph_id, r"c:\docs\scan.pdf#pdf");
        assert_eq!(second["path"], r"C:\other\scan.pdf");
        assert_eq!(second["paragraphId"], r"c:\other\scan.pdf#pdf");
        assert_ne!(first["paragraphId"], second["paragraphId"]);
        assert!(!first["paragraphId"].as_str().unwrap_or("").starts_with("pdf-page:"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn path_before_unit_keeps_a_hash_inside_the_path() {
        assert_eq!(
            path_before_unit(r"C:\docs\a#b.txt#12"),
            r"C:\docs\a#b.txt"
        );
        assert!(is_http_url("https://example.com/a"));
        assert!(!is_http_url("note:abc"));
    }
}
