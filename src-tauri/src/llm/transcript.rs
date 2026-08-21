//! Group scanned-PDF page sources and build a single transcript markdown.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::db::LlmSourceRow;
use crate::pathutil;

pub fn image_group_key(row: &LlmSourceRow) -> Option<String> {
    if !row.is_image() {
        return None;
    }
    let path = pathutil::simplify_windows_path(row.path.trim());
    if path.is_empty() {
        return None;
    }
    Some(path.to_ascii_lowercase())
}

pub fn parse_pdf_page_no(paragraph_id: &str) -> Option<u32> {
    let rest = paragraph_id.trim().strip_prefix("pdf-page:")?;
    rest.parse().ok()
}

pub fn file_label(path: &str, title: &str) -> String {
    let p = pathutil::simplify_windows_path(path.trim());
    if !p.is_empty() {
        if let Some(name) = Path::new(&p).file_name().and_then(|n| n.to_str()) {
            if !name.is_empty() {
                return name.to_string();
            }
        }
    }
    let t = title.trim();
    if t.is_empty() {
        "添付ファイル".into()
    } else if let Some((head, _)) = t.split_once('（') {
        let head = head.trim();
        if head.is_empty() {
            t.to_string()
        } else {
            head.to_string()
        }
    } else {
        t.to_string()
    }
}

pub fn sort_image_pages(rows: &mut [LlmSourceRow]) {
    rows.sort_by(|a, b| {
        match (
            parse_pdf_page_no(&a.paragraph_id),
            parse_pdf_page_no(&b.paragraph_id),
        ) {
            (Some(x), Some(y)) => x.cmp(&y).then(a.sort_order.cmp(&b.sort_order)),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a
                .sort_order
                .cmp(&b.sort_order)
                .then(a.created_at.cmp(&b.created_at)),
        }
    });
}

/// Pages in `shown` that share `seed`'s image group, in page order.
pub fn group_members<'a>(
    shown: &'a [LlmSourceRow],
    seed: &'a LlmSourceRow,
) -> Vec<&'a LlmSourceRow> {
    let Some(key) = image_group_key(seed) else {
        return vec![seed];
    };
    let mut members: Vec<&LlmSourceRow> = shown
        .iter()
        .filter(|s| image_group_key(s).as_deref() == Some(key.as_str()))
        .collect();
    if members.is_empty() {
        members.push(seed);
    }
    members.sort_by(|a, b| {
        match (
            parse_pdf_page_no(&a.paragraph_id),
            parse_pdf_page_no(&b.paragraph_id),
        ) {
            (Some(x), Some(y)) => x.cmp(&y).then(a.sort_order.cmp(&b.sort_order)),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a
                .sort_order
                .cmp(&b.sort_order)
                .then(a.created_at.cmp(&b.created_at)),
        }
    });
    members
}

pub fn group_cite_no(members: &[&LlmSourceRow]) -> i64 {
    members
        .iter()
        .filter(|s| s.cite_no > 0)
        .map(|s| s.cite_no)
        .min()
        .unwrap_or_else(|| members.first().map(|s| s.sort_order + 1).unwrap_or(1))
}

fn page_heading(row: &LlmSourceRow) -> Option<String> {
    parse_pdf_page_no(&row.paragraph_id).map(|n| format!("{n}ページ目"))
}

/// Transcript body only (no `# filename` title). Used inside 【出典】 and as MD body.
pub fn transcript_body(members: &[&LlmSourceRow], multi_page: bool) -> String {
    let mut parts: Vec<String> = Vec::new();
    for row in members {
        let body = row.body.trim();
        if body.is_empty() {
            continue;
        }
        if multi_page {
            if let Some(h) = page_heading(row) {
                parts.push(format!("## {h}\n\n{body}"));
                continue;
            }
        }
        parts.push(body.to_string());
    }
    parts.join("\n\n")
}

pub fn save_markdown(members: &[&LlmSourceRow], filename: &str, total_pages: usize) -> String {
    let written = members.iter().filter(|s| !s.body.trim().is_empty()).count();
    let mut out = format!("# {filename}\n");
    if total_pages > written && written > 0 {
        out.push('\n');
        out.push_str(&format!(
            "（{total_pages}ページ中 {written}ページを書き出し）\n"
        ));
    }
    let body = transcript_body(members, members.len() > 1 || total_pages > 1);
    if !body.is_empty() {
        out.push('\n');
        out.push_str(&body);
        out.push('\n');
    }
    out
}

fn sanitize_stem(raw: &str) -> String {
    let s: String = raw
        .chars()
        .map(|c| match c {
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            _ => c,
        })
        .collect();
    let s = s.trim().trim_end_matches('.');
    if s.is_empty() {
        "argos-transcript".into()
    } else {
        s.to_string()
    }
}

pub fn write_md_file(path: &Path, contents: &str, overwrite: bool) -> Result<(bool, bool), String> {
    let existed = path.exists();
    if existed && !overwrite {
        return Ok((true, false));
    }
    std::fs::write(path, contents).map_err(|e| format!("書き込めませんでした（{e}）。"))?;
    Ok((existed, true))
}

pub struct TranscriptSave {
    pub dest: PathBuf,
    pub markdown: String,
}

/// `members` is the full image group (including empty / pending / error pages).
pub fn prepare_transcript_save(members: &[LlmSourceRow]) -> Result<TranscriptSave, String> {
    let seed = members
        .first()
        .ok_or_else(|| "出典が見つかりません。".to_string())?;
    let dest = dest_md_path(&seed.path)?;
    let writable: Vec<&LlmSourceRow> = members.iter().filter(|s| s.is_injectable()).collect();
    if writable.is_empty() {
        return Err("書き出せる書き起こしがありません。".into());
    }
    let filename = file_label(&seed.path, &seed.title);
    Ok(TranscriptSave {
        dest,
        markdown: save_markdown(&writable, &filename, members.len()),
    })
}

pub fn dest_md_path(original: &str) -> Result<PathBuf, String> {
    let original = pathutil::simplify_windows_path(original.trim());
    if original.is_empty() {
        return Err("元ファイルのパスが空です。".into());
    }
    let p = Path::new(&original);
    let parent = p
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .ok_or_else(|| "保存先フォルダが分かりません。".to_string())?;
    if !parent.exists() {
        return Err("保存先フォルダが存在しません。".into());
    }
    let stem = p
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("argos-transcript");
    Ok(parent.join(format!("{}.md", sanitize_stem(stem))))
}

/// Walk `shown` in order, emitting each image group once and leaving non-image rows as-is.
pub fn walk_format_units<'a>(shown: &'a [LlmSourceRow]) -> Vec<Vec<&'a LlmSourceRow>> {
    let mut used: HashSet<String> = HashSet::new();
    let mut units: Vec<Vec<&LlmSourceRow>> = Vec::new();
    for s in shown {
        if let Some(key) = image_group_key(s) {
            if !used.insert(key.clone()) {
                continue;
            }
            units.push(group_members(shown, s));
        } else {
            units.push(vec![s]);
        }
    }
    units
}

pub fn omission_note(shown: &[&LlmSourceRow], pool: &[LlmSourceRow]) -> Option<String> {
    let seed = *shown.first()?;
    let Some(key) = image_group_key(seed) else {
        return None;
    };
    let pool_n = pool
        .iter()
        .filter(|s| {
            image_group_key(s).as_deref() == Some(key.as_str()) && !s.body.trim().is_empty()
        })
        .count();
    let shown_n = shown.iter().filter(|s| !s.body.trim().is_empty()).count();
    if pool_n > shown_n && shown_n > 0 {
        let last = shown
            .iter()
            .filter_map(|s| parse_pdf_page_no(&s.paragraph_id))
            .max()
            .unwrap_or(shown_n as u32);
        Some(format!("（{}ページ以降は文字数のため省略）", last + 1))
    } else {
        None
    }
}

/// Map group key -> cite_no to apply on kept rows, allocating new numbers for new groups.
pub fn assign_group_cites(
    kept: &mut [LlmSourceRow],
    all: &[LlmSourceRow],
    pending_ids: &HashSet<String>,
    next_cite: &mut i64,
) {
    let mut group_cite: HashMap<String, i64> = HashMap::new();
    for s in all.iter().chain(kept.iter()) {
        let Some(k) = image_group_key(s) else {
            continue;
        };
        if s.cite_no > 0 {
            group_cite
                .entry(k)
                .and_modify(|n| *n = (*n).min(s.cite_no))
                .or_insert(s.cite_no);
        }
    }
    let mut minted: HashSet<String> = HashSet::new();
    for s in kept.iter() {
        let Some(k) = image_group_key(s) else {
            continue;
        };
        if group_cite.contains_key(&k) || minted.contains(&k) {
            continue;
        }
        if pending_ids.contains(&s.id) {
            *next_cite += 1;
            group_cite.insert(k.clone(), *next_cite);
            minted.insert(k);
        }
    }
    for s in kept.iter_mut() {
        if let Some(k) = image_group_key(s) {
            if let Some(&n) = group_cite.get(&k) {
                s.cite_no = n;
            }
        } else if pending_ids.contains(&s.id) && s.cite_no <= 0 {
            *next_cite += 1;
            s.cite_no = *next_cite;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(id: &str, path: &str, page: u32, body: &str, cite: i64) -> LlmSourceRow {
        LlmSourceRow {
            id: id.into(),
            thread_id: "t".into(),
            sort_order: page as i64,
            origin: "attach".into(),
            path: path.into(),
            title: format!("scan.pdf（{page}ページ目）"),
            paragraph_id: format!("pdf-page:{page}"),
            body: body.into(),
            query: String::new(),
            created_at: page as i64,
            grain: "file".into(),
            unit_body: String::new(),
            injected_user_message_id: String::new(),
            cited_assistant_message_id: String::new(),
            cite_no: cite,
            kind: "image".into(),
            stored_relpath: format!("{id}.jpg"),
            ocr_status: String::new(),
        }
    }

    #[test]
    fn dest_md_uses_stem() {
        let dir = std::env::temp_dir().join(format!("argos-md-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let pdf = dir.join("契約書.pdf");
        std::fs::write(&pdf, b"x").unwrap();
        let md = dest_md_path(&pdf.to_string_lossy()).unwrap();
        assert_eq!(md.file_name().unwrap(), "契約書.md");
        let jpg = dir.join("写真.jpg");
        std::fs::write(&jpg, b"x").unwrap();
        let md2 = dest_md_path(&jpg.to_string_lossy()).unwrap();
        assert_eq!(md2.file_name().unwrap(), "写真.md");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dest_md_sanitizes_forbidden() {
        assert_eq!(sanitize_stem(r"a:b*c"), "a_b_c");
        assert_eq!(sanitize_stem("..."), "argos-transcript");
    }

    #[test]
    fn save_markdown_pages_and_partial() {
        let a = img("1", r"C:\scan.pdf", 1, "一枚目", 0);
        let b = img("2", r"C:\scan.pdf", 2, "二枚目", 0);
        let md = save_markdown(&[&a, &b], "scan.pdf", 5);
        assert!(md.contains("# scan.pdf"));
        assert!(md.contains("（5ページ中 2ページを書き出し）"));
        assert!(md.contains("## 1ページ目"));
        assert!(md.contains("## 2ページ目"));
        assert!(md.contains("一枚目"));
    }

    #[test]
    fn single_image_has_no_page_heading() {
        let a = img("1", r"C:\photo.jpg", 1, "写真", 0);
        let mut one = a.clone();
        one.paragraph_id.clear();
        one.title = "photo.jpg".into();
        let md = save_markdown(&[&one], "photo.jpg", 1);
        assert!(!md.contains("ページ目"));
        assert!(md.contains("写真"));
    }

    #[test]
    fn empty_path_is_not_grouped() {
        let mut a = img("1", "", 1, "a", 0);
        a.path.clear();
        assert!(image_group_key(&a).is_none());
        let mut b = img("2", "", 2, "b", 0);
        b.path.clear();
        let rows = [a, b];
        let units = walk_format_units(&rows);
        assert_eq!(units.len(), 2);
    }

    #[test]
    fn write_md_refuses_overwrite() {
        let dir = std::env::temp_dir().join(format!("argos-mdw-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.md");
        std::fs::write(&path, "old").unwrap();
        let (existed, written) = write_md_file(&path, "new", false).unwrap();
        assert!(existed && !written);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "old");
        let (existed, written) = write_md_file(&path, "new", true).unwrap();
        assert!(existed && written);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prepare_skips_empty_and_pending() {
        let dir = std::env::temp_dir().join(format!("argos-prep-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let pdf = dir.join("scan.pdf");
        std::fs::write(&pdf, b"x").unwrap();
        let mut a = img("1", &pdf.to_string_lossy(), 1, "本文", 0);
        let mut b = img("2", &pdf.to_string_lossy(), 2, "", 0);
        b.ocr_status = "pending".into();
        let mut c = img("3", &pdf.to_string_lossy(), 3, "", 0);
        c.ocr_status = "error".into();
        let plan = prepare_transcript_save(&[a.clone(), b, c]).unwrap();
        assert_eq!(plan.dest.file_name().unwrap(), "scan.md");
        assert!(plan.markdown.contains("本文"));
        assert!(plan.markdown.contains("（3ページ中 1ページを書き出し）"));
        a.body.clear();
        a.ocr_status = "pending".into();
        assert!(prepare_transcript_save(&[a]).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dest_md_rejects_empty_path() {
        assert!(dest_md_path("").is_err());
        assert!(dest_md_path("   ").is_err());
    }

    #[test]
    fn walk_units_one_chip_per_path() {
        let a = img("1", r"C:\scan.pdf", 1, "a", 2);
        let b = img("2", r"C:\scan.pdf", 2, "b", 3);
        let mut text = img("t", r"C:\scan.pdf", 1, "hit", 4);
        text.kind = "text".into();
        text.paragraph_id = "p1".into();
        let rows = [a, b, text];
        let units = walk_format_units(&rows);
        assert_eq!(units.len(), 2);
        assert_eq!(units[0].len(), 2);
        assert_eq!(units[1].len(), 1);
        assert!(!units[1][0].is_image());
    }
}
