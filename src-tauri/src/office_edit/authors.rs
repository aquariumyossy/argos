//! Word core properties: `dc:creator` (作成者) and `cp:lastModifiedBy` (最終更新者).
//!
//! Body replacements do not rewrite these two fields, even when the find string
//! is the author's name. They change only when the tool is given an explicit
//! set or clear. Other document-property leaves stay in the replacement.

use std::fs;
use std::io::Read;
use std::path::Path;

use serde_json::Value;

use super::xmlscan;
use super::ChangeStats;

/// Shown once on the system prompt, including a prompt the user edited.
pub const LLM_DOC_AUTHOR_SENTINEL: &str =
    "作成者と最終更新者を変えるか、何を書くかが利用者の指示から明らかでないときは";
pub const LLM_DOC_AUTHOR_HINT: &str = "\
作成者と最終更新者を変えるか、何を書くかが利用者の指示から明らかでないときは、edit_office の説明に従ってください。";

const MAX_AUTHOR_CHARS: usize = 200;
const CORE_PART: &str = "docProps/core.xml";
const DC_NS: &str = "http://purl.org/dc/elements/1.1/";
const CP_NS: &str = "http://schemas.openxmlformats.org/package/2006/metadata/core-properties";
const CORE_REL: &str =
    "http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties";
const MAX_CORE_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AuthorField {
    Keep,
    Set(String),
    Clear,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CoreAuthorEdit {
    pub creator: AuthorField,
    pub last_modified_by: AuthorField,
}

impl CoreAuthorEdit {
    pub(crate) fn keep() -> Self {
        Self {
            creator: AuthorField::Keep,
            last_modified_by: AuthorField::Keep,
        }
    }

    pub(crate) fn requested(&self) -> bool {
        !matches!(self.creator, AuthorField::Keep)
            || !matches!(self.last_modified_by, AuthorField::Keep)
    }
}

impl Default for CoreAuthorEdit {
    fn default() -> Self {
        Self::keep()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DocxAuthors {
    pub creator: String,
    pub last_modified_by: String,
}

pub fn docx_author_lines(authors: &DocxAuthors) -> String {
    format!(
        "作成者: {}\n最終更新者: {}",
        display_author(&authors.creator),
        display_author(&authors.last_modified_by)
    )
}

fn display_author(value: &str) -> String {
    let flat = value.replace(['\n', '\r', '\t'], " ");
    let trimmed = flat.trim();
    if trimmed.is_empty() {
        "（なし）".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Read `dc:creator` and `cp:lastModifiedBy`. Missing fields are empty strings.
pub fn read_docx_authors(path: &Path) -> Result<DocxAuthors, String> {
    let file = fs::File::open(path).map_err(|_| "ファイルを開けません。".to_string())?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|_| "Office ファイルとして読めません。".to_string())?;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|_| "Office ファイルとして読めません。".to_string())?;
        if !is_core_part(entry.name()) {
            continue;
        }
        if entry.size() > MAX_CORE_BYTES {
            return Ok(DocxAuthors::default());
        }
        let mut xml = String::new();
        entry
            .read_to_string(&mut xml)
            .map_err(|_| "文書のプロパティを読めません。".to_string())?;
        return Ok(authors_in_xml(&xml));
    }
    Ok(DocxAuthors::default())
}

pub(crate) fn core_authors_from_args(args: &Value) -> Result<CoreAuthorEdit, String> {
    Ok(CoreAuthorEdit {
        creator: author_field(args, "creator", "clear_creator", "作成者")?,
        last_modified_by: author_field(
            args,
            "last_modified_by",
            "clear_last_modified_by",
            "最終更新者",
        )?,
    })
}

fn author_field(
    args: &Value,
    set_key: &str,
    clear_key: &str,
    label: &str,
) -> Result<AuthorField, String> {
    let clear = super::arg_bool(args.get(clear_key));
    let set = match args.get(set_key) {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) => Some(text.clone()),
        Some(_) => return Err(format!("{set_key} は文字列にしてください。")),
    };
    match (set, clear) {
        (Some(text), true) if !text.trim().is_empty() => Err(format!(
            "{label}は {set_key} と {clear_key} を同時に指定できません。"
        )),
        (_, true) => Ok(AuthorField::Clear),
        (Some(text), false) => {
            let text = text.trim();
            if text.is_empty() {
                return Err(format!(
                    "{label}を空にするときは {clear_key} を true にしてください。"
                ));
            }
            if text.chars().any(|ch| ch.is_control()) {
                return Err(format!("{label}に改行や制御文字は使えません。"));
            }
            if text.chars().count() > MAX_AUTHOR_CHARS {
                return Err(format!("{label}が長すぎます。"));
            }
            Ok(AuthorField::Set(text.to_string()))
        }
        (None, false) => Ok(AuthorField::Keep),
    }
}

pub(crate) fn apply(
    entries: &mut Vec<(String, Vec<u8>)>,
    edit: &CoreAuthorEdit,
    stats: &mut ChangeStats,
) -> Result<(), String> {
    if !edit.requested() {
        return Ok(());
    }
    let index = entries.iter().position(|(name, _)| is_core_part(name));
    let mut xml = if let Some(index) = index {
        xmlscan::decode_xml_bytes(&entries[index].1)
            .ok_or_else(|| "文書のプロパティを読めません。".to_string())?
    } else {
        empty_core_xml()
    };
    xml = ensure_namespaces(&xml);
    let mut changed = false;
    let mut notes = Vec::new();
    for (local, label, field) in [
        ("creator", "作成者", &edit.creator),
        ("lastModifiedBy", "最終更新者", &edit.last_modified_by),
    ] {
        if matches!(field, AuthorField::Keep) {
            continue;
        }
        let want = match field {
            AuthorField::Set(value) => value.clone(),
            AuthorField::Clear => String::new(),
            AuthorField::Keep => String::new(),
        };
        let (next, did_change) = upsert_leaf(&xml, local, &want)?;
        xml = next;
        changed |= did_change;
        notes.push(field_note(label, field, did_change));
    }
    if changed {
        let bytes = xml.into_bytes();
        if let Some(index) = index {
            entries[index].1 = bytes;
        } else {
            ensure_content_type(entries)?;
            ensure_package_rels(entries)?;
            entries.push((CORE_PART.to_string(), bytes));
        }
        stats.core_authors = true;
    }
    stats.notes.extend(notes);
    Ok(())
}

fn field_note(label: &str, field: &AuthorField, changed: bool) -> String {
    match (field, changed) {
        (AuthorField::Clear, true) => format!("{label}を空にしました。"),
        (AuthorField::Clear, false) => format!("{label}はすでに空です。"),
        (AuthorField::Set(value), true) => format!("{label}を「{}」にしました。", one_line(value)),
        (AuthorField::Set(value), false) => {
            format!("{label}はすでに「{}」です。", one_line(value))
        }
        (AuthorField::Keep, _) => String::new(),
    }
}

fn one_line(text: &str) -> String {
    text.replace(['\n', '\r', '\t'], " ")
}

fn empty_core_xml() -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<cp:coreProperties xmlns:cp=\"{CP_NS}\" xmlns:dc=\"{DC_NS}\"></cp:coreProperties>"
    )
}

fn authors_in_xml(xml: &str) -> DocxAuthors {
    DocxAuthors {
        creator: first_leaf(xml, "creator").unwrap_or_default(),
        last_modified_by: first_leaf(xml, "lastModifiedBy").unwrap_or_default(),
    }
}

fn first_leaf(xml: &str, local: &str) -> Option<String> {
    let xml = expand_self_closing(xml, local);
    let mut pos = 0usize;
    while let Some(tag) = xmlscan::next_tag(&xml, pos) {
        if !tag.is_end && !tag.is_self && tag.local == local {
            let close = format!("</{}>", tag.qname);
            if let Some(rel) = xml[tag.end..].find(&close) {
                return Some(leaf_value(&xml[tag.end..tag.end + rel]));
            }
        }
        pos = tag.end;
    }
    None
}

fn leaf_value(raw: &str) -> String {
    let trimmed = raw.trim();
    if let Some(inner) = trimmed
        .strip_prefix("<![CDATA[")
        .and_then(|rest| rest.strip_suffix("]]>"))
    {
        return inner.to_string();
    }
    xmlscan::unescape_text(raw)
}

fn upsert_leaf(xml: &str, local: &str, value: &str) -> Result<(String, bool), String> {
    let xml = expand_self_closing(xml, local);
    let mut found = false;
    let mut changed = false;
    let mut out = String::with_capacity(xml.len() + value.len());
    let mut pos = 0usize;
    while let Some(tag) = xmlscan::next_tag(&xml, pos) {
        out.push_str(&xml[pos..tag.start]);
        if !tag.is_end && !tag.is_self && tag.local == local {
            let close = format!("</{}>", tag.qname);
            if let Some(rel) = xml[tag.end..].find(&close) {
                found = true;
                let inner_end = tag.end + rel;
                let close_end = inner_end + close.len();
                let current = leaf_value(&xml[tag.end..inner_end]);
                if current == value {
                    out.push_str(&xml[tag.start..close_end]);
                } else {
                    changed = true;
                    out.push_str(&xml[tag.start..tag.end]);
                    out.push_str(&xmlscan::escape_text(value));
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
    if found {
        return Ok((out, changed));
    }
    if value.is_empty() {
        return Ok((out, false));
    }
    let qname = if local == "creator" {
        "dc:creator"
    } else {
        "cp:lastModifiedBy"
    };
    let element = format!("<{qname}>{}</{qname}>", xmlscan::escape_text(value));
    Ok((insert_before_core_end(&out, &element)?, true))
}

fn expand_self_closing(xml: &str, local: &str) -> String {
    let mut out = String::with_capacity(xml.len());
    let mut pos = 0usize;
    let mut expanded = false;
    while let Some(tag) = xmlscan::next_tag(xml, pos) {
        out.push_str(&xml[pos..tag.start]);
        if !tag.is_end && tag.is_self && tag.local == local {
            expanded = true;
            let raw = &xml[tag.start..tag.end];
            let open = raw
                .trim_end_matches('>')
                .trim_end()
                .trim_end_matches('/')
                .trim_end();
            out.push_str(open);
            out.push('>');
            out.push_str("</");
            out.push_str(&tag.qname);
            out.push('>');
        } else {
            out.push_str(&xml[tag.start..tag.end]);
        }
        pos = tag.end;
    }
    out.push_str(&xml[pos..]);
    if expanded {
        out
    } else {
        xml.to_string()
    }
}

fn insert_before_core_end(xml: &str, element: &str) -> Result<String, String> {
    let mut at = None;
    let mut pos = 0usize;
    while let Some(tag) = xmlscan::next_tag(xml, pos) {
        if tag.is_end && tag.local == "coreProperties" {
            at = Some(tag.start);
        }
        pos = tag.end;
    }
    let Some(at) = at else {
        return Err("文書のプロパティを更新できません。".into());
    };
    let mut out = String::with_capacity(xml.len() + element.len());
    out.push_str(&xml[..at]);
    out.push_str(element);
    out.push_str(&xml[at..]);
    Ok(out)
}

fn ensure_namespaces(xml: &str) -> String {
    if xml.contains("xmlns:dc=") && xml.contains("xmlns:cp=") {
        return xml.to_string();
    }
    let mut pos = 0usize;
    while let Some(tag) = xmlscan::next_tag(xml, pos) {
        if !tag.is_end && tag.local == "coreProperties" {
            let mut insert = String::new();
            if !xml.contains("xmlns:cp=") {
                insert.push_str(&format!(" xmlns:cp=\"{CP_NS}\""));
            }
            if !xml.contains("xmlns:dc=") {
                insert.push_str(&format!(" xmlns:dc=\"{DC_NS}\""));
            }
            let raw = &xml[tag.start..tag.end];
            let rewritten = if let Some(head) = raw.strip_suffix("/>") {
                format!("{head}{insert}/>")
            } else if let Some(head) = raw.strip_suffix('>') {
                format!("{head}{insert}>")
            } else {
                raw.to_string()
            };
            let mut out = String::with_capacity(xml.len() + insert.len());
            out.push_str(&xml[..tag.start]);
            out.push_str(&rewritten);
            out.push_str(&xml[tag.end..]);
            return out;
        }
        pos = tag.end;
    }
    xml.to_string()
}

fn ensure_content_type(entries: &mut [(String, Vec<u8>)]) -> Result<(), String> {
    let Some(index) = entries.iter().position(|(name, _)| {
        name.replace('\\', "/")
            .eq_ignore_ascii_case("[Content_Types].xml")
    }) else {
        return Ok(());
    };
    let xml = xmlscan::decode_xml_bytes(&entries[index].1)
        .ok_or_else(|| "文書のプロパティを追加できません。".to_string())?;
    if xml.to_ascii_lowercase().contains("docprops/core.xml") {
        return Ok(());
    }
    let Some(at) = xml.rfind("</Types>") else {
        return Err("文書のプロパティを追加できません。".into());
    };
    let override_tag = "<Override PartName=\"/docProps/core.xml\" ContentType=\"application/vnd.openxmlformats-package.core-properties+xml\"/>";
    let mut next = String::with_capacity(xml.len() + override_tag.len());
    next.push_str(&xml[..at]);
    next.push_str(override_tag);
    next.push_str(&xml[at..]);
    entries[index].1 = next.into_bytes();
    Ok(())
}

fn ensure_package_rels(entries: &mut [(String, Vec<u8>)]) -> Result<(), String> {
    let Some(index) = entries
        .iter()
        .position(|(name, _)| name.replace('\\', "/").eq_ignore_ascii_case("_rels/.rels"))
    else {
        return Ok(());
    };
    let xml = xmlscan::decode_xml_bytes(&entries[index].1)
        .ok_or_else(|| "文書のプロパティを追加できません。".to_string())?;
    if xml
        .to_ascii_lowercase()
        .contains("metadata/core-properties")
    {
        return Ok(());
    }
    let Some(at) = xml.rfind("</Relationships>") else {
        return Err("文書のプロパティを追加できません。".into());
    };
    let id = fresh_rel_id(&xml);
    let element =
        format!("<Relationship Id=\"{id}\" Type=\"{CORE_REL}\" Target=\"docProps/core.xml\"/>");
    let mut next = String::with_capacity(xml.len() + element.len());
    next.push_str(&xml[..at]);
    next.push_str(&element);
    next.push_str(&xml[at..]);
    entries[index].1 = next.into_bytes();
    Ok(())
}

fn fresh_rel_id(xml: &str) -> String {
    for n in 1..200 {
        let id = format!("rId{n}");
        if !xml.contains(&format!("Id=\"{id}\"")) && !xml.contains(&format!("Id='{id}'")) {
            return id;
        }
    }
    "rIdCoreProps".to_string()
}

fn is_core_part(name: &str) -> bool {
    name.replace('\\', "/")
        .trim_start_matches('/')
        .eq_ignore_ascii_case(CORE_PART)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_creator_and_last_modified_by_and_treats_a_missing_field_as_blank() {
        let xml = r#"<?xml version="1.0"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/">
  <dc:creator>山田 太郎</dc:creator>
  <cp:lastModifiedBy></cp:lastModifiedBy>
</cp:coreProperties>"#;
        let authors = authors_in_xml(xml);
        assert_eq!(authors.creator, "山田 太郎");
        assert_eq!(authors.last_modified_by, "");
        assert_eq!(
            docx_author_lines(&authors),
            "作成者: 山田 太郎\n最終更新者: （なし）"
        );
    }

    #[test]
    fn reads_a_self_closing_creator_as_blank() {
        let xml = r#"<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:creator/><cp:lastModifiedBy>佐藤</cp:lastModifiedBy></cp:coreProperties>"#;
        let authors = authors_in_xml(xml);
        assert_eq!(authors.creator, "");
        assert_eq!(authors.last_modified_by, "佐藤");
    }
}
