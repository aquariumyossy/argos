//! List and read Word, Excel, and HTML files that already sit in the output folder.
//!
//! The model uses the list (name, kind, modified time) to choose a file. Reading
//! stays inside that folder: bare names only, and both the folder and the file
//! must canonicalize to a path under `data_dir/output`.

use std::fs;
use std::path::Path;
use std::time::SystemTime;

use serde_json::{json, Value};

use crate::llm::create_office::{contained_output_dir, output_dir, output_file_path};

pub const TOOL_LIST_OUTPUT: &str = "list_output_files";
pub const TOOL_READ_OUTPUT: &str = "read_output_file";
pub const LLM_OUTPUT_SENTINEL: &str = "出力フォルダのファイル一覧を見るツールがあります";
pub const LLM_OUTPUT_HINT: &str = "\
出力フォルダのファイル一覧を見るツールがあります。過去に作った Word・Excel・HTML を、添付がなくても list_output_files で名前・種類・更新日時を見て選べます。依頼に合うファイルが一つなら、そのファイル名で read_output_file を呼び、中身を読んでから編集してください。合うファイルが複数あるときは、その候補の名前・種類・更新日時を挙げてどれかを利用者に聞いて、答えがあるまで読んだり編集したりしないでください。Word と Excel は edit_office、HTML は edit_html を使い、元のファイルは残して新しいファイルを保存します。検索フォルダのファイルはこの方法では読んだり編集したりしません。";

const MAX_LISTED: usize = 200;
const MAX_TEXT_CHARS: usize = 80_000;
const MAX_HTML_BYTES: usize = 512 * 1024;
const MAX_OFFICE_BYTES: u64 = 40 * 1024 * 1024;
const EMPTY_LIST: &str = "出力フォルダにファイルはありません。";
const ASK_LINE: &str = "依頼に合うファイルが一つなら、そのファイル名で read_output_file を呼んでください。複数あるなら、候補の名前・種類・更新日時を挙げてどれか聞いてください。";

struct ListedFile {
    name: String,
    kind: &'static str,
    modified: Option<SystemTime>,
}

pub fn tool_schemas() -> Vec<Value> {
    vec![list_schema(), read_schema()]
}

pub fn list_for_tool(data_dir: &Path) -> String {
    match list_output_files(data_dir) {
        Ok(text) => text,
        Err(message) => message,
    }
}

pub fn read_for_tool(data_dir: &Path, args: &Value) -> String {
    match read_output_file(data_dir, args) {
        Ok(text) => text,
        Err(message) => message,
    }
}

fn list_output_files(data_dir: &Path) -> Result<String, String> {
    let dir = output_dir(data_dir);
    if !dir.exists() {
        return Ok(EMPTY_LIST.into());
    }
    let canon_dir = match contained_output_dir(data_dir) {
        Ok(path) => path,
        Err(message) if message.contains('外') => return Err(message),
        Err(_) => return Ok(EMPTY_LIST.into()),
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
        return Ok(EMPTY_LIST.into());
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

fn read_output_file(data_dir: &Path, args: &Value) -> Result<String, String> {
    let name = requested_name(args)?;
    let path = output_file_path(data_dir, name)?;
    let meta = fs::metadata(&path).map_err(|_| "ファイルが見つかりません。".to_string())?;
    if !meta.is_file() {
        return Err("ファイルが見つかりません。".into());
    }
    let kind = kind_label(name)
        .ok_or_else(|| "Word、Excel、HTML のファイル名を指定してください。".to_string())?;
    let html = name.to_ascii_lowercase().ends_with(".html");
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
        "ファイル: {}\n種類: {kind}\n更新日時: {when}\n\n",
        one_line(name)
    );
    if html {
        out.push_str(
            "以下は HTML のソースです。edit_html の html には、このソースだけを渡してください。\n",
        );
    }
    out.push_str(&body);
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
    } else if lower.ends_with(".html") {
        Some("HTML")
    } else {
        None
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

fn list_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": TOOL_LIST_OUTPUT,
            "description": "Argos の出力フォルダにある Word・Excel・HTML を、名前・種類・更新日時（新しい順）で返す。本文は返さない。利用者が「先週の○○の表を直して」のように過去のファイルを指したときは、まずこれを呼ぶ。名前・種類・更新日時が合うファイルが一つなら、そのファイル名で read_output_file を呼ぶ。候補が複数あるときは、それらの名前・種類・更新日時を挙げてどれかを利用者に聞いて、答えがあるまで読んだり編集したりしない。検索フォルダや出力フォルダの外は見ない。",
            "parameters": { "type": "object", "properties": {} }
        }
    })
}

fn read_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": TOOL_READ_OUTPUT,
            "description": "出力フォルダにある Word・Excel・HTML の中身を、添付なしで読む。filename は list_output_files で見たファイル名だけ。パスは渡さない。Word と Excel は本文、HTML はソースを返す。出力フォルダの外、シンボリックリンクで外へ出るパス、検索フォルダのファイルは読めない。対象が一つに決まってから、編集の前に呼ぶ。",
            "parameters": {
                "type": "object",
                "properties": {
                    "filename": { "type": "string", "description": "出力フォルダのファイル名。例: 売上集計.xlsx。パスは渡さない。" }
                },
                "required": ["filename"]
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::create_html::write_html;
    use crate::llm::create_office::{output_dir, write_docx, write_xlsx};
    use std::path::PathBuf;
    use std::time::{Duration, UNIX_EPOCH};

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
        assert_eq!(list_for_tool(&tmp.0), EMPTY_LIST);
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

        let text = list_for_tool(&tmp.0);
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
        let text = list_for_tool(&tmp.0);
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

        let word = read_for_tool(&tmp.0, &json!({"filename": docx.name}));
        assert!(word.contains("りんごが120個"), "{word}");
        assert!(word.contains("種類: Word"), "{word}");
        assert!(word.contains(&docx.name), "{word}");
        assert!(!word.contains(tmp.0.to_str().unwrap()), "{word}");

        let sheet = read_for_tool(&tmp.0, &json!({"filename": xlsx.name}));
        assert!(sheet.contains("一覧"), "{sheet}");
        assert!(sheet.contains("りんご"), "{sheet}");
        assert!(sheet.contains("300"), "{sheet}");
        assert!(sheet.contains("種類: Excel"), "{sheet}");

        let page = read_for_tool(&tmp.0, &json!({"filename": html.name}));
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
            let err = read_for_tool(&tmp.0, &json!({"filename": name}));
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
        let err = read_for_tool(&tmp.0, &json!({"filename": "大きい.html"}));
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

        let listed = list_for_tool(&tmp.0);
        assert!(listed.contains('外'), "{listed}");
        assert!(!listed.contains("秘密.docx"), "{listed}");
        assert!(!listed.contains("外の本文"), "{listed}");
        let read = read_for_tool(&tmp.0, &json!({"filename": "秘密.docx"}));
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
        let listed = list_for_tool(&tmp.0);
        assert!(listed.contains("残す.docx"), "{listed}");
        assert!(!listed.contains("link.docx"), "{listed}");
        assert!(!listed.contains("外の本文"), "{listed}");
        let read = read_for_tool(&tmp.0, &json!({"filename": "link.docx"}));
        assert!(read.contains('外'), "{read}");
        assert!(!read.contains("外の本文"), "{read}");
        assert_eq!(fs::read_to_string(&file).unwrap(), "外の本文");
    }

    #[test]
    fn the_model_is_told_to_pick_one_file_and_ask_when_several_match() {
        let sys = super::super::system_for_request(&crate::db::Settings::default());
        assert_eq!(sys.matches(LLM_OUTPUT_SENTINEL).count(), 1, "{sys}");
        assert!(sys.contains("名前・種類・更新日時"), "{sys}");
        assert!(sys.contains("複数"), "{sys}");
        assert!(sys.contains("聞いて"), "{sys}");
        assert!(sys.contains("read_output_file"), "{sys}");
        assert!(sys.contains("新しいファイルを保存"), "{sys}");
        assert!(sys.contains("検索フォルダ"), "{sys}");
        let again = super::super::system_for_request(&crate::db::Settings {
            llm_system_prompt: sys,
            ..crate::db::Settings::default()
        });
        assert_eq!(again.matches(LLM_OUTPUT_SENTINEL).count(), 1);
        let schema = crate::llm::tools::tools_schema(false, false, false, false).to_string();
        assert!(schema.contains(TOOL_LIST_OUTPUT), "{schema}");
        assert!(schema.contains(TOOL_READ_OUTPUT), "{schema}");
        assert!(schema.contains("更新日時"), "{schema}");
        assert!(schema.contains("候補が複数"), "{schema}");
        assert!(schema.contains("検索フォルダ"), "{schema}");
        assert!(crate::llm::tools::counts_toward_tool_round(&[
            TOOL_LIST_OUTPUT
        ]));
        assert!(crate::llm::tools::counts_toward_tool_round(&[
            TOOL_READ_OUTPUT
        ]));
    }
}
