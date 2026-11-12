use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::json;
use zip::write::SimpleFileOptions;
use zip::ZipWriter;

use super::{
    edit_path, output_file_path, resolve_office_path, strip_verbatim_prefix, EditRequest,
    FILE_LINE, OFFICE_MARKER, OUTPUT_DIR_NAME, PATH_LINE,
};
use crate::db::LlmSourceRow;

fn temp_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("argos-office-{label}-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_zip(path: &Path, files: &[(&str, &str)]) {
    let file = fs::File::create(path).unwrap();
    let mut zip = ZipWriter::new(file);
    let opts = SimpleFileOptions::default();
    for (name, body) in files {
        zip.start_file(*name, opts).unwrap();
        zip.write_all(body.as_bytes()).unwrap();
    }
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

fn source(path: &str, cite: i64) -> LlmSourceRow {
    LlmSourceRow {
        id: format!("id{cite}"),
        thread_id: "t".into(),
        sort_order: cite,
        origin: "attach".into(),
        path: path.into(),
        title: super::file_name_of(path),
        paragraph_id: String::new(),
        body: String::new(),
        query: String::new(),
        created_at: 0,
        grain: "file".into(),
        unit_body: String::new(),
        injected_user_message_id: String::new(),
        cited_assistant_message_id: String::new(),
        cite_no: cite,
        kind: "text".into(),
        stored_relpath: String::new(),
        ocr_status: String::new(),
    }
}

fn request(replacements: &[(&str, &str)], paragraphs: &[(&str, &str)]) -> EditRequest {
    EditRequest::from_args(&json!({
        "replacements": replacements.iter().map(|(find, replace)| json!({"find": find, "replace": replace})).collect::<Vec<_>>(),
        "paragraphs": paragraphs.iter().map(|(match_text, text)| json!({"match": match_text, "text": text})).collect::<Vec<_>>(),
    }))
    .unwrap()
}

const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

fn word_document() -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W}">
  <w:body>
    <w:p>
      <w:r><w:rPr><w:rFonts w:ascii="MS Mincho" w:eastAsia="ＭＳ 明朝"/></w:rPr><w:t>原告は</w:t></w:r>
      <w:r><w:rPr><w:b/></w:rPr><w:t>山</w:t></w:r>
      <w:r><w:t>田太郎</w:t></w:r>
      <w:r><w:t>である。</w:t></w:r>
    </w:p>
    <w:tbl>
      <w:tr><w:tc><w:p><w:r><w:t>表の中の山田太郎</w:t></w:r></w:p></w:tc></w:tr>
    </w:tbl>
    <w:p>
      <w:pPr><w:jc w:val="both"/></w:pPr>
      <w:r><w:rPr><w:sz w:val="28"/></w:rPr><w:t>この段落だけを新しい文にする。</w:t></w:r>
    </w:p>
    <w:p><w:r><w:t>残す段落。事件番号は変わらない。</w:t></w:r></w:p>
    <w:p>
      <w:del w:id="1" w:author="審査" w:date="2024-01-01T00:00:00Z"><w:r><w:delText>山</w:delText></w:r><w:r><w:delText>田太郎を削除</w:delText></w:r></w:del>
      <w:ins w:id="2" w:author="審査" w:date="2024-01-01T00:00:00Z"><w:r><w:t>山</w:t></w:r><w:r><w:t>田太郎を挿入</w:t></w:r></w:ins>
    </w:p>
    <w:p>
      <w:r><w:t>前文</w:t></w:r>
      <w:r><w:pict><w:txbxContent><w:p><w:r><w:t>枠</w:t></w:r><w:r><w:t>内の山田太郎</w:t></w:r></w:p></w:txbxContent></w:pict></w:r>
      <w:r><w:t>後文</w:t></w:r>
    </w:p>
  </w:body>
</w:document>"#
    )
}

fn header_xml() -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:hdr xmlns:w="{W}"><w:p><w:r><w:t>ヘッダー山</w:t></w:r><w:r><w:t>田太郎</w:t></w:r></w:p></w:hdr>"#
    )
}

fn footer_xml() -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:ftr xmlns:w="{W}"><w:p><w:r><w:t>フッターの山田太郎</w:t></w:r></w:p></w:ftr>"#
    )
}

fn footnotes_xml() -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:footnotes xmlns:w="{W}"><w:footnote w:id="1"><w:p><w:r><w:t>脚注の山田太郎</w:t></w:r></w:p></w:footnote></w:footnotes>"#
    )
}

fn endnotes_xml() -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:endnotes xmlns:w="{W}"><w:endnote w:id="1"><w:p><w:r><w:t>文末脚注の山</w:t></w:r><w:r><w:t>田太郎</w:t></w:r></w:p></w:endnote></w:endnotes>"#
    )
}

fn comments_xml() -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:comments xmlns:w="{W}">
  <w:comment w:id="0" w:author="山田太郎" w:date="2024-01-15T00:00:00Z">
    <w:p><w:r><w:t>コメント山</w:t></w:r><w:r><w:t>田太郎</w:t></w:r></w:p>
  </w:comment>
</w:comments>"#
    )
}

fn core_xml() -> &'static str {
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/">
  <dc:title>山田太郎事件</dc:title>
  <dc:creator>山田太郎</dc:creator>
</cp:coreProperties>"#
}

#[test]
fn docx_edits_split_runs_stories_and_reports_property_leftovers() {
    let dir = temp_dir("docx");
    let src = dir.join("契約書.docx");
    write_zip(
        &src,
        &[
            ("word/document.xml", &word_document()),
            ("word/header1.xml", &header_xml()),
            ("word/footer1.xml", &footer_xml()),
            ("word/footnotes.xml", &footnotes_xml()),
            ("word/endnotes.xml", &endnotes_xml()),
            ("word/comments.xml", &comments_xml()),
            ("docProps/core.xml", core_xml()),
        ],
    );
    let before = fs::read(&src).unwrap();
    let out_dir = dir.join("out");
    let req = request(
        &[("山田太郎", "佐藤花子")],
        &[("この段落だけを新しい文にする。", "書き換えた本文です。")],
    );
    let edited = edit_path(&src, &out_dir, "2026-11-12 09-30-00", &req).unwrap();
    assert_eq!(fs::read(&src).unwrap(), before, "original bytes must stay");
    assert_eq!(
        edited.filename,
        "契約書_編集済み（2026-11-12 09-30-00）.docx"
    );
    assert!(edited.path.starts_with(&out_dir));
    assert!(edited.report.starts_with(&format!(
        "{FILE_LINE}契約書_編集済み（2026-11-12 09-30-00）.docx\n{OFFICE_MARKER}\tdocx\t契約書_編集済み（2026-11-12 09-30-00）.docx\n"
    )));
    assert!(edited
        .report
        .contains(&format!("{PATH_LINE}{}", edited.path.display())));
    assert!(edited.report.contains("残存: あり"), "{}", edited.report);
    assert!(
        edited.report.contains("文書のプロパティ"),
        "{}",
        edited.report
    );
    assert!(
        edited.report.contains("段落の書き換え: 1 件"),
        "{}",
        edited.report
    );

    let document = read_entry(&edited.path, "word/document.xml");
    assert!(
        document.contains(r#"w:rFonts w:ascii="MS Mincho" w:eastAsia="ＭＳ 明朝""#),
        "{document}"
    );
    assert!(document.contains("<w:b/>"), "{document}");
    assert!(document.contains("<w:tbl>"), "{document}");
    assert!(document.contains(r#"<w:jc w:val="both"/>"#), "{document}");
    assert!(document.contains(r#"<w:sz w:val="28"/>"#), "{document}");
    assert!(document.contains("佐藤花子"), "{document}");
    assert!(!document.contains("山田太郎"), "{document}");
    assert!(document.contains("書き換えた本文です。"), "{document}");
    assert!(
        document.contains("残す段落。事件番号は変わらない。"),
        "{document}"
    );
    assert!(
        document.contains("<w:delText>佐藤花子</w:delText>"),
        "{document}"
    );
    assert!(
        document.contains("<w:delText>を削除</w:delText>"),
        "{document}"
    );
    assert!(document.contains("<w:t>佐藤花子</w:t>"), "{document}");
    assert!(document.contains("<w:t>を挿入</w:t>"), "{document}");
    assert_eq!(document.matches("<w:ins ").count(), 1, "{document}");
    assert_eq!(document.matches("<w:del ").count(), 1, "{document}");
    assert!(
        !document.contains("<w:ins w:id=\"3\""),
        "must not record a new revision"
    );

    for (name, needle) in [
        ("word/header1.xml", "ヘッダー佐藤花子"),
        ("word/footer1.xml", "フッターの佐藤花子"),
        ("word/footnotes.xml", "脚注の佐藤花子"),
        ("word/endnotes.xml", "文末脚注の佐藤花子"),
        ("word/comments.xml", "コメント佐藤花子"),
    ] {
        let xml = read_entry(&edited.path, name);
        assert!(xml.contains(needle), "{name}: {xml}");
        assert!(!xml.contains("山田太郎"), "{name}: {xml}");
    }
    let comments = read_entry(&edited.path, "word/comments.xml");
    assert!(comments.contains(r#"w:author="佐藤花子""#), "{comments}");
    let core = read_entry(&edited.path, "docProps/core.xml");
    assert!(
        core.contains("山田太郎"),
        "properties stay, and the report names them"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn replacement_that_contains_the_old_text_is_not_a_false_leftover() {
    let dir = temp_dir("explain");
    let src = dir.join("メモ.docx");
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W}"><w:body><w:p><w:r><w:t>山田</w:t></w:r></w:p></w:body></w:document>"#
    );
    write_zip(&src, &[("word/document.xml", &xml)]);
    let edited = edit_path(
        &src,
        &dir.join("out"),
        "2026-11-12 09-30-01",
        &request(&[("山田", "山田太郎")], &[]),
    )
    .unwrap();
    assert!(edited.report.contains("残存: なし"), "{}", edited.report);
    let document = read_entry(&edited.path, "word/document.xml");
    assert!(document.contains("山田太郎"), "{document}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn ambiguous_paragraph_does_not_write() {
    let dir = temp_dir("ambig");
    let src = dir.join("契約書.docx");
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W}"><w:body>
  <w:p><w:r><w:t>同じ文言です。甲。</w:t></w:r></w:p>
  <w:p><w:r><w:t>同じ文言です。乙。</w:t></w:r></w:p>
</w:body></w:document>"#
    );
    write_zip(&src, &[("word/document.xml", &xml)]);
    let out_dir = dir.join("out");
    let err = edit_path(
        &src,
        &out_dir,
        "2026-11-12 09-30-02",
        &request(&[], &[("同じ文言です", "新しい段落")]),
    )
    .unwrap_err();
    assert!(err.contains("2 件"), "{err}");
    assert!(!out_dir.exists() || fs::read_dir(&out_dir).unwrap().next().is_none());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn legacy_doc_is_rejected_without_touching_it() {
    let dir = temp_dir("doc");
    let src = dir.join("旧.doc");
    fs::write(&src, b"not a docx").unwrap();
    let before = fs::read(&src).unwrap();
    let err = edit_path(
        &src,
        &dir.join("out"),
        "2026-11-12 09-30-03",
        &request(&[("a", "b")], &[]),
    )
    .unwrap_err();
    assert!(err.contains(".doc"), "{err}");
    assert_eq!(fs::read(&src).unwrap(), before);
    let _ = fs::remove_dir_all(&dir);
}

fn xlsx_package() -> Vec<(&'static str, String)> {
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
  <Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
  <Override PartName="/xl/worksheets/sheet2.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
  <Override PartName="/xl/sharedStrings.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml"/>
</Types>"#.to_string();
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>
</Relationships>"#.to_string();
    let workbook = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <sheets>
    <sheet name="旧シート" sheetId="1" r:id="rId1"/>
    <sheet name="資料" sheetId="2" r:id="rId2"/>
  </sheets>
  <definedNames>
    <definedName name="先頭">'旧シート'!$A$1</definedName>
  </definedNames>
</workbook>"#.to_string();
    let workbook_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet2.xml"/>
  <Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings" Target="sharedStrings.xml"/>
</Relationships>"#.to_string();
    let shared = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="2" uniqueCount="2">
  <si>
    <r><rPr><rFont val="游ゴシック"/></rPr><t>山</t></r>
    <r><rPr><rFont val="游ゴシック"/></rPr><t>田太郎</t></r>
  </si>
  <si><t>このセルだけを変える</t></si>
</sst>"#
        .to_string();
    let sheet1 = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <sheetData>
    <row r="1">
      <c r="A1" t="s"><v>0</v></c>
      <c r="B1" t="s"><v>1</v></c>
    </row>
    <row r="2">
      <c r="A2"><f>旧シート!B1</f><v>0</v></c>
    </row>
  </sheetData>
</worksheet>"#
        .to_string();
    let sheet2 = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <sheetData>
    <row r="1">
      <c r="C3" t="inlineStr"><is><r><rPr><rFont val="游ゴシック"/></rPr><t>別紙の山</t></r><r><t>田太郎</t></r></is></c>
    </row>
  </sheetData>
</worksheet>"#.to_string();
    let comments = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<comments xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <commentList><comment ref="A1" authorId="0"><text><t>メモの山田太郎</t></text></comment></commentList>
</comments>"#.to_string();
    vec![
        ("[Content_Types].xml", content_types),
        ("_rels/.rels", rels),
        ("xl/workbook.xml", workbook),
        ("xl/_rels/workbook.xml.rels", workbook_rels),
        ("xl/sharedStrings.xml", shared),
        ("xl/worksheets/sheet1.xml", sheet1),
        ("xl/worksheets/sheet2.xml", sheet2),
        ("xl/comments1.xml", comments),
    ]
}

#[test]
fn xlsx_edits_every_sheet_split_runs_and_sheet_names() {
    let dir = temp_dir("xlsx");
    let src = dir.join("一覧.xlsx");
    let files = xlsx_package();
    let borrowed: Vec<(&str, &str)> = files
        .iter()
        .map(|(name, body)| (*name, body.as_str()))
        .collect();
    write_zip(&src, &borrowed);
    let before = fs::read(&src).unwrap();
    let edited = edit_path(
        &src,
        &dir.join("out"),
        "2026-11-12 10-00-00",
        &request(
            &[("山田太郎", "佐藤花子"), ("旧シート", "新シート")],
            &[("このセルだけを変える", "セルを書き換えた")],
        ),
    )
    .unwrap();
    assert_eq!(fs::read(&src).unwrap(), before);
    assert_eq!(edited.filename, "一覧_編集済み（2026-11-12 10-00-00）.xlsx");
    assert!(edited.report.contains("残存: あり"), "{}", edited.report);
    assert!(edited.report.contains("コメント"), "{}", edited.report);
    assert!(
        edited.report.contains("段落の書き換え: 1 件"),
        "{}",
        edited.report
    );

    let shared = read_entry(&edited.path, "xl/sharedStrings.xml");
    assert!(shared.contains(r#"rFont val="游ゴシック""#), "{shared}");
    assert!(shared.contains("佐藤花子"), "{shared}");
    assert!(shared.contains("セルを書き換えた"), "{shared}");
    assert!(!shared.contains("山田太郎"), "{shared}");
    assert!(!shared.contains("このセルだけを変える"), "{shared}");

    let sheet2 = read_entry(&edited.path, "xl/worksheets/sheet2.xml");
    assert!(sheet2.contains("別紙の佐藤花子"), "{sheet2}");
    assert!(sheet2.contains(r#"rFont val="游ゴシック""#), "{sheet2}");
    assert!(!sheet2.contains("山田太郎"), "{sheet2}");

    let workbook = read_entry(&edited.path, "xl/workbook.xml");
    assert!(workbook.contains(r#"name="新シート""#), "{workbook}");
    assert!(!workbook.contains(r#"name="旧シート""#), "{workbook}");
    assert!(
        workbook.contains("新シート!$A$1") || workbook.contains("'新シート'!$A$1"),
        "{workbook}"
    );
    assert!(!workbook.contains("旧シート"), "{workbook}");

    let sheet1 = read_entry(&edited.path, "xl/worksheets/sheet1.xml");
    assert!(
        sheet1.contains("新シート!B1") || sheet1.contains("'新シート'!B1"),
        "{sheet1}"
    );
    assert!(!sheet1.contains("旧シート"), "{sheet1}");

    let comments = read_entry(&edited.path, "xl/comments1.xml");
    assert!(comments.contains("山田太郎"), "{comments}");

    let extracted = crate::extractor::extract_file(&edited.path).expect("calamine reads the xlsx");
    let blob = extracted.pages.join("\n");
    assert!(blob.contains("佐藤花子"), "{blob}");
    assert!(blob.contains("新シート"), "{blob}");
    assert!(blob.contains("セルを書き換えた"), "{blob}");
    assert!(!blob.contains("山田太郎"), "{blob}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn xlsx_cell_rewrite_is_ambiguous_when_two_cells_match() {
    let dir = temp_dir("xlsx-ambig");
    let src = dir.join("表.xlsx");
    let files = xlsx_package();
    let borrowed: Vec<(&str, &str)> = files.iter().map(|(n, b)| (*n, b.as_str())).collect();
    write_zip(&src, &borrowed);
    let err = edit_path(
        &src,
        &dir.join("out"),
        "2026-11-12 10-00-01",
        &request(&[], &[("山田太郎", "別人")]),
    )
    .unwrap_err();
    assert!(err.contains("セル"), "{err}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn resolve_uses_cite_filename_or_the_only_attachment() {
    let rows = vec![
        source(r"C:\cases\契約書.docx", 3),
        source(r"C:\cases\別紙.xlsx", 4),
        source(r"C:\cases\メモ.txt", 5),
    ];
    assert_eq!(
        resolve_office_path(&rows, &json!({"cite_no": 4})).unwrap(),
        r"C:\cases\別紙.xlsx"
    );
    assert_eq!(
        resolve_office_path(&rows, &json!({"filename": "契約書.docx"})).unwrap(),
        r"C:\cases\契約書.docx"
    );
    assert!(resolve_office_path(&rows, &json!({"cite_no": 5}))
        .unwrap_err()
        .contains(".docx"));
    assert!(resolve_office_path(&rows, &json!({}))
        .unwrap_err()
        .contains("複数"));
    let only = vec![source(r"C:\cases\契約書.docx", 1)];
    assert_eq!(
        resolve_office_path(&only, &json!({})).unwrap(),
        r"C:\cases\契約書.docx"
    );
}

#[test]
fn xls_is_rejected() {
    let dir = temp_dir("xls");
    let src = dir.join("旧.xls");
    fs::write(&src, b"xls").unwrap();
    let err = edit_path(
        &src,
        &dir,
        "2026-11-12 10-00-02",
        &request(&[("a", "b")], &[]),
    )
    .unwrap_err();
    assert!(err.contains(".xls"), "{err}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn search_hits_are_not_editable() {
    let mut hit = source(r"C:\cases\契約書.docx", 3);
    hit.origin = "tool".into();
    let by_cite =
        resolve_office_path(std::slice::from_ref(&hit), &json!({"cite_no": 3})).unwrap_err();
    assert!(by_cite.contains("添付"), "{by_cite}");
    let by_name = resolve_office_path(
        std::slice::from_ref(&hit),
        &json!({"filename": "契約書.docx"}),
    )
    .unwrap_err();
    assert!(by_name.contains("添付"), "{by_name}");
    let attached = source(r"C:\cases\契約書.docx", 3);
    assert_eq!(
        resolve_office_path(std::slice::from_ref(&attached), &json!({"cite_no": 3})).unwrap(),
        r"C:\cases\契約書.docx"
    );
}

#[test]
fn output_names_stay_inside_the_output_folder() {
    let dir = temp_dir("open");
    let out = dir.join(OUTPUT_DIR_NAME);
    fs::create_dir_all(&out).unwrap();
    let name = "契約書_編集済み（2026-11-12 09-30-00）.docx";
    fs::write(out.join(name), b"x").unwrap();
    let resolved = output_file_path(&dir, name).unwrap();
    let canon_dir = fs::canonicalize(&out).unwrap();
    assert!(resolved.starts_with(&canon_dir), "{resolved:?}");
    assert_eq!(resolved.file_name().and_then(|n| n.to_str()), Some(name));
    for bad in [
        r"..\secret.docx",
        "../secret.docx",
        r"C:\Windows\cmd.docx",
        r"\\server\share\a.docx",
        "foo/bar.docx",
        r"foo\bar.docx",
        "..",
        ".hidden.docx",
        "a..b.docx",
    ] {
        assert!(output_file_path(&dir, bad).is_err(), "{bad}");
    }
    assert_eq!(
        strip_verbatim_prefix(Path::new(
            r"\\?\C:\Users\a\AppData\Roaming\Argos\output\契約.docx"
        ))
        .to_string_lossy(),
        r"C:\Users\a\AppData\Roaming\Argos\output\契約.docx"
    );
    assert_eq!(
        strip_verbatim_prefix(Path::new(r"\\?\UNC\server\share\Argos\output\a.xlsx"))
            .to_string_lossy(),
        r"\\server\share\Argos\output\a.xlsx"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn symlink_outside_the_output_folder_is_rejected() {
    let dir = temp_dir("link");
    let out = dir.join(OUTPUT_DIR_NAME);
    fs::create_dir_all(&out).unwrap();
    let outside = dir.join("outside.docx");
    fs::write(&outside, b"x").unwrap();
    std::os::unix::fs::symlink(&outside, out.join("link.docx")).unwrap();
    let err = output_file_path(&dir, "link.docx").unwrap_err();
    assert!(err.contains("外"), "{err}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn xlsx_line_break_stays_inside_the_text_node() {
    let dir = temp_dir("xlsx-break");
    let src = dir.join("一覧.xlsx");
    let files = xlsx_package();
    let borrowed: Vec<(&str, &str)> = files
        .iter()
        .map(|(name, body)| (*name, body.as_str()))
        .collect();
    write_zip(&src, &borrowed);
    let edited = edit_path(
        &src,
        &dir.join("out"),
        "2026-11-12 11-00-00",
        &request(&[], &[("このセルだけを変える", "1行目\n2行目")]),
    )
    .unwrap();
    let shared = read_entry(&edited.path, "xl/sharedStrings.xml");
    assert!(!shared.contains("<w:br"), "{shared}");
    assert!(shared.contains("xml:space=\"preserve\""), "{shared}");
    assert!(shared.contains("1行目\n2行目"), "{shared}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn docx_leftover_check_reports_fields_and_hyperlinks_without_rewriting_them() {
    let dir = temp_dir("field");
    let src = dir.join("契約書.docx");
    let document = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W}" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <w:body>
    <w:p><w:r><w:t>本文の山田太郎</w:t></w:r></w:p>
    <w:p>
      <w:r><w:instrText xml:space="preserve"> REF 山田太郎 \h </w:instrText></w:r>
      <w:hyperlink r:id="rId5"><w:r><w:t>表示名</w:t></w:r></w:hyperlink>
    </w:p>
  </w:body>
</w:document>"#
    );
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId5" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://example.com/山田太郎" TargetMode="External"/>
</Relationships>"#;
    write_zip(
        &src,
        &[
            ("word/document.xml", document.as_str()),
            ("word/_rels/document.xml.rels", rels),
        ],
    );
    let edited = edit_path(
        &src,
        &dir.join("out"),
        "2026-11-12 11-30-00",
        &request(&[("山田太郎", "佐藤花子")], &[]),
    )
    .unwrap();
    let document = read_entry(&edited.path, "word/document.xml");
    assert!(document.contains("本文の佐藤花子"), "{document}");
    assert!(document.contains("REF 山田太郎"), "{document}");
    assert!(!document.contains("REF 佐藤花子"), "{document}");
    let rels = read_entry(&edited.path, "word/_rels/document.xml.rels");
    assert!(rels.contains("https://example.com/山田太郎"), "{rels}");
    assert!(edited.report.contains("フィールド"), "{}", edited.report);
    assert!(
        edited.report.contains("ハイパーリンク"),
        "{}",
        edited.report
    );
    let _ = fs::remove_dir_all(&dir);
}
