//! Mark statute citations the chat did not verify.

use crate::search::legal_ref::legal_ref_spans;
use crate::statute::xmlparse::parse_article_query;

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

pub fn summary_lines(content: &str) -> Vec<String> {
    content
        .lines()
        .filter(|line| line.starts_with("確認済み: ") || line.starts_with("未確認: "))
        .map(|line| line.to_string())
        .collect()
}

pub fn mark_unverified(answer: &str, verified: &[VerifiedCite]) -> String {
    let chars: Vec<char> = answer.chars().collect();
    let mut inserts = Vec::new();
    for (start, end) in legal_ref_spans(answer) {
        if end <= start || end > chars.len() {
            continue;
        }
        let cite: String = chars[start..end].iter().collect();
        if !cite.contains('条') {
            continue;
        }
        let Some((key, _)) = parse_article_query(&cite) else {
            continue;
        };
        let Some(name) = law_name_before(&chars, start) else {
            continue;
        };
        if DENY_LAW_NAME.contains(&name.as_str()) {
            continue;
        }
        if verified
            .iter()
            .any(|item| item.key == key && item.names.iter().any(|n| n == &name))
        {
            continue;
        }
        if already_marked(&chars, end) {
            continue;
        }
        inserts.push(end);
    }
    if inserts.is_empty() {
        return answer.to_string();
    }
    inserts.sort_unstable();
    inserts.dedup();
    let mut out = String::new();
    let mut at = 0usize;
    for end in inserts {
        out.extend(chars[at..end].iter());
        out.push_str("（要確認）");
        at = end;
    }
    out.extend(chars[at..].iter());
    out
}

fn law_name_before(chars: &[char], cite_start: usize) -> Option<String> {
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
        return None;
    }
    if name.ends_with("憲法") || name.ends_with('法') {
        Some(name)
    } else {
        None
    }
}

fn is_law_char(c: char) -> bool {
    matches!(c, '\u{4E00}'..='\u{9FFF}' | '\u{3400}'..='\u{4DBF}' | '\u{30A0}'..='\u{30FF}' | '々')
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
}
