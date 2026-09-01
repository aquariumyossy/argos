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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiffHunk {
    pub gap_before: usize,
    pub lines: Vec<DiffLine>,
}

/// Full-memo `read_note` / `write_note` without `heading` is refused above this.
pub const NOTE_FULL_CHAR_CAP: usize = 12_000;
/// After prefix/suffix trim, skip LCS when either side exceeds this many lines.
const DIFF_LCS_LINE_CAP: usize = 1_500;
/// Merge change islands separated by this many equal lines or fewer.
const HUNK_MERGE_EQ: usize = 2;

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
    let mut pre = 0usize;
    while pre < a.len() && pre < b.len() && a[pre] == b[pre] {
        pre += 1;
    }
    let mut suf = 0usize;
    while suf < a.len().saturating_sub(pre) && suf < b.len().saturating_sub(pre) && a[a.len() - 1 - suf] == b[b.len() - 1 - suf]
    {
        suf += 1;
    }
    let a_mid = &a[pre..a.len() - suf];
    let b_mid = &b[pre..b.len() - suf];
    let mut out = Vec::with_capacity(a.len() + b.len());
    for line in &a[..pre] {
        out.push(eq_line(line));
    }
    if a_mid.len() > DIFF_LCS_LINE_CAP || b_mid.len() > DIFF_LCS_LINE_CAP {
        for line in a_mid {
            out.push(del_line(line));
        }
        for line in b_mid {
            out.push(add_line(line));
        }
    } else if !a_mid.is_empty() || !b_mid.is_empty() {
        let table = lcs_table(a_mid, b_mid);
        out.extend(backtrack_iter(a_mid, b_mid, &table));
    }
    for line in &a[a.len() - suf..] {
        out.push(eq_line(line));
    }
    out
}

pub fn diff_hunks(old: &str, new: &str) -> Vec<DiffHunk> {
    hunks_from_diff(&line_diff(old, new))
}

pub fn heavy_delete(diff: &[DiffLine]) -> bool {
    let del = diff.iter().filter(|l| l.kind == "del").count();
    let add = diff.iter().filter(|l| l.kind == "add").count();
    let changed = del + add;
    changed > 0 && del * 2 > changed
}

pub fn keep_hunk(base: &str, current: &str, hunk_index: usize) -> Result<String, String> {
    reconstruct_hunk(base, current, hunk_index, true)
}

pub fn revert_hunk(base: &str, current: &str, hunk_index: usize) -> Result<String, String> {
    reconstruct_hunk(base, current, hunk_index, false)
}

fn reconstruct_hunk(
    base: &str,
    current: &str,
    hunk_index: usize,
    keep: bool,
) -> Result<String, String> {
    let diff = line_diff(base, current);
    let spans = hunk_spans(&diff);
    if hunk_index >= spans.len() {
        return Err("その変更はもうありません。差分を読み直してください。".into());
    }
    let mut out: Vec<&str> = Vec::new();
    let mut h = 0usize;
    for (i, line) in diff.iter().enumerate() {
        while h < spans.len() && i >= spans[h].1 {
            h += 1;
        }
        let in_hunk = h < spans.len() && i >= spans[h].0 && i < spans[h].1;
        let use_new = if !in_hunk {
            true
        } else if keep {
            h == hunk_index
        } else {
            h != hunk_index
        };
        let include = match line.kind {
            "eq" => true,
            "add" => use_new,
            "del" => !use_new,
            _ => false,
        };
        if include {
            out.push(line.text.as_str());
        }
    }
    Ok(out.join("\n"))
}

fn hunks_from_diff(diff: &[DiffLine]) -> Vec<DiffHunk> {
    let spans = hunk_spans(diff);
    let mut prev = 0usize;
    let mut hunks = Vec::with_capacity(spans.len());
    for (start, end) in spans {
        hunks.push(DiffHunk {
            gap_before: start.saturating_sub(prev),
            lines: diff[start..end].to_vec(),
        });
        prev = end;
    }
    hunks
}

fn hunk_spans(diff: &[DiffLine]) -> Vec<(usize, usize)> {
    let mut islands: Vec<(usize, usize)> = Vec::new();
    let mut i = 0usize;
    while i < diff.len() {
        if diff[i].kind == "eq" {
            i += 1;
            continue;
        }
        let start = i;
        while i < diff.len() && diff[i].kind != "eq" {
            i += 1;
        }
        islands.push((start, i));
    }
    if islands.is_empty() {
        return Vec::new();
    }
    let mut merged: Vec<(usize, usize)> = Vec::new();
    let mut cur = islands[0];
    for &(start, end) in islands.iter().skip(1) {
        let gap = start.saturating_sub(cur.1);
        if gap <= HUNK_MERGE_EQ {
            cur.1 = end;
        } else {
            merged.push(cur);
            cur = (start, end);
        }
    }
    merged.push(cur);
    merged
}

fn split_lines(s: &str) -> Vec<&str> {
    if s.is_empty() {
        return Vec::new();
    }
    s.split('\n').collect()
}

fn eq_line(text: &str) -> DiffLine {
    DiffLine {
        kind: "eq",
        text: text.to_string(),
    }
}

fn add_line(text: &str) -> DiffLine {
    DiffLine {
        kind: "add",
        text: text.to_string(),
    }
}

fn del_line(text: &str) -> DiffLine {
    DiffLine {
        kind: "del",
        text: text.to_string(),
    }
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

fn backtrack_iter(a: &[&str], b: &[&str], t: &[Vec<usize>]) -> Vec<DiffLine> {
    let mut i = a.len();
    let mut j = b.len();
    let mut rev = Vec::new();
    while i > 0 || j > 0 {
        if i > 0 && j > 0 && a[i - 1] == b[j - 1] {
            rev.push(eq_line(a[i - 1]));
            i -= 1;
            j -= 1;
        } else if j > 0 && (i == 0 || t[i][j - 1] >= t[i.saturating_sub(1)][j]) {
            rev.push(add_line(b[j - 1]));
            j -= 1;
        } else if i > 0 {
            rev.push(del_line(a[i - 1]));
            i -= 1;
        } else {
            break;
        }
    }
    rev.reverse();
    rev
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
    fn line_diff_trims_equal_prefix_suffix() {
        let old = "p\nq\nmiddle\nr\ns\n";
        let new = "p\nq\nchanged\nr\ns\n";
        let d = line_diff(old, new);
        let kinds: Vec<_> = d.iter().map(|l| l.kind).collect();
        assert_eq!(kinds, ["eq", "eq", "del", "add", "eq", "eq", "eq"]);
    }

    #[test]
    fn line_diff_skips_lcs_when_mid_is_huge() {
        let old: String = (0..DIFF_LCS_LINE_CAP + 2)
            .map(|i| format!("o{i}\n"))
            .collect();
        let new: String = (0..DIFF_LCS_LINE_CAP + 2)
            .map(|i| format!("n{i}\n"))
            .collect();
        let d = line_diff(&old, &new);
        assert!(d.iter().any(|l| l.kind == "del"));
        assert!(d.iter().any(|l| l.kind == "add"));
        assert!(!d.iter().any(|l| l.kind == "eq" && l.text.starts_with('o')));
        let hunks = diff_hunks(&old, &new);
        assert_eq!(hunks.len(), 1);
    }

    #[test]
    fn hunks_merge_when_eq_gap_is_small() {
        let hunks = diff_hunks("a\nx\nb\nc\ny\n", "a\nX\nb\nc\nY\n");
        assert_eq!(hunks.len(), 1, "two changes with 2 eq lines merge");
        let hunks = diff_hunks("a\nx\nb\nc\nd\ny\n", "a\nX\nb\nc\nd\nY\n");
        assert_eq!(hunks.len(), 2, "3 eq lines stay split");
    }

    #[test]
    fn keep_and_revert_hunk_two_islands() {
        let base = "a\nx\nb\nc\nd\ny\ne\n";
        let current = "a\nX\nb\nc\nd\nY\ne\n";
        let hunks = diff_hunks(base, current);
        assert_eq!(hunks.len(), 2);
        let kept = keep_hunk(base, current, 0).unwrap();
        assert_eq!(kept, "a\nX\nb\nc\nd\ny\ne\n");
        let reverted = revert_hunk(base, current, 0).unwrap();
        assert_eq!(reverted, "a\nx\nb\nc\nd\nY\ne\n");
        assert!(keep_hunk(base, current, 9).is_err());
    }

    #[test]
    fn heavy_delete_when_majority_removed() {
        let d = line_diff("a\nb\nc\nd\n", "a\n");
        assert!(heavy_delete(&d));
        let d = line_diff("a\n", "a\nb\nc\n");
        assert!(!heavy_delete(&d));
    }

    #[test]
    fn caps_long_excerpt() {
        let s = cap_chars(&"あ".repeat(3000), 100);
        assert_eq!(s.chars().count(), 101);
        assert!(s.ends_with('…'));
    }
}
