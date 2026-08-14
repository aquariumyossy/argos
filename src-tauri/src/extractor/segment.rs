//! Morphological paragraph segmentation for search indexing.
//!
//! Split on blank lines (and DOCX `w:p` text already joined with blank lines).
//! Article patterns like `第N条` are used only for optional labels, not boundaries.

/// Merge units shorter than this (character count) into neighbors.
pub const UNIT_MIN_CHARS: usize = 40;
/// Split units longer than this into sub-chunks (parent label kept).
pub const UNIT_MAX_CHARS: usize = 1000;
/// Mail bodies stay one unit unless longer than this (no blank-line split).
pub const MAIL_UNIT_MAX_CHARS: usize = 16000;
/// Overlap when splitting oversized units (boundary-straddle only; keep tiny).
pub const UNIT_SPLIT_OVERLAP: usize = 16;

#[derive(Debug, Clone)]
pub struct SearchUnit {
    pub text: String,
    pub page: Option<u32>,
    /// Stable within a file for this index build (0-based sequence).
    pub unit_id: u32,
    pub label: String,
    /// `paragraph`, `message` (one email), or `chunk` (oversized split).
    pub kind: String,
}

/// Segment extracted pages into search units (blank-line first, then min/max rules).
pub fn segment_pages(pages: &[String]) -> Vec<SearchUnit> {
    let mut raw: Vec<(String, Option<u32>)> = Vec::new();
    for (page_idx, page) in pages.iter().enumerate() {
        let page_num = Some((page_idx as u32) + 1);
        for block in split_blank_line_blocks(page) {
            if !block.chars().any(|c| !c.is_whitespace()) {
                continue;
            }
            raw.push((block, page_num));
        }
    }

    if raw.is_empty() {
        return Vec::new();
    }

    let merged = merge_short_blocks(raw, UNIT_MIN_CHARS);
    let mut units = Vec::new();
    let mut unit_id = 0u32;
    for (text, page) in merged {
        let label = unit_label(&text, unit_id);
        if text.chars().count() <= UNIT_MAX_CHARS {
            units.push(SearchUnit {
                text,
                page,
                unit_id,
                label,
                kind: "paragraph".into(),
            });
            unit_id += 1;
            continue;
        }
        push_windowed_chunks(&mut units, &mut unit_id, &text, page, &label, UNIT_MAX_CHARS);
    }
    units
}

/// One search unit per email unless the body exceeds [`MAIL_UNIT_MAX_CHARS`].
/// Does not split on blank lines (greetings, signatures, quoted replies stay together).
pub fn segment_mail_body(body: &str) -> Vec<SearchUnit> {
    let text = body.trim();
    if text.is_empty() {
        return Vec::new();
    }
    let label = unit_label(text, 0);
    if text.chars().count() <= MAIL_UNIT_MAX_CHARS {
        return vec![SearchUnit {
            text: text.to_string(),
            page: Some(1),
            unit_id: 0,
            label,
            kind: "message".into(),
        }];
    }
    let mut units = Vec::new();
    let mut unit_id = 0u32;
    push_windowed_chunks(
        &mut units,
        &mut unit_id,
        text,
        Some(1),
        &label,
        MAIL_UNIT_MAX_CHARS,
    );
    units
}

fn push_windowed_chunks(
    units: &mut Vec<SearchUnit>,
    unit_id: &mut u32,
    text: &str,
    page: Option<u32>,
    label: &str,
    max_chars: usize,
) {
    let chars: Vec<char> = text.chars().collect();
    let mut start = 0usize;
    while start < chars.len() {
        let end = (start + max_chars).min(chars.len());
        let piece: String = chars[start..end].iter().collect();
        if piece.chars().any(|c| !c.is_whitespace()) {
            units.push(SearchUnit {
                text: piece,
                page,
                unit_id: *unit_id,
                label: label.to_string(),
                kind: "chunk".into(),
            });
            *unit_id += 1;
        }
        if end >= chars.len() {
            break;
        }
        start = end.saturating_sub(UNIT_SPLIT_OVERLAP).max(start + 1);
    }
}

fn split_blank_line_blocks(text: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current = String::new();
    let mut blank_run = 0usize;
    for line in text.lines() {
        if line.chars().all(|c| c.is_whitespace()) {
            blank_run += 1;
            if blank_run >= 1 && !current.is_empty() {
                blocks.push(current.trim().to_string());
                current.clear();
            }
            continue;
        }
        blank_run = 0;
        if !current.is_empty() {
            current.push('\n');
        }
        current.push_str(line.trim_end());
    }
    if !current.is_empty() {
        blocks.push(current.trim().to_string());
    }
    blocks
}

fn merge_short_blocks(
    blocks: Vec<(String, Option<u32>)>,
    min_chars: usize,
) -> Vec<(String, Option<u32>)> {
    let mut out: Vec<(String, Option<u32>)> = Vec::new();
    for (text, page) in blocks {
        let len = text.chars().count();
        if let Some(last) = out.last_mut() {
            let last_len = last.0.chars().count();
            if last_len < min_chars || len < min_chars {
                last.0.push_str("\n\n");
                last.0.push_str(&text);
                if last.1.is_none() {
                    last.1 = page;
                }
                continue;
            }
        }
        out.push((text, page));
    }
    out
}

fn unit_label(text: &str, unit_id: u32) -> String {
    if let Some(label) = article_label_at_start(text) {
        return label;
    }
    let compact: String = text
        .chars()
        .filter(|c| *c != '\n' && *c != '\r')
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let chars: Vec<char> = compact.chars().collect();
    if chars.is_empty() {
        return format!("¶{}", unit_id + 1);
    }
    if chars.len() > 36 {
        let head: String = chars[..36].iter().collect();
        format!("{head}…")
    } else {
        compact
    }
}

/// Best-effort label from a leading `第…条` (optional heading in parentheses).
fn article_label_at_start(text: &str) -> Option<String> {
    let mut line = text.lines().next()?.trim();
    while let Some(stripped) = line.strip_prefix('#') {
        line = stripped.trim_start();
    }
    if !line.starts_with('第') {
        return None;
    }
    let jou = line.find('条')?;
    // `条` is 3 bytes in UTF-8; ensure we end on a char boundary.
    let after_jou = jou + '条'.len_utf8();
    if after_jou > line.len() || !line.is_char_boundary(after_jou) {
        return None;
    }
    let mut end = after_jou;
    // Optional branch: の二 / の2
    let rest = &line[end..];
    if let Some(stripped) = rest.strip_prefix('の') {
        let mut i = 0;
        for c in stripped.chars() {
            if is_article_numeral(c) {
                i += c.len_utf8();
            } else {
                break;
            }
        }
        if i > 0 {
            end += 'の'.len_utf8() + i;
        }
    }
    // Optional （見出し）
    let rest = line[end..].trim_start();
    let trimmed = end + (line[end..].len() - rest.len());
    if rest.starts_with('（') || rest.starts_with('(') {
        let close = if rest.starts_with('（') {
            rest.find('）')
        } else {
            rest.find(')')
        };
        if let Some(c) = close {
            end = trimmed
                + c
                + if rest.starts_with('（') {
                    '）'.len_utf8()
                } else {
                    1
                };
        }
    }
    let label = line[..end].trim();
    // Guard against absurdly long "labels"
    if label.chars().count() > 40 || label.chars().count() < 2 {
        return None;
    }
    Some(label.to_string())
}

fn is_article_numeral(c: char) -> bool {
    c.is_ascii_digit()
        || matches!(
            c,
            '０'..='９'
                | '〇'
                | '零'
                | '一'
                | '二'
                | '三'
                | '四'
                | '五'
                | '六'
                | '七'
                | '八'
                | '九'
                | '十'
                | '百'
                | '千'
                | '万'
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_lines_split_articles() {
        let text = "\
## 第一条 （基本原則）
- **１**: 私権は公共の福祉に適合しなければならない。

## 第二条 （解釈の基準）
- ****: この法律は個人の尊厳を旨として解釈する。
";
        let units = segment_pages(&[text.into()]);
        assert!(units.len() >= 2, "got {} units: {:?}", units.len(), units.iter().map(|u| &u.label).collect::<Vec<_>>());
        assert!(
            units[0].label.contains("第一条"),
            "label={}",
            units[0].label
        );
        assert!(
            units.iter().any(|u| u.label.contains("第二条")),
            "labels={:?}",
            units.iter().map(|u| &u.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn short_blocks_merge() {
        let text = "見出しだけ\n\n本文が続く段落です。十分に長い内容をここに書きます。";
        let units = segment_pages(&[text.into()]);
        assert_eq!(units.len(), 1, "short heading should merge: {:?}", units);
    }

    #[test]
    fn long_block_splits() {
        // Varied content so adjacent windows are not identical strings.
        let text: String = (0..2500).map(|i| char::from_u32(0x3042 + (i % 20) as u32).unwrap()).collect();
        let units = segment_pages(&[text]);
        assert!(units.len() >= 2);
        assert!(units.iter().all(|u| u.kind == "chunk"));
        assert_ne!(units[0].text, units[1].text);
    }

    #[test]
    fn article_label_kanji_and_branch() {
        assert_eq!(
            article_label_at_start("第百二十一条の二 （原状回復）\n本文"),
            Some("第百二十一条の二 （原状回復）".into())
        );
        assert_eq!(
            article_label_at_start("第三条の二\n本文"),
            Some("第三条の二".into())
        );
    }

    #[test]
    fn mail_keeps_blank_line_paragraphs_as_one_unit() {
        let body = "\
○○様

お世話になっております。△△の□□です。

先日ご相談の件、下記のとおりご連絡します。

よろしくお願いいたします。
";
        let units = segment_mail_body(body);
        assert_eq!(units.len(), 1, "got {} units: {:?}", units.len(), units);
        assert_eq!(units[0].kind, "message");
        assert!(units[0].text.contains("先日ご相談"));
        assert!(units[0].text.contains("よろしくお願い"));
    }

    #[test]
    fn mail_oversized_body_splits_into_chunks() {
        let text: String = (0..MAIL_UNIT_MAX_CHARS + 500)
            .map(|i| char::from_u32(0x3042 + (i % 20) as u32).unwrap())
            .collect();
        let units = segment_mail_body(&text);
        assert!(units.len() >= 2, "got {} units", units.len());
        assert!(units.iter().all(|u| u.kind == "chunk"));
        assert_ne!(units[0].text, units[1].text);
    }

    #[test]
    fn mail_whitespace_only_is_empty() {
        assert!(segment_mail_body("  \n\n  ").is_empty());
    }
}
