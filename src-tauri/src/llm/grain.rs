//! Expand a chat source from one indexed unit to the whole file (and back).

use crate::db::Settings;
use crate::mail;
use crate::search::{self, SearchHit};
use crate::search::tantivy_backend::TantivyBackend;

/// Refuse to load a file larger than this into one source body.
pub const FILE_GRAIN_HARD_CAP: usize = 200_000;
const OVERLAP_MIN: usize = 8;
const OVERLAP_SCAN: usize = 32;

pub fn is_file_grain(grain: &str) -> bool {
    grain.trim().eq_ignore_ascii_case("file")
}

pub fn char_len(s: &str) -> usize {
    s.chars().count()
}

/// Join indexed units in document order. Overlapping windowed chunks are stitched.
pub fn concat_unit_texts<S: AsRef<str>>(texts: impl IntoIterator<Item = S>) -> String {
    let mut out = String::new();
    for raw in texts {
        let t = raw.as_ref().trim();
        if t.is_empty() {
            continue;
        }
        if out.is_empty() {
            out.push_str(t);
            continue;
        }
        let n = suffix_prefix_overlap(&out, t);
        if n >= OVERLAP_MIN {
            let rest: String = t.chars().skip(n).collect();
            out.push_str(&rest);
        } else {
            out.push_str("\n\n");
            out.push_str(t);
        }
    }
    out
}

fn suffix_prefix_overlap(left: &str, right: &str) -> usize {
    let a: Vec<char> = left.chars().collect();
    let b: Vec<char> = right.chars().collect();
    let max = a.len().min(b.len()).min(OVERLAP_SCAN);
    for n in (1..=max).rev() {
        if a[a.len() - n..] == b[..n] {
            return n;
        }
    }
    0
}

pub fn collect_path_units(
    local: &TantivyBackend,
    mail: &TantivyBackend,
    path: &str,
) -> Result<Vec<SearchHit>, String> {
    let path = path.trim();
    if path.is_empty() {
        return Ok(Vec::new());
    }
    let backend = if mail::is_outlook_path(path) {
        mail
    } else {
        local
    };
    backend.units_for_path(path, 20_000)
}

pub fn check_file_body_size(body: &str) -> Result<(), String> {
    let n = char_len(body);
    if n > FILE_GRAIN_HARD_CAP {
        return Err(format!(
            "ファイルが大きすぎます（{n} 文字）。{FILE_GRAIN_HARD_CAP} 文字までです。"
        ));
    }
    Ok(())
}

pub fn file_body_from_units(units: &[SearchHit]) -> Result<String, String> {
    if units.is_empty() {
        return Err(
            "このファイルはローカルのインデックスにありません。リモートのみのヒットは全文にできません。"
                .into(),
        );
    }
    let body = concat_unit_texts(units.iter().map(|u| u.preview_text.as_str()));
    if body.trim().is_empty() {
        return Err("ファイル本文が空です。".into());
    }
    check_file_body_size(&body)?;
    Ok(body)
}

pub fn unit_body_from_index(
    settings: &Settings,
    local: &TantivyBackend,
    mail: &TantivyBackend,
    paragraph_id: &str,
) -> Result<String, String> {
    let id = paragraph_id.trim();
    if id.is_empty() {
        return Err("段落 ID が無い出典は段落に戻せません。".into());
    }
    let hit = search::run_preview(settings, local, Some(mail), id)?
        .ok_or_else(|| "元の段落がインデックスにありません。全文のままにします。".to_string())?;
    let body = hit.preview_text.trim();
    if body.is_empty() {
        return Err("元の段落本文が空です。".into());
    }
    Ok(body.to_string())
}

pub fn saved_unit_body(unit_body: &str) -> Option<&str> {
    let t = unit_body.trim();
    if t.is_empty() {
        None
    } else {
        Some(t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_paragraphs_in_order() {
        let s = concat_unit_texts(["第1条 本文。", "第2条 続き。", "第3条"]);
        assert_eq!(s, "第1条 本文。\n\n第2条 続き。\n\n第3条");
    }

    #[test]
    fn stitches_overlapping_chunks() {
        let overlap = "0123456789abcdef";
        let a = format!("HELLO{overlap}");
        let b = format!("{overlap}WORLD");
        let s = concat_unit_texts([a.as_str(), b.as_str()]);
        assert_eq!(s, format!("HELLO{overlap}WORLD"));
    }

    #[test]
    fn file_body_rejects_empty() {
        let err = file_body_from_units(&[]).unwrap_err();
        assert!(err.contains("インデックス"));
    }

    #[test]
    fn rejects_over_hard_cap() {
        let s = "あ".repeat(FILE_GRAIN_HARD_CAP + 1);
        let err = check_file_body_size(&s).unwrap_err();
        assert!(err.contains("大きすぎます"));
    }

    #[test]
    fn prefers_saved_unit_body() {
        assert_eq!(saved_unit_body("  段落  "), Some("段落"));
        assert_eq!(saved_unit_body("  "), None);
    }
}
