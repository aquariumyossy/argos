//! List and read Word, Excel, and HTML files that already sit in the output folder.
//!
//! The model uses the list (name, kind, modified time) to choose a file. Reading
//! stays inside that folder: bare names only, and both the folder and the file
//! must canonicalize to a path under `data_dir/output`.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::{json, Value};

use crate::llm::create_office::{
    contained_output_dir, is_output_document, name_taken_message, output_dir,
    output_document_names, output_file_path, strip_verbatim_prefix,
};
use crate::office_edit::{FILE_LINE, OFFICE_MARKER, PATH_LINE};

pub const TOOL_OUTPUT_FILE: &str = "output_file";
pub const LLM_OUTPUT_SENTINEL: &str = "出力フォルダのファイル一覧を見るツールがあります";
pub const LLM_OUTPUT_HINT: &str = "\
出力フォルダのファイル一覧を見るツールがあります。output_file で一覧・本文・リネームをします。";

const MAX_LISTED: usize = 200;
const MAX_TEXT_CHARS: usize = 80_000;
const MAX_HTML_BYTES: usize = 512 * 1024;
const MAX_OFFICE_BYTES: u64 = 40 * 1024 * 1024;
const EMPTY_LIST: &str = "出力フォルダにファイルはありません。";
const ASK_LINE: &str = "一つならそのファイル名で action read。複数あるなら、名前・種類・更新日時を挙げて利用者に聞いてください。";
/// Present only when the tool result contains the complete HTML source.
pub const HTML_SOURCE_NOTE: &str =
    "以下は HTML のソースです。edit_html の html には、このソースだけを渡してください。";
/// Returned instead of a source that `fit_tool_text` would cut.
pub const HTML_READ_REFUSAL: &str = "この HTML は文脈に入りきらないため、ソースは返していません。切れた本文を edit_html に渡さないでください。ファイルは保存されていません。";

struct ListedFile {
    name: String,
    kind: &'static str,
    modified: Option<SystemTime>,
}

pub fn tool_schemas() -> Vec<Value> {
    vec![schema()]
}

pub fn exec(db: &crate::db::Db, data_dir: &Path, args: &Value, context_tokens: usize) -> String {
    match action_name(args) {
        "list" => list_for_tool(data_dir, args),
        "read" => read_for_tool(data_dir, args, context_tokens),
        "rename" => rename_for_tool(db, data_dir, args),
        _ => "action は list、read、rename のいずれかです。".into(),
    }
}

pub fn list_for_tool(data_dir: &Path, args: &Value) -> String {
    match list_output_files(data_dir, &name_filter(args)) {
        Ok(text) => text,
        Err(message) => message,
    }
}

pub fn read_for_tool(data_dir: &Path, args: &Value, context_tokens: usize) -> String {
    match read_output_file(data_dir, args, context_tokens) {
        Ok(text) => text,
        Err(message) => message,
    }
}

pub fn rename_for_tool(db: &crate::db::Db, data_dir: &Path, args: &Value) -> String {
    match rename_output_file(db, data_dir, args) {
        Ok(text) => text,
        Err(message) => message,
    }
}

fn action_name(args: &Value) -> &str {
    args.get("action")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .trim()
}

pub fn is_html_source_payload(content: &str) -> bool {
    content.contains(HTML_SOURCE_NOTE)
}

/// Keep a complete HTML source. If `fit_tool_text` would shorten it, return an
/// error that does not contain the source and does not ask for `edit_html`.
pub fn deliver_html_read(content: String, budget_tokens: usize, used_tokens: usize) -> String {
    if !is_html_source_payload(&content) {
        return content;
    }
    let fitted = crate::llm::context::fit_tool_text(content.clone(), budget_tokens, used_tokens);
    if fitted == content {
        content
    } else {
        HTML_READ_REFUSAL.into()
    }
}

fn list_output_files(data_dir: &Path, filter: &str) -> Result<String, String> {
    let dir = output_dir(data_dir);
    if !dir.exists() {
        return Ok(empty_list(filter));
    }
    let canon_dir = match contained_output_dir(data_dir) {
        Ok(path) => path,
        Err(message) if message.contains('外') => return Err(message),
        Err(_) => return Ok(empty_list(filter)),
    };
    let mut rows = Vec::new();
    let entries = fs::read_dir(&dir).map_err(|_| "出力フォルダを読めませんでした。".to_string())?;
    for entry in entries {
        let Ok(entry) = entry else {
            continue;
        };
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let Some(kind) = kind_label(&name) else {
            continue;
        };
        if !name_matches(&name, filter) {
            continue;
        }
        let Ok(path) = output_file_path(data_dir, &name) else {
            continue;
        };
        if !path.starts_with(&canon_dir) {
            continue;
        }
        let Ok(meta) = fs::metadata(&path) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        rows.push(ListedFile {
            name,
            kind,
            modified: meta.modified().ok(),
        });
    }
    if rows.is_empty() {
        return Ok(empty_list(filter));
    }
    rows.sort_by(|left, right| {
        right
            .modified
            .cmp(&left.modified)
            .then_with(|| left.name.cmp(&right.name))
    });
    let extra = rows.len().saturating_sub(MAX_LISTED);
    rows.truncate(MAX_LISTED);
    let today = chrono::Local::now().format("%Y-%m-%d");
    let mut lines = vec![
        format!("今日は {today} です。"),
        "名前\t種類\t更新日時".to_string(),
    ];
    for row in &rows {
        let when = row
            .modified
            .map(format_mtime)
            .unwrap_or_else(|| "不明".to_string());
        lines.push(format!("{}\t{}\t{when}", one_line(&row.name), row.kind));
    }
    if extra > 0 {
        lines.push(format!(
            "ほかに {extra} 件あります。上は新しい {MAX_LISTED} 件です。"
        ));
    }
    lines.push(ASK_LINE.to_string());
    Ok(lines.join("\n"))
}

fn read_output_file(
    data_dir: &Path,
    args: &Value,
    context_tokens: usize,
) -> Result<String, String> {
    let name = requested_name(args)?;
    let path = output_file_path(data_dir, name)?;
    let meta = fs::metadata(&path).map_err(|_| "ファイルが見つかりません。".to_string())?;
    if !meta.is_file() {
        return Err("ファイルが見つかりません。".into());
    }
    let kind = kind_label(name)
        .ok_or_else(|| "Word、Excel、HTML のファイル名を指定してください。".to_string())?;
    let html = is_html_name(name);
    let body = if html {
        read_html(&path, meta.len())?
    } else {
        if meta.len() > MAX_OFFICE_BYTES {
            return Err("ファイルが大きすぎます。".into());
        }
        cap_text(&read_office_text(&path)?)
    };
    let when = meta
        .modified()
        .ok()
        .map(format_mtime)
        .unwrap_or_else(|| "不明".to_string());
    let mut out = format!(
        "ファイル: {}\n種類: {kind}\n更新日時: {when}\n",
        one_line(name)
    );
    if kind == "Word" {
        let lines = match crate::office_edit::read_docx_authors(&path) {
            Ok(authors) => crate::office_edit::docx_author_lines(&authors),
            Err(_) => "作成者: （読めませんでした）\n最終更新者: （読めませんでした）".to_string(),
        };
        out.push_str(&lines);
        out.push('\n');
    }
    out.push('\n');
    if html {
        out.push_str(HTML_SOURCE_NOTE);
        out.push('\n');
    }
    out.push_str(&body);
    if html {
        let allowance = crate::llm::context::tool_result_allowance_tokens(context_tokens);
        return Ok(deliver_html_read(out, allowance, 0));
    }
    Ok(out)
}

fn read_html(path: &Path, len: u64) -> Result<String, String> {
    if len > MAX_HTML_BYTES as u64 {
        return Err("ファイルが大きすぎます。".into());
    }
    let bytes = fs::read(path).map_err(|_| "ファイルを読めませんでした。".to_string())?;
    if bytes.len() > MAX_HTML_BYTES {
        return Err("ファイルが大きすぎます。".into());
    }
    let text = crate::extractor::decode_html_bytes(&bytes);
    if text.contains('\0') || !text.chars().any(|ch| !ch.is_whitespace()) {
        return Err("本文が空です。".into());
    }
    Ok(text)
}

fn read_office_text(path: &Path) -> Result<String, String> {
    let doc = crate::extractor::extract_file(path).map_err(|err| {
        if err.contains("no extractable") || err.contains("spreadsheet: no") {
            "本文が空です。".to_string()
        } else {
            "ファイルを読めませんでした。".to_string()
        }
    })?;
    let text = doc.pages.join("\n\n");
    if !text.chars().any(|ch| !ch.is_whitespace()) {
        return Err("本文が空です。".into());
    }
    Ok(text)
}

fn requested_name(args: &Value) -> Result<&str, String> {
    let name = args
        .get("filename")
        .and_then(|value| value.as_str())
        .or_else(|| args.get("name").and_then(|value| value.as_str()))
        .unwrap_or("")
        .trim()
        .trim_matches(|ch| ch == '"' || ch == '\'');
    if name.is_empty() {
        return Err("ファイル名を指定してください。".into());
    }
    Ok(name)
}

fn kind_label(name: &str) -> Option<&'static str> {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".docx") {
        Some("Word")
    } else if lower.ends_with(".xlsx") {
        Some("Excel")
    } else if is_html_name(name) {
        Some("HTML")
    } else {
        None
    }
}

fn is_html_name(name: &str) -> bool {
    name.to_ascii_lowercase().ends_with(".html")
}

fn name_filter(args: &Value) -> String {
    args.get("name")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .trim()
        .to_string()
}

fn name_matches(name: &str, filter: &str) -> bool {
    if filter.is_empty() {
        return true;
    }
    name.to_lowercase().contains(&filter.to_lowercase())
}

fn empty_list(filter: &str) -> String {
    if filter.is_empty() {
        EMPTY_LIST.into()
    } else {
        format!("「{}」を含むファイルはありません。", one_line(filter))
    }
}

fn format_mtime(time: SystemTime) -> String {
    let dt = chrono::DateTime::<chrono::Local>::from(time);
    dt.format("%Y-%m-%d %H:%M").to_string()
}

fn cap_text(text: &str) -> String {
    if text.chars().count() <= MAX_TEXT_CHARS {
        return text.to_string();
    }
    let mut out: String = text.chars().take(MAX_TEXT_CHARS).collect();
    out.push_str("\n（以降は省略しました。見えていない箇所は編集で指定できません。）");
    out
}

fn one_line(text: &str) -> String {
    text.replace(['\n', '\r', '\t'], " ")
}

fn schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": TOOL_OUTPUT_FILE,
            "description": "出力フォルダの Word・Excel・HTML を list / read / rename する。パスは渡さない。検索フォルダや出力フォルダの外は見ない。list は名前・種類・更新日時（新しい順、最大200件）を返し、本文は返さない。name でファイル名の一部を指定すると、それより古いファイルもその語を含むものだけ返す。一つに決まったら read で中身を読む。候補が複数あるときは、名前・種類・更新日時を挙げて利用者に聞き、答えがあるまで読んだり編集したりしない。read の Word と Excel は本文、HTML はソース。Word には作成者（dc:creator）と最終更新者（cp:lastModifiedBy）を含める。空なら（なし）。HTML が文脈に入りきらないときはソースを返さず、edit_html に渡させない。rename は filename を new_filename に変える。拡張子は変えない。同名があるときは何も書かず既存のファイル名を返す。overwrite が true のときだけ置き換える。overwrite は利用者が同意したときだけ true にする。開かれていて失敗したときはどちらも動かさない。結果にはリネームしたことと旧名と新名を書く。編集は Word と Excel が edit_office、HTML が edit_html。",
            "parameters": {
                "type": "object",
                "properties": {
                    "action": { "type": "string", "description": "list、read、rename のいずれか。" },
                    "name": { "type": "string", "description": "list のとき、ファイル名の一部。空なら新しい順に最大200件。" },
                    "filename": { "type": "string", "description": "read と rename の対象ファイル名。パスは渡さない。" },
                    "new_filename": { "type": "string", "description": "rename の新しいファイル名。拡張子は filename と同じ。" },
                    "overwrite": { "type": "boolean", "description": "rename で同名があるとき、true なら置き換える。省略時は何も書かない。" }
                },
                "required": ["action"]
            }
        }
    })
}

fn rename_output_file(db: &crate::db::Db, data_dir: &Path, args: &Value) -> Result<String, String> {
    let old_name = requested_name(args)?.to_string();
    let new_raw = args
        .get("new_filename")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .trim();
    if new_raw.is_empty() {
        return Err("新しいファイル名を指定してください。".into());
    }
    let new_name = crate::llm::create_office::validate_new_output_name(new_raw)?;
    if extension_of(&old_name) != extension_of(&new_name) {
        return Err("拡張子は変えられません。".into());
    }
    if old_name.eq_ignore_ascii_case(&new_name) {
        return Ok(format!(
            "ファイル名は同じです。リネームはしていません。\n旧ファイル名: {old_name}\n新ファイル名: {new_name}"
        ));
    }
    let src = output_file_path(data_dir, &old_name)?;
    let dir = output_dir(data_dir);
    let dest = dir.join(&new_name);
    if dest.parent() != Some(dir.as_path()) {
        return Err("ファイル名だけを指定してください。".into());
    }
    let overwrite = arg_overwrite(args);
    if dest.exists() && !same_output_file(&src, &dest) {
        if !overwrite {
            return Ok(rename_clash_message(&old_name, &new_name, &dir));
        }
    } else if dest.exists() {
        return Ok(format!(
            "同じファイルです。リネームはしていません。\n旧ファイル名: {old_name}\n新ファイル名: {new_name}"
        ));
    }
    let old_paths = path_forms(&src);
    let backup = move_output_file(&src, &dest, overwrite)?;
    let new_path = display_path(&dest);
    let rewritten = |content: &str| rewrite_output_text(content, &old_name, &new_name, &new_path);
    if let Err(err) =
        db.apply_output_file_rename(&old_name, &new_name, &old_paths, &new_path, &dir, rewritten)
    {
        undo_output_move(&src, &dest, backup.as_deref());
        return Err(format!(
            "リネームの記録を保存できませんでした（{err}）。ファイルは元の名前のままです。"
        ));
    }
    if let Some(path) = backup {
        let _ = fs::remove_file(path);
    }
    Ok(format!(
        "リネームしました。\n旧ファイル名: {old_name}\n新ファイル名: {new_name}"
    ))
}

fn rename_clash_message(old_name: &str, new_name: &str, dir: &Path) -> String {
    let names = output_document_names(dir);
    format!(
        "リネームはしていません。\n旧ファイル名: {old_name}\n新ファイル名: {new_name}\n{}",
        name_taken_message(new_name, &names)
    )
}

fn extension_of(name: &str) -> String {
    name.rsplit('.').next().unwrap_or("").to_ascii_lowercase()
}

fn arg_overwrite(args: &Value) -> bool {
    match args.get("overwrite") {
        Some(Value::Bool(flag)) => *flag,
        Some(Value::Number(number)) => number.as_i64() == Some(1),
        Some(Value::String(text)) => matches!(text.trim(), "1" | "true" | "yes" | "on"),
        _ => false,
    }
}

fn same_output_file(left: &Path, right: &Path) -> bool {
    if let (Ok(left), Ok(right)) = (fs::canonicalize(left), fs::canonicalize(right)) {
        return left == right;
    }
    false
}

fn path_forms(path: &Path) -> Vec<String> {
    let mut out = vec![path.to_string_lossy().into_owned()];
    if let Ok(canon) = fs::canonicalize(path) {
        out.push(canon.to_string_lossy().into_owned());
        out.push(strip_verbatim_prefix(&canon).to_string_lossy().into_owned());
    }
    out.sort();
    out.dedup();
    out
}

fn display_path(path: &Path) -> String {
    match fs::canonicalize(path) {
        Ok(canon) => strip_verbatim_prefix(&canon).to_string_lossy().into_owned(),
        Err(_) => path.to_string_lossy().into_owned(),
    }
}

fn move_output_file(src: &Path, dest: &Path, overwrite: bool) -> Result<Option<PathBuf>, String> {
    if dest.exists() && !same_output_file(src, dest) {
        if !overwrite {
            return Err("ファイルは書いていません。".into());
        }
        let backup = dest
            .parent()
            .unwrap_or(dest)
            .join(format!(".argos-rename-{}.tmp", uuid::Uuid::new_v4()));
        rename_step(dest, &backup, RenameStep::Dest)?;
        if let Err(err) = rename_step(src, dest, RenameStep::Source) {
            let _ = fs::rename(&backup, dest);
            return Err(err);
        }
        return Ok(Some(backup));
    }
    rename_step(src, dest, RenameStep::Source)?;
    Ok(None)
}

fn undo_output_move(src: &Path, dest: &Path, backup: Option<&Path>) {
    if let Some(backup) = backup {
        let _ = fs::rename(dest, src);
        let _ = fs::rename(backup, dest);
    } else {
        let _ = fs::rename(dest, src);
    }
}

#[derive(Clone, Copy)]
enum RenameStep {
    Dest,
    Source,
}

fn rename_step(from: &Path, to: &Path, step: RenameStep) -> Result<(), String> {
    if let Some(code) = forced_rename_code(step) {
        return Err(rename_failure_message(&std::io::Error::from_raw_os_error(
            code,
        )));
    }
    fs::rename(from, to).map_err(|err| rename_failure_message(&err))
}

fn rename_failure_message(err: &std::io::Error) -> String {
    let code = err.raw_os_error().unwrap_or(0);
    eprintln!("argos: output rename failed (os error {code}): {err}");
    if crate::llm::create_office::is_sharing_or_lock_error(err) {
        locked_rename_message()
    } else {
        "リネームできませんでした。どちらのファイルも動かしていません。".into()
    }
}

fn locked_rename_message() -> String {
    "別のアプリ（Word や Excel など）で開かれている可能性があります。閉じてから、もう一度試してください。どちらのファイルも動かしていません。"
        .into()
}

#[cfg(test)]
thread_local! {
    static FAIL_RENAME: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
    static FAIL_RENAME_CODE: std::cell::Cell<i32> = const { std::cell::Cell::new(32) };
}

/// Fail the next output rename with a sharing violation. `1` is the destination, `2` is the source.
#[cfg(test)]
pub(crate) fn fail_next_output_rename(step: u8) {
    fail_next_output_rename_os(step, 32);
}

/// Fail the next output rename with a raw OS code.
#[cfg(test)]
pub(crate) fn fail_next_output_rename_os(step: u8, code: i32) {
    FAIL_RENAME.with(|slot| slot.set(step));
    FAIL_RENAME_CODE.with(|slot| slot.set(code));
}

fn forced_rename_code(step: RenameStep) -> Option<i32> {
    #[cfg(test)]
    {
        let want = match step {
            RenameStep::Dest => 1,
            RenameStep::Source => 2,
        };
        FAIL_RENAME.with(|slot| {
            if slot.get() == want {
                slot.set(0);
                Some(FAIL_RENAME_CODE.with(|code| code.get()))
            } else {
                None
            }
        })
    }
    #[cfg(not(test))]
    {
        let _ = step;
        None
    }
}

/// Rewrite marker, `出力ファイル:` and `保存先:` lines. Other text is left as written.
pub(crate) fn rewrite_output_text(
    content: &str,
    old_name: &str,
    new_name: &str,
    new_path: &str,
) -> String {
    let mut lines = Vec::new();
    for line in content.split('\n') {
        lines.push(rewrite_output_line(line, old_name, new_name, new_path));
    }
    let mut out = lines.join("\n");
    if content.ends_with('\n') && !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

fn rewrite_output_line(line: &str, old_name: &str, new_name: &str, new_path: &str) -> String {
    let marker = format!("{OFFICE_MARKER}\t");
    if let Some(rest) = line.strip_prefix(&marker) {
        let mut parts = rest.split('\t');
        let Some(kind) = parts.next() else {
            return line.to_string();
        };
        let Some(name) = parts.next() else {
            return line.to_string();
        };
        if name.eq_ignore_ascii_case(old_name) && is_output_document(name) {
            return format!("{OFFICE_MARKER}\t{kind}\t{new_name}");
        }
        return line.to_string();
    }
    if let Some(rest) = line.strip_prefix(FILE_LINE) {
        if rest.trim().eq_ignore_ascii_case(old_name) {
            return format!("{FILE_LINE}{new_name}");
        }
    }
    if let Some(rest) = line.strip_prefix(PATH_LINE) {
        let path = rest.trim();
        if path_file_name_is(path, old_name) {
            return format!("{PATH_LINE}{new_path}");
        }
    }
    line.to_string()
}

fn path_file_name_is(path: &str, name: &str) -> bool {
    std::path::Path::new(path)
        .file_name()
        .and_then(|item| item.to_str())
        .is_some_and(|file| file.eq_ignore_ascii_case(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::create_html::write_html;
    use crate::llm::create_office::{output_dir, write_docx, write_xlsx};
    use crate::office_edit::{FILE_LINE, OFFICE_MARKER, PATH_LINE};
    use std::path::PathBuf;
    use std::time::{Duration, UNIX_EPOCH};

    const CONTEXT: usize = crate::db::DEFAULT_LLM_MAX_CONTEXT_TOKENS as usize;

    fn llm_message(id: &str, role: &str, content: &str) -> crate::db::LlmMessageRow {
        crate::db::LlmMessageRow {
            id: id.into(),
            thread_id: "t".into(),
            role: role.into(),
            content: content.into(),
            created_at: 0,
            reasoning: String::new(),
            tool_calls: String::new(),
            tool_call_id: String::new(),
            tool_name: String::new(),
            kind: String::new(),
            original_content: String::new(),
            script_fix: String::new(),
            carries_summary: false,
        }
    }

    struct Tmp(PathBuf);

    impl Tmp {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "argos-output-files-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn set_mtime(path: &Path, secs: u64) {
        let file = fs::OpenOptions::new().write(true).open(path).unwrap();
        file.set_modified(UNIX_EPOCH + Duration::from_secs(secs))
            .unwrap();
    }

    fn touch(dir: &Path, name: &str, secs: u64) {
        let path = dir.join(name);
        fs::write(&path, b"x").unwrap();
        set_mtime(&path, secs);
    }

    #[test]
    fn listing_shows_name_kind_and_modified_time_newest_first() {
        let tmp = Tmp::new();
        assert_eq!(list_for_tool(&tmp.0, &json!({})), EMPTY_LIST);
        assert!(!output_dir(&tmp.0).exists());

        let dir = output_dir(&tmp.0);
        fs::create_dir_all(&dir).unwrap();
        let old = 1_700_000_000u64;
        let mid = old + 86_400 * 5;
        let new = old + 86_400 * 10;
        touch(&dir, "古いメモ.docx", old);
        touch(&dir, "手数料.html", mid);
        touch(&dir, "新しい表.xlsx", new);
        touch(&dir, "メモ.txt", new + 10);
        fs::create_dir_all(dir.join("子")).unwrap();
        fs::write(dir.join("子").join("内.docx"), b"x").unwrap();
        fs::write(tmp.0.join("親.docx"), b"outside").unwrap();

        let text = list_for_tool(&tmp.0, &json!({}));
        let old_at = format_mtime(UNIX_EPOCH + Duration::from_secs(old));
        let mid_at = format_mtime(UNIX_EPOCH + Duration::from_secs(mid));
        let new_at = format_mtime(UNIX_EPOCH + Duration::from_secs(new));
        assert!(text.contains("名前\t種類\t更新日時"), "{text}");
        assert!(
            text.contains(&format!("古いメモ.docx\tWord\t{old_at}")),
            "{text}"
        );
        assert!(
            text.contains(&format!("手数料.html\tHTML\t{mid_at}")),
            "{text}"
        );
        assert!(
            text.contains(&format!("新しい表.xlsx\tExcel\t{new_at}")),
            "{text}"
        );
        let excel = text.find("新しい表.xlsx").unwrap();
        let html = text.find("手数料.html").unwrap();
        let word = text.find("古いメモ.docx").unwrap();
        assert!(excel < html && html < word, "{text}");
        assert!(text.contains("複数あるなら"), "{text}");
        assert!(!text.contains("メモ.txt"), "{text}");
        assert!(!text.contains("内.docx"), "{text}");
        assert!(!text.contains("親.docx"), "{text}");
        assert!(!text.contains(tmp.0.to_str().unwrap()), "{text}");
        assert_eq!(fs::read(tmp.0.join("親.docx")).unwrap(), b"outside");
    }

    #[test]
    fn listing_keeps_only_the_newest_slice() {
        let tmp = Tmp::new();
        let dir = output_dir(&tmp.0);
        fs::create_dir_all(&dir).unwrap();
        for index in 0..=MAX_LISTED {
            touch(
                &dir,
                &format!("表{index}.xlsx"),
                1_700_000_000 + index as u64,
            );
        }
        let text = list_for_tool(&tmp.0, &json!({}));
        assert!(text.contains(&format!("表{MAX_LISTED}.xlsx")), "{text}");
        assert!(!text.contains("表0.xlsx\t"), "{text}");
        assert!(text.contains("ほかに 1 件あります"), "{text}");
    }

    #[test]
    fn reading_word_excel_and_html_returns_contents_without_writing() {
        let tmp = Tmp::new();
        let docx = write_docx(
            &tmp.0,
            &json!({
                "filename": "先週の売上",
                "blocks": [{"type": "paragraph", "text": "りんごが120個"}]
            }),
        )
        .unwrap();
        let xlsx = write_xlsx(
            &tmp.0,
            &json!({
                "filename": "売上表",
                "sheets": [{"name": "一覧", "rows": [["品目", "金額"], ["りんご", "300"]]}]
            }),
        )
        .unwrap();
        let html = write_html(
            &tmp.0,
            &json!({
                "filename": "手数料",
                "html": "<!DOCTYPE html><html><body><p>手数料 1200</p></body></html>"
            }),
        )
        .unwrap();
        let before = [
            fs::read(&docx.path).unwrap(),
            fs::read(&xlsx.path).unwrap(),
            fs::read(&html.path).unwrap(),
        ];

        let word = read_for_tool(&tmp.0, &json!({"filename": docx.name}), CONTEXT);
        assert!(word.contains("りんごが120個"), "{word}");
        assert!(word.contains("種類: Word"), "{word}");
        assert!(word.contains("作成者: （なし）"), "{word}");
        assert!(word.contains("最終更新者: （なし）"), "{word}");
        assert!(word.contains(&docx.name), "{word}");
        assert!(!word.contains(tmp.0.to_str().unwrap()), "{word}");

        let sheet = read_for_tool(&tmp.0, &json!({"filename": xlsx.name}), CONTEXT);
        assert!(sheet.contains("一覧"), "{sheet}");
        assert!(sheet.contains("りんご"), "{sheet}");
        assert!(sheet.contains("300"), "{sheet}");
        assert!(sheet.contains("種類: Excel"), "{sheet}");

        let page = read_for_tool(&tmp.0, &json!({"filename": html.name}), CONTEXT);
        assert!(page.contains("<p>手数料 1200</p>"), "{page}");
        assert!(page.contains("種類: HTML"), "{page}");
        assert!(page.contains("edit_html"), "{page}");

        assert_eq!(fs::read(&docx.path).unwrap(), before[0]);
        assert_eq!(fs::read(&xlsx.path).unwrap(), before[1]);
        assert_eq!(fs::read(&html.path).unwrap(), before[2]);
        let names: Vec<_> = fs::read_dir(output_dir(&tmp.0))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 3, "{names:?}");
    }

    #[test]
    fn reading_rejects_paths_outside_the_output_folder() {
        let tmp = Tmp::new();
        let outside = tmp.0.join("secret.docx");
        fs::write(&outside, b"UNIQUE_OUTSIDE").unwrap();
        fs::create_dir_all(output_dir(&tmp.0)).unwrap();
        let inside = output_dir(&tmp.0).join("手数料.html");
        fs::write(&inside, "<p>内側</p>").unwrap();
        let absolute = fs::canonicalize(&inside).unwrap();

        for name in [
            "../secret.docx",
            r"..\secret.docx",
            "foo/bar.docx",
            r"foo\bar.docx",
            "a..b.docx",
            ".hidden.docx",
            r"C:\Windows\secret.docx",
            r"\\server\share\a.docx",
            absolute.to_str().unwrap(),
            "notes.txt",
            "",
        ] {
            let err = read_for_tool(&tmp.0, &json!({"filename": name}), CONTEXT);
            assert!(
                err.contains("ファイル名")
                    || err.contains("Word")
                    || err.contains("指定")
                    || err.contains("外"),
                "{name}: {err}"
            );
            assert!(!err.contains("UNIQUE_OUTSIDE"), "{name}: {err}");
            assert!(!err.contains("内側"), "{name}: {err}");
        }
        assert_eq!(fs::read(&outside).unwrap(), b"UNIQUE_OUTSIDE");
        assert_eq!(fs::read_to_string(&inside).unwrap(), "<p>内側</p>");
        assert_eq!(
            fs::read_dir(output_dir(&tmp.0)).unwrap().count(),
            1,
            "reading must not create a file"
        );
    }

    #[test]
    fn oversized_html_is_refused_without_a_partial_body() {
        let tmp = Tmp::new();
        let dir = output_dir(&tmp.0);
        fs::create_dir_all(&dir).unwrap();
        let body = format!("<p>{}</p>", "あ".repeat(MAX_HTML_BYTES));
        let path = dir.join("大きい.html");
        fs::write(&path, &body).unwrap();
        let before = fs::read(&path).unwrap();
        let err = read_for_tool(&tmp.0, &json!({"filename": "大きい.html"}), CONTEXT);
        assert!(err.contains("大きすぎ"), "{err}");
        assert!(!err.contains('あ'), "{err}");
        assert_eq!(fs::read(&path).unwrap(), before);
    }

    #[test]
    fn long_office_text_is_cut_on_a_character_boundary() {
        let text = "あ".repeat(MAX_TEXT_CHARS + 5);
        let capped = cap_text(&text);
        assert!(capped.contains("省略"), "{capped}");
        assert_eq!(
            capped.chars().filter(|ch| *ch == 'あ').count(),
            MAX_TEXT_CHARS
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_that_leave_the_output_folder_are_not_listed_or_read() {
        let tmp = Tmp::new();
        let outside_dir = tmp.0.join("elsewhere");
        fs::create_dir_all(&outside_dir).unwrap();
        let secret = outside_dir.join("秘密.docx");
        fs::write(&secret, "外の本文").unwrap();
        std::os::unix::fs::symlink(&outside_dir, output_dir(&tmp.0)).unwrap();

        let listed = list_for_tool(&tmp.0, &json!({}));
        assert!(listed.contains('外'), "{listed}");
        assert!(!listed.contains("秘密.docx"), "{listed}");
        assert!(!listed.contains("外の本文"), "{listed}");
        let read = read_for_tool(&tmp.0, &json!({"filename": "秘密.docx"}), CONTEXT);
        assert!(read.contains('外'), "{read}");
        assert!(!read.contains("外の本文"), "{read}");
        assert_eq!(fs::read_to_string(&secret).unwrap(), "外の本文");

        let _ = fs::remove_file(output_dir(&tmp.0));
        let dir = output_dir(&tmp.0);
        fs::create_dir_all(&dir).unwrap();
        let file = tmp.0.join("outside.docx");
        fs::write(&file, "外の本文").unwrap();
        std::os::unix::fs::symlink(&file, dir.join("link.docx")).unwrap();
        fs::write(dir.join("残す.docx"), b"keep").unwrap();
        let listed = list_for_tool(&tmp.0, &json!({}));
        assert!(listed.contains("残す.docx"), "{listed}");
        assert!(!listed.contains("link.docx"), "{listed}");
        assert!(!listed.contains("外の本文"), "{listed}");
        let read = read_for_tool(&tmp.0, &json!({"filename": "link.docx"}), CONTEXT);
        assert!(read.contains('外'), "{read}");
        assert!(!read.contains("外の本文"), "{read}");
        assert_eq!(fs::read_to_string(&file).unwrap(), "外の本文");
    }

    #[test]
    fn the_model_is_told_to_pick_one_file_and_ask_when_several_match() {
        let sys = super::super::system_for_request(&crate::db::Settings::default());
        assert_eq!(sys.matches(LLM_OUTPUT_SENTINEL).count(), 1, "{sys}");
        assert!(sys.contains("output_file"), "{sys}");
        let again = super::super::system_for_request(&crate::db::Settings {
            llm_system_prompt: sys,
            ..crate::db::Settings::default()
        });
        assert_eq!(again.matches(LLM_OUTPUT_SENTINEL).count(), 1);
        let schema = crate::llm::tools::tools_schema(false, false, false, false).to_string();
        assert!(schema.contains(TOOL_OUTPUT_FILE), "{schema}");
        assert!(!schema.contains("list_output_files"), "{schema}");
        assert!(!schema.contains("read_output_file"), "{schema}");
        assert_eq!(schema.matches("利用者が同意したときだけ").count(), 2, "{schema}");
        assert!(schema.contains("更新日時"), "{schema}");
        assert!(schema.contains("候補が複数"), "{schema}");
        assert!(schema.contains("検索フォルダ"), "{schema}");
        assert!(schema.contains("rename"), "{schema}");
        assert!(crate::llm::tools::counts_toward_tool_round(&[
            TOOL_OUTPUT_FILE
        ]));
        assert!(schema.contains("ファイル名の一部"), "{schema}");
    }

    #[test]
    fn name_filter_finds_a_file_older_than_the_newest_slice() {
        let tmp = Tmp::new();
        let dir = output_dir(&tmp.0);
        fs::create_dir_all(&dir).unwrap();
        let old = 1_700_000_000u64;
        touch(&dir, "古い売上表.xlsx", old);
        for index in 1..=MAX_LISTED {
            touch(&dir, &format!("表{index}.xlsx"), old + index as u64);
        }
        let all = list_for_tool(&tmp.0, &json!({}));
        assert!(!all.contains("古い売上表.xlsx"), "{all}");
        let found = list_for_tool(&tmp.0, &json!({"name": "古い売上"}));
        let when = format_mtime(UNIX_EPOCH + Duration::from_secs(old));
        assert!(
            found.contains(&format!("古い売上表.xlsx\tExcel\t{when}")),
            "{found}"
        );
        assert!(!found.contains("表1.xlsx"), "{found}");
        let none = list_for_tool(&tmp.0, &json!({"name": "存在しない"}));
        assert!(none.contains("存在しない"), "{none}");
        assert!(!none.contains("古い売上表.xlsx"), "{none}");
    }

    #[test]
    fn reading_a_docx_shows_the_core_author_and_does_not_write() {
        let tmp = Tmp::new();
        let dir = output_dir(&tmp.0);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("契約書.docx");
        let mut zip = zip::ZipWriter::new(fs::File::create(&path).unwrap());
        let opts = zip::write::SimpleFileOptions::default();
        zip.start_file("word/document.xml", opts).unwrap();
        std::io::Write::write_all(
            &mut zip,
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><w:document><w:body><w:p><w:r><w:t>本文の当事者</w:t></w:r></w:p></w:body></w:document>"
                .as_bytes(),
        )
        .unwrap();
        zip.start_file("docProps/core.xml", opts).unwrap();
        std::io::Write::write_all(
            &mut zip,
            "<cp:coreProperties xmlns:cp=\"http://schemas.openxmlformats.org/package/2006/metadata/core-properties\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\"><dc:creator>山田太郎</dc:creator></cp:coreProperties>"
                .as_bytes(),
        )
        .unwrap();
        zip.finish().unwrap();
        let before = fs::read(&path).unwrap();
        let word = read_for_tool(&tmp.0, &json!({"filename": "契約書.docx"}), CONTEXT);
        assert!(word.contains("本文の当事者"), "{word}");
        assert!(word.contains("作成者: 山田太郎"), "{word}");
        assert!(word.contains("最終更新者: （なし）"), "{word}");
        assert!(!word.contains(tmp.0.to_str().unwrap()), "{word}");
        assert_eq!(fs::read(&path).unwrap(), before);
    }

    #[test]
    fn htm_is_not_listed_or_read() {
        let tmp = Tmp::new();
        let dir = output_dir(&tmp.0);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("手数料.htm"),
            "<html><body><p>htm本文</p></body></html>",
        )
        .unwrap();
        fs::write(
            dir.join("手数料.html"),
            "<html><body><p>html本文</p></body></html>",
        )
        .unwrap();
        let listed = list_for_tool(&tmp.0, &json!({}));
        assert!(listed.contains("手数料.html\tHTML\t"), "{listed}");
        assert!(
            listed.lines().all(|line| !line.starts_with("手数料.htm\t")),
            "{listed}"
        );
        assert!(!listed.contains("htm本文"), "{listed}");
        let read = read_for_tool(&tmp.0, &json!({"filename": "手数料.htm"}), CONTEXT);
        assert!(
            read.contains("ファイル名") || read.contains("Word"),
            "{read}"
        );
        assert!(!read.contains("htm本文"), "{read}");
        let html = read_for_tool(&tmp.0, &json!({"filename": "手数料.html"}), CONTEXT);
        assert!(html.contains("html本文"), "{html}");
    }

    #[test]
    fn a_later_send_does_not_halve_an_html_source() {
        let source = format!("<html><body><p>{}</p></body></html>", "あ".repeat(4_000));
        let payload = format!(
            "ファイル: ページ.html\n種類: HTML\n更新日時: 2026-11-01 00:00\n\n{HTML_SOURCE_NOTE}\n{source}"
        );
        let mut tool = llm_message("t1", "tool", &payload);
        tool.tool_name = TOOL_OUTPUT_FILE.into();
        let history = vec![llm_message("u1", "user", "直して"), tool];
        let (turns, _) = crate::llm::context::assemble_turns("役割", &[], &history, 400);
        let tool_turn = turns
            .iter()
            .find(|turn| turn.role == "tool")
            .expect("html tool result stays in the request");
        assert_eq!(tool_turn.content, HTML_READ_REFUSAL);
        assert!(!tool_turn.content.contains(HTML_SOURCE_NOTE));
        assert!(!tool_turn.content.contains('あ'));
    }

    #[test]
    fn html_that_does_not_fit_the_tool_budget_is_refused_without_a_source() {
        let tmp = Tmp::new();
        let dir = output_dir(&tmp.0);
        fs::create_dir_all(&dir).unwrap();
        let html = format!("<html><body><p>{}</p></body></html>", "あ".repeat(20_000));
        let path = dir.join("大きい計算.html");
        fs::write(&path, &html).unwrap();
        let before = fs::read(&path).unwrap();
        let min_ctx = crate::db::MIN_LLM_CONTEXT_TOKENS as usize;
        let refused = read_for_tool(&tmp.0, &json!({"filename": "大きい計算.html"}), min_ctx);
        assert_eq!(refused, HTML_READ_REFUSAL);
        assert!(!refused.contains('あ'), "{refused}");
        assert!(!refused.contains("edit_html の html には"), "{refused}");
        let full = read_for_tool(&tmp.0, &json!({"filename": "大きい計算.html"}), CONTEXT);
        assert!(full.contains(HTML_SOURCE_NOTE), "{full}");
        assert!(full.contains("</html>"), "{full}");
        assert!(full.contains(&"あ".repeat(100)), "{full}");
        assert_eq!(deliver_html_read(full, 100, 0), HTML_READ_REFUSAL);
        assert_eq!(fs::read(&path).unwrap(), before);
    }

    #[test]
    fn a_followup_does_not_leave_a_truncated_html_source() {
        let source = format!("<html><body>{}</body></html>", "あ".repeat(4_000));
        let payload = format!(
            "ファイル: ページ.html\n種類: HTML\n更新日時: 2026-11-01 00:00\n\n{HTML_SOURCE_NOTE}\n{source}"
        );
        let mut tool = crate::llm::context::ChatTurn::text("tool", payload);
        tool.name = Some(TOOL_OUTPUT_FILE.into());
        let mut turns = vec![crate::llm::context::ChatTurn::text("user", "直して"), tool];
        crate::llm::context::fit_followup_request(&mut turns, 400);
        assert_eq!(turns[1].content, HTML_READ_REFUSAL);
        assert!(!turns[1].content.contains('あ'));
        assert!(!turns[1].content.contains("edit_html の html には"));

        let small = format!(
            "ファイル: ページ.html\n種類: HTML\n更新日時: 2026-11-01 00:00\n\n{HTML_SOURCE_NOTE}\n<html><body><p>短い</p></body></html>"
        );
        let mut kept = crate::llm::context::ChatTurn::text("tool", small);
        kept.name = Some(TOOL_OUTPUT_FILE.into());
        let mut turns = vec![crate::llm::context::ChatTurn::text("user", "直して"), kept];
        crate::llm::context::fit_followup_request(&mut turns, 8_000);
        assert!(
            turns[1].content.contains("<p>短い</p>"),
            "{}",
            turns[1].content
        );
        assert!(turns[1].content.contains(HTML_SOURCE_NOTE));
    }

    fn docx_with(dir: &Path, name: &str, text: &str) {
        fs::create_dir_all(dir).unwrap();
        let path = dir.join(name);
        let mut zip = zip::ZipWriter::new(fs::File::create(&path).unwrap());
        let opts = zip::write::SimpleFileOptions::default();
        zip.start_file("word/document.xml", opts).unwrap();
        let xml = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body><w:p><w:r><w:t>{text}</w:t></w:r></w:p></w:body></w:document>"
        );
        std::io::Write::write_all(&mut zip, xml.as_bytes()).unwrap();
        zip.finish().unwrap();
    }

    fn docx_text(path: &Path) -> String {
        let file = fs::File::open(path).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        let mut entry = archive.by_name("word/document.xml").unwrap();
        let mut xml = String::new();
        std::io::Read::read_to_string(&mut entry, &mut xml).unwrap();
        xml
    }

    #[test]
    fn rename_rejects_bad_names_extension_changes_and_clashes() {
        let tmp = Tmp::new();
        let db = crate::db::Db::open(&tmp.0.join("argos.db")).unwrap();
        let dir = output_dir(&tmp.0);
        docx_with(&dir, "契約.docx", "甲");
        fs::write(dir.join("別紙.docx"), b"not-a-real-docx").unwrap();
        let missing = rename_for_tool(
            &db,
            &tmp.0,
            &json!({"filename": "無い.docx", "new_filename": "次.docx"}),
        );
        assert!(missing.contains("見つかりません"), "{missing}");
        for (bad, needle) in [
            ("../外.docx", "ファイル名"),
            ("C:\\Windows\\a.docx", "ファイル名"),
            (".hidden.docx", "ファイル名"),
            ("メモ.txt", "Word"),
            ("", "新しいファイル名"),
        ] {
            let err = rename_for_tool(
                &db,
                &tmp.0,
                &json!({"filename": "契約.docx", "new_filename": bad}),
            );
            assert!(err.contains(needle), "{bad}: {err}");
            assert!(dir.join("契約.docx").is_file(), "{bad}");
        }
        let ext = rename_for_tool(
            &db,
            &tmp.0,
            &json!({"filename": "契約.docx", "new_filename": "契約.xlsx"}),
        );
        assert!(ext.contains("拡張子"), "{ext}");
        assert!(dir.join("契約.docx").is_file());
        assert!(!dir.join("契約.xlsx").is_file());
        let clash = rename_for_tool(
            &db,
            &tmp.0,
            &json!({"filename": "契約.docx", "new_filename": "別紙.docx"}),
        );
        assert!(clash.contains("リネームはしていません"), "{clash}");
        assert!(clash.contains("旧ファイル名: 契約.docx"), "{clash}");
        assert!(clash.contains("新ファイル名: 別紙.docx"), "{clash}");
        assert!(clash.contains("既存のファイル名"), "{clash}");
        assert!(clash.contains("契約.docx"), "{clash}");
        assert!(clash.contains("別紙.docx"), "{clash}");
        assert!(!fs::read(dir.join("契約.docx")).unwrap().is_empty());
        assert_eq!(fs::read(dir.join("別紙.docx")).unwrap(), b"not-a-real-docx");
    }

    #[test]
    fn rename_moves_the_file_updates_history_cards_and_the_edit_chain() {
        let tmp = Tmp::new();
        let db = crate::db::Db::open(&tmp.0.join("argos.db")).unwrap();
        let dir = output_dir(&tmp.0);
        docx_with(&dir, "メモ.docx", "甲乙");
        let thread = db.create_llm_thread("a", false).unwrap();
        let other = db.create_llm_thread("b", false).unwrap();
        let first = crate::office_edit::edit_in_thread(
            &db,
            &tmp.0,
            "",
            &thread.id,
            &json!({"filename": "メモ.docx", "replacements": [{"find": "甲", "replace": "丁"}]}),
        )
        .unwrap();
        let old = first.filename.clone();
        let new = "20261128メモ（1）.docx";
        let old_path = dir.join(&old);
        let original = dir.join("メモ.docx");
        db.insert_llm_office_edit(&other.id, &old, &original.to_string_lossy())
            .unwrap();
        let marker = format!(
            "{FILE_LINE}{old}\n{OFFICE_MARKER}\tdocx\t{old}\n{PATH_LINE}{}\n本文の丁乙\n",
            old_path.display()
        );
        db.insert_llm_message_full(&thread.id, "tool", &marker, "", "", "c1", "edit_office", "")
            .unwrap();
        db.insert_llm_notice(&thread.id, &format!("{OFFICE_MARKER}\tdocx\t{old}\n"))
            .unwrap();
        db.insert_llm_message(&thread.id, "assistant", &format!("{old} を直しました"))
            .unwrap();
        db.insert_llm_source(
            &thread.id,
            "attach",
            &old_path.to_string_lossy(),
            &old,
            "",
            "丁乙",
            "",
        )
        .unwrap();
        db.insert_llm_source(
            &thread.id,
            "attach",
            &old_path.to_string_lossy(),
            "手元のメモ",
            "custom",
            "丁乙",
            "",
        )
        .unwrap();
        let note = db.create_note("案件").unwrap();
        db.update_note_memo(&note.id, &format!("{old} を参照"))
            .unwrap();

        let done = rename_for_tool(&db, &tmp.0, &json!({"filename": old, "new_filename": new}));
        assert!(done.contains("リネームしました"), "{done}");
        assert!(done.contains(&format!("旧ファイル名: {old}")), "{done}");
        assert!(done.contains(&format!("新ファイル名: {new}")), "{done}");
        assert!(!old_path.exists());
        assert!(dir.join(new).is_file());
        assert!(docx_text(&original).contains("甲乙"));
        for thread_id in [&thread.id, &other.id] {
            let rows = db.list_llm_office_edits(thread_id).unwrap();
            assert_eq!(
                rows.last().map(|row| row.output_name.as_str()),
                Some(new),
                "{thread_id} {rows:?}"
            );
        }
        let kept_source = db.list_llm_office_edits(&other.id).unwrap();
        assert!(
            kept_source[0].source_path.ends_with("メモ.docx"),
            "{}",
            kept_source[0].source_path
        );
        let messages = db.list_llm_messages(&thread.id).unwrap();
        let tool = messages.iter().find(|row| row.role == "tool").unwrap();
        assert!(
            tool.content.contains(&format!("{FILE_LINE}{new}")),
            "{}",
            tool.content
        );
        assert!(
            tool.content
                .contains(&format!("{OFFICE_MARKER}\tdocx\t{new}")),
            "{}",
            tool.content
        );
        assert!(!tool.content.contains(&old), "{}", tool.content);
        assert!(tool.content.contains("本文の丁乙"), "{}", tool.content);
        let notice = messages.iter().find(|row| row.kind == "notice").unwrap();
        assert!(
            notice.content.contains(new) && !notice.content.contains(&old),
            "{}",
            notice.content
        );
        let assistant = messages
            .iter()
            .find(|row| row.role == "assistant" && row.kind.is_empty())
            .unwrap();
        assert!(assistant.content.contains(&old), "{}", assistant.content);
        let sources = db.list_llm_sources(&thread.id).unwrap();
        let titled = sources.iter().find(|row| row.title == new).unwrap();
        assert!(titled.path.ends_with(new), "{}", titled.path);
        let custom = sources.iter().find(|row| row.title == "手元のメモ").unwrap();
        assert!(custom.path.ends_with(new), "{}", custom.path);
        assert_eq!(
            db.get_note(&note.id).unwrap().unwrap().memo,
            format!("{old} を参照")
        );

        let followed = crate::office_edit::edit_in_thread(
            &db,
            &tmp.0,
            "",
            &thread.id,
            &json!({"filename": "メモ.docx", "replacements": [{"find": "乙", "replace": "戊"}]}),
        )
        .unwrap();
        assert_eq!(followed.filename, new);
        assert!(docx_text(&dir.join(new)).contains("丁戊"));
        assert!(docx_text(&original).contains("甲乙"));
        let names: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".docx"))
            .collect();
        assert!(names.contains(&new.to_string()), "{names:?}");
        assert!(names.contains(&"メモ.docx".to_string()), "{names:?}");
        assert!(
            !names.iter().any(|name| name.contains("編集済み")),
            "{names:?}"
        );
    }

    #[test]
    fn rename_overwrite_replaces_and_a_locked_move_touches_neither_file() {
        let tmp = Tmp::new();
        let db = crate::db::Db::open(&tmp.0.join("argos.db")).unwrap();
        let dir = output_dir(&tmp.0);
        docx_with(&dir, "甲.docx", "甲の本文");
        docx_with(&dir, "乙.docx", "乙の本文");
        let kept_b = fs::read(dir.join("乙.docx")).unwrap();
        let kept_a = fs::read(dir.join("甲.docx")).unwrap();
        super::fail_next_output_rename(1);
        let locked = rename_for_tool(
            &db,
            &tmp.0,
            &json!({"filename": "甲.docx", "new_filename": "乙.docx", "overwrite": true}),
        );
        assert!(locked.contains("動かしていません"), "{locked}");
        assert_eq!(fs::read(dir.join("甲.docx")).unwrap(), kept_a);
        assert_eq!(fs::read(dir.join("乙.docx")).unwrap(), kept_b);
        let done = rename_for_tool(
            &db,
            &tmp.0,
            &json!({"filename": "甲.docx", "new_filename": "乙.docx", "overwrite": true}),
        );
        assert!(done.contains("リネームしました"), "{done}");
        assert!(!dir.join("甲.docx").exists());
        assert_eq!(fs::read(dir.join("乙.docx")).unwrap(), kept_a);
        docx_with(&dir, "丙.docx", "丙");
        let before = fs::read(dir.join("丙.docx")).unwrap();
        super::fail_next_output_rename(2);
        let source_locked = rename_for_tool(
            &db,
            &tmp.0,
            &json!({"filename": "丙.docx", "new_filename": "丁.docx"}),
        );
        assert!(
            source_locked.contains("動かしていません"),
            "{source_locked}"
        );
        assert_eq!(fs::read(dir.join("丙.docx")).unwrap(), before);
        assert!(!dir.join("丁.docx").exists());
    }

    #[test]
    fn edit_office_new_filename_uses_that_name_and_refuses_a_clash() {
        let tmp = Tmp::new();
        let db = crate::db::Db::open(&tmp.0.join("argos.db")).unwrap();
        let dir = output_dir(&tmp.0);
        docx_with(&dir, "メモ.docx", "甲乙");
        docx_with(&dir, "既存.docx", "そのまま");
        let thread = db.create_llm_thread("named", false).unwrap();
        let original = fs::read(dir.join("メモ.docx")).unwrap();
        let existing = fs::read(dir.join("既存.docx")).unwrap();
        let clash = crate::office_edit::edit_in_thread(
            &db,
            &tmp.0,
            "",
            &thread.id,
            &json!({
                "filename": "メモ.docx",
                "new_filename": "既存.docx",
                "replacements": [{"find": "甲", "replace": "丁"}]
            }),
        )
        .unwrap_err();
        assert!(clash.contains("既にあります"), "{clash}");
        assert!(clash.contains("既存のファイル名"), "{clash}");
        assert!(clash.contains("既存.docx"), "{clash}");
        assert_eq!(fs::read(dir.join("メモ.docx")).unwrap(), original);
        assert_eq!(fs::read(dir.join("既存.docx")).unwrap(), existing);
        let bad_ext = crate::office_edit::edit_in_thread(
            &db,
            &tmp.0,
            "",
            &thread.id,
            &json!({
                "filename": "メモ.docx",
                "new_filename": "メモ.xlsx",
                "replacements": [{"find": "甲", "replace": "丁"}]
            }),
        )
        .unwrap_err();
        assert!(bad_ext.contains("拡張子"), "{bad_ext}");
        assert_eq!(fs::read(dir.join("メモ.docx")).unwrap(), original);

        let saved = crate::office_edit::edit_in_thread(
            &db,
            &tmp.0,
            "",
            &thread.id,
            &json!({
                "filename": "メモ.docx",
                "new_filename": "20261128メモ（1）.docx",
                "replacements": [{"find": "甲", "replace": "丁"}]
            }),
        )
        .unwrap();
        assert_eq!(saved.filename, "20261128メモ（1）.docx");
        assert!(
            saved.report.contains("指定したファイル名で保存しました"),
            "{}",
            saved.report
        );
        assert!(
            saved.report.contains("旧ファイル名: メモ.docx"),
            "{}",
            saved.report
        );
        assert!(
            saved
                .report
                .contains("新ファイル名: 20261128メモ（1）.docx"),
            "{}",
            saved.report
        );
        assert!(docx_text(&dir.join("20261128メモ（1）.docx")).contains("丁乙"));
        assert_eq!(fs::read(dir.join("メモ.docx")).unwrap(), original);
        let rows = db.list_llm_office_edits(&thread.id).unwrap();
        assert_eq!(rows.last().unwrap().output_name, "20261128メモ（1）.docx");

        let followed = crate::office_edit::edit_in_thread(
            &db,
            &tmp.0,
            "",
            &thread.id,
            &json!({"filename": "メモ.docx", "replacements": [{"find": "乙", "replace": "戊"}]}),
        )
        .unwrap();
        assert_eq!(followed.filename, "20261128メモ（1）.docx");
        assert!(docx_text(&dir.join("20261128メモ（1）.docx")).contains("丁戊"));
        assert_eq!(fs::read(dir.join("メモ.docx")).unwrap(), original);
        let edited: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains("編集済み"))
            .collect();
        assert!(edited.is_empty(), "{edited:?}");
    }

    #[test]
    fn edit_office_new_filename_does_not_write_a_sibling_when_the_target_is_locked() {
        let tmp = Tmp::new();
        let db = crate::db::Db::open(&tmp.0.join("argos.db")).unwrap();
        let dir = output_dir(&tmp.0);
        docx_with(&dir, "メモ.docx", "甲乙");
        docx_with(&dir, "先.docx", "先の本文");
        let memo = fs::read(dir.join("メモ.docx")).unwrap();
        let prior = fs::read(dir.join("先.docx")).unwrap();
        let thread = db.create_llm_thread("locked", false).unwrap();
        crate::office_edit::fail_next_replace_of_existing_file();
        let err = crate::office_edit::edit_in_thread(
            &db,
            &tmp.0,
            "",
            &thread.id,
            &json!({
                "filename": "メモ.docx",
                "new_filename": "先.docx",
                "overwrite": true,
                "replacements": [{"find": "甲", "replace": "丁"}]
            }),
        )
        .unwrap_err();
        assert!(err.contains("開かれている"), "{err}");
        assert_eq!(fs::read(dir.join("メモ.docx")).unwrap(), memo);
        assert_eq!(fs::read(dir.join("先.docx")).unwrap(), prior);
        let names: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| !name.starts_with('.'))
            .collect();
        assert_eq!(names.len(), 2, "{names:?}");
    }

    #[test]
    fn edit_office_new_filename_equal_to_the_source_requires_overwrite() {
        let tmp = Tmp::new();
        let db = crate::db::Db::open(&tmp.0.join("argos.db")).unwrap();
        let dir = output_dir(&tmp.0);
        docx_with(&dir, "メモ.docx", "甲乙");
        let thread = db.create_llm_thread("same", false).unwrap();
        let original = fs::read(dir.join("メモ.docx")).unwrap();
        let refused = crate::office_edit::edit_in_thread(
            &db,
            &tmp.0,
            "",
            &thread.id,
            &json!({
                "filename": "メモ.docx",
                "new_filename": "メモ.docx",
                "replacements": [{"find": "甲", "replace": "丁"}]
            }),
        )
        .unwrap_err();
        assert!(refused.contains("既にあります"), "{refused}");
        assert!(refused.contains("書いていません"), "{refused}");
        assert_eq!(fs::read(dir.join("メモ.docx")).unwrap(), original);

        let saved = crate::office_edit::edit_in_thread(
            &db,
            &tmp.0,
            "",
            &thread.id,
            &json!({
                "filename": "メモ.docx",
                "new_filename": "メモ.docx",
                "overwrite": true,
                "replacements": [{"find": "甲", "replace": "丁"}]
            }),
        )
        .unwrap();
        assert_eq!(saved.filename, "メモ.docx");
        assert!(docx_text(&dir.join("メモ.docx")).contains("丁乙"));
        let names = docx_names(&dir);
        assert_eq!(names, vec!["メモ.docx".to_string()], "{names:?}");
    }

    #[test]
    fn continuing_the_chain_file_under_its_own_name_does_not_need_overwrite() {
        let tmp = Tmp::new();
        let db = crate::db::Db::open(&tmp.0.join("argos.db")).unwrap();
        let dir = output_dir(&tmp.0);
        docx_with(&dir, "メモ.docx", "甲乙");
        let thread = db.create_llm_thread("chain", false).unwrap();
        let original = fs::read(dir.join("メモ.docx")).unwrap();
        let first = crate::office_edit::edit_in_thread(
            &db,
            &tmp.0,
            "",
            &thread.id,
            &json!({"filename": "メモ.docx", "replacements": [{"find": "甲", "replace": "丁"}]}),
        )
        .unwrap();
        let chain = first.filename.clone();
        let continued = crate::office_edit::edit_in_thread(
            &db,
            &tmp.0,
            "",
            &thread.id,
            &json!({
                "filename": chain,
                "new_filename": chain,
                "replacements": [{"find": "乙", "replace": "戊"}]
            }),
        )
        .unwrap();
        assert_eq!(continued.filename, chain);
        assert!(docx_text(&dir.join(&chain)).contains("丁戊"));
        assert_eq!(fs::read(dir.join("メモ.docx")).unwrap(), original);
        let back = crate::office_edit::edit_in_thread(
            &db,
            &tmp.0,
            "",
            &thread.id,
            &json!({
                "filename": "メモ.docx",
                "new_filename": "メモ.docx",
                "replacements": [{"find": "甲", "replace": "己"}]
            }),
        )
        .unwrap_err();
        assert!(back.contains("既にあります"), "{back}");
        assert_eq!(fs::read(dir.join("メモ.docx")).unwrap(), original);
        assert!(docx_text(&dir.join(&chain)).contains("丁戊"));
    }

    #[test]
    fn invalid_new_names_are_rejected_and_not_rewritten() {
        let tmp = Tmp::new();
        let db = crate::db::Db::open(&tmp.0.join("argos.db")).unwrap();
        let dir = output_dir(&tmp.0);
        docx_with(&dir, "メモ.docx", "甲乙");
        fs::write(dir.join("表.html"), "<p>甲</p>").unwrap();
        let thread = db.create_llm_thread("names", false).unwrap();
        let before = fs::read(dir.join("メモ.docx")).unwrap();
        let html_before = fs::read(dir.join("表.html")).unwrap();
        let names = [
            "契約*.docx",
            "問?.docx",
            "a\"b.docx",
            "a<b.docx",
            "a>b.docx",
            "a|b.docx",
            "CON.docx",
            "CON.v2.docx",
            "NUL.docx",
            "COM1.docx",
            "lpt9.docx",
            "メモ .docx",
        ];
        for name in names {
            let renamed = rename_for_tool(
                &db,
                &tmp.0,
                &json!({"filename": "メモ.docx", "new_filename": name}),
            );
            assert!(renamed.contains("無効"), "{name}: {renamed}");
            let edited = crate::office_edit::edit_in_thread(
                &db,
                &tmp.0,
                "",
                &thread.id,
                &json!({
                    "filename": "メモ.docx",
                    "new_filename": name,
                    "replacements": [{"find": "甲", "replace": "丁"}]
                }),
            )
            .unwrap_err();
            assert!(edited.contains("無効"), "{name}: {edited}");
            assert_eq!(fs::read(dir.join("メモ.docx")).unwrap(), before, "{name}");
        }
        let long = format!("{}.docx", "あ".repeat(61));
        let long_rename = rename_for_tool(
            &db,
            &tmp.0,
            &json!({"filename": "メモ.docx", "new_filename": long}),
        );
        assert!(long_rename.contains("無効"), "{long_rename}");
        let long_edit = crate::office_edit::edit_in_thread(
            &db,
            &tmp.0,
            "",
            &thread.id,
            &json!({
                "filename": "メモ.docx",
                "new_filename": long,
                "replacements": [{"find": "甲", "replace": "丁"}]
            }),
        )
        .unwrap_err();
        assert!(long_edit.contains("無効"), "{long_edit}");
        for name in ["prn.xlsx", "AUX.html"] {
            let renamed = rename_for_tool(
                &db,
                &tmp.0,
                &json!({"filename": "メモ.docx", "new_filename": name}),
            );
            assert!(renamed.contains("無効"), "{name}: {renamed}");
            let edited = crate::office_edit::edit_in_thread(
                &db,
                &tmp.0,
                "",
                &thread.id,
                &json!({
                    "filename": "メモ.docx",
                    "new_filename": name,
                    "replacements": [{"find": "甲", "replace": "丁"}]
                }),
            )
            .unwrap_err();
            assert!(edited.contains("無効"), "{name}: {edited}");
        }
        let html_bad = rename_for_tool(
            &db,
            &tmp.0,
            &json!({"filename": "表.html", "new_filename": "CON.html"}),
        );
        assert!(html_bad.contains("無効"), "{html_bad}");
        assert_eq!(fs::read(dir.join("メモ.docx")).unwrap(), before);
        assert_eq!(fs::read(dir.join("表.html")).unwrap(), html_before);
        let written: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .filter(|name| name.contains('_'))
            .collect();
        assert!(written.is_empty(), "{written:?}");
    }

    #[test]
    fn a_rename_error_that_is_not_a_lock_reports_the_os_reason() {
        let tmp = Tmp::new();
        let db = crate::db::Db::open(&tmp.0.join("argos.db")).unwrap();
        let dir = output_dir(&tmp.0);
        docx_with(&dir, "甲.docx", "甲の本文");
        let before = fs::read(dir.join("甲.docx")).unwrap();
        super::fail_next_output_rename_os(2, 2);
        let err = rename_for_tool(
            &db,
            &tmp.0,
            &json!({"filename": "甲.docx", "new_filename": "乙.docx"}),
        );
        assert!(err.contains("リネームできませんでした"), "{err}");
        assert!(err.contains("動かしていません"), "{err}");
        assert!(!err.contains("開かれている"), "{err}");
        assert!(!err.contains("os error"), "{err}");
        assert_eq!(fs::read(dir.join("甲.docx")).unwrap(), before);
        assert!(!dir.join("乙.docx").exists());
    }

    #[test]
    fn an_explicit_save_error_that_is_not_a_lock_reports_the_os_reason() {
        let tmp = Tmp::new();
        let db = crate::db::Db::open(&tmp.0.join("argos.db")).unwrap();
        let dir = output_dir(&tmp.0);
        docx_with(&dir, "メモ.docx", "甲乙");
        docx_with(&dir, "先.docx", "先の本文");
        let memo = fs::read(dir.join("メモ.docx")).unwrap();
        let prior = fs::read(dir.join("先.docx")).unwrap();
        let thread = db.create_llm_thread("reason", false).unwrap();
        crate::office_edit::fail_next_replace_os_error(2);
        let err = crate::office_edit::edit_in_thread(
            &db,
            &tmp.0,
            "",
            &thread.id,
            &json!({
                "filename": "メモ.docx",
                "new_filename": "先.docx",
                "overwrite": true,
                "replacements": [{"find": "甲", "replace": "丁"}]
            }),
        )
        .unwrap_err();
        assert!(err.contains("保存しませんでした"), "{err}");
        assert!(err.contains("そのまま"), "{err}");
        assert!(!err.contains("開かれている"), "{err}");
        assert!(!err.contains("os error"), "{err}");
        assert_eq!(fs::read(dir.join("メモ.docx")).unwrap(), memo);
        assert_eq!(fs::read(dir.join("先.docx")).unwrap(), prior);
    }

    #[test]
    fn a_long_chain_file_can_be_saved_under_its_own_name() {
        let tmp = Tmp::new();
        let db = crate::db::Db::open(&tmp.0.join("argos.db")).unwrap();
        let dir = output_dir(&tmp.0);
        let stem = "あ".repeat(40);
        let filename = format!("{stem}.docx");
        docx_with(&dir, &filename, "甲乙");
        let thread = db.create_llm_thread("long", false).unwrap();
        let original = fs::read(dir.join(&filename)).unwrap();
        let first = crate::office_edit::edit_in_thread(
            &db,
            &tmp.0,
            "",
            &thread.id,
            &json!({"filename": filename, "replacements": [{"find": "甲", "replace": "丁"}]}),
        )
        .unwrap();
        let chain = first.filename.clone();
        let chain_stem = chain.rsplit_once('.').unwrap().0;
        assert!(chain_stem.chars().count() > 60, "{chain}");
        let continued = crate::office_edit::edit_in_thread(
            &db,
            &tmp.0,
            "",
            &thread.id,
            &json!({
                "filename": chain,
                "new_filename": chain,
                "replacements": [{"find": "乙", "replace": "戊"}]
            }),
        )
        .unwrap();
        assert_eq!(continued.filename, chain);
        assert!(docx_text(&dir.join(&chain)).contains("丁戊"));
        assert_eq!(fs::read(dir.join(&filename)).unwrap(), original);
        let other = format!("{}.docx", "い".repeat(61));
        let rejected = crate::office_edit::edit_in_thread(
            &db,
            &tmp.0,
            "",
            &thread.id,
            &json!({
                "filename": chain,
                "new_filename": other,
                "replacements": [{"find": "丁", "replace": "己"}]
            }),
        )
        .unwrap_err();
        assert!(rejected.contains("無効"), "{rejected}");
        assert!(docx_text(&dir.join(&chain)).contains("丁戊"));
    }

    #[test]
    fn reserved_names_with_extra_dots_are_rejected() {
        let tmp = Tmp::new();
        let db = crate::db::Db::open(&tmp.0.join("argos.db")).unwrap();
        let dir = output_dir(&tmp.0);
        docx_with(&dir, "メモ.docx", "甲乙");
        let thread = db.create_llm_thread("dots", false).unwrap();
        let before = fs::read(dir.join("メモ.docx")).unwrap();
        for name in ["CON.v2.docx", "aux.v2.docx", "COM1.extra.docx", "lpt2.backup.docx"] {
            let renamed = rename_for_tool(
                &db,
                &tmp.0,
                &json!({"filename": "メモ.docx", "new_filename": name}),
            );
            assert!(renamed.contains("無効"), "{name}: {renamed}");
            let edited = crate::office_edit::edit_in_thread(
                &db,
                &tmp.0,
                "",
                &thread.id,
                &json!({
                    "filename": "メモ.docx",
                    "new_filename": name,
                    "replacements": [{"find": "甲", "replace": "丁"}]
                }),
            )
            .unwrap_err();
            assert!(edited.contains("無効"), "{name}: {edited}");
            assert_eq!(fs::read(dir.join("メモ.docx")).unwrap(), before, "{name}");
            assert!(!dir.join(name).exists(), "{name}");
        }
        let ok = rename_for_tool(
            &db,
            &tmp.0,
            &json!({"filename": "メモ.docx", "new_filename": "付録.v2.docx"}),
        );
        assert!(ok.contains("リネームしました"), "{ok}");
        assert!(dir.join("付録.v2.docx").is_file());
    }

    #[test]
    fn access_denied_is_reported_as_the_file_being_open() {
        let err5 = std::io::Error::from_raw_os_error(5);
        let err32 = std::io::Error::from_raw_os_error(32);
        let err33 = std::io::Error::from_raw_os_error(33);
        let err2 = std::io::Error::from_raw_os_error(2);
        assert!(crate::llm::create_office::is_sharing_or_lock_error(&err5));
        assert!(crate::llm::create_office::is_sharing_or_lock_error(&err32));
        assert!(crate::llm::create_office::is_sharing_or_lock_error(&err33));
        assert!(!crate::llm::create_office::is_sharing_or_lock_error(&err2));
        assert!(crate::llm::create_office::message_is_sharing_or_lock(
            "出力を保存できません（os error 5）。"
        ));
        assert!(!crate::llm::create_office::message_is_sharing_or_lock(
            "出力を保存できません（os error 50）。"
        ));

        let tmp = Tmp::new();
        let db = crate::db::Db::open(&tmp.0.join("argos.db")).unwrap();
        let dir = output_dir(&tmp.0);
        docx_with(&dir, "甲.docx", "甲の本文");
        docx_with(&dir, "先.docx", "先の本文");
        let before = fs::read(dir.join("甲.docx")).unwrap();
        let prior = fs::read(dir.join("先.docx")).unwrap();
        super::fail_next_output_rename_os(2, 5);
        let renamed = rename_for_tool(
            &db,
            &tmp.0,
            &json!({"filename": "甲.docx", "new_filename": "乙.docx"}),
        );
        assert!(renamed.contains("開かれている"), "{renamed}");
        assert!(renamed.contains("Word"), "{renamed}");
        assert!(renamed.contains("閉じて"), "{renamed}");
        assert!(renamed.contains("動かしていません"), "{renamed}");
        assert!(!renamed.contains("os error"), "{renamed}");
        assert_eq!(fs::read(dir.join("甲.docx")).unwrap(), before);
        assert!(!dir.join("乙.docx").exists());

        let thread = db.create_llm_thread("denied", false).unwrap();
        crate::office_edit::fail_next_replace_os_error(5);
        let saved = crate::office_edit::edit_in_thread(
            &db,
            &tmp.0,
            "",
            &thread.id,
            &json!({
                "filename": "甲.docx",
                "new_filename": "先.docx",
                "overwrite": true,
                "replacements": [{"find": "甲", "replace": "丁"}]
            }),
        )
        .unwrap_err();
        assert!(saved.contains("開かれている"), "{saved}");
        assert!(saved.contains("Excel"), "{saved}");
        assert!(saved.contains("閉じて"), "{saved}");
        assert!(saved.contains("そのまま"), "{saved}");
        assert!(!saved.contains("os error"), "{saved}");
        assert_eq!(fs::read(dir.join("甲.docx")).unwrap(), before);
        assert_eq!(fs::read(dir.join("先.docx")).unwrap(), prior);
    }
}

fn docx_names(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.ends_with(".docx") && !name.starts_with('.'))
        .collect();
    names.sort();
    names
}
