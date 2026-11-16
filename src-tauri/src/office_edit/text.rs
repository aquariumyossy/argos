//! Plain-text replacement across runs that were split in the Office XML.

/// How many near-miss strings a missed `find` may show.
pub const NEAR_MISS_LIMIT: usize = 3;

/// A replacement, and the source slices that matched only by whitespace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplaceHit {
    pub count: usize,
    pub sources: Vec<String>,
}

/// A source string the file actually contains, plus a separate whitespace note.
///
/// `exact` is pasteable into the next `find`. `note` is not part of that string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NearMiss {
    pub exact: String,
    pub note: String,
}

/// ASCII space, ideographic space, NBSP, or tab.
pub fn is_flex_ws(ch: char) -> bool {
    matches!(ch, ' ' | '\u{3000}' | '\u{00A0}' | '\t')
}

/// Non-overlapping byte spans of `needle` in `text`, left to right.
pub fn find_spans(text: &str, needle: &str) -> Vec<(usize, usize)> {
    if needle.is_empty() || needle.len() > text.len() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i + needle.len() <= text.len() {
        if let Some(pos) = text[i..].find(needle) {
            let start = i + pos;
            let end = start + needle.len();
            out.push((start, end));
            i = end;
        } else {
            break;
        }
    }
    out
}

/// Replace `find` with `replace` across `pieces` as if they were one string.
///
/// A match that starts in one piece and ends in a later piece is written into
/// the piece where it starts. Later pieces keep only the unmatched tail.
/// The inserted text is not searched again for `find`.
pub fn replace_pieces(pieces: &mut [String], find: &str, replace: &str) -> usize {
    if find.is_empty() || pieces.is_empty() || find == replace {
        return 0;
    }
    let (concat, ranges) = concat_pieces(pieces);
    let spans = find_spans(&concat, find);
    if spans.is_empty() {
        return 0;
    }
    apply_piece_spans(pieces, &ranges, &spans, replace);
    spans.len()
}

/// Exact matches win. A whitespace retry runs only when `find` has 0 exact hits
/// in these `pieces`.
///
/// On that retry, each whitespace run in `find` matches a non-empty run of
/// ASCII space, U+3000, NBSP, or tab in the source. A match may still cross
/// `pieces`. `sources` lists the distinct source slices, in order.
pub fn replace_pieces_flex(pieces: &mut [String], find: &str, replace: &str) -> ReplaceHit {
    if find.is_empty() || pieces.is_empty() || find == replace {
        return ReplaceHit {
            count: 0,
            sources: Vec::new(),
        };
    }
    let (concat, ranges) = concat_pieces(pieces);
    let (spans, sources) = mixed_spans(&concat, find);
    if spans.is_empty() {
        return ReplaceHit {
            count: 0,
            sources: Vec::new(),
        };
    }
    apply_piece_spans(pieces, &ranges, &spans, replace);
    ReplaceHit {
        count: spans.len(),
        sources,
    }
}

fn concat_pieces(pieces: &[String]) -> (String, Vec<(usize, usize, usize)>) {
    let mut ranges = Vec::with_capacity(pieces.len());
    let mut concat = String::new();
    for (index, piece) in pieces.iter().enumerate() {
        let start = concat.len();
        concat.push_str(piece);
        ranges.push((index, start, concat.len()));
    }
    (concat, ranges)
}

fn apply_piece_spans(
    pieces: &mut [String],
    ranges: &[(usize, usize, usize)],
    spans: &[(usize, usize)],
    replace: &str,
) {
    let total = ranges.last().map(|(_, _, end)| *end).unwrap_or(0);
    let mut built = vec![String::new(); pieces.len()];
    let mut cursor = 0usize;
    for (start, end) in spans {
        copy_range(&mut built, pieces, ranges, cursor, *start);
        if let Some((index, _, _)) = ranges
            .iter()
            .find(|(_, piece_start, piece_end)| *start >= *piece_start && *start < *piece_end)
        {
            built[*index].push_str(replace);
        }
        cursor = *end;
    }
    copy_range(&mut built, pieces, ranges, cursor, total);
    for (index, piece) in pieces.iter_mut().enumerate() {
        *piece = std::mem::take(&mut built[index]);
    }
}

enum Part<'a> {
    Lit(&'a str),
    Ws,
}

fn find_parts(find: &str) -> Vec<Part<'_>> {
    let mut parts = Vec::new();
    let mut index = 0usize;
    while index < find.len() {
        let ch = find[index..].chars().next().unwrap();
        if is_flex_ws(ch) {
            parts.push(Part::Ws);
            index += ch.len_utf8();
            while index < find.len() {
                let next = find[index..].chars().next().unwrap();
                if !is_flex_ws(next) {
                    break;
                }
                index += next.len_utf8();
            }
        } else {
            let start = index;
            index += ch.len_utf8();
            while index < find.len() {
                let next = find[index..].chars().next().unwrap();
                if is_flex_ws(next) {
                    break;
                }
                index += next.len_utf8();
            }
            parts.push(Part::Lit(&find[start..index]));
        }
    }
    parts
}

fn has_flex_ws(find: &str) -> bool {
    find.chars().any(is_flex_ws)
}

/// Exact hits win for this whole string. Flex runs only when the exact `find` has 0 hits.
fn mixed_spans(text: &str, find: &str) -> (Vec<(usize, usize)>, Vec<String>) {
    let exact = find_spans(text, find);
    if !exact.is_empty() || !has_flex_ws(find) {
        return (exact, Vec::new());
    }
    let parts = find_parts(find);
    let mut spans = Vec::new();
    let mut sources = Vec::new();
    let mut index = 0usize;
    while index < text.len() {
        if !text.is_char_boundary(index) {
            index += 1;
            continue;
        }
        if let Some(end) = match_parts_at(text, index, &parts) {
            let slice = text[index..end].to_string();
            if !sources.contains(&slice) {
                sources.push(slice);
            }
            spans.push((index, end));
            index = end;
            continue;
        }
        index += text[index..]
            .chars()
            .next()
            .map(|ch| ch.len_utf8())
            .unwrap_or(1);
    }
    (spans, sources)
}

fn match_parts_at(text: &str, start: usize, parts: &[Part<'_>]) -> Option<usize> {
    let mut pos = start;
    for part in parts {
        match part {
            Part::Lit(lit) => {
                if !text[pos..].starts_with(lit) {
                    return None;
                }
                pos += lit.len();
            }
            Part::Ws => {
                let mut consumed = 0usize;
                for ch in text[pos..].chars() {
                    if !is_flex_ws(ch) {
                        break;
                    }
                    consumed += ch.len_utf8();
                }
                if consumed == 0 {
                    return None;
                }
                pos += consumed;
            }
        }
    }
    if pos == start {
        None
    } else {
        Some(pos)
    }
}

/// Kinds of whitespace in `text`, for a note that sits outside the pasteable string.
pub fn whitespace_note(text: &str) -> String {
    let mut kinds = Vec::new();
    if text.contains('\u{3000}') {
        kinds.push("全角空白");
    }
    if text.contains('\u{00A0}') {
        kinds.push("NBSP");
    }
    if text.contains('\t') {
        kinds.push("タブ");
    }
    if text.contains('\n') || text.contains('\r') {
        kinds.push("改行");
    }
    if text.contains("  ") {
        kinds.push("連続する半角空白");
    }
    let mut extras = Vec::new();
    for ch in text.chars() {
        if ch.is_whitespace() && !matches!(ch, ' ' | '\u{3000}' | '\u{00A0}' | '\t' | '\n' | '\r') {
            let label = format!("U+{code:04X}", code = u32::from(ch));
            if !extras.contains(&label) {
                extras.push(label);
            }
        }
    }
    let mut note = kinds.join("、");
    if !extras.is_empty() {
        if !note.is_empty() {
            note.push('、');
        }
        note.push_str(&extras.join("、"));
    }
    note
}

/// Source slices whose non-whitespace characters equal `find`, but the slice
/// itself is not `find`. At most `limit` distinct strings, in appearance order.
pub fn near_misses(texts: &[String], find: &str, limit: usize) -> Vec<NearMiss> {
    if limit == 0 || find.is_empty() {
        return Vec::new();
    }
    let target: Vec<char> = find.chars().filter(|ch| !ch.is_whitespace()).collect();
    if target.is_empty() {
        return Vec::new();
    }
    let mut found = Vec::new();
    for text in texts {
        collect_compact(text, find, &target, &mut found, limit);
        if found.len() >= limit {
            break;
        }
    }
    found.truncate(limit);
    found
        .into_iter()
        .map(|exact| {
            let mut note = whitespace_note(&exact);
            if note.is_empty()
                && find.chars().any(|ch| ch.is_whitespace())
                && !exact.chars().any(|ch| ch.is_whitespace())
            {
                note = "空白なし".to_string();
            }
            NearMiss { exact, note }
        })
        .collect()
}

fn collect_compact(text: &str, find: &str, target: &[char], out: &mut Vec<String>, limit: usize) {
    if out.len() >= limit {
        return;
    }
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    for start in 0..chars.len() {
        if chars[start].1.is_whitespace() {
            continue;
        }
        let Some(slice) = align_compact(text, &chars, start, target) else {
            continue;
        };
        if slice == find || out.iter().any(|item| item == &slice) {
            continue;
        }
        out.push(slice);
        if out.len() >= limit {
            return;
        }
    }
}

fn align_compact(
    text: &str,
    chars: &[(usize, char)],
    start: usize,
    target: &[char],
) -> Option<String> {
    let mut matched = 0usize;
    let mut cursor = start;
    let mut end_byte = chars[start].0 + chars[start].1.len_utf8();
    while matched < target.len() {
        if cursor >= chars.len() {
            return None;
        }
        let ch = chars[cursor].1;
        if ch.is_whitespace() {
            if matched == 0 {
                return None;
            }
            cursor += 1;
            continue;
        }
        if ch != target[matched] {
            return None;
        }
        end_byte = chars[cursor].0 + ch.len_utf8();
        matched += 1;
        cursor += 1;
    }
    Some(text[chars[start].0..end_byte].to_string())
}

fn copy_range(
    built: &mut [String],
    pieces: &[String],
    ranges: &[(usize, usize, usize)],
    from: usize,
    to: usize,
) {
    if from >= to {
        return;
    }
    for &(index, start, end) in ranges {
        let lo = from.max(start);
        let hi = to.min(end);
        if lo < hi {
            built[index].push_str(&pieces[index][lo - start..hi - start]);
        }
    }
}

/// Occurrences of `find` that are not already inside `explain`.
///
/// Replacing 「山田」 with 「山田太郎」 leaves 「山田」 inside the new text.
/// Those occurrences are the edit, not leftovers.
pub fn unexplained_starts(text: &str, find: &str, explain: &str) -> Vec<usize> {
    if find.is_empty() {
        return Vec::new();
    }
    let covers = if !explain.is_empty() && explain.contains(find) {
        find_spans(text, explain)
    } else {
        Vec::new()
    };
    find_spans(text, find)
        .into_iter()
        .filter(|(start, end)| !covers.iter().any(|(from, to)| start >= from && end <= to))
        .map(|(start, _)| start)
        .collect()
}

/// Collapse whitespace so a paragraph copied from the extracted chat text still matches.
pub fn collapse_ws(text: &str) -> String {
    let mut out = String::new();
    let mut pending = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            if !out.is_empty() {
                pending = true;
            }
        } else {
            if pending {
                out.push(' ');
                pending = false;
            }
            out.push(ch);
        }
    }
    out
}

pub fn contains_match(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() || haystack.is_empty() {
        return false;
    }
    if haystack.contains(needle) {
        return true;
    }
    let folded_needle = collapse_ws(needle);
    !folded_needle.is_empty() && collapse_ws(haystack).contains(&folded_needle)
}

pub fn snippet(text: &str, byte_start: usize, needle: &str) -> String {
    let start = byte_start.min(text.len());
    let start = floor_char(text, start);
    let needle_chars = needle.chars().count();
    let chars: Vec<char> = text.chars().collect();
    let at = text[..start].chars().count();
    let from = at.saturating_sub(12);
    let to = (at + needle_chars + 12).min(chars.len());
    let mut out: String = chars[from..to].iter().collect();
    out = out.replace(['\n', '\r', '\t'], " ");
    if from > 0 {
        out.insert(0, '…');
    }
    if to < chars.len() {
        out.push('…');
    }
    out
}

fn floor_char(text: &str, mut index: usize) -> usize {
    if index > text.len() {
        return text.len();
    }
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_pieces_take_the_replacement_in_the_first_piece() {
        let mut pieces = vec!["山田".to_string(), "太郎".to_string()];
        let n = replace_pieces(&mut pieces, "山田太郎", "佐藤花子");
        assert_eq!(n, 1);
        assert_eq!(pieces, vec!["佐藤花子".to_string(), String::new()]);
    }

    #[test]
    fn replacement_text_is_not_searched_again() {
        let mut pieces = vec!["山田".to_string()];
        let n = replace_pieces(&mut pieces, "山田", "山田太郎");
        assert_eq!(n, 1);
        assert_eq!(pieces, vec!["山田太郎".to_string()]);
        assert!(unexplained_starts("山田太郎", "山田", "山田太郎").is_empty());
    }

    #[test]
    fn a_real_remainder_is_not_explained_by_the_new_text() {
        let starts = unexplained_starts("山田太郎と山田", "山田", "山田太郎");
        assert_eq!(starts.len(), 1);
    }

    #[test]
    fn flex_match_crosses_runs_for_fullwidth_nbsp_and_tab() {
        let mut pieces = vec!["山田".to_string(), "　太郎".to_string()];
        let hit = replace_pieces_flex(&mut pieces, "山田 太郎", "佐藤花子");
        assert_eq!(hit.count, 1);
        assert_eq!(hit.sources, vec!["山田　太郎".to_string()]);
        assert_eq!(pieces, vec!["佐藤花子".to_string(), String::new()]);

        let mut nbsp = vec!["山田".to_string(), "\u{00A0}太郎".to_string()];
        let hit = replace_pieces_flex(&mut nbsp, "山田 太郎", "佐藤花子");
        assert_eq!(hit.count, 1);
        assert_eq!(hit.sources, vec!["山田\u{00A0}太郎".to_string()]);
        assert!(whitespace_note(&hit.sources[0]).contains("NBSP"));

        let mut tabbed = vec!["山田\t".to_string(), "太郎".to_string()];
        let hit = replace_pieces_flex(&mut tabbed, "山田 太郎", "佐藤花子");
        assert_eq!(hit.count, 1);
        assert_eq!(hit.sources, vec!["山田\t太郎".to_string()]);
        assert_eq!(tabbed, vec!["佐藤花子".to_string(), String::new()]);
        assert!(whitespace_note("山田\t太郎").contains("タブ"));
    }

    #[test]
    fn exact_match_takes_priority_over_a_whitespace_variant() {
        let mut pieces = vec!["A B/A　B".to_string()];
        let hit = replace_pieces_flex(&mut pieces, "A B", "Z");
        assert_eq!(hit.count, 1);
        assert!(hit.sources.is_empty());
        assert_eq!(pieces, vec!["Z/A　B".to_string()]);
    }

    #[test]
    fn flex_match_is_not_searched_again() {
        let mut pieces = vec!["A　B".to_string()];
        let hit = replace_pieces_flex(&mut pieces, "A B", "A Bです");
        assert_eq!(hit.count, 1);
        assert_eq!(pieces, vec!["A Bです".to_string()]);
    }

    #[test]
    fn a_missing_space_is_not_a_flex_match() {
        let mut pieces = vec!["山田太郎".to_string()];
        let hit = replace_pieces_flex(&mut pieces, "山田 太郎", "佐藤花子");
        assert_eq!(hit.count, 0);
        assert_eq!(pieces, vec!["山田太郎".to_string()]);
    }

    #[test]
    fn near_miss_keeps_the_source_bytes_and_annotates_beside_them() {
        let texts = vec![
            "前文山田\u{2003}太郎。".to_string(),
            "鈴木一郎です".to_string(),
        ];
        let misses = near_misses(&texts, "山田 太郎", 3);
        assert_eq!(misses.len(), 1);
        assert_eq!(misses[0].exact, "山田\u{2003}太郎");
        assert_eq!(misses[0].note, "U+2003");
        assert!(!misses[0].exact.contains("U+"));
        assert!(!misses[0].exact.contains('※'));

        let missing = near_misses(&["鈴木一郎です".to_string()], "鈴木 一郎", 3);
        assert_eq!(missing[0].exact, "鈴木一郎");
        assert_eq!(missing[0].note, "空白なし");

        let fullwidth = near_misses(&["名前は山田　太郎。".to_string()], "山田太郎", 3);
        assert_eq!(fullwidth[0].exact, "山田　太郎");
        assert_eq!(fullwidth[0].note, "全角空白");
        assert_eq!(fullwidth[0].exact.chars().nth(2), Some('\u{3000}'));
    }

    #[test]
    fn near_miss_stops_at_a_few_distinct_strings() {
        let texts = vec![
            "AA".to_string(),
            "A\tA".to_string(),
            "A　A".to_string(),
            "A\u{00A0}A".to_string(),
        ];
        let misses = near_misses(&texts, "A A", 3);
        assert_eq!(misses.len(), 3);
        assert_eq!(misses[0].exact, "AA");
        assert_eq!(misses[2].exact, "A　A");
    }
}
