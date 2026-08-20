//! OS file attachments for chat: classify, copy images, join extracted text.

use std::fs;
use std::path::{Path, PathBuf};

use crate::extractor;
use crate::llm::grain;
use crate::pathutil;

pub const IMAGE_MAX_BYTES: u64 = 8 * 1024 * 1024;
pub const CHAT_FILES_DIR: &str = "chat-files";

const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachKind {
    Text,
    Image,
}

pub fn file_ext(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase()
}

pub fn classify_path(path: &Path) -> Result<AttachKind, String> {
    if path.is_dir() {
        return Err("フォルダは添付できません。ファイルを選んでください。".into());
    }
    let ext = file_ext(path);
    if IMAGE_EXTS.contains(&ext.as_str()) {
        ensure_regular_file(path)?;
        return Ok(AttachKind::Image);
    }
    if extractor::is_supported(path) {
        ensure_regular_file(path)?;
        return Ok(AttachKind::Text);
    }
    if ext.is_empty() {
        return Err("対応していない形式です。".into());
    }
    Err(format!("対応していない形式です（.{ext}）。"))
}

fn ensure_regular_file(path: &Path) -> Result<(), String> {
    if path.is_dir() {
        return Err("フォルダは添付できません。ファイルを選んでください。".into());
    }
    if !path.is_file() {
        return Err("ファイルが見つかりません。".into());
    }
    Ok(())
}

pub fn mime_for_ext(ext: &str) -> &'static str {
    match ext {
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => "image/jpeg",
    }
}

pub fn extract_text_body(path: &Path) -> Result<(String, String), String> {
    match extract_attach_doc(path)? {
        AttachDoc::Text { title, body } => Ok((title, body)),
        AttachDoc::EmptyPdf => Err(map_extract_err(extractor::SKIP_NO_TEXT.into())),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachDoc {
    Text { title: String, body: String },
    EmptyPdf,
}

pub fn extract_attach_doc(path: &Path) -> Result<AttachDoc, String> {
    if file_ext(path) != "pdf" {
        let (title, body) = extract_non_pdf_body(path)?;
        return Ok(AttachDoc::Text { title, body });
    }
    pdf_extract_to_attach(extractor::extract_file(path), &file_title(path))
}

fn extract_non_pdf_body(path: &Path) -> Result<(String, String), String> {
    let doc = extractor::extract_file(path).map_err(map_extract_err)?;
    let body = doc
        .pages
        .iter()
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    if body.trim().is_empty() {
        return Err("本文を抽出できませんでした。".into());
    }
    grain::check_file_body_size(&body)?;
    let title = if doc.title.trim().is_empty() {
        file_title(path)
    } else {
        doc.title
    };
    Ok((title, body))
}

pub fn pdf_extract_to_attach(
    result: Result<extractor::ExtractedDoc, String>,
    file_title: &str,
) -> Result<AttachDoc, String> {
    match result {
        Ok(doc) => {
            let body = doc
                .pages
                .iter()
                .map(|p| p.trim())
                .filter(|p| !p.is_empty())
                .collect::<Vec<_>>()
                .join("\n\n");
            if body.trim().is_empty() {
                return Ok(AttachDoc::EmptyPdf);
            }
            grain::check_file_body_size(&body)?;
            let title = if doc.title.trim().is_empty() {
                file_title.to_string()
            } else {
                doc.title
            };
            Ok(AttachDoc::Text { title, body })
        }
        Err(err) if extractor::is_skippable_extract_error(&err) => Ok(AttachDoc::EmptyPdf),
        Err(err) => Err(map_extract_err(err)),
    }
}

fn map_extract_err(err: String) -> String {
    if extractor::is_skippable_extract_error(&err) || err.contains("no extractable text") {
        "テキストを抽出できませんでした（画像のみの PDF など）。".into()
    } else {
        format!("読み込みに失敗しました（{err}）。")
    }
}

pub fn file_title(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("添付ファイル")
        .to_string()
}

pub fn stored_relpath(thread_id: &str, source_id: &str, ext: &str) -> String {
    format!("{CHAT_FILES_DIR}/{thread_id}/{source_id}.{ext}")
}

pub fn resolve_stored(data_dir: &Path, rel: &str) -> Result<PathBuf, String> {
    let rel = rel.replace('\\', "/");
    let rel = rel.trim().trim_start_matches('/');
    if rel.is_empty() {
        return Err("保存パスがありません。".into());
    }
    let mut out = data_dir.to_path_buf();
    let mut parts = rel.split('/');
    if parts.next() != Some(CHAT_FILES_DIR) {
        return Err("保存パスが不正です。".into());
    }
    out.push(CHAT_FILES_DIR);
    for part in parts {
        if part.is_empty() || part == "." || part == ".." {
            return Err("保存パスが不正です。".into());
        }
        out.push(part);
    }
    Ok(out)
}

pub fn copy_into_store(data_dir: &Path, src: &Path, rel: &str) -> Result<PathBuf, String> {
    let dest = resolve_stored(data_dir, rel)?;
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    fs::copy(src, &dest).map_err(|e| format!("ファイルを保存できませんでした（{e}）。"))?;
    Ok(dest)
}

pub fn write_bytes_into_store(data_dir: &Path, rel: &str, bytes: &[u8]) -> Result<PathBuf, String> {
    if bytes.is_empty() {
        return Err("画像ファイルが空です。".into());
    }
    if bytes.len() as u64 > IMAGE_MAX_BYTES {
        return Err(format!(
            "画像が大きすぎます（{} MBまで）。",
            IMAGE_MAX_BYTES / (1024 * 1024)
        ));
    }
    let dest = resolve_stored(data_dir, rel)?;
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    fs::write(&dest, bytes).map_err(|e| format!("ファイルを保存できませんでした（{e}）。"))?;
    Ok(dest)
}

pub fn remove_stored(data_dir: &Path, rel: &str) {
    let rel = rel.trim();
    if rel.is_empty() {
        return;
    }
    if let Ok(path) = resolve_stored(data_dir, rel) {
        let _ = fs::remove_file(path);
    }
}

pub fn remove_thread_store(data_dir: &Path, thread_id: &str) {
    let id = thread_id.trim();
    if id.is_empty() || id.contains("..") || id.contains('/') || id.contains('\\') {
        return;
    }
    let dir = data_dir.join(CHAT_FILES_DIR).join(id);
    let _ = fs::remove_dir_all(dir);
}

pub fn check_image_size(path: &Path) -> Result<u64, String> {
    let meta = fs::metadata(path).map_err(|e| e.to_string())?;
    let n = meta.len();
    if n == 0 {
        return Err("画像ファイルが空です。".into());
    }
    if n > IMAGE_MAX_BYTES {
        return Err(format!(
            "画像が大きすぎます（{} MBまで）。",
            IMAGE_MAX_BYTES / (1024 * 1024)
        ));
    }
    Ok(n)
}

pub fn normalize_os_path(raw: &str) -> String {
    pathutil::simplify_windows_path(raw.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_stored_rejects_dotdot() {
        let root = PathBuf::from("C:\\Argos");
        assert!(resolve_stored(&root, "chat-files/../secret.png").is_err());
        assert!(resolve_stored(&root, "index/x.png").is_err());
        let ok = resolve_stored(&root, "chat-files/tid/sid.png").unwrap();
        assert!(ok.ends_with(Path::new("chat-files").join("tid").join("sid.png")));
    }

    #[test]
    fn classify_rejects_unknown_ext() {
        assert!(classify_path(Path::new("C:\\a.pptx")).is_err());
    }

    fn extracted(pages: &[&str]) -> extractor::ExtractedDoc {
        extractor::ExtractedDoc {
            title: "t.pdf".into(),
            pages: pages.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn empty_pdf_extract_is_empty_variant() {
        assert_eq!(
            pdf_extract_to_attach(Ok(extracted(&[])), "a.pdf").unwrap(),
            AttachDoc::EmptyPdf
        );
        assert_eq!(
            pdf_extract_to_attach(Ok(extracted(&["  ", "\n"])), "a.pdf").unwrap(),
            AttachDoc::EmptyPdf
        );
        assert_eq!(
            pdf_extract_to_attach(Err(extractor::SKIP_NO_TEXT.into()), "a.pdf").unwrap(),
            AttachDoc::EmptyPdf
        );
        assert_eq!(
            pdf_extract_to_attach(
                Err("pdf extract panicked (unsupported encoding or corrupt PDF)".into()),
                "a.pdf"
            )
            .unwrap(),
            AttachDoc::EmptyPdf
        );
    }

    #[test]
    fn text_pdf_extract_keeps_body() {
        match pdf_extract_to_attach(Ok(extracted(&["第一条", "第二条"])), "a.pdf").unwrap() {
            AttachDoc::Text { title, body } => {
                assert_eq!(title, "t.pdf");
                assert_eq!(body, "第一条\n\n第二条");
            }
            AttachDoc::EmptyPdf => panic!("expected text"),
        }
    }

    #[test]
    fn oversized_pdf_text_is_not_empty() {
        let huge = "あ".repeat(crate::llm::grain::FILE_GRAIN_HARD_CAP + 1);
        let err = pdf_extract_to_attach(Ok(extracted(&[huge.as_str()])), "a.pdf").unwrap_err();
        assert!(err.contains("大きすぎます"), "{err}");
    }

    #[test]
    fn hard_pdf_failure_is_not_empty() {
        let err =
            pdf_extract_to_attach(Err("pdf extract failed: boom".into()), "a.pdf").unwrap_err();
        assert!(err.contains("読み込みに失敗しました"), "{err}");
    }
}
