//! Edit an attached .docx or .xlsx into a new file. The source is only opened for reading.

mod docx;
mod text;
mod xlsx;
mod xmlscan;

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::db::LlmSourceRow;
use crate::pathutil;

/// Same folder as chat file creation (`data_dir/output`).
pub const OUTPUT_DIR_NAME: &str = "output";
pub const FILE_LINE: &str = "出力ファイル: ";
pub const PATH_LINE: &str = "保存先: ";
/// Third field is a bare file name. The chat button passes that name, never a path.
pub const OFFICE_MARKER: &str = "[[argos-office]]";
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
}

#[derive(Debug)]
pub struct EditOutcome {
    pub filename: String,
    pub path: PathBuf,
    pub report: String,
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
}

#[derive(Debug)]
pub(crate) struct ChangeStats {
    pub hits: Vec<(String, String, usize)>,
    pub paragraphs: usize,
    pub notes: Vec<String>,
    pub paragraph_checks: Vec<(String, String)>,
}

impl ChangeStats {
    fn changed(&self) -> bool {
        self.paragraphs > 0 || self.hits.iter().any(|(_, _, count)| *count > 0)
    }
}

/// Run the chat tool. Errors are sentences the model can show or correct.
pub fn edit_from_tool(state: &crate::state::AppState, thread_id: &str, args: &Value) -> String {
    match edit_from_tool_inner(state, thread_id, args) {
        Ok(outcome) => outcome.report,
        Err(message) => message,
    }
}

fn edit_from_tool_inner(
    state: &crate::state::AppState,
    thread_id: &str,
    args: &Value,
) -> Result<EditOutcome, String> {
    let sources = state
        .db
        .list_llm_sources(thread_id)
        .map_err(|err| format!("添付を読めません（{err}）。"))?;
    let path = resolve_office_path(&sources, args)?;
    let request = EditRequest::from_args(args)?;
    let stamp = chrono::Local::now().format("%Y-%m-%d %H-%M-%S").to_string();
    edit_path(
        Path::new(&path),
        &state.data_dir.join(OUTPUT_DIR_NAME),
        &stamp,
        &request,
    )
}

pub fn edit_path(
    src: &Path,
    output_dir: &Path,
    stamp: &str,
    request: &EditRequest,
) -> Result<EditOutcome, String> {
    if request.replacements.is_empty() && request.paragraphs.is_empty() {
        return Err("replacements か paragraphs を指定してください。".into());
    }
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
        let mut message = String::from("指定した文字列は見つかりませんでした。");
        for note in &stats.notes {
            message.push('\n');
            message.push_str(note);
        }
        return Err(message);
    }
    fs::create_dir_all(output_dir).map_err(|err| format!("出力フォルダを作れません（{err}）。"))?;
    let filename = output_filename(src, stamp, &ext);
    let dest = unique_dest(output_dir, &filename, src);
    write_zip(&dest, &entries)?;
    let written = read_zip(&dest)?;
    let probes = probes_of(request, &stats);
    let (leftovers, truncated) = match ext.as_str() {
        "docx" => docx::check(&written, &probes),
        _ => xlsx::check(&written, &probes),
    };
    let filename = dest
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or(filename);
    Ok(EditOutcome {
        report: format_report(&filename, &dest, &stats, &leftovers, truncated),
        filename,
        path: dest,
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

impl EditRequest {
    pub fn from_args(args: &Value) -> Result<Self, String> {
        let mut replacements = Vec::new();
        for item in value_list(args.get("replacements")) {
            let find = first_text(&item, &["find", "from", "old"]);
            let replace = text_or_empty(&item, &["replace", "to", "new"]);
            if find.is_empty() && replace.is_empty() {
                continue;
            }
            push_replacement(&mut replacements, find, replace)?;
        }
        if replacements.is_empty() {
            let find = first_text(args, &["find", "from", "old"]);
            if !find.is_empty() {
                push_replacement(
                    &mut replacements,
                    find,
                    text_or_empty(args, &["replace", "to", "new"]),
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
        if replacements.is_empty() && paragraphs.is_empty() {
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
        })
    }
}

fn push_replacement(
    out: &mut Vec<Replacement>,
    find: String,
    replace: String,
) -> Result<(), String> {
    if find.is_empty() {
        return Err("置換する文字列 find が空です。".into());
    }
    if find.chars().count() > MAX_FIND_CHARS || replace.chars().count() > MAX_REPLACE_CHARS {
        return Err("置換する文字列が長すぎます。".into());
    }
    if find == replace {
        return Ok(());
    }
    out.push(Replacement { find, replace });
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

/// Resolve a bare file name inside `data_dir/output`.
/// Rejects separators, `..`, drive letters, and UNC names, then checks the
/// canonical path stays inside that folder.
pub fn output_file_path(data_dir: &Path, name: &str) -> Result<PathBuf, String> {
    let name = bare_output_name(name)?;
    let dir = data_dir.join(OUTPUT_DIR_NAME);
    if !dir.is_dir() {
        return Err("出力フォルダがありません。".into());
    }
    let joined = dir.join(name);
    if joined.parent() != Some(dir.as_path())
        || joined.file_name().and_then(|n| n.to_str()) != Some(name)
    {
        return Err("ファイル名だけを指定してください。".into());
    }
    let canon_dir = fs::canonicalize(&dir).map_err(|_| "出力フォルダがありません。".to_string())?;
    let canon_file =
        fs::canonicalize(&joined).map_err(|_| "ファイルが見つかりません。".to_string())?;
    if !canon_file.starts_with(&canon_dir) {
        return Err("出力フォルダの外は開けません。".into());
    }
    Ok(canon_file)
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
    path: &Path,
    stats: &ChangeStats,
    leftovers: &[Leftover],
    truncated: bool,
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
            if !item.snippet.is_empty() {
                out.push('（');
                out.push_str(&one_line(&item.snippet));
                out.push('）');
            }
            out.push('\n');
        }
        if truncated {
            out.push_str("ほかにも残っています。\n");
        }
    }
    for (find, replace, count) in &stats.hits {
        if *count == 0 {
            out.push_str(&format!(
                "文字列置換: 「{}」は見つかりませんでした\n",
                one_line(find)
            ));
        } else {
            out.push_str(&format!(
                "文字列置換: 「{}」→「{}」を {count} 箇所\n",
                one_line(find),
                one_line(replace)
            ));
        }
    }
    if stats.paragraphs > 0 {
        out.push_str(&format!("段落の書き換え: {} 件\n", stats.paragraphs));
    }
    for note in &stats.notes {
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

fn output_filename(src: &Path, stamp: &str, ext: &str) -> String {
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
    let stamp = if stamp.trim().is_empty() {
        "日時"
    } else {
        stamp.trim()
    };
    format!("{safe}_編集済み（{stamp}）.{ext}")
}

fn unique_dest(dir: &Path, filename: &str, src: &Path) -> PathBuf {
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

fn write_zip(path: &Path, entries: &[(String, Vec<u8>)]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| format!("出力フォルダを作れません（{err}）。"))?;
    }
    let tmp = path.with_extension("tmpzip");
    {
        let file =
            fs::File::create(&tmp).map_err(|err| format!("出力を書き出せません（{err}）。"))?;
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
    }
    fs::rename(&tmp, path).map_err(|err| {
        let _ = fs::remove_file(&tmp);
        format!("出力を保存できません（{err}）。")
    })?;
    Ok(())
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
