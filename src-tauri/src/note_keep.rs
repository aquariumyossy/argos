//! Badge keeps for an index hit or a web page.
//!
//! Paragraph keeps from the search popup stay as cards. A badge stores the
//! same note item row, with `badge: true`, and opens the file or the page.

use crate::db::{Db, NoteItemRow};
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
