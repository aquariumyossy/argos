//! XML text nodes for Office packages. Markup outside the text is left as-is.

use super::text::replace_pieces;

#[derive(Debug, Clone)]
pub struct TextNode {
    pub open_start: usize,
    pub inner_start: usize,
    pub close_end: usize,
    pub text: String,
    pub para: u32,
    pub para_local: String,
    pub deleted: bool,
    pub local: String,
    pub dirty: bool,
}

#[derive(Debug, Clone)]
pub struct ParaGroup {
    pub local: String,
    pub visible: Vec<usize>,
    pub deleted: Vec<usize>,
    pub first: usize,
}

pub struct ScanOpt {
    pub para_locals: &'static [&'static str],
    pub text_locals: &'static [&'static str],
    pub track_del: bool,
    /// `<v>` text of a cell whose `t` attribute is `str` (cached formula text).
    pub cached_string_values: bool,
}

#[derive(Debug)]
pub(crate) struct Tag {
    pub qname: String,
    pub local: String,
    pub start: usize,
    pub end: usize,
    pub is_end: bool,
    pub is_self: bool,
}

pub fn scan_text_nodes(xml: &str, opt: &ScanOpt) -> Vec<TextNode> {
    let mut nodes = Vec::new();
    let mut para_stack: Vec<(u32, String)> = Vec::new();
    let mut next_para = 1u32;
    let mut del_depth = 0i32;
    let mut cell_type = String::new();
    let mut in_cell = false;
    let mut pos = 0usize;
    while let Some(tag) = next_tag(xml, pos) {
        if tag.is_end {
            if opt.para_locals.contains(&tag.local.as_str()) {
                para_stack.pop();
            }
            if opt.track_del && is_deletion(&tag.local) {
                del_depth = del_depth.saturating_sub(1);
            }
            if tag.local == "c" {
                in_cell = false;
                cell_type.clear();
            }
            pos = tag.end;
            continue;
        }
        if opt.track_del && is_deletion(&tag.local) && !tag.is_self {
            del_depth += 1;
        }
        if tag.local == "c" && !tag.is_self {
            in_cell = true;
            cell_type = attr_local_value(&xml[tag.start..tag.end], "t").unwrap_or_default();
        }
        if opt.para_locals.contains(&tag.local.as_str()) && !tag.is_self {
            para_stack.push((next_para, tag.local.clone()));
            next_para += 1;
        }
        let take_text = opt.text_locals.contains(&tag.local.as_str())
            || (opt.cached_string_values && tag.local == "v" && in_cell && cell_type == "str");
        if take_text && !tag.is_self {
            let close = format!("</{}>", tag.qname);
            if let Some(rel) = xml[tag.end..].find(&close) {
                let inner_start = tag.end;
                let inner_end = tag.end + rel;
                let close_end = inner_end + close.len();
                let (para, para_local) = para_stack
                    .last()
                    .map(|(id, local)| (*id, local.clone()))
                    .unwrap_or((0, String::new()));
                let deleted = tag.local == "delText" || (opt.track_del && del_depth > 0);
                nodes.push(TextNode {
                    open_start: tag.start,
                    inner_start,
                    close_end,
                    text: decode_node_text(&xml[inner_start..inner_end]),
                    para,
                    para_local,
                    deleted,
                    local: tag.local.clone(),
                    dirty: false,
                });
                pos = close_end;
                continue;
            }
        }
        pos = tag.end;
    }
    nodes
}

fn is_deletion(local: &str) -> bool {
    local == "del" || local == "moveFrom"
}

pub fn group_nodes(nodes: &[TextNode]) -> Vec<ParaGroup> {
    let mut grouped: Vec<ParaGroup> = Vec::new();
    let mut index_of: std::collections::HashMap<u32, usize> = std::collections::HashMap::new();
    for (index, node) in nodes.iter().enumerate() {
        if node.para == 0 {
            let mut group = ParaGroup {
                local: node.para_local.clone(),
                visible: Vec::new(),
                deleted: Vec::new(),
                first: index,
            };
            push_node(&mut group, index, node.deleted);
            grouped.push(group);
            continue;
        }
        if let Some(&slot) = index_of.get(&node.para) {
            push_node(&mut grouped[slot], index, node.deleted);
            continue;
        }
        let mut group = ParaGroup {
            local: node.para_local.clone(),
            visible: Vec::new(),
            deleted: Vec::new(),
            first: index,
        };
        push_node(&mut group, index, node.deleted);
        index_of.insert(node.para, grouped.len());
        grouped.push(group);
    }
    grouped.sort_by_key(|group| group.first);
    grouped
}

fn push_node(group: &mut ParaGroup, index: usize, deleted: bool) {
    if deleted {
        group.deleted.push(index);
    } else {
        group.visible.push(index);
    }
}

pub fn joined(nodes: &[TextNode], indexes: &[usize]) -> String {
    let mut out = String::new();
    for &index in indexes {
        out.push_str(&nodes[index].text);
    }
    out
}

pub fn replace_in_group(
    nodes: &mut [TextNode],
    indexes: &[usize],
    find: &str,
    replace: &str,
) -> usize {
    if indexes.is_empty() {
        return 0;
    }
    let mut pieces: Vec<String> = indexes
        .iter()
        .map(|&index| nodes[index].text.clone())
        .collect();
    let count = replace_pieces(&mut pieces, find, replace);
    if count == 0 {
        return 0;
    }
    for (slot, &index) in indexes.iter().enumerate() {
        if nodes[index].text != pieces[slot] {
            nodes[index].text = pieces[slot].clone();
            nodes[index].dirty = true;
        }
    }
    count
}

pub fn rewrite_group(nodes: &mut [TextNode], group: &ParaGroup, new_text: &str) {
    let primary = if group.visible.is_empty() {
        group.deleted.clone()
    } else {
        group.visible.clone()
    };
    if primary.is_empty() {
        return;
    }
    for (slot, &index) in primary.iter().enumerate() {
        let next = if slot == 0 {
            new_text.to_string()
        } else {
            String::new()
        };
        if nodes[index].text != next {
            nodes[index].text = next;
            nodes[index].dirty = true;
        }
    }
    if !group.visible.is_empty() {
        for &index in &group.deleted {
            if !nodes[index].text.is_empty() {
                nodes[index].text.clear();
                nodes[index].dirty = true;
            }
        }
    }
}

pub fn render_nodes(xml: &str, nodes: &[TextNode]) -> String {
    let mut dirty: Vec<&TextNode> = nodes.iter().filter(|node| node.dirty).collect();
    dirty.sort_by_key(|node| std::cmp::Reverse(node.open_start));
    let mut out = xml.to_string();
    for node in dirty {
        let open = &xml[node.open_start..node.inner_start];
        let rendered = render_text_element(open, &node.text, &node.local);
        out.replace_range(node.open_start..node.close_end, &rendered);
    }
    out
}

fn render_text_element(open: &str, text: &str, local: &str) -> String {
    let text = text.replace('\r', "");
    // Word splits a line break into w:br. Excel shared strings keep a newline
    // inside <t xml:space="preserve">; a w:br there is not valid xlsx.
    let word_break = local == "t" && text.contains('\n') && is_word_text_tag(open);
    let open = ensure_preserve(open, &text);
    if !word_break {
        let close = close_tag(&open);
        return format!("{open}{}{close}", escape_text(&text));
    }
    let mut out = String::new();
    for (index, line) in text.split('\n').enumerate() {
        if index > 0 {
            out.push_str("<w:br/>");
        }
        let line_open = ensure_preserve(&open, line);
        out.push_str(&line_open);
        out.push_str(&escape_text(line));
        out.push_str(&close_tag(&line_open));
    }
    out
}

fn is_word_text_tag(open: &str) -> bool {
    let trimmed = open.trim_start();
    trimmed.starts_with("<w:t") || trimmed.starts_with("<w:delText")
}

fn close_tag(open: &str) -> String {
    let inner = open.trim_start_matches('<').trim_end_matches('>').trim();
    let name = inner
        .split_whitespace()
        .next()
        .unwrap_or("t")
        .trim_end_matches('/');
    format!("</{name}>")
}

fn ensure_preserve(open: &str, text: &str) -> String {
    let needs = text.contains('\n')
        || text.contains('\t')
        || text.starts_with(char::is_whitespace)
        || text.ends_with(char::is_whitespace)
        || text.contains("  ");
    if !needs || open.contains("xml:space") {
        return open.to_string();
    }
    let Some(end) = open.rfind('>') else {
        return open.to_string();
    };
    let mut out = open.to_string();
    out.insert_str(end, " xml:space=\"preserve\"");
    out
}

pub fn map_attr_local(xml: &str, local: &str, mut edit: impl FnMut(&str) -> String) -> String {
    let mut out = String::with_capacity(xml.len());
    let mut pos = 0usize;
    while let Some(tag) = next_tag(xml, pos) {
        out.push_str(&xml[pos..tag.start]);
        let raw = &xml[tag.start..tag.end];
        if tag.is_end {
            out.push_str(raw);
        } else {
            out.push_str(&rewrite_attr_local(raw, local, &mut edit));
        }
        pos = tag.end;
    }
    out.push_str(&xml[pos..]);
    out
}

fn rewrite_attr_local(tag: &str, local: &str, edit: &mut impl FnMut(&str) -> String) -> String {
    let attrs = attributes(tag);
    let Some(attr) = attrs
        .iter()
        .rev()
        .find(|attr| attr_local(&attr.key) == local)
    else {
        return tag.to_string();
    };
    let next = edit(&attr.value);
    if next == attr.value || attr.val_start == 0 || attr.val_end >= tag.len() {
        return tag.to_string();
    }
    let mut out = String::new();
    out.push_str(&tag[..attr.val_start - 1]);
    out.push('"');
    out.push_str(&escape_attr(&next));
    out.push('"');
    out.push_str(&tag[attr.val_end + 1..]);
    out
}

/// Rewrite one attribute on start tags whose element local name is `element`.
pub fn map_element_attr(
    xml: &str,
    element: &str,
    attr: &str,
    mut edit: impl FnMut(&str) -> String,
) -> String {
    let mut out = String::with_capacity(xml.len());
    let mut pos = 0usize;
    while let Some(tag) = next_tag(xml, pos) {
        out.push_str(&xml[pos..tag.start]);
        let raw = &xml[tag.start..tag.end];
        if !tag.is_end && tag.local == element {
            out.push_str(&rewrite_attr_local(raw, attr, &mut edit));
        } else {
            out.push_str(raw);
        }
        pos = tag.end;
    }
    out.push_str(&xml[pos..]);
    out
}

pub fn collect_attr_local(xml: &str, local: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while let Some(tag) = next_tag(xml, pos) {
        if !tag.is_end {
            if let Some(value) = attr_local_value(&xml[tag.start..tag.end], local) {
                if !value.is_empty() {
                    out.push(value);
                }
            }
        }
        pos = tag.end;
    }
    out
}

pub fn attr_local_value(tag: &str, local: &str) -> Option<String> {
    attributes(tag)
        .into_iter()
        .find(|attr| attr_local(&attr.key) == local)
        .map(|attr| attr.value)
}

pub fn map_leaf_local(xml: &str, local: &str, mut edit: impl FnMut(&str) -> String) -> String {
    let mut out = String::with_capacity(xml.len());
    let mut pos = 0usize;
    while let Some(tag) = next_tag(xml, pos) {
        out.push_str(&xml[pos..tag.start]);
        if !tag.is_end && !tag.is_self && tag.local == local {
            let close = format!("</{}>", tag.qname);
            if let Some(rel) = xml[tag.end..].find(&close) {
                let inner_end = tag.end + rel;
                let close_end = inner_end + close.len();
                let current = decode_node_text(&xml[tag.end..inner_end]);
                let next = edit(&current);
                if next == current {
                    out.push_str(&xml[tag.start..close_end]);
                } else {
                    out.push_str(&xml[tag.start..tag.end]);
                    out.push_str(&escape_text(&next));
                    out.push_str(&close);
                }
                pos = close_end;
                continue;
            }
        }
        out.push_str(&xml[tag.start..tag.end]);
        pos = tag.end;
    }
    out.push_str(&xml[pos..]);
    out
}

pub fn leaf_texts(xml: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut stack: Vec<(String, String, usize)> = Vec::new();
    let mut pos = 0usize;
    while let Some(tag) = next_tag(xml, pos) {
        if tag.is_end {
            if let Some(index) = stack.iter().rposition(|(name, _, _)| name == &tag.qname) {
                let (_, local, inner) = stack[index].clone();
                stack.truncate(index);
                let raw = &xml[inner..tag.start];
                if !raw.contains('<') && !raw.trim().is_empty() {
                    out.push((local, decode_node_text(raw)));
                }
            }
            pos = tag.end;
            continue;
        }
        if !tag.is_self {
            stack.push((tag.qname.clone(), tag.local.clone(), tag.end));
        }
        pos = tag.end;
    }
    out
}

struct Attr {
    key: String,
    value: String,
    val_start: usize,
    val_end: usize,
}

fn attributes(tag: &str) -> Vec<Attr> {
    let bytes = tag.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() && matches!(bytes[i], b'<' | b'/' | b' ' | b'\t' | b'\n' | b'\r') {
        i += 1;
    }
    while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'>' && bytes[i] != b'/'
    {
        i += 1;
    }
    let mut out = Vec::new();
    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] == b'>' || bytes[i] == b'/' {
            break;
        }
        let key_start = i;
        while i < bytes.len()
            && bytes[i] != b'='
            && !bytes[i].is_ascii_whitespace()
            && bytes[i] != b'>'
        {
            i += 1;
        }
        if !tag.is_char_boundary(key_start) || !tag.is_char_boundary(i) {
            break;
        }
        let key = tag[key_start..i].to_string();
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'=' {
            break;
        }
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let quote = bytes[i];
        if quote != b'"' && quote != b'\'' {
            break;
        }
        i += 1;
        let val_start = i;
        while i < bytes.len() && bytes[i] != quote {
            i += 1;
        }
        if i >= bytes.len() || !tag.is_char_boundary(val_start) || !tag.is_char_boundary(i) {
            break;
        }
        let value = unescape_text(&tag[val_start..i]);
        out.push(Attr {
            key,
            value,
            val_start,
            val_end: i,
        });
        i += 1;
    }
    out
}

fn attr_local(key: &str) -> &str {
    key.rsplit(':').next().unwrap_or(key)
}

pub(crate) fn next_tag(xml: &str, from: usize) -> Option<Tag> {
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
                return Some(parse_tag(start, i, raw));
            }
            i += 1;
        }
        return None;
    }
    None
}

fn parse_tag(start: usize, end: usize, raw: &str) -> Tag {
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
    Tag {
        qname,
        local,
        start,
        end,
        is_end,
        is_self,
    }
}

fn decode_node_text(raw: &str) -> String {
    let trimmed = raw.trim();
    if let Some(inner) = trimmed
        .strip_prefix("<![CDATA[")
        .and_then(|rest| rest.strip_suffix("]]>"))
    {
        return inner.to_string();
    }
    unescape_text(raw)
}

pub fn unescape_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        if let Some(end) = rest.find(';') {
            let entity = &rest[1..end];
            if let Some(ch) = decode_entity(entity) {
                out.push(ch);
                rest = &rest[end + 1..];
                continue;
            }
        }
        out.push('&');
        rest = &rest[1..];
    }
    out.push_str(rest);
    out
}

fn decode_entity(entity: &str) -> Option<char> {
    match entity {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        _ => {
            let num = entity.strip_prefix('#')?;
            let code = if let Some(hex) = num.strip_prefix(['x', 'X']) {
                u32::from_str_radix(hex, 16).ok()?
            } else {
                num.parse().ok()?
            };
            char::from_u32(code).filter(|ch| is_xml_char(*ch))
        }
    }
}

pub fn escape_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ if is_xml_char(ch) => out.push(ch),
            _ => {}
        }
    }
    out
}

fn escape_attr(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '"' => out.push_str("&quot;"),
            _ if is_xml_char(ch) => out.push(ch),
            _ => {}
        }
    }
    out
}

fn is_xml_char(ch: char) -> bool {
    matches!(ch, '\u{9}' | '\u{A}' | '\u{D}' | '\u{20}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'..='\u{10FFFF}')
}

pub fn decode_xml_bytes(bytes: &[u8]) -> Option<String> {
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    String::from_utf8(bytes.to_vec()).ok()
}
