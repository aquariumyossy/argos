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
    /// `YYYY-MM-DD`. Empty when the row has no usable date.
    pub as_of: String,
}

pub fn parse_law_xml(xml: &str) -> Result<ParsedLaw, String> {
    let mut parser = LawParser::default();
    parser.feed(xml)?;
    if parser.law_id.is_empty() && parser.articles.is_empty() {
        return Err("法令XMLに本文がありません".into());
    }
    dedupe_article_keys(&mut parser.articles);
    Ok(ParsedLaw {
        law_id: parser.law_id,
        law_num: squash(&parser.law_num),
        name: squash(&parser.law_name),
        abbrev: squash(&parser.abbrev),
        articles: parser.articles,
    })
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
                    field = Field::None;
                } else if in_row {
                    field = match name.as_str() {
                        "LawId" => Field::Id,
                        "EnforcementDate" => Field::Enforcement,
                        "AmendPromulgationDate" => Field::Amend,
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
                        _ => {}
                    }
                }
            }
            Ok(Event::End(e)) => {
                if local(e.name().as_ref()) == "LawNameListInfo" && in_row {
                    let as_of = revision_date(&enforcement, &amend);
                    let id = law_id.trim().to_string();
                    if !id.is_empty() {
                        out.push(ListedUpdate { law_id: id, as_of });
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
        if self.capture_id {
            self.law_id.push_str(text);
        }
        if self.capture_num {
            self.law_num.push_str(text);
        }
        if self.capture_name && self.rt == 0 {
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
    let (main, branch) = match num.split_once('_') {
        Some((main, branch)) => (main, Some(branch)),
        None => (num, None),
    };
    let main_n: u32 = main.parse().ok()?;
    let branch_n = match branch {
        Some(branch) if !branch.is_empty() => Some(branch.parse::<u32>().ok()?),
        _ => None,
    };
    let key_body = match branch_n {
        Some(branch) => format!("{main_n}_{branch}"),
        None => main_n.to_string(),
    };
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
    display.push_str(&main_n.to_string());
    display.push('条');
    if let Some(branch) = branch_n {
        display.push('の');
        display.push_str(&branch.to_string());
    }
    Some((key, display))
}

/// `555`, `第555条`, `第五百五十五条`, `548の2`.
pub fn parse_article_query(raw: &str) -> Option<(String, String)> {
    let text = raw.trim();
    if text.is_empty() {
        return None;
    }
    let prepared = if text.contains('条') {
        text.to_string()
    } else if let Some((main, branch)) = text.split_once('の') {
        format!("第{main}条の{branch}")
    } else {
        format!("第{text}条")
    };
    let normalized = crate::search::legal_ref::normalize_legal_refs(&prepared);
    let chars: Vec<char> = normalized.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '第' {
            i += 1;
            continue;
        }
        let start = i + 1;
        let mut j = start;
        while j < chars.len() && chars[j].is_ascii_digit() {
            j += 1;
        }
        if j == start || j >= chars.len() || chars[j] != '条' {
            i += 1;
            continue;
        }
        let main: String = chars[start..j].iter().collect();
        let mut k = j + 1;
        let branch = if k < chars.len() && chars[k] == 'の' {
            k += 1;
            let bstart = k;
            while k < chars.len() && chars[k].is_ascii_digit() {
                k += 1;
            }
            if k > bstart {
                Some(chars[bstart..k].iter().collect::<String>())
            } else {
                None
            }
        } else {
            None
        };
        let attr = match &branch {
            Some(branch) => format!("{main}_{branch}"),
            None => main,
        };
        return article_identity(&attr, false);
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
        assert_eq!(keys, vec!["1", "3_2", "s1"]);
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
        assert_eq!(law.articles[2].num, "附則第1条");
        assert!(law.articles[2].body.contains("附則の本文"));
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
        assert!(parse_article_query("").is_none());
        assert!(parse_article_query("条なし").is_none());
    }
}
