//! Split a saved attachment into 第n条 spans so a re-read can return one article.
//!
//! Headings are lines that start with 第…条 (optional の枝番, optional Markdown `#`).
//! A mention such as 第6条の規定 in the middle of a sentence is not a heading.
//! Numbers follow `legal_ref` (算用数字、全角、漢数字).

use crate::search::legal_ref::{is_numeral_char, parse_legal_numeral};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ArticleNo {
    pub main: u32,
    pub branch: Option<u32>,
}

impl ArticleNo {
    /// Arabic label the model can pass back (`第6条`, `第6条の2`).
    pub fn label(self) -> String {
        match self.branch {
            Some(branch) => format!("第{}条の{}", self.main, branch),
            None => format!("第{}条", self.main),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArticleSpan {
    pub no: ArticleNo,
    /// 附則 / 別紙 / 以上, or a later duplicate of the same number (`2`).
    pub part: Option<String>,
    /// Exact slice of the saved text. A caption line `（…）` immediately above the heading is included.
    pub text: String,
    /// 1-based character index of `text` in the indexed document.
    pub start_char: usize,
}

impl ArticleSpan {
    pub fn display_label(&self) -> String {
        match &self.part {
            Some(part) => format!("{}（{part}）", self.no.label()),
            None => self.no.label(),
        }
    }
}

struct Mark {
    start: usize,
    no: ArticleNo,
    part: Option<String>,
}

/// Article headings in document order. Each span is a substring of `text`.
/// A line that is only `附則`, `別紙`, or `以上` ends the previous article and
/// does not become part of its body. The same article number later in the
/// document keeps its own span and label.
pub fn article_spans(text: &str) -> Vec<ArticleSpan> {
    let mut marks: Vec<Mark> = Vec::new();
    let mut boundaries: Vec<usize> = Vec::new();
    let mut region: Option<String> = None;
    let mut offset = 0usize;
    let mut prev: Option<(usize, String)> = None;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let lead = line.len() - trimmed.len();
        let at = offset + lead;
        let logical = strip_hashes(trimmed.trim_end());
        if let Some(part) = boundary_part(&logical) {
            boundaries.push(at);
            region = Some(part);
        } else if let Some(no) = article_no_at_line_start(trimmed) {
            let mut start = at;
            if let Some((prev_at, prev_text)) = &prev {
                if is_standalone_caption(prev_text) {
                    start = *prev_at;
                }
            }
            marks.push(Mark {
                start,
                no,
                part: region.clone(),
            });
        }
        if !logical.is_empty() {
            prev = Some((at, logical));
        }
        offset += line.len();
    }
    let mut out = Vec::with_capacity(marks.len());
    for (index, mark) in marks.iter().enumerate() {
        let next_mark = marks.get(index + 1).map(|next| next.start);
        let next_boundary = boundaries.iter().copied().find(|at| *at > mark.start);
        let end = match (next_mark, next_boundary) {
            (Some(mark_at), Some(boundary_at)) => mark_at.min(boundary_at),
            (Some(mark_at), None) => mark_at,
            (None, Some(boundary_at)) => boundary_at,
            (None, None) => text.len(),
        };
        if end <= mark.start || !text.is_char_boundary(mark.start) || !text.is_char_boundary(end) {
            continue;
        }
        let slice = text[mark.start..end].trim_end();
        if slice.is_empty() {
            continue;
        }
        out.push(ArticleSpan {
            no: mark.no,
            part: mark.part.clone(),
            text: slice.to_string(),
            start_char: text[..mark.start].chars().count() + 1,
        });
    }
    disambiguate_duplicate_numbers(&mut out);
    out
}

fn disambiguate_duplicate_numbers(spans: &mut [ArticleSpan]) {
    for index in 0..spans.len() {
        if spans[index].part.is_some() {
            continue;
        }
        let no = spans[index].no;
        let earlier = spans[..index].iter().filter(|span| span.no == no).count();
        if earlier > 0 {
            spans[index].part = Some((earlier + 1).to_string());
        }
    }
}

/// `6`, `6の2`, `第6条の2`, `第六条の二`, `第６条の２`.
pub fn parse_article_token(raw: &str) -> Option<ArticleNo> {
    let mut s = raw.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(rest) = s.strip_prefix('第') {
        s = rest.trim();
    }
    let (main_part, branch_part) = if let Some((main, branch)) = s.split_once('の') {
        (main.trim(), Some(branch.trim()))
    } else {
        (s, None)
    };
    let main_part = main_part.trim_end_matches('条').trim();
    let main = parse_legal_numeral(main_part).filter(|n| *n > 0)?;
    let branch = match branch_part {
        None => None,
        Some(branch) => {
            let branch = branch
                .split(['（', '('])
                .next()
                .unwrap_or(branch)
                .trim()
                .trim_end_matches('条')
                .trim();
            if branch.is_empty() {
                return None;
            }
            Some(parse_legal_numeral(branch).filter(|n| *n > 0)?)
        }
    };
    Some(ArticleNo { main, branch })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CharRangeError {
    /// `start` was below 1.
    Start,
    /// `end` was below `start`.
    Order,
    /// `start` was past the last character.
    PastEnd { len: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharRange {
    pub text: String,
    /// 1-based inclusive start that was applied.
    pub start: usize,
    /// 1-based inclusive end that was applied.
    pub end: usize,
    pub len: usize,
    /// The requested end was past `len` and was pulled back to the last character.
    pub clamped: bool,
}

/// 1-based inclusive character range. `end == None` runs through the last character.
/// Indexes count Unicode scalar values, the same way the rest of the chat budget does.
pub fn char_range(
    text: &str,
    start: usize,
    end: Option<usize>,
) -> Result<CharRange, CharRangeError> {
    let len = text.chars().count();
    if start < 1 {
        return Err(CharRangeError::Start);
    }
    if len == 0 || start > len {
        return Err(CharRangeError::PastEnd { len });
    }
    let requested = end.unwrap_or(len);
    if requested < start {
        return Err(CharRangeError::Order);
    }
    let clamped = requested > len;
    let end = requested.min(len);
    let text: String = text.chars().skip(start - 1).take(end - start + 1).collect();
    Ok(CharRange {
        text,
        start,
        end,
        len,
        clamped,
    })
}

/// Labels in document order, without duplicates.
pub fn available_labels(spans: &[ArticleSpan]) -> Vec<String> {
    let mut out = Vec::new();
    for span in spans {
        let label = span.display_label();
        if !out.contains(&label) {
            out.push(label);
        }
    }
    out
}

fn strip_hashes(line: &str) -> String {
    let mut s = line.trim();
    loop {
        if let Some(rest) = s.strip_prefix('#') {
            s = rest.trim_start();
            continue;
        }
        break;
    }
    s.to_string()
}

fn boundary_part(line: &str) -> Option<String> {
    let compact: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    if is_supplement_heading(&compact) {
        return Some("附則".into());
    }
    if compact == "別紙" || compact == "（別紙）" || compact == "(別紙)" {
        return Some("別紙".into());
    }
    if let Some(rest) = compact.strip_prefix("別紙") {
        let id: String = rest.chars().take_while(|c| is_exhibit_char(*c)).collect();
        let after: String = rest.chars().skip(id.chars().count()).collect();
        if !id.is_empty()
            && (after.is_empty() || (after.starts_with('（') && after.ends_with('）')))
        {
            return Some(format!("別紙{id}"));
        }
    }
    if compact.starts_with("（別紙") && compact.ends_with('）') && compact.chars().count() <= 12 {
        let inner = compact.trim_start_matches('（').trim_end_matches('）');
        return Some(inner.to_string());
    }
    if compact == "以上" || compact == "以上。" || compact == "（以上）" {
        return Some("以上".into());
    }
    None
}

fn is_exhibit_char(c: char) -> bool {
    c.is_ascii_digit() || ('０'..='９').contains(&c)
}

/// A supplement heading, not a sentence that merely mentions 附則.
/// `附則`, `附則。`, `附　則`, and `附則（令和2年4月1日施行）` are headings.
/// `附則に…` and `附則第2条の規定に…` stay in the current article.
fn is_supplement_heading(compact: &str) -> bool {
    let Some(rest) = compact.strip_prefix("附則") else {
        return false;
    };
    let rest = rest.trim_end_matches('。');
    if rest.is_empty() {
        return true;
    }
    is_standalone_caption(rest)
}

fn is_standalone_caption(line: &str) -> bool {
    let s = line.trim();
    if s.is_empty() {
        return false;
    }
    let (open, close) = if s.starts_with('（') && s.ends_with('）') {
        ('（', '）')
    } else if s.starts_with('(') && s.ends_with(')') {
        ('(', ')')
    } else {
        return false;
    };
    let chars: Vec<char> = s.chars().collect();
    let mut depth = 0i32;
    for (index, c) in chars.iter().enumerate() {
        if *c == open {
            depth += 1;
        } else if *c == close {
            depth -= 1;
            if depth == 0 {
                return index + 1 == chars.len() && s.chars().count() <= 42;
            }
        }
    }
    false
}

fn article_no_at_line_start(line: &str) -> Option<ArticleNo> {
    let mut s = line.trim();
    loop {
        if let Some(rest) = s.strip_prefix('#') {
            s = rest.trim_start();
            continue;
        }
        break;
    }
    let chars: Vec<char> = s.chars().collect();
    if chars.first() != Some(&'第') {
        return None;
    }
    let mut i = skip_ws(&chars, 1);
    let num_start = i;
    while i < chars.len() && is_numeral_char(chars[i]) {
        i += 1;
    }
    if i == num_start {
        return None;
    }
    let main_str: String = chars[num_start..i].iter().collect();
    let main = parse_legal_numeral(&main_str).filter(|n| *n > 0)?;
    i = skip_ws(&chars, i);
    if i >= chars.len() || chars[i] != '条' {
        return None;
    }
    i += 1;
    let mut branch = None;
    let after_article = skip_ws(&chars, i);
    if after_article < chars.len() && chars[after_article] == 'の' {
        let bstart = skip_ws(&chars, after_article + 1);
        let mut j = bstart;
        while j < chars.len() && is_numeral_char(chars[j]) {
            j += 1;
        }
        if j == bstart {
            // 第6条の規定 — a cross-reference, not 第6条の2.
            return None;
        }
        let branch_str: String = chars[bstart..j].iter().collect();
        branch = Some(parse_legal_numeral(&branch_str).filter(|n| *n > 0)?);
        i = j;
    }
    let tail: String = chars[i..].iter().collect();
    if !is_heading_tail(&tail) {
        return None;
    }
    Some(ArticleNo { main, branch })
}

fn skip_ws(chars: &[char], mut i: usize) -> usize {
    while i < chars.len() && chars[i].is_whitespace() {
        i += 1;
    }
    i
}

fn is_heading_tail(tail: &str) -> bool {
    let tail = tail.trim_end();
    if tail.trim().is_empty() {
        return true;
    }
    if tail.starts_with(|c: char| c.is_whitespace()) {
        return true;
    }
    if let Some(after) = title_remainder(tail) {
        return !starts_with_cross_ref(after.trim_start());
    }
    false
}

/// Text after a `（見出し）` whose closer is the matching outer paren.
fn title_remainder(tail: &str) -> Option<&str> {
    let (open, close) = if tail.starts_with('（') {
        ('（', '）')
    } else if tail.starts_with('(') {
        ('(', ')')
    } else {
        return None;
    };
    let chars: Vec<(usize, char)> = tail.char_indices().collect();
    let mut depth = 0i32;
    let mut close_byte: Option<usize> = None;
    for (byte, c) in &chars {
        if *c == open {
            depth += 1;
        } else if *c == close {
            depth -= 1;
            if depth == 0 {
                close_byte = Some(*byte);
                break;
            }
        }
    }
    let close_byte = close_byte?;
    let inner = tail.get(open.len_utf8()..close_byte)?;
    if inner.chars().count() > 80 {
        return None;
    }
    let after = close_byte + close.len_utf8();
    Some(tail.get(after..).unwrap_or(""))
}

/// A same-line continuation that points at another provision, not the article's own body.
/// Bare は / に / の / を / において stay headings: 約款 often writes the body on the heading line.
fn starts_with_cross_ref(after: &str) -> bool {
    let after = after.trim_start();
    if after.is_empty() {
        return false;
    }
    if after.starts_with('、') || after.starts_with('，') || after.starts_with(',') {
        return true;
    }
    if item_provision_ref(after) {
        return true;
    }
    const PHRASES: &[&str] = &[
        "に定める",
        "に定め",
        "に従",
        "にかか",
        "に関わ",
        "に係わ",
        "に基",
        "により",
        "によって",
        "による",
        "に規定",
        "に掲",
        "に記載",
        "に該当",
        "に準",
        "に反",
        "に違反",
        "に抵触",
        "につき",
        "について",
        "にいう",
        "の規定",
        "の定め",
        "の場合",
        "の趣旨",
        "の適用",
        "を除",
        "を準用",
        "を適用",
        "及び",
        "および",
        "又は",
        "または",
        "若しくは",
        "もしくは",
        "から",
        "まで",
    ];
    PHRASES.iter().any(|phrase| after.starts_with(phrase))
}

/// `第1項の規定` / `第一項の規定` after a title is a cross-reference, not the article body.
fn item_provision_ref(after: &str) -> bool {
    let chars: Vec<char> = after.chars().collect();
    if chars.first() != Some(&'第') {
        return false;
    }
    let mut i = 1usize;
    let start = i;
    while i < chars.len() && is_numeral_char(chars[i]) {
        i += 1;
    }
    if i == start || i >= chars.len() {
        return false;
    }
    if chars[i] != '項' && chars[i] != '号' {
        return false;
    }
    let rest: String = chars[i + 1..].iter().collect();
    let rest = rest.trim_start();
    rest.is_empty() || rest.starts_with('の') || rest.starts_with('に')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_headings_with_branch_fullwidth_kanji_and_markdown() {
        let text = "\
前文。第3条の規定は前文の中にある。

## 第一条 （基本原則）
私権は公共の福祉に適合しなければならない。

第６条（全角）
全角数字の本文。

第６条の２（全角の枝番）
全角枝番の本文。

第六条（漢数字）
漢数字の本文。

第百二十一条の二（原状回復）
原状回復する。
";
        let spans = article_spans(text);
        let numbers: Vec<ArticleNo> = spans.iter().map(|span| span.no).collect();
        assert_eq!(
            numbers,
            vec![
                ArticleNo {
                    main: 1,
                    branch: None
                },
                ArticleNo {
                    main: 6,
                    branch: None
                },
                ArticleNo {
                    main: 6,
                    branch: Some(2)
                },
                ArticleNo {
                    main: 6,
                    branch: None
                },
                ArticleNo {
                    main: 121,
                    branch: Some(2)
                },
            ]
        );
        assert!(spans[0].text.starts_with("## 第一条"));
        assert!(spans[0]
            .text
            .contains("私権は公共の福祉に適合しなければならない。"));
        assert!(!spans[0].text.contains("全角数字の本文"));
        assert_eq!(spans[1].text, "第６条（全角）\n全角数字の本文。");
        assert!(spans[2].text.contains("全角枝番の本文。"));
        assert!(!spans[2].text.contains("漢数字の本文"));
        assert!(spans[3].text.starts_with("第六条（漢数字）"));
        assert!(spans[4].text.contains("原状回復する。"));
        assert_eq!(
            available_labels(&spans),
            vec![
                "第1条".to_string(),
                "第6条".to_string(),
                "第6条の2".to_string(),
                "第6条（2）".to_string(),
                "第121条の2".to_string()
            ]
        );
    }

    #[test]
    fn branch_article_is_not_part_of_the_main_article() {
        let text = "\
第6条（解除）
甲は解除できる。

第6条の2（中途解約）
甲は30日前に通知する。

第7条（準拠法）
日本法による。
";
        let spans = article_spans(text);
        assert_eq!(spans.len(), 3);
        assert_eq!(
            spans[0].no,
            ArticleNo {
                main: 6,
                branch: None
            }
        );
        assert_eq!(spans[0].text, "第6条（解除）\n甲は解除できる。");
        assert!(!spans[0].text.contains("30日前"));
        assert_eq!(
            spans[1].no,
            ArticleNo {
                main: 6,
                branch: Some(2)
            }
        );
        assert!(spans[1].text.contains("30日前に通知する。"));
        assert!(!spans[1].text.contains("日本法"));
        assert_eq!(spans[2].text, "第7条（準拠法）\n日本法による。");
    }

    #[test]
    fn cross_reference_stays_inside_the_current_article() {
        let text = "\
第5条（通知）
次のとおり。
第6条の規定にかかわらず、通知は書面による。
第6条（解除）に定める場合を除く。

第6条（解除）
解除の本文。
";
        let spans = article_spans(text);
        assert_eq!(spans.len(), 2, "{spans:?}");
        assert!(spans[0].text.contains("第6条の規定にかかわらず"));
        assert!(spans[0].text.contains("第6条（解除）に定める場合を除く。"));
        assert_eq!(spans[1].text, "第6条（解除）\n解除の本文。");
        assert!(article_spans("第6条の規定により解除する。\n").is_empty());
    }

    #[test]
    fn statute_line_keeps_the_body_on_the_same_line() {
        let text = "\
第一条　私権は、公共の福祉に適合しなければならない。
次の行も第一条。

第二条（解釈）
解釈の本文。
";
        let spans = article_spans(text);
        assert_eq!(spans.len(), 2);
        assert!(spans[0]
            .text
            .starts_with("第一条　私権は、公共の福祉に適合しなければならない。"));
        assert!(spans[0].text.contains("次の行も第一条。"));
        assert!(!spans[0].text.contains("解釈の本文"));
        assert_eq!(spans[1].no.main, 2);
    }

    #[test]
    fn same_line_body_after_a_title_is_part_of_the_article() {
        let text = "第6条（解除）甲は同一行に書く。\n";
        let spans = article_spans(text);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].text, "第6条（解除）甲は同一行に書く。");
    }

    #[test]
    fn parses_article_tokens() {
        assert_eq!(
            parse_article_token("6"),
            Some(ArticleNo {
                main: 6,
                branch: None
            })
        );
        assert_eq!(
            parse_article_token("第6条"),
            Some(ArticleNo {
                main: 6,
                branch: None
            })
        );
        assert_eq!(
            parse_article_token("6の2"),
            Some(ArticleNo {
                main: 6,
                branch: Some(2)
            })
        );
        assert_eq!(
            parse_article_token("第6条の2"),
            Some(ArticleNo {
                main: 6,
                branch: Some(2)
            })
        );
        assert_eq!(
            parse_article_token("第六条の二"),
            Some(ArticleNo {
                main: 6,
                branch: Some(2)
            })
        );
        assert_eq!(
            parse_article_token("第６条の２"),
            Some(ArticleNo {
                main: 6,
                branch: Some(2)
            })
        );
        assert_eq!(
            parse_article_token("第百二十一条の二"),
            Some(ArticleNo {
                main: 121,
                branch: Some(2)
            })
        );
        assert_eq!(parse_article_token("解除"), None);
        assert_eq!(parse_article_token("第0条"), None);
    }

    #[test]
    fn char_range_is_one_based_and_inclusive() {
        let text = "あいうえお";
        let mid = char_range(text, 2, Some(4)).unwrap();
        assert_eq!(mid.text, "いうえ");
        assert!(!mid.clamped);
        let tail = char_range(text, 4, None).unwrap();
        assert_eq!(tail.text, "えお");
        assert_eq!(tail.end, 5);
        let clamped = char_range(text, 2, Some(99)).unwrap();
        assert_eq!(clamped.text, "いうえお");
        assert!(clamped.clamped);
        assert_eq!(clamped.end, 5);
        assert_eq!(
            char_range(text, 0, Some(1)).unwrap_err(),
            CharRangeError::Start
        );
        assert_eq!(
            char_range(text, 3, Some(2)).unwrap_err(),
            CharRangeError::Order
        );
        assert_eq!(
            char_range(text, 9, Some(10)).unwrap_err(),
            CharRangeError::PastEnd { len: 5 }
        );
    }

    #[test]
    fn available_labels_follow_the_document() {
        let text = "第7条（後）\n後。\n\n第6条（前）\n前。\n";
        let labels = available_labels(&article_spans(text));
        assert_eq!(labels, vec!["第7条".to_string(), "第6条".to_string()]);
    }

    #[test]
    fn caption_line_starts_the_following_article() {
        let text = "\
第1条（目的）
目的の本文。

（解除）
第2条
解除の本文。
";
        let spans = article_spans(text);
        assert_eq!(spans.len(), 2, "{spans:?}");
        assert!(!spans[0].text.contains("（解除）"), "{}", spans[0].text);
        assert!(spans[0].text.contains("目的の本文"));
        assert!(spans[1].text.starts_with("（解除）"), "{}", spans[1].text);
        assert!(spans[1].text.contains("解除の本文"));
        assert!(!spans[1].text.contains("目的の本文"));
    }

    #[test]
    fn titled_item_cross_reference_is_not_a_heading() {
        let text = "\
第4条（通知）
通知は書面による。
第6条（解除）第1項の規定にかかわらず、甲は通知できる。
第六条（解除）第一項の規定にかかわらず、乙は解除できない。

第6条（解除）
解除の本文。
";
        let spans = article_spans(text);
        assert_eq!(spans.len(), 2, "{spans:?}");
        assert!(spans[0].text.contains("第1項の規定にかかわらず"));
        assert!(spans[0].text.contains("第一項の規定にかかわらず"));
        assert_eq!(spans[1].text, "第6条（解除）\n解除の本文。");
        assert!(article_spans("第6条の規定により解除する。\n").is_empty());
    }

    #[test]
    fn nested_title_parentheses_belong_to_the_heading() {
        let text = "第3条（秘密情報（個人情報を含む。）の管理）\n甲は秘密を守る。\n";
        let spans = article_spans(text);
        assert_eq!(spans.len(), 1, "{spans:?}");
        assert_eq!(spans[0].no.main, 3);
        assert!(spans[0].text.contains("個人情報を含む"));
        assert!(spans[0].text.contains("の管理"));
        assert!(spans[0].text.contains("甲は秘密を守る"));
    }

    #[test]
    fn supplement_and_closing_do_not_merge_into_the_last_article() {
        let text = "\
第1条（本則）
本則の本文。

第2条（終了）
終了の本文。
以上

甲　署名

附則
第1条（施行）
施行の本文。

別紙
第1条（仕様）
仕様の本文。
";
        let spans = article_spans(text);
        assert_eq!(spans.len(), 4, "{spans:?}");
        assert!(spans[0].text.contains("本則の本文"));
        assert!(!spans[0].text.contains("以上"));
        assert!(!spans[0].text.contains("署名"));
        assert!(spans[1].text.contains("終了の本文"));
        assert!(!spans[1].text.contains("以上"), "{}", spans[1].text);
        assert!(!spans[1].text.contains("署名"));
        assert_eq!(spans[2].display_label(), "第1条（附則）");
        assert!(spans[2].text.contains("施行の本文"));
        assert!(!spans[2].text.contains("仕様の本文"));
        assert_eq!(spans[3].display_label(), "第1条（別紙）");
        assert!(spans[3].text.contains("仕様の本文"));
        let labels = available_labels(&spans);
        assert!(labels.contains(&"第1条".to_string()), "{labels:?}");
        assert!(labels.contains(&"第1条（附則）".to_string()), "{labels:?}");
        assert!(labels.contains(&"第1条（別紙）".to_string()), "{labels:?}");
    }

    #[test]
    fn one_line_yakkan_body_is_its_own_article() {
        let text = "\
第1条（定義）において「本件製品」とは製品をいう。
第5条（契約期間）は、令和2年4月1日から1年間とする。
第6条（解除）に定める場合を除く。
";
        let spans = article_spans(text);
        assert!(
            spans.iter().any(|span| {
                span.no.main == 1 && span.text.contains("本件製品") && span.text.contains("において")
            }),
            "{spans:?}"
        );
        let fifth = spans.iter().find(|span| span.no.main == 5).expect("第5条");
        assert!(fifth.text.contains("令和2年4月1日"), "{}", fifth.text);
        assert!(fifth.text.contains("に定める場合を除く"), "{}", fifth.text);
        assert!(spans.iter().all(|span| span.no.main != 6), "{spans:?}");
        assert!(article_spans("第6条（解除）に定める場合を除く。\n").is_empty());
    }

    #[test]
    fn survival_clause_listing_titles_is_not_a_new_article() {
        let text = "\
第4条（終了）
本契約が終了したときも、次の条は残る。
第5条（秘密保持）、第8条（損害賠償）及び本条の規定は、本契約終了後も有効に存続する。
";
        let spans = article_spans(text);
        assert_eq!(spans.len(), 1, "{spans:?}");
        assert_eq!(spans[0].no.main, 4);
        assert!(spans[0].text.contains("存続する"));
        assert!(spans.iter().all(|span| span.display_label() != "第5条"));
        let labels = available_labels(&spans);
        assert!(!labels.contains(&"第5条".to_string()), "{labels:?}");
    }

    #[test]
    fn dated_supplement_is_a_boundary_with_or_without_a_following_article() {
        let with_article = "\
第2条（終了）
終了の本文。
附則（令和2年4月1日施行）
第1条（施行）
施行の本文。
";
        let spans = article_spans(with_article);
        assert!(
            !spans.iter().any(|span| span.no.main == 2 && span.text.contains("施行の本文")),
            "{spans:?}"
        );
        assert!(!spans[0].text.contains("附則"), "{}", spans[0].text);
        let supplement = spans.iter().find(|span| span.no.main == 1).expect("附則の第1条");
        assert_eq!(supplement.display_label(), "第1条（附則）");
        assert!(supplement.text.contains("施行の本文"));

        let bare = "\
第2条（終了）
終了の本文。
附則（令和2年4月1日施行）
この附則は公布の日から施行する。
";
        let spans = article_spans(bare);
        assert_eq!(spans.len(), 1, "{spans:?}");
        assert!(!spans[0].text.contains("公布"), "{}", spans[0].text);
        assert!(!spans[0].text.contains("附則"), "{}", spans[0].text);
    }

    #[test]
    fn numbered_exhibits_keep_separate_labels() {
        let text = "\
第1条（本則）
本則の本文。

別紙1
第1条（仕様）
仕様の本文。

別紙2
第1条（図面）
図面の本文。
";
        let spans = article_spans(text);
        let labels = available_labels(&spans);
        assert!(labels.contains(&"第1条".to_string()), "{labels:?}");
        assert!(labels.contains(&"第1条（別紙1）".to_string()), "{labels:?}");
        assert!(labels.contains(&"第1条（別紙2）".to_string()), "{labels:?}");
        assert_eq!(
            labels.iter().filter(|label| *label == "第1条（別紙）").count(),
            0,
            "{labels:?}"
        );
        let spec = spans.iter().find(|span| span.text.contains("仕様の本文")).unwrap();
        let drawing = spans.iter().find(|span| span.text.contains("図面の本文")).unwrap();
        assert_ne!(spec.display_label(), drawing.display_label());
        assert!(spec.text.contains("仕様の本文") && !spec.text.contains("図面の本文"));
        assert!(drawing.text.contains("図面の本文") && !drawing.text.contains("仕様の本文"));
    }

    #[test]
    fn a_sentence_that_starts_with_supplement_stays_in_the_article() {
        let text = "\
第8条（損害賠償）
甲は損害を賠償する。
附則に別段の定めがある場合を除く。
乙は遅延損害金を支払う。
第9条（解除）
解除できる。
";
        let spans = article_spans(text);
        assert_eq!(spans.len(), 2, "{spans:?}");
        assert_eq!(spans[0].display_label(), "第8条");
        assert!(spans[0].text.contains("附則に別段の定めがある場合を除く。"), "{}", spans[0].text);
        assert!(spans[0].text.contains("乙は遅延損害金を支払う。"), "{}", spans[0].text);
        assert_eq!(spans[1].display_label(), "第9条", "{spans:?}");
        assert!(!spans[1].display_label().contains("附則"));
        let cross = "\
第8条（損害賠償）
甲は損害を賠償する。
附則第2条の規定にかかわらず、乙は支払う。
第9条（解除）
解除できる。
";
        let spans = article_spans(cross);
        assert!(spans[0].text.contains("附則第2条の規定にかかわらず"), "{spans:?}");
        assert_eq!(spans.last().unwrap().display_label(), "第9条");
        for heading in ["附則", "附則。", "附　則", "## 附則"] {
            let sample = format!("第2条（終了）\n終了。\n{heading}\n第1条（施行）\n施行の本文。\n");
            let spans = article_spans(&sample);
            assert_eq!(
                spans.iter().find(|span| span.no.main == 1).map(|span| span.display_label()),
                Some("第1条（附則）".into()),
                "{heading}: {spans:?}"
            );
            assert!(!spans[0].text.contains("施行の本文"), "{heading}");
        }
    }
}
