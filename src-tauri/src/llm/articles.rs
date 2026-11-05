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
    /// Exact slice of the saved text, from this heading through the line before the next.
    pub text: String,
}

/// Article headings in document order. The text of each span is a substring of `text`.
pub fn article_spans(text: &str) -> Vec<ArticleSpan> {
    let mut marks: Vec<(usize, ArticleNo)> = Vec::new();
    let mut offset = 0usize;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let lead = line.len() - trimmed.len();
        if let Some(no) = article_no_at_line_start(trimmed) {
            marks.push((offset + lead, no));
        }
        offset += line.len();
    }
    let mut out = Vec::with_capacity(marks.len());
    for (index, (start, no)) in marks.iter().enumerate() {
        let end = marks
            .get(index + 1)
            .map(|(next, _)| *next)
            .unwrap_or(text.len());
        let slice = text[*start..end].trim_end();
        if slice.is_empty() {
            continue;
        }
        out.push(ArticleSpan {
            no: *no,
            text: slice.to_string(),
        });
    }
    out
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
        let label = span.no.label();
        if !out.contains(&label) {
            out.push(label);
        }
    }
    out
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

/// Text after a short `（見出し）` / `(heading)`, when the tail begins with one.
fn title_remainder(tail: &str) -> Option<&str> {
    let (open, close) = if tail.starts_with('（') {
        ("（", "）")
    } else if tail.starts_with('(') {
        ("(", ")")
    } else {
        return None;
    };
    let rest = tail.get(open.len()..)?;
    let end = rest.find(close)?;
    let inner = &rest[..end];
    if inner.chars().count() > 40 {
        return None;
    }
    Some(&rest[end + close.len()..])
}

fn starts_with_cross_ref(after: &str) -> bool {
    if after.is_empty() {
        return false;
    }
    const PREFIXES: &[&str] = &[
        "について",
        "により",
        "によって",
        "による",
        "及び",
        "および",
        "又は",
        "または",
        "若しくは",
        "もしくは",
        "から",
        "まで",
        "に",
        "の",
        "を",
        "が",
        "は",
        "と",
        "で",
        "へ",
        "や",
    ];
    PREFIXES.iter().any(|prefix| after.starts_with(prefix))
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
}
