//! Plain-text replacement across runs that were split in the Office XML.

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
    let mut ranges = Vec::with_capacity(pieces.len());
    let mut concat = String::new();
    for (index, piece) in pieces.iter().enumerate() {
        let start = concat.len();
        concat.push_str(piece);
        ranges.push((index, start, concat.len()));
    }
    let spans = find_spans(&concat, find);
    if spans.is_empty() {
        return 0;
    }
    let mut built = vec![String::new(); pieces.len()];
    let mut cursor = 0usize;
    for (start, end) in &spans {
        copy_range(&mut built, pieces, &ranges, cursor, *start);
        if let Some((index, _, _)) = ranges
            .iter()
            .find(|(_, piece_start, piece_end)| *start >= *piece_start && *start < *piece_end)
        {
            built[*index].push_str(replace);
        }
        cursor = *end;
    }
    copy_range(&mut built, pieces, &ranges, cursor, concat.len());
    for (index, piece) in pieces.iter_mut().enumerate() {
        *piece = std::mem::take(&mut built[index]);
    }
    spans.len()
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
}
