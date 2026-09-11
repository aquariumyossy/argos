mod docx;
mod html;
pub mod segment;

use std::fs;
use std::path::Path;

use docx::extract_docx;

pub use html::{decode_html_bytes, decode_html_bytes_with_charset, html_to_text};
pub use segment::{
    segment_mail_body, segment_pages, SearchUnit, MAIL_UNIT_MAX_CHARS, UNIT_MAX_CHARS,
    UNIT_MIN_CHARS,
};

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
    let pages = extract_pdf_pages_from_bytes(&bytes)?;
    let title = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("untitled")
        .to_string();
    Ok(ExtractedDoc { title, pages })
}

/// PDF text from in-memory bytes (chat URL fetch). Panics in pdf-extract are isolated.
pub fn extract_pdf_pages_from_bytes(bytes: &[u8]) -> Result<Vec<String>, String> {
    let extract_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        pdf_extract::extract_text_from_mem(bytes)
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
    Ok(pages)
}

/// Extract errors that should count as skipped (not hard failures).
pub const SKIP_NO_TEXT: &str = "no extractable text (image-only or empty)";

pub fn is_skippable_extract_error(err: &str) -> bool {
    err == SKIP_NO_TEXT || err.starts_with("pdf extract panicked")
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

pub(super) fn file_title(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("untitled")
        .to_string()
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
}
