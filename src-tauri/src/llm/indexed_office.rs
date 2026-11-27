//! Copy an indexed `.docx` / `.xlsx` into the output folder so `edit_office` can edit it.
//!
//! The path is the one search results already show. The indexed file is opened read-only.
//! A path that is not in the index is never opened.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::db::{Db, FolderRow, IndexedFileRow};
use crate::llm::create_office::{
    file_stem, notice_body, prepare_output_dir, unique_path, OfficeToolResult, SavedOfficeFile,
};
use crate::pathutil;

pub const TOOL_COPY_INDEXED_OFFICE: &str = "copy_indexed_office";

/// Opening of the one system sentence. Used so a saved prompt does not gain a second copy.
pub const LLM_INDEXED_COPY_SENTINEL: &str = "過去の文書をひな型に再利用するときは";

const MAX_FILE_BYTES: u64 = 40 * 1024 * 1024;
const ACTIVE_STATUS: &[&str] = &["ok", "skipped"];

/// One sentence for the system prompt while index search is on.
pub fn format_system_line() -> String {
    format!(
        "\n{LLM_INDEXED_COPY_SENTINEL}、先に {TOOL_COPY_INDEXED_OFFICE} で原本を取り、その作業コピーを edit_office で編集し、置き換える値は search_index で集めてください。"
    )
}

pub fn tool_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": TOOL_COPY_INDEXED_OFFICE,
            "description": "索引にある .docx / .xlsx の原本を、出力フォルダへ作業コピーとして複製する。原本は変更しない。path には search_index の出典に書かれたパス、または paragraph_id を渡す。返ったファイル名を edit_office の filename に渡して編集する。メール、ウェブ、このPCに無いファイルは複製しない。",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "search_index の出典に書かれたファイルのパス、または paragraph_id。"
                    }
                },
                "required": ["path"]
            }
        }
    })
}

/// Chat card for the working copy. Same marker as the other saved office files.
pub fn notice_for_tool(tool_name: &str, saved: Option<&SavedOfficeFile>) -> Option<String> {
    if tool_name != TOOL_COPY_INDEXED_OFFICE {
        return None;
    }
    let file = saved?;
    if file.name.trim().is_empty() {
        return None;
    }
    Some(notice_body(file))
}

pub fn exec(db: &Db, data_dir: &Path, args: &Value) -> OfficeToolResult {
    match copy_indexed_office(db, data_dir, args) {
        Ok(file) => {
            let message = tool_message(&file);
            OfficeToolResult {
                message,
                saved: Some(file),
            }
        }
        Err(message) => OfficeToolResult {
            message,
            saved: None,
        },
    }
}

fn tool_message(file: &SavedOfficeFile) -> String {
    format!(
        "原本は変更していません。作業コピー『{}』を出力フォルダに保存しました。edit_office の filename にこのファイル名だけを渡して編集してください。",
        one_line(&file.name)
    )
}

fn copy_indexed_office(db: &Db, data_dir: &Path, args: &Value) -> Result<SavedOfficeFile, String> {
    let requested = argument_path(args)?;
    let row = indexed_row(db, &requested)?;
    if path_is_excluded(db, &row.path)? {
        return Err("除外されたパスは複製できません。".into());
    }
    let folder = db
        .get_folder(row.folder_id)
        .map_err(|err| format!("索引を読めません（{err}）。"))?
        .ok_or_else(|| "索引フォルダが見つかりません。".to_string())?;
    if !folder.enabled {
        return Err("そのフォルダは検索対象外です。".into());
    }
    let src = open_original(&row.path, &folder)?;
    if path_is_excluded(db, &src.to_string_lossy())? {
        return Err("除外されたパスは複製できません。".into());
    }
    let original_name = src
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "ファイル名を読めません。".to_string())?;
    let ext = office_extension(original_name)?;
    let meta = fs::metadata(&src).map_err(|_| missing_original())?;
    if !meta.is_file() {
        return Err("ファイルが見つかりません。".into());
    }
    if meta.len() == 0 {
        return Err("ファイルが空です。".into());
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err("ファイルが大きすぎます。".into());
    }
    let before = fs::read(&src).map_err(|_| missing_original())?;
    let saved = write_working_copy(data_dir, &src, original_name, ext, &before)?;
    let after = fs::read(&src).map_err(|_| missing_original())?;
    if after != before {
        let _ = fs::remove_file(&saved.path);
        return Err("原本が変わったため、作業コピーを破棄しました。".into());
    }
    Ok(saved)
}

fn missing_original() -> String {
    "原本が見つかりません。ファイルが移動または削除された可能性があります。".into()
}

fn argument_path(args: &Value) -> Result<String, String> {
    let path = field(args, "path");
    let paragraph = field(args, "paragraph_id");
    let id = field(args, "id");
    let filled: Vec<&str> = [path.as_str(), paragraph.as_str(), id.as_str()]
        .into_iter()
        .filter(|text| !text.is_empty())
        .collect();
    if filled.is_empty() {
        return Err(need_path());
    }
    let mut resolved = Vec::new();
    for text in &filled {
        resolved.push(file_path_of_hit(text)?);
    }
    let first = &resolved[0];
    if resolved.iter().any(|path| path != first) {
        return Err(
            "path と paragraph_id が別のファイルを指しています。出典のパスだけを指定してください。"
                .into(),
        );
    }
    Ok(first.clone())
}

fn need_path() -> String {
    "パスを指定してください。search_index の出典に書かれたパスを指定してください。".into()
}

fn field(args: &Value, key: &str) -> String {
    args.get(key)
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .trim()
        .to_string()
}

fn file_path_of_hit(raw: &str) -> Result<String, String> {
    reject_virtual(raw)?;
    let simplified = pathutil::simplify_windows_path(raw.trim());
    let path = strip_paragraph_unit(&simplified);
    if path.is_empty() || path == "\\" {
        return Err(need_path());
    }
    Ok(path)
}

fn reject_virtual(raw: &str) -> Result<(), String> {
    let trimmed = raw.trim();
    if trimmed.is_empty()
        || trimmed.contains('\0')
        || trimmed.contains('\n')
        || trimmed.contains('\r')
    {
        return Err(need_path());
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") || trimmed.contains("://") {
        return Err("ウェブのURLは複製できません。索引のファイルのパスを指定してください。".into());
    }
    if crate::mail::is_outlook_path(trimmed)
        || lower.starts_with("outlookcal:")
        || lower.starts_with("note:")
        || lower.starts_with("mailfolder:")
    {
        return Err(
            "メールやノートは複製できません。索引の .docx / .xlsx のパスを指定してください。"
                .into(),
        );
    }
    Ok(())
}

/// `path#unit` is the paragraph id search results show. A `#` that is part of the
/// path stays, matching `neighbor_ids`: only a trailing integer is the unit.
fn strip_paragraph_unit(path: &str) -> String {
    let Some(index) = path.rfind('#') else {
        return path.to_string();
    };
    let unit = &path[index + 1..];
    if unit.is_empty() || !unit.chars().all(|ch| ch.is_ascii_digit()) {
        return path.to_string();
    }
    path[..index].trim_end_matches('\\').to_string()
}

fn indexed_row(db: &Db, path: &str) -> Result<IndexedFileRow, String> {
    let row = db
        .find_indexed_file(path)
        .map_err(|err| format!("索引を読めません（{err}）。"))?
        .ok_or_else(not_indexed)?;
    if !ACTIVE_STATUS
        .iter()
        .any(|status| row.status.eq_ignore_ascii_case(status))
    {
        return Err(not_indexed());
    }
    Ok(row)
}

fn not_indexed() -> String {
    "索引にそのファイルはありません。search_index の出典に書かれたパスを指定してください。".into()
}

fn path_is_excluded(db: &Db, path: &str) -> Result<bool, String> {
    let rows = db
        .list_exclude_paths()
        .map_err(|err| format!("索引を読めません（{err}）。"))?;
    Ok(rows
        .iter()
        .any(|row| pathutil::path_starts_with(path, &row.path)))
}

fn open_original(stored: &str, folder: &FolderRow) -> Result<PathBuf, String> {
    let parts = relative_parts(stored, folder)?;
    let local = join_parts(Path::new(&folder.path), &parts);
    if local.is_file() {
        return contained_file(Path::new(&folder.path), &local);
    }
    let public = pathutil::effective_public_root(&folder.path, &folder.public_path);
    let local_root = pathutil::simplify_windows_path(&folder.path);
    if !public.eq_ignore_ascii_case(&local_root) {
        let via_public = join_parts(Path::new(&public), &parts);
        if via_public.is_file() {
            return contained_file(Path::new(&public), &via_public);
        }
    }
    Err(missing_original())
}

fn relative_parts(stored: &str, folder: &FolderRow) -> Result<Vec<String>, String> {
    let stored = pathutil::simplify_windows_path(stored);
    let public = pathutil::effective_public_root(&folder.path, &folder.public_path);
    let local = pathutil::simplify_windows_path(&folder.path);
    let root = if pathutil::path_starts_with(&stored, &public) {
        public
    } else if pathutil::path_starts_with(&stored, &local) {
        local
    } else {
        return Err("索引フォルダの外は読めません。".into());
    };
    let rest = stored[root.len()..].trim_start_matches('\\');
    if rest.is_empty() {
        return Err("ファイルが見つかりません。".into());
    }
    let mut parts = Vec::new();
    for part in rest.split('\\') {
        if part.is_empty()
            || part == "."
            || part == ".."
            || part.contains('/')
            || part.contains(':')
            || part.contains('\0')
        {
            return Err("索引フォルダの外は読めません。".into());
        }
        parts.push(part.to_string());
    }
    Ok(parts)
}

fn join_parts(root: &Path, parts: &[String]) -> PathBuf {
    let mut path = root.to_path_buf();
    for part in parts {
        path.push(part);
    }
    path
}

fn contained_file(root: &Path, file: &Path) -> Result<PathBuf, String> {
    let canon_root =
        fs::canonicalize(root).map_err(|_| "索引フォルダが見つかりません。".to_string())?;
    if !canon_root.is_dir() {
        return Err("索引フォルダが見つかりません。".into());
    }
    let canon_file = fs::canonicalize(file).map_err(|_| missing_original())?;
    if !canon_file.is_file() {
        return Err("ファイルが見つかりません。".into());
    }
    if !canon_file.starts_with(&canon_root) {
        return Err("索引フォルダの外は読めません。".into());
    }
    Ok(canon_file)
}

fn office_extension(name: &str) -> Result<&'static str, String> {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".docx") {
        return Ok("docx");
    }
    if lower.ends_with(".xlsx") {
        return Ok("xlsx");
    }
    if lower.ends_with(".doc") || lower.ends_with(".xls") {
        return Err(
            "旧形式の .doc / .xls は編集できません。.docx / .xlsx を指定してください。".into(),
        );
    }
    Err("編集できるのは .docx と .xlsx だけです。".into())
}

fn write_working_copy(
    data_dir: &Path,
    src: &Path,
    original_name: &str,
    ext: &str,
    before: &[u8],
) -> Result<SavedOfficeFile, String> {
    let dir = prepare_output_dir(data_dir)?;
    let stem = file_stem(&json!({ "filename": original_name }), "文書")?;
    let dest = unique_path(&dir, &stem, ext);
    if dest.parent() != Some(dir.as_path()) || same_path(src, &dest) {
        return Err("作業コピーを保存できません。".into());
    }
    let tmp = dir.join(format!(".argos-{}.tmp", uuid::Uuid::new_v4()));
    if let Err(err) = copy_readonly(src, &tmp) {
        let _ = fs::remove_file(&tmp);
        return Err(err);
    }
    if let Err(err) = fs::rename(&tmp, &dest) {
        let _ = fs::remove_file(&tmp);
        return Err(format!("作業コピーを保存できません（{err}）。"));
    }
    let copied = fs::read(&dest).map_err(|err| {
        let _ = fs::remove_file(&dest);
        format!("作業コピーを保存できません（{err}）。")
    })?;
    if copied != before {
        let _ = fs::remove_file(&dest);
        return Err("作業コピーを保存できません。".into());
    }
    let name = dest
        .file_name()
        .and_then(|item| item.to_str())
        .ok_or_else(|| "ファイル名を作れませんでした。".to_string())?;
    Ok(SavedOfficeFile {
        kind: ext.to_string(),
        name: name.to_string(),
        path: dest.to_string_lossy().to_string(),
    })
}

/// `File::open` is read-only. The indexed file is never opened for writing.
fn copy_readonly(src: &Path, dest: &Path) -> Result<(), String> {
    let mut input = fs::File::open(src).map_err(|_| missing_original())?;
    let mut output =
        fs::File::create(dest).map_err(|err| format!("作業コピーを保存できません（{err}）。"))?;
    std::io::copy(&mut input, &mut output)
        .map_err(|err| format!("作業コピーを保存できません（{err}）。"))?;
    output
        .flush()
        .map_err(|err| format!("作業コピーを保存できません（{err}）。"))?;
    Ok(())
}

fn same_path(left: &Path, right: &Path) -> bool {
    pathutil::normalize_for_compare(&left.to_string_lossy())
        .eq_ignore_ascii_case(&pathutil::normalize_for_compare(&right.to_string_lossy()))
}

fn one_line(text: &str) -> String {
    text.replace(['\n', '\r', '\t'], " ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;
    use zip::ZipWriter;

    struct Tmp(PathBuf);

    impl Tmp {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("argos-copy-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn write_docx(path: &Path, text: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let xml = format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>{text}</w:t></w:r></w:p></w:body></w:document>"#
        );
        let file = fs::File::create(path).unwrap();
        let mut zip = ZipWriter::new(file);
        let opts = SimpleFileOptions::default();
        zip.start_file("word/document.xml", opts).unwrap();
        zip.write_all(xml.as_bytes()).unwrap();
        zip.finish().unwrap();
    }

    fn read_entry(path: &Path, name: &str) -> String {
        let file = fs::File::open(path).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        let mut entry = archive.by_name(name).unwrap();
        let mut xml = String::new();
        std::io::Read::read_to_string(&mut entry, &mut xml).unwrap();
        xml
    }

    fn index_file(db: &Db, folder: &FolderRow, file: &Path) -> String {
        let stored =
            pathutil::to_indexed_path(&file.to_string_lossy(), &folder.path, &folder.public_path);
        let meta = fs::metadata(file).unwrap();
        db.upsert_file(folder.id, &stored, "docx", meta.len() as i64, 0, "test")
            .unwrap();
        stored
    }

    fn output_docx_names(data_dir: &Path) -> Vec<String> {
        let dir = data_dir.join(crate::llm::create_office::OUTPUT_DIR_NAME);
        if !dir.exists() {
            return Vec::new();
        }
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| name.to_ascii_lowercase().ends_with(".docx"))
            .collect();
        names.sort();
        names
    }

    #[test]
    fn system_hint_is_one_sentence() {
        let line = format_system_line();
        assert_eq!(line.matches('。').count(), 1, "{line}");
        assert!(line.contains(LLM_INDEXED_COPY_SENTINEL), "{line}");
        assert!(line.contains(TOOL_COPY_INDEXED_OFFICE), "{line}");
        assert!(line.contains("edit_office"), "{line}");
        assert!(line.contains("search_index"), "{line}");
        assert!(line.contains("原本"), "{line}");
    }

    #[test]
    fn copy_leaves_the_original_and_edit_office_edits_the_working_copy() {
        let tmp = Tmp::new();
        let db = Db::open(&tmp.0.join("argos.db")).unwrap();
        let cases = tmp.0.join("cases");
        fs::create_dir_all(&cases).unwrap();
        let src = cases.join("カバーレター.docx");
        write_docx(&src, "山田太郎");
        let original = fs::read(&src).unwrap();
        let folder = db
            .add_folder(&cases.to_string_lossy(), r"\\server\share\cases")
            .unwrap();
        let stored = index_file(&db, &folder, &src);
        assert!(
            stored.contains("server"),
            "search results show the public path: {stored}"
        );
        let data_dir = tmp.0.join("data");
        fs::create_dir_all(&data_dir).unwrap();

        let copied = exec(
            &db,
            &data_dir,
            &json!({ "paragraph_id": format!("{stored}#4") }),
        );
        assert!(copied.saved.is_some(), "{}", copied.message);
        let saved = copied.saved.unwrap();
        assert_eq!(saved.name, "カバーレター.docx");
        let output = fs::canonicalize(data_dir.join("output")).unwrap();
        assert_eq!(
            fs::canonicalize(Path::new(&saved.path).parent().unwrap()).unwrap(),
            output
        );
        assert_eq!(fs::read(&src).unwrap(), original);
        assert_eq!(fs::read(&saved.path).unwrap(), original);
        assert!(
            copied.message.contains("原本は変更していません"),
            "{}",
            copied.message
        );
        assert!(copied.message.contains(&saved.name), "{}", copied.message);
        assert!(copied.message.contains("edit_office"), "{}", copied.message);
        let notice = notice_for_tool(TOOL_COPY_INDEXED_OFFICE, Some(&saved)).unwrap();
        assert!(
            notice.contains(&format!("[[argos-office]]\tdocx\t{}", saved.name)),
            "{notice}"
        );
        assert!(!notice.contains(&stored), "{notice}");

        let again = exec(&db, &data_dir, &json!({ "path": stored }));
        let second = again.saved.expect(&again.message);
        assert_eq!(second.name, "カバーレター (2).docx");
        assert_eq!(fs::read(&second.path).unwrap(), original);
        assert_eq!(fs::read(&saved.path).unwrap(), original);
        assert_eq!(fs::read(&src).unwrap(), original);

        let thread = db.create_llm_thread("ひな型", true).unwrap();
        let edited = crate::office_edit::edit_in_thread(
            &db,
            &data_dir,
            "",
            &thread.id,
            &json!({
                "filename": saved.name,
                "replacements": [{"find": "山田太郎", "replace": "佐藤花子"}],
                "paragraphs": []
            }),
        )
        .expect("edit_office edits the working copy");
        assert!(edited.filename.contains("編集済み"), "{}", edited.filename);
        assert!(fs::canonicalize(&edited.path).unwrap().starts_with(&output));
        assert!(read_entry(&edited.path, "word/document.xml").contains("佐藤花子"));
        assert!(!read_entry(&edited.path, "word/document.xml").contains("山田太郎"));
        assert_eq!(fs::read(&src).unwrap(), original, "indexed original stays");
        assert_eq!(
            fs::read(&saved.path).unwrap(),
            original,
            "working copy stays; edit_office writes a new file"
        );
    }

    #[test]
    fn missing_or_unknown_hit_is_refused() {
        let tmp = Tmp::new();
        let db = Db::open(&tmp.0.join("argos.db")).unwrap();
        let cases = tmp.0.join("cases");
        fs::create_dir_all(&cases).unwrap();
        let folder = db.add_folder(&cases.to_string_lossy(), "").unwrap();
        let data_dir = tmp.0.join("data");
        fs::create_dir_all(&data_dir).unwrap();

        let secret = tmp.0.join("秘密.docx");
        write_docx(&secret, "秘密");
        let secret_bytes = fs::read(&secret).unwrap();

        let gone = cases.join("消えた.docx");
        write_docx(&gone, "山田太郎");
        let stored_gone = index_file(&db, &folder, &gone);
        fs::remove_file(&gone).unwrap();

        let loose = cases.join("未登録.docx");
        write_docx(&loose, "未登録");
        let loose_bytes = fs::read(&loose).unwrap();

        let pdf = cases.join("memo.pdf");
        fs::write(&pdf, b"%PDF").unwrap();
        let stored_pdf =
            pathutil::to_indexed_path(&pdf.to_string_lossy(), &folder.path, &folder.public_path);
        db.upsert_file(folder.id, &stored_pdf, "pdf", 4, 0, "pdf")
            .unwrap();

        let escaped = format!(
            "{}\\..\\秘密.docx",
            pathutil::simplify_windows_path(&cases.to_string_lossy())
        );
        db.upsert_file(folder.id, &escaped, "docx", 1, 0, "x")
            .unwrap();

        let refused = [
            json!({}),
            json!({ "path": "" }),
            json!({ "path": "https://example.test/a.docx" }),
            json!({ "path": "outlook:store/entry" }),
            json!({ "path": secret.to_string_lossy().to_string() }),
            json!({ "path": loose.to_string_lossy().to_string() }),
            json!({ "path": stored_gone }),
            json!({ "path": stored_pdf }),
            json!({ "path": escaped }),
            json!({ "paragraph_id": format!("{stored_gone}#2") }),
        ];
        for args in &refused {
            let result = exec(&db, &data_dir, args);
            assert!(result.saved.is_none(), "{args} -> {}", result.message);
            assert!(!result.message.is_empty(), "{args}");
        }
        assert!(output_docx_names(&data_dir).is_empty());
        assert_eq!(fs::read(&secret).unwrap(), secret_bytes);
        assert_eq!(fs::read(&loose).unwrap(), loose_bytes);
        assert!(!gone.exists());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_outside_the_folder_is_not_copied() {
        let tmp = Tmp::new();
        let db = Db::open(&tmp.0.join("argos.db")).unwrap();
        let cases = tmp.0.join("cases");
        fs::create_dir_all(&cases).unwrap();
        let folder = db.add_folder(&cases.to_string_lossy(), "").unwrap();
        let secret = tmp.0.join("秘密.docx");
        write_docx(&secret, "秘密");
        let secret_bytes = fs::read(&secret).unwrap();
        let link = cases.join("link.docx");
        std::os::unix::fs::symlink(&secret, &link).unwrap();
        let stored = index_file(&db, &folder, &link);
        let data_dir = tmp.0.join("data");
        fs::create_dir_all(&data_dir).unwrap();

        let result = exec(&db, &data_dir, &json!({ "path": stored }));
        assert!(result.saved.is_none(), "{}", result.message);
        assert!(result.message.contains("外"), "{}", result.message);
        assert!(output_docx_names(&data_dir).is_empty());
        assert_eq!(fs::read(&secret).unwrap(), secret_bytes);
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
    }
}
