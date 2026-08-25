//! ATX heading split for note memos. Headings inside fenced code are ignored.

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    /// Heading text without leading `#`. Empty for the preamble before the first heading.
    pub heading: String,
    /// 0 = preamble, 1–6 = ATX level.
    pub level: u8,
    /// Full section text, including the heading line when present.
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SectionError {
    Missing,
    Duplicate,
}

impl SectionError {
    pub fn message(&self, heading: &str) -> String {
        match self {
            SectionError::Missing => format!("見出し「{heading}」はありません。"),
            SectionError::Duplicate => {
                format!("見出し「{heading}」が複数あります。より具体的な見出しを指定してください。")
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiffLine {
    pub kind: &'static str,
    pub text: String,
}

pub fn outline(md: &str) -> Vec<String> {
    split_sections(md)
        .into_iter()
        .filter(|s| s.level > 0)
        .map(|s| s.heading)
        .collect()
}

pub fn split_sections(md: &str) -> Vec<Section> {
    let lines: Vec<&str> = md.split_inclusive('\n').collect();
    if lines.is_empty() {
        return vec![Section {
            heading: String::new(),
            level: 0,
            text: String::new(),
        }];
    }
    let mut sections: Vec<Section> = Vec::new();
    let mut cur_heading = String::new();
    let mut cur_level: u8 = 0;
    let mut buf = String::new();
    let mut fence: Option<(char, usize)> = None;

    let flush = |heading: &str, level: u8, buf: &mut String, out: &mut Vec<Section>| {
        out.push(Section {
            heading: heading.to_string(),
            level,
            text: std::mem::take(buf),
        });
    };

    for line in &lines {
        let body = line.trim_end_matches(['\r', '\n']);
        if let Some((ch, n)) = fence {
            buf.push_str(line);
            if is_fence_close(body, ch, n) {
                fence = None;
            }
            continue;
        }
        if let Some(open) = parse_fence_open(body) {
            fence = Some(open);
            buf.push_str(line);
            continue;
        }
        if let Some((level, title)) = parse_atx_heading(body) {
            if !buf.is_empty() || !sections.is_empty() || cur_level > 0 {
                flush(&cur_heading, cur_level, &mut buf, &mut sections);
            }
            cur_heading = title;
            cur_level = level;
            buf.push_str(line);
            continue;
        }
        buf.push_str(line);
    }
    flush(&cur_heading, cur_level, &mut buf, &mut sections);
    if sections.is_empty() {
        sections.push(Section {
            heading: String::new(),
            level: 0,
            text: md.to_string(),
        });
    }
    sections
}

pub fn find_section<'a>(sections: &'a [Section], heading: &str) -> Result<&'a Section, SectionError> {
    let want = normalize_heading(heading);
    if want.is_empty() {
        return Err(SectionError::Missing);
    }
    let hits: Vec<&Section> = sections
        .iter()
        .filter(|s| s.level > 0 && normalize_heading(&s.heading) == want)
        .collect();
    match hits.len() {
        0 => Err(SectionError::Missing),
        1 => Ok(hits[0]),
        _ => Err(SectionError::Duplicate),
    }
}

/// Replace the named section. `new_text` is the full section (heading line + body).
pub fn replace_section(md: &str, heading: &str, new_text: &str) -> Result<String, SectionError> {
    let sections = split_sections(md);
    let _ = find_section(&sections, heading)?;
    let want = normalize_heading(heading);
    let mut out = String::new();
    for s in sections {
        if s.level > 0 && normalize_heading(&s.heading) == want {
            out.push_str(&ensure_trailing_nl(new_text));
        } else {
            out.push_str(&s.text);
        }
    }
    Ok(out)
}

/// Append `chunk` at the end of the named section, or at the document end when `heading` is None.
pub fn append_chunk(md: &str, heading: Option<&str>, chunk: &str) -> Result<String, SectionError> {
    let chunk = chunk.trim_end();
    if chunk.is_empty() {
        return Ok(md.to_string());
    }
    let piece = format_append_chunk(chunk);
    let Some(h) = heading.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(append_at_end(md, &piece));
    };
    let sections = split_sections(md);
    match find_section(&sections, h) {
        Ok(_) => {
            let want = normalize_heading(h);
            let mut out = String::new();
            for s in sections {
                if s.level > 0 && normalize_heading(&s.heading) == want {
                    let mut text = s.text;
                    if !text.ends_with('\n') {
                        text.push('\n');
                    }
                    text.push_str(&piece);
                    out.push_str(&text);
                } else {
                    out.push_str(&s.text);
                }
            }
            Ok(out)
        }
        Err(SectionError::Duplicate) => Err(SectionError::Duplicate),
        Err(SectionError::Missing) => Ok(append_at_end(md, &format_insert_section(h, chunk))),
    }
}

/// Insert a new heading section at the end.
pub fn insert_section(md: &str, heading: &str, body: &str) -> String {
    append_at_end(md, &format_insert_section(heading, body))
}

pub fn format_insert_section(heading: &str, body: &str) -> String {
    let h = heading.trim().trim_start_matches('#').trim();
    let mut s = format!("## {h}\n");
    let b = body.trim();
    if !b.is_empty() {
        s.push('\n');
        s.push_str(b);
        if !b.ends_with('\n') {
            s.push('\n');
        }
    }
    s
}

pub fn format_append_chunk(chunk: &str) -> String {
    let t = chunk.trim_end();
    if t.is_empty() {
        return String::new();
    }
    let mut s = String::new();
    s.push('\n');
    s.push_str(t);
    if !t.ends_with('\n') {
        s.push('\n');
    }
    s
}

/// Undo an append when `md` still ends with `chunk` (with the same wrapping as `format_append_chunk`).
pub fn undo_append(md: &str, chunk: &str) -> Result<String, String> {
    let piece = format_append_chunk(chunk);
    if piece.is_empty() {
        return Err("取り消す追記が空です。".into());
    }
    if let Some(stripped) = md.strip_suffix(&piece) {
        return Ok(stripped.to_string());
    }
    let alt = format!("{piece}\n");
    if let Some(stripped) = md.strip_suffix(&alt) {
        return Ok(stripped.to_string());
    }
    Err("メモ末尾が追記と一致しないため取り消せません。".into())
}

pub fn line_diff(old: &str, new: &str) -> Vec<DiffLine> {
    let a: Vec<&str> = split_lines(old);
    let b: Vec<&str> = split_lines(new);
    let table = lcs_table(&a, &b);
    let mut out = Vec::new();
    backtrack(&a, &b, &table, a.len(), b.len(), &mut out);
    out
}

fn split_lines(s: &str) -> Vec<&str> {
    if s.is_empty() {
        return Vec::new();
    }
    s.split('\n').collect()
}

fn lcs_table(a: &[&str], b: &[&str]) -> Vec<Vec<usize>> {
    let mut t = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for i in 0..a.len() {
        for j in 0..b.len() {
            t[i + 1][j + 1] = if a[i] == b[j] {
                t[i][j] + 1
            } else {
                t[i][j + 1].max(t[i + 1][j])
            };
        }
    }
    t
}

fn backtrack(
    a: &[&str],
    b: &[&str],
    t: &[Vec<usize>],
    i: usize,
    j: usize,
    out: &mut Vec<DiffLine>,
) {
    if i > 0 && j > 0 && a[i - 1] == b[j - 1] {
        backtrack(a, b, t, i - 1, j - 1, out);
        out.push(DiffLine {
            kind: "eq",
            text: a[i - 1].to_string(),
        });
    } else if j > 0 && (i == 0 || t[i][j - 1] >= t[i - 1][j]) {
        backtrack(a, b, t, i, j - 1, out);
        out.push(DiffLine {
            kind: "add",
            text: b[j - 1].to_string(),
        });
    } else if i > 0 {
        backtrack(a, b, t, i - 1, j, out);
        out.push(DiffLine {
            kind: "del",
            text: a[i - 1].to_string(),
        });
    }
}

fn append_at_end(md: &str, piece: &str) -> String {
    if piece.is_empty() {
        return md.to_string();
    }
    let mut out = md.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(piece);
    out
}

fn ensure_trailing_nl(s: &str) -> String {
    if s.is_empty() || s.ends_with('\n') {
        s.to_string()
    } else {
        format!("{s}\n")
    }
}

pub fn normalize_heading(raw: &str) -> String {
    raw.trim()
        .trim_start_matches('#')
        .trim()
        .trim_end_matches('#')
        .trim()
        .to_string()
}

pub fn parse_atx_heading(line: &str) -> Option<(u8, String)> {
    let t = line.trim_end();
    if t.starts_with("    ") || t.starts_with('\t') {
        return None;
    }
    let bytes = t.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() && bytes[i] == b'#' {
        i += 1;
    }
    if i == 0 || i > 6 {
        return None;
    }
    if i < bytes.len() && bytes[i] != b' ' && bytes[i] != b'\t' {
        return None;
    }
    let title = t[i..].trim().trim_end_matches('#').trim();
    if title.is_empty() {
        return None;
    }
    Some((i as u8, title.to_string()))
}

fn parse_fence_open(line: &str) -> Option<(char, usize)> {
    let t = line.trim_start_matches(|c: char| c == ' ' || c == '\t');
    let ch = t.chars().next()?;
    if ch != '`' && ch != '~' {
        return None;
    }
    let n = t.chars().take_while(|c| *c == ch).count();
    if n < 3 {
        return None;
    }
    Some((ch, n))
}

fn is_fence_close(line: &str, ch: char, n: usize) -> bool {
    let t = line.trim_start_matches(|c: char| c == ' ' || c == '\t');
    if !t.chars().all(|c| c == ch) {
        return false;
    }
    t.chars().count() >= n
}

pub fn heading_line(level: u8, title: &str) -> String {
    let n = level.clamp(1, 6) as usize;
    format!("{} {}\n", "#".repeat(n), title.trim())
}

/// Ensure `text` is a full section including the ATX heading line.
pub fn normalize_section_text(heading: &str, text: &str, level: u8) -> String {
    let heading = normalize_heading(heading);
    let text = text.trim_start_matches('\u{feff}').to_string();
    let first = text.lines().next().unwrap_or("");
    if let Some((_, title)) = parse_atx_heading(first) {
        if normalize_heading(&title) == heading {
            return ensure_trailing_nl(&text);
        }
    }
    let mut out = heading_line(level, &heading);
    let body = text.trim();
    if !body.is_empty() {
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
        out.push_str(body);
        if !body.ends_with('\n') {
            out.push('\n');
        }
    }
    out
}

pub fn cap_chars(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push_str("…");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_atx_and_skips_fence() {
        let md = "# 争点\n本文\n\n```\n# 偽\n```\n\n## 日程\n- [ ] 提出 @2026-09-01\n";
        let secs = split_sections(md);
        assert_eq!(secs.len(), 2);
        assert_eq!(secs[0].heading, "争点");
        assert!(secs[0].text.contains("```"));
        assert!(secs[0].text.contains("# 偽"));
        assert_eq!(secs[1].heading, "日程");
        assert_eq!(outline(md), vec!["争点".to_string(), "日程".to_string()]);
    }

    #[test]
    fn duplicate_heading_errors() {
        let md = "## 日程\na\n## 日程\nb\n";
        let secs = split_sections(md);
        assert!(matches!(
            find_section(&secs, "日程"),
            Err(SectionError::Duplicate)
        ));
    }

    #[test]
    fn missing_append_inserts_section() {
        let md = "# 争点\n\n";
        let out = append_chunk(md, Some("日程"), "- [ ] 期限 @2026-09-01").unwrap();
        assert!(out.contains("## 日程"));
        assert!(out.contains("@2026-09-01"));
        assert!(out.contains("# 争点"));
    }

    #[test]
    fn replace_keeps_other_sections() {
        let md = "# 争点\nA\n\n# 日程\nB\n";
        let out = replace_section(md, "日程", "# 日程\nC\n").unwrap();
        assert!(out.contains("# 争点\nA"));
        assert!(out.contains("# 日程\nC"));
        assert!(!out.contains("# 日程\nB"));
    }

    #[test]
    fn undo_append_only_if_suffix_matches() {
        let chunk = "追記行";
        let md = append_chunk("既存\n", None, chunk).unwrap();
        assert_eq!(undo_append(&md, chunk).unwrap(), "既存\n");
        assert!(undo_append("既存\n人が書いた\n", chunk).is_err());
    }

    #[test]
    fn line_diff_marks_add_and_del() {
        let d = line_diff("a\nb\n", "a\nc\n");
        let kinds: Vec<_> = d.iter().map(|l| l.kind).collect();
        assert_eq!(kinds, ["eq", "del", "add", "eq"]);
    }

    #[test]
    fn caps_long_excerpt() {
        let s = cap_chars(&"あ".repeat(3000), 100);
        assert_eq!(s.chars().count(), 101);
        assert!(s.ends_with('…'));
    }
}
