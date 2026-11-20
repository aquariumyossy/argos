use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::json;
use zip::write::SimpleFileOptions;
use zip::ZipWriter;

use super::{
    edit_path, output_file_path, resolve_edit_source, resolve_office_path, strip_verbatim_prefix,
    EditRequest, FILE_LINE, OFFICE_MARKER, OUTPUT_DIR_NAME, PATH_LINE, SOURCE_LINE,
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

fn zip_contains(path: &Path, name: &str) -> bool {
    let file = fs::File::open(path).unwrap();
    let mut archive = zip::ZipArchive::new(file).unwrap();
    let found = archive.by_name(name).is_ok();
    found
}

fn assert_docx_xml_well_formed(path: &Path) {
    let file = fs::File::open(path).unwrap();
    let mut archive = zip::ZipArchive::new(file).unwrap();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).unwrap();
        let name = entry.name().to_string();
        let lower = name.to_ascii_lowercase();
        if !lower.ends_with(".xml") && !lower.ends_with(".rels") {
            continue;
        }
        let mut xml = String::new();
        std::io::Read::read_to_string(&mut entry, &mut xml).unwrap();
        let mut reader = quick_xml::Reader::from_str(&xml);
        let mut buf = Vec::new();
        loop {
            match reader.read_event_into(&mut buf) {
                Ok(quick_xml::events::Event::Eof) => break,
                Ok(_) => {}
                Err(err) => panic!("{name}: {err}\n{xml}"),
            }
            buf.clear();
        }
    }
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
fn docx_edits_split_runs_stories_comments_and_properties() {
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
    assert!(
        edited
            .report
            .contains(&format!("{SOURCE_LINE}契約書.docx\n")),
        "{}",
        edited.report
    );
    assert!(
        !edited
            .report
            .contains(&format!("{SOURCE_LINE}{}", src.display())),
        "{}",
        edited.report
    );
    assert!(edited
        .report
        .contains(&format!("{PATH_LINE}{}", edited.path.display())));
    assert!(edited.report.contains("残存: なし"), "{}", edited.report);
    assert!(
        !edited.report.contains("文書のプロパティ"),
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
    assert!(core.contains("佐藤花子事件"), "{core}");
    assert!(core.contains("<dc:creator>佐藤花子</dc:creator>"), "{core}");
    assert!(!core.contains("山田太郎"), "{core}");
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
    assert!(
        edited.report.contains(&format!("{SOURCE_LINE}一覧.xlsx\n")),
        "{}",
        edited.report
    );
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
    let chart = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<c:chartSpace xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
  <c:tx><a:t>山田太郎</a:t></c:tx>
</c:chartSpace>"#;
    let diagram = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<dgm:dataModel xmlns:dgm="http://schemas.openxmlformats.org/drawingml/2006/diagram" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
  <a:t>山田太郎</a:t>
</dgm:dataModel>"#;
    let drawing = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:drawing xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
  <a:t>山田太郎</a:t>
</w:drawing>"#;
    write_zip(
        &src,
        &[
            ("word/document.xml", document.as_str()),
            ("word/_rels/document.xml.rels", rels),
            ("word/charts/chart1.xml", chart),
            ("word/diagrams/data1.xml", diagram),
            ("word/drawings/drawing1.xml", drawing),
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
    assert!(
        edited
            .report
            .contains("フィールド（このツールでは変更できない場所）"),
        "{}",
        edited.report
    );
    assert!(
        edited
            .report
            .contains("ハイパーリンク（このツールでは変更できない場所）"),
        "{}",
        edited.report
    );
    let chart = read_entry(&edited.path, "word/charts/chart1.xml");
    assert!(chart.contains("山田太郎"), "{chart}");
    assert!(
        edited
            .report
            .contains("グラフ（このツールでは変更できない場所）"),
        "{}",
        edited.report
    );
    let diagram = read_entry(&edited.path, "word/diagrams/data1.xml");
    assert!(diagram.contains("山田太郎"), "{diagram}");
    assert!(
        edited
            .report
            .contains("図（このツールでは変更できない場所）"),
        "{}",
        edited.report
    );
    let drawing = read_entry(&edited.path, "word/drawings/drawing1.xml");
    assert!(drawing.contains("山田太郎"), "{drawing}");
    assert!(
        edited
            .report
            .contains("図形（このツールでは変更できない場所）"),
        "{}",
        edited.report
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn output_folder_edit_writes_a_new_file_and_leaves_the_original() {
    let dir = temp_dir("keep");
    let out = dir.join(OUTPUT_DIR_NAME);
    fs::create_dir_all(&out).unwrap();
    let src = out.join("売上表.docx");
    let other = out.join("別紙.docx");
    write_zip(&src, &[("word/document.xml", &word_document())]);
    write_zip(&other, &[("word/document.xml", &word_document())]);
    let before = fs::read(&src).unwrap();
    let other_before = fs::read(&other).unwrap();
    let err = resolve_edit_source(&[], &dir, &json!({})).unwrap_err();
    assert!(err.contains("添付"), "{err}");
    let path = resolve_edit_source(&[], &dir, &json!({"filename": "売上表.docx"})).unwrap();
    assert_eq!(
        path.file_name().and_then(|name| name.to_str()),
        Some("売上表.docx")
    );
    let edited = edit_path(
        &path,
        &out,
        "2026-11-10 09-00-00",
        &request(&[("山田太郎", "佐藤花子")], &[]),
    )
    .unwrap();
    assert_eq!(fs::read(&src).unwrap(), before);
    assert_eq!(fs::read(&other).unwrap(), other_before);
    assert_ne!(edited.filename, "売上表.docx");
    assert!(edited.filename.contains("編集済み"), "{}", edited.filename);
    assert!(edited.path.exists());
    assert_ne!(
        fs::canonicalize(&edited.path).unwrap(),
        fs::canonicalize(&src).unwrap()
    );
    assert!(edited
        .report
        .contains(&format!("{OFFICE_MARKER}\tdocx\t{}", edited.filename)));
    assert!(edited
        .report
        .contains(&format!("{SOURCE_LINE}売上表.docx\n")));
    let original = read_entry(&src, "word/document.xml");
    assert!(original.contains("山田太郎"), "{original}");
    assert!(!original.contains("佐藤花子"), "{original}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn output_xlsx_edit_keeps_the_original_bytes() {
    let dir = temp_dir("xlsx-keep");
    let out = dir.join(OUTPUT_DIR_NAME);
    fs::create_dir_all(&out).unwrap();
    let src = out.join("売上表.xlsx");
    let files = xlsx_package();
    let borrowed: Vec<(&str, &str)> = files
        .iter()
        .map(|(name, body)| (*name, body.as_str()))
        .collect();
    write_zip(&src, &borrowed);
    let before = fs::read(&src).unwrap();
    let path = resolve_edit_source(&[], &dir, &json!({"filename": "売上表.xlsx"})).unwrap();
    let edited = edit_path(
        &path,
        &out,
        "2026-11-10 09-00-00",
        &request(&[("山田太郎", "佐藤花子")], &[]),
    )
    .unwrap();
    assert_eq!(fs::read(&src).unwrap(), before);
    assert_ne!(edited.filename, "売上表.xlsx");
    assert!(edited
        .report
        .contains(&format!("{OFFICE_MARKER}\txlsx\t{}", edited.filename)));
    assert!(!read_entry(&src, "xl/sharedStrings.xml").contains("佐藤花子"));
    assert!(read_entry(&edited.path, "xl/sharedStrings.xml").contains("佐藤花子"));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn explicit_filename_prefers_the_output_file_over_cite_and_attachment() {
    let dir = temp_dir("name-wins");
    let out = dir.join(OUTPUT_DIR_NAME);
    fs::create_dir_all(&out).unwrap();
    fs::write(out.join("契約書.docx"), b"output").unwrap();
    fs::write(out.join("売上表.docx"), b"sheet").unwrap();
    let attached = dir.join("契約書.docx");
    fs::write(&attached, b"attached").unwrap();
    let row = source(&attached.to_string_lossy(), 1);
    let rows = std::slice::from_ref(&row);
    let by_name = resolve_edit_source(rows, &dir, &json!({"filename": "契約書.docx"})).unwrap();
    assert_eq!(fs::read(&by_name).unwrap(), b"output");
    let by_cite = resolve_edit_source(rows, &dir, &json!({"cite_no": 1})).unwrap();
    assert_eq!(fs::read(&by_cite).unwrap(), b"attached");
    let filename_over_cite = resolve_edit_source(
        rows,
        &dir,
        &json!({"cite_no": 1, "filename": "売上表.docx"}),
    )
    .unwrap();
    assert_eq!(fs::read(&filename_over_cite).unwrap(), b"sheet");
    let disagree = resolve_edit_source(rows, &dir, &json!({"cite_no": 1, "filename": "ない.docx"}))
        .unwrap_err();
    assert!(disagree.contains("別のファイル"), "{disagree}");
    assert_eq!(fs::read(&attached).unwrap(), b"attached");
    fs::remove_file(out.join("契約書.docx")).unwrap();
    let cite_and_same_name = resolve_edit_source(
        rows,
        &dir,
        &json!({"cite_no": 1, "filename": "契約書.docx"}),
    )
    .unwrap();
    assert_eq!(fs::read(&cite_and_same_name).unwrap(), b"attached");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_search_folder_path_is_not_opened_for_edit() {
    let dir = temp_dir("search");
    let out = dir.join(OUTPUT_DIR_NAME);
    fs::create_dir_all(&out).unwrap();
    fs::write(out.join("契約書.docx"), b"output").unwrap();
    let mut hit = source(r"C:\cases\契約書.docx", 3);
    hit.origin = "tool".into();
    let by_path = resolve_edit_source(
        std::slice::from_ref(&hit),
        &dir,
        &json!({"path": r"C:\cases\契約書.docx"}),
    )
    .unwrap_err();
    assert!(
        by_path.contains("ファイル名") || by_path.contains("検索") || by_path.contains("添付"),
        "{by_path}"
    );
    let by_cite =
        resolve_edit_source(std::slice::from_ref(&hit), &dir, &json!({"cite_no": 3})).unwrap_err();
    assert!(
        by_cite.contains("検索") || by_cite.contains("添付"),
        "{by_cite}"
    );
    let chosen = resolve_edit_source(
        std::slice::from_ref(&hit),
        &dir,
        &json!({"filename": "契約書.docx"}),
    )
    .unwrap();
    assert!(chosen.starts_with(fs::canonicalize(&out).unwrap()));
    assert_eq!(fs::read(&chosen).unwrap(), b"output");
    let traversal =
        resolve_edit_source(&[], &dir, &json!({"filename": r"..\..\契約書.docx"})).unwrap_err();
    assert!(
        traversal.contains("ファイル名") || traversal.contains("外"),
        "{traversal}"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn editing_through_a_symlink_outside_the_output_folder_is_rejected() {
    let dir = temp_dir("edit-link");
    let outside = dir.join("elsewhere");
    fs::create_dir_all(&outside).unwrap();
    let secret = outside.join("売上表.docx");
    fs::write(&secret, b"secret").unwrap();
    std::os::unix::fs::symlink(&outside, dir.join(OUTPUT_DIR_NAME)).unwrap();
    let err = resolve_edit_source(&[], &dir, &json!({"filename": "売上表.docx"})).unwrap_err();
    assert!(err.contains("外"), "{err}");
    assert_eq!(fs::read(&secret).unwrap(), b"secret");

    let _ = fs::remove_file(dir.join(OUTPUT_DIR_NAME));
    let out = dir.join(OUTPUT_DIR_NAME);
    fs::create_dir_all(&out).unwrap();
    let file = dir.join("outside.docx");
    fs::write(&file, b"secret").unwrap();
    std::os::unix::fs::symlink(&file, out.join("link.docx")).unwrap();
    let err = resolve_edit_source(&[], &dir, &json!({"filename": "link.docx"})).unwrap_err();
    assert!(err.contains("外"), "{err}");
    assert_eq!(fs::read(&file).unwrap(), b"secret");
    assert!(!out
        .join("link_編集済み（2026-11-10 09-00-00）.docx")
        .exists());
    let _ = fs::remove_dir_all(&dir);
}

fn word_body(inner: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W}"><w:body>{inner}</w:body></w:document>"#
    )
}

fn inside_quotes(line: &str) -> &str {
    let open = line.find('「').unwrap_or_else(|| panic!("{line}")) + '「'.len_utf8();
    let close = line[open..].find('」').unwrap_or_else(|| panic!("{line}")) + open;
    &line[open..close]
}

fn pasteable_near<'a>(report: &'a str, marker: &str) -> &'a str {
    let lines: Vec<&str> = report.lines().collect();
    for (index, line) in lines.iter().enumerate() {
        let candidate = line.starts_with("近い文字列") || line.starts_with("- ");
        if !candidate {
            continue;
        }
        let quoted = if line.contains('「') {
            *line
        } else {
            lines.get(index + 1).copied().unwrap_or("")
        };
        if quoted.contains(marker) {
            let exact = inside_quotes(quoted);
            assert!(
                !quoted.contains('※'),
                "pasteable line must not carry the note: {quoted}"
            );
            return exact;
        }
    }
    panic!("no near miss containing {marker:?} in {report}");
}

#[test]
fn whitespace_replacements_match_fullwidth_nbsp_and_tab_across_runs() {
    let dir = temp_dir("ws-docx");
    let src = dir.join("名簿.docx");
    let document = word_body(
        r#"<w:p><w:r><w:t>山田</w:t></w:r><w:r><w:t>　太郎</w:t></w:r></w:p>
<w:p><w:r><w:t>A BとA　B</w:t></w:r></w:p>
<w:p><w:r><w:t>列</w:t></w:r><w:r><w:t>&#9;名</w:t></w:r></w:p>
<w:p><w:r><w:t>番</w:t></w:r><w:r><w:t>&#x00A0;号</w:t></w:r></w:p>"#,
    );
    write_zip(&src, &[("word/document.xml", document.as_str())]);
    let edited = edit_path(
        &src,
        &dir.join("out"),
        "2026-11-16 12-00-00",
        &request(
            &[
                ("山田 太郎", "佐藤花子"),
                ("A B", "Z"),
                ("列 名", "項目"),
                ("番 号", "番号"),
            ],
            &[],
        ),
    )
    .unwrap();
    let document = read_entry(&edited.path, "word/document.xml");
    assert!(document.contains("佐藤花子"), "{document}");
    assert!(!document.contains("山田"), "{document}");
    assert!(document.contains("ZとZ"), "{document}");
    assert!(!document.contains("A　B"), "{document}");
    assert!(document.contains("項目"), "{document}");
    assert!(document.contains("番号"), "{document}");
    assert!(!document.contains('\t'), "{document}");
    assert!(!document.contains('\u{00A0}'), "{document}");
    assert!(
        edited
            .report
            .contains("『山田 太郎』→ 原文は『山田　太郎』（全角空白）で一致"),
        "{}",
        edited.report
    );
    assert!(
        edited
            .report
            .contains("『列 名』→ 原文は『列\t名』（タブ）で一致"),
        "{}",
        edited.report
    );
    assert!(
        edited
            .report
            .contains("『番 号』→ 原文は『番\u{00A0}号』（NBSP）で一致"),
        "{}",
        edited.report
    );
    assert!(
        edited
            .report
            .contains("『A B』→ 原文は『A　B』（全角空白）で一致"),
        "{}",
        edited.report
    );
    assert!(
        edited.report.contains("完全一致 1、空白の違い 1"),
        "{}",
        edited.report
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_miss_reports_byte_exact_near_misses_and_an_unreachable_chart() {
    let dir = temp_dir("near");
    let src = dir.join("名簿.docx");
    let document = word_body(
        r#"<w:p><w:r><w:t>前文山田&#x2003;太郎。</w:t></w:r></w:p>
<w:p><w:r><w:t>鈴木一郎です</w:t></w:r></w:p>
<w:p><w:r><w:t>事件</w:t></w:r></w:p>"#,
    );
    let chart = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<c:chartSpace xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
  <c:tx><a:t>残る図表</a:t></c:tx>
</c:chartSpace>"#;
    write_zip(
        &src,
        &[
            ("word/document.xml", document.as_str()),
            ("word/charts/chart1.xml", chart),
        ],
    );
    let edited = edit_path(
        &src,
        &dir.join("out"),
        "2026-11-16 12-00-01",
        &request(
            &[
                ("事件", "案件"),
                ("山田 太郎", "佐藤花子"),
                ("鈴木 一郎", "高橋"),
                ("残る図表", "消す"),
            ],
            &[],
        ),
    )
    .unwrap();
    let document = read_entry(&edited.path, "word/document.xml");
    assert!(document.contains("案件"), "{document}");
    assert!(
        document.contains("&#x2003;") || document.contains('\u{2003}'),
        "{document}"
    );
    assert!(document.contains("鈴木一郎"), "{document}");
    let chart = read_entry(&edited.path, "word/charts/chart1.xml");
    assert!(chart.contains("残る図表"), "{chart}");
    let em = pasteable_near(&edited.report, "\u{2003}");
    assert_eq!(em, "山田\u{2003}太郎");
    assert!(!em.contains("U+"));
    assert!(!em.contains('※'));
    assert_eq!(whitespace_note_follows(&edited.report, em), "※U+2003");
    let missing = pasteable_near(&edited.report, "鈴木一郎");
    assert_eq!(missing, "鈴木一郎");
    assert_eq!(
        whitespace_note_follows(&edited.report, missing),
        "※空白なし"
    );
    assert!(
        edited
            .report
            .contains("グラフ（このツールでは変更できない場所）"),
        "{}",
        edited.report
    );
    assert!(edited.report.contains("残存: あり"), "{}", edited.report);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_total_miss_names_the_exact_string_in_an_unreachable_part() {
    let dir = temp_dir("near-err");
    let src = dir.join("名簿.docx");
    let document = word_body(
        r#"<w:p><w:r><w:t>関係ない</w:t></w:r></w:p><w:p><w:r><w:t>山田&#x2003;太郎</w:t></w:r></w:p>"#,
    );
    let chart = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<c:chartSpace xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
  <c:tx><a:t>山田太郎</a:t></c:tx>
</c:chartSpace>"#;
    write_zip(
        &src,
        &[
            ("word/document.xml", document.as_str()),
            ("word/charts/chart1.xml", chart),
        ],
    );
    let before = fs::read(&src).unwrap();
    let err = edit_path(
        &src,
        &dir.join("out"),
        "2026-11-16 12-00-02",
        &request(&[("山田 太郎", "佐藤花子")], &[]),
    )
    .unwrap_err();
    assert_eq!(fs::read(&src).unwrap(), before);
    assert!(err.contains("見つかりませんでした"), "{err}");
    assert_eq!(pasteable_near(&err, "\u{2003}"), "山田\u{2003}太郎");
    assert_eq!(whitespace_note_follows(&err, "山田\u{2003}太郎"), "※U+2003");
    // The chart's exact "山田太郎" is a compact match of the spaced find too.
    assert_eq!(pasteable_near(&err, "山田太郎"), "山田太郎");
    assert!(
        err.lines().any(|line| {
            line.starts_with("- ")
                && line.contains("グラフ（このツールでは変更できない場所）")
                && !line.contains('「')
        }),
        "{err}"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_zero_hit_near_miss_in_an_unreachable_part_names_that_place() {
    let dir = temp_dir("near-place");
    let src = dir.join("訴状.docx");
    let document = word_body(
        "<w:p><w:r><w:t>関係ない本文</w:t></w:r></w:p>\
<w:p><w:r><w:instrText xml:space=\"preserve\"> REF 山 田 太 郎 \\h </w:instrText></w:r></w:p>",
    );
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId5" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://example.com/山田　太郎" TargetMode="External"/>
</Relationships>"#;
    let chart = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<c:chartSpace xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
  <c:tx><a:t>山　田　太　郎</a:t></c:tx>
</c:chartSpace>"#;
    write_zip(
        &src,
        &[
            ("word/document.xml", document.as_str()),
            ("word/_rels/document.xml.rels", rels),
            ("word/charts/chart1.xml", chart),
        ],
    );
    let err = edit_path(
        &src,
        &dir.join("out"),
        "2026-11-16 14-00-00",
        &request(&[("山田太郎", "佐藤花子")], &[]),
    )
    .unwrap_err();
    assert!(!dir.join("out").exists());
    assert_eq!(
        pasteable_at(&err, "グラフ（このツールでは変更できない場所）"),
        "山\u{3000}田\u{3000}太\u{3000}郎"
    );
    assert_eq!(
        whitespace_note_follows(&err, "山\u{3000}田\u{3000}太\u{3000}郎"),
        "※全角空白"
    );
    assert_eq!(
        pasteable_at(&err, "ハイパーリンク（このツールでは変更できない場所）"),
        "山田\u{3000}太郎"
    );
    assert_eq!(
        pasteable_at(&err, "フィールド（このツールでは変更できない場所）"),
        "山 田 太 郎"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn an_xlsx_zero_hit_near_miss_in_a_chart_names_that_place() {
    let dir = temp_dir("xlsx-near-place");
    let src = dir.join("一覧.xlsx");
    let shared = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><si><t>関係ない</t></si></sst>"#;
    let sheet = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <sheetData><row r="1"><c r="A1" t="s"><v>0</v></c></row></sheetData>
</worksheet>"#;
    let chart = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<c:chartSpace xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
  <c:tx><a:t>山{sp}田{sp}太{sp}郎</a:t></c:tx>
</c:chartSpace>"#,
        sp = "\u{3000}"
    );
    write_zip(
        &src,
        &[
            ("xl/sharedStrings.xml", shared),
            ("xl/worksheets/sheet1.xml", sheet),
            ("xl/charts/chart1.xml", chart.as_str()),
        ],
    );
    let err = edit_path(
        &src,
        &dir.join("out"),
        "2026-11-16 14-00-01",
        &request(&[("山田太郎", "佐藤花子")], &[]),
    )
    .unwrap_err();
    assert!(!dir.join("out").exists());
    assert_eq!(
        pasteable_at(&err, "グラフ（このツールでは変更できない場所）"),
        "山\u{3000}田\u{3000}太\u{3000}郎"
    );
    assert_eq!(
        whitespace_note_follows(&err, "山\u{3000}田\u{3000}太\u{3000}郎"),
        "※全角空白"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn an_exact_miss_that_lives_only_in_a_chart_is_labelled_unreachable() {
    let dir = temp_dir("chart-only");
    let src = dir.join("名簿.docx");
    let document = word_body(r#"<w:p><w:r><w:t>関係ない</w:t></w:r></w:p>"#);
    let chart = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<c:chartSpace xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
  <c:tx><a:t>山田太郎</a:t></c:tx>
</c:chartSpace>"#;
    write_zip(
        &src,
        &[
            ("word/document.xml", document.as_str()),
            ("word/charts/chart1.xml", chart),
        ],
    );
    let err = edit_path(
        &src,
        &dir.join("out"),
        "2026-11-16 12-00-03",
        &request(&[("山田太郎", "佐藤花子")], &[]),
    )
    .unwrap_err();
    assert!(
        err.contains("「山田太郎」はグラフ（このツールでは変更できない場所）にあります。"),
        "{err}"
    );
    assert!(!dir.join("out").exists());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn properties_people_and_initials_are_replaced_and_revisions_stay() {
    let dir = temp_dir("props");
    let src = dir.join("名簿.docx");
    let document = word_body(
        r#"<w:p>
      <w:del w:id="7" w:author="保持する著者" w:date="2024-01-01T00:00:00Z"><w:r><w:delText>削除文</w:delText></w:r></w:del>
      <w:ins w:id="8" w:author="保持する著者" w:date="2024-01-01T00:00:00Z"><w:r><w:t>挿入文</w:t></w:r></w:ins>
    </w:p>"#,
    );
    let comments = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:comments xmlns:w="{W}">
  <w:comment w:id="0" w:author="別人物" w:date="2024-01-15T00:00:00Z" w:initials="山田　太郎">
    <w:p><w:r><w:t>メモ</w:t></w:r></w:p>
  </w:comment>
</w:comments>"#
    );
    let core = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/">
  <dc:title>山田 太郎の文書</dc:title>
  <dc:subject>件名は山田　太郎</dc:subject>
  <dc:creator>山田 太郎</dc:creator>
  <cp:keywords>山田&#x00A0;太郎</cp:keywords>
  <dc:description>説明</dc:description>
  <cp:lastModifiedBy>山田　太郎</cp:lastModifiedBy>
  <cp:revision>3</cp:revision>
</cp:coreProperties>"#;
    let app = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties" xmlns:vt="http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes">
  <Company>山田 太郎</Company>
  <Manager>山田　太郎</Manager>
  <HeadingPairs>
    <vt:vector size="2" baseType="variant">
      <vt:variant><vt:lpstr>ワークシート</vt:lpstr></vt:variant>
      <vt:variant><vt:i4>1</vt:i4></vt:variant>
    </vt:vector>
  </HeadingPairs>
</Properties>"#;
    let people = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w15:people xmlns:w15="http://schemas.microsoft.com/office/word/2012/wordml">
  <w15:person w15:author="山田　太郎" w15:contact="keep-me">
    <w15:presenceInfo w15:providerId="None" w15:userId="user-1"/>
  </w15:person>
</w15:people>"#;
    write_zip(
        &src,
        &[
            ("word/document.xml", document.as_str()),
            ("word/comments.xml", comments.as_str()),
            ("docProps/core.xml", core),
            ("docProps/app.xml", app),
            ("word/people.xml", people),
        ],
    );
    let edited = edit_path(
        &src,
        &dir.join("out"),
        "2026-11-16 12-00-04",
        &request(&[("山田 太郎", "佐藤花子")], &[]),
    )
    .unwrap();
    let document = read_entry(&edited.path, "word/document.xml");
    assert!(document.contains(r#"w:id="7""#), "{document}");
    assert!(document.contains(r#"w:id="8""#), "{document}");
    assert_eq!(document.matches("<w:del ").count(), 1, "{document}");
    assert_eq!(document.matches("<w:ins ").count(), 1, "{document}");
    assert!(
        document.contains("<w:delText>削除文</w:delText>"),
        "{document}"
    );
    assert!(document.contains("<w:t>挿入文</w:t>"), "{document}");
    assert!(
        document.contains(r#"w:author="保持する著者""#),
        "{document}"
    );

    let comments = read_entry(&edited.path, "word/comments.xml");
    assert!(comments.contains(r#"w:author="別人物""#), "{comments}");
    assert!(comments.contains(r#"w:initials="佐藤花子""#), "{comments}");
    assert!(!comments.contains("山田"), "{comments}");

    let core = read_entry(&edited.path, "docProps/core.xml");
    assert!(core.contains("xmlns:dc="), "{core}");
    assert!(
        core.contains("<dc:title>佐藤花子の文書</dc:title>"),
        "{core}"
    );
    assert!(
        core.contains("<dc:subject>件名は佐藤花子</dc:subject>"),
        "{core}"
    );
    assert!(core.contains("<dc:creator>佐藤花子</dc:creator>"), "{core}");
    assert!(
        core.contains("<cp:keywords>佐藤花子</cp:keywords>"),
        "{core}"
    );
    assert!(
        core.contains("<cp:lastModifiedBy>佐藤花子</cp:lastModifiedBy>"),
        "{core}"
    );
    assert!(
        core.contains("<dc:description>説明</dc:description>"),
        "{core}"
    );
    assert!(core.contains("<cp:revision>3</cp:revision>"), "{core}");
    assert!(!core.contains("山田"), "{core}");

    let app = read_entry(&edited.path, "docProps/app.xml");
    assert!(app.contains("<Company>佐藤花子</Company>"), "{app}");
    assert!(app.contains("<Manager>佐藤花子</Manager>"), "{app}");
    assert!(app.contains("<vt:lpstr>ワークシート</vt:lpstr>"), "{app}");
    assert!(app.contains("<vt:vector"), "{app}");
    assert!(!app.contains("山田"), "{app}");

    let people = read_entry(&edited.path, "word/people.xml");
    assert!(people.contains(r#"w15:author="佐藤花子""#), "{people}");
    assert!(people.contains(r#"w15:contact="keep-me""#), "{people}");
    assert!(people.contains(r#"w15:userId="user-1""#), "{people}");
    assert!(!people.contains("山田"), "{people}");

    assert!(edited.report.contains("残存: なし"), "{}", edited.report);
    assert!(
        edited.report.contains("『山田　太郎』（全角空白）"),
        "{}",
        edited.report
    );
    assert!(
        edited.report.contains("『山田\u{00A0}太郎』（NBSP）"),
        "{}",
        edited.report
    );
    assert!(!edited.report.contains("イニシャル（"), "{}", edited.report);
    assert!(
        !edited.report.contains("文書のプロパティ"),
        "{}",
        edited.report
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn xlsx_replacements_follow_whitespace_and_properties_and_mark_charts() {
    let dir = temp_dir("ws-xlsx");
    let src = dir.join("一覧.xlsx");
    let shared = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <si><r><t>山田</t></r><r><t>　太郎</t></r></si>
  <si><t>見出し</t></si>
  <si><r><t>鈴</t></r><r><t>木&#x2003;一郎</t></r></si>
</sst>"#;
    let sheet = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <sheetData>
    <row r="1">
      <c r="A1" t="s"><v>0</v></c>
      <c r="B1" t="inlineStr"><is><r><t>番</t></r><r><t>&#x00A0;号</t></r></is></c>
      <c r="C1" t="inlineStr"><is><t>列&#9;名</t></is></c>
    </row>
  </sheetData>
</worksheet>"#;
    let workbook = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <sheets><sheet name="旧　シート" sheetId="1" r:id="rId1"/></sheets>
</workbook>"#;
    let core = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/">
  <dc:creator>山田　太郎</dc:creator>
</cp:coreProperties>"#;
    let app = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties">
  <Company>山田 太郎</Company>
  <Manager>担当</Manager>
</Properties>"#;
    let chart = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<c:chartSpace xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
  <c:tx><a:t>残る図表</a:t></c:tx>
</c:chartSpace>"#;
    write_zip(
        &src,
        &[
            ("xl/sharedStrings.xml", shared),
            ("xl/worksheets/sheet1.xml", sheet),
            ("xl/workbook.xml", workbook),
            ("docProps/core.xml", core),
            ("docProps/app.xml", app),
            ("xl/charts/chart1.xml", chart),
        ],
    );
    let edited = edit_path(
        &src,
        &dir.join("out"),
        "2026-11-16 12-00-05",
        &request(
            &[
                ("山田 太郎", "佐藤花子"),
                ("番 号", "番号"),
                ("列 名", "項目"),
                ("旧 シート", "新シート"),
                ("見出し", "表題"),
                ("鈴木 一郎", "高橋"),
                ("残る図表", "消す"),
            ],
            &[],
        ),
    )
    .unwrap();
    let shared = read_entry(&edited.path, "xl/sharedStrings.xml");
    assert!(shared.contains("佐藤花子"), "{shared}");
    assert!(shared.contains("表題"), "{shared}");
    assert!(
        shared.contains("木\u{2003}一郎") || shared.contains("&#x2003;"),
        "{shared}"
    );
    assert!(!shared.contains("山田"), "{shared}");
    let sheet = read_entry(&edited.path, "xl/worksheets/sheet1.xml");
    assert!(sheet.contains("番号"), "{sheet}");
    assert!(sheet.contains("項目"), "{sheet}");
    assert!(!sheet.contains('\u{00A0}'), "{sheet}");
    assert!(!sheet.contains('\t'), "{sheet}");
    let workbook = read_entry(&edited.path, "xl/workbook.xml");
    assert!(workbook.contains(r#"name="新シート""#), "{workbook}");
    assert!(!workbook.contains("旧"), "{workbook}");
    let core = read_entry(&edited.path, "docProps/core.xml");
    assert!(core.contains("<dc:creator>佐藤花子</dc:creator>"), "{core}");
    assert!(!core.contains("山田"), "{core}");
    let app = read_entry(&edited.path, "docProps/app.xml");
    assert!(app.contains("<Company>佐藤花子</Company>"), "{app}");
    assert!(app.contains("<Manager>担当</Manager>"), "{app}");
    let chart = read_entry(&edited.path, "xl/charts/chart1.xml");
    assert!(chart.contains("残る図表"), "{chart}");
    assert_eq!(
        pasteable_near(&edited.report, "\u{2003}"),
        "鈴木\u{2003}一郎"
    );
    assert!(
        edited
            .report
            .contains("『山田 太郎』→ 原文は『山田　太郎』（全角空白）で一致"),
        "{}",
        edited.report
    );
    assert!(
        edited
            .report
            .contains("『旧 シート』→ 原文は『旧　シート』（全角空白）で一致"),
        "{}",
        edited.report
    );
    assert!(
        edited
            .report
            .contains("『番 号』→ 原文は『番\u{00A0}号』（NBSP）で一致"),
        "{}",
        edited.report
    );
    assert!(
        edited
            .report
            .contains("『列 名』→ 原文は『列\t名』（タブ）で一致"),
        "{}",
        edited.report
    );
    assert!(
        edited
            .report
            .contains("グラフ（このツールでは変更できない場所）"),
        "{}",
        edited.report
    );
    assert!(
        !edited.report.contains("文書のプロパティ"),
        "{}",
        edited.report
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_follow_up_edit_does_not_stack_the_suffix_or_overwrite() {
    assert_eq!(
        super::output_filename(
            Path::new("契約書_編集済み（2026-11-12 09-30-00）.docx"),
            "2026-11-16 12-00-00",
            "docx",
        ),
        "契約書_編集済み（2026-11-16 12-00-00）.docx"
    );
    assert_eq!(
        super::output_filename(
            Path::new("甲_乙_編集済み（2026-11-12 09-30-00）_編集済み（2026-11-13 01-00-00）.xlsx",),
            "2026-11-16 12-00-00",
            "xlsx",
        ),
        "甲_乙_編集済み（2026-11-16 12-00-00）.xlsx"
    );
    assert_eq!(
        super::output_filename(Path::new("契約書.docx"), "2026-11-16 12-00-00", "docx"),
        "契約書_編集済み（2026-11-16 12-00-00）.docx"
    );

    let dir = temp_dir("stack");
    let out = dir.join("out");
    fs::create_dir_all(&out).unwrap();
    let src = out.join("契約書_編集済み（2026-11-12 09-30-00）.docx");
    write_zip(
        &src,
        &[(
            "word/document.xml",
            word_body(r#"<w:p><w:r><w:t>佐藤花子</w:t></w:r></w:p>"#).as_str(),
        )],
    );
    let before = fs::read(&src).unwrap();
    let edited = edit_path(
        &src,
        &out,
        "2026-11-16 12-00-00",
        &request(&[("佐藤花子", "鈴木一郎")], &[]),
    )
    .unwrap();
    assert_eq!(
        edited.filename,
        "契約書_編集済み（2026-11-16 12-00-00）.docx"
    );
    assert_eq!(fs::read(&src).unwrap(), before);
    assert!(read_entry(&edited.path, "word/document.xml").contains("鈴木一郎"));
    let again = edit_path(
        &edited.path,
        &out,
        "2026-11-16 12-00-00",
        &request(&[("鈴木一郎", "高橋次郎")], &[]),
    )
    .unwrap();
    assert_eq!(
        again.filename,
        "契約書_編集済み（2026-11-16 12-00-00）（2）.docx"
    );
    assert_ne!(again.path, edited.path);
    assert!(edited.path.exists());
    assert!(read_entry(&edited.path, "word/document.xml").contains("鈴木一郎"));
    assert!(!read_entry(&edited.path, "word/document.xml").contains("高橋次郎"));
    assert!(read_entry(&again.path, "word/document.xml").contains("高橋次郎"));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn the_same_chat_overwrites_its_own_edited_file_and_leaves_failures() {
    let dir = temp_dir("overwrite-chat");
    let out = dir.join("out");
    fs::create_dir_all(&out).unwrap();
    let name = "契約書_編集済み（2026-11-12 09-30-00）.docx";
    let src = out.join(name);
    write_zip(
        &src,
        &[(
            "word/document.xml",
            word_body(r#"<w:p><w:r><w:t>佐藤花子</w:t></w:r></w:p>"#).as_str(),
        )],
    );
    let prior = format!("{FILE_LINE}{name}\n{OFFICE_MARKER}\tdocx\t{name}\n");
    assert!(super::replaces_chat_output(
        &src,
        &out,
        &[("edit_office", prior.as_str())]
    ));
    assert!(!super::replaces_chat_output(
        &src,
        &out,
        &[("create_docx", prior.as_str())]
    ));
    assert!(!super::replaces_chat_output(&src, &out, &[]));
    let other = format!("{OFFICE_MARKER}\tdocx\t別紙_編集済み（2026-11-12 09-30-00）.docx\n");
    assert!(!super::replaces_chat_output(
        &src,
        &out,
        &[("edit_office", other.as_str())]
    ));

    let edited = super::edit_path_mode(
        &src,
        &out,
        "2026-11-16 12-00-00",
        &request(&[("佐藤花子", "鈴木一郎")], &[]),
        true,
    )
    .unwrap();
    assert_eq!(edited.filename, name);
    assert_eq!(edited.path, src);
    assert!(
        edited.report.contains("前回の編集済みファイルに上書きした"),
        "{}",
        edited.report
    );
    assert!(read_entry(&src, "word/document.xml").contains("鈴木一郎"));
    assert!(!read_entry(&src, "word/document.xml").contains("佐藤花子"));
    let names = docx_names(&out);
    assert_eq!(names, vec![name.to_string()], "{names:?}");
    assert!(dir_has_no_temp(&out));

    let again = super::edit_path_mode(
        &src,
        &out,
        "2026-11-16 13-00-00",
        &request(&[("鈴木一郎", "高橋次郎")], &[]),
        true,
    )
    .unwrap();
    assert_eq!(again.path, src);
    assert_eq!(again.filename, name);
    assert!(read_entry(&src, "word/document.xml").contains("高橋次郎"));
    assert_eq!(docx_names(&out), vec![name.to_string()]);

    let kept = fs::read(&src).unwrap();
    let err = super::edit_path_mode(
        &src,
        &out,
        "2026-11-16 14-00-00",
        &request(&[("いない人", "誰か")], &[]),
        true,
    )
    .unwrap_err();
    assert!(err.contains("見つかりません"), "{err}");
    assert_eq!(fs::read(&src).unwrap(), kept);
    assert_eq!(docx_names(&out), vec![name.to_string()]);
    assert!(dir_has_no_temp(&out));

    let outside = dir.join(name);
    write_zip(
        &outside,
        &[(
            "word/document.xml",
            word_body(r#"<w:p><w:r><w:t>佐藤花子</w:t></w:r></w:p>"#).as_str(),
        )],
    );
    let outside_before = fs::read(&outside).unwrap();
    assert!(!super::replaces_chat_output(
        &outside,
        &out,
        &[("edit_office", prior.as_str())]
    ));
    let fresh = super::edit_path_mode(
        &outside,
        &out,
        "2026-11-16 15-00-00",
        &request(&[("佐藤花子", "鈴木一郎")], &[]),
        false,
    )
    .unwrap();
    assert_eq!(fs::read(&outside).unwrap(), outside_before);
    assert_ne!(fresh.path, outside);
    assert_eq!(
        fresh.filename,
        "契約書_編集済み（2026-11-16 15-00-00）.docx"
    );
    assert!(!fresh.report.contains("上書きした"), "{}", fresh.report);
    assert!(fresh.path.starts_with(&out));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn an_open_edited_file_is_left_and_a_new_file_is_saved() {
    let dir = temp_dir("overwrite-open");
    let out = dir.join("out");
    fs::create_dir_all(&out).unwrap();
    let name = "契約書_編集済み（2026-11-12 09-30-00）.docx";
    let src = out.join(name);
    write_zip(
        &src,
        &[(
            "word/document.xml",
            word_body(r#"<w:p><w:r><w:t>佐藤花子</w:t></w:r></w:p>"#).as_str(),
        )],
    );
    let before = fs::read(&src).unwrap();
    super::fail_next_replace_of_existing_file();
    let edited = super::edit_path_mode(
        &src,
        &out,
        "2026-11-16 16-00-00",
        &request(&[("佐藤花子", "鈴木一郎")], &[]),
        true,
    )
    .unwrap();
    let fresh = "契約書_編集済み（2026-11-16 16-00-00）.docx";
    assert_eq!(edited.filename, fresh);
    assert_eq!(edited.path, out.join(fresh));
    assert!(edited.path.is_file());
    assert_eq!(fs::read(&src).unwrap(), before);
    assert!(read_entry(&src, "word/document.xml").contains("佐藤花子"));
    assert!(read_entry(&edited.path, "word/document.xml").contains("鈴木一郎"));
    assert!(
        edited
            .report
            .contains("前回のファイルが開かれていたため、新しいファイルに保存した"),
        "{}",
        edited.report
    );
    assert!(
        edited.report.contains(&format!("{FILE_LINE}{fresh}\n")),
        "{}",
        edited.report
    );
    assert!(
        !edited.report.contains("前回の編集済みファイルに上書きした"),
        "{}",
        edited.report
    );
    let mut expected = vec![fresh.to_string(), name.to_string()];
    expected.sort();
    assert_eq!(docx_names(&out), expected);
    assert!(dir_has_no_temp(&out));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_transient_replace_failure_still_overwrites() {
    let dir = temp_dir("overwrite-retry");
    let out = dir.join("out");
    fs::create_dir_all(&out).unwrap();
    let name = "契約書_編集済み（2026-11-12 09-30-00）.docx";
    let src = out.join(name);
    write_zip(
        &src,
        &[(
            "word/document.xml",
            word_body(r#"<w:p><w:r><w:t>佐藤花子</w:t></w:r></w:p>"#).as_str(),
        )],
    );
    super::fail_replace_of_existing_file_times(1);
    let edited = super::edit_path_mode(
        &src,
        &out,
        "2026-11-16 16-30-00",
        &request(&[("佐藤花子", "鈴木一郎")], &[]),
        true,
    )
    .unwrap();
    assert_eq!(edited.path, src);
    assert!(
        edited.report.contains("前回の編集済みファイルに上書きした"),
        "{}",
        edited.report
    );
    assert!(read_entry(&src, "word/document.xml").contains("鈴木一郎"));
    assert_eq!(docx_names(&out), vec![name.to_string()]);
    assert!(dir_has_no_temp(&out));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn repeated_edits_of_one_source_update_the_same_file() {
    let (dir, db, thread) = open_chat("same-source");
    let out = dir.join(super::OUTPUT_DIR_NAME);
    fs::create_dir_all(&out).unwrap();
    let original = out.join("メモ.docx");
    write_zip(
        &original,
        &[(
            "word/document.xml",
            word_body(r#"<w:p><w:r><w:t>甲乙丙</w:t></w:r></w:p>"#).as_str(),
        )],
    );
    let original_bytes = fs::read(&original).unwrap();

    let first = edit_named(&db, &dir, &thread, "メモ.docx", "甲", "丁");
    assert!(!first.report.contains("上書きした"), "{}", first.report);
    assert!(first.filename.contains("メモ_編集済み（"));
    assert_eq!(fs::read(&original).unwrap(), original_bytes);
    assert!(read_entry(&first.path, "word/document.xml").contains("丁乙丙"));

    db.insert_llm_message_full(
        &thread,
        "tool",
        "（文脈の上限に達したため、この結果は省略しました）",
        "",
        "",
        "call-1",
        "edit_office",
        "",
    )
    .unwrap();

    let second = edit_named(&db, &dir, &thread, "メモ.docx", "乙", "戊");
    assert_eq!(second.path, first.path);
    assert!(
        second.report.contains("前回の編集済みファイルに上書きした"),
        "{}",
        second.report
    );
    assert!(read_entry(&first.path, "word/document.xml").contains("丁戊丙"));
    assert_eq!(fs::read(&original).unwrap(), original_bytes);

    let third = edit_named(&db, &dir, &thread, "メモ.docx", "丙", "己");
    assert_eq!(third.path, first.path);
    assert!(read_entry(&first.path, "word/document.xml").contains("丁戊己"));
    assert!(!read_entry(&first.path, "word/document.xml").contains("甲"));
    assert_eq!(edited_names(&out), vec![first.filename.clone()]);
    assert_eq!(fs::read(&original).unwrap(), original_bytes);
    assert!(dir_has_no_temp(&out));

    let other = db.create_llm_thread("other", false).unwrap();
    let elsewhere = edit_named(&db, &dir, &other.id, "メモ.docx", "甲", "壬");
    assert_ne!(elsewhere.path, first.path);
    assert!(read_entry(&elsewhere.path, "word/document.xml").contains("壬乙丙"));
    assert!(read_entry(&first.path, "word/document.xml").contains("丁戊己"));
    assert_eq!(fs::read(&original).unwrap(), original_bytes);
    assert_eq!(edited_names(&out).len(), 2);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn an_attached_original_stays_put_while_follow_ups_stack() {
    let (dir, db, thread) = open_chat("attached-source");
    let original = dir.join("契約書.docx");
    write_zip(
        &original,
        &[(
            "word/document.xml",
            word_body(r#"<w:p><w:r><w:t>甲です</w:t></w:r></w:p>"#).as_str(),
        )],
    );
    let original_bytes = fs::read(&original).unwrap();
    db.insert_llm_source(
        &thread,
        "attach",
        &original.to_string_lossy(),
        "契約書.docx",
        "",
        "",
        "",
    )
    .unwrap();
    let first = edit_named(&db, &dir, &thread, "契約書.docx", "甲", "乙");
    assert_eq!(fs::read(&original).unwrap(), original_bytes);
    assert!(read_entry(&first.path, "word/document.xml").contains("乙です"));
    db.insert_llm_message_full(
        &thread,
        "tool",
        "（文脈の上限に達したため、この結果は省略しました）",
        "",
        "",
        "call-1",
        "edit_office",
        "",
    )
    .unwrap();
    let second = edit_named(&db, &dir, &thread, "契約書.docx", "乙", "丙");
    assert_eq!(second.path, first.path);
    assert!(
        second.report.contains("前回の編集済みファイルに上書きした"),
        "{}",
        second.report
    );
    assert!(read_entry(&first.path, "word/document.xml").contains("丙です"));
    assert_eq!(fs::read(&original).unwrap(), original_bytes);
    assert_eq!(
        edited_names(&dir.join(super::OUTPUT_DIR_NAME)),
        vec![first.filename]
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn alternating_originals_keep_their_own_edited_files() {
    let (dir, db, thread) = open_chat("alternate");
    let out = dir.join(super::OUTPUT_DIR_NAME);
    fs::create_dir_all(&out).unwrap();
    let contract = out.join("契約書.docx");
    let exhibit = out.join("別紙.docx");
    write_zip(
        &contract,
        &[(
            "word/document.xml",
            word_body(r#"<w:p><w:r><w:t>甲乙丙</w:t></w:r></w:p>"#).as_str(),
        )],
    );
    write_zip(
        &exhibit,
        &[(
            "word/document.xml",
            word_body(r#"<w:p><w:r><w:t>一二</w:t></w:r></w:p>"#).as_str(),
        )],
    );
    let contract_bytes = fs::read(&contract).unwrap();
    let exhibit_bytes = fs::read(&exhibit).unwrap();

    let first = edit_named(&db, &dir, &thread, "契約書.docx", "甲", "丁");
    let other = edit_named(&db, &dir, &thread, "別紙.docx", "一", "壱");
    assert_ne!(first.path, other.path);
    let third = edit_named(&db, &dir, &thread, "契約書.docx", "乙", "戊");
    assert_eq!(third.path, first.path);
    assert!(
        third.report.contains(&format!(
            "前回の編集済みファイル「{}」を編集した",
            first.filename
        )),
        "{}",
        third.report
    );
    assert!(read_entry(&first.path, "word/document.xml").contains("丁戊丙"));
    assert!(read_entry(&other.path, "word/document.xml").contains("壱二"));
    assert_eq!(fs::read(&contract).unwrap(), contract_bytes);
    assert_eq!(fs::read(&exhibit).unwrap(), exhibit_bytes);
    let contract_edits: Vec<_> = edited_names(&out)
        .into_iter()
        .filter(|name| name.starts_with("契約書_編集済み"))
        .collect();
    assert_eq!(contract_edits, vec![first.filename.clone()]);
    let recorded = db.list_llm_office_edits(&thread).unwrap();
    assert_eq!(
        recorded.last().map(|row| row.output_name.as_str()),
        Some(first.filename.as_str())
    );

    let by_name = edit_named(&db, &dir, &thread, &first.filename, "丙", "己");
    assert_eq!(by_name.filename, first.filename);
    assert!(read_entry(&first.path, "word/document.xml").contains("丁戊己"));
    let again = edit_named(&db, &dir, &thread, "契約書.docx", "丁", "庚");
    assert_eq!(again.path, first.path);
    assert!(read_entry(&first.path, "word/document.xml").contains("庚戊己"));
    assert_eq!(fs::read(&contract).unwrap(), contract_bytes);
    assert_eq!(
        db.list_llm_office_edits(&thread)
            .unwrap()
            .last()
            .map(|row| row.output_name.clone()),
        Some(first.filename)
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn fresh_starts_from_the_original_and_a_miss_names_the_edited_file() {
    let (dir, db, thread) = open_chat("fresh");
    let out = dir.join(super::OUTPUT_DIR_NAME);
    fs::create_dir_all(&out).unwrap();
    let original = out.join("契約書.docx");
    write_zip(
        &original,
        &[(
            "word/document.xml",
            word_body(r#"<w:p><w:r><w:t>甲乙</w:t></w:r></w:p>"#).as_str(),
        )],
    );
    let original_bytes = fs::read(&original).unwrap();
    let first = edit_named(&db, &dir, &thread, "契約書.docx", "甲", "丁");
    let kept = fs::read(&first.path).unwrap();

    let missed = super::edit_in_thread(
        &db,
        &dir,
        "",
        &thread,
        &json!({
            "filename": "契約書.docx",
            "replacements": [{"find": "いない", "replace": "誰か"}],
            "paragraphs": []
        }),
    )
    .unwrap_err();
    assert!(
        missed.contains(&format!(
            "前回の編集済みファイル「{}」の中を探したが、指定した文字列は見つからなかった。fresh は、利用者が元の版からやり直すと頼んだときだけ指定する。",
            first.filename
        )),
        "{missed}"
    );
    assert_eq!(fs::read(&original).unwrap(), original_bytes);
    assert_eq!(fs::read(&first.path).unwrap(), kept);

    let restarted = super::edit_in_thread(
        &db,
        &dir,
        "",
        &thread,
        &json!({
            "filename": "契約書.docx",
            "fresh": true,
            "replacements": [{"find": "乙", "replace": "戊"}],
            "paragraphs": []
        }),
    )
    .unwrap();
    assert_ne!(restarted.path, first.path);
    assert!(
        !restarted.report.contains("を編集した"),
        "{}",
        restarted.report
    );
    assert!(read_entry(&restarted.path, "word/document.xml").contains("甲戊"));
    assert!(!read_entry(&restarted.path, "word/document.xml").contains("丁"));
    assert_eq!(fs::read(&first.path).unwrap(), kept);
    assert_eq!(fs::read(&original).unwrap(), original_bytes);

    let stacked = edit_named(&db, &dir, &thread, "契約書.docx", "甲", "己");
    assert_eq!(stacked.path, restarted.path);
    assert!(read_entry(&restarted.path, "word/document.xml").contains("己戊"));
    assert_eq!(fs::read(&first.path).unwrap(), kept);
    assert_eq!(fs::read(&original).unwrap(), original_bytes);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn fresh_string_true_starts_from_the_original() {
    let (dir, db, thread) = open_chat("fresh-string");
    let out = dir.join(super::OUTPUT_DIR_NAME);
    fs::create_dir_all(&out).unwrap();
    let original = out.join("別紙.docx");
    write_zip(
        &original,
        &[(
            "word/document.xml",
            word_body(r#"<w:p><w:r><w:t>甲</w:t></w:r></w:p>"#).as_str(),
        )],
    );
    let original_bytes = fs::read(&original).unwrap();
    let first = edit_named(&db, &dir, &thread, "別紙.docx", "甲", "乙");
    let kept = fs::read(&first.path).unwrap();
    let restarted = super::edit_in_thread(
        &db,
        &dir,
        "",
        &thread,
        &json!({
            "filename": "別紙.docx",
            "fresh": "true",
            "replacements": [{"find": "甲", "replace": "丙"}],
            "paragraphs": []
        }),
    )
    .unwrap();
    assert_ne!(restarted.path, first.path);
    assert!(read_entry(&restarted.path, "word/document.xml").contains("丙"));
    assert!(!read_entry(&restarted.path, "word/document.xml").contains("乙"));
    assert_eq!(fs::read(&first.path).unwrap(), kept);
    assert_eq!(fs::read(&original).unwrap(), original_bytes);
    let _ = fs::remove_dir_all(&dir);
}

fn open_chat(label: &str) -> (PathBuf, crate::db::Db, String) {
    let dir = temp_dir(label);
    let db = crate::db::Db::open(&dir.join("argos.db")).unwrap();
    let thread = db.create_llm_thread(label, false).unwrap();
    (dir, db, thread.id)
}

fn edit_named(
    db: &crate::db::Db,
    data_dir: &Path,
    thread: &str,
    filename: &str,
    find: &str,
    replace: &str,
) -> super::EditOutcome {
    super::edit_in_thread(
        db,
        data_dir,
        "",
        thread,
        &json!({
            "filename": filename,
            "replacements": [{"find": find, "replace": replace}],
            "paragraphs": []
        }),
    )
    .unwrap_or_else(|err| panic!("{filename}: {err}"))
}

fn edited_names(dir: &Path) -> Vec<String> {
    docx_names(dir)
        .into_iter()
        .filter(|name| name.contains("編集済み"))
        .collect()
}

fn docx_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.to_ascii_lowercase().ends_with(".docx"))
        .collect();
    names.sort();
    names
}

fn dir_has_no_temp(dir: &Path) -> bool {
    fs::read_dir(dir).unwrap().all(|entry| {
        let name = entry.unwrap().file_name().to_string_lossy().into_owned();
        !name.ends_with(".tmp") && !name.ends_with(".tmpzip")
    })
}

fn pasteable_at<'a>(report: &'a str, location: &str) -> &'a str {
    let lines: Vec<&str> = report.lines().collect();
    let index = lines
        .iter()
        .position(|line| line.starts_with("- ") && line.contains(location))
        .unwrap_or_else(|| panic!("no leftover at {location} in {report}"));
    let quoted = lines
        .get(index + 1)
        .copied()
        .filter(|line| line.contains('「'))
        .unwrap_or(lines[index]);
    assert!(
        quoted.starts_with('「') && quoted.ends_with('」') && !quoted.contains('※'),
        "pasteable line should be only the quoted source: {quoted}\n{report}"
    );
    inside_quotes(quoted)
}

fn whitespace_note_follows<'a>(report: &'a str, exact: &str) -> &'a str {
    let lines: Vec<&str> = report.lines().collect();
    let quoted = format!("「{exact}」");
    let index = lines
        .iter()
        .position(|line| *line == quoted)
        .unwrap_or_else(|| panic!("no lone pasteable line {quoted} in {report}"));
    lines
        .get(index + 1)
        .copied()
        .unwrap_or_else(|| panic!("no note after {quoted} in {report}"))
}

#[test]
fn a_spaceless_find_leaves_spaced_spellings_and_reports_them() {
    let dir = temp_dir("opus");
    let src = dir.join("訴状.docx");
    let document = word_body(
        "<w:p><w:r><w:t>本文の山田太郎</w:t></w:r></w:p>\
<w:p><w:r><w:t>山\u{3000}田\u{3000}太\u{3000}郎</w:t></w:r></w:p>\
<w:p><w:r><w:t>山田\u{3000}太郎</w:t></w:r></w:p>",
    );
    write_zip(&src, &[("word/document.xml", document.as_str())]);
    let before = fs::read(&src).unwrap();
    let edited = edit_path(
        &src,
        &dir.join("out"),
        "2026-11-16 13-00-00",
        &request(&[("山田太郎", "佐藤花子")], &[]),
    )
    .unwrap();
    assert_eq!(fs::read(&src).unwrap(), before);
    let document = read_entry(&edited.path, "word/document.xml");
    assert!(document.contains("本文の佐藤花子"), "{document}");
    assert!(
        document.contains("山\u{3000}田\u{3000}太\u{3000}郎"),
        "{document}"
    );
    assert!(document.contains("山田\u{3000}太郎"), "{document}");
    assert_eq!(
        pasteable_at(&edited.report, "本文の2段落目"),
        "山\u{3000}田\u{3000}太\u{3000}郎"
    );
    assert_eq!(
        whitespace_note_follows(&edited.report, "山\u{3000}田\u{3000}太\u{3000}郎"),
        "※全角空白"
    );
    assert_eq!(
        pasteable_at(&edited.report, "本文の3段落目"),
        "山田\u{3000}太郎"
    );
    assert_eq!(
        whitespace_note_follows(&edited.report, "山田\u{3000}太郎"),
        "※全角空白"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn shorter_finds_do_not_rewrite_spaced_spellings() {
    let dir = temp_dir("short-find");
    let src = dir.join("署名.docx");
    let document = word_body(
        "<w:p><w:r><w:t>署名\u{3000}甲\u{3000}乙</w:t></w:r></w:p>\
<w:p><w:r><w:t>山\u{3000}田\u{3000}太\u{3000}郎</w:t></w:r></w:p>",
    );
    write_zip(&src, &[("word/document.xml", document.as_str())]);
    let before = fs::read(&src).unwrap();
    let err = edit_path(
        &src,
        &dir.join("out"),
        "2026-11-16 13-00-04",
        &request(&[("甲乙", "丙丁"), ("山田", "鈴木")], &[]),
    )
    .unwrap_err();
    assert_eq!(fs::read(&src).unwrap(), before);
    assert!(!dir.join("out").exists());
    assert!(!err.contains("丙丁"), "{err}");
    assert!(!err.contains("鈴木"), "{err}");
    assert_eq!(pasteable_near(&err, "甲\u{3000}乙"), "甲\u{3000}乙");
    assert_eq!(pasteable_near(&err, "山\u{3000}田"), "山\u{3000}田");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_missed_spaceless_find_reports_a_spread_spelling() {
    let dir = temp_dir("spread-miss");
    let src = dir.join("訴状.docx");
    let document = word_body("<w:p><w:r><w:t>山\u{3000}田\u{3000}太\u{3000}郎</w:t></w:r></w:p>");
    write_zip(&src, &[("word/document.xml", document.as_str())]);
    let before = fs::read(&src).unwrap();
    let err = edit_path(
        &src,
        &dir.join("out"),
        "2026-11-16 13-00-03",
        &request(&[("山田太郎", "佐藤花子")], &[]),
    )
    .unwrap_err();
    assert_eq!(fs::read(&src).unwrap(), before);
    assert!(!dir.join("out").exists());
    assert_eq!(
        pasteable_near(&err, "\u{3000}"),
        "山\u{3000}田\u{3000}太\u{3000}郎"
    );
    assert_eq!(
        whitespace_note_follows(&err, "山\u{3000}田\u{3000}太\u{3000}郎"),
        "※全角空白"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_body_hit_still_reports_chart_and_hyperlink_whitespace_leftovers() {
    let dir = temp_dir("left-ws");
    let src = dir.join("訴状.docx");
    let document = word_body(
        r#"<w:p><w:r><w:t>本文の山田太郎</w:t></w:r></w:p>
<w:p><w:r><w:instrText xml:space="preserve"> REF 山田　太郎 \h </w:instrText></w:r></w:p>"#,
    );
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId5" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://example.com/山田 太郎" TargetMode="External"/>
</Relationships>"#;
    let chart = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<c:chartSpace xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
  <c:tx><a:t>山田　太郎</a:t></c:tx>
</c:chartSpace>"#;
    write_zip(
        &src,
        &[
            ("word/document.xml", document.as_str()),
            ("word/_rels/document.xml.rels", rels),
            ("word/charts/chart1.xml", chart),
        ],
    );
    let edited = edit_path(
        &src,
        &dir.join("out"),
        "2026-11-16 13-00-01",
        &request(&[("山田太郎", "佐藤花子")], &[]),
    )
    .unwrap();
    let document = read_entry(&edited.path, "word/document.xml");
    assert!(document.contains("本文の佐藤花子"), "{document}");
    assert!(document.contains("REF 山田　太郎"), "{document}");
    let chart = read_entry(&edited.path, "word/charts/chart1.xml");
    assert!(chart.contains("山田　太郎"), "{chart}");
    let rels = read_entry(&edited.path, "word/_rels/document.xml.rels");
    assert!(rels.contains("山田 太郎"), "{rels}");
    assert!(edited.report.contains("残存: あり"), "{}", edited.report);
    assert!(
        edited.report.contains("完全一致 1、空白の違い 0") || edited.report.contains("を 1 箇所"),
        "{}",
        edited.report
    );
    let chart_text = pasteable_at(&edited.report, "グラフ（このツールでは変更できない場所）");
    assert_eq!(chart_text, "山田\u{3000}太郎");
    assert!(!chart_text.contains("全角"));
    assert_eq!(
        whitespace_note_follows(&edited.report, chart_text),
        "※全角空白"
    );
    let link_text = pasteable_at(
        &edited.report,
        "ハイパーリンク（このツールでは変更できない場所）",
    );
    assert_eq!(link_text, "山田 太郎");
    assert_eq!(
        whitespace_note_follows(&edited.report, link_text),
        "※半角空白"
    );
    let field_text = pasteable_at(
        &edited.report,
        "フィールド（このツールでは変更できない場所）",
    );
    assert_eq!(field_text, "山田\u{3000}太郎");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn an_xlsx_body_hit_still_reports_a_chart_whitespace_leftover() {
    let dir = temp_dir("xlsx-left");
    let src = dir.join("一覧.xlsx");
    let shared = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <si><t>本文の山田太郎</t></si>
</sst>"#;
    let sheet = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <sheetData><row r="1"><c r="A1" t="s"><v>0</v></c></row></sheetData>
</worksheet>"#;
    let chart = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<c:chartSpace xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
  <c:tx><a:t>山田　太郎</a:t></c:tx>
</c:chartSpace>"#;
    write_zip(
        &src,
        &[
            ("xl/sharedStrings.xml", shared),
            ("xl/worksheets/sheet1.xml", sheet),
            ("xl/charts/chart1.xml", chart),
        ],
    );
    let edited = edit_path(
        &src,
        &dir.join("out"),
        "2026-11-16 13-00-02",
        &request(&[("山田太郎", "佐藤花子")], &[]),
    )
    .unwrap();
    let shared = read_entry(&edited.path, "xl/sharedStrings.xml");
    assert!(shared.contains("本文の佐藤花子"), "{shared}");
    assert!(!shared.contains("山田"), "{shared}");
    let chart_text = pasteable_at(&edited.report, "グラフ（このツールでは変更できない場所）");
    assert_eq!(chart_text, "山田\u{3000}太郎");
    assert!(edited.report.contains("※全角空白"), "{}", edited.report);
    let _ = fs::remove_dir_all(&dir);
}

fn alt_box(text: &str) -> String {
    format!(
        r#"<mc:AlternateContent><mc:Choice Requires="wps"><w:drawing><w:txbxContent><w:p><w:r><w:t>{text}</w:t></w:r></w:p></w:txbxContent></w:drawing></mc:Choice><mc:Fallback><w:pict><v:textbox><w:txbxContent><w:p><w:r><w:t>{text}</w:t></w:r></w:p></w:txbxContent></v:textbox></w:pict></mc:Fallback></mc:AlternateContent>"#
    )
}

fn story_part(root: &str, inner: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<{root} xmlns:w="{W}" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">{inner}</{root}>"#
    )
}

#[test]
fn replacements_reach_text_boxes_and_shapes_but_not_charts_or_diagrams() {
    let dir = temp_dir("shapes");
    let src = dir.join("契約書.docx");
    let document = story_part(
        "w:document",
        &format!(
            r#"<w:body>
    <w:p><w:r><w:t>前文</w:t></w:r><w:r>{}</w:r><w:r><w:t>後文</w:t></w:r></w:p>
    <w:p><w:r><w:t>本文の山田太郎</w:t></w:r></w:p>
    <w:p><w:r><w:drawing><a:txBody><a:p><a:r><a:t>図形の山田太郎</a:t></a:r></a:p></a:txBody></w:drawing></w:r></w:p>
    <w:p><w:r><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/chart"><a:t>図表の山田太郎</a:t></a:graphicData></w:r></w:p>
  </w:body>"#,
            alt_box("枠の山田太郎")
        ),
    );
    let header = story_part(
        "w:hdr",
        &format!(
            r#"<w:p><w:r><w:t>ヘッダーの山田太郎</w:t></w:r></w:p><w:p><w:r>{}</w:r></w:p>"#,
            alt_box("ヘッダー枠の山田太郎")
        ),
    );
    let footer = story_part(
        "w:ftr",
        &format!(
            r#"<w:p><w:r>{}</w:r></w:p>"#,
            alt_box("フッター枠の山田太郎")
        ),
    );
    let footnotes = story_part(
        "w:footnotes",
        &format!(
            r#"<w:footnote w:id="1"><w:p><w:r>{}</w:r></w:p></w:footnote>"#,
            alt_box("脚注枠の山田太郎")
        ),
    );
    let endnotes = story_part(
        "w:endnotes",
        &format!(
            r#"<w:endnote w:id="1"><w:p><w:r>{}</w:r></w:p></w:endnote>"#,
            alt_box("文末脚注枠の山田太郎")
        ),
    );
    let chart = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<c:chartSpace xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
  <c:tx><a:t>グラフの山田太郎</a:t></c:tx>
</c:chartSpace>"#;
    let diagram = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<dgm:dataModel xmlns:dgm="http://schemas.openxmlformats.org/drawingml/2006/diagram" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
  <a:t>図の山田太郎</a:t>
</dgm:dataModel>"#;
    let drawing = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:drawing xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><a:t>別図形の山田太郎</a:t></w:drawing>"#;
    write_zip(
        &src,
        &[
            ("word/document.xml", document.as_str()),
            ("word/header1.xml", header.as_str()),
            ("word/footer1.xml", footer.as_str()),
            ("word/footnotes.xml", footnotes.as_str()),
            ("word/endnotes.xml", endnotes.as_str()),
            ("word/charts/chart1.xml", chart),
            ("word/diagrams/data1.xml", diagram),
            ("word/drawings/drawing1.xml", drawing),
        ],
    );
    let edited = edit_path(
        &src,
        &dir.join("out"),
        "2026-11-16 16-00-00",
        &request(&[("山田太郎", "佐藤花子")], &[]),
    )
    .unwrap();
    assert!(
        edited
            .report
            .contains("文字列置換: 「山田太郎」→「佐藤花子」を 8 箇所\n"),
        "{}",
        edited.report
    );
    for (name, needle) in [
        ("word/document.xml", "本文の佐藤花子"),
        ("word/document.xml", "枠の佐藤花子"),
        ("word/document.xml", "図形の佐藤花子"),
        ("word/document.xml", "前文"),
        ("word/document.xml", "後文"),
        ("word/header1.xml", "ヘッダーの佐藤花子"),
        ("word/header1.xml", "ヘッダー枠の佐藤花子"),
        ("word/footer1.xml", "フッター枠の佐藤花子"),
        ("word/footnotes.xml", "脚注枠の佐藤花子"),
        ("word/endnotes.xml", "文末脚注枠の佐藤花子"),
    ] {
        let xml = read_entry(&edited.path, name);
        assert!(xml.contains(needle), "{name} missing {needle}: {xml}");
    }
    for name in [
        "word/header1.xml",
        "word/footer1.xml",
        "word/footnotes.xml",
        "word/endnotes.xml",
    ] {
        let xml = read_entry(&edited.path, name);
        assert!(!xml.contains("山田太郎"), "{name}: {xml}");
    }
    let document = read_entry(&edited.path, "word/document.xml");
    assert_eq!(document.matches("枠の佐藤花子").count(), 2, "{document}");
    assert!(document.contains("図表の山田太郎"), "{document}");
    assert!(
        edited
            .report
            .contains("グラフ（このツールでは変更できない場所）"),
        "{}",
        edited.report
    );
    assert!(
        edited
            .report
            .contains("図（このツールでは変更できない場所）"),
        "{}",
        edited.report
    );
    assert!(
        edited
            .report
            .contains("図形（このツールでは変更できない場所）"),
        "{}",
        edited.report
    );
    assert!(!edited
        .report
        .contains("ヘッダー（このツールでは変更できない場所）"));
    assert!(!edited
        .report
        .contains("フッター（このツールでは変更できない場所）"));
    assert!(!edited
        .report
        .contains("脚注（このツールでは変更できない場所）"));
    assert!(!edited
        .report
        .contains("文末脚注（このツールでは変更できない場所）"));
    assert!(read_entry(&edited.path, "word/charts/chart1.xml").contains("グラフの山田太郎"));
    assert!(read_entry(&edited.path, "word/diagrams/data1.xml").contains("図の山田太郎"));
    assert!(read_entry(&edited.path, "word/drawings/drawing1.xml").contains("別図形の山田太郎"));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn text_box_whitespace_follows_body_rules_and_reports_the_source_bytes() {
    let dir = temp_dir("shape-ws");
    let src = dir.join("名簿.docx");
    let document = story_part(
        "w:document",
        &format!(
            r#"<w:body>
    <w:p><w:r><w:t>本文の対象</w:t></w:r></w:p>
    <w:p><w:r>{}</w:r></w:p>
    <w:p><w:r><w:txbxContent><w:p><w:r><w:t>鈴木　一郎</w:t></w:r></w:p></w:txbxContent></w:r></w:p>
  </w:body>"#,
            alt_box("山田　太郎")
        ),
    );
    write_zip(&src, &[("word/document.xml", document.as_str())]);
    let edited = edit_path(
        &src,
        &dir.join("out"),
        "2026-11-16 16-10-00",
        &request(&[("山田 太郎", "佐藤花子"), ("鈴木一郎", "高橋")], &[]),
    )
    .unwrap();
    let document = read_entry(&edited.path, "word/document.xml");
    assert_eq!(document.matches("佐藤花子").count(), 2, "{document}");
    assert!(document.contains("鈴木　一郎"), "{document}");
    assert!(!document.contains("高橋"), "{document}");
    assert!(
        edited
            .report
            .contains("「山田 太郎」→「佐藤花子」を 1 箇所（完全一致 0、空白の違い 1）"),
        "{}",
        edited.report
    );
    assert!(
        edited
            .report
            .contains("『山田 太郎』→ 原文は『山田　太郎』（全角空白）で一致"),
        "{}",
        edited.report
    );
    assert_eq!(
        pasteable_at(&edited.report, "本文の3段落目"),
        "鈴木\u{3000}一郎"
    );
    assert_eq!(
        whitespace_note_follows(&edited.report, "鈴木\u{3000}一郎"),
        "※全角空白"
    );
    assert!(!edited.report.contains("このツールでは変更できない場所"));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn paragraph_rewrite_of_a_text_box_updates_the_fallback_without_counting_twice() {
    let dir = temp_dir("shape-para");
    let src = dir.join("契約書.docx");
    let document = story_part(
        "w:document",
        &format!(
            r#"<w:body><w:p><w:r><w:t>残す段落。</w:t></w:r></w:p><w:p><w:r>{}</w:r></w:p></w:body>"#,
            alt_box("この枠だけを新しい文にする。")
        ),
    );
    write_zip(&src, &[("word/document.xml", document.as_str())]);
    let edited = edit_path(
        &src,
        &dir.join("out"),
        "2026-11-16 16-20-00",
        &request(
            &[],
            &[("この枠だけを新しい文にする。", "書き換えた枠です。")],
        ),
    )
    .unwrap();
    assert!(
        edited.report.contains("段落の書き換え: 1 件"),
        "{}",
        edited.report
    );
    let document = read_entry(&edited.path, "word/document.xml");
    assert_eq!(
        document.matches("書き換えた枠です。").count(),
        2,
        "{document}"
    );
    assert!(!document.contains("この枠だけ"), "{document}");
    assert!(document.contains("残す段落。"), "{document}");
    let _ = fs::remove_dir_all(&dir);
}

fn tracked(items: &[(&str, &str, &str)], author: &str) -> EditRequest {
    let mut req = EditRequest::from_args(&json!({
        "tracked": true,
        "replacements": items.iter().map(|(find, replace, comment)| json!({
            "find": find,
            "replace": replace,
            "comment": comment,
        })).collect::<Vec<_>>(),
    }))
    .unwrap();
    if !author.is_empty() {
        req.revision_author = author.into();
    }
    req.revision_date = "2026-11-16T00:00:00Z".into();
    req
}

#[test]
fn blank_revision_author_is_argos() {
    assert_eq!(super::office_revision_author(""), "Argos");
    assert_eq!(super::office_revision_author(" 　 "), "Argos");
    assert_eq!(super::office_revision_author(" 山田 "), "山田");
}

#[test]
fn tracked_replacement_records_deletion_and_insertion() {
    let dir = temp_dir("track");
    let src = dir.join("契約書.docx");
    let document =
        word_body(r#"<w:p><w:r><w:t>山</w:t></w:r><w:r><w:t>田太郎である。</w:t></w:r></w:p>"#);
    let core = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/">
  <dc:creator>山田太郎</dc:creator>
</cp:coreProperties>"#;
    write_zip(
        &src,
        &[
            ("word/document.xml", document.as_str()),
            ("docProps/core.xml", core),
        ],
    );
    let edited = edit_path(
        &src,
        &dir.join("out"),
        "2026-11-16 18-00-00",
        &tracked(&[("山田太郎", "佐藤花子", "")], ""),
    )
    .unwrap();
    let document = read_entry(&edited.path, "word/document.xml");
    assert!(document.contains("<w:del "), "{document}");
    assert!(document.contains("<w:ins "), "{document}");
    assert!(document.contains("w:author=\"Argos\""), "{document}");
    assert!(
        document.contains("w:date=\"2026-11-16T00:00:00Z\""),
        "{document}"
    );
    assert!(document.contains("<w:delText>山</w:delText>"), "{document}");
    assert!(
        document.contains("<w:delText>田太郎</w:delText>"),
        "{document}"
    );
    assert!(document.contains("<w:t>佐藤花子</w:t>"), "{document}");
    assert!(document.contains("である。"), "{document}");
    assert!(
        edited
            .report
            .contains("文字列置換: 「山田太郎」→「佐藤花子」を 2 箇所"),
        "{}",
        edited.report
    );
    assert!(
        edited
            .report
            .contains("変更履歴: 作成者「Argos」で記録しました。"),
        "{}",
        edited.report
    );
    assert!(edited.report.contains("残存: なし"), "{}", edited.report);
    let core = read_entry(&edited.path, "docProps/core.xml");
    assert!(core.contains("佐藤花子"), "{core}");
    assert!(!core.contains("山田太郎"), "{core}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn tracked_replacement_keeps_siblings_in_the_same_run() {
    let dir = temp_dir("track-br");
    let src = dir.join("契約書.docx");
    let document = word_body(
        r#"<w:p><w:r><w:tab/><w:t>山田太郎</w:t><w:br/></w:r><w:r><w:t>である。</w:t></w:r></w:p>"#,
    );
    write_zip(&src, &[("word/document.xml", document.as_str())]);
    let edited = edit_path(
        &src,
        &dir.join("out"),
        "2026-11-16 18-05-00",
        &tracked(&[("山田太郎", "佐藤花子", "")], ""),
    )
    .unwrap();
    let document = read_entry(&edited.path, "word/document.xml");
    assert!(document.contains("<w:tab"), "{document}");
    assert!(document.contains("<w:br"), "{document}");
    assert!(
        document.contains("<w:delText>山田太郎</w:delText>"),
        "{document}"
    );
    assert!(document.contains("<w:t>佐藤花子</w:t>"), "{document}");
    assert!(document.contains("である。"), "{document}");
    let tab = document.find("<w:tab").expect(&document);
    let deleted = document.find("<w:del ").expect(&document);
    let inserted = document.find("<w:ins ").expect(&document);
    let br = document.find("<w:br").expect(&document);
    assert!(tab < deleted && inserted < br, "{document}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn tracked_comment_is_anchored_and_uses_the_setting_author() {
    let dir = temp_dir("track-cmt");
    let src = dir.join("契約書.docx");
    let document = word_body(r#"<w:p><w:r><w:t>本文の山田太郎</w:t></w:r></w:p>"#);
    let header = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:hdr xmlns:w="{W}"><w:p><w:r><w:t>ヘッダーの山田太郎</w:t></w:r></w:p></w:hdr>"#
    );
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/></Relationships>"#;
    let types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    write_zip(
        &src,
        &[
            ("word/document.xml", document.as_str()),
            ("word/header1.xml", header.as_str()),
            ("word/_rels/document.xml.rels", rels),
            ("[Content_Types].xml", types),
        ],
    );
    let edited = edit_path(
        &src,
        &dir.join("out"),
        "2026-11-16 18-10-00",
        &tracked(&[("山田太郎", "佐藤花子", "当事者名を改めた")], "審査太郎"),
    )
    .unwrap();
    let document = read_entry(&edited.path, "word/document.xml");
    assert!(document.contains("w:author=\"審査太郎\""), "{document}");
    assert!(document.contains("<w:commentRangeStart "), "{document}");
    assert!(document.contains("<w:commentReference "), "{document}");
    let prefix = document.find("<w:t>本文の</w:t>").expect(&document);
    let anchor = document.find("<w:commentRangeStart ").expect(&document);
    assert!(prefix < anchor, "{document}");
    let span = &document[anchor..document.find("<w:commentReference ").expect(&document)];
    assert!(span.contains("<w:delText>山田太郎</w:delText>"), "{span}");
    assert!(span.contains("<w:t>佐藤花子</w:t>"), "{span}");
    let header = read_entry(&edited.path, "word/header1.xml");
    assert!(
        header.contains("<w:delText>ヘッダーの山田太郎</w:delText>")
            || header.contains("ヘッダーの"),
        "{header}"
    );
    assert!(header.contains("佐藤花子"), "{header}");
    assert!(header.contains("<w:ins "), "{header}");
    assert!(!header.contains("<w:commentRangeStart "), "{header}");
    let comments = read_entry(&edited.path, "word/comments.xml");
    assert!(comments.contains("当事者名を改めた"), "{comments}");
    assert!(comments.contains("w:author=\"審査太郎\""), "{comments}");
    let rels = read_entry(&edited.path, "word/_rels/document.xml.rels");
    assert!(rels.contains("comments.xml"), "{rels}");
    assert!(rels.contains("header1.xml"), "{rels}");
    assert!(!zip_contains(&edited.path, "word/_rels/header1.xml.rels"));
    let types = read_entry(&edited.path, "[Content_Types].xml");
    assert!(types.contains("/word/comments.xml"), "{types}");
    assert!(types.contains("/word/document.xml"), "{types}");
    assert!(
        edited.report.contains("コメント: 1 件"),
        "{}",
        edited.report
    );
    assert!(
        edited
            .report
            .contains("ヘッダー、フッター、脚注、文末脚注にはコメントを付けられないため、その箇所のコメントは付けませんでした。"),
        "{}",
        edited.report
    );
    assert!(
        edited
            .report
            .contains("文字列置換: 「山田太郎」→「佐藤花子」を 2 箇所"),
        "{}",
        edited.report
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn comment_without_tracked_changes_does_not_wrap_a_deletion() {
    let dir = temp_dir("cmt-plain");
    let src = dir.join("メモ.docx");
    let document = word_body(r#"<w:p><w:r><w:t>本文の山田太郎</w:t></w:r></w:p>"#);
    write_zip(&src, &[("word/document.xml", document.as_str())]);
    let mut req = EditRequest::from_args(&json!({
        "replacements": [{"find": "山田太郎", "replace": "佐藤花子", "comment": "確認"}]
    }))
    .unwrap();
    req.revision_date = "2026-11-16T00:00:00Z".into();
    let edited = edit_path(&src, &dir.join("out"), "2026-11-16 18-20-00", &req).unwrap();
    let document = read_entry(&edited.path, "word/document.xml");
    assert!(document.contains("佐藤花子"), "{document}");
    assert!(!document.contains("<w:del "), "{document}");
    assert!(document.contains("<w:commentRangeStart "), "{document}");
    assert!(read_entry(&edited.path, "word/comments.xml").contains("確認"));
    assert!(
        edited.report.contains("コメント: 1 件"),
        "{}",
        edited.report
    );
    assert!(!edited.report.contains("変更履歴:"), "{}", edited.report);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn tracked_whitespace_in_a_text_box_keeps_the_source_bytes() {
    let dir = temp_dir("track-ws");
    let src = dir.join("枠.docx");
    let document = story_part(
        "w:document",
        &format!(
            r#"<w:body><w:p><w:r>{}</w:r></w:p></w:body>"#,
            alt_box("山田　太郎")
        ),
    );
    write_zip(&src, &[("word/document.xml", document.as_str())]);
    let edited = edit_path(
        &src,
        &dir.join("out"),
        "2026-11-16 18-30-00",
        &tracked(&[("山田 太郎", "佐藤花子", "")], ""),
    )
    .unwrap();
    let document = read_entry(&edited.path, "word/document.xml");
    assert!(document.contains("山田　太郎"), "{document}");
    assert!(document.contains("<w:delText"), "{document}");
    assert!(document.contains("佐藤花子"), "{document}");
    assert_eq!(document.matches("<w:ins ").count(), 2, "{document}");
    assert!(edited.report.contains("空白の違い 1"), "{}", edited.report);
    assert!(
        edited.report.contains("『山田　太郎』"),
        "{}",
        edited.report
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn xlsx_says_tracked_changes_and_comments_do_not_apply() {
    let dir = temp_dir("track-xlsx");
    let src = dir.join("一覧.xlsx");
    let shared = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><si><t>山田太郎</t></si></sst>"#;
    let sheet = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <sheetData><row r="1"><c r="A1" t="s"><v>0</v></c></row></sheetData>
</worksheet>"#;
    write_zip(
        &src,
        &[
            ("xl/sharedStrings.xml", shared),
            ("xl/worksheets/sheet1.xml", sheet),
        ],
    );
    let edited = edit_path(
        &src,
        &dir.join("out"),
        "2026-11-16 18-40-00",
        &tracked(&[("山田太郎", "佐藤花子", "メモ")], ""),
    )
    .unwrap();
    let shared = read_entry(&edited.path, "xl/sharedStrings.xml");
    assert!(shared.contains("佐藤花子"), "{shared}");
    assert!(!shared.contains("山田太郎"), "{shared}");
    assert!(
        edited.report.contains(
            "変更履歴としての記録とコメントは Excel では使えません。文字だけ置換しました。"
        ),
        "{}",
        edited.report
    );
    let missed = edit_path(
        &src,
        &dir.join("out"),
        "2026-11-16 18-41-00",
        &tracked(&[("存在しない語", "別の語", "メモ")], ""),
    )
    .unwrap_err();
    assert!(missed.contains("見つかりませんでした"), "{missed}");
    assert!(
        missed.contains(
            "変更履歴としての記録とコメントは Excel では使えません。文字だけ置換しました。"
        ),
        "{missed}"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn tracked_shape_text_stays_a_direct_replacement() {
    let dir = temp_dir("track-shape");
    let src = dir.join("図.docx");
    let document = story_part(
        "w:document",
        r#"<w:body>
    <w:p><w:r><w:t>本文の山田太郎</w:t></w:r></w:p>
    <w:p><w:r><w:drawing><a:txBody><a:p><a:r><a:t>図形の山田太郎</a:t></a:r></a:p></a:txBody></w:drawing></w:r></w:p>
  </w:body>"#,
    );
    write_zip(&src, &[("word/document.xml", document.as_str())]);
    let edited = edit_path(
        &src,
        &dir.join("out"),
        "2026-11-16 18-50-00",
        &tracked(&[("山田太郎", "佐藤花子", "")], ""),
    )
    .unwrap();
    let document = read_entry(&edited.path, "word/document.xml");
    assert!(document.contains("<w:t>本文の</w:t>"), "{document}");
    assert!(
        document.contains("<w:delText>山田太郎</w:delText>"),
        "{document}"
    );
    assert!(document.contains("<w:ins "), "{document}");
    assert!(document.contains("<a:t>図形の佐藤花子</a:t>"), "{document}");
    assert!(
        !document.contains("<a:t>図形の山田太郎</a:t>"),
        "{document}"
    );
    assert!(
        edited
            .report
            .contains("図形の文字は変更履歴にできないため、文字だけ置換しました。"),
        "{}",
        edited.report
    );
    assert!(
        edited
            .report
            .contains("文字列置換: 「山田太郎」→「佐藤花子」を 2 箇所"),
        "{}",
        edited.report
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn tracked_output_accepts_a_follow_up_edit() {
    let dir = temp_dir("track-chain");
    let src = dir.join("契約書.docx");
    let document =
        word_body(r#"<w:p><w:r><w:t>山</w:t></w:r><w:r><w:t>田太郎である。</w:t></w:r></w:p>"#);
    write_zip(&src, &[("word/document.xml", document.as_str())]);
    let first = edit_path(
        &src,
        &dir.join("out"),
        "2026-11-16 19-00-00",
        &tracked(&[("山田太郎", "佐藤花子", "")], ""),
    )
    .unwrap();
    let mut again = request(&[("佐藤花子", "鈴木一郎")], &[]);
    again.revision_date = "2026-11-16T01:00:00Z".into();
    let second = edit_path(&first.path, &dir.join("out"), "2026-11-16 19-10-00", &again).unwrap();
    let document = read_entry(&second.path, "word/document.xml");
    assert!(document.contains("鈴木一郎"), "{document}");
    assert!(document.contains("である。"), "{document}");
    assert!(document.contains("<w:delText>山</w:delText>"), "{document}");
    assert!(
        document.contains("<w:delText>田太郎</w:delText>"),
        "{document}"
    );
    assert!(!document.contains("佐藤花子である。"), "{document}");
    assert!(second.report.contains("残存: なし"), "{}", second.report);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn tracked_run_with_breaks_between_text_nodes_is_emitted_once() {
    let dir = temp_dir("track-brs");
    let src = dir.join("改行.docx");
    let document = word_body(
        r#"<w:p><w:r><w:t>前文</w:t><w:br/><w:t>山田太郎</w:t><w:br/><w:t>後文</w:t></w:r></w:p><w:p><w:r><w:t>山田</w:t><w:br/><w:t>太郎です</w:t></w:r></w:p>"#,
    );
    write_zip(&src, &[("word/document.xml", document.as_str())]);
    let edited = edit_path(
        &src,
        &dir.join("out"),
        "2026-11-16 20-00-00",
        &tracked(&[("山田太郎", "佐藤花子", "")], ""),
    )
    .unwrap();
    let document = read_entry(&edited.path, "word/document.xml");
    assert_docx_xml_well_formed(&edited.path);
    assert_eq!(document.matches("前文").count(), 1, "{document}");
    assert_eq!(document.matches("後文").count(), 1, "{document}");
    assert_eq!(document.matches("佐藤花子").count(), 2, "{document}");
    assert_eq!(document.matches("<w:br").count(), 3, "{document}");
    assert_eq!(
        document.matches("<w:delText>山田太郎</w:delText>").count(),
        1,
        "{document}"
    );
    assert!(
        document.contains("<w:delText>山田</w:delText>"),
        "{document}"
    );
    assert!(
        document.contains("<w:delText>太郎</w:delText>"),
        "{document}"
    );
    assert!(document.contains(">です</w:t>"), "{document}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn tracked_match_across_hyperlink_stays_well_formed() {
    let dir = temp_dir("track-link");
    let src = dir.join("リンク.docx");
    let document = word_body(
        r#"<w:p><w:hyperlink r:id="rId1"><w:r><w:t>山田</w:t></w:r></w:hyperlink><w:r><w:t>太郎です</w:t></w:r></w:p><w:p><w:ins w:id="9" w:author="旧" w:date="2020-01-01T00:00:00Z"><w:r><w:t>山</w:t></w:r></w:ins><w:sdt><w:sdtContent><w:r><w:t>田太郎</w:t></w:r></w:sdtContent></w:sdt></w:p>"#,
    );
    write_zip(&src, &[("word/document.xml", document.as_str())]);
    let edited = edit_path(
        &src,
        &dir.join("out"),
        "2026-11-16 20-10-00",
        &tracked(&[("山田太郎", "佐藤花子", "")], ""),
    )
    .unwrap();
    let document = read_entry(&edited.path, "word/document.xml");
    assert_docx_xml_well_formed(&edited.path);
    assert!(document.contains("<w:hyperlink "), "{document}");
    assert!(document.contains("</w:hyperlink>"), "{document}");
    assert!(document.contains("</w:sdt>"), "{document}");
    assert!(document.contains("</w:ins>"), "{document}");
    assert!(document.contains("佐藤花子"), "{document}");
    assert!(!document.contains(">山田<"), "{document}");
    assert!(
        edited.report.contains(
            "途中で別の要素をまたぐ置換は変更履歴にできないため、その箇所は文字だけ置換しました。"
        ),
        "{}",
        edited.report
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn tracked_chain_does_not_rewrite_earlier_deletions() {
    let dir = temp_dir("track-chain-del");
    let src = dir.join("契約書.docx");
    let document = word_body(
        r#"<w:p><w:del w:id="1" w:author="Argos" w:date="2020-01-01T00:00:00Z"><w:r><w:delText>山田太郎</w:delText></w:r></w:del><w:r><w:t>本文の山田太郎</w:t></w:r></w:p>"#,
    );
    write_zip(&src, &[("word/document.xml", document.as_str())]);
    let first = edit_path(
        &src,
        &dir.join("out"),
        "2026-11-16 20-20-00",
        &tracked(&[("山田太郎", "佐藤花子", "")], ""),
    )
    .unwrap();
    let first_xml = read_entry(&first.path, "word/document.xml");
    assert_docx_xml_well_formed(&first.path);
    assert_eq!(
        first_xml.matches("<w:delText>山田太郎</w:delText>").count(),
        2,
        "{first_xml}"
    );
    assert!(
        first_xml.contains("w:date=\"2020-01-01T00:00:00Z\""),
        "{first_xml}"
    );
    assert!(first.report.contains("残存: なし"), "{}", first.report);
    assert!(
        first
            .report
            .contains("文字列置換: 「山田太郎」→「佐藤花子」を 1 箇所"),
        "{}",
        first.report
    );
    let mut again = tracked(
        &[("山田太郎", "別人", ""), ("佐藤花子", "鈴木一郎", "")],
        "",
    );
    again.revision_date = "2026-11-16T09:00:00Z".into();
    let second = edit_path(&first.path, &dir.join("out"), "2026-11-16 20-30-00", &again).unwrap();
    let document = read_entry(&second.path, "word/document.xml");
    assert_docx_xml_well_formed(&second.path);
    assert_eq!(
        document.matches("<w:delText>山田太郎</w:delText>").count(),
        2,
        "{document}"
    );
    assert!(
        !document.contains("<w:delText>別人</w:delText>"),
        "{document}"
    );
    assert!(document.contains("鈴木一郎"), "{document}");
    assert!(
        document.contains("w:date=\"2020-01-01T00:00:00Z\""),
        "{document}"
    );
    assert!(second.report.contains("残存: なし"), "{}", second.report);
    assert!(
        second.report.contains("「山田太郎」は見つかりませんでした"),
        "{}",
        second.report
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn comments_are_registered_on_document_and_skipped_outside_it() {
    let dir = temp_dir("track-cmt-where");
    let src = dir.join("契約書.docx");
    let document = word_body(r#"<w:p><w:r><w:t>本文の山田太郎</w:t></w:r></w:p>"#);
    let header = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:hdr xmlns:w="{W}"><w:p><w:r><w:t>ヘッダーの山田太郎</w:t></w:r></w:p></w:hdr>"#
    );
    let footer = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:ftr xmlns:w="{W}"><w:p><w:r><w:t>フッターの山田太郎</w:t></w:r></w:p></w:ftr>"#
    );
    let footnotes = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:footnotes xmlns:w="{W}"><w:footnote w:id="1"><w:p><w:r><w:t>脚注の山田太郎</w:t></w:r></w:p></w:footnote></w:footnotes>"#
    );
    let endnotes = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:endnotes xmlns:w="{W}"><w:endnote w:id="1"><w:p><w:r><w:t>文末の山田太郎</w:t></w:r></w:p></w:endnote></w:endnotes>"#
    );
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/></Relationships>"#;
    write_zip(
        &src,
        &[
            ("word/document.xml", document.as_str()),
            ("word/header1.xml", header.as_str()),
            ("word/footer1.xml", footer.as_str()),
            ("word/footnotes.xml", footnotes.as_str()),
            ("word/endnotes.xml", endnotes.as_str()),
            ("word/_rels/document.xml.rels", rels),
        ],
    );
    let edited = edit_path(
        &src,
        &dir.join("out"),
        "2026-11-16 20-40-00",
        &tracked(&[("山田太郎", "佐藤花子", "注記")], "審査太郎"),
    )
    .unwrap();
    assert_docx_xml_well_formed(&edited.path);
    let document = read_entry(&edited.path, "word/document.xml");
    assert!(document.contains("<w:commentRangeStart "), "{document}");
    for (name, label) in [
        ("word/header1.xml", "ヘッダーの"),
        ("word/footer1.xml", "フッターの"),
        ("word/footnotes.xml", "脚注の"),
        ("word/endnotes.xml", "文末の"),
    ] {
        let xml = read_entry(&edited.path, name);
        assert!(xml.contains("佐藤花子"), "{name}: {xml}");
        assert!(xml.contains(label), "{name}: {xml}");
        assert!(!xml.contains("<w:commentRangeStart "), "{name}: {xml}");
    }
    let rels = read_entry(&edited.path, "word/_rels/document.xml.rels");
    assert!(rels.contains("comments.xml"), "{rels}");
    assert!(rels.contains("header1.xml"), "{rels}");
    for name in [
        "word/_rels/header1.xml.rels",
        "word/_rels/footer1.xml.rels",
        "word/_rels/footnotes.xml.rels",
        "word/_rels/endnotes.xml.rels",
    ] {
        assert!(!zip_contains(&edited.path, name), "{name}");
    }
    assert!(
        edited.report.contains("コメント: 1 件"),
        "{}",
        edited.report
    );
    assert!(
        edited.report.contains(
            "ヘッダー、フッター、脚注、文末脚注にはコメントを付けられないため、その箇所のコメントは付けませんでした。"
        ),
        "{}",
        edited.report
    );
    assert!(
        edited
            .report
            .contains("文字列置換: 「山田太郎」→「佐藤花子」を 5 箇所"),
        "{}",
        edited.report
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn revision_author_matching_the_find_is_not_rewritten() {
    let dir = temp_dir("track-author");
    let src = dir.join("契約書.docx");
    let document = word_body(r#"<w:p><w:r><w:t>山田太郎です。</w:t></w:r></w:p>"#);
    let core = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:creator>山田太郎</dc:creator></cp:coreProperties>"#;
    write_zip(
        &src,
        &[
            ("word/document.xml", document.as_str()),
            ("docProps/core.xml", core),
        ],
    );
    let edited = edit_path(
        &src,
        &dir.join("out"),
        "2026-11-16 20-50-00",
        &tracked(&[("山田太郎", "佐藤花子", "確認")], "山田太郎"),
    )
    .unwrap();
    assert_docx_xml_well_formed(&edited.path);
    let document = read_entry(&edited.path, "word/document.xml");
    assert!(document.contains("w:author=\"山田太郎\""), "{document}");
    assert!(!document.contains("w:author=\"佐藤花子\""), "{document}");
    assert!(
        document.contains("<w:delText>山田太郎</w:delText>"),
        "{document}"
    );
    assert!(document.contains("<w:t>佐藤花子</w:t>"), "{document}");
    let comments = read_entry(&edited.path, "word/comments.xml");
    assert!(comments.contains("w:author=\"山田太郎\""), "{comments}");
    assert!(comments.contains("確認"), "{comments}");
    let core = read_entry(&edited.path, "docProps/core.xml");
    assert!(core.contains("佐藤花子"), "{core}");
    assert!(!core.contains("山田太郎"), "{core}");
    assert!(
        edited
            .report
            .contains("文字列置換: 「山田太郎」→「佐藤花子」を 2 箇所"),
        "{}",
        edited.report
    );
    let _ = fs::remove_dir_all(&dir);
}

fn write_inventory_xlsx(path: &Path, sheet: &str, workbook_body: &str) {
    let workbook = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="在庫" sheetId="1" r:id="rId1"/></sheets>{workbook_body}</workbook>"#
    );
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#;
    let shared = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="2" uniqueCount="2"><si><t>山田太郎</t></si><si><t>849995</t></si></sst>"#;
    let chain = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<calcChain xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><c r="C1" i="1" l="1"/></calcChain>"#;
    write_zip(
        path,
        &[
            ("xl/workbook.xml", workbook.as_str()),
            ("xl/_rels/workbook.xml.rels", rels),
            ("xl/sharedStrings.xml", shared),
            ("xl/calcChain.xml", chain),
            ("xl/worksheets/sheet1.xml", sheet),
        ],
    );
}

#[test]
fn xlsx_replaces_numeric_cells_ignoring_commas_and_yen() {
    let dir = temp_dir("xlsx-num");
    let src = dir.join("在庫.xlsx");
    let sheet = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1"><v>849995</v></c><c r="B1" t="n"><v>1232</v></c><c r="C1"><f>A1+1</f><v>849995</v></c><c r="D1" t="s"><v>1</v></c><c r="E1" t="str"><f>T("山田太郎")</f><v>山田太郎</v></c><c r="F1" t="s"><v>0</v></c><c r="G1" t="inlineStr"><is><t>備考</t></is></c></row></sheetData></worksheet>"#;
    write_inventory_xlsx(
        &src,
        sheet,
        r#"<calcPr calcMode="auto" fullCalcOnLoad="0"/>"#,
    );
    let chain_before = read_entry(&src, "xl/calcChain.xml");
    let edited = edit_path(
        &src,
        &dir.join("out"),
        "2026-11-17 06-10-00",
        &request(
            &[
                ("¥849,995", "¥1,000"),
                ("￥１，２３２", "2,000"),
                ("山田太郎", "佐藤花子"),
            ],
            &[],
        ),
    )
    .unwrap();
    let sheet = read_entry(&edited.path, "xl/worksheets/sheet1.xml");
    assert!(sheet.contains(r#"<c r="A1"><v>1000</v></c>"#), "{sheet}");
    assert!(
        sheet.contains(r#"<c r="B1" t="n"><v>2000</v></c>"#),
        "{sheet}"
    );
    assert!(!sheet.contains("t=\"s\"><v>1000</v>"), "{sheet}");
    assert!(
        sheet.contains(r#"<c r="C1"><f>A1+1</f><v>849995</v></c>"#),
        "{sheet}"
    );
    assert!(
        sheet.contains(r#"<c r="E1" t="str"><f>T("山田太郎")</f><v>佐藤花子</v></c>"#),
        "{sheet}"
    );
    assert!(sheet.contains("<t>備考</t>"), "{sheet}");
    let shared = read_entry(&edited.path, "xl/sharedStrings.xml");
    assert!(shared.contains("<t>佐藤花子</t>"), "{shared}");
    assert!(shared.contains("<t>849995</t>"), "{shared}");
    assert!(!shared.contains("山田太郎"), "{shared}");
    let workbook = read_entry(&edited.path, "xl/workbook.xml");
    assert!(workbook.contains("fullCalcOnLoad=\"1\""), "{workbook}");
    assert!(!workbook.contains("fullCalcOnLoad=\"0\""), "{workbook}");
    assert_eq!(read_entry(&edited.path, "xl/calcChain.xml"), chain_before);
    assert!(
        edited
            .report
            .contains("シート「在庫」セル C1 は計算式のセルなので変えていない。"),
        "{}",
        edited.report
    );
    assert!(
        !edited
            .report
            .contains("シート「在庫」セル E1 は計算式のセルなので変えていない。"),
        "{}",
        edited.report
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn xlsx_inserts_full_calc_on_load_when_calc_pr_is_missing() {
    let dir = temp_dir("xlsx-calc");
    let src = dir.join("在庫.xlsx");
    let sheet = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1"><v>849995</v></c></row></sheetData></worksheet>"#;
    write_inventory_xlsx(&src, sheet, "");
    let edited = edit_path(
        &src,
        &dir.join("out"),
        "2026-11-17 06-11-00",
        &request(&[("849,995", "1000")], &[]),
    )
    .unwrap();
    let sheet = read_entry(&edited.path, "xl/worksheets/sheet1.xml");
    assert!(sheet.contains("<v>1000</v>"), "{sheet}");
    assert!(!sheet.contains("t=\"s\""), "{sheet}");
    let workbook = read_entry(&edited.path, "xl/workbook.xml");
    assert!(
        workbook.contains("<calcPr fullCalcOnLoad=\"1\"/></workbook>"),
        "{workbook}"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn xlsx_skips_a_numeric_cell_when_the_replacement_is_not_a_number() {
    let dir = temp_dir("xlsx-nan");
    let src = dir.join("在庫.xlsx");
    let sheet = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="B1" t="n"><v>1232</v></c></row></sheetData></worksheet>"#;
    write_inventory_xlsx(&src, sheet, "<calcPr calcMode=\"auto\"/>");
    let err = edit_path(
        &src,
        &dir.join("out"),
        "2026-11-17 06-12-00",
        &request(&[("1,232", "メモ")], &[]),
    )
    .unwrap_err();
    assert!(
        err.contains("シート「在庫」セル B1 は数値のセルですが、「メモ」は数値として書けないので変えていません。"),
        "{err}"
    );
    let sheet = read_entry(&src, "xl/worksheets/sheet1.xml");
    assert!(sheet.contains("<v>1232</v>"), "{sheet}");
    let workbook = read_entry(&src, "xl/workbook.xml");
    assert!(!workbook.contains("fullCalcOnLoad"), "{workbook}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn xlsx_reports_a_formula_cell_when_nothing_else_changes() {
    let dir = temp_dir("xlsx-formula");
    let src = dir.join("在庫.xlsx");
    let sheet = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="C1"><f>A1+1</f><v>849995</v></c></row></sheetData></worksheet>"#;
    write_inventory_xlsx(&src, sheet, "<calcPr calcMode=\"auto\"/>");
    let err = edit_path(
        &src,
        &dir.join("out"),
        "2026-11-17 06-13-00",
        &request(&[("￥849,995", "1000")], &[]),
    )
    .unwrap_err();
    assert!(
        err.contains("シート「在庫」セル C1 は計算式のセルなので変えていない。"),
        "{err}"
    );
    let sheet = read_entry(&src, "xl/worksheets/sheet1.xml");
    assert!(sheet.contains(r#"<f>A1+1</f><v>849995</v>"#), "{sheet}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn xlsx_numeric_match_is_the_whole_cell_and_strings_stay() {
    let dir = temp_dir("xlsx-whole");
    let src = dir.join("在庫.xlsx");
    let sheet = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1"><v>849995</v></c><c r="F1" t="s"><v>0</v></c></row></sheetData></worksheet>"#;
    let workbook = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="在庫" sheetId="1" r:id="rId1"/></sheets><calcPr calcMode="auto"/></workbook>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#;
    let shared = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><si><t>山田太郎</t></si></sst>"#;
    write_zip(
        &src,
        &[
            ("xl/workbook.xml", workbook),
            ("xl/_rels/workbook.xml.rels", rels),
            ("xl/sharedStrings.xml", shared),
            ("xl/worksheets/sheet1.xml", sheet),
        ],
    );
    let edited = edit_path(
        &src,
        &dir.join("out"),
        "2026-11-17 06-14-00",
        &request(&[("995", "1"), ("山田太郎", "佐藤花子")], &[]),
    )
    .unwrap();
    let sheet = read_entry(&edited.path, "xl/worksheets/sheet1.xml");
    assert!(sheet.contains("<v>849995</v>"), "{sheet}");
    assert!(!sheet.contains("<v>1</v>"), "{sheet}");
    let shared = read_entry(&edited.path, "xl/sharedStrings.xml");
    assert!(shared.contains("<t>佐藤花子</t>"), "{shared}");
    assert!(!shared.contains("995"), "{shared}");
    let workbook = read_entry(&edited.path, "xl/workbook.xml");
    assert!(workbook.contains("fullCalcOnLoad=\"1\""), "{workbook}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn xlsx_replaces_cached_formula_text_and_recalculates() {
    let dir = temp_dir("xlsx-str-formula");
    let src = dir.join("宛名.xlsx");
    let sheet = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1" t="str"><f>A1&amp;" 様"</f><v>山田太郎 様</v></c><c r="C1"><f>A1+1</f><v>849995</v></c></row></sheetData></worksheet>"#;
    write_inventory_xlsx(&src, sheet, "<calcPr calcMode=\"auto\"/>");
    let edited = edit_path(
        &src,
        &dir.join("out"),
        "2026-11-17 06-20-00",
        &request(&[("山田太郎", "佐藤花子"), ("849995", "1")], &[]),
    )
    .unwrap();
    let sheet = read_entry(&edited.path, "xl/worksheets/sheet1.xml");
    assert!(sheet.contains(r#"<f>A1&amp;" 様"</f>"#), "{sheet}");
    assert!(sheet.contains("<v>佐藤花子 様</v>"), "{sheet}");
    assert!(!sheet.contains("<v>山田太郎 様</v>"), "{sheet}");
    assert!(
        sheet.contains(r#"<c r="C1"><f>A1+1</f><v>849995</v></c>"#),
        "{sheet}"
    );
    let shared = read_entry(&edited.path, "xl/sharedStrings.xml");
    assert!(shared.contains("<t>佐藤花子</t>"), "{shared}");
    let workbook = read_entry(&edited.path, "xl/workbook.xml");
    assert!(workbook.contains("fullCalcOnLoad=\"1\""), "{workbook}");
    assert!(
        edited
            .report
            .contains("文字列置換: 「山田太郎」→「佐藤花子」を 2 箇所"),
        "{}",
        edited.report
    );
    assert!(
        edited
            .report
            .contains("シート「在庫」セル C1 は計算式のセルなので変えていない。"),
        "{}",
        edited.report
    );
    assert!(
        !edited.report.contains("セル B1 は計算式"),
        "{}",
        edited.report
    );
    let _ = fs::remove_dir_all(&dir);
}

fn core_props(creator: &str, modified: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/">
  <dc:title>題名の山田</dc:title>
  <dc:creator>{creator}</dc:creator>
  <cp:lastModifiedBy>{modified}</cp:lastModifiedBy>
</cp:coreProperties>"#
    )
}

fn docx_with_core(dir: &Path, core: &str) -> PathBuf {
    let src = dir.join("契約書.docx");
    let document = word_body(r#"<w:p><w:r><w:t>原告は残る。</w:t></w:r></w:p>"#);
    write_zip(
        &src,
        &[
            ("word/document.xml", document.as_str()),
            ("docProps/core.xml", core),
        ],
    );
    src
}

#[test]
fn editing_leaves_the_document_author_unless_the_tool_is_asked() {
    let dir = temp_dir("author-keep");
    let src = docx_with_core(&dir, &core_props("山田太郎", "佐藤花子"));
    let edited = edit_path(
        &src,
        &dir.join("out"),
        "2026-11-20 01-00-00",
        &request(&[("原告", "申立人")], &[]),
    )
    .unwrap();
    let core = read_entry(&edited.path, "docProps/core.xml");
    assert!(core.contains("<dc:creator>山田太郎</dc:creator>"), "{core}");
    assert!(
        core.contains("<cp:lastModifiedBy>佐藤花子</cp:lastModifiedBy>"),
        "{core}"
    );
    assert!(core.contains("<dc:title>題名の山田</dc:title>"), "{core}");
    let document = read_entry(&edited.path, "word/document.xml");
    assert!(document.contains("申立人は残る。"), "{document}");
    assert!(!edited.report.contains("作成者を"), "{}", edited.report);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn docx_author_can_be_set_cleared_and_overrides_a_matching_replacement() {
    let dir = temp_dir("author-set");
    let src = docx_with_core(&dir, &core_props("山田太郎", "山田太郎"));
    let req = EditRequest::from_args(&json!({
        "replacements": [{"find": "山田", "replace": "佐藤"}],
        "creator": "事務所",
        "clear_last_modified_by": true,
    }))
    .unwrap();
    let edited = edit_path(&src, &dir.join("out"), "2026-11-20 01-10-00", &req).unwrap();
    let core = read_entry(&edited.path, "docProps/core.xml");
    assert!(core.contains("<dc:creator>事務所</dc:creator>"), "{core}");
    assert!(
        core.contains("<cp:lastModifiedBy></cp:lastModifiedBy>"),
        "{core}"
    );
    assert!(core.contains("<dc:title>題名の佐藤</dc:title>"), "{core}");
    assert!(!core.contains("山田"), "{core}");
    let document = read_entry(&edited.path, "word/document.xml");
    assert!(document.contains("原告は残る。"), "{document}");
    assert!(
        edited.report.contains("作成者を「事務所」にしました。"),
        "{}",
        edited.report
    );
    assert!(
        edited.report.contains("最終更新者を空にしました。"),
        "{}",
        edited.report
    );
    let authors = super::read_docx_authors(&edited.path).unwrap();
    assert_eq!(authors.creator, "事務所");
    assert_eq!(authors.last_modified_by, "");
    assert_eq!(
        super::docx_author_lines(&authors),
        "作成者: 事務所\n最終更新者: （なし）"
    );
    assert_docx_xml_well_formed(&edited.path);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn docx_author_can_be_cleared_and_last_modified_by_set_without_other_edits() {
    let dir = temp_dir("author-only");
    let src = docx_with_core(&dir, &core_props("山田太郎", "佐藤花子"));
    let req = EditRequest::from_args(&json!({
        "clear_creator": true,
        "last_modified_by": "A&B <確認>",
    }))
    .unwrap();
    let edited = edit_path(&src, &dir.join("out"), "2026-11-20 01-20-00", &req).unwrap();
    let core = read_entry(&edited.path, "docProps/core.xml");
    assert!(core.contains("<dc:creator></dc:creator>"), "{core}");
    assert!(
        core.contains("<cp:lastModifiedBy>A&amp;B &lt;確認&gt;</cp:lastModifiedBy>"),
        "{core}"
    );
    assert!(core.contains("<dc:title>題名の山田</dc:title>"), "{core}");
    let document = read_entry(&edited.path, "word/document.xml");
    assert!(document.contains("原告は残る。"), "{document}");
    let authors = super::read_docx_authors(&edited.path).unwrap();
    assert_eq!(authors.creator, "");
    assert_eq!(authors.last_modified_by, "A&B <確認>");
    assert_docx_xml_well_formed(&edited.path);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn setting_the_author_creates_core_properties_when_the_package_has_none() {
    let dir = temp_dir("author-create");
    let src = dir.join("新規.docx");
    let document = word_body(r#"<w:p><w:r><w:t>本文だけ。</w:t></w:r></w:p>"#);
    let types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;
    write_zip(
        &src,
        &[
            ("word/document.xml", document.as_str()),
            ("[Content_Types].xml", types),
            ("_rels/.rels", rels),
        ],
    );
    let req = EditRequest::from_args(&json!({"creator": "確認係", "last_modified_by": "確認係"}))
        .unwrap();
    let edited = edit_path(&src, &dir.join("out"), "2026-11-20 01-30-00", &req).unwrap();
    let core = read_entry(&edited.path, "docProps/core.xml");
    assert!(core.contains("<dc:creator>確認係</dc:creator>"), "{core}");
    assert!(
        core.contains("<cp:lastModifiedBy>確認係</cp:lastModifiedBy>"),
        "{core}"
    );
    let types = read_entry(&edited.path, "[Content_Types].xml");
    assert!(
        types.to_ascii_lowercase().contains("docprops/core.xml"),
        "{types}"
    );
    let rels = read_entry(&edited.path, "_rels/.rels");
    assert!(rels.contains("metadata/core-properties"), "{rels}");
    assert!(rels.contains("Target=\"docProps/core.xml\""), "{rels}");
    assert!(rels.contains("Id=\"rId1\""), "{rels}");
    assert_ne!(rels.matches("Id=\"rId1\"").count(), 2, "{rels}");
    let document = read_entry(&edited.path, "word/document.xml");
    assert!(document.contains("本文だけ。"), "{document}");
    assert_docx_xml_well_formed(&edited.path);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn author_arguments_are_rejected_when_they_conflict_or_target_excel() {
    let both = EditRequest::from_args(&json!({
        "creator": "誰か",
        "clear_creator": true,
        "replacements": [{"find": "甲", "replace": "乙"}],
    }));
    assert!(both.unwrap_err().contains("同時に指定できません"));
    let empty = EditRequest::from_args(
        &json!({"creator": "  ", "replacements": [{"find": "甲", "replace": "乙"}]}),
    );
    assert!(empty.unwrap_err().contains("clear_creator"));
    let missing = EditRequest::from_args(&json!({})).unwrap_err();
    assert!(missing.contains("replacements"));

    let dir = temp_dir("author-xlsx");
    let src = dir.join("表.xlsx");
    let req = EditRequest::from_args(&json!({
        "creator": "誰か",
        "replacements": [{"find": "甲", "replace": "乙"}],
    }))
    .unwrap();
    let err = edit_path(&src, &dir.join("out"), "2026-11-20 01-40-00", &req).unwrap_err();
    assert!(err.contains("Word"), "{err}");
    assert!(!dir.join("out").exists());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn asking_again_for_the_same_author_does_not_save_a_file() {
    let dir = temp_dir("author-same");
    let src = docx_with_core(&dir, &core_props("確認係", "確認係"));
    let req = EditRequest::from_args(&json!({"creator": "確認係", "last_modified_by": "確認係"}))
        .unwrap();
    let err = edit_path(&src, &dir.join("out"), "2026-11-20 01-50-00", &req).unwrap_err();
    assert!(err.contains("同じでした"), "{err}");
    assert!(!dir.join("out").exists());
    let _ = fs::remove_dir_all(&dir);
}
