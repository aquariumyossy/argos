//! Characters that do not belong in Japanese text.
//!
//! The allowlist and the script ranges are the ones LexCrew-Doc ships in
//! `src/shared/japaneseHan.ts`. Latin and Greek stay allowed. A CJK ideograph
//! is allowed only when the bitmap says it is used in Japanese.

#[path = "japanese_han_data.rs"]
mod japanese_han_data;

use std::collections::HashSet;
use std::sync::OnceLock;

use base64::Engine;
use serde_json::Value;

use japanese_han_data::{HAN_BMP_A, HAN_BMP_A0, HAN_BMP_B, HAN_BMP_B0, HAN_EXTRA};

const SHOWN_LIMIT: usize = 12;

struct Tables {
    bmp_a: Vec<u8>,
    bmp_b: Vec<u8>,
    extra: HashSet<u32>,
}

fn tables() -> &'static Tables {
    static TABLES: OnceLock<Tables> = OnceLock::new();
    TABLES.get_or_init(|| {
        let decode = |raw: &str| {
            base64::engine::general_purpose::STANDARD
                .decode(raw)
                .expect("japanese han bitmap")
        };
        Tables {
            bmp_a: decode(HAN_BMP_A),
            bmp_b: decode(HAN_BMP_B),
            extra: HAN_EXTRA.iter().copied().collect(),
        }
    })
}

fn bit_at(table: &[u8], index: u32) -> bool {
    let index = index as usize;
    table
        .get(index >> 3)
        .is_some_and(|byte| byte & (1 << (index & 7)) != 0)
}

/// CJK unified / compatibility ideographs, including extensions.
pub fn is_cjk_ideograph(cp: u32) -> bool {
    (0x3400..=0x4dbf).contains(&cp)
        || (0x4e00..=0x9fff).contains(&cp)
        || (0xf900..=0xfaff).contains(&cp)
        || (0x20000..=0x2a6df).contains(&cp)
        || (0x2a700..=0x2b73f).contains(&cp)
        || (0x2b740..=0x2b81f).contains(&cp)
        || (0x2b820..=0x2ceaf).contains(&cp)
        || (0x2ceb0..=0x2ebef).contains(&cp)
        || (0x30000..=0x3134f).contains(&cp)
        || (0x31350..=0x323af).contains(&cp)
}

pub fn is_japanese_han(cp: u32) -> bool {
    let tables = tables();
    if cp >= HAN_BMP_A0 && cp < HAN_BMP_A0 + (tables.bmp_a.len() as u32) * 8 {
        return bit_at(&tables.bmp_a, cp - HAN_BMP_A0);
    }
    if cp >= HAN_BMP_B0 && cp < HAN_BMP_B0 + (tables.bmp_b.len() as u32) * 8 {
        return bit_at(&tables.bmp_b, cp - HAN_BMP_B0);
    }
    tables.extra.contains(&cp)
}

/// Writing systems that have no place in a Japanese legal document.
pub fn is_foreign_script(cp: u32) -> bool {
    (0x0400..=0x052f).contains(&cp)
        || (0x0590..=0x05ff).contains(&cp)
        || (0x0600..=0x06ff).contains(&cp)
        || (0x0700..=0x077f).contains(&cp)
        || (0x0900..=0x097f).contains(&cp)
        || (0x0e00..=0x0e7f).contains(&cp)
        || (0x1100..=0x11ff).contains(&cp)
        || (0x2e80..=0x2fdf).contains(&cp)
        || (0x3100..=0x312f).contains(&cp)
        || (0x3130..=0x318f).contains(&cp)
        || (0xa960..=0xa97f).contains(&cp)
        || (0xac00..=0xd7ff).contains(&cp)
        || (0xfb50..=0xfdff).contains(&cp)
        || (0xfe70..=0xfefc).contains(&cp)
        || (0xffa0..=0xffdc).contains(&cp)
}

fn is_foreign_char(ch: char) -> bool {
    let cp = ch as u32;
    is_foreign_script(cp) || (is_cjk_ideograph(cp) && !is_japanese_han(cp))
}

/// Distinct glyphs that do not belong in Japanese text, in first-seen order.
pub fn foreign_chars(parts: &[impl AsRef<str>]) -> Vec<char> {
    let mut seen = HashSet::new();
    let mut ordered = Vec::new();
    for part in parts {
        for ch in part.as_ref().chars() {
            if !seen.insert(ch) || !is_foreign_char(ch) {
                continue;
            }
            ordered.push(ch);
        }
    }
    ordered
}

fn format_chars(chars: &[char]) -> String {
    let shown: String = chars
        .iter()
        .take(SHOWN_LIMIT)
        .map(|ch| ch.to_string())
        .collect::<Vec<_>>()
        .join("、");
    let more = if chars.len() > SHOWN_LIMIT {
        format!(" ほか{}字", chars.len() - SHOWN_LIMIT)
    } else {
        String::new()
    };
    format!("{shown}{more}")
}

/// Chat-pane notice. Empty or Japanese-only text returns `None`.
pub fn foreign_char_notice(parts: &[impl AsRef<str>]) -> Option<String> {
    let chars = foreign_chars(parts);
    if chars.is_empty() {
        None
    } else {
        Some(format!("日本語で用いない文字: {}", format_chars(&chars)))
    }
}

/// Model-authored strings a write tool would put into a file or a note.
///
/// `find` and `match` are the document text the tool has to locate, so they
/// are not the model's own writing. A heading used to find a note section is
/// the same kind of locator.
pub fn authored_tool_texts(name: &str, args: &Value) -> Vec<String> {
    match name {
        "edit_office" => edit_office_authored(args),
        "create_docx" => docx_authored(args),
        "create_xlsx" => xlsx_authored(args),
        "create_html" | "edit_html" => string_field(args, "html").into_iter().collect(),
        "create_note" | "write_note" => {
            let mut out = Vec::new();
            push_field(&mut out, args, "title");
            push_field(&mut out, args, "memo");
            out
        }
        _ => Vec::new(),
    }
}

/// `None` when this call may run. The sentence is the tool result body.
pub fn foreign_char_tool_error(name: &str, args: &Value) -> Option<String> {
    let chars = foreign_chars(&authored_tool_texts(name, args));
    if chars.is_empty() {
        None
    } else {
        Some(format!(
            "日本語で用いない文字が含まれています（{}）。その部分を日本語（漢字の新字体・ひらがな・カタカナ）で書き直してください。",
            format_chars(&chars)
        ))
    }
}

fn edit_office_authored(args: &Value) -> Vec<String> {
    let mut out = Vec::new();
    for item in objects_of(args.get("replacements")) {
        push_field(&mut out, &item, "replace");
        push_field(&mut out, &item, "comment");
    }
    for item in objects_of(args.get("paragraphs")) {
        push_field(&mut out, &item, "text");
    }
    out
}

fn docx_authored(args: &Value) -> Vec<String> {
    let mut out = Vec::new();
    for block in objects_of(args.get("blocks")) {
        push_field(&mut out, &block, "text");
        if let Some(items) = block.get("items").and_then(|value| value.as_array()) {
            for item in items {
                push_string(&mut out, item);
            }
        }
        push_rows(&mut out, block.get("rows"));
    }
    out
}

fn xlsx_authored(args: &Value) -> Vec<String> {
    let mut out = Vec::new();
    for sheet in objects_of(args.get("sheets")) {
        push_field(&mut out, &sheet, "name");
        push_rows(&mut out, sheet.get("rows"));
    }
    out
}

fn push_rows(out: &mut Vec<String>, rows: Option<&Value>) {
    let Some(rows) = rows.and_then(|value| value.as_array()) else {
        return;
    };
    for row in rows {
        let Some(cells) = row.as_array() else {
            push_string(out, row);
            continue;
        };
        for cell in cells {
            push_string(out, cell);
        }
    }
}

fn push_field(out: &mut Vec<String>, obj: &Value, key: &str) {
    push_string(out, obj.get(key).unwrap_or(&Value::Null));
}

fn push_string(out: &mut Vec<String>, value: &Value) {
    if let Some(text) = value.as_str() {
        if !text.is_empty() {
            out.push(text.to_string());
        }
    }
}

fn string_field(obj: &Value, key: &str) -> Option<String> {
    obj.get(key)
        .and_then(|value| value.as_str())
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn objects_of(value: Option<&Value>) -> Vec<Value> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn chars(text: &str) -> Vec<char> {
        foreign_chars(&[text])
    }

    #[test]
    fn ignores_japanese_legal_prose() {
        assert!(chars("第1条　甲は乙に対し、瑕疵を理由に契約を解除できる。").is_empty());
        assert!(chars("損害賠償　原告　被告　謄本　捺印").is_empty());
    }

    #[test]
    fn flags_simplified_chinese_that_is_not_japanese_running_text() {
        // 个 is a JIS X 0208 kanji, so it stays. 这问题 are not.
        assert_eq!(chars("这个合同的问题"), vec!['这', '问', '题']);
        assert_eq!(chars("门发经问题"), vec!['门', '发', '经', '问', '题']);
        // 从 is JIS X 0208. The other simplified forms are not.
        assert_eq!(chars("为了对从开关"), vec!['为', '对', '开', '关']);
        assert_eq!(
            chars("们这说话门问题"),
            vec!['们', '这', '说', '话', '门', '问', '题']
        );
    }

    #[test]
    fn flags_hangul() {
        assert_eq!(
            chars("権利侵害の主張 기타 クレームについては"),
            vec!['기', '타']
        );
        assert_eq!(chars("계약"), vec!['계', '약']);
        assert_eq!(chars("ﾯ"), vec!['ﾯ']);
    }

    #[test]
    fn flags_other_scripts() {
        assert_eq!(chars("Привет"), vec!['П', 'р', 'и', 'в', 'е', 'т']);
        assert!(!chars("สัญญา").is_empty());
    }

    #[test]
    fn flags_kangxi_radicals_that_look_like_kanji() {
        assert_eq!(chars("⼀"), vec!['⼀']);
        assert!(chars("一").is_empty());
    }

    #[test]
    fn keeps_shared_shinjitai_and_japanese_kyujitai() {
        assert!(chars("国会学体来与門発國學").is_empty());
    }

    #[test]
    fn keeps_jis_extended_shinjitai() {
        assert!(chars("嘘をつく").is_empty());
        assert!(chars("侠客の躯を掴み、麹と蝉と泪").is_empty());
        assert!(is_japanese_han('嘘' as u32));
        assert!(is_japanese_han('噓' as u32));
        assert!(is_japanese_han('個' as u32));
        // These look simplified and are still JIS X 0208.
        assert!(is_japanese_han('个' as u32));
        assert!(is_japanese_han('无' as u32));
        assert!(is_japanese_han('从' as u32));
        assert!(is_japanese_han('广' as u32));
    }

    #[test]
    fn ignores_kana_punctuation_latin_and_greek() {
        assert!(chars("はい。OK 123 「引用」").is_empty());
        assert!(chars("α版　μm　ＡＩツール　ｱｲｳ").is_empty());
        assert!(chars("https://example.com/a?b=1").is_empty());
        assert!(!is_cjk_ideograph('あ' as u32));
    }

    #[test]
    fn dedupes_in_first_seen_order() {
        assert_eq!(foreign_chars(&["这这", "问这"]), vec!['这', '问']);
    }

    #[test]
    fn notice_lists_the_glyphs() {
        assert!(foreign_char_notice(&["条項を点検しました。"]).is_none());
        assert_eq!(
            foreign_char_notice(&["这个条款。"]).as_deref(),
            Some("日本語で用いない文字: 这")
        );
        assert_eq!(
            foreign_char_notice(&["主張 기타 クレーム"]).as_deref(),
            Some("日本語で用いない文字: 기、타")
        );
    }

    #[test]
    fn edit_office_checks_replacement_and_comment_but_not_find() {
        let find_only = json!({
            "replacements": [{"find": "这个合同", "replace": "本契約", "comment": ""}]
        });
        assert!(foreign_char_tool_error("edit_office", &find_only).is_none());
        let paragraph_match = json!({
            "paragraphs": [{"match": "这个", "text": "本契約とする。"}]
        });
        assert!(foreign_char_tool_error("edit_office", &paragraph_match).is_none());
        let written = json!({
            "replacements": [{"find": "甲", "replace": "这个", "comment": "확인"}]
        });
        let error = foreign_char_tool_error("edit_office", &written).unwrap();
        assert!(error.contains('这'));
        assert!(error.contains('확'));
        assert!(error.contains("書き直してください"));
    }

    #[test]
    fn write_tools_check_authored_text_and_reads_are_ignored() {
        let docx = json!({
            "filename": "这个",
            "blocks": [{"type": "paragraph", "text": "本文", "items": ["甲"]}]
        });
        assert!(foreign_char_tool_error("create_docx", &docx).is_none());
        let docx_bad = json!({
            "filename": "メモ",
            "blocks": [{"type": "bullets", "items": ["这个"]}]
        });
        assert!(foreign_char_tool_error("create_docx", &docx_bad)
            .unwrap()
            .contains('这'));
        let sheet = json!({
            "sheets": [{"name": "売上", "rows": [["甲", "这个"]]}]
        });
        assert!(foreign_char_tool_error("create_xlsx", &sheet)
            .unwrap()
            .contains('这'));
        let html = json!({"filename": "这个", "html": "<p>計算</p>"});
        assert!(foreign_char_tool_error("create_html", &html).is_none());
        assert!(
            foreign_char_tool_error("edit_html", &json!({"html": "<p>这个</p>"}))
                .unwrap()
                .contains('这')
        );
        assert!(
            foreign_char_tool_error("write_note", &json!({"heading": "这个", "memo": "本文"}))
                .is_none()
        );
        assert!(
            foreign_char_tool_error("create_note", &json!({"title": "メモ", "memo": "기타"}))
                .unwrap()
                .contains('기')
        );
        assert!(
            foreign_char_tool_error("search_index", &json!({"query": "最高人民法院"})).is_none()
        );
        assert!(foreign_char_tool_error("read_unit", &json!({"articles": "这个"})).is_none());
    }

    #[test]
    fn jis_x_0208_and_cp932_kanji_and_tsuchi_yoshi_are_allowed() {
        // encoding_rs SHIFT_JIS is Windows-31J (cp932). Python's shift_jis
        // kanji are a subset of that set, so this covers JIS X 0208 as well.
        let mut count = 0u32;
        let leads = (0x81u8..0xA0).chain(0xE0u8..0xFD);
        let seconds = (0x40u8..0x7F).chain(0x80u8..0xFD);
        for lead in leads {
            for second in seconds.clone() {
                let bytes = [lead, second];
                let (text, _, failed) = encoding_rs::SHIFT_JIS.decode(&bytes);
                if failed {
                    continue;
                }
                let mut chars_in = text.chars();
                let Some(ch) = chars_in.next() else {
                    continue;
                };
                if chars_in.next().is_some() || !is_cjk_ideograph(ch as u32) {
                    continue;
                }
                assert!(
                    is_japanese_han(ch as u32),
                    "U+{:04X} {ch} from CP932 {:02X}{:02X}",
                    ch as u32,
                    lead,
                    second
                );
                count += 1;
            }
        }
        assert!(count >= 6_356, "JIS X 0208 has 6356 kanji, saw {count}");
        assert!(count >= 6_716, "CP932 has 6716 kanji, saw {count}");
        assert!(is_japanese_han(0x9AD9), "髙");
        assert!(is_japanese_han(0x20BB7), "𠮷");
        assert!(chars("髙橋𠮷田姜砺薮").is_empty());
        for ch in ['们', '这', '说', '话', '门', '问', '题'] {
            assert!(!is_japanese_han(ch as u32), "{ch}");
        }
        assert_eq!(chars("기타"), vec!['기', '타']);
    }

    #[test]
    fn typescript_allowlist_matches_the_rust_one() {
        let ts = include_str!("../../../src/features/chat/japaneseHanData.ts");
        let a = quoted_after(ts, "HAN_BMP_A =");
        let b = quoted_after(ts, "HAN_BMP_B =");
        assert_eq!(a, super::japanese_han_data::HAN_BMP_A);
        assert_eq!(b, super::japanese_han_data::HAN_BMP_B);
        let start = ts.find("HAN_EXTRA = [").unwrap();
        let end = ts[start..].find("];").unwrap();
        let nums: Vec<u32> = ts[start..start + end]
            .split(|ch: char| !ch.is_ascii_digit())
            .filter(|item| !item.is_empty())
            .map(|item| item.parse().unwrap())
            .collect();
        assert_eq!(nums, super::japanese_han_data::HAN_EXTRA);
    }

    fn quoted_after<'a>(text: &'a str, marker: &str) -> &'a str {
        let from = text.find(marker).unwrap() + marker.len();
        let rest = text[from..].trim_start();
        let rest = rest.strip_prefix('"').unwrap();
        let end = rest.find('"').unwrap();
        &rest[..end]
    }
}
