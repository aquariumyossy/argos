//! e-Gov 法令API (Version 1) XML.
//!
//! Full laws (`lawdata`) and one-article responses (`articles`) share the
//! `Article` element. Amendment instructions and the table of contents are not
//! current article text.

use quick_xml::events::Event;
use quick_xml::Reader;

use crate::statute::refresh::format_ymd;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedArticle {
    pub key: String,
    pub num: String,
    pub heading: String,
    pub body: String,
    pub suppl: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedLaw {
    pub law_id: String,
    pub law_num: String,
    pub name: String,
    pub abbrev: String,
    pub articles: Vec<ParsedArticle>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedLaw {
    pub law_id: String,
    pub name: String,
    pub law_num: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedUpdate {
    pub law_id: String,
    /// In-force text date, `YYYY-MM-DD`. Empty when this row is not yet in force.
    pub as_of: String,
    /// Enforcement date when the API marks the amendment as not yet in force.
    pub enforce_on: String,
    pub pending: bool,
}

pub fn parse_law_xml(xml: &str) -> Result<ParsedLaw, String> {
    let mut parser = LawParser::default();
    parser.feed(xml)?;
    let code = parser.result_code.trim();
    if !code.is_empty() && code != "0" {
        return Err(format!(
            "e-Gov 法令APIの結果コードが成功ではありません ({code})"
        ));
    }
    if parser.articles.is_empty() {
        return Err("法令XMLに条文がありません".into());
    }
    dedupe_article_keys(&mut parser.articles);
    let law_num = squash(&parser.law_num);
    Ok(ParsedLaw {
        law_id: parser.law_id,
        name: display_law_name(&law_num, &parser.law_name),
        law_num,
        abbrev: squash(&parser.abbrev),
        articles: parser.articles,
    })
}

/// e-Gov puts the law number inside `LawTitle` for some acts
/// (`昭和二十二年法律第五十四号（私的独占の…）`). Store the name in the parentheses.
fn display_law_name(law_num: &str, law_name: &str) -> String {
    let name = squash(law_name);
    let num = squash(law_num);
    if num.is_empty() || !name.starts_with(&num) {
        return name;
    }
    let rest = name[num.len()..].trim();
    let rest = rest
        .trim_start_matches(['（', '('])
        .trim_end_matches(['）', ')'])
        .trim();
    if rest.is_empty() {
        name
    } else {
        rest.to_string()
    }
}

pub fn parse_law_list(xml: &str) -> Result<Vec<ListedLaw>, String> {
    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();
    let mut out = Vec::new();
    let mut current: Option<ListedLaw> = None;
    let mut field = Field::None;
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = local(e.name().as_ref());
                if name == "LawNameListInfo" {
                    current = Some(ListedLaw {
                        law_id: String::new(),
                        name: String::new(),
                        law_num: String::new(),
                    });
                    field = Field::None;
                } else if current.is_some() {
                    field = match name.as_str() {
                        "LawId" => Field::Id,
                        "LawName" => Field::Name,
                        "LawNo" => Field::Num,
                        _ => Field::None,
                    };
                }
            }
            Ok(Event::Empty(e)) => {
                if local(e.name().as_ref()) == "LawNameListInfo" {
                    current = None;
                }
                field = Field::None;
            }
            Ok(Event::Text(t)) => {
                if let Some(row) = current.as_mut() {
                    let text = t.unescape().map_err(|e| e.to_string())?;
                    push_field(row, field, &text);
                }
            }
            Ok(Event::End(e)) => {
                if local(e.name().as_ref()) == "LawNameListInfo" {
                    if let Some(row) = current.take() {
                        if !row.law_id.is_empty() && !row.name.is_empty() {
                            out.push(row);
                        }
                    }
                }
                field = Field::None;
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(e.to_string()),
            _ => {}
        }
        buf.clear();
    }
    Ok(out)
}

pub fn parse_update_list(xml: &str) -> Result<Vec<ListedUpdate>, String> {
    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();
    let mut out = Vec::new();
    let mut law_id = String::new();
    let mut enforcement = String::new();
    let mut amend = String::new();
    let mut flag = String::new();
    let mut in_row = false;
    let mut field = Field::None;
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = local(e.name().as_ref());
                if name == "LawNameListInfo" {
                    in_row = true;
                    law_id.clear();
                    enforcement.clear();
                    amend.clear();
                    flag.clear();
                    field = Field::None;
                } else if in_row {
                    field = match name.as_str() {
                        "LawId" => Field::Id,
                        "EnforcementDate" => Field::Enforcement,
                        "AmendPromulgationDate" => Field::Amend,
                        "EnforcementFlg" => Field::Flag,
                        _ => Field::None,
                    };
                }
            }
            Ok(Event::Text(t)) => {
                if in_row {
                    let text = t.unescape().map_err(|e| e.to_string())?;
                    match field {
                        Field::Id => law_id.push_str(&text),
                        Field::Enforcement => enforcement.push_str(&text),
                        Field::Amend => amend.push_str(&text),
                        Field::Flag => flag.push_str(&text),
                        _ => {}
                    }
                }
            }
            Ok(Event::End(e)) => {
                if local(e.name().as_ref()) == "LawNameListInfo" && in_row {
                    let pending = flag.trim() == "1";
                    let enforce_on = crate::statute::refresh::parse_ymd(&enforcement)
                        .map(format_ymd)
                        .unwrap_or_default();
                    let as_of = if pending {
                        String::new()
                    } else {
                        revision_date(&enforcement, &amend)
                    };
                    let id = law_id.trim().to_string();
                    if !id.is_empty() {
                        out.push(ListedUpdate {
                            law_id: id,
                            as_of,
                            enforce_on,
                            pending,
                        });
                    }
                    in_row = false;
                }
                field = Field::None;
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(e.to_string()),
            _ => {}
        }
        buf.clear();
    }
    Ok(out)
}

fn revision_date(enforcement: &str, amend: &str) -> String {
    for raw in [enforcement, amend] {
        if let Some(date) = crate::statute::refresh::parse_ymd(raw) {
            return format_ymd(date);
        }
    }
    String::new()
}

#[derive(Clone, Copy)]
enum Field {
    None,
    Id,
    Name,
    Num,
    Enforcement,
    Amend,
    Flag,
}

fn push_field(row: &mut ListedLaw, field: Field, text: &str) {
    match field {
        Field::Id => row.law_id.push_str(text),
        Field::Name => row.name.push_str(text),
        Field::Num => row.law_num.push_str(text),
        _ => {}
    }
}

fn squash(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join("")
        .trim()
        .to_string()
}

#[derive(Default)]
struct LawParser {
    law_id: String,
    law_num: String,
    law_name: String,
    abbrev: String,
    articles: Vec<ParsedArticle>,
    amend: u32,
    toc: u32,
    rt: u32,
    suppl: u32,
    capture_id: bool,
    capture_num: bool,
    capture_name: bool,
    capture_code: bool,
    in_result: u32,
    result_code: String,
    article: Option<ArticleBuf>,
    skip_article: u32,
}

struct ArticleBuf {
    key: String,
    num: String,
    heading: String,
    body: String,
    suppl: bool,
    delete: bool,
    depth: u32,
    caption: bool,
    title: bool,
    after_label: bool,
}

impl LawParser {
    fn feed(&mut self, xml: &str) -> Result<(), String> {
        let mut reader = Reader::from_str(xml);
        let mut buf = Vec::new();
        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(e)) => {
                    let name = local(e.name().as_ref());
                    let abbrev = attr(&e, b"Abbrev");
                    let num = attr(&e, b"Num");
                    let delete = attr(&e, b"Delete");
                    self.begin(&name, &abbrev, &num, &delete);
                }
                Ok(Event::Empty(e)) => {
                    let name = local(e.name().as_ref());
                    let abbrev = attr(&e, b"Abbrev");
                    let num = attr(&e, b"Num");
                    let delete = attr(&e, b"Delete");
                    self.begin(&name, &abbrev, &num, &delete);
                    self.finish(&name);
                }
                Ok(Event::Text(t)) => {
                    let text = t.unescape().map_err(|e| e.to_string())?;
                    self.text(&text);
                }
                Ok(Event::CData(t)) => {
                    let text = String::from_utf8_lossy(t.as_ref());
                    self.text(&text);
                }
                Ok(Event::End(e)) => {
                    let name = local(e.name().as_ref());
                    self.finish(&name);
                }
                Ok(Event::Eof) => break,
                Err(e) => return Err(e.to_string()),
                _ => {}
            }
            buf.clear();
        }
        Ok(())
    }

    fn begin(&mut self, name: &str, abbrev: &str, num: &str, delete: &str) {
        match name {
            "AmendProvision" => self.amend = self.amend.saturating_add(1),
            "TOC" => self.toc = self.toc.saturating_add(1),
            "Rt" => self.rt = self.rt.saturating_add(1),
            "SupplProvision" => self.suppl = self.suppl.saturating_add(1),
            "Result" => self.in_result = self.in_result.saturating_add(1),
            "Code" => self.capture_code = self.in_result > 0 && self.result_code.is_empty(),
            "LawId" => self.capture_id = self.article.is_none() && self.skip_article == 0,
            "LawNum" => self.capture_num = self.article.is_none() && self.law_num.is_empty(),
            "LawTitle" => {
                self.capture_name = self.article.is_none() && self.law_name.is_empty();
                if self.abbrev.is_empty() && !abbrev.is_empty() {
                    self.abbrev = abbrev.to_string();
                }
            }
            "Article" => self.begin_article(num, delete),
            "ArticleCaption" => {
                if let Some(article) = self.article.as_mut() {
                    article.caption = true;
                }
            }
            "ArticleTitle" => {
                if let Some(article) = self.article.as_mut() {
                    article.title = true;
                }
            }
            "Paragraph" | "Item" | "Subitem1" | "Subitem2" | "Subitem3" | "Subitem4"
            | "Subitem5" | "Subitem6" | "Subitem7" | "Subitem8" | "Subitem9" | "Subitem10" => {
                if let Some(article) = self.article.as_mut() {
                    if !article.body.is_empty() && !article.body.ends_with('\n') {
                        article.body.push('\n');
                    }
                }
            }
            "ItemTitle" | "Subitem1Title" | "Subitem2Title" | "Subitem3Title" => {
                if let Some(article) = self.article.as_mut() {
                    article.after_label = true;
                }
            }
            _ => {}
        }
    }

    fn begin_article(&mut self, num: &str, delete: &str) {
        if self.amend > 0 || self.toc > 0 || self.skip_article > 0 {
            self.skip_article = self.skip_article.saturating_add(1);
            return;
        }
        if let Some(article) = self.article.as_mut() {
            article.depth = article.depth.saturating_add(1);
            return;
        }
        let suppl = self.suppl > 0;
        let Some((key, display)) = article_identity(num, suppl) else {
            self.skip_article = 1;
            return;
        };
        self.article = Some(ArticleBuf {
            key,
            num: display,
            heading: String::new(),
            body: String::new(),
            suppl,
            delete: delete.eq_ignore_ascii_case("true"),
            depth: 1,
            caption: false,
            title: false,
            after_label: false,
        });
    }

    fn text(&mut self, text: &str) {
        if text.is_empty() || text.trim().is_empty() {
            return;
        }
        if self.capture_code {
            self.result_code.push_str(text.trim());
        }
        if self.capture_id {
            self.law_id.push_str(text);
        }
        if self.capture_num {
            self.law_num.push_str(text);
        }
        // LawNum nested in LawTitle must not become part of the stored name.
        if self.capture_name && self.rt == 0 && !self.capture_num {
            self.law_name.push_str(text);
        }
        let Some(article) = self.article.as_mut() else {
            return;
        };
        if self.rt > 0 || article.title {
            return;
        }
        if article.caption {
            article.heading.push_str(text);
            return;
        }
        article.body.push_str(text);
        if article.after_label && !text.trim().is_empty() {
            article.body.push('　');
            article.after_label = false;
        }
    }

    fn finish(&mut self, name: &str) {
        match name {
            "AmendProvision" => self.amend = self.amend.saturating_sub(1),
            "TOC" => self.toc = self.toc.saturating_sub(1),
            "Rt" => self.rt = self.rt.saturating_sub(1),
            "SupplProvision" => self.suppl = self.suppl.saturating_sub(1),
            "Result" => self.in_result = self.in_result.saturating_sub(1),
            "Code" => self.capture_code = false,
            "LawId" => self.capture_id = false,
            "LawNum" => self.capture_num = false,
            "LawTitle" => self.capture_name = false,
            "ArticleCaption" => {
                if let Some(article) = self.article.as_mut() {
                    article.caption = false;
                }
            }
            "ArticleTitle" => {
                if let Some(article) = self.article.as_mut() {
                    article.title = false;
                }
            }
            "ItemTitle" | "Subitem1Title" | "Subitem2Title" | "Subitem3Title" => {}
            "Article" => self.finish_article(),
            _ => {}
        }
    }

    fn finish_article(&mut self) {
        if self.skip_article > 0 {
            self.skip_article -= 1;
            return;
        }
        let Some(article) = self.article.as_mut() else {
            return;
        };
        article.depth = article.depth.saturating_sub(1);
        if article.depth > 0 {
            return;
        }
        let article = self.article.take().expect("article");
        if article.delete {
            return;
        }
        let body = cleanup_body(&article.body);
        if body.is_empty() {
            return;
        }
        self.articles.push(ParsedArticle {
            key: article.key,
            num: article.num,
            heading: cleanup_heading(&article.heading),
            body,
            suppl: article.suppl,
        });
    }
}

fn local(name: &[u8]) -> String {
    let raw = match name.rsplit(|b| *b == b':').next() {
        Some(rest) => rest,
        None => name,
    };
    String::from_utf8_lossy(raw).into_owned()
}

fn attr(e: &quick_xml::events::BytesStart<'_>, key: &[u8]) -> String {
    for attr in e.attributes().flatten() {
        if attr.key.as_ref() == key {
            if let Ok(value) = attr.unescape_value() {
                return value.into_owned();
            }
        }
    }
    String::new()
}

pub fn article_identity(num: &str, suppl: bool) -> Option<(String, String)> {
    let num = num.trim();
    if num.is_empty() {
        return None;
    }
    let mut parts = Vec::new();
    for part in num.split('_') {
        if part.is_empty() {
            return None;
        }
        let n: u32 = part.parse().ok()?;
        if n == 0 || n > 999_999 {
            return None;
        }
        parts.push(n);
    }
    if parts.is_empty() {
        return None;
    }
    let key_body = parts
        .iter()
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join("_");
    let key = if suppl {
        format!("s{key_body}")
    } else {
        key_body
    };
    let mut display = String::new();
    if suppl {
        display.push_str("附則");
    }
    display.push('第');
    display.push_str(&parts[0].to_string());
    display.push('条');
    for branch in parts.iter().skip(1) {
        display.push('の');
        display.push_str(&branch.to_string());
    }
    Some((key, display))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArticleSpan {
    pub start: usize,
    pub end: usize,
    pub key: String,
    pub display: String,
    pub suppl: bool,
}

/// Every 第N条 in `text`, including 附則 and branch numbers of any depth.
/// `start`/`end` are char indexes. A following 項 or 号 is not part of the span.
pub fn article_spans(text: &str) -> Vec<ArticleSpan> {
    let prepared = ensure_article_mark(text);
    let chars: Vec<char> = prepared.chars().collect();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i] != '第' {
            i += 1;
            continue;
        }
        let Some((main, mut j)) = take_numeral(&chars, i + 1) else {
            i += 1;
            continue;
        };
        if chars.get(j) != Some(&'条') {
            i += 1;
            continue;
        }
        j += 1;
        let mut parts = vec![main];
        while chars.get(j) == Some(&'の') {
            let Some((branch, next)) = take_numeral(&chars, j + 1) else {
                break;
            };
            if chars.get(next) == Some(&'条') {
                break;
            }
            parts.push(branch);
            j = next;
        }
        let suppl_at = suppl_prefix(&chars, i);
        let suppl = suppl_at.is_some();
        let start = suppl_at.unwrap_or(i);
        let attr = parts
            .iter()
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join("_");
        let Some((key, display)) = article_identity(&attr, suppl) else {
            i = j;
            continue;
        };
        out.push(ArticleSpan {
            start,
            end: j,
            key,
            display,
            suppl,
        });
        i = j;
    }
    out
}

/// `555`, `第555条`, `第五百五十五条`, `548の2`, `第32条の4の2`, `附則第1条`.
pub fn parse_article_query(raw: &str) -> Option<(String, String)> {
    let text = raw.trim();
    if text.is_empty() {
        return None;
    }
    if !text.contains('第')
        && !text.contains('条')
        && text
            .chars()
            .all(|c| c.is_ascii_digit() || ('０'..='９').contains(&c) || c == '_')
    {
        let ascii: String = text
            .chars()
            .map(|c| {
                if ('０'..='９').contains(&c) {
                    char::from(b'0' + (c as u32 - '０' as u32) as u8)
                } else {
                    c
                }
            })
            .collect();
        return article_identity(&ascii, false);
    }
    let span = article_spans(text).into_iter().next()?;
    Some((span.key, span.display))
}

fn ensure_article_mark(text: &str) -> String {
    if text.contains('条') {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    if !text.contains('第') {
        out.push('第');
    }
    let mut inserted = false;
    for c in chars {
        if !inserted && c == 'の' {
            out.push('条');
            inserted = true;
        }
        out.push(c);
    }
    if !inserted {
        out.push('条');
    }
    out
}

fn take_numeral(chars: &[char], start: usize) -> Option<(u32, usize)> {
    let mut end = start;
    while end < chars.len() && crate::search::legal_ref::is_numeral_char(chars[end]) {
        end += 1;
    }
    while end > start {
        let token: String = chars[start..end].iter().collect();
        if let Some(n) = crate::search::legal_ref::parse_legal_numeral(&token) {
            if n > 0 && n <= 999_999 {
                return Some((n, end));
            }
        }
        end -= 1;
    }
    None
}

fn suppl_prefix(chars: &[char], dai: usize) -> Option<usize> {
    let mut i = dai;
    while i > 0 && chars[i - 1].is_whitespace() {
        i -= 1;
    }
    if i >= 2 && chars[i - 2] == '附' && chars[i - 1] == '則' {
        return Some(i - 2);
    }
    if i >= 3 && chars[i - 3] == '附' && chars[i - 1] == '則' && chars[i - 2].is_whitespace() {
        return Some(i - 3);
    }
    None
}

fn dedupe_article_keys(articles: &mut [ParsedArticle]) {
    let mut seen = std::collections::HashMap::<String, u32>::new();
    for article in articles.iter_mut() {
        let count = seen.entry(article.key.clone()).or_insert(0);
        *count += 1;
        if *count > 1 {
            article.key = format!("{}#{}", article.key, *count);
        }
    }
}

fn cleanup_heading(raw: &str) -> String {
    let text = squash(raw);
    text.trim_matches(|c| matches!(c, '（' | '）' | '(' | ')' | ' '))
        .trim()
        .to_string()
}

fn cleanup_body(raw: &str) -> String {
    let mut out = String::new();
    let mut blank = false;
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            if !out.is_empty() && !blank {
                out.push('\n');
                blank = true;
            }
            continue;
        }
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(line);
        blank = false;
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINI: &str = include_str!("fixtures/mini_law.xml");
    const ARTICLE: &str = include_str!("fixtures/article_555.xml");
    const CONSTITUTION: &str = include_str!("fixtures/constitution_9.xml");
    const UPDATES: &str = include_str!("fixtures/update_list.xml");
    const NAMES: &str = include_str!("fixtures/law_list.xml");

    #[test]
    fn parses_a_law_and_skips_deleted_toc_and_amendment_text() {
        let law = parse_law_xml(MINI).unwrap();
        assert_eq!(law.law_id, "TEST0001");
        assert_eq!(law.name, "試法");
        assert_eq!(law.abbrev, "試法");
        assert_eq!(law.law_num, "令和八年法律第一号");
        let keys: Vec<&str> = law.articles.iter().map(|a| a.key.as_str()).collect();
        assert_eq!(keys, vec!["1", "3_2", "32_4_2", "s1"]);
        let first = &law.articles[0];
        assert_eq!(first.num, "第1条");
        assert_eq!(first.heading, "趣旨");
        assert!(first.body.contains("試験のためのもの"));
        assert!(!first.body.contains("目次"));
        let branch = &law.articles[1];
        assert_eq!(branch.num, "第3条の2");
        assert_eq!(branch.heading, "枝番");
        assert!(branch.body.contains("枝番の本文"));
        assert!(branch.body.contains("一　号の本文"));
        assert!(branch.body.contains("売主"));
        assert!(!branch.body.contains("うりぬし"));
        assert_eq!(law.articles[2].key, "32_4_2");
        assert_eq!(law.articles[2].num, "第32条の4の2");
        assert!(law.articles[2].body.contains("二重枝番の本文"));
        assert_eq!(law.articles[3].num, "附則第1条");
        assert!(law.articles[3].body.contains("附則の本文"));
        assert!(law.articles.iter().all(|a| !a.body.contains("改正規定")));
        assert!(law.articles.iter().all(|a| !a.body.contains("削除条文")));
    }

    #[test]
    fn parses_recorded_egov_article_responses() {
        let civil = parse_law_xml(ARTICLE).unwrap();
        assert_eq!(civil.law_id, "129AC0000000089");
        assert_eq!(civil.articles.len(), 1);
        let article = &civil.articles[0];
        assert_eq!(article.key, "555");
        assert_eq!(article.num, "第555条");
        assert_eq!(article.heading, "売買");
        assert!(article.body.contains("財産権を相手方に移転"));
        let constitution = parse_law_xml(CONSTITUTION).unwrap();
        assert_eq!(constitution.law_id, "321CONSTITUTION");
        assert_eq!(constitution.articles[0].key, "9");
        assert!(constitution.articles[0].body.contains("国際平和"));
        assert!(constitution.articles[0].body.contains("交戦権"));
    }

    #[test]
    fn parses_recorded_update_and_name_lists() {
        let updates = parse_update_list(UPDATES).unwrap();
        assert_eq!(updates[0].law_id, "129AC0000000089");
        assert_eq!(updates[0].as_of, "2026-04-01");
        assert_eq!(updates[1].law_id, "140AC0000000045");
        assert_eq!(updates[1].as_of, "2024-01-15");
        assert_eq!(updates[2].as_of, "");
        assert!(!updates[0].pending);
        assert!(updates[3].pending);
        assert_eq!(updates[3].as_of, "");
        assert_eq!(updates[3].enforce_on, "2027-04-01");
        assert_eq!(updates[3].law_id, "417AC0000000086");
        let names = parse_law_list(NAMES).unwrap();
        assert_eq!(names[0].law_id, "129AC0000000089");
        assert_eq!(names[0].name, "民法");
        assert!(names[0].law_num.contains("第八十九号"));
    }

    #[test]
    fn article_query_accepts_the_forms_lawyers_type() {
        assert_eq!(parse_article_query("555").unwrap().0, "555");
        assert_eq!(parse_article_query("第555条").unwrap().1, "第555条");
        assert_eq!(parse_article_query("第五百五十五条").unwrap().0, "555");
        assert_eq!(parse_article_query("548の2").unwrap().1, "第548条の2");
        assert_eq!(parse_article_query("第548条の2").unwrap().0, "548_2");
        assert_eq!(parse_article_query("第32条の4の2").unwrap().0, "32_4_2");
        assert_eq!(
            parse_article_query("第三十二条の四の二").unwrap().1,
            "第32条の4の2"
        );
        assert_eq!(parse_article_query("３２の４の２").unwrap().0, "32_4_2");
        assert_eq!(parse_article_query("附則第1条").unwrap().0, "s1");
        assert_eq!(parse_article_query("附則第1条").unwrap().1, "附則第1条");
        assert_eq!(parse_article_query("第1条").unwrap().0, "1");
        let spans = article_spans("民法第1条及び第2条");
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[1].key, "2");
        assert!(parse_article_query("").is_none());
        assert!(parse_article_query("条なし").is_none());
    }

    #[test]
    fn rejects_an_egov_error_body_and_strips_the_law_number_from_the_title() {
        let error = r#"<?xml version="1.0" encoding="UTF-8"?>
<DataRoot><Result><Code>1</Code><Message>該当なし</Message></Result>
<ApplData><LawId>129AC0000000089</LawId></ApplData></DataRoot>"#;
        let err = parse_law_xml(error).unwrap_err();
        assert!(err.contains("結果コード"), "{err}");
        let empty = r#"<?xml version="1.0" encoding="UTF-8"?>
<DataRoot><Result><Code>0</Code></Result>
<ApplData><LawId>129AC0000000089</LawId><LawFullText></LawFullText></ApplData></DataRoot>"#;
        assert!(parse_law_xml(empty).unwrap_err().contains("条文がありません"));
        let titled = r#"<?xml version="1.0" encoding="UTF-8"?>
<DataRoot><Result><Code>0</Code></Result><ApplData><LawFullText><Law>
<LawNum>昭和二十二年法律第五十四号</LawNum>
<LawBody><LawTitle Abbrev="独禁法">昭和二十二年法律第五十四号（私的独占の禁止及び公正取引の確保に関する法律）</LawTitle>
<MainProvision><Article Num="1"><ArticleTitle>第一条</ArticleTitle>
<Paragraph><ParagraphSentence><Sentence>本文。</Sentence></ParagraphSentence></Paragraph>
</Article></MainProvision></LawBody></Law></LawFullText></ApplData></DataRoot>"#;
        let law = parse_law_xml(titled).unwrap();
        assert_eq!(law.name, "私的独占の禁止及び公正取引の確保に関する法律");
        assert_eq!(law.law_num, "昭和二十二年法律第五十四号");
        assert!(!law.name.contains("昭和二十二年"));
    }
}
