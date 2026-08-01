//! HTML (.html / .htm) text extraction with charset detection.
//!
//! Encoding resolution order:
//! 1. BOM (UTF-8 / UTF-16 LE / UTF-16 BE)
//! 2. `<meta charset>` / Content-Type charset / XML `encoding=`
//! 3. Valid UTF-8
//! 4. Shift_JIS (common for Japanese local HTML)
//! 5. windows-1252 with replacement

use encoding_rs::{Encoding, SHIFT_JIS, UTF_8, WINDOWS_1252};

/// Decode HTML bytes to Unicode text using BOM, meta charset, and Japanese-friendly fallbacks.
pub fn decode_html_bytes(bytes: &[u8]) -> String {
    let (enc, offset) = detect_encoding(bytes);
    let payload = &bytes[offset..];
    let (cow, _, _) = enc.decode(payload);
    cow.into_owned()
}

/// Strip markup and return (optional document title, body text).
pub fn html_to_text(html: &str) -> (Option<String>, String) {
    let without_noise = remove_noise_sections(html);
    let title = extract_title(&without_noise);
    let stripped = strip_tags(&without_noise);
    let text = decode_entities(&stripped);
    let collapsed = collapse_whitespace(&text);
    (title, collapsed)
}

fn detect_encoding(bytes: &[u8]) -> (&'static Encoding, usize) {
    if let Some((enc, len)) = Encoding::for_bom(bytes) {
        return (enc, len);
    }
    if let Some(label) = sniff_charset_label(bytes) {
        if let Some(enc) = Encoding::for_label(label.as_bytes()) {
            return (enc, 0);
        }
        // Common Japanese aliases not always in for_label under older spellings.
        let lower = label.to_ascii_lowercase();
        if matches!(
            lower.as_str(),
            "shift_jis" | "shift-jis" | "sjis" | "x-sjis" | "windows-31j" | "cp932" | "ms932"
        ) {
            return (SHIFT_JIS, 0);
        }
    }
    if std::str::from_utf8(bytes).is_ok() {
        return (UTF_8, 0);
    }
    // Local Japanese HTML without charset is often Shift_JIS.
    let (_, _, sjis_err) = SHIFT_JIS.decode(bytes);
    if !sjis_err {
        return (SHIFT_JIS, 0);
    }
    (WINDOWS_1252, 0)
}

fn sniff_charset_label(bytes: &[u8]) -> Option<String> {
    let n = bytes.len().min(4096);
    let head = ascii_lossy(&bytes[..n]);
    let lower = head.to_ascii_lowercase();

    if let Some(v) = find_meta_charset(&lower, &head) {
        return Some(v);
    }
    if let Some(v) = find_content_type_charset(&lower, &head) {
        return Some(v);
    }
    find_xml_encoding(&lower, &head)
}

fn ascii_lossy(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| if b < 128 { b as char } else { '?' }).collect()
}

fn find_meta_charset(lower: &str, original: &str) -> Option<String> {
    // <meta charset="utf-8"> or <meta charset=utf-8>
    let mut search = 0usize;
    while let Some(rel) = lower[search..].find("charset") {
        let i = search + rel;
        // Prefer "<meta ... charset" over Content-Type's charset= (handled separately).
        let before = &lower[..i];
        if let Some(meta_rel) = before.rfind("<meta") {
            let between = &lower[meta_rel..i];
            if !between.contains('>') {
                return parse_charset_after(original, i);
            }
        }
        search = i + 7;
    }
    None
}

fn find_content_type_charset(lower: &str, original: &str) -> Option<String> {
    let mut search = 0usize;
    while let Some(rel) = lower[search..].find("http-equiv") {
        let i = search + rel;
        let after = &lower[i..];
        let end = after.find('>').unwrap_or(after.len().min(300));
        let tag = &after[..end];
        if tag.contains("content-type") {
            if let Some(cs) = tag.find("charset") {
                let abs = i + cs;
                return parse_charset_after(original, abs);
            }
        }
        search = i + 10;
    }
    None
}

fn find_xml_encoding(lower: &str, original: &str) -> Option<String> {
    let start = lower.find("<?xml")?;
    let slice = &lower[start..];
    let end = slice.find("?>").unwrap_or(slice.len().min(200));
    let decl = &slice[..end];
    let enc_rel = decl.find("encoding")?;
    parse_charset_after(original, start + enc_rel)
}

/// `pos` points at the start of "charset" or "encoding" in `original`.
fn parse_charset_after(original: &str, pos: usize) -> Option<String> {
    let rest = &original[pos..];
    let eq = rest.find('=')?;
    let mut chars = rest[eq + 1..].chars().peekable();
    while matches!(chars.peek(), Some(c) if c.is_whitespace()) {
        chars.next();
    }
    let first = chars.next()?;
    let value: String = if first == '"' || first == '\'' {
        let quote = first;
        chars.take_while(|&c| c != quote).collect()
    } else {
        std::iter::once(first)
            .chain(chars.take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
            .collect()
    };
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn remove_noise_sections(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len());
    let mut i = 0usize;
    let html_chars: Vec<char> = html.chars().collect();
    let lower_chars: Vec<char> = lower.chars().collect();
    let len = html_chars.len();

    while i < len {
        if starts_with_chars(&lower_chars, i, &['<', '!', '-', '-']) {
            if let Some(end) = find_chars(&lower_chars, i + 4, &['-', '-', '>']) {
                i = end + 3;
                continue;
            }
        }
        if let Some(end) = match_open_close_chars(&lower_chars, i, "script") {
            i = end;
            continue;
        }
        if let Some(end) = match_open_close_chars(&lower_chars, i, "style") {
            i = end;
            continue;
        }
        if let Some(end) = match_open_close_chars(&lower_chars, i, "noscript") {
            i = end;
            continue;
        }
        out.push(html_chars[i]);
        i += 1;
    }
    out
}

fn starts_with_chars(hay: &[char], i: usize, needle: &[char]) -> bool {
    if i + needle.len() > hay.len() {
        return false;
    }
    &hay[i..i + needle.len()] == needle
}

fn find_chars(hay: &[char], start: usize, needle: &[char]) -> Option<usize> {
    if needle.is_empty() || start >= hay.len() {
        return None;
    }
    (start..=hay.len().saturating_sub(needle.len())).find(|&i| &hay[i..i + needle.len()] == needle)
}

fn match_open_close_chars(hay: &[char], i: usize, tag: &str) -> Option<usize> {
    if hay.get(i) != Some(&'<') {
        return None;
    }
    let tag_chars: Vec<char> = tag.chars().collect();
    let j = i + 1;
    if hay.get(j) == Some(&'/') {
        return None;
    }
    if j + tag_chars.len() > hay.len() {
        return None;
    }
    for (k, tc) in tag_chars.iter().enumerate() {
        let c = hay[j + k];
        if c.to_ascii_lowercase() != *tc {
            return None;
        }
    }
    let after = j + tag_chars.len();
    let boundary = hay.get(after).copied().unwrap_or('\0');
    if !matches!(boundary, '>' | '/' | ' ' | '\t' | '\n' | '\r') {
        return None;
    }
    let open_end = find_chars(hay, after, &['>'])? + 1;

    let mut close: Vec<char> = vec!['<', '/'];
    close.extend(tag_chars.iter().copied());
    let mut search = open_end;
    while let Some(pos) = find_chars_ci(hay, search, &close) {
        let after_tag = pos + close.len();
        let b = hay.get(after_tag).copied().unwrap_or('\0');
        if matches!(b, '>' | ' ' | '\t' | '\n' | '\r') {
            return find_chars(hay, after_tag, &['>']).map(|e| e + 1);
        }
        search = pos + 2;
    }
    Some(hay.len())
}

fn find_chars_ci(hay: &[char], start: usize, needle: &[char]) -> Option<usize> {
    if needle.is_empty() || start >= hay.len() {
        return None;
    }
    (start..=hay.len().saturating_sub(needle.len())).find(|&i| {
        hay[i..i + needle.len()]
            .iter()
            .zip(needle.iter())
            .all(|(a, b)| a.to_ascii_lowercase() == b.to_ascii_lowercase())
    })
}

fn extract_title(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let start = lower.find("<title")?;
    let after = html[start..].find('>')? + start + 1;
    let end_rel = lower[after..].find("</title")?;
    let raw = &html[after..after + end_rel];
    let text = collapse_whitespace(&decode_entities(&strip_tags(raw)));
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut last_was_space = true;
    for ch in html.chars() {
        match ch {
            '<' => {
                in_tag = true;
            }
            '>' => {
                in_tag = false;
                if !last_was_space {
                    out.push(' ');
                    last_was_space = true;
                }
            }
            _ if !in_tag => {
                if ch.is_whitespace() {
                    if !last_was_space {
                        out.push(' ');
                        last_was_space = true;
                    }
                } else {
                    out.push(ch);
                    last_was_space = false;
                }
            }
            _ => {}
        }
    }
    out
}

fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '&' {
            out.push(ch);
            continue;
        }
        let mut ent = String::new();
        let mut found_semi = false;
        while let Some(&c) = chars.peek() {
            if c == ';' {
                chars.next();
                found_semi = true;
                break;
            }
            if ent.len() > 32 || c.is_whitespace() || c == '&' {
                break;
            }
            ent.push(c);
            chars.next();
        }
        if found_semi {
            if let Some(decoded) = entity_to_char(&ent) {
                out.push(decoded);
                continue;
            }
            out.push('&');
            out.push_str(&ent);
            out.push(';');
        } else {
            out.push('&');
            out.push_str(&ent);
        }
    }
    out
}

fn entity_to_char(name: &str) -> Option<char> {
    if let Some(rest) = name.strip_prefix('#') {
        let code = if let Some(hex) = rest.strip_prefix('x').or_else(|| rest.strip_prefix('X')) {
            u32::from_str_radix(hex, 16).ok()?
        } else {
            rest.parse::<u32>().ok()?
        };
        return char::from_u32(code);
    }
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => '\u{00A0}',
        "copy" => '©',
        "reg" => '®',
        "trade" => '™',
        "mdash" => '—',
        "ndash" => '–',
        "hellip" => '…',
        "laquo" => '«',
        "raquo" => '»',
        "times" => '×',
        "divide" => '÷',
        "yen" => '¥',
        "euro" => '€',
        _ => return None,
    })
}

fn collapse_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last_was_space = true;
    for ch in text.chars() {
        if ch.is_whitespace() {
            if !last_was_space {
                out.push(' ');
                last_was_space = true;
            }
        } else {
            out.push(ch);
            last_was_space = false;
        }
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use encoding_rs::SHIFT_JIS;

    #[test]
    fn utf8_meta_and_entities() {
        let html = r#"<!DOCTYPE html>
<html><head><meta charset="utf-8"><title>契約&amp;損害賠償</title>
<style>body{color:red}</style>
<script>var x = "秘密";</script>
</head><body><p>本文の&nbsp;テスト</p></body></html>"#;
        let (title, text) = html_to_text(html);
        assert_eq!(title.as_deref(), Some("契約&損害賠償"));
        assert!(text.contains("本文の"));
        assert!(text.contains("テスト"));
        assert!(!text.contains("秘密"));
        assert!(!text.contains("color"));
    }

    #[test]
    fn shift_jis_meta_charset() {
        let html = String::from(
            r#"<html><head><meta http-equiv="Content-Type" content="text/html; charset=Shift_JIS">
<title>"#,
        );
        let (title_bytes, _, _) = SHIFT_JIS.encode("日本語タイトル");
        // Build mixed: ASCII + SJIS body
        let mut bytes = html.into_bytes();
        bytes.extend_from_slice(&title_bytes);
        bytes.extend_from_slice(
            br#"</title></head><body><p>"#,
        );
        let (body_bytes, _, _) = SHIFT_JIS.encode("検索対象の本文です");
        bytes.extend_from_slice(&body_bytes);
        bytes.extend_from_slice(br#"</p></body></html>"#);

        let decoded = decode_html_bytes(&bytes);
        let (title, text) = html_to_text(&decoded);
        assert_eq!(title.as_deref(), Some("日本語タイトル"));
        assert!(text.contains("検索対象の本文です"));
    }

    #[test]
    fn bom_utf8() {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(
            br#"<html><head><title>BOM</title></head><body>hello</body></html>"#,
        );
        let decoded = decode_html_bytes(&bytes);
        let (title, text) = html_to_text(&decoded);
        assert_eq!(title.as_deref(), Some("BOM"));
        assert!(text.contains("hello"));
    }

    #[test]
    fn sjis_without_charset_falls_back() {
        let (body, _, _) = SHIFT_JIS.encode(
            "<html><head><title>無指定</title></head><body>シフトJIS本文</body></html>",
        );
        let decoded = decode_html_bytes(&body);
        let (title, text) = html_to_text(&decoded);
        assert_eq!(title.as_deref(), Some("無指定"));
        assert!(text.contains("シフトJIS本文"));
    }

    #[test]
    fn numeric_entities() {
        let s = decode_entities("A&#65;B&#x41;C");
        assert_eq!(s, "AABAC");
    }
}
