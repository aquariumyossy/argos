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
    let names = zip_names(&mut archive);
    let chart_names = part_names(&names, PartKind::Chart);
    let diagram_names = part_names(&names, PartKind::Diagram);
    let suppress = Embed {
        chart: !chart_names.is_empty(),
        diagram: !diagram_names.is_empty(),
    };
    let xml = read_zip_entry(&mut archive, "word/document.xml")?;
    let comments_xml = read_zip_entry_optional(&mut archive, "word/comments.xml");
    let (body, mut markup) = render_document(&xml, comments_xml.as_deref(), suppress);
    let mut sections = Vec::new();
    for name in part_names(&names, PartKind::Header) {
        let Some(part) = read_zip_entry_optional(&mut archive, &name) else {
            continue;
        };
        let (text, mark) = render_story(&part, Some("ヘッダー"), suppress);
        markup |= mark;
        push_section(&mut sections, text);
    }
    push_section(&mut sections, body);
    for (kind, place) in [
        (PartKind::Footer, "フッター"),
        (PartKind::Footnotes, "脚注"),
        (PartKind::Endnotes, "文末脚注"),
    ] {
        for name in part_names(&names, kind) {
            let Some(part) = read_zip_entry_optional(&mut archive, &name) else {
                continue;
            };
            let (text, mark) = render_story(&part, Some(place), suppress);
            markup |= mark;
            push_section(&mut sections, text);
        }
    }
    for name in &chart_names {
        let Some(part) = read_zip_entry_optional(&mut archive, name) else {
            continue;
        };
        push_section(&mut sections, labeled_lines("グラフ", &chart_lines(&part)));
    }
    for name in &diagram_names {
        let Some(part) = read_zip_entry_optional(&mut archive, name) else {
            continue;
        };
        push_section(&mut sections, labeled_lines("図", &diagram_lines(&part)));
    }
    for name in part_names(&names, PartKind::Drawing) {
        let Some(part) = read_zip_entry_optional(&mut archive, &name) else {
            continue;
        };
        let (text, mark) = render_story(&part, None, suppress);
        markup |= mark;
        push_section(&mut sections, text);
    }
    let text = with_legend(sections.join("\n\n"), markup);
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
    let (text, markup) = render_document(document_xml, comments_xml, Embed::off());
    with_legend(text, markup)
}

fn render_document(document_xml: &str, comments_xml: Option<&str>, embed: Embed) -> (String, bool) {
    let comments = comments_xml
        .map(parse_comments)
        .unwrap_or_else(|| CommentSet::default());
    let (mut paras, mut markup, used) = story_paragraphs(document_xml, &comments, None, embed);
    for c in &comments.order {
        if used.contains(&c.id) {
            continue;
        }
        let mark = format_comment(&c.author, &c.date, &c.text);
        if mark.is_empty() {
            continue;
        }
        paras.push(mark);
        markup = true;
    }
    (join_paras(&paras), markup)
}

fn render_story(xml: &str, place: Option<&str>, embed: Embed) -> (String, bool) {
    let (paras, markup, _) = story_paragraphs(xml, &CommentSet::default(), place, embed);
    (join_paras(&paras), markup)
}

fn join_paras(paras: &[String]) -> String {
    if paras.is_empty() {
        String::new()
    } else {
        paras.join("\n\n")
    }
}

fn with_legend(text: String, markup: bool) -> String {
    if markup {
        format!("{DOCX_MARKUP_LEGEND}\n\n{text}")
    } else {
        text
    }
}

fn push_section(sections: &mut Vec<String>, text: String) {
    if text.chars().any(|c| !c.is_whitespace()) {
        sections.push(text);
    }
}

fn labeled_lines(place: &str, lines: &[String]) -> String {
    lines
        .iter()
        .map(|line| label_line(Some(place), None, line))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[derive(Clone, Copy)]
struct Embed {
    chart: bool,
    diagram: bool,
}

impl Embed {
    fn off() -> Self {
        Self {
            chart: false,
            diagram: false,
        }
    }

    fn hides(self, kind: LiftKind) -> bool {
        match kind {
            LiftKind::Chart => self.chart,
            LiftKind::Diagram => self.diagram,
            LiftKind::TextBox | LiftKind::Shape => false,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PartKind {
    Header,
    Footer,
    Footnotes,
    Endnotes,
    Chart,
    Diagram,
    Drawing,
}

fn part_names(names: &[String], kind: PartKind) -> Vec<String> {
    let mut matched: Vec<String> = names
        .iter()
        .filter(|name| part_kind(name) == Some(kind))
        .cloned()
        .collect();
    if kind == PartKind::Diagram {
        let data: Vec<String> = matched
            .iter()
            .filter(|name| file_name(name).to_ascii_lowercase().starts_with("data"))
            .cloned()
            .collect();
        if !data.is_empty() {
            matched = data;
        }
    }
    matched.sort();
    matched
}

fn part_kind(name: &str) -> Option<PartKind> {
    let path = name.replace('\\', "/");
    let lower = path.to_ascii_lowercase();
    let file = file_name(&lower);
    if !lower.split('/').any(|part| part == "word") {
        return None;
    }
    if lower.contains("/charts/") && file.ends_with(".xml") {
        return Some(PartKind::Chart);
    }
    if lower.contains("/diagrams/") && file.ends_with(".xml") && !file.ends_with(".xml.rels") {
        return Some(PartKind::Diagram);
    }
    if lower.contains("/drawings/") && file.ends_with(".xml") {
        return Some(PartKind::Drawing);
    }
    if file == "footnotes.xml" {
        return Some(PartKind::Footnotes);
    }
    if file == "endnotes.xml" {
        return Some(PartKind::Endnotes);
    }
    if file.starts_with("header") && file.ends_with(".xml") {
        return Some(PartKind::Header);
    }
    if file.starts_with("footer") && file.ends_with(".xml") {
        return Some(PartKind::Footer);
    }
    None
}

fn file_name(name: &str) -> String {
    name.rsplit(['/', '\\']).next().unwrap_or(name).to_string()
}

fn zip_names(archive: &mut ZipArchive<fs::File>) -> Vec<String> {
    let mut names = Vec::new();
    for index in 0..archive.len() {
        let Ok(entry) = archive.by_index(index) else {
            continue;
        };
        if entry.is_file() {
            names.push(entry.name().to_string());
        }
    }
    names
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LiftKind {
    TextBox,
    Shape,
    Chart,
    Diagram,
}

struct Lifted {
    kind: LiftKind,
    paras: Vec<String>,
    markup: bool,
    used: HashSet<String>,
}

fn story_paragraphs(
    xml: &str,
    comments: &CommentSet,
    place: Option<&str>,
    embed: Embed,
) -> (Vec<String>, bool, HashSet<String>) {
    let stripped = without_duplicate_fallback(xml);
    let (peeled, lifted) = lift_regions(&stripped, comments);
    let mut walk = walk_document(&peeled, comments);
    if walk.paras.is_empty() && lifted.is_empty() {
        let plain = strip_xml_text(&peeled);
        if !plain.is_empty() {
            walk.paras.push(plain);
        }
    }
    let mut markup = walk.has_markup;
    let (paras, used_regions) = expand_markers(&walk.paras, &lifted, place, embed);
    for item in &lifted {
        if item.markup {
            markup = true;
        }
        for id in &item.used {
            walk.used.insert(id.clone());
        }
    }
    let mut paras = paras;
    for (index, item) in lifted.iter().enumerate() {
        if used_regions[index] || embed.hides(item.kind) {
            continue;
        }
        for para in &item.paras {
            let line = label_line(place, Some(item.kind), para);
            if !line.is_empty() {
                paras.push(line);
            }
        }
    }
    (paras, markup, walk.used)
}

fn expand_markers(
    paras: &[String],
    lifted: &[Lifted],
    place: Option<&str>,
    embed: Embed,
) -> (Vec<String>, Vec<bool>) {
    let mut used = vec![false; lifted.len()];
    let mut out = Vec::new();
    for para in paras {
        if !para.contains('\u{E000}') {
            let line = label_plain(place, para);
            if line.is_empty() && place.is_some() {
                continue;
            }
            out.push(line);
            continue;
        }
        let mut rest = para.as_str();
        let mut plain = String::new();
        while let Some(start) = rest.find('\u{E000}') {
            plain.push_str(&rest[..start]);
            let after = &rest[start + '\u{E000}'.len_utf8()..];
            let Some(end) = after.find('\u{E001}') else {
                plain.push_str(&rest[start..]);
                rest = "";
                break;
            };
            let index = after[..end].parse::<usize>().ok();
            flush_plain(&mut out, place, &plain);
            plain.clear();
            if let Some(index) = index {
                if let Some(item) = lifted.get(index) {
                    used[index] = true;
                    if !embed.hides(item.kind) {
                        for piece in &item.paras {
                            let line = label_line(place, Some(item.kind), piece);
                            if !line.is_empty() {
                                out.push(line);
                            }
                        }
                    }
                }
            }
            rest = &after[end + '\u{E001}'.len_utf8()..];
        }
        plain.push_str(rest);
        flush_plain(&mut out, place, &plain);
    }
    (out, used)
}

fn flush_plain(out: &mut Vec<String>, place: Option<&str>, plain: &str) {
    if plain.trim().is_empty() {
        return;
    }
    let line = label_plain(place, plain.trim());
    if !line.is_empty() {
        out.push(line);
    }
}

fn label_plain(place: Option<&str>, text: &str) -> String {
    label_line(place, None, text)
}

fn label_line(place: Option<&str>, kind: Option<LiftKind>, text: &str) -> String {
    let text = text.trim();
    if text.is_empty() {
        return String::new();
    }
    let mut prefix = String::new();
    if let Some(place) = place {
        prefix.push('〔');
        prefix.push_str(place);
        prefix.push('〕');
    }
    if let Some(kind) = kind {
        let word = match kind {
            LiftKind::TextBox | LiftKind::Shape => "テキストボックス/図形",
            LiftKind::Chart => "グラフ",
            LiftKind::Diagram => "図",
        };
        prefix.push('〔');
        prefix.push_str(word);
        prefix.push('〕');
    }
    if prefix.is_empty() {
        text.to_string()
    } else {
        format!("{prefix}{text}")
    }
}

fn without_duplicate_fallback(xml: &str) -> String {
    let mut out = String::new();
    let mut pos = 0usize;
    let mut ac_ids: Vec<usize> = Vec::new();
    let mut choice_depth: Vec<i32> = Vec::new();
    let mut choice_has_text: Vec<bool> = Vec::new();
    let mut open_fallbacks: Vec<usize> = Vec::new();
    let mut skipping: i32 = 0;
    while let Some(tag) = next_xml_tag(xml, pos) {
        let gap = &xml[pos..tag.start];
        if skipping == 0 && open_fallbacks.is_empty() && gap.chars().any(|ch| !ch.is_whitespace()) {
            for &id in &ac_ids {
                if choice_depth[id] > 0 {
                    choice_has_text[id] = true;
                }
            }
        }
        if skipping > 0 {
            if tag.local == "Fallback" && !tag.is_end && !tag.is_self {
                skipping += 1;
            } else if tag.local == "Fallback" && tag.is_end && skipping > 0 {
                skipping -= 1;
                if skipping == 0 {
                    open_fallbacks.pop();
                }
            }
            pos = tag.end;
            continue;
        }
        if tag.local == "AlternateContent" && !tag.is_end && !tag.is_self {
            ac_ids.push(choice_depth.len());
            choice_depth.push(0);
            choice_has_text.push(false);
        } else if tag.local == "AlternateContent" && tag.is_end {
            ac_ids.pop();
        } else if tag.local == "Choice" && !tag.is_end && !tag.is_self {
            if let Some(&id) = ac_ids.last() {
                choice_depth[id] += 1;
            }
        } else if tag.local == "Choice" && tag.is_end {
            if let Some(&id) = ac_ids.last() {
                choice_depth[id] = choice_depth[id].saturating_sub(1);
            }
        } else if tag.local == "Fallback" && !tag.is_end && !tag.is_self {
            if let Some(&id) = ac_ids.last() {
                if choice_has_text.get(id).copied().unwrap_or(false) {
                    open_fallbacks.push(id);
                    skipping = 1;
                    out.push_str(gap);
                    pos = tag.end;
                    continue;
                }
            }
        }
        out.push_str(gap);
        out.push_str(&xml[tag.start..tag.end]);
        pos = tag.end;
    }
    if skipping == 0 {
        out.push_str(&xml[pos..]);
    }
    out
}

fn lift_regions(xml: &str, comments: &CommentSet) -> (String, Vec<Lifted>) {
    let mut out = String::new();
    let mut lifted = Vec::new();
    let mut pos = 0usize;
    while let Some(tag) = next_xml_tag(xml, pos) {
        if !tag.is_end && !tag.is_self && tag.local == "txbxContent" {
            if let Some(end) = element_end(xml, tag.end, &tag.qname) {
                let inner = &xml[tag.end..end - close_len(&tag.qname)];
                let (paras, markup, used) = textbox_paragraphs(inner, comments);
                out.push_str(&xml[pos..tag.start]);
                if paras
                    .iter()
                    .any(|para| para.chars().any(|c| !c.is_whitespace()))
                {
                    let index = lifted.len();
                    lifted.push(Lifted {
                        kind: LiftKind::TextBox,
                        paras,
                        markup,
                        used,
                    });
                    out.push_str(&marker(index));
                }
                pos = end;
                continue;
            }
        }
        if !tag.is_end && !tag.is_self {
            if let Some(kind) = region_kind(&tag, xml) {
                if let Some(end) = element_end(xml, tag.end, &tag.qname) {
                    let inner_end = end - close_len(&tag.qname);
                    let inner = &xml[tag.end..inner_end];
                    let paras = match kind {
                        LiftKind::Chart => chart_lines(inner),
                        LiftKind::Diagram => diagram_lines(inner),
                        LiftKind::TextBox | LiftKind::Shape => Vec::new(),
                    };
                    out.push_str(&xml[pos..tag.start]);
                    if !paras.is_empty() {
                        let index = lifted.len();
                        lifted.push(Lifted {
                            kind,
                            paras,
                            markup: false,
                            used: HashSet::new(),
                        });
                        out.push_str(&marker(index));
                    }
                    pos = end;
                    continue;
                }
            }
        }
        if !tag.is_end && !tag.is_self && tag.qname == "a:t" {
            let close = format!("</{}>", tag.qname);
            if let Some(rel) = xml[tag.end..].find(&close) {
                let inner_end = tag.end + rel;
                let text = xml[tag.end..inner_end].trim();
                if !text.is_empty() && !text.contains('<') {
                    let index = lifted.len();
                    lifted.push(Lifted {
                        kind: LiftKind::Shape,
                        paras: vec![text.to_string()],
                        markup: false,
                        used: HashSet::new(),
                    });
                    out.push_str(&xml[pos..tag.end]);
                    out.push_str(&marker(index));
                    pos = inner_end;
                    continue;
                }
            }
        }
        out.push_str(&xml[pos..tag.end]);
        pos = tag.end;
    }
    out.push_str(&xml[pos..]);
    (out, lifted)
}

fn textbox_paragraphs(xml: &str, comments: &CommentSet) -> (Vec<String>, bool, HashSet<String>) {
    // One more peel so a text box does not keep its own VML fallback.
    let stripped = without_duplicate_fallback(xml);
    let walk = walk_document(&stripped, comments);
    let paras = if walk.paras.is_empty() {
        let plain = strip_xml_text(&stripped);
        if plain.is_empty() {
            Vec::new()
        } else {
            vec![plain]
        }
    } else {
        walk.paras
    };
    (paras, walk.has_markup, walk.used)
}

fn region_kind(tag: &XmlTag, xml: &str) -> Option<LiftKind> {
    match tag.local.as_str() {
        "chart" | "chartSpace" | "chartsheet" => Some(LiftKind::Chart),
        "relIds" | "dataModel" => Some(LiftKind::Diagram),
        "graphicData" => {
            let raw = &xml[tag.start..tag.end];
            let uri = xml_attr(raw, "uri").to_ascii_lowercase();
            if uri.contains("chart") {
                Some(LiftKind::Chart)
            } else if uri.contains("diagram") {
                Some(LiftKind::Diagram)
            } else {
                None
            }
        }
        _ => None,
    }
}

fn marker(index: usize) -> String {
    format!("\u{E000}{index}\u{E001}")
}

fn close_len(qname: &str) -> usize {
    format!("</{qname}>").len()
}

fn chart_lines(xml: &str) -> Vec<String> {
    leaf_pairs(xml, &["t", "v"])
        .into_iter()
        .filter(|(local, text)| *local != "v" || !is_numeric_blob(text))
        .map(|(_, text)| text)
        .collect()
}

fn diagram_lines(xml: &str) -> Vec<String> {
    leaf_pairs(xml, &["t"])
        .into_iter()
        .map(|(_, text)| text)
        .collect()
}

fn is_numeric_blob(text: &str) -> bool {
    let text = text.trim();
    !text.is_empty()
        && text.chars().all(|ch| {
            ch.is_ascii_digit() || matches!(ch, '.' | ',' | '-' | '+' | '%' | ' ' | 'e' | 'E')
        })
}

fn leaf_pairs(xml: &str, locals: &[&str]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while let Some(tag) = next_xml_tag(xml, pos) {
        if !tag.is_end && !tag.is_self && locals.iter().any(|local| *local == tag.local) {
            let close = format!("</{}>", tag.qname);
            if let Some(rel) = xml[tag.end..].find(&close) {
                let inner = &xml[tag.end..tag.end + rel];
                if !inner.contains('<') {
                    let text = inner.trim();
                    if !text.is_empty() {
                        out.push((tag.local.clone(), text.to_string()));
                    }
                    pos = tag.end + rel + close.len();
                    continue;
                }
            }
        }
        pos = tag.end;
    }
    out
}

struct XmlTag {
    qname: String,
    local: String,
    start: usize,
    end: usize,
    is_end: bool,
    is_self: bool,
}

fn element_end(xml: &str, from: usize, qname: &str) -> Option<usize> {
    let mut depth = 1i32;
    let mut pos = from;
    while let Some(tag) = next_xml_tag(xml, pos) {
        if tag.qname == qname && !tag.is_end && !tag.is_self {
            depth += 1;
        } else if tag.is_end && tag.qname == qname {
            depth -= 1;
            if depth == 0 {
                return Some(tag.end);
            }
        }
        pos = tag.end;
    }
    None
}

fn next_xml_tag(xml: &str, from: usize) -> Option<XmlTag> {
    let bytes = xml.as_bytes();
    let mut i = from.min(bytes.len());
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        if xml[i..].starts_with("<!--") {
            let end = xml[i + 4..].find("-->")?;
            i = i + 4 + end + 3;
            continue;
        }
        if xml[i..].starts_with("<![CDATA[") {
            let end = xml[i + 9..].find("]]>")?;
            i = i + 9 + end + 3;
            continue;
        }
        if xml[i..].starts_with("<?") {
            let end = xml[i + 2..].find("?>")?;
            i = i + 2 + end + 2;
            continue;
        }
        let start = i;
        i += 1;
        let mut quote: Option<u8> = None;
        while i < bytes.len() {
            let byte = bytes[i];
            if let Some(q) = quote {
                if byte == q {
                    quote = None;
                }
            } else if byte == b'"' || byte == b'\'' {
                quote = Some(byte);
            } else if byte == b'>' {
                i += 1;
                let raw = &xml[start + 1..i - 1];
                return Some(parse_xml_tag(start, i, raw));
            }
            i += 1;
        }
        return None;
    }
    None
}

fn parse_xml_tag(start: usize, end: usize, raw: &str) -> XmlTag {
    let trimmed = raw.trim();
    let is_end = trimmed.starts_with('/');
    let is_self = trimmed.ends_with('/');
    let body = trimmed.trim_start_matches('/').trim_end_matches('/').trim();
    let qname = body
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_end_matches('/')
        .to_string();
    let local = qname.rsplit(':').next().unwrap_or(&qname).to_string();
    XmlTag {
        qname,
        local,
        start,
        end,
        is_end,
        is_self,
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

    fn alt_box(text: &str) -> String {
        format!(
            r#"<mc:AlternateContent><mc:Choice Requires="wps"><w:drawing><w:txbxContent><w:p><w:r><w:t>{text}</w:t></w:r></w:p></w:txbxContent></w:drawing></mc:Choice><mc:Fallback><w:pict><v:textbox><w:txbxContent><w:p><w:r><w:t>{text}</w:t></w:r></w:p></w:txbxContent></v:textbox></w:pict></mc:Fallback></mc:AlternateContent>"#
        )
    }

    fn write_parts(files: &[(&str, &str)]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("argos-docx-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sample.docx");
        let file = fs::File::create(&path).unwrap();
        let mut zip = ZipWriter::new(file);
        let opts = SimpleFileOptions::default();
        for (name, body) in files {
            zip.start_file(*name, opts).unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
        path
    }

    #[test]
    fn alternate_content_text_box_is_read_once_between_neighbors() {
        let xml = wrap_body(&format!(
            r#"<w:p><w:r><w:t>前文</w:t></w:r><w:r>{}</w:r><w:r><w:t>後文</w:t></w:r></w:p>"#,
            alt_box("枠の中の文章")
        ));
        let text = docx_body_text(&xml, None);
        assert_eq!(text.matches("枠の中の文章").count(), 1, "{text}");
        assert!(
            text.contains("〔テキストボックス/図形〕枠の中の文章"),
            "{text}"
        );
        assert!(text.contains("前文"), "{text}");
        assert!(text.contains("後文"), "{text}");
        let fallback_only = wrap_body(
            r#"<w:p><w:r><mc:AlternateContent><mc:Choice Requires="wps"><w:drawing/></mc:Choice><mc:Fallback><w:txbxContent><w:p><w:r><w:t>残った枠</w:t></w:r></w:p></w:txbxContent></mc:Fallback></mc:AlternateContent></w:r></w:p>"#,
        );
        let kept = docx_body_text(&fallback_only, None);
        assert!(kept.contains("残った枠"), "{kept}");
    }

    #[test]
    fn embedded_chart_and_diagram_are_labeled_without_a_part_file() {
        let xml = wrap_body(
            r#"<w:p><w:r><w:t>本文</w:t></w:r></w:p><w:p><w:r><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/chart"><c:tx><a:t>埋め込みグラフ</a:t><c:v>12</c:v><c:v>4月</c:v></c:tx></a:graphicData></w:r></w:p><w:p><w:r><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/diagram"><a:t>埋め込み図</a:t></a:graphicData></w:r></w:p>"#,
        );
        let text = docx_body_text(&xml, None);
        assert!(text.contains("〔グラフ〕埋め込みグラフ"), "{text}");
        assert!(text.contains("〔グラフ〕4月"), "{text}");
        assert!(!text.contains("〔グラフ〕12"), "{text}");
        assert!(text.contains("〔図〕埋め込み図"), "{text}");
        assert_eq!(text.matches("埋め込みグラフ").count(), 1, "{text}");
        assert!(text.contains("本文"), "{text}");
    }

    #[test]
    fn extract_file_reads_stories_shapes_charts_and_diagrams_with_location() {
        let document = format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p><w:r><w:t>前文</w:t></w:r><w:r>{}</w:r><w:r><w:t>後文</w:t></w:r></w:p>
    <w:p><w:r><w:t>本文だけ</w:t></w:r></w:p>
    <w:p><w:r><w:drawing><a:txBody><a:p><a:r><a:t>図形の文章</a:t></a:r></a:p></a:txBody></w:drawing></w:r></w:p>
    <w:p><w:r><w:pict><w:txbxContent><w:p><w:r><w:t>古い枠</w:t></w:r></w:p></w:txbxContent></w:pict></w:r></w:p>
    <w:p><w:r><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/chart"><a:t>グラフの題</a:t></a:graphicData></w:r></w:p>
    <w:p><w:r><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/diagram"><a:t>図の項目</a:t></a:graphicData></w:r></w:p>
  </w:body>
</w:document>"#,
            alt_box("枠の中の文章")
        );
        let header = format!(
            r#"<?xml version="1.0"?><w:hdr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:p><w:r><w:t>ヘッダーの文章</w:t></w:r></w:p><w:p><w:r><w:txbxContent><w:p><w:r><w:t>ヘッダー枠</w:t></w:r></w:p></w:txbxContent></w:r></w:p></w:hdr>"#
        );
        let footer = r#"<?xml version="1.0"?><w:ftr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:p><w:r><w:t>フッターの文章</w:t></w:r></w:p><w:p><w:r><w:txbxContent><w:p><w:r><w:t>フッター枠</w:t></w:r></w:p></w:txbxContent></w:r></w:p></w:ftr>"#;
        let footnotes = r#"<?xml version="1.0"?><w:footnotes xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:footnote w:id="1"><w:p><w:r><w:t>脚注の文章</w:t></w:r></w:p></w:footnote></w:footnotes>"#;
        let endnotes = r#"<?xml version="1.0"?><w:endnotes xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:endnote w:id="1"><w:p><w:r><w:t>文末脚注の文章</w:t></w:r></w:p></w:endnote></w:endnotes>"#;
        let chart = r#"<?xml version="1.0"?><c:chartSpace xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><c:tx><a:t>グラフの題</a:t></c:tx><c:v>12</c:v><c:v>4月</c:v></c:chartSpace>"#;
        let diagram = r#"<?xml version="1.0"?><dgm:dataModel xmlns:dgm="http://schemas.openxmlformats.org/drawingml/2006/diagram" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><dgm:pt><dgm:t><a:p><a:r><a:t>図の項目</a:t></a:r></a:p></dgm:t></dgm:pt></dgm:dataModel>"#;
        let drawing_dup = r#"<?xml version="1.0"?><dgm:drawing xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><a:t>図の項目</a:t></dgm:drawing>"#;
        let drawing = r#"<?xml version="1.0"?><w:drawing xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><a:t>別の図形</a:t></w:drawing>"#;
        let path = write_parts(&[
            ("word/document.xml", document.as_str()),
            ("word/header1.xml", header.as_str()),
            ("word/footer1.xml", footer),
            ("word/footnotes.xml", footnotes),
            ("word/endnotes.xml", endnotes),
            ("word/charts/chart1.xml", chart),
            ("word/diagrams/data1.xml", diagram),
            ("word/diagrams/drawing1.xml", drawing_dup),
            ("word/drawings/drawing1.xml", drawing),
        ]);
        let doc = crate::extractor::extract_file(&path).unwrap();
        let text = &doc.pages[0];
        assert!(text.contains("〔ヘッダー〕ヘッダーの文章"), "{text}");
        assert!(
            text.contains("〔ヘッダー〕〔テキストボックス/図形〕ヘッダー枠"),
            "{text}"
        );
        assert!(text.contains("〔フッター〕フッターの文章"), "{text}");
        assert!(
            text.contains("〔フッター〕〔テキストボックス/図形〕フッター枠"),
            "{text}"
        );
        assert!(text.contains("〔脚注〕脚注の文章"), "{text}");
        assert!(text.contains("〔文末脚注〕文末脚注の文章"), "{text}");
        assert!(text.contains("前文") && text.contains("後文"), "{text}");
        assert!(text.contains("本文だけ"), "{text}");
        assert_eq!(text.matches("枠の中の文章").count(), 1, "{text}");
        assert!(
            text.contains("〔テキストボックス/図形〕枠の中の文章"),
            "{text}"
        );
        assert!(
            text.contains("〔テキストボックス/図形〕図形の文章"),
            "{text}"
        );
        assert!(text.contains("〔テキストボックス/図形〕古い枠"), "{text}");
        assert!(text.contains("〔テキストボックス/図形〕別の図形"), "{text}");
        assert_eq!(text.matches("グラフの題").count(), 1, "{text}");
        assert!(text.contains("〔グラフ〕グラフの題"), "{text}");
        assert!(text.contains("〔グラフ〕4月"), "{text}");
        assert!(!text.contains("〔グラフ〕12"), "{text}");
        assert_eq!(text.matches("図の項目").count(), 1, "{text}");
        assert!(text.contains("〔図〕図の項目"), "{text}");
        let header_at = text.find("〔ヘッダー〕").unwrap();
        let body_at = text.find("本文だけ").unwrap();
        let note_at = text.find("〔脚注〕").unwrap();
        let chart_at = text.find("〔グラフ〕").unwrap();
        assert!(header_at < body_at && body_at < note_at && note_at < chart_at);
        let _ = fs::remove_file(&path);
        let _ = fs::remove_dir(path.parent().unwrap());
    }
}
