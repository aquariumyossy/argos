//! Edit an attached .docx or .xlsx, or one already in the output folder.
//! An original file is never overwritten. A follow-up edit of a file this chat's
//! `edit_office` already saved replaces that output in place. Search-folder files stay closed.

mod authors;
pub(crate) mod docx;
mod revise;
mod text;
mod xlsx;
mod xmlscan;

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

use crate::db::LlmSourceRow;
use crate::pathutil;

pub use authors::{docx_author_lines, read_docx_authors, DocxAuthors};
pub use authors::{LLM_DOC_AUTHOR_HINT, LLM_DOC_AUTHOR_SENTINEL};

/// Same folder as chat file creation (`data_dir/output`).
pub const OUTPUT_DIR_NAME: &str = "output";
pub const FILE_LINE: &str = "出力ファイル: ";
pub const PATH_LINE: &str = "保存先: ";
/// Bare name of the attachment that was edited. The chat card shows this, never a path.
pub const SOURCE_LINE: &str = "元ファイル: ";
/// Third field is a bare file name. The chat card passes that name, never a path.
pub const OFFICE_MARKER: &str = "[[argos-office]]";
/// Shown when this chat's earlier `edit_office` output is replaced in place.
const OVERWRITE_LINE: &str = "前回の編集済みファイルに上書きした";
/// Shown when that file is open, so the edit is saved as a new file instead.
const OPEN_FILE_LINE: &str = "前回のファイルが開かれていたため、新しいファイルに保存した";
const EDIT_MARK: &str = "_編集済み（";
const RENAME_ATTEMPTS: u32 = 3;
const RENAME_PAUSE: Duration = Duration::from_millis(150);
const MAX_FILE_BYTES: u64 = 40 * 1024 * 1024;
const MAX_UNPACKED_BYTES: usize = 80 * 1024 * 1024;
const MAX_REPLACEMENTS: usize = 40;
const MAX_PARAGRAPHS: usize = 20;
const MAX_FIND_CHARS: usize = 500;
const MAX_REPLACE_CHARS: usize = 2_000;
const MAX_PARAGRAPH_CHARS: usize = 8_000;
pub(crate) const MAX_LEFTOVERS: usize = 30;

#[derive(Debug, Clone)]
pub struct Replacement {
    pub find: String,
    pub replace: String,
    /// Empty when this replacement has no comment.
    pub comment: String,
}

#[derive(Debug, Clone)]
pub struct ParagraphEdit {
    pub match_text: String,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct EditRequest {
    pub replacements: Vec<Replacement>,
    pub paragraphs: Vec<ParagraphEdit>,
    /// Write each docx replacement as `w:del` + `w:ins` instead of changing the text.
    pub track_changes: bool,
    /// Author stored on those revisions and comments. Never empty (`Argos` when unset).
    pub revision_author: String,
    /// `w:date` for those revisions and comments. Empty until the edit starts.
    pub revision_date: String,
    /// `dc:creator` and `cp:lastModifiedBy`. Unchanged unless the tool asked.
    pub(crate) core_authors: authors::CoreAuthorEdit,
}

/// Blank settings use this author on revisions and comments.
pub fn office_revision_author(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        "Argos".into()
    } else {
        trimmed.to_string()
    }
}

#[derive(Debug)]
pub struct EditOutcome {
    pub filename: String,
    pub path: PathBuf,
    pub report: String,
    pub(crate) save: SaveNotice,
}

#[derive(Debug, Clone)]
pub(crate) struct Probe {
    pub find: String,
    pub explain: String,
}

#[derive(Debug, Clone)]
pub(crate) struct Leftover {
    pub find: String,
    pub location: String,
    pub snippet: String,
    /// `snippet` is the source string itself and can be pasted into the next find.
    pub pasteable: bool,
    pub ws_note: String,
}

#[derive(Debug)]
pub(crate) struct ChangeStats {
    pub hits: Vec<(String, String, usize)>,
    pub paragraphs: usize,
    pub notes: Vec<String>,
    pub paragraph_checks: Vec<(String, String)>,
    /// Parallel to `hits`: source slices matched by whitespace, not by exact text.
    pub ws_sources: Vec<Vec<String>>,
    /// Parallel to `hits`: how many of `hits` were exact.
    pub exact_hits: Vec<usize>,
    /// Parallel to `hits`: pasteable near-misses for a find that missed entirely.
    pub near_misses: Vec<Vec<text::NearMiss>>,
    /// `dc:creator` or `cp:lastModifiedBy` was written to a different value.
    pub core_authors: bool,
}

impl ChangeStats {
    fn changed(&self) -> bool {
        self.core_authors || self.paragraphs > 0 || self.hits.iter().any(|(_, _, count)| *count > 0)
    }

    pub(crate) fn from_request(req: &EditRequest) -> Self {
        let count = req.replacements.len();
        Self {
            hits: req
                .replacements
                .iter()
                .map(|item| (item.find.clone(), item.replace.clone(), 0))
                .collect(),
            paragraphs: 0,
            notes: Vec::new(),
            paragraph_checks: Vec::new(),
            ws_sources: vec![Vec::new(); count],
            exact_hits: vec![0; count],
            near_misses: vec![Vec::new(); count],
            core_authors: false,
        }
    }
}

pub(crate) fn record_hit(stats: &mut ChangeStats, index: usize, hit: text::ReplaceHit) {
    if index >= stats.hits.len() {
        return;
    }
    stats.hits[index].2 += hit.count;
    if let Some(exact) = stats.exact_hits.get_mut(index) {
        *exact += hit.exact;
    }
    let Some(slot) = stats.ws_sources.get_mut(index) else {
        return;
    };
    for source in hit.sources {
        if !slot.iter().any(|item| item == &source) {
            slot.push(source);
        }
    }
}

/// Apply every replacement to one attribute or property string.
pub(crate) fn replace_value(text: &str, req: &EditRequest, stats: &mut ChangeStats) -> String {
    let mut pieces = vec![text.to_string()];
    for (index, item) in req.replacements.iter().enumerate() {
        let hit = text::replace_pieces_flex(&mut pieces, &item.find, &item.replace);
        record_hit(stats, index, hit);
    }
    pieces.remove(0)
}

/// Run the chat tool. Errors are sentences the model can show or correct.
pub fn edit_from_tool(state: &crate::state::AppState, thread_id: &str, args: &Value) -> String {
    let author = state.settings.read().office_revision_author.clone();
    match edit_in_thread(&state.db, &state.data_dir, &author, thread_id, args) {
        Ok(outcome) => outcome.report,
        Err(message) => message,
    }
}

/// Edit one office file for a chat and remember the file that was written.
pub(crate) fn edit_in_thread(
    db: &crate::db::Db,
    data_dir: &Path,
    revision_author: &str,
    thread_id: &str,
    args: &Value,
) -> Result<EditOutcome, String> {
    let sources = db
        .list_llm_sources(thread_id)
        .map_err(|err| format!("添付を読めません（{err}）。"))?;
    let requested = resolve_edit_source(&sources, data_dir, args)?;
    let mut request = EditRequest::from_args(args)?;
    request.revision_author = office_revision_author(revision_author);
    if request.revision_date.trim().is_empty() {
        request.revision_date = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    }
    let stamp = chrono::Local::now().format("%Y-%m-%d %H-%M-%S").to_string();
    let output_dir = data_dir.join(OUTPUT_DIR_NAME);
    let messages = db
        .list_llm_messages(thread_id)
        .map_err(|err| format!("会話を読めません（{err}）。"))?;
    let prior: Vec<(&str, &str)> = messages
        .iter()
        .map(|message| (message.tool_name.as_str(), message.content.as_str()))
        .collect();
    let edits = db
        .list_llm_office_edits(thread_id)
        .map_err(|err| format!("編集の記録を読めません（{err}）。"))?;
    let fresh = arg_bool(args.get("fresh"));
    let (read_path, overwrite) = if fresh {
        (requested.clone(), false)
    } else {
        edit_target(&requested, &output_dir, &prior, &edits)
    };
    let redirected = !fresh && !same_file(&requested, &read_path);
    let edited_name = read_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut outcome = match edit_path_mode(&read_path, &output_dir, &stamp, &request, overwrite) {
        Ok(outcome) => outcome,
        Err(message) if redirected && message.contains("見つかりません") => {
            return Err(format!(
                "前回の編集済みファイル「{edited_name}」の中を探したが、指定した文字列は見つからなかった。fresh は、利用者が元の版からやり直すと頼んだときだけ指定する。\n{message}"
            ));
        }
        Err(message) if redirected => {
            return Err(format!(
                "前回の編集済みファイル「{edited_name}」を対象にしました。\n{message}"
            ));
        }
        Err(message) => return Err(message),
    };
    if redirected {
        outcome.report.insert_str(
            0,
            &format!("前回の編集済みファイル「{edited_name}」を編集した\n"),
        );
    }
    if let Err(err) = remember_office_edit(db, thread_id, &requested, &read_path, &outcome) {
        outcome
            .report
            .push_str(&format!("編集の記録を保存できませんでした（{err}）。\n"));
    }
    Ok(outcome)
}

pub fn edit_path(
    src: &Path,
    output_dir: &Path,
    stamp: &str,
    request: &EditRequest,
) -> Result<EditOutcome, String> {
    edit_path_mode(src, output_dir, stamp, request, false)
}

/// `overwrite` replaces `src` only when the caller has already decided it is
/// this chat's own `_編集済み` output. The file is written to a temp name in
/// the same folder and renamed into place, so a failed edit leaves `src` as it was.
pub(crate) fn edit_path_mode(
    src: &Path,
    output_dir: &Path,
    stamp: &str,
    request: &EditRequest,
    overwrite: bool,
) -> Result<EditOutcome, String> {
    if request.replacements.is_empty()
        && request.paragraphs.is_empty()
        && !request.core_authors.requested()
    {
        return Err("replacements か paragraphs を指定してください。".into());
    }
    let mut request = request.clone();
    if request.revision_author.trim().is_empty() {
        request.revision_author = office_revision_author("");
    }
    if request.revision_date.trim().is_empty() {
        request.revision_date = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    }
    let request = &request;
    let ext = extension_of(src);
    match ext.as_str() {
        "docx" | "xlsx" => {}
        "doc" | "xls" => {
            return Err(
                "旧形式の .doc / .xls は編集できません。.docx / .xlsx を添付してください。".into(),
            );
        }
        _ => return Err("編集できるのは .docx と .xlsx だけです。".into()),
    }
    if request.core_authors.requested() && ext != "docx" {
        return Err("作成者と最終更新者を変えられるのは Word (.docx) だけです。".into());
    }
    let meta = fs::metadata(src).map_err(|_| "ファイルが見つかりません。".to_string())?;
    if !meta.is_file() {
        return Err("ファイルが見つかりません。".into());
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err("ファイルが大きすぎます。".into());
    }
    let mut entries = read_zip(src)?;
    let stats = match ext.as_str() {
        "docx" => docx::edit(&mut entries, request)?,
        "xlsx" => xlsx::edit(&mut entries, request)?,
        _ => return Err("編集できるのは .docx と .xlsx だけです。".into()),
    };
    if !stats.changed() {
        if request.core_authors.requested()
            && request.replacements.is_empty()
            && request.paragraphs.is_empty()
        {
            return Err(
                "作成者と最終更新者は、指定した内容と同じでした。ファイルは保存していません。"
                    .into(),
            );
        }
        let mut message = String::from("指定した文字列は見つかりませんでした。");
        let detail = format_replacement_lines(&stats);
        if !detail.is_empty() {
            message.push('\n');
            message.push_str(detail.trim_end());
        }
        for note in &stats.notes {
            message.push('\n');
            message.push_str(note.trim_end());
        }
        return Err(message);
    }
    fs::create_dir_all(output_dir).map_err(|err| format!("出力フォルダを作れません（{err}）。"))?;
    let replace_existing = overwrite && is_direct_output_file(src, output_dir);
    let filename = output_filename(src, stamp, &ext);
    let (dest, notice) = if replace_existing {
        match write_zip(src, &entries) {
            Ok(()) => (src.to_path_buf(), SaveNotice::Overwrote),
            Err(PlaceError::Replace(_)) => {
                let dest = unique_dest(output_dir, &filename, src);
                write_zip(&dest, &entries).map_err(PlaceError::message)?;
                (dest, SaveNotice::Opened)
            }
            Err(PlaceError::Write(message)) => return Err(message),
        }
    } else {
        let dest = unique_dest(output_dir, &filename, src);
        write_zip(&dest, &entries).map_err(PlaceError::message)?;
        (dest, SaveNotice::New)
    };
    let written = read_zip(&dest)?;
    let probes = probes_of(request, &stats);
    let (leftovers, truncated) = match ext.as_str() {
        "docx" => docx::check(
            &written,
            &probes,
            request
                .track_changes
                .then_some(request.revision_author.as_str()),
        ),
        _ => xlsx::check(&written, &probes),
    };
    let filename = dest
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or(filename);
    let source_name = src
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(EditOutcome {
        report: format_report(
            &filename,
            &source_name,
            &dest,
            &stats,
            &leftovers,
            truncated,
            notice,
        ),
        filename,
        path: dest,
        save: notice,
    })
}

pub fn resolve_office_path(sources: &[LlmSourceRow], args: &Value) -> Result<String, String> {
    let cited: Vec<&LlmSourceRow> = sources
        .iter()
        .filter(|row| is_office_path(&row.path))
        .collect();
    let office: Vec<&LlmSourceRow> = cited
        .iter()
        .copied()
        .filter(|row| is_attached(row))
        .collect();
    let cite = arg_i64(args.get("cite_no")).filter(|value| *value > 0);
    if let Some(cite_no) = cite {
        let rows: Vec<&LlmSourceRow> = cited
            .iter()
            .copied()
            .filter(|row| row.cite_no == cite_no)
            .collect();
        if rows.is_empty() {
            let any = sources.iter().any(|row| row.cite_no == cite_no);
            if any {
                return Err(format!(
                    "出典 [{cite_no}] は .docx / .xlsx ではありません。"
                ));
            }
            return Err(format!("出典 [{cite_no}] はこの会話にありません。"));
        }
        if rows.iter().all(|row| !is_attached(row)) {
            return Err(not_attached_message());
        }
        let attached: Vec<&LlmSourceRow> =
            rows.into_iter().filter(|row| is_attached(row)).collect();
        return Ok(attached[0].path.clone());
    }
    if office.is_empty() {
        if cited.iter().any(|row| !is_attached(row)) {
            return Err(not_attached_message());
        }
        return Err("この会話に添付された .docx / .xlsx がありません。".into());
    }
    let wanted = first_text(args, &["path", "filename", "file"]);
    if wanted.is_empty() {
        if office.len() == 1 {
            return Ok(office[0].path.clone());
        }
        let names = office
            .iter()
            .map(|row| file_name_of(&row.path))
            .collect::<Vec<_>>()
            .join("、");
        return Err(format!(
            "添付が複数あります。cite_no か filename で指定してください（{names}）。"
        ));
    }
    let wanted_name = file_name_of(&wanted);
    let wanted_key = pathutil::normalize_for_compare(&wanted);
    let name_hits: Vec<&LlmSourceRow> = cited
        .iter()
        .copied()
        .filter(|row| {
            pathutil::normalize_for_compare(&row.path).eq_ignore_ascii_case(&wanted_key)
                || file_name_of(&row.path).eq_ignore_ascii_case(&wanted_name)
        })
        .collect();
    let matched: Vec<&LlmSourceRow> = name_hits
        .iter()
        .copied()
        .filter(|row| is_attached(row))
        .collect();
    if matched.is_empty() {
        if !name_hits.is_empty() {
            return Err(not_attached_message());
        }
        return Err(
            "指定されたファイルはこの会話の添付ではありません。.docx / .xlsx の出典を指定してください。"
                .into(),
        );
    }
    if matched.len() > 1
        && !matched.iter().all(|row| {
            pathutil::normalize_for_compare(&row.path)
                .eq_ignore_ascii_case(&pathutil::normalize_for_compare(&matched[0].path))
        })
    {
        return Err("同じ名前の添付が複数あります。cite_no で指定してください。".into());
    }
    Ok(matched[0].path.clone())
}

/// An explicit file name inside the output folder wins over `cite_no` and over
/// an attachment of the same name. `cite_no` alone still means that attachment.
/// A name and a cite that point at different files are an error. Search-folder
/// files stay closed, and paths that leave the output folder are refused.
pub fn resolve_edit_source(
    sources: &[LlmSourceRow],
    data_dir: &Path,
    args: &Value,
) -> Result<PathBuf, String> {
    let cite = arg_i64(args.get("cite_no")).filter(|value| *value > 0);
    let wanted = first_text(args, &["filename", "file", "path"]);
    if wanted.is_empty() {
        return resolve_office_path(sources, args).map(PathBuf::from);
    }
    if let Ok(path) = output_file_path(data_dir, &wanted) {
        return Ok(path);
    }
    if let Some(cite_no) = cite {
        let cite_args = serde_json::json!({ "cite_no": cite_no });
        return match resolve_office_path(sources, &cite_args) {
            Ok(path) if names_agree(&path, &wanted) => Ok(PathBuf::from(path)),
            Ok(path) => Err(format!(
                "filename「{}」と cite_no [{cite_no}]（{}）は別のファイルです。出力フォルダのファイルは filename だけ、添付は cite_no だけを指定してください。",
                one_line_name(&wanted),
                file_name_of(&path)
            )),
            Err(err) => Err(err),
        };
    }
    match resolve_office_path(sources, args) {
        Ok(path) => Ok(PathBuf::from(path)),
        Err(attach_err) => match output_file_path(data_dir, &wanted) {
            Ok(path) => Ok(path),
            Err(output_err) => Err(prefer_attachment_or_output_error(attach_err, output_err)),
        },
    }
}

fn names_agree(path: &str, wanted: &str) -> bool {
    let wanted_name = file_name_of(wanted);
    if wanted_name.is_empty() {
        return false;
    }
    pathutil::normalize_for_compare(path)
        .eq_ignore_ascii_case(&pathutil::normalize_for_compare(wanted))
        || file_name_of(path).eq_ignore_ascii_case(&wanted_name)
}

fn one_line_name(text: &str) -> String {
    text.replace(['\n', '\r', '\t'], " ")
}

fn prefer_attachment_or_output_error(attach_err: String, output_err: String) -> String {
    if attach_err.contains("検索で見つかった")
        && (output_err.contains("見つかりません")
            || output_err.contains("出力フォルダがありません"))
    {
        return attach_err;
    }
    if output_error_is_final(&output_err) {
        return output_err;
    }
    attach_err
}

fn output_error_is_final(message: &str) -> bool {
    message.contains("ファイル名だけ")
        || message.contains("外は開けません")
        || message.contains("Word か Excel")
        || message.contains("見つかりません")
        || message.contains("出力フォルダがありません")
}

impl EditRequest {
    pub fn from_args(args: &Value) -> Result<Self, String> {
        let mut replacements = Vec::new();
        for item in value_list(args.get("replacements")) {
            let find = first_text(&item, &["find", "from", "old"]);
            let replace = text_or_empty(&item, &["replace", "to", "new"]);
            if find.is_empty() && replace.is_empty() {
                continue;
            }
            let comment = text_or_empty(&item, &["comment", "note"]);
            push_replacement(&mut replacements, find, replace, comment)?;
        }
        if replacements.is_empty() {
            let find = first_text(args, &["find", "from", "old"]);
            if !find.is_empty() {
                push_replacement(
                    &mut replacements,
                    find,
                    text_or_empty(args, &["replace", "to", "new"]),
                    text_or_empty(args, &["comment", "note"]),
                )?;
            }
        }
        let mut paragraphs = Vec::new();
        for item in value_list(args.get("paragraphs")) {
            let match_text = first_text(&item, &["match", "find", "paragraph"]);
            let text = text_or_empty(&item, &["text", "replace", "body"]);
            if match_text.is_empty() && text.is_empty() {
                continue;
            }
            push_paragraph(&mut paragraphs, match_text, text)?;
        }
        let core_authors = authors::core_authors_from_args(args)?;
        if replacements.is_empty() && paragraphs.is_empty() && !core_authors.requested() {
            return Err("replacements か paragraphs を指定してください。".into());
        }
        if replacements.len() > MAX_REPLACEMENTS {
            return Err(format!(
                "文字列置換は1回の呼び出しで {MAX_REPLACEMENTS} 件までです。"
            ));
        }
        if paragraphs.len() > MAX_PARAGRAPHS {
            return Err(format!(
                "段落の書き換えは1回の呼び出しで {MAX_PARAGRAPHS} 件までです。"
            ));
        }
        Ok(Self {
            replacements,
            paragraphs,
            track_changes: arg_bool(args.get("tracked")),
            revision_author: office_revision_author(""),
            revision_date: String::new(),
            core_authors,
        })
    }
}

fn arg_bool(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Bool(flag)) => *flag,
        Some(Value::Number(number)) => number.as_i64() == Some(1),
        Some(Value::String(text)) => matches!(text.trim(), "1" | "true" | "yes" | "on"),
        _ => false,
    }
}

fn push_replacement(
    out: &mut Vec<Replacement>,
    find: String,
    replace: String,
    comment: String,
) -> Result<(), String> {
    if find.is_empty() {
        return Err("置換する文字列 find が空です。".into());
    }
    if find.chars().count() > MAX_FIND_CHARS
        || replace.chars().count() > MAX_REPLACE_CHARS
        || comment.chars().count() > MAX_REPLACE_CHARS
    {
        return Err("置換する文字列が長すぎます。".into());
    }
    if find == replace && comment.trim().is_empty() {
        return Ok(());
    }
    out.push(Replacement {
        find,
        replace,
        comment: comment.trim().to_string(),
    });
    Ok(())
}

fn push_paragraph(
    out: &mut Vec<ParagraphEdit>,
    match_text: String,
    text: String,
) -> Result<(), String> {
    if match_text.is_empty() {
        return Err("段落を特定する match が空です。".into());
    }
    if match_text.chars().count() > MAX_FIND_CHARS || text.chars().count() > MAX_PARAGRAPH_CHARS {
        return Err("段落の文字列が長すぎます。".into());
    }
    out.push(ParagraphEdit { match_text, text });
    Ok(())
}

fn value_list(value: Option<&Value>) -> Vec<Value> {
    let Some(value) = value else {
        return Vec::new();
    };
    if let Some(list) = value.as_array() {
        return list.clone();
    }
    if value.is_object() {
        return vec![value.clone()];
    }
    if let Some(text) = value.as_str() {
        if let Ok(Value::Array(list)) = serde_json::from_str(text) {
            return list;
        }
        if let Ok(item @ Value::Object(_)) = serde_json::from_str::<Value>(text) {
            return vec![item];
        }
    }
    Vec::new()
}

fn first_text(value: &Value, keys: &[&str]) -> String {
    for key in keys {
        if let Some(text) = value.get(*key).and_then(|item| item.as_str()) {
            let text = text.trim();
            if !text.is_empty() {
                return text.to_string();
            }
        }
    }
    String::new()
}

fn text_or_empty(value: &Value, keys: &[&str]) -> String {
    for key in keys {
        if let Some(text) = value.get(*key).and_then(|item| item.as_str()) {
            return text.to_string();
        }
    }
    String::new()
}

fn arg_i64(value: Option<&Value>) -> Option<i64> {
    let value = value?;
    if let Some(number) = value.as_i64() {
        return Some(number);
    }
    if let Some(number) = value.as_u64() {
        return i64::try_from(number).ok();
    }
    value
        .as_str()
        .and_then(|text| text.trim().parse::<i64>().ok())
}

fn probes_of(request: &EditRequest, stats: &ChangeStats) -> Vec<Probe> {
    let mut probes = Vec::new();
    for item in &request.replacements {
        probes.push(Probe {
            find: item.find.clone(),
            explain: item.replace.clone(),
        });
    }
    for (old, new) in &stats.paragraph_checks {
        probes.push(Probe {
            find: old.clone(),
            explain: new.clone(),
        });
    }
    probes
}

pub(crate) fn push_leftover(
    leftovers: &mut Vec<Leftover>,
    truncated: &mut bool,
    probe: &Probe,
    text: &str,
    location: &str,
    limit: usize,
) {
    if *truncated || text.is_empty() || location.is_empty() || probe.find.is_empty() {
        return;
    }
    for start in text::unexplained_starts(text, &probe.find, &probe.explain) {
        if leftovers.len() >= limit {
            *truncated = true;
            return;
        }
        leftovers.push(Leftover {
            find: probe.find.clone(),
            location: location.to_string(),
            snippet: text::snippet(text, start, &probe.find),
            pasteable: false,
            ws_note: String::new(),
        });
    }
    for slice in text::ws_variant_slices(text, &probe.find, &probe.explain) {
        if leftovers.len() >= limit {
            *truncated = true;
            return;
        }
        if leftovers.iter().any(|item| {
            item.pasteable
                && item.find == probe.find
                && item.location == location
                && item.snippet == slice
        }) {
            continue;
        }
        leftovers.push(Leftover {
            find: probe.find.clone(),
            location: location.to_string(),
            snippet: slice.clone(),
            pasteable: true,
            ws_note: text::variant_note(&probe.find, &slice),
        });
    }
}

fn is_attached(row: &LlmSourceRow) -> bool {
    row.origin.eq_ignore_ascii_case("attach")
}

fn not_attached_message() -> String {
    "検索で見つかったファイルは編集できません。この会話に添付した .docx / .xlsx を指定してください。"
        .into()
}

/// Resolve a bare `.docx` or `.xlsx` name inside `data_dir/output`.
///
/// The containment rules, including a symlinked output folder, live in
/// `create_office::output_file_path`.
pub fn output_file_path(data_dir: &Path, name: &str) -> Result<PathBuf, String> {
    bare_output_name(name)?;
    crate::llm::create_office::output_file_path(data_dir, name)
}

/// `canonicalize` on Windows returns `\\?\C:\...` or `\\?\UNC\server\...`.
/// Drop that prefix after the containment check, immediately before opening.
pub fn strip_verbatim_prefix(path: &Path) -> PathBuf {
    PathBuf::from(strip_verbatim_text(&path.to_string_lossy()))
}

fn strip_verbatim_text(path: &str) -> String {
    if starts_with_ignore_ascii(path, r"\\?\UNC\") {
        return format!(r"\\{}", &path["\\\\?\\UNC\\".len()..]);
    }
    if starts_with_ignore_ascii(path, r"\\?\") {
        return path[r"\\?\".len()..].to_string();
    }
    path.to_string()
}

fn starts_with_ignore_ascii(text: &str, prefix: &str) -> bool {
    text.len() >= prefix.len()
        && text.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
}

fn bare_output_name(raw: &str) -> Result<&str, String> {
    let name = raw.trim();
    if name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name.contains(':')
        || name.contains('\0')
        || name.contains("..")
        || name.starts_with('.')
        || name.chars().any(|c| c.is_control())
    {
        return Err("ファイル名だけを指定してください。".into());
    }
    let lower = name.to_ascii_lowercase();
    if !lower.ends_with(".docx") && !lower.ends_with(".xlsx") {
        return Err("Word か Excel のファイル名を指定してください。".into());
    }
    Ok(name)
}

fn format_report(
    filename: &str,
    source_name: &str,
    path: &Path,
    stats: &ChangeStats,
    leftovers: &[Leftover],
    truncated: bool,
    notice: SaveNotice,
) -> String {
    let mut out = String::new();
    out.push_str(FILE_LINE);
    out.push_str(filename);
    out.push('\n');
    let kind = if filename.to_ascii_lowercase().ends_with(".xlsx") {
        "xlsx"
    } else {
        "docx"
    };
    out.push_str(OFFICE_MARKER);
    out.push('\t');
    out.push_str(kind);
    out.push('\t');
    out.push_str(filename);
    out.push('\n');
    match notice {
        SaveNotice::Overwrote => {
            out.push_str(OVERWRITE_LINE);
            out.push('\n');
        }
        SaveNotice::Opened => {
            out.push_str(OPEN_FILE_LINE);
            out.push('\n');
        }
        SaveNotice::New => {}
    }
    let source = one_line(source_name);
    let source = source.trim();
    if !source.is_empty() {
        out.push_str(SOURCE_LINE);
        out.push_str(source);
        out.push('\n');
    }
    out.push_str(PATH_LINE);
    out.push_str(&path.to_string_lossy());
    out.push('\n');
    if leftovers.is_empty() {
        out.push_str("残存: なし\n");
    } else {
        out.push_str("残存: あり\n");
        let mut current = String::new();
        for item in leftovers {
            if item.find != current {
                current = item.find.clone();
                out.push('「');
                out.push_str(&one_line(&item.find));
                out.push_str("」が残っています。\n");
            }
            out.push_str("- ");
            out.push_str(&item.location);
            if item.pasteable {
                // The quoted line is only the source string, so it can be copied into find.
                out.push('\n');
                out.push('「');
                out.push_str(&item.snippet);
                out.push_str("」\n");
                if !item.ws_note.is_empty() {
                    out.push('※');
                    out.push_str(&item.ws_note);
                    out.push('\n');
                }
            } else if !item.snippet.is_empty() {
                out.push('（');
                out.push_str(&one_line(&item.snippet));
                out.push_str("）\n");
            } else {
                out.push('\n');
            }
        }
        if truncated {
            out.push_str("ほかにも残っています。\n");
        }
    }
    out.push_str(&format_replacement_lines(stats));
    if stats.paragraphs > 0 {
        out.push_str(&format!("段落の書き換え: {} 件\n", stats.paragraphs));
    }
    for note in &stats.notes {
        // check() already lists the same unreachable hit in 残存.
        if !leftovers.is_empty() && note.contains("にあります") {
            continue;
        }
        out.push_str(note);
        if !note.ends_with('\n') {
            out.push('\n');
        }
    }
    out
}

fn one_line(text: &str) -> String {
    text.replace(['\n', '\r', '\t'], " ")
}

pub(crate) const UNREACHABLE: &str = "このツールでは変更できない場所";

pub(crate) fn unreachable_location(label: &str) -> String {
    if label.contains(UNREACHABLE) {
        label.to_string()
    } else {
        format!("{label}（{UNREACHABLE}）")
    }
}

/// Charts, diagrams, and other DrawingML parts this tool does not rewrite.
pub(crate) fn unreachable_kind(name: &str) -> Option<&'static str> {
    let path = format!(
        "/{}",
        name.replace('\\', "/")
            .trim_start_matches('/')
            .to_ascii_lowercase()
    );
    if path.contains("/charts/") || path.contains("/chartsheets/") {
        Some("グラフ")
    } else if path.contains("/diagrams/") {
        Some("図")
    } else if path.contains("/drawings/") {
        Some("図形")
    } else {
        None
    }
}

fn format_replacement_lines(stats: &ChangeStats) -> String {
    let mut out = String::new();
    for index in 0..stats.hits.len() {
        let find = stats.hits[index].0.clone();
        let replace = stats.hits[index].1.clone();
        let count = stats.hits[index].2;
        if count == 0 {
            out.push_str(&format!(
                "文字列置換: 「{}」は見つかりませんでした\n",
                one_line(&find)
            ));
            if let Some(misses) = stats.near_misses.get(index) {
                for miss in misses.iter().take(text::NEAR_MISS_LIMIT) {
                    if miss.location.is_empty() {
                        out.push_str("近い文字列:\n");
                    } else {
                        out.push_str("- ");
                        out.push_str(&miss.location);
                        out.push('\n');
                    }
                    out.push('「');
                    out.push_str(&miss.exact);
                    out.push_str("」\n");
                    if !miss.note.is_empty() {
                        out.push('※');
                        out.push_str(&miss.note);
                        out.push('\n');
                    }
                }
            }
        } else {
            let exact = stats.exact_hits.get(index).copied().unwrap_or(0);
            let sources = stats
                .ws_sources
                .get(index)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            if sources.is_empty() {
                out.push_str(&format!(
                    "文字列置換: 「{}」→「{}」を {count} 箇所\n",
                    one_line(&find),
                    one_line(&replace)
                ));
            } else {
                let differed = count.saturating_sub(exact);
                out.push_str(&format!(
                    "文字列置換: 「{}」→「{}」を {count} 箇所（完全一致 {exact}、空白の違い {differed}）\n",
                    one_line(&find),
                    one_line(&replace)
                ));
                out.push_str(&format_ws_line(&find, sources));
            }
        }
    }
    out
}

fn format_ws_line(find: &str, sources: &[String]) -> String {
    let shown = sources.len().min(5);
    let mut line = String::new();
    line.push('『');
    line.push_str(find);
    line.push_str("』→ 原文は");
    for (index, source) in sources.iter().take(shown).enumerate() {
        if index > 0 {
            line.push('、');
        }
        line.push('『');
        line.push_str(source);
        line.push('』');
        let note = text::whitespace_note(source);
        if !note.is_empty() {
            line.push('（');
            line.push_str(&note);
            line.push('）');
        }
    }
    line.push_str("で一致\n");
    line
}

/// Find/replace inside document properties and Word people authors.
///
/// Leaf text and the listed attributes only. Tracked changes and comments are
/// left in the stories that already hold them.
pub(crate) fn edit_doc_props(
    entries: &mut [(String, Vec<u8>)],
    req: &EditRequest,
    stats: &mut ChangeStats,
) {
    if req.replacements.is_empty() {
        return;
    }
    for (name, bytes) in entries.iter_mut() {
        let path = name.replace('\\', "/").to_ascii_lowercase();
        let kind = if path == "docprops/core.xml" || path == "docprops/app.xml" {
            PropKind::Leaves
        } else if path.ends_with("word/people.xml") {
            PropKind::People
        } else {
            continue;
        };
        let Some(xml) = xmlscan::decode_xml_bytes(bytes) else {
            continue;
        };
        let next = match kind {
            PropKind::Leaves => map_property_leaves(&xml, req, stats),
            PropKind::People => map_people(&xml, req, stats),
        };
        if next != xml {
            *bytes = next.into_bytes();
        }
    }
}

enum PropKind {
    Leaves,
    People,
}

/// `dc:creator` and `cp:lastModifiedBy` in the core properties part.
pub(crate) fn is_document_author_leaf(part: &str, local: &str) -> bool {
    let path = part.replace('\\', "/").trim_start_matches('/').to_ascii_lowercase();
    path == "docprops/core.xml" && matches!(local, "creator" | "lastModifiedBy")
}

/// Body replacement does not rewrite these two fields. When one still contains
/// a replaced-from string, add one line. The field itself stays as it was.
pub(crate) fn note_remaining_authors(
    entries: &[(String, Vec<u8>)],
    req: &EditRequest,
    stats: &mut ChangeStats,
) {
    if req.replacements.is_empty() {
        return;
    }
    let Some(xml) = entries.iter().find_map(|(name, bytes)| {
        let path = name
            .replace('\\', "/")
            .trim_start_matches('/')
            .to_ascii_lowercase();
        if path != "docprops/core.xml" {
            return None;
        }
        xmlscan::decode_xml_bytes(bytes)
    }) else {
        return;
    };
    let leaves = xmlscan::leaf_texts(&xml);
    for (local, label) in [("creator", "作成者欄"), ("lastModifiedBy", "最終更新者欄")] {
        let values: Vec<&str> = leaves
            .iter()
            .filter(|(name, _)| name == local)
            .map(|(_, text)| text.as_str())
            .collect();
        for item in &req.replacements {
            if item.find.is_empty() || !values.iter().any(|value| value.contains(&item.find)) {
                continue;
            }
            let line = format!("{label}に『{}』が残っています。", one_line(&item.find));
            if !stats.notes.iter().any(|note| note == &line) {
                stats.notes.push(line);
            }
        }
    }
}

fn map_property_leaves(xml: &str, req: &EditRequest, stats: &mut ChangeStats) -> String {
    let mut locals = Vec::new();
    for (local, _) in xmlscan::leaf_texts(xml) {
        if !locals.iter().any(|item| item == &local) {
            locals.push(local);
        }
    }
    let mut xml = xml.to_string();
    for local in locals {
        // dc:creator and cp:lastModifiedBy change only through the explicit
        // author arguments. A body replacement of the same name must not
        // rewrite them. Other property leaves stay in the replacement.
        if is_document_author_leaf("docprops/core.xml", &local) {
            continue;
        }
        xml = xmlscan::map_leaf_local(&xml, &local, |value| replace_value(value, req, stats));
    }
    xml
}

fn map_people(xml: &str, req: &EditRequest, stats: &mut ChangeStats) -> String {
    let xml = xmlscan::map_element_attr(xml, "person", "author", |value| {
        replace_value(value, req, stats)
    });
    xmlscan::map_element_attr(&xml, "person", "initials", |value| {
        replace_value(value, req, stats)
    })
}

/// Say where a find, or a whitespace variant of it, still sits in a part this
/// tool cannot rewrite. Runs whether or not the replacement already hit.
pub(crate) fn note_unreachable_misses(entries: &[(String, Vec<u8>)], stats: &mut ChangeStats) {
    let finds: Vec<(String, String)> = stats
        .hits
        .iter()
        .map(|(find, replace, _)| (find.clone(), replace.clone()))
        .collect();
    for (find, explain) in finds {
        if find.is_empty() {
            continue;
        }
        for place in unreachable_places(entries, &find) {
            stats
                .notes
                .push(format!("「{}」は{place}にあります。", one_line(&find)));
        }
        for (place, slice) in unreachable_variants(entries, &find, &explain) {
            let note = text::variant_note(&find, &slice);
            let mut line = format!("「{}」は{place}にあります。\n「{slice}」", one_line(&find));
            if !note.is_empty() {
                line.push('\n');
                line.push('※');
                line.push_str(&note);
            }
            stats.notes.push(line);
        }
    }
}

fn unreachable_places(entries: &[(String, Vec<u8>)], find: &str) -> Vec<String> {
    let mut places = Vec::new();
    for (name, bytes) in entries {
        let Some(xml) = xmlscan::decode_xml_bytes(bytes) else {
            continue;
        };
        if docx::is_word_story(name) {
            for (local, text) in xmlscan::leaf_texts(&xml) {
                if matches!(local.as_str(), "instrText" | "delInstrText") && text.contains(find) {
                    push_place(&mut places, &unreachable_location("フィールド"));
                }
            }
            for (kind, text) in docx::locked_group_texts(&xml) {
                if text.contains(find) {
                    push_place(&mut places, &unreachable_location(kind));
                }
            }
            continue;
        }
        if docx::is_rels(name) {
            for target in docx::hyperlink_targets(&xml) {
                if target.contains(find) {
                    push_place(&mut places, &unreachable_location("ハイパーリンク"));
                }
            }
            continue;
        }
        if let Some(kind) = unreachable_kind(name) {
            let in_text = xmlscan::leaf_texts(&xml)
                .into_iter()
                .any(|(_, text)| text.contains(find));
            if in_text {
                push_place(&mut places, &unreachable_location(kind));
            }
        }
    }
    places
}

fn unreachable_variants(
    entries: &[(String, Vec<u8>)],
    find: &str,
    explain: &str,
) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (name, bytes) in entries {
        let Some(xml) = xmlscan::decode_xml_bytes(bytes) else {
            continue;
        };
        let mut take = |place: &str, text: &str| {
            for slice in text::ws_variant_slices(text, find, explain) {
                if out.iter().any(|(got, seen)| got == place && seen == &slice) {
                    continue;
                }
                out.push((place.to_string(), slice));
            }
        };
        if docx::is_word_story(name) {
            for (local, text) in xmlscan::leaf_texts(&xml) {
                if matches!(local.as_str(), "instrText" | "delInstrText") {
                    take(&unreachable_location("フィールド"), &text);
                }
            }
            for (kind, text) in docx::locked_group_texts(&xml) {
                take(&unreachable_location(kind), &text);
            }
            continue;
        }
        if docx::is_rels(name) {
            for target in docx::hyperlink_targets(&xml) {
                take(&unreachable_location("ハイパーリンク"), &target);
            }
            continue;
        }
        if let Some(kind) = unreachable_kind(name) {
            let place = unreachable_location(kind);
            for (_, text) in xmlscan::leaf_texts(&xml) {
                take(&place, &text);
            }
        }
    }
    out
}

fn push_place(places: &mut Vec<String>, place: &str) {
    if !places.iter().any(|item| item == place) {
        places.push(place.to_string());
    }
}

/// Drop a previous `_編集済み（…）` suffix so a follow-up pass does not stack it.
fn strip_prior_edit_suffix(stem: &str) -> &str {
    match stem.find(EDIT_MARK) {
        Some(index) if index > 0 => stem[..index].trim().trim_end_matches(['.', ' ']),
        _ => stem,
    }
}

pub(crate) fn output_filename(src: &Path, stamp: &str, ext: &str) -> String {
    let stem = src
        .file_stem()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "文書".into());
    let mut safe = String::new();
    for ch in stem.chars() {
        if matches!(ch, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || ch.is_control() {
            safe.push('_');
        } else {
            safe.push(ch);
        }
    }
    let safe = safe.trim().trim_end_matches(['.', ' ']).trim();
    let safe = if safe.is_empty() { "文書" } else { safe };
    let safe = strip_prior_edit_suffix(safe);
    let safe = if safe.is_empty() { "文書" } else { safe };
    let stamp = if stamp.trim().is_empty() {
        "日時"
    } else {
        stamp.trim()
    };
    format!("{safe}_編集済み（{stamp}）.{ext}")
}

pub(crate) fn unique_dest(dir: &Path, filename: &str, src: &Path) -> PathBuf {
    let candidate = dir.join(filename);
    if dest_ok(&candidate, src) {
        return candidate;
    }
    let stem = Path::new(filename)
        .file_stem()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "文書".into());
    let ext = extension_of(Path::new(filename));
    for index in 2..1000 {
        let candidate = dir.join(format!("{stem}（{index}）.{ext}"));
        if dest_ok(&candidate, src) {
            return candidate;
        }
    }
    dir.join(format!("{stem}（{}）.{ext}", uuid::Uuid::new_v4()))
}

fn dest_ok(dest: &Path, src: &Path) -> bool {
    !dest.exists() && !same_path(dest, src)
}

fn same_path(left: &Path, right: &Path) -> bool {
    pathutil::normalize_for_compare(&left.to_string_lossy())
        .eq_ignore_ascii_case(&pathutil::normalize_for_compare(&right.to_string_lossy()))
}

fn read_zip(path: &Path) -> Result<Vec<(String, Vec<u8>)>, String> {
    let file = fs::File::open(path).map_err(|_| "ファイルが見つかりません。".to_string())?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|err| format!("Office ファイルとして読めません（{err}）。"))?;
    let mut entries = Vec::new();
    let mut total = 0usize;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|err| format!("Office ファイルとして読めません（{err}）。"))?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().to_string();
        let mut bytes = Vec::new();
        entry
            .read_to_end(&mut bytes)
            .map_err(|err| format!("Office ファイルとして読めません（{err}）。"))?;
        total = total.saturating_add(bytes.len());
        if total > MAX_UNPACKED_BYTES {
            return Err("ファイルが大きすぎます。".into());
        }
        entries.push((name, bytes));
    }
    Ok(entries)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SaveNotice {
    New,
    Overwrote,
    Opened,
}

enum PlaceError {
    Write(String),
    Replace(String),
}

impl PlaceError {
    fn message(self) -> String {
        match self {
            PlaceError::Write(message) | PlaceError::Replace(message) => message,
        }
    }
}

fn write_zip(path: &Path, entries: &[(String, Vec<u8>)]) -> Result<(), PlaceError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)
        .map_err(|err| PlaceError::Write(format!("出力フォルダを作れません（{err}）。")))?;
    let tmp = parent.join(format!(".argos-{}.tmp", uuid::Uuid::new_v4()));
    if let Err(err) = write_zip_file(&tmp, entries) {
        let _ = fs::remove_file(&tmp);
        return Err(PlaceError::Write(err));
    }
    if let Err(err) = rename_onto(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(PlaceError::Replace(err));
    }
    Ok(())
}

fn rename_onto(tmp: &Path, dest: &Path) -> Result<(), String> {
    let mut last = String::new();
    for attempt in 0..RENAME_ATTEMPTS {
        if forced_replace_failure(dest) {
            last = "出力を保存できません（os error 5）。".into();
        } else {
            match fs::rename(tmp, dest) {
                Ok(()) => return Ok(()),
                Err(err) => last = format!("出力を保存できません（{err}）。"),
            }
        }
        if attempt + 1 < RENAME_ATTEMPTS {
            std::thread::sleep(RENAME_PAUSE);
        }
    }
    Err(last)
}

fn forced_replace_failure(dest: &Path) -> bool {
    #[cfg(test)]
    {
        return take_forced_replace_failure(dest);
    }
    #[cfg(not(test))]
    {
        let _ = dest;
        false
    }
}

#[cfg(test)]
thread_local! {
    static FORCE_REPLACE_FAILURE: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// The next renames onto a file that already exists fail for every retry, as if
/// Word or Excel kept it open. A later save of a new file is unaffected.
#[cfg(test)]
pub(crate) fn fail_next_replace_of_existing_file() {
    fail_replace_of_existing_file_times(RENAME_ATTEMPTS);
}

/// Fail the next `times` renames onto an existing file. One failure is retried.
#[cfg(test)]
pub(crate) fn fail_replace_of_existing_file_times(times: u32) {
    FORCE_REPLACE_FAILURE.with(|left| left.set(times));
}

#[cfg(test)]
fn take_forced_replace_failure(dest: &Path) -> bool {
    FORCE_REPLACE_FAILURE.with(|left| {
        if left.get() > 0 && dest.exists() {
            left.set(left.get() - 1);
            true
        } else {
            false
        }
    })
}

fn write_zip_file(path: &Path, entries: &[(String, Vec<u8>)]) -> Result<(), String> {
    let file = fs::File::create(path).map_err(|err| format!("出力を書き出せません（{err}）。"))?;
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, bytes) in entries {
        zip.start_file(name.as_str(), options)
            .map_err(|err| format!("出力を書き出せません（{err}）。"))?;
        zip.write_all(bytes)
            .map_err(|err| format!("出力を書き出せません（{err}）。"))?;
    }
    zip.finish()
        .map_err(|err| format!("出力を書き出せません（{err}）。"))?;
    Ok(())
}

/// True when `src` is this chat's own `_編集済み` file in the output folder.
///
/// `prior` is `(tool name, tool result)` from that chat only. A file saved by
/// another tool, or named in another chat, does not qualify.
pub(crate) fn replaces_chat_output(src: &Path, output_dir: &Path, prior: &[(&str, &str)]) -> bool {
    let Some(name) = edited_output_name(src, output_dir) else {
        return false;
    };
    prior
        .iter()
        .any(|(tool, content)| *tool == "edit_office" && marker_names_file(content, name))
}

/// Where to read, and whether to write back over that file.
///
/// Passing one of this chat's edited files updates that file. Passing an
/// original updates the newest edited file still on disk that was made from it.
/// A newer edit of a different original does not hide that file.
fn edit_target(
    requested: &Path,
    output_dir: &Path,
    prior: &[(&str, &str)],
    edits: &[crate::db::LlmOfficeEdit],
) -> (PathBuf, bool) {
    if replaces_chat_output(requested, output_dir, prior) {
        return (requested.to_path_buf(), true);
    }
    if let Some(name) = edited_output_name(requested, output_dir) {
        if edits
            .iter()
            .any(|edit| edit.output_name.eq_ignore_ascii_case(name))
        {
            return (requested.to_path_buf(), true);
        }
    }
    for edit in edits.iter().rev() {
        if !edit.output_name.contains(EDIT_MARK) {
            continue;
        }
        let output = output_dir.join(&edit.output_name);
        if !output.is_file() || same_file(requested, &output) {
            continue;
        }
        if same_file(requested, Path::new(&edit.source_path)) {
            return (output, true);
        }
    }
    (requested.to_path_buf(), false)
}

fn edited_output_name<'a>(src: &'a Path, output_dir: &Path) -> Option<&'a str> {
    if !is_direct_output_file(src, output_dir) {
        return None;
    }
    let name = src.file_name().and_then(|item| item.to_str())?;
    name.contains(EDIT_MARK).then_some(name)
}

fn remember_office_edit(
    db: &crate::db::Db,
    thread_id: &str,
    requested: &Path,
    read_path: &Path,
    outcome: &EditOutcome,
) -> Result<(), String> {
    let read_name = read_path
        .file_name()
        .and_then(|item| item.to_str())
        .unwrap_or("");
    let source = path_key(requested);
    let retargeted = match outcome.save {
        SaveNotice::New => false,
        SaveNotice::Overwrote | SaveNotice::Opened => {
            db.retarget_llm_office_edit(thread_id, read_name, &outcome.filename)
                .map_err(|err| err.to_string())?
                > 0
        }
    };
    if !retargeted {
        db.insert_llm_office_edit(thread_id, &outcome.filename, &source)
            .map_err(|err| err.to_string())?;
    }
    Ok(())
}

fn path_key(path: &Path) -> String {
    fs::canonicalize(path)
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_string_lossy().into_owned())
}

fn same_file(left: &Path, right: &Path) -> bool {
    if let (Ok(left), Ok(right)) = (fs::canonicalize(left), fs::canonicalize(right)) {
        return left == right;
    }
    pathutil::normalize_for_compare(&left.to_string_lossy())
        .eq_ignore_ascii_case(&pathutil::normalize_for_compare(&right.to_string_lossy()))
}

fn is_direct_output_file(src: &Path, output_dir: &Path) -> bool {
    let Ok(dir) = fs::canonicalize(output_dir) else {
        return false;
    };
    let Ok(file) = fs::canonicalize(src) else {
        return false;
    };
    file.is_file() && file.parent() == Some(dir.as_path())
}

fn marker_names_file(content: &str, filename: &str) -> bool {
    let prefix = format!("{OFFICE_MARKER}\t");
    content.lines().any(|line| {
        let Some(rest) = line.strip_prefix(&prefix) else {
            return false;
        };
        let Some((_, name)) = rest.split_once('\t') else {
            return false;
        };
        name.eq_ignore_ascii_case(filename)
    })
}

fn extension_of(path: &Path) -> String {
    path.extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

fn is_office_path(path: &str) -> bool {
    matches!(
        file_name_of(path)
            .rsplit('.')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str(),
        "docx" | "xlsx"
    )
}

fn file_name_of(path: &str) -> String {
    path.trim()
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(path)
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests;
