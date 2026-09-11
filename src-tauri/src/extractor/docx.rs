//! DOCX text extraction, including unaccepted track changes and comments.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::Path;

use zip::ZipArchive;

use super::{file_title, ExtractedDoc, SKIP_NO_TEXT};

pub(super) const DOCX_MARKUP_LEGEND: &str = "※ 〔-〕削除  〔+〕挿入  〔注〕コメント";

pub(super) fn extract_docx(path: &Path) -> Result<ExtractedDoc, String> {
    let file = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut archive = ZipArchive::new(file).map_err(|e| e.to_string())?;
    let xml = read_zip_entry(&mut archive, "word/document.xml")?;
    let comments_xml = read_zip_entry_optional(&mut archive, "word/comments.xml");
    let text = docx_body_text(&xml, comments_xml.as_deref());
    if !text.chars().any(|c| !c.is_whitespace()) {
        return Err(SKIP_NO_TEXT.into());
    }
    Ok(ExtractedDoc {
        title: file_title(path),
        pages: vec![text],
    })
}

fn read_zip_entry(archive: &mut ZipArchive<fs::File>, name: &str) -> Result<String, String> {
    let mut entry = archive.by_name(name).map_err(|e| e.to_string())?;
    let mut xml = String::new();
    entry.read_to_string(&mut xml).map_err(|e| e.to_string())?;
    Ok(xml)
}

fn read_zip_entry_optional(archive: &mut ZipArchive<fs::File>, name: &str) -> Option<String> {
    let mut entry = archive.by_name(name).ok()?;
    let mut xml = String::new();
    entry.read_to_string(&mut xml).ok()?;
    Some(xml)
}

fn docx_body_text(document_xml: &str, comments_xml: Option<&str>) -> String {
    let comments = comments_xml
        .map(parse_comments)
        .unwrap_or_else(|| CommentSet::default());
    let mut walk = walk_document(document_xml, &comments);
    if walk.paras.is_empty() {
        walk.paras = vec![strip_xml_text(document_xml)];
    }
    for c in &comments.order {
        if walk.used.contains(&c.id) {
            continue;
        }
        let mark = format_comment(&c.author, &c.date, &c.text);
        if mark.is_empty() {
            continue;
        }
        walk.paras.push(mark);
        walk.has_markup = true;
    }
    let text = if walk.paras.is_empty() {
        String::new()
    } else {
        walk.paras.join("\n\n")
    };
    if walk.has_markup {
        format!("{DOCX_MARKUP_LEGEND}\n\n{text}")
    } else {
        text
    }
}

#[derive(Debug, Clone, Default)]
struct Comment {
    id: String,
    author: String,
    date: String,
    text: String,
}

#[derive(Debug, Default)]
struct CommentSet {
    by_id: HashMap<String, Comment>,
    order: Vec<Comment>,
}

fn parse_comments(xml: &str) -> CommentSet {
    let mut out = CommentSet::default();
    let mut rest = xml;
    while let Some(rel) = rest.find("<w:comment") {
        let from = &rest[rel..];
        let is_comment = from.starts_with("<w:comment>")
            || from.starts_with("<w:comment ")
            || from.starts_with("<w:comment\t")
            || from.starts_with("<w:comment\n")
            || from.starts_with("<w:comment\r");
        if !is_comment {
            rest = &from[1..];
            continue;
        }
        let Some(gt) = from.find('>') else {
            break;
        };
        if from[..gt].ends_with('/') {
            rest = &from[gt + 1..];
            continue;
        }
        let open = &from[1..gt];
        let after_open = &from[gt + 1..];
        let Some(end_rel) = after_open.find("</w:comment>") else {
            break;
        };
        let inner = &after_open[..end_rel];
        let id = xml_attr(open, "w:id");
        if !id.is_empty() && !out.by_id.contains_key(&id) {
            let comment = Comment {
                id: id.clone(),
                author: xml_attr(open, "w:author"),
                date: date_only(&xml_attr(open, "w:date")),
                text: strip_xml_text(inner),
            };
            out.by_id.insert(id, comment.clone());
            out.order.push(comment);
        }
        rest = &after_open[end_rel + "</w:comment>".len()..];
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunKind {
    Normal,
    Ins,
    Del,
}

struct WalkResult {
    paras: Vec<String>,
    used: HashSet<String>,
    has_markup: bool,
}

fn walk_document(xml: &str, comments: &CommentSet) -> WalkResult {
    let mut in_tag = false;
    let mut tag_buf = String::new();
    let mut in_para = false;
    let mut builder = ParaBuilder::new();
    let mut paras = Vec::new();
    let mut ins: Vec<String> = Vec::new();
    let mut del: Vec<String> = Vec::new();
    let mut emitted = HashSet::new();
    let mut used = HashSet::new();
    let mut has_markup = false;

    for ch in xml.chars() {
        match ch {
            '<' => {
                in_tag = true;
                tag_buf.clear();
            }
            '>' => {
                in_tag = false;
                handle_tag(
                    &tag_buf,
                    &mut in_para,
                    &mut builder,
                    &mut paras,
                    &mut ins,
                    &mut del,
                    &mut emitted,
                    &mut used,
                    &mut has_markup,
                    comments,
                );
            }
            _ if in_tag => tag_buf.push(ch),
            _ if in_para => {
                let (kind, author) = current_run(&ins, &del);
                builder.push_char(kind, &author, ch);
            }
            _ => {}
        }
    }

    WalkResult {
        paras,
        used,
        has_markup,
    }
}

fn current_run(ins: &[String], del: &[String]) -> (RunKind, String) {
    if let Some(author) = del.last() {
        (RunKind::Del, author.clone())
    } else if let Some(author) = ins.last() {
        (RunKind::Ins, author.clone())
    } else {
        (RunKind::Normal, String::new())
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_tag(
    tag_buf: &str,
    in_para: &mut bool,
    builder: &mut ParaBuilder,
    paras: &mut Vec<String>,
    ins: &mut Vec<String>,
    del: &mut Vec<String>,
    emitted: &mut HashSet<String>,
    used: &mut HashSet<String>,
    has_markup: &mut bool,
    comments: &CommentSet,
) {
    let raw = tag_buf.trim();
    let is_end = raw.starts_with('/');
    let is_self = raw.ends_with('/');
    let name = tag_name(raw);

    if name == "w:br" || name == "w:cr" {
        if *in_para {
            let (kind, author) = current_run(ins, del);
            builder.break_line(kind, &author);
        }
        return;
    }
    if name == "w:tab" {
        if *in_para {
            let (kind, author) = current_run(ins, del);
            builder.tab(kind, &author);
        }
        return;
    }
    if name == "w:ins" || name == "w:moveTo" {
        if is_end {
            ins.pop();
        } else {
            ins.push(xml_attr(raw, "w:author"));
            if is_self {
                ins.pop();
            }
        }
        return;
    }
    if name == "w:del" || name == "w:moveFrom" {
        if is_end {
            del.pop();
        } else {
            del.push(xml_attr(raw, "w:author"));
            if is_self {
                del.pop();
            }
        }
        return;
    }
    if name == "w:commentRangeEnd" || name == "w:commentReference" {
        let id = xml_attr(raw, "w:id");
        if emit_comment(&id, comments, *in_para, builder, paras, emitted, used) {
            *has_markup = true;
        }
        return;
    }
    if name != "w:p" {
        return;
    }
    if is_self && !is_end {
        if !*in_para {
            paras.push(String::new());
        }
        return;
    }
    if is_end {
        if *in_para {
            paras.push(builder.finish());
            if builder.take_markup() {
                *has_markup = true;
            }
            *in_para = false;
        }
        return;
    }
    if !*in_para {
        *in_para = true;
        *builder = ParaBuilder::new();
    }
}

fn emit_comment(
    id: &str,
    comments: &CommentSet,
    in_para: bool,
    builder: &mut ParaBuilder,
    paras: &mut Vec<String>,
    emitted: &mut HashSet<String>,
    used: &mut HashSet<String>,
) -> bool {
    if id.is_empty() || !emitted.insert(id.to_string()) {
        return false;
    }
    let Some(c) = comments.by_id.get(id) else {
        return false;
    };
    let mark = format_comment(&c.author, &c.date, &c.text);
    if mark.is_empty() {
        return false;
    }
    used.insert(id.to_string());
    if in_para {
        builder.push_raw(&mark);
    } else if let Some(last) = paras.last_mut() {
        last.push_str(&mark);
    } else {
        paras.push(mark);
    }
    true
}

fn format_rev(sign: char, author: &str, text: &str) -> String {
    let text = text.trim();
    if text.is_empty() {
        return String::new();
    }
    if author.is_empty() {
        format!("〔{sign} {text}〕")
    } else {
        format!("〔{sign}{author}: {text}〕")
    }
}

fn format_comment(author: &str, date: &str, text: &str) -> String {
    let text = text.trim();
    if text.is_empty() {
        return String::new();
    }
    let mut head = String::from("〔注");
    if !author.is_empty() {
        head.push(' ');
        head.push_str(author);
    }
    if !date.is_empty() {
        head.push(' ');
        head.push_str(date);
    }
    head.push_str(": ");
    head.push_str(text);
    head.push('〕');
    head
}

fn tag_name(raw: &str) -> &str {
    raw.trim_start_matches('/')
        .trim_end_matches('/')
        .trim()
        .split(|c: char| c.is_whitespace() || c == '/')
        .next()
        .unwrap_or("")
}

fn xml_attr(tag: &str, key: &str) -> String {
    for quote in ['"', '\''] {
        let pat = format!("{key}={quote}");
        if let Some(pos) = tag.find(&pat) {
            let rest = &tag[pos + pat.len()..];
            if let Some(end) = rest.find(quote) {
                return rest[..end].to_string();
            }
        }
    }
    String::new()
}

fn date_only(raw: &str) -> String {
    let s = raw.trim();
    if s.len() >= 10 {
        let ymd: String = s.chars().take(10).collect();
        if ymd.as_bytes().get(4) == Some(&b'-') && ymd.as_bytes().get(7) == Some(&b'-') {
            return ymd;
        }
    }
    String::new()
}

struct ParaBuilder {
    out: String,
    last_was_space: bool,
    mark_sign: Option<char>,
    mark_author: String,
    mark_text: String,
    mark_space: bool,
    has_markup: bool,
}

impl ParaBuilder {
    fn new() -> Self {
        Self {
            out: String::new(),
            last_was_space: true,
            mark_sign: None,
            mark_author: String::new(),
            mark_text: String::new(),
            mark_space: true,
            has_markup: false,
        }
    }

    fn push_char(&mut self, kind: RunKind, author: &str, ch: char) {
        match kind {
            RunKind::Normal => {
                let _ = self.flush_mark();
                self.push_plain(ch);
            }
            RunKind::Ins => self.push_marked('+', author, ch),
            RunKind::Del => self.push_marked('-', author, ch),
        }
    }

    fn break_line(&mut self, kind: RunKind, author: &str) {
        match kind {
            RunKind::Normal => {
                let _ = self.flush_mark();
                self.out.push('\n');
                self.last_was_space = true;
            }
            RunKind::Ins => self.push_marked_break('+', author),
            RunKind::Del => self.push_marked_break('-', author),
        }
    }

    fn tab(&mut self, kind: RunKind, author: &str) {
        match kind {
            RunKind::Normal => {
                let _ = self.flush_mark();
                self.out.push('\t');
                self.last_was_space = true;
            }
            RunKind::Ins => self.push_marked_tab('+', author),
            RunKind::Del => self.push_marked_tab('-', author),
        }
    }

    fn push_raw(&mut self, s: &str) {
        let _ = self.flush_mark();
        self.out.push_str(s);
        self.last_was_space = false;
    }

    fn push_plain(&mut self, ch: char) {
        if ch.is_whitespace() {
            if !self.last_was_space {
                self.out.push(' ');
                self.last_was_space = true;
            }
        } else {
            self.out.push(ch);
            self.last_was_space = false;
        }
    }

    fn ensure_mark(&mut self, sign: char, author: &str) {
        if self.mark_sign == Some(sign) && self.mark_author == author {
            return;
        }
        let _ = self.flush_mark();
        self.mark_sign = Some(sign);
        self.mark_author = author.to_string();
        self.mark_text.clear();
        self.mark_space = true;
    }

    fn push_marked(&mut self, sign: char, author: &str, ch: char) {
        self.ensure_mark(sign, author);
        if ch.is_whitespace() {
            if !self.mark_space {
                self.mark_text.push(' ');
                self.mark_space = true;
            }
        } else {
            self.mark_text.push(ch);
            self.mark_space = false;
        }
    }

    fn push_marked_break(&mut self, sign: char, author: &str) {
        self.ensure_mark(sign, author);
        self.mark_text.push('\n');
        self.mark_space = true;
    }

    fn push_marked_tab(&mut self, sign: char, author: &str) {
        self.ensure_mark(sign, author);
        self.mark_text.push('\t');
        self.mark_space = true;
    }

    fn flush_mark(&mut self) -> bool {
        let Some(sign) = self.mark_sign.take() else {
            return false;
        };
        let author = std::mem::take(&mut self.mark_author);
        let text = std::mem::take(&mut self.mark_text);
        self.mark_space = true;
        let mark = format_rev(sign, &author, &text);
        if mark.is_empty() {
            return false;
        }
        self.out.push_str(&mark);
        self.last_was_space = false;
        self.has_markup = true;
        true
    }

    fn finish(&mut self) -> String {
        let _ = self.flush_mark();
        let s = std::mem::take(&mut self.out);
        s.trim().to_string()
    }

    fn take_markup(&mut self) -> bool {
        let v = self.has_markup;
        self.has_markup = false;
        v
    }
}

/// Extract text per `w:p` element (empty paragraphs kept as blank separators).
#[cfg(test)]
fn docx_paragraphs(xml: &str) -> Vec<String> {
    walk_document(xml, &CommentSet::default()).paras
}

fn strip_xml_text(xml: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    let mut tag_buf = String::new();
    let mut last_was_space = true;
    for ch in xml.chars() {
        match ch {
            '<' => {
                in_tag = true;
                tag_buf.clear();
            }
            '>' => {
                in_tag = false;
                let raw = tag_buf.trim();
                let name = raw
                    .trim_start_matches('/')
                    .split(|c: char| c.is_whitespace() || c == '/')
                    .next()
                    .unwrap_or("");
                if name == "w:br" || name == "w:cr" {
                    out.push('\n');
                    last_was_space = true;
                } else if name == "w:tab" {
                    out.push('\t');
                    last_was_space = true;
                }
            }
            _ if in_tag => tag_buf.push(ch),
            _ => {
                if ch.is_whitespace() {
                    if !last_was_space {
                        out.push(' ');
                        last_was_space = true;
                    }
                } else {
                    out.push(ch);
                    last_was_space = false;
                }
            }
        }
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;
    use zip::ZipWriter;

    fn wrap_body(inner: &str) -> String {
        format!(
            r#"<?xml version="1.0"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>{inner}</w:body>
</w:document>"#
        )
    }

    fn write_docx(document: &str, comments: Option<&str>) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("argos-docx-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sample.docx");
        let file = fs::File::create(&path).unwrap();
        let mut zip = ZipWriter::new(file);
        let opts = SimpleFileOptions::default();
        zip.start_file("word/document.xml", opts).unwrap();
        zip.write_all(document.as_bytes()).unwrap();
        if let Some(c) = comments {
            zip.start_file("word/comments.xml", opts).unwrap();
            zip.write_all(c.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
        path
    }

    #[test]
    fn docx_paragraphs_split_on_wp() {
        let xml = r#"<?xml version="1.0"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p><w:r><w:t>第一条（目的）</w:t></w:r></w:p>
    <w:p><w:pPr/><w:r><w:t>この契約は甲乙間の取引条件を定めることを目的として締結されるものであり、十分な長さの本文を持つ。</w:t></w:r></w:p>
    <w:p><w:r><w:t></w:t></w:r></w:p>
    <w:p><w:r><w:t>第二条（定義）</w:t></w:r></w:p>
    <w:p><w:r><w:t>本契約において用いる用語の定義は次のとおりとし、こちらも十分な長さの段落本文とするものである。</w:t></w:r></w:p>
  </w:body>
</w:document>"#;
        let paras = docx_paragraphs(xml);
        assert!(
            paras.iter().any(|p| p.contains("第一条")),
            "paras={paras:?}"
        );
        assert!(
            paras.iter().any(|p| p.contains("第二条")),
            "paras={paras:?}"
        );
        let joined = paras.join("\n\n");
        assert!(!joined.contains("※"), "plain xml must not grow a legend");
        let units = crate::extractor::segment_pages(&[joined]);
        assert!(
            units.len() >= 2,
            "expected blank-line units from w:p, got {}: {:?}",
            units.len(),
            units.iter().map(|u| &u.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn strip_xml_joins_adjacent_runs() {
        let inner = r#"<w:r><w:t>１</w:t></w:r><w:r><w:t>ヵ月以内</w:t></w:r>"#;
        assert_eq!(strip_xml_text(inner), "１ヵ月以内");
    }

    #[test]
    fn strip_xml_joins_split_parentheses() {
        let inner = concat!(
            r#"<w:r><w:t>（</w:t></w:r>"#,
            r#"<w:r><w:t>甲または甲の技術者の故意または過失による瑕疵</w:t></w:r>"#,
            r#"<w:r><w:t>）</w:t></w:r>"#,
        );
        assert_eq!(
            strip_xml_text(inner),
            "（甲または甲の技術者の故意または過失による瑕疵）"
        );
    }

    #[test]
    fn strip_xml_preserves_soft_break_and_tab() {
        let inner = r#"<w:r><w:t>前段</w:t><w:br/><w:t>後段</w:t><w:tab/><w:t>続き</w:t></w:r>"#;
        assert_eq!(strip_xml_text(inner), "前段\n後段\t続き");
    }

    #[test]
    fn docx_paragraph_with_split_runs_has_no_internal_newline() {
        let xml = concat!(
            r#"<?xml version="1.0"?>"#,
            r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">"#,
            r#"<w:body><w:p>"#,
            r#"<w:r><w:t>１</w:t></w:r>"#,
            r#"<w:r><w:t>ヵ月以内（個別契約において別途</w:t></w:r>"#,
            r#"<w:r><w:t>期間</w:t></w:r>"#,
            r#"<w:r><w:t>を定めた場合は個別契約の定めに従う。）</w:t></w:r>"#,
            r#"</w:p></w:body></w:document>"#,
        );
        let paras = docx_paragraphs(xml);
        assert_eq!(paras.len(), 1);
        assert_eq!(
            paras[0],
            "１ヵ月以内（個別契約において別途期間を定めた場合は個別契約の定めに従う。）"
        );
        assert!(!paras[0].contains('\n'));
    }

    #[test]
    fn ins_and_del_are_marked_and_not_mixed() {
        let xml = wrap_body(concat!(
            r#"<w:p><w:r><w:t>この契約は甲乙間の</w:t></w:r>"#,
            r#"<w:del w:author="山田"><w:r><w:delText>乙の同意を要する</w:delText></w:r></w:del>"#,
            r#"<w:ins w:author="山田"><w:r><w:t>甲の書面による承諾を要する</w:t></w:r></w:ins>"#,
            r#"<w:r><w:t>取引条件を定める。</w:t></w:r></w:p>"#,
        ));
        let text = docx_body_text(&xml, None);
        assert!(text.starts_with(DOCX_MARKUP_LEGEND), "{text}");
        assert!(text.contains("〔-山田: 乙の同意を要する〕"), "{text}");
        assert!(
            text.contains("〔+山田: 甲の書面による承諾を要する〕"),
            "{text}"
        );
        assert!(
            text.contains("この契約は甲乙間の〔-山田: 乙の同意を要する〕〔+山田: 甲の書面による承諾を要する〕取引条件を定める。"),
            "{text}"
        );
    }

    #[test]
    fn ins_wrapping_paragraph_is_marked() {
        let xml = wrap_body(
            r#"<w:ins w:author="山田"><w:p><w:r><w:t>新しい段落</w:t></w:r></w:p></w:ins>"#,
        );
        let text = docx_body_text(&xml, None);
        assert!(text.contains("〔+山田: 新しい段落〕"), "{text}");
        assert!(text.starts_with(DOCX_MARKUP_LEGEND), "{text}");
    }

    #[test]
    fn spanning_comment_emitted_once_at_range_end() {
        let document = wrap_body(concat!(
            r#"<w:p><w:commentRangeStart w:id="0"/><w:r><w:t>第一段落の本文は十分に長い。</w:t></w:r></w:p>"#,
            r#"<w:p><w:r><w:t>第二段落の本文も十分に長い。</w:t></w:r>"#,
            r#"<w:commentRangeEnd w:id="0"/>"#,
            r#"<w:r><w:commentReference w:id="0"/></w:r></w:p>"#,
        ));
        let comments = r#"<w:comments xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
          <w:comment w:id="0" w:author="山田" w:date="2024-01-15T10:00:00Z">
            <w:p><w:r><w:t>定義が曖昧</w:t></w:r></w:p>
          </w:comment>
        </w:comments>"#;
        let text = docx_body_text(&document, Some(comments));
        let n = text.matches("〔注 山田 2024-01-15: 定義が曖昧〕").count();
        assert_eq!(n, 1, "{text}");
        assert!(
            text.contains("第二段落の本文も十分に長い。〔注 山田 2024-01-15: 定義が曖昧〕"),
            "{text}"
        );
        assert!(!text.contains("第一段落の本文は十分に長い。〔注"), "{text}");
    }

    #[test]
    fn comment_on_range_without_wrapping_ins() {
        let document = wrap_body(concat!(
            r#"<w:p><w:r><w:t>この契約は甲乙間の取引条件を定める。</w:t></w:r>"#,
            r#"<w:commentRangeStart w:id="1"/>"#,
            r#"<w:commentRangeEnd w:id="1"/>"#,
            r#"<w:r><w:commentReference w:id="1"/></w:r></w:p>"#,
        ));
        let comments = r#"<w:comments xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
          <w:comment w:id="0" w:author="他" w:date="2024-01-01T00:00:00Z">
            <w:p><w:r><w:t>使われない</w:t></w:r></w:p>
          </w:comment>
          <w:comment w:id="1" w:author="山田" w:date="2024-01-15T10:00:00Z">
            <w:p><w:r><w:t>定義が曖昧</w:t></w:r></w:p>
          </w:comment>
        </w:comments>"#;
        let text = docx_body_text(&document, Some(comments));
        assert!(
            text.contains("この契約は甲乙間の取引条件を定める。〔注 山田 2024-01-15: 定義が曖昧〕"),
            "{text}"
        );
        assert!(text.contains("〔注 他 2024-01-01: 使われない〕"), "{text}");
    }

    #[test]
    fn extract_docx_without_comments_xml_does_not_fail() {
        let xml = wrap_body(r#"<w:p><w:r><w:t>本文だけ</w:t></w:r></w:p>"#);
        let path = write_docx(&xml, None);
        let doc = extract_docx(&path).unwrap();
        assert_eq!(doc.pages[0], "本文だけ");
        let _ = fs::remove_file(&path);
        let _ = fs::remove_dir(path.parent().unwrap());
    }

    #[test]
    fn extract_docx_reads_comments_xml() {
        let document = wrap_body(concat!(
            r#"<w:p><w:r><w:t>対象文。</w:t></w:r>"#,
            r#"<w:commentRangeStart w:id="0"/>"#,
            r#"<w:commentRangeEnd w:id="0"/>"#,
            r#"<w:r><w:commentReference w:id="0"/></w:r></w:p>"#,
        ));
        let comments = r#"<w:comments xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
          <w:comment w:id="0" w:author="山田" w:date="2024-01-15T10:00:00Z">
            <w:p><w:r><w:t>定義が曖昧</w:t></w:r></w:p>
          </w:comment>
        </w:comments>"#;
        let path = write_docx(&document, Some(comments));
        let doc = extract_docx(&path).unwrap();
        assert!(
            doc.pages[0].contains("〔注 山田 2024-01-15: 定義が曖昧〕"),
            "{}",
            doc.pages[0]
        );
        let _ = fs::remove_file(&path);
        let _ = fs::remove_dir(path.parent().unwrap());
    }

    #[test]
    fn plain_document_has_no_legend() {
        let xml = wrap_body(r#"<w:p><w:r><w:t>第一条（目的）</w:t></w:r></w:p>"#);
        let text = docx_body_text(&xml, None);
        assert_eq!(text, "第一条（目的）");
    }

    #[test]
    fn adjacent_del_runs_merge() {
        let xml = wrap_body(
            r#"<w:p><w:del w:author="山田"><w:r><w:delText>あい</w:delText></w:r><w:r><w:delText>うえ</w:delText></w:r></w:del></w:p>"#,
        );
        let text = docx_body_text(&xml, None);
        assert!(text.contains("〔-山田: あいうえ〕"), "{text}");
        assert_eq!(text.matches("〔-山田:").count(), 1, "{text}");
    }

    #[test]
    fn del_inner_wt_is_still_deletion() {
        let xml = wrap_body(
            r#"<w:p><w:r><w:t>残</w:t></w:r><w:del w:author="山田"><w:r><w:t>旧文</w:t></w:r></w:del></w:p>"#,
        );
        let text = docx_body_text(&xml, None);
        assert!(text.contains("残〔-山田: 旧文〕"), "{text}");
    }
}
