mod html;
pub mod segment;

use std::fs;
use std::io::Read;
use std::path::Path;

use zip::ZipArchive;

pub use segment::{segment_pages, SearchUnit, UNIT_MAX_CHARS, UNIT_MIN_CHARS};

#[derive(Debug, Clone)]
pub struct ExtractedDoc {
    pub title: String,
    pub pages: Vec<String>, // for PDF: one string per page; others: single element
}

pub fn extract_file(path: &Path) -> Result<ExtractedDoc, String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    match ext.as_str() {
        "txt" | "md" | "markdown" | "json" => extract_text(path),
        "html" | "htm" => extract_html(path),
        "pdf" => extract_pdf(path),
        "docx" => extract_docx(path),
        "doc" => extract_doc(path),
        "jtd" => extract_jtd(path),
        "xls" | "xlsx" => extract_spreadsheet(path),
        _ => Err(format!("unsupported extension: {ext}")),
    }
}

pub fn is_supported(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase()
            .as_str(),
        "txt" | "md" | "markdown" | "json" | "html" | "htm" | "pdf" | "docx" | "doc" | "jtd"
            | "xls" | "xlsx"
    )
}

fn extract_text(path: &Path) -> Result<ExtractedDoc, String> {
    let content = fs::read_to_string(path).map_err(|e| e.to_string())?;
    let title = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("untitled")
        .to_string();
    Ok(ExtractedDoc {
        title,
        pages: vec![content],
    })
}

fn extract_html(path: &Path) -> Result<ExtractedDoc, String> {
    let bytes = fs::read(path).map_err(|e| e.to_string())?;
    let decoded = html::decode_html_bytes(&bytes);
    let (doc_title, text) = html::html_to_text(&decoded);
    if !text.chars().any(|c| !c.is_whitespace()) {
        return Err(SKIP_NO_TEXT.into());
    }
    // List title matches PDF/Office: filename with extension.
    // Keep <title> searchable by prefixing the body when it differs.
    let file_name = file_title(path);
    let body = match doc_title {
        Some(page) if !page.is_empty() && page != file_name => format!("{page}\n{text}"),
        _ => text,
    };
    Ok(ExtractedDoc {
        title: file_name,
        pages: vec![body],
    })
}

fn extract_pdf(path: &Path) -> Result<ExtractedDoc, String> {
    let bytes = fs::read(path).map_err(|e| e.to_string())?;
    // pdf-extract panics on some encodings (e.g. StandardEncoding); isolate so indexing continues.
    let extract_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        pdf_extract::extract_text_from_mem(&bytes)
    }));
    let text = match extract_result {
        Ok(Ok(t)) => t,
        Ok(Err(e)) => return Err(format!("pdf extract failed: {e}")),
        Err(_) => {
            return Err(
                "pdf extract panicked (unsupported encoding or corrupt PDF)".into(),
            );
        }
    };

    let title = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("untitled")
        .to_string();
    // pdf-extract may join pages with form feeds
    let pages: Vec<String> = if text.contains('\u{c}') {
        text.split('\u{c}')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    } else if text.chars().any(|c| !c.is_whitespace()) {
        vec![text]
    } else {
        Vec::new()
    };

    if pages.is_empty() || pages.iter().all(|p| !p.chars().any(|c| !c.is_whitespace())) {
        return Err(SKIP_NO_TEXT.into());
    }

    Ok(ExtractedDoc { title, pages })
}

/// Extract errors that should count as skipped (not hard failures).
pub const SKIP_NO_TEXT: &str = "no extractable text (image-only or empty)";

pub fn is_skippable_extract_error(err: &str) -> bool {
    err == SKIP_NO_TEXT || err.starts_with("pdf extract panicked")
}

fn extract_docx(path: &Path) -> Result<ExtractedDoc, String> {
    let file = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut archive = ZipArchive::new(file).map_err(|e| e.to_string())?;
    let mut xml = String::new();
    {
        let mut entry = archive
            .by_name("word/document.xml")
            .map_err(|e| e.to_string())?;
        entry.read_to_string(&mut xml).map_err(|e| e.to_string())?;
    }
    // Preserve `w:p` boundaries as blank-line separated blocks for segmentation.
    let paragraphs = docx_paragraphs(&xml);
    let text = if paragraphs.is_empty() {
        strip_xml_text(&xml)
    } else {
        paragraphs.join("\n\n")
    };
    if !text.chars().any(|c| !c.is_whitespace()) {
        return Err(SKIP_NO_TEXT.into());
    }
    Ok(ExtractedDoc {
        title: file_title(path),
        pages: vec![text],
    })
}

/// Extract text per `w:p` element (empty paragraphs kept as blank separators).
fn docx_paragraphs(xml: &str) -> Vec<String> {
    let mut paras = Vec::new();
    let mut rest = xml;
    while let Some(rel) = rest.find("<w:p") {
        let from = &rest[rel..];
        // Reject lookalikes: w:pPr, w:pStyle, w:pict, …
        let is_paragraph = from.starts_with("<w:p>")
            || from.starts_with("<w:p ")
            || from.starts_with("<w:p\t")
            || from.starts_with("<w:p\n")
            || from.starts_with("<w:p\r")
            || from.starts_with("<w:p/");
        if !is_paragraph {
            rest = &from[1..]; // skip this `<` and keep searching
            continue;
        }
        let Some(gt) = from.find('>') else {
            break;
        };
        // Self-closing empty paragraph → blank separator
        if from[..gt].ends_with('/') {
            paras.push(String::new());
            rest = &from[gt + 1..];
            continue;
        }
        let after_open = &from[gt + 1..];
        let Some(end_rel) = after_open.find("</w:p>") else {
            break;
        };
        let inner = &after_open[..end_rel];
        paras.push(strip_xml_text(inner));
        rest = &after_open[end_rel + "</w:p>".len()..];
    }
    paras
}

fn extract_doc(path: &Path) -> Result<ExtractedDoc, String> {
    let bytes = fs::read(path).map_err(|e| e.to_string())?;
    let text = rwml::extract_text(&bytes).map_err(|e| e.to_string())?;
    Ok(ExtractedDoc {
        title: file_title(path),
        pages: vec![text],
    })
}

fn extract_jtd(path: &Path) -> Result<ExtractedDoc, String> {
    let bytes = fs::read(path).map_err(|e| e.to_string())?;
    let text = rjtd_core::document_text::extract_document_text(&bytes);
    if text.trim().is_empty() {
        return Err("jtd: no extractable text".to_string());
    }
    Ok(ExtractedDoc {
        title: file_title(path),
        pages: vec![text],
    })
}

fn extract_spreadsheet(path: &Path) -> Result<ExtractedDoc, String> {
    use calamine::{open_workbook_auto, Data, Reader};

    let mut workbook = open_workbook_auto(path).map_err(|e| e.to_string())?;
    let sheet_names = workbook.sheet_names().to_vec();
    let mut pages = Vec::new();

    for name in sheet_names {
        let Ok(range) = workbook.worksheet_range(&name) else {
            continue;
        };
        let mut lines = Vec::new();
        lines.push(name.clone());
        for row in range.rows() {
            let cells: Vec<String> = row
                .iter()
                .filter_map(|c| match c {
                    Data::Empty => None,
                    other => {
                        let s = other.to_string();
                        if s.trim().is_empty() {
                            None
                        } else {
                            Some(s)
                        }
                    }
                })
                .collect();
            if !cells.is_empty() {
                lines.push(cells.join("\t"));
            }
        }
        let page = lines.join("\n");
        if page.trim().len() > name.len() {
            pages.push(page);
        }
    }

    if pages.is_empty() {
        return Err("spreadsheet: no extractable text".to_string());
    }
    Ok(ExtractedDoc {
        title: file_title(path),
        pages,
    })
}

fn file_title(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("untitled")
        .to_string()
}

fn strip_xml_text(xml: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    let mut last_was_space = true;
    for ch in xml.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                if !last_was_space {
                    out.push('\n');
                    last_was_space = true;
                }
            }
            _ if !in_tag => {
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
            _ => {}
        }
    }
    out.trim().to_string()
}

#[derive(Debug, Clone)]
pub struct Chunk {
    pub text: String,
    pub page: Option<u32>,
    pub chunk_id: u32,
}

pub fn chunk_pages(pages: &[String], size: usize, overlap: usize) -> Vec<Chunk> {
    let mut chunks = Vec::new();
    let mut chunk_id = 0u32;
    for (page_idx, page) in pages.iter().enumerate() {
        let chars: Vec<char> = page.chars().collect();
        if chars.is_empty() {
            continue;
        }
        let mut start = 0usize;
        while start < chars.len() {
            let end = (start + size).min(chars.len());
            let text: String = chars[start..end].iter().collect();
            if !text.trim().is_empty() {
                chunks.push(Chunk {
                    text,
                    page: Some((page_idx as u32) + 1),
                    chunk_id,
                });
                chunk_id += 1;
            }
            if end >= chars.len() {
                break;
            }
            start = end.saturating_sub(overlap).max(start + 1);
        }
    }
    chunks
}

pub fn content_hash(bytes: &[u8]) -> String {
    let h = xxhash_rust::xxh64::xxh64(bytes, 0);
    format!("{h:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn file_title_keeps_html_extension() {
        assert_eq!(file_title(Path::new("C:\\docs\\report.html")), "report.html");
        assert_eq!(file_title(Path::new("C:\\docs\\index.htm")), "index.htm");
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
        let units = segment_pages(&[joined]);
        assert!(
            units.len() >= 2,
            "expected blank-line units from w:p, got {}: {:?}",
            units.len(),
            units.iter().map(|u| &u.label).collect::<Vec<_>>()
        );
    }
}
