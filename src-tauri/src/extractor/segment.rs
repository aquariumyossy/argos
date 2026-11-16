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
///
/// Docx location lines (`〔ヘッダー〕` and the other story labels) are for chat.
/// Indexing drops those labels and keeps each story line as its own unit, so a
/// short header or chart title cannot join a `第N条` paragraph.
pub fn segment_pages(pages: &[String]) -> Vec<SearchUnit> {
    let mut raw: Vec<IndexBlock> = Vec::new();
    for (page_idx, page) in pages.iter().enumerate() {
        let page_num = Some((page_idx as u32) + 1);
        for block in split_blank_line_blocks(page) {
            if let Some(prepared) = prepare_index_block(block, page_num) {
                raw.push(prepared);
            }
        }
    }

    if raw.is_empty() {
        return Vec::new();
    }

    let merged = merge_short_blocks(raw, UNIT_MIN_CHARS);
    let mut units = Vec::new();
    let mut unit_id = 0u32;
    for block in merged {
        let label = unit_label(&block.text, unit_id);
        if block.text.chars().count() <= UNIT_MAX_CHARS {
            units.push(SearchUnit {
                text: block.text,
                page: block.page,
                unit_id,
                label,
                kind: "paragraph".into(),
            });
            unit_id += 1;
            continue;
        }
        push_windowed_chunks(
            &mut units,
            &mut unit_id,
            &block.text,
            block.page,
            &label,
            UNIT_MAX_CHARS,
        );
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

struct IndexBlock {
    text: String,
    page: Option<u32>,
    /// Header, footer, note, chart, diagram, or text box. Not merged into body.
    story: bool,
}

/// Location prefixes the docx extractor adds for chat. Longest first.
const STORY_LABELS: &[&str] = &[
    "〔テキストボックス/図形〕",
    "〔文末脚注〕",
    "〔ヘッダー〕",
    "〔フッター〕",
    "〔グラフ〕",
    "〔脚注〕",
    "〔図〕",
];

fn prepare_index_block(text: String, page: Option<u32>) -> Option<IndexBlock> {
    if !text.chars().any(|c| !c.is_whitespace()) {
        return None;
    }
    if let Some(body) = strip_story_labels(&text) {
        if !body.chars().any(|c| !c.is_whitespace()) {
            return None;
        }
        return Some(IndexBlock {
            text: body,
            page,
            story: true,
        });
    }
    Some(IndexBlock {
        text,
        page,
        story: false,
    })
}

/// `Some` when `text` starts with one or more story labels. The labels are removed.
fn strip_story_labels(text: &str) -> Option<String> {
    let mut rest = text.trim_start();
    let mut stripped = false;
    loop {
        let next = STORY_LABELS
            .iter()
            .find_map(|label| rest.strip_prefix(label));
        let Some(next) = next else {
            break;
        };
        rest = next.trim_start();
        stripped = true;
    }
    if stripped {
        Some(rest.trim().to_string())
    } else {
        None
    }
}

fn merge_short_blocks(blocks: Vec<IndexBlock>, min_chars: usize) -> Vec<IndexBlock> {
    let mut out: Vec<IndexBlock> = Vec::new();
    for block in blocks {
        if block.story {
            out.push(block);
            continue;
        }
        if let Some(last) = out.last_mut() {
            if !last.story {
                let last_len = last.text.chars().count();
                let len = block.text.chars().count();
                if last_len < min_chars || len < min_chars {
                    last.text.push_str("\n\n");
                    last.text.push_str(&block.text);
                    if last.page.is_none() {
                        last.page = block.page;
                    }
                    continue;
                }
            }
        }
        out.push(block);
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
            '０'
                ..='９'
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
        assert!(
            units.len() >= 2,
            "got {} units: {:?}",
            units.len(),
            units.iter().map(|u| &u.label).collect::<Vec<_>>()
        );
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
        let text: String = (0..2500)
            .map(|i| char::from_u32(0x3042 + (i % 20) as u32).unwrap())
            .collect();
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

    #[test]
    fn docx_story_lines_do_not_merge_into_articles() {
        let body1 = "この契約は当事者間の取引条件を定めることを目的として締結されるものであり、十分な長さの本文を持つ。";
        let body2 = "本契約において用いる用語の定義は次のとおりとし、こちらも十分な長さの段落本文とするものである。";
        assert!(
            body1.chars().count() >= UNIT_MIN_CHARS,
            "{}",
            body1.chars().count()
        );
        assert!(
            body2.chars().count() >= UNIT_MIN_CHARS,
            "{}",
            body2.chars().count()
        );
        let text = format!(
            "〔ヘッダー〕甲株式会社\n\n\
             第一条（目的）\n\n\
             {body1}\n\n\
             〔テキストボックス/図形〕枠内の文言\n\n\
             第二条（定義）\n\n\
             {body2}\n\n\
             〔フッター〕機密\n\n\
             〔脚注〕注記\n\n\
             〔文末脚注〕末注\n\n\
             〔グラフ〕売上\n\n\
             〔図〕組織"
        );
        let units = segment_pages(&[text]);
        let article1_unit = units
            .iter()
            .find(|unit| unit.label.contains("第一条"))
            .expect("第一条 unit");
        let article2_unit = units
            .iter()
            .find(|unit| unit.label.contains("第二条"))
            .expect("第二条 unit");
        assert!(
            article1_unit.text.starts_with("第一条"),
            "{}",
            article1_unit.text
        );
        assert!(
            article2_unit.text.starts_with("第二条"),
            "{}",
            article2_unit.text
        );
        for unit in [article1_unit, article2_unit] {
            assert!(!unit.text.contains("甲株式会社"), "{}", unit.text);
            assert!(!unit.text.contains("枠内の文言"), "{}", unit.text);
            assert!(!unit.text.contains("機密"), "{}", unit.text);
            assert!(!unit.text.contains("注記"), "{}", unit.text);
            assert!(!unit.text.contains("末注"), "{}", unit.text);
            assert!(!unit.text.contains("売上"), "{}", unit.text);
            assert!(!unit.text.contains("組織"), "{}", unit.text);
            assert!(!unit.text.contains('〔'), "{}", unit.text);
            assert!(!unit.text.contains("グラフ"), "{}", unit.text);
        }
        assert_eq!(
            units
                .iter()
                .filter(|unit| unit.text == "甲株式会社")
                .count(),
            1
        );
        for story in ["枠内の文言", "機密", "注記", "末注", "売上", "組織"] {
            assert!(
                units.iter().any(|unit| unit.text == story),
                "missing {story}: {:?}",
                units
                    .iter()
                    .map(|unit| unit.text.as_str())
                    .collect::<Vec<_>>()
            );
        }
        assert!(units.iter().all(|unit| !unit.text.contains('〔')));
    }
}
