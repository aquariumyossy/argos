//! Title and memo search shared with the notes list.
//!
//! The notes window folds with NFKC and lowercase before a substring check
//! (`matchesListQuery`). This is the same fold, so fullwidth letters and
//! halfwidth kana match the way the list does.

use unicode_normalization::UnicodeNormalization;

pub const NOTE_SEARCH_LIMIT: usize = 8;
const SNIPPET_RADIUS: usize = 40;

pub fn fold_note_text(text: &str) -> String {
    text.nfkc().flat_map(|c| c.to_lowercase()).collect()
}

/// Character span in `text` whose folded form contains `needle` (already folded).
fn folded_span(text: &str, needle: &str) -> Option<(usize, usize)> {
    if needle.is_empty() {
        return None;
    }
    let mut folded = String::new();
    let mut map: Vec<usize> = Vec::new();
    for (i, ch) in text.chars().enumerate() {
        let piece: String = ch.nfkc().flat_map(|c| c.to_lowercase()).collect();
        for _ in piece.chars() {
            map.push(i);
        }
        folded.push_str(&piece);
    }
    let byte = folded.find(needle)?;
    let start_chars = folded[..byte].chars().count();
    let needle_chars = needle.chars().count();
    let end_chars = start_chars + needle_chars;
    if end_chars == 0 || end_chars > map.len() {
        return None;
    }
    let orig_start = map[start_chars];
    let orig_end = map[end_chars - 1] + 1;
    Some((orig_start, orig_end))
}

fn squash(text: &str) -> String {
    let mut out = String::new();
    let mut space = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            if !out.is_empty() {
                space = true;
            }
            continue;
        }
        if space {
            out.push(' ');
            space = false;
        }
        out.push(ch);
    }
    out
}

fn window(text: &str, start: usize, end: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    let from = start.saturating_sub(SNIPPET_RADIUS);
    let to = (end + SNIPPET_RADIUS).min(chars.len());
    let mut body = squash(&chars[from..to].iter().collect::<String>());
    if from > 0 && !body.is_empty() {
        body.insert(0, '…');
    }
    if to < chars.len() && !body.is_empty() {
        body.push('…');
    }
    body
}

/// `Some` when `query` occurs in the title or the memo. The string is a short
/// excerpt of the memo, or a title-only note when the memo has no match.
pub fn match_snippet(title: &str, memo: &str, query: &str) -> Option<String> {
    let needle = fold_note_text(query.trim());
    if needle.is_empty() {
        return None;
    }
    if let Some((start, end)) = folded_span(memo, &needle) {
        let snippet = window(memo, start, end);
        if snippet.is_empty() {
            return Some("本文が一致".into());
        }
        return Some(snippet);
    }
    if fold_note_text(title).contains(&needle) {
        let chars: Vec<char> = memo.chars().collect();
        if chars.is_empty() {
            return Some("題名が一致".into());
        }
        let head = window(memo, 0, chars.len().min(SNIPPET_RADIUS));
        if head.is_empty() {
            return Some("題名が一致".into());
        }
        return Some(head);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fullwidth_and_halfwidth_kana_match() {
        let snippet = match_snippet("事件", "契約はＡＢＣです。", "abc").unwrap();
        assert!(snippet.contains("ＡＢＣ"), "{snippet}");
        let kana = match_snippet("メモ", "当事者はｶﾀｶﾅ商事", "カタカナ").unwrap();
        assert!(kana.contains("ｶﾀｶﾅ"), "{kana}");
    }

    #[test]
    fn title_hit_without_a_memo_match_still_returns() {
        assert_eq!(
            match_snippet("解除の要件", "本文は別の話", "解除"),
            Some("本文は別の話".into())
        );
        assert_eq!(
            match_snippet("解除の要件", "", "解除"),
            Some("題名が一致".into())
        );
    }

    #[test]
    fn a_miss_and_an_empty_query_are_none() {
        assert!(match_snippet("事件", "本文", "存在しない").is_none());
        assert!(match_snippet("事件", "本文", "  ").is_none());
    }

    #[test]
    fn snippet_is_a_window_around_the_hit() {
        let memo = format!("{}解除{}", "あ".repeat(80), "い".repeat(80));
        let snippet = match_snippet("題", &memo, "解除").unwrap();
        assert!(snippet.starts_with('…'), "{snippet}");
        assert!(snippet.ends_with('…'), "{snippet}");
        assert!(snippet.contains("解除"), "{snippet}");
        assert!(snippet.chars().count() < memo.chars().count());
    }
}
