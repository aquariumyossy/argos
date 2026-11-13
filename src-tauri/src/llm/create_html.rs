//! HTML files from chat tools, saved in the same output folder as Word and Excel.
//!
//! `create_html` writes a new file. `edit_html` reads an HTML file that is already
//! in that folder and writes a new file beside it. The original is left as it is.
//! Paths are bare file names; containment is `create_office::output_file_path`.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::llm::create_office::{
    file_stem, output_dir, output_file_path, unique_path, OfficeToolResult, SavedOfficeFile,
};
use crate::office_edit::{
    output_filename, unique_dest, FILE_LINE, OFFICE_MARKER, PATH_LINE, SOURCE_LINE,
};

pub const TOOL_CREATE_HTML: &str = "create_html";
pub const TOOL_EDIT_HTML: &str = "edit_html";
pub const LLM_HTML_SENTINEL: &str = "新しい HTML を作るツールがあります";
pub const LLM_HTML_HINT: &str = "\
新しい HTML を作るツールがあります。画面で操作する小さな計算（手数料、歩合、日数、利息など）を求められたときだけ create_html を使い、CSS と JavaScript を含めてよいです。保存したファイル名を回答に書いてください。作り直すときは edit_html を使い、元のファイルは残して新しいファイルを保存します。プレビューから外部への通信はできません。";

const MAX_HTML_BYTES: usize = 512 * 1024;

pub fn tool_schemas() -> Vec<Value> {
    vec![create_schema(), edit_schema()]
}

pub fn exec_create(data_dir: &Path, args: &Value) -> OfficeToolResult {
    match write_html(data_dir, args) {
        Ok(file) => OfficeToolResult {
            message: html_tool_message(&file),
            saved: Some(file),
        },
        Err(message) => OfficeToolResult {
            message,
            saved: None,
        },
    }
}

pub fn exec_edit(data_dir: &Path, args: &Value) -> String {
    let stamp = chrono::Local::now().format("%Y-%m-%d %H-%M-%S").to_string();
    match edit_html_at(data_dir, args, &stamp) {
        Ok(edited) => edited.report,
        Err(message) => message,
    }
}

#[derive(Debug)]
pub struct HtmlEdit {
    pub saved: SavedOfficeFile,
    pub source_name: String,
    pub report: String,
}

pub fn write_html(data_dir: &Path, args: &Value) -> Result<SavedOfficeFile, String> {
    let stem = file_stem(args, "ページ")?;
    let html = require_html(args)?;
    let dir = output_dir(data_dir);
    fs::create_dir_all(&dir).map_err(|err| format!("出力フォルダを作れませんでした（{err}）。"))?;
    let dest = unique_path(&dir, &stem, "html");
    write_contained(&dir, &dest, &html)?;
    saved_file(&dest)
}

pub fn edit_html_at(data_dir: &Path, args: &Value, stamp: &str) -> Result<HtmlEdit, String> {
    let requested = source_name(args)?;
    if !requested.to_ascii_lowercase().ends_with(".html") {
        return Err("編集できるのは出力フォルダの .html だけです。".into());
    }
    let src = output_file_path(data_dir, requested)?;
    let meta = fs::metadata(&src).map_err(|_| "ファイルが見つかりません。".to_string())?;
    if !meta.is_file() {
        return Err("ファイルが見つかりません。".into());
    }
    let source_name = src
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| name.to_ascii_lowercase().ends_with(".html"))
        .ok_or_else(|| "編集できるのは出力フォルダの .html だけです。".to_string())?
        .to_string();
    let html = require_html(args)?;
    let dir = output_dir(data_dir);
    fs::create_dir_all(&dir).map_err(|err| format!("出力フォルダを作れませんでした（{err}）。"))?;
    let filename = output_filename(&src, stamp, "html");
    let dest = unique_dest(&dir, &filename, &src);
    write_contained(&dir, &dest, &html)?;
    let saved = saved_file(&dest)?;
    let report = format_report(&saved.name, &source_name, &dest);
    Ok(HtmlEdit {
        saved,
        source_name,
        report,
    })
}

fn html_tool_message(file: &SavedOfficeFile) -> String {
    format!(
        "HTMLファイル『{}』を Argos の出力フォルダに保存しました。回答にはこのファイル名を書いてください。",
        file.name
    )
}

fn format_report(filename: &str, source_name: &str, path: &Path) -> String {
    format!(
        "{FILE_LINE}{filename}\n{OFFICE_MARKER}\thtml\t{filename}\n{SOURCE_LINE}{source_name}\n{PATH_LINE}{}\n",
        path.display()
    )
}

fn source_name(args: &Value) -> Result<&str, String> {
    let name = args
        .get("filename")
        .and_then(|value| value.as_str())
        .or_else(|| args.get("name").and_then(|value| value.as_str()))
        .unwrap_or("")
        .trim();
    if name.is_empty() {
        return Err("編集する HTML のファイル名を指定してください。".into());
    }
    Ok(name)
}

fn require_html(args: &Value) -> Result<String, String> {
    let html = args
        .get("html")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    if html.contains('\0') || html.trim().is_empty() || !html.contains('<') || !html.contains('>') {
        return Err("HTML の本文を指定してください。".into());
    }
    if html.len() > MAX_HTML_BYTES {
        return Err("HTML が大きすぎます。".into());
    }
    Ok(html.to_string())
}

fn saved_file(path: &Path) -> Result<SavedOfficeFile, String> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "ファイル名を作れませんでした。".to_string())?;
    Ok(SavedOfficeFile {
        kind: "html".into(),
        name: name.to_string(),
        path: path.to_string_lossy().into_owned(),
    })
}

fn write_contained(dir: &Path, dest: &Path, html: &str) -> Result<(), String> {
    let name = dest
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "ファイル名を作れませんでした。".to_string())?;
    if name.is_empty()
        || name.starts_with('.')
        || name.contains('/')
        || name.contains('\\')
        || name.contains("..")
        || !name.to_ascii_lowercase().ends_with(".html")
    {
        return Err("ファイル名を作れませんでした。".into());
    }
    if dest.parent() != Some(dir) {
        return Err("出力フォルダの外には保存できません。".into());
    }
    let tmp = dir.join(format!(".argos-{}.tmp", uuid::Uuid::new_v4()));
    if let Err(err) = fs::write(&tmp, html.as_bytes()) {
        let _ = fs::remove_file(&tmp);
        return Err(format!("ファイルを保存できませんでした（{err}）。"));
    }
    if let Err(err) = fs::rename(&tmp, dest) {
        let _ = fs::remove_file(&tmp);
        return Err(format!("ファイルを保存できませんでした（{err}）。"));
    }
    let canon_dir = fs::canonicalize(dir).map_err(|_| "出力フォルダがありません。".to_string())?;
    let canon_file =
        fs::canonicalize(dest).map_err(|_| "ファイルを保存できませんでした。".to_string())?;
    if !canon_file.starts_with(&canon_dir) {
        let _ = fs::remove_file(dest);
        return Err("出力フォルダの外には保存できません。".into());
    }
    Ok(())
}

fn create_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": TOOL_CREATE_HTML,
            "description": "新しい HTML を作り、Argos の出力フォルダに保存する。手数料・歩合・日数・利息などの小さな計算アプリに使う。CSS と JavaScript を含めてよい。html は完全な文書。プレビューは外部サイト・画像・フォント・スクリプト・通信を読み込まない。既存ファイルは上書きしない。保存したファイル名を回答に書く。",
            "parameters": {
                "type": "object",
                "properties": {
                    "filename": { "type": "string", "description": "拡張子なしのファイル名。例: 手数料計算" },
                    "html": { "type": "string", "description": "完全な HTML。style と script を含めてよい。" }
                },
                "required": ["filename", "html"]
            }
        }
    })
}

fn edit_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": TOOL_EDIT_HTML,
            "description": "出力フォルダにある HTML を編集し、元ファイルは残して新しいファイルを保存する。filename は保存済みのファイル名（例: 手数料計算.html）。パスは渡さない。html は編集後の完全な文書。結果のファイル名を回答に書く。",
            "parameters": {
                "type": "object",
                "properties": {
                    "filename": { "type": "string", "description": "出力フォルダの HTML ファイル名。例: 手数料計算.html" },
                    "html": { "type": "string", "description": "編集後の完全な HTML。" }
                },
                "required": ["filename", "html"]
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::create_office::{output_dir, output_file_path, OUTPUT_DIR_NAME};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct Tmp(PathBuf);

    impl Tmp {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "argos-html-{}-{}",
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

    fn page(body: &str) -> String {
        format!("<!DOCTYPE html><html><body>{body}</body></html>")
    }

    #[test]
    fn create_writes_only_inside_the_output_folder() {
        let tmp = Tmp::new();
        let saved = write_html(
            &tmp.0,
            &json!({
                "filename": r"..\..\秘密",
                "html": page("<p>計算</p>")
            }),
        )
        .unwrap();
        let dir = output_dir(&tmp.0);
        assert_eq!(Path::new(&saved.path).parent().unwrap(), dir);
        assert!(saved.name.ends_with(".html"), "{}", saved.name);
        assert!(
            !saved.name.contains('/') && !saved.name.contains('\\') && !saved.name.contains("..")
        );
        let canon = fs::canonicalize(&saved.path).unwrap();
        assert!(canon.starts_with(fs::canonicalize(&dir).unwrap()));
        assert_eq!(
            fs::read_to_string(&saved.path).unwrap(),
            page("<p>計算</p>")
        );
        let resolved = output_file_path(&tmp.0, &saved.name).unwrap();
        assert_eq!(resolved, canon);
    }

    #[test]
    fn create_collision_adds_the_office_suffix() {
        let tmp = Tmp::new();
        let html = page("<button>1</button>");
        let first = write_html(&tmp.0, &json!({"filename": "手数料", "html": html})).unwrap();
        let second = write_html(&tmp.0, &json!({"filename": "手数料.html", "html": html})).unwrap();
        assert_eq!(first.name, "手数料.html");
        assert_eq!(second.name, "手数料 (2).html");
        assert!(output_file_path(&tmp.0, "手数料.html").is_ok());
    }

    #[test]
    fn create_rejects_empty_and_oversized_html() {
        let tmp = Tmp::new();
        let err = write_html(&tmp.0, &json!({"filename": "a", "html": "ただの文章"})).unwrap_err();
        assert!(err.contains("HTML"), "{err}");
        let big = "a".repeat(MAX_HTML_BYTES);
        let err = write_html(
            &tmp.0,
            &json!({"filename": "a", "html": format!("<p>{big}</p>")}),
        )
        .unwrap_err();
        assert!(err.contains("大きすぎ"), "{err}");
        assert!(!output_dir(&tmp.0).join("a.html").exists());
    }

    #[test]
    fn edit_keeps_the_original_and_uses_the_office_edit_name() {
        let tmp = Tmp::new();
        let original = page("<p>旧</p>");
        let created = write_html(&tmp.0, &json!({"filename": "手数料", "html": original})).unwrap();
        let edited = edit_html_at(
            &tmp.0,
            &json!({"filename": created.name, "html": page("<p>新</p><script>1</script>")}),
            "2026-11-13 02-50-00",
        )
        .unwrap();
        assert_eq!(
            edited.saved.name,
            "手数料_編集済み（2026-11-13 02-50-00）.html"
        );
        assert_eq!(edited.source_name, "手数料.html");
        assert_eq!(fs::read_to_string(&created.path).unwrap(), original);
        assert!(fs::read_to_string(&edited.saved.path)
            .unwrap()
            .contains("<p>新</p>"));
        assert!(edited
            .report
            .contains(&format!("{OFFICE_MARKER}\thtml\t{}", edited.saved.name)));
        assert!(edited
            .report
            .contains(&format!("{SOURCE_LINE}手数料.html\n")));
        assert!(!edited.report.lines().any(|line| {
            line.starts_with(OFFICE_MARKER) && (line.contains('\\') || line.contains('/'))
        }));
        let again = edit_html_at(
            &tmp.0,
            &json!({"filename": "手数料.html", "html": page("<p>次</p>")}),
            "2026-11-13 02-50-00",
        )
        .unwrap();
        assert_eq!(
            again.saved.name,
            "手数料_編集済み（2026-11-13 02-50-00）（2）.html"
        );
    }

    #[test]
    fn edit_rejects_paths_outside_the_output_folder() {
        let tmp = Tmp::new();
        let _ = write_html(
            &tmp.0,
            &json!({"filename": "手数料", "html": page("<p>a</p>")}),
        )
        .unwrap();
        for name in [
            r"..\手数料.html",
            "../手数料.html",
            r"C:\Windows\calc.html",
            r"\\server\share\a.html",
            "手数料.docx",
            "notes.txt",
            "",
        ] {
            let err = edit_html_at(
                &tmp.0,
                &json!({"filename": name, "html": page("<p>b</p>")}),
                "2026-11-13 02-50-00",
            )
            .unwrap_err();
            assert!(
                err.contains("ファイル名")
                    || err.contains(".html")
                    || err.contains("出力")
                    || err.contains("見つかりません"),
                "{name}: {err}"
            );
        }
        assert_eq!(
            fs::read_dir(tmp.0.join(OUTPUT_DIR_NAME))
                .unwrap()
                .filter(|entry| {
                    entry
                        .as_ref()
                        .ok()
                        .and_then(|entry| entry.path().extension().map(|ext| ext == "html"))
                        .unwrap_or(false)
                })
                .count(),
            1
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_outside_the_output_folder_is_rejected() {
        let tmp = Tmp::new();
        let dir = output_dir(&tmp.0);
        fs::create_dir_all(&dir).unwrap();
        let outside = tmp.0.join("outside.html");
        fs::write(&outside, page("<p>外</p>")).unwrap();
        std::os::unix::fs::symlink(&outside, dir.join("link.html")).unwrap();
        let err = edit_html_at(
            &tmp.0,
            &json!({"filename": "link.html", "html": page("<p>内</p>")}),
            "2026-11-13 02-50-00",
        )
        .unwrap_err();
        assert!(
            err.contains("外は開けません") || err.contains("ファイル名"),
            "{err}"
        );
        assert_eq!(fs::read_to_string(&outside).unwrap(), page("<p>外</p>"));
    }

    #[test]
    fn notice_names_the_file_and_not_a_path() {
        let file = SavedOfficeFile {
            kind: "html".into(),
            name: "手数料.html".into(),
            path: r"C:\Windows\system32\cmd.exe".into(),
        };
        assert!(crate::llm::create_office::notice_for_tool("read_note", Some(&file)).is_none());
        assert!(crate::llm::create_office::notice_for_tool(
            crate::llm::create_office::TOOL_CREATE_DOCX,
            Some(&file)
        )
        .is_some());
        let notice =
            crate::llm::create_office::notice_for_tool(TOOL_CREATE_HTML, Some(&file)).unwrap();
        assert!(notice.contains("HTMLファイルを保存しました"));
        assert!(notice.contains("手数料.html"));
        assert!(!notice.contains("Windows"));
        assert!(!notice.contains("cmd.exe"));
        let tmp = Tmp::new();
        let result = exec_create(&tmp.0, &json!({"filename": "a", "html": page("<p>x</p>")}));
        assert!(result.saved.is_some());
        assert!(!result.message.contains(OFFICE_MARKER));
    }

    #[test]
    fn schema_offers_html_only() {
        let schema = crate::llm::tools::tools_schema(false, false, false, false).to_string();
        assert!(schema.contains(TOOL_CREATE_HTML), "{schema}");
        assert!(schema.contains(TOOL_EDIT_HTML), "{schema}");
        assert!(schema.contains("JavaScript"));
        assert!(!schema.contains("create_py"));
        assert!(!schema.contains("run_python"));
        assert!(crate::llm::tools::counts_toward_tool_round(&[
            TOOL_CREATE_HTML
        ]));
        assert!(crate::llm::tools::counts_toward_tool_round(&[
            TOOL_EDIT_HTML
        ]));
        let sys = super::super::system_for_request(&crate::db::Settings::default());
        assert_eq!(sys.matches(LLM_HTML_SENTINEL).count(), 1, "{sys}");
        let again = super::super::system_for_request(&crate::db::Settings {
            llm_system_prompt: sys,
            ..crate::db::Settings::default()
        });
        assert_eq!(again.matches(LLM_HTML_SENTINEL).count(), 1);
    }
}
