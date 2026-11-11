//! Mark statute citations the chat did not verify.

use crate::statute::xmlparse::{article_spans, parse_article_query};

const TOOL_RESULT_OMITTED: &str = "（文脈の上限に達したため、この結果は省略しました）";
const TOOL_RESULT_TRUNCATION_NOTE: &str = "…（長いので省略しました）";

pub const LLM_STATUTE_HINT: &str = "\
条文を回答に書く前に law_article で、その条の本文を取得してください。1回に10条まで、1つの回答で10回までです。この回数はウェブ検索や他のツール往復には数えません。返ってきた見出しと本文だけを根拠にし、本文にない効果は書かないでください。取得できなかった条番号には「要確認」と付けてください。";
pub const LLM_STATUTE_SENTINEL: &str = "law_article で、その条の本文を取得";

const DENY_LAW_NAME: &[&str] = &[
    "方法",
    "手法",
    "療法",
    "用法",
    "違法",
    "適法",
    "合法",
    "魔法",
    "司法",
    "立法",
    "憲法改正",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedCite {
    pub names: Vec<String>,
    pub key: String,
}

#[derive(Debug, Clone)]
pub struct ToolArticle {
    pub names: Vec<String>,
    pub article_num: String,
    pub heading: String,
    pub body: String,
    pub as_of: String,
    pub verified: bool,
    pub offline: bool,
    pub note: String,
    pub law_query: String,
    pub article_query: String,
}

pub fn format_tool_result(articles: &[ToolArticle], omitted: usize) -> String {
    let mut out = String::new();
    for article in articles {
        if article.verified {
            out.push_str("確認済み: ");
            out.push_str(&article.names.join("|"));
            out.push(' ');
            out.push_str(&article.article_num);
            out.push('\n');
        } else {
            out.push_str("未確認: ");
            out.push_str(&article.law_query);
            out.push(' ');
            out.push_str(&article.article_query);
            out.push('\n');
        }
    }
    if omitted > 0 {
        out.push_str(&format!(
            "未確認: （{omitted}件は1回の上限10条を超えたため取得していません）\n"
        ));
    }
    out.push('\n');
    for article in articles {
        if article.verified {
            out.push_str(&format!(
                "【{} {}】\n",
                article.names.first().map(|s| s.as_str()).unwrap_or("法令"),
                article.article_num
            ));
            if !article.heading.is_empty() {
                out.push_str(&format!("見出し: {}\n", article.heading));
            }
            if !article.as_of.is_empty() {
                out.push_str(&format!("時点: {}\n", article.as_of));
            }
            if article.offline {
                out.push_str("改正確認: オフラインのため、保存済みの本文を返しました。\n");
            }
            out.push_str(&article.body);
            out.push_str("\n\n");
        } else {
            out.push_str(&format!(
                "【{} {}】要確認\n{}\n\n",
                article.law_query, article.article_query, article.note
            ));
        }
    }
    out
}

pub fn verified_from_tool_output(content: &str) -> Vec<VerifiedCite> {
    let mut out = Vec::new();
    for line in content.lines() {
        let Some(rest) = line.strip_prefix("確認済み: ") else {
            continue;
        };
        let Some((names, article)) = rest.rsplit_once(' ') else {
            continue;
        };
        let Some((key, _)) = parse_article_query(article) else {
            continue;
        };
        let names = names
            .split('|')
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(|name| name.to_string())
            .collect::<Vec<_>>();
        if names.is_empty() {
            continue;
        }
        out.push(VerifiedCite { names, key });
    }
    out
}

/// Verified cites from the tool text the model actually received.
/// A result replaced by the context-limit stub, or an article whose body was
/// cut, does not count. The law name and the article number both have to match
/// a section that arrived whole.
pub fn verified_from_delivered(content: &str) -> Vec<VerifiedCite> {
    if content.contains(TOOL_RESULT_OMITTED) {
        return Vec::new();
    }
    let header = verified_from_tool_output(content);
    let Some(cut) = content.find(TOOL_RESULT_TRUNCATION_NOTE) else {
        return header;
    };
    let complete = complete_sections(content, cut);
    header
        .into_iter()
        .filter(|cite| {
            complete.iter().any(|section| {
                section.key == cite.key && cite.names.iter().any(|name| name == &section.name)
            })
        })
        .collect()
}

struct CompleteSection {
    name: String,
    key: String,
}

fn complete_sections(content: &str, cut: usize) -> Vec<CompleteSection> {
    let chars: Vec<char> = content.chars().collect();
    let mut byte_at = Vec::with_capacity(chars.len() + 1);
    let mut n = 0usize;
    byte_at.push(0);
    for ch in &chars {
        n += ch.len_utf8();
        byte_at.push(n);
    }
    let cut_chars = byte_at
        .iter()
        .position(|b| *b >= cut)
        .unwrap_or(chars.len());
    let mut starts = Vec::new();
    for (i, ch) in chars.iter().copied().enumerate() {
        if ch == '【' {
            starts.push(i);
        }
    }
    let mut sections = Vec::new();
    for (idx, start) in starts.iter().copied().enumerate() {
        let end = starts.get(idx + 1).copied().unwrap_or(chars.len());
        if cut_chars < end {
            continue;
        }
        let heading: String = chars[start..end.min(chars.len())]
            .iter()
            .take_while(|c| **c != '】')
            .collect();
        let Some(span) = article_spans(&heading).into_iter().next() else {
            continue;
        };
        if span.start > heading.chars().count() {
            continue;
        }
        let name: String = heading.chars().take(span.start).collect();
        let name = name.trim_start_matches('【').trim();
        if name.is_empty() {
            continue;
        }
        sections.push(CompleteSection {
            name: name.to_string(),
            key: span.key,
        });
    }
    sections
}

pub fn summary_lines(content: &str) -> Vec<String> {
    content
        .lines()
        .filter(|line| line.starts_with("確認済み: ") || line.starts_with("未確認: "))
        .map(|line| line.to_string())
        .collect()
}

pub fn mark_unverified(answer: &str, verified: &[VerifiedCite]) -> String {
    let chars: Vec<char> = answer.chars().collect();
    let spans = article_spans(answer);
    let mut inserts = Vec::new();
    let mut current_law: Option<String> = None;
    let mut current_suppl = false;
    let mut previous_end = 0usize;
    for span in &spans {
        if span.start > chars.len() || span.end > chars.len() || span.start > span.end {
            continue;
        }
        let prev = previous_end.min(span.start);
        let between: String = chars[prev..span.start].iter().collect();
        let inherit = current_law.is_some()
            && (is_connector(&between) || between.trim() == "同法" || between.ends_with("同法"));
        let window = law_window(&chars, span.start);
        let dou = window.as_deref() == Some("同法");
        let name = if dou && current_law.is_none() {
            verified
                .iter()
                .find(|item| item.key == span.key)
                .and_then(|item| item.names.first().cloned())
        } else if inherit || dou {
            current_law.clone()
        } else if let Some(window) = window.as_deref() {
            if DENY_LAW_NAME.iter().any(|deny| window.ends_with(deny)) {
                current_law = None;
                previous_end = span.end;
                if span.suppl && !already_marked(&chars, span.end) {
                    inserts.push(span.end);
                }
                continue;
            }
            match longest_verified_suffix(window, verified) {
                Some(found) => {
                    current_law = Some(found.clone());
                    current_suppl = span.suppl;
                    Some(found)
                }
                None => {
                    if window.ends_with("憲法") || window.ends_with('法') {
                        current_law = Some(window.to_string());
                        current_suppl = span.suppl;
                        Some(window.to_string())
                    } else {
                        current_law = None;
                        previous_end = span.end;
                        if span.suppl && !already_marked(&chars, span.end) {
                            inserts.push(span.end);
                        }
                        continue;
                    }
                }
            }
        } else {
            current_law = None;
            previous_end = span.end;
            if span.suppl && !already_marked(&chars, span.end) {
                inserts.push(span.end);
            }
            continue;
        };
        let Some(name) = name else {
            previous_end = span.end;
            if span.suppl && !already_marked(&chars, span.end) {
                inserts.push(span.end);
            }
            continue;
        };
        if DENY_LAW_NAME.contains(&name.as_str()) {
            previous_end = span.end;
            continue;
        }
        let suppl = span.suppl || ((inherit || dou) && current_suppl);
        current_suppl = suppl;
        if !inherit && !dou {
            current_law = Some(name.clone());
        }
        // 附則 is never treated as verified. Amendment acts each carry their own.
        let ok = !suppl
            && verified.iter().any(|item| {
                item.key == span.key && item.names.iter().any(|candidate| candidate == &name)
            });
        if !ok && !already_marked(&chars, span.end) {
            inserts.push(span.end);
        }
        previous_end = span.end;
    }
    if inserts.is_empty() {
        return answer.to_string();
    }
    inserts.sort_unstable();
    inserts.dedup();
    let mut out = String::new();
    let mut at = 0usize;
    for end in inserts {
        let end = end.min(chars.len());
        if end < at {
            continue;
        }
        out.extend(chars[at..end].iter());
        out.push_str("（要確認）");
        at = end;
    }
    out.extend(chars[at..].iter());
    out
}

fn longest_verified_suffix(window: &str, verified: &[VerifiedCite]) -> Option<String> {
    let mut best: Option<String> = None;
    for item in verified {
        for name in &item.names {
            if name.chars().count() < 2 {
                continue;
            }
            if window == name || window.ends_with(name.as_str()) {
                if best.as_ref().map(|b| name.chars().count() > b.chars().count()).unwrap_or(true) {
                    best = Some(name.clone());
                }
            }
        }
    }
    best
}

fn law_window(chars: &[char], cite_start: usize) -> Option<String> {
    let mut end = cite_start;
    while end > 0 && chars[end - 1].is_whitespace() {
        end -= 1;
    }
    let mut start = end;
    while start > 0 && is_law_char(chars[start - 1]) {
        start -= 1;
    }
    if start == end {
        return None;
    }
    let name: String = chars[start..end].iter().collect();
    if name.chars().count() < 2 {
        None
    } else {
        Some(name)
    }
}

fn is_law_char(c: char) -> bool {
    matches!(
        c,
        '\u{4E00}'..='\u{9FFF}'
            | '\u{3400}'..='\u{4DBF}'
            | '\u{3040}'..='\u{309F}'
            | '\u{30A0}'..='\u{30FF}'
            | '々'
            | '、'
            | '，'
    )
}

fn is_connector(between: &str) -> bool {
    let text: String = between.chars().filter(|c| !c.is_whitespace()).collect();
    matches!(
        text.as_str(),
        "及び" | "および" | "又は" | "または" | "若しくは" | "並びに" | "と" | "、" | "，" | "・"
    )
}

fn already_marked(chars: &[char], end: usize) -> bool {
    let mut i = end;
    while i < chars.len() && chars[i].is_whitespace() {
        i += 1;
    }
    let rest: String = chars[i..].iter().take(8).collect();
    rest.starts_with("（要確認）") || rest.starts_with("要確認")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn verified_cite(names: &[&str], key: &str) -> VerifiedCite {
        VerifiedCite {
            names: names.iter().map(|s| (*s).to_string()).collect(),
            key: key.into(),
        }
    }

    #[test]
    fn unverified_article_numbers_are_marked_and_verified_ones_are_not() {
        let verified = vec![verified_cite(&["民法"], "555")];
        let answer = "民法第555条は売買です。民法第1条は基本原則です。方法第1条は関係ありません。契約書第5条は条名だけです。";
        let marked = mark_unverified(answer, &verified);
        assert!(marked.contains("民法第555条は売買"), "{marked}");
        assert!(!marked.contains("民法第555条（要確認）"), "{marked}");
        assert!(marked.contains("民法第1条（要確認）"), "{marked}");
        assert!(marked.contains("方法第1条は関係"), "{marked}");
        assert!(!marked.contains("方法第1条（要確認）"), "{marked}");
        assert!(marked.contains("契約書第5条は"), "{marked}");
        assert!(!marked.contains("契約書第5条（要確認）"), "{marked}");
        let again = mark_unverified(&marked, &verified);
        assert_eq!(again.matches("（要確認）").count(), 1, "{again}");
    }

    #[test]
    fn alias_and_branch_numbers_count_as_verified() {
        let labor = vec![verified_cite(&["労働基準法", "労基法"], "36")];
        let marked = mark_unverified("労基法第36条は時間外です。労基法第37条は割増です。", &labor);
        assert!(marked.contains("労基法第36条は"), "{marked}");
        assert!(marked.contains("労基法第37条（要確認）"), "{marked}");
        let branched = vec![verified_cite(&["民法"], "548_2")];
        let text = mark_unverified("民法第548条の2は定型約款です。", &branched);
        assert!(!text.contains("要確認"), "{text}");
    }

    #[test]
    fn tool_header_round_trips_and_summary_keeps_only_the_header() {
        let articles = vec![ToolArticle {
            names: vec!["民法".into()],
            article_num: "第555条".into(),
            heading: "売買".into(),
            body: "売買は、当事者の一方がある財産権を相手方に移転することを約する。".into(),
            as_of: "2026-04-01".into(),
            verified: true,
            offline: false,
            note: String::new(),
            law_query: "民法".into(),
            article_query: "555".into(),
        }];
        let text = format_tool_result(&articles, 0);
        let parsed = verified_from_tool_output(&text);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].key, "555");
        assert_eq!(parsed[0].names, vec!["民法".to_string()]);
        let lines = summary_lines(&text);
        assert_eq!(lines, vec!["確認済み: 民法 第555条".to_string()]);
        assert!(lines.iter().all(|line| !line.contains("財産権")));
        assert!(text.contains("見出し: 売買"));
        assert!(text.contains("時点: 2026-04-01"));
    }

    #[test]
    fn marking_follows_the_law_that_precedes_the_article() {
        let verified = vec![
            verified_cite(&["民法"], "32_4"),
            verified_cite(&["個人情報の保護に関する法律", "個人情報保護法"], "27"),
            verified_cite(&["民法"], "1"),
            verified_cite(&["民法"], "548_2"),
        ];
        let same = mark_unverified("同法第32条の4第1項は適用されます。", &verified);
        assert!(!same.contains("要確認"), "{same}");
        let full = mark_unverified("個人情報の保護に関する法律第27条は第三者提供です。", &verified);
        assert!(!full.contains("要確認"), "{full}");
        let pair = mark_unverified("民法第1条及び第2条。", &verified);
        assert!(pair.contains("第1条は") || pair.contains("民法第1条及"), "{pair}");
        assert!(pair.contains("第2条（要確認）"), "{pair}");
        assert!(!pair.contains("第1条（要確認）"), "{pair}");
        let amended = mark_unverified("改正民法第548条の2は定型約款です。", &verified);
        assert!(!amended.contains("要確認"), "{amended}");
        let suppl = mark_unverified("民法附則第1条。", &[verified_cite(&["民法"], "1")]);
        assert!(suppl.contains("附則第1条（要確認）"), "{suppl}");
        let suppl_named = mark_unverified("民法附則第1条。", &[verified_cite(&["民法"], "s1")]);
        assert!(suppl_named.contains("要確認"), "{suppl_named}");
        let main = vec![verified_cite(&["民法"], "1")];
        for sample in [
            "民法附則第1条",
            "附則3条",
            "民法附則（平成二十九年法律第四十四号）第1条",
            "附則（平成29年法律第44号）第1条",
            "民法附則の第1条",
        ] {
            let marked = mark_unverified(sample, &main);
            assert!(
                marked.contains("（要確認）"),
                "{sample} -> {marked}"
            );
        }
        let bare = mark_unverified("民法555条は売買です。民法1条は別です。", &verified);
        assert!(bare.contains("555条（要確認）"), "{bare}");
        assert!(bare.contains("民法1条は"), "{bare}");
        assert!(!bare.contains("1条（要確認）"), "{bare}");
        let omitted = mark_unverified("根拠は民法第548の2", &verified);
        assert!(!omitted.contains("要確認"), "{omitted}");
        let omitted_other = mark_unverified("根拠は民法第548の2", &[]);
        assert!(omitted_other.contains("第548の2（要確認）"), "{omitted_other}");
    }

    #[test]
    fn a_truncated_tool_result_does_not_count_as_verified() {
        let full = "確認済み: 民法 第555条\n\n【民法 第555条】\n見出し: 売買\n本文です。\n";
        assert_eq!(verified_from_delivered(full).len(), 1);
        let stub = "（文脈の上限に達したため、この結果は省略しました）";
        assert!(verified_from_delivered(stub).is_empty());
        let cut = "確認済み: 民法 第555条\n確認済み: 民法 第1条\n\n【民法 第555条】\n本文です。\n\n【民法 第1条】\n途中…（長いので省略しました）";
        let kept = verified_from_delivered(cut);
        assert_eq!(kept.len(), 1, "{kept:?}");
        assert_eq!(kept[0].key, "555");
        let same_number = "確認済み: 民法 第1条\n確認済み: 会社法 第1条\n\n【民法 第1条】\n全文です。\n\n【会社法 第1条】\n途中…（長いので省略しました）";
        let kept_law = verified_from_delivered(same_number);
        assert_eq!(kept_law.len(), 1, "{kept_law:?}");
        assert_eq!(kept_law[0].names, vec!["民法".to_string()]);
        assert_eq!(kept_law[0].key, "1");
    }

    #[test]
    fn marking_does_not_panic_when_the_citation_has_no_jou() {
        let samples = [
            "",
            "根拠は民法第548の2",
            "民法555条",
            "民法1条",
            "第548の2",
            "附則3条",
            "555条",
            "条",
            "第",
            "の2",
            "民法第1条及び第2条",
            "同法第32条の4第1項",
        ];
        for sample in samples {
            let marked = mark_unverified(sample, &[]);
            assert!(marked.chars().count() >= sample.chars().count(), "{sample}");
            for span in article_spans(sample) {
                let len = sample.chars().count();
                assert!(span.start <= span.end && span.end <= len, "{sample} {span:?}");
            }
        }
        let mut state = 0x1234_5678_u64;
        for _ in 0..300 {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            let n = (state % 48) as usize;
            let alphabet = [
                'あ', '第', '条', 'の', '1', '2', '5', '法', '民', '附', '則', ' ', '8', '4',
            ];
            let mut text = String::new();
            for k in 0..n {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1);
                text.push(alphabet[(state.wrapping_add(k as u64) as usize) % alphabet.len()]);
            }
            let marked = mark_unverified(&text, &[verified_cite(&["民法"], "1")]);
            assert!(marked.chars().count() >= text.chars().count());
            let len = text.chars().count();
            for span in article_spans(&text) {
                assert!(span.start <= span.end && span.end <= len);
            }
            let _ = parse_article_query(&text);
        }
    }
}
