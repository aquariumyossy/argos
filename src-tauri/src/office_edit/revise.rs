//! Record Word replacements as tracked changes, and anchor optional comments.

use super::text::flex_spans;
use super::xmlscan::{escape_attr, escape_text, next_tag, replace_in_group, ParaGroup, TextNode};
use super::{EditRequest, Replacement};

pub struct MadeComment {
    pub id: u32,
    pub text: String,
}

pub struct ReviseHit {
    pub index: usize,
    pub count: usize,
    pub exact: usize,
    pub sources: Vec<String>,
    /// False for the VML fallback copy of the same drawing.
    pub record: bool,
    /// True when this hit was written as w:del + w:ins.
    pub tracked: bool,
}

pub struct ReviseOut {
    pub xml: String,
    pub comments: Vec<MadeComment>,
    pub hits: Vec<ReviseHit>,
    /// A drawingML `a:t` was changed in place because it cannot hold `w:del`.
    pub shape_direct: bool,
    /// A match crossed a non-run element, so that occurrence was written in place.
    pub untracked_span: bool,
    /// A comment was dropped because this story cannot hold one.
    pub skipped_comments: bool,
}

struct Mark {
    start: usize,
    end: usize,
    source: String,
    replacement: String,
    comment: String,
    index: usize,
    exact: bool,
}

struct Piece {
    concat_start: usize,
    concat_end: usize,
    run_start: usize,
    run_end: usize,
    node_start: usize,
    node_end: usize,
    rpr: String,
    text: String,
}

pub fn revise_story(
    xml: &str,
    nodes: &[TextNode],
    groups: &[ParaGroup],
    req: &EditRequest,
    next_id: &mut u32,
    allow_comments: bool,
) -> ReviseOut {
    let mut nodes_owned: Vec<TextNode> = nodes.to_vec();
    let runs = element_ranges(xml, "r");
    let mut splices: Vec<(usize, usize, String)> = Vec::new();
    let mut comments = Vec::new();
    let mut hits = Vec::new();
    let mut shape_direct = false;
    let mut untracked_span = false;
    let mut skipped_comments = false;

    for group in groups {
        let visible: Vec<usize> = group
            .visible
            .iter()
            .copied()
            .filter(|&index| nodes_owned[index].locked.is_none())
            .collect();
        let deleted: Vec<usize> = group
            .deleted
            .iter()
            .copied()
            .filter(|&index| nodes_owned[index].locked.is_none())
            .collect();
        let (word, shapes): (Vec<usize>, Vec<usize>) = visible
            .iter()
            .copied()
            .partition(|&index| is_word_t(xml, &nodes_owned[index]));

        for (index, item) in req.replacements.iter().enumerate() {
            if !shapes.is_empty() {
                let hit = replace_in_group(&mut nodes_owned, &shapes, &item.find, &item.replace);
                if hit.count > 0 {
                    shape_direct = true;
                    hits.push(ReviseHit {
                        index,
                        count: hit.count,
                        exact: hit.exact,
                        sources: hit.sources,
                        record: !group.fallback,
                        tracked: false,
                    });
                }
            }
            // Tracked edits leave every existing deletion alone, including ones from
            // an earlier call in this conversation. Direct edits still update them.
            if !req.track_changes {
                let hit = replace_in_group(&mut nodes_owned, &deleted, &item.find, &item.replace);
                if hit.count > 0 {
                    hits.push(ReviseHit {
                        index,
                        count: hit.count,
                        exact: hit.exact,
                        sources: hit.sources,
                        record: !group.fallback,
                        tracked: false,
                    });
                }
            }
        }

        if let Some((start, end, new_xml, group_hits, group_comments, span, skipped)) =
            revise_word_runs(
                xml,
                &nodes_owned,
                &word,
                &runs,
                req,
                next_id,
                !group.fallback,
                allow_comments,
            )
        {
            splices.push((start, end, new_xml));
            hits.extend(group_hits);
            comments.extend(group_comments);
            untracked_span |= span;
            skipped_comments |= skipped;
        } else {
            // A w:t with no surrounding w:r cannot become w:del / w:ins.
            for (index, item) in req.replacements.iter().enumerate() {
                let hit = replace_in_group(&mut nodes_owned, &word, &item.find, &item.replace);
                if hit.count > 0 {
                    hits.push(ReviseHit {
                        index,
                        count: hit.count,
                        exact: hit.exact,
                        sources: hit.sources,
                        record: !group.fallback,
                        tracked: false,
                    });
                }
            }
        }
    }

    for node in &nodes_owned {
        if !node.dirty {
            continue;
        }
        let open = &xml[node.open_start..node.inner_start];
        let rendered = render_plain(open, &node.text);
        splices.push((node.open_start, node.close_end, rendered));
    }

    splices.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
    let mut kept = Vec::new();
    for splice in splices {
        let inside = kept
            .iter()
            .any(|(start, end, _): &(usize, usize, String)| *start <= splice.0 && splice.1 <= *end);
        if !inside {
            kept.push(splice);
        }
    }
    kept.sort_by(|a, b| b.0.cmp(&a.0));
    let mut out = xml.to_string();
    for (start, end, new_xml) in kept {
        if start < end && end <= out.len() {
            out.replace_range(start..end, &new_xml);
        }
    }
    ReviseOut {
        xml: out,
        comments,
        hits,
        shape_direct,
        untracked_span,
        skipped_comments,
    }
}

fn revise_word_runs(
    xml: &str,
    nodes: &[TextNode],
    indexes: &[usize],
    runs: &[(usize, usize)],
    req: &EditRequest,
    next_id: &mut u32,
    record: bool,
    allow_comments: bool,
) -> Option<(
    usize,
    usize,
    String,
    Vec<ReviseHit>,
    Vec<MadeComment>,
    bool,
    bool,
)> {
    if indexes.is_empty() || req.replacements.is_empty() {
        return None;
    }
    let mut pieces = Vec::new();
    let mut concat = String::new();
    for &index in indexes {
        let Some(run) = run_covering(runs, nodes[index].open_start) else {
            return None;
        };
        let start = concat.len();
        concat.push_str(&nodes[index].text);
        pieces.push(Piece {
            concat_start: start,
            concat_end: concat.len(),
            run_start: run.0,
            run_end: run.1,
            node_start: nodes[index].open_start,
            node_end: nodes[index].close_end,
            rpr: run_rpr(&xml[run.0..run.1]),
            text: nodes[index].text.clone(),
        });
    }
    let marks = marks_in(&concat, &req.replacements);
    if marks.is_empty() {
        return None;
    }
    let region_start = pieces[0].run_start;
    let region_end = pieces[pieces.len() - 1].run_end;
    let (new_xml, hits, comments, untracked, skipped) = emit_region(
        xml,
        &pieces,
        &marks,
        region_start,
        req,
        next_id,
        record,
        allow_comments,
    );
    Some((
        region_start,
        region_end,
        new_xml,
        hits,
        comments,
        untracked,
        skipped,
    ))
}

fn marks_in(text: &str, replacements: &[Replacement]) -> Vec<Mark> {
    let mut marks = Vec::new();
    let mut occupied: Vec<(usize, usize)> = Vec::new();
    for (index, item) in replacements.iter().enumerate() {
        let comment = item.comment.trim();
        for span in flex_spans(text, &item.find) {
            if occupied
                .iter()
                .any(|(start, end)| span.start < *end && *start < span.end)
            {
                continue;
            }
            occupied.push((span.start, span.end));
            let source = text[span.start..span.end].to_string();
            marks.push(Mark {
                start: span.start,
                end: span.end,
                source,
                replacement: item.replace.clone(),
                comment: comment.to_string(),
                index,
                exact: span.exact,
            });
        }
    }
    marks.sort_by_key(|mark| mark.start);
    marks
}

fn emit_region(
    xml: &str,
    pieces: &[Piece],
    marks: &[Mark],
    region_start: usize,
    req: &EditRequest,
    next_id: &mut u32,
    record: bool,
    allow_comments: bool,
) -> (String, Vec<ReviseHit>, Vec<MadeComment>, bool, bool) {
    let mut out = String::new();
    let mut hits = Vec::new();
    let mut comments = Vec::new();
    let mut untracked = false;
    let mut skipped = false;
    let author = escape_attr(&req.revision_author);
    let date = escape_attr(&req.revision_date);
    let mut piece_i = 0usize;
    let mut local = 0usize;
    let mut omit_until = 0usize;
    let mut run_open = false;
    let mut lead_done: Option<usize> = None;

    while piece_i < pieces.len() {
        let piece = &pieces[piece_i];
        let first_of_run = piece_i == 0 || pieces[piece_i - 1].run_start != piece.run_start;
        let last_of_run =
            piece_i + 1 == pieces.len() || pieces[piece_i + 1].run_start != piece.run_start;

        if first_of_run && local == 0 && lead_done != Some(piece.run_start) {
            close_run(&mut out, &mut run_open);
            let gap_from = if piece_i == 0 {
                region_start
            } else {
                pieces[piece_i - 1].run_end
            };
            if gap_from < piece.run_start {
                out.push_str(&xml[gap_from..piece.run_start]);
            }
            let lead = extras_before(xml, piece);
            if !lead.is_empty() {
                out.push_str("<w:r>");
                out.push_str(&piece.rpr);
                out.push_str(lead);
                run_open = true;
            }
            lead_done = Some(piece.run_start);
        }

        if local >= piece.text.len() {
            if last_of_run {
                let trail = extras_after(xml, piece);
                if !trail.is_empty() {
                    if !run_open {
                        out.push_str("<w:r>");
                        out.push_str(&piece.rpr);
                        run_open = true;
                    }
                    out.push_str(trail);
                }
                close_run(&mut out, &mut run_open);
            } else {
                let between = &xml[piece.node_end..pieces[piece_i + 1].node_start];
                if !between.is_empty() {
                    if !run_open {
                        out.push_str("<w:r>");
                        out.push_str(&piece.rpr);
                        run_open = true;
                    }
                    out.push_str(between);
                }
            }
            piece_i += 1;
            local = 0;
            continue;
        }

        let abs = piece.concat_start + local;
        if abs < omit_until {
            local = piece.concat_end.min(omit_until) - piece.concat_start;
            continue;
        }

        if let Some(mark_at) = marks
            .iter()
            .position(|mark| mark.start >= abs && mark.start < piece.concat_end)
        {
            let mark = &marks[mark_at];
            if mark.start > abs {
                if !run_open {
                    out.push_str("<w:r>");
                    out.push_str(&piece.rpr);
                    run_open = true;
                }
                out.push_str(&text_node(
                    "t",
                    &piece.text[local..mark.start - piece.concat_start],
                ));
                local = mark.start - piece.concat_start;
                continue;
            }
            let cross = req.track_changes && mark_crosses(xml, pieces, mark);
            let (last_i, last_local) = mark_end_pos(pieces, mark.end);
            let tracked = req.track_changes && !cross;
            if cross {
                untracked = true;
            }
            close_run(&mut out, &mut run_open);
            let body = if tracked {
                tracked_change(xml, pieces, mark, &author, &date, next_id)
            } else {
                synth_run(&piece.rpr, "t", &mark.replacement)
            };
            out.push_str(&with_comment(
                &body,
                &mark.comment,
                record,
                allow_comments,
                next_id,
                &mut comments,
                &mut skipped,
            ));
            if record {
                hits.push(ReviseHit {
                    index: mark.index,
                    count: 1,
                    exact: usize::from(mark.exact),
                    sources: if mark.exact {
                        Vec::new()
                    } else {
                        vec![mark.source.clone()]
                    },
                    record: true,
                    tracked,
                });
            }
            if tracked {
                piece_i = last_i;
                local = last_local;
                lead_done = Some(pieces[last_i].run_start);
                continue;
            }
            omit_until = mark.end;
            local = mark.end.min(piece.concat_end) - piece.concat_start;
            continue;
        }

        if !run_open {
            out.push_str("<w:r>");
            out.push_str(&piece.rpr);
            run_open = true;
        }
        out.push_str(&text_node("t", &piece.text[local..]));
        local = piece.text.len();
    }
    close_run(&mut out, &mut run_open);
    (out, hits, comments, untracked, skipped)
}

fn close_run(out: &mut String, run_open: &mut bool) {
    if *run_open {
        out.push_str("</w:r>");
        *run_open = false;
    }
}

fn mark_crosses(xml: &str, pieces: &[Piece], mark: &Mark) -> bool {
    let mut previous: Option<usize> = None;
    for (index, piece) in pieces.iter().enumerate() {
        if piece.concat_end <= mark.start || piece.concat_start >= mark.end {
            continue;
        }
        if let Some(previous) = previous {
            if pieces[previous].run_start != piece.run_start {
                let gap = &xml[pieces[previous].run_end..piece.run_start];
                if gap.contains('<') {
                    return true;
                }
            }
        }
        previous = Some(index);
    }
    false
}

fn mark_end_pos(pieces: &[Piece], end: usize) -> (usize, usize) {
    for (index, piece) in pieces.iter().enumerate() {
        if end > piece.concat_start && end <= piece.concat_end {
            return (index, end - piece.concat_start);
        }
    }
    (
        pieces.len().saturating_sub(1),
        pieces.last().map(|piece| piece.text.len()).unwrap_or(0),
    )
}

fn tracked_change(
    xml: &str,
    pieces: &[Piece],
    mark: &Mark,
    author: &str,
    date: &str,
    next_id: &mut u32,
) -> String {
    let overlapping: Vec<usize> = pieces
        .iter()
        .enumerate()
        .filter(|(_, piece)| piece.concat_end > mark.start && piece.concat_start < mark.end)
        .map(|(index, _)| index)
        .collect();
    let mut deleted = String::new();
    for (nth, &index) in overlapping.iter().enumerate() {
        let piece = &pieces[index];
        let mut lead = String::new();
        if nth > 0 {
            let previous = overlapping[nth - 1];
            if pieces[previous].run_start == piece.run_start {
                let between = &xml[pieces[previous].node_end..piece.node_start];
                if !between.is_empty() {
                    deleted.push_str("<w:r>");
                    deleted.push_str(&piece.rpr);
                    deleted.push_str(between);
                    deleted.push_str("</w:r>");
                }
            } else {
                deleted.push_str(&xml[pieces[previous].run_end..piece.run_start]);
                lead = extras_before(xml, piece).to_string();
            }
        }
        let local_start = mark.start.max(piece.concat_start) - piece.concat_start;
        let local_end = mark.end.min(piece.concat_end) - piece.concat_start;
        let slice = &piece.text[local_start..local_end];
        if slice.is_empty() && lead.is_empty() {
            continue;
        }
        deleted.push_str("<w:r>");
        deleted.push_str(&piece.rpr);
        deleted.push_str(&lead);
        if !slice.is_empty() {
            deleted.push_str(&text_node("delText", slice));
        }
        deleted.push_str("</w:r>");
    }
    let ins_rpr = overlapping
        .first()
        .map(|&index| pieces[index].rpr.as_str())
        .unwrap_or("");
    let del_id = take_id(next_id);
    let ins_id = take_id(next_id);
    format!(
        "<w:del w:id=\"{del_id}\" w:author=\"{author}\" w:date=\"{date}\">{deleted}</w:del><w:ins w:id=\"{ins_id}\" w:author=\"{author}\" w:date=\"{date}\">{}</w:ins>",
        synth_run(ins_rpr, "t", &mark.replacement)
    )
}

fn with_comment(
    body: &str,
    comment: &str,
    record: bool,
    allow_comments: bool,
    next_id: &mut u32,
    comments: &mut Vec<MadeComment>,
    skipped: &mut bool,
) -> String {
    if comment.is_empty() || !record {
        return body.to_string();
    }
    if !allow_comments {
        *skipped = true;
        return body.to_string();
    }
    let comment_id = take_id(next_id);
    comments.push(MadeComment {
        id: comment_id,
        text: comment.to_string(),
    });
    format!(
        "<w:commentRangeStart w:id=\"{comment_id}\"/>{body}<w:commentRangeEnd w:id=\"{comment_id}\"/><w:r><w:commentReference w:id=\"{comment_id}\"/></w:r>"
    )
}

fn take_id(next_id: &mut u32) -> u32 {
    let id = *next_id;
    *next_id = next_id.saturating_add(1);
    id
}

fn synth_run(rpr: &str, local: &str, text: &str) -> String {
    format!("<w:r>{rpr}{}</w:r>", text_node(local, text))
}

fn extras_before<'a>(xml: &'a str, piece: &Piece) -> &'a str {
    let run = &xml[piece.run_start..piece.node_start];
    let Some(end) = run.find('>') else {
        return "";
    };
    let mut rest = &run[end + 1..];
    let trimmed = rest.trim_start();
    if trimmed.starts_with("<w:rPr") {
        if let Some(rel) = rest.find("</w:rPr>") {
            rest = &rest[rel + "</w:rPr>".len()..];
        } else if let Some(rel) = rest.find("/>") {
            rest = &rest[rel + 2..];
        }
    }
    rest
}

fn extras_after<'a>(xml: &'a str, piece: &Piece) -> &'a str {
    let after = &xml[piece.node_end..piece.run_end];
    after.trim_end().strip_suffix("</w:r>").unwrap_or(after)
}

fn text_node(local: &str, text: &str) -> String {
    let preserve = text.starts_with(char::is_whitespace)
        || text.ends_with(char::is_whitespace)
        || text.contains("  ")
        || text.contains('\t');
    let space = if preserve {
        " xml:space=\"preserve\""
    } else {
        ""
    };
    format!("<w:{local}{space}>{}</w:{local}>", escape_text(text))
}

fn render_plain(open: &str, text: &str) -> String {
    let close = close_of(open);
    format!("{open}{}{close}", escape_text(text))
}

fn close_of(open: &str) -> String {
    let inner = open.trim_start_matches('<').trim_end_matches('>').trim();
    let name = inner
        .split_whitespace()
        .next()
        .unwrap_or("t")
        .trim_end_matches('/');
    format!("</{name}>")
}

fn is_word_t(xml: &str, node: &TextNode) -> bool {
    xml[node.open_start..node.inner_start]
        .trim_start()
        .starts_with("<w:t")
}

fn run_covering(runs: &[(usize, usize)], pos: usize) -> Option<(usize, usize)> {
    runs.iter()
        .filter(|(start, end)| *start <= pos && pos < *end)
        .min_by_key(|(start, end)| end.saturating_sub(*start))
        .copied()
}

fn element_ranges(xml: &str, local: &str) -> Vec<(usize, usize)> {
    let mut stack = Vec::new();
    let mut out = Vec::new();
    let mut pos = 0usize;
    while let Some(tag) = next_tag(xml, pos) {
        if tag.local == local && !tag.is_end && !tag.is_self {
            stack.push(tag.start);
        } else if tag.is_end && tag.local == local {
            if let Some(start) = stack.pop() {
                out.push((start, tag.end));
            }
        }
        pos = tag.end;
    }
    out
}

fn run_rpr(run_xml: &str) -> String {
    let Some(start) = run_xml.find("<w:rPr") else {
        return String::new();
    };
    if let Some(rel) = run_xml[start..].find("</w:rPr>") {
        return run_xml[start..start + rel + "</w:rPr>".len()].to_string();
    }
    if let Some(rel) = run_xml[start..].find("/>") {
        let end = start + rel + 2;
        if !run_xml[start..end].contains('<') || run_xml[start..end].starts_with("<w:rPr") {
            return run_xml[start..end].to_string();
        }
    }
    String::new()
}

/// Deletions written by `author`, whatever date they carry.
///
/// A later tracked edit uses a new timestamp, so the date cannot identify the
/// deletions this author already recorded.
pub fn own_del_ranges(xml: &str, author: &str) -> Vec<(usize, usize)> {
    let author = escape_attr(author);
    element_ranges(xml, "del")
        .into_iter()
        .filter(|(start, end)| {
            let header_end = xml[*start..*end]
                .find('>')
                .map(|index| *start + index)
                .unwrap_or(*start);
            let header = &xml[*start..header_end];
            header.contains(&format!("w:author=\"{author}\""))
        })
        .collect()
}

pub fn max_revision_id(entries: &[(String, Vec<u8>)]) -> u32 {
    let mut max_id = 0u32;
    for (_, bytes) in entries {
        let Ok(xml) = std::str::from_utf8(bytes) else {
            continue;
        };
        max_id = max_id.max(max_attr_id(xml, "w:id=\""));
        max_id = max_id.max(max_attr_id(xml, "w:id='"));
    }
    max_id
}

fn max_attr_id(xml: &str, key: &str) -> u32 {
    let mut max_id = 0u32;
    let mut rest = xml;
    while let Some(found) = rest.find(key) {
        rest = &rest[found + key.len()..];
        let quote = if key.ends_with('"') { '"' } else { '\'' };
        let Some(end) = rest.find(quote) else {
            break;
        };
        if let Ok(value) = rest[..end].parse::<u32>() {
            max_id = max_id.max(value);
        }
        rest = &rest[end..];
    }
    max_id
}

pub fn install_comments(
    entries: &mut Vec<(String, Vec<u8>)>,
    comments: &[MadeComment],
    author: &str,
    date: &str,
) {
    if comments.is_empty() {
        return;
    }
    upsert_comments_part(entries, comments, author, date);
    ensure_content_type(entries);
    // Comments are a document-level part. Header, footer, and note stories
    // do not get their own comments relationship.
    ensure_comments_rel(entries, "word/document.xml");
}

fn upsert_comments_part(
    entries: &mut Vec<(String, Vec<u8>)>,
    comments: &[MadeComment],
    author: &str,
    date: &str,
) {
    let block: String = comments
        .iter()
        .map(|comment| comment_xml(comment, author, date))
        .collect();
    if let Some((_, bytes)) = entries
        .iter_mut()
        .find(|(name, _)| file_name(name) == "comments.xml")
    {
        let Ok(xml) = std::str::from_utf8(bytes) else {
            return;
        };
        let updated = if let Some(at) = xml.rfind("</w:comments>") {
            let mut out = String::new();
            out.push_str(&xml[..at]);
            out.push_str(&block);
            out.push_str(&xml[at..]);
            out
        } else {
            format!("{xml}{block}")
        };
        *bytes = updated.into_bytes();
        return;
    }
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<w:comments xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">{block}</w:comments>"
    );
    entries.push(("word/comments.xml".into(), xml.into_bytes()));
}

fn comment_xml(comment: &MadeComment, author: &str, date: &str) -> String {
    let initials = author.chars().next().unwrap_or('A');
    format!(
        "<w:comment w:id=\"{}\" w:author=\"{}\" w:date=\"{}\" w:initials=\"{}\"><w:p><w:r>{}</w:r></w:p></w:comment>",
        comment.id,
        escape_attr(author),
        escape_attr(date),
        escape_attr(&initials.to_string()),
        text_node("t", &comment.text)
    )
}

fn ensure_content_type(entries: &mut Vec<(String, Vec<u8>)>) {
    const PART: &str = "PartName=\"/word/comments.xml\"";
    const OVERRIDE: &str = "<Override PartName=\"/word/comments.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml\"/>";
    if let Some((_, bytes)) = entries
        .iter_mut()
        .find(|(name, _)| file_name(name) == "[content_types].xml")
    {
        let Ok(xml) = std::str::from_utf8(bytes) else {
            return;
        };
        if xml.contains(PART) {
            return;
        }
        let updated = if let Some(at) = xml.rfind("</Types>") {
            let mut out = String::new();
            out.push_str(&xml[..at]);
            out.push_str(OVERRIDE);
            out.push_str(&xml[at..]);
            out
        } else {
            format!("{xml}{OVERRIDE}")
        };
        *bytes = updated.into_bytes();
        return;
    }
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">{OVERRIDE}</Types>"
    );
    entries.push(("[Content_Types].xml".into(), xml.into_bytes()));
}

fn ensure_comments_rel(entries: &mut Vec<(String, Vec<u8>)>, story: &str) {
    let rels = rels_name(story);
    const TYPE: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments";
    if let Some((_, bytes)) = entries
        .iter_mut()
        .find(|(name, _)| name.replace('\\', "/") == rels)
    {
        let Ok(xml) = std::str::from_utf8(bytes) else {
            return;
        };
        if xml.contains(TYPE) {
            return;
        }
        let id = next_rel_id(xml);
        let rel = format!("<Relationship Id=\"{id}\" Type=\"{TYPE}\" Target=\"comments.xml\"/>");
        let updated = if let Some(at) = xml.rfind("</Relationships>") {
            let mut out = String::new();
            out.push_str(&xml[..at]);
            out.push_str(&rel);
            out.push_str(&xml[at..]);
            out
        } else {
            format!("{xml}{rel}")
        };
        *bytes = updated.into_bytes();
        return;
    }
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"{TYPE}\" Target=\"comments.xml\"/></Relationships>"
    );
    entries.push((rels, xml.into_bytes()));
}

fn rels_name(story: &str) -> String {
    let story = story.replace('\\', "/");
    match story.rfind('/') {
        Some(slash) => {
            let dir = &story[..slash];
            let file = &story[slash + 1..];
            format!("{dir}/_rels/{file}.rels")
        }
        None => format!("_rels/{story}.rels"),
    }
}

fn next_rel_id(xml: &str) -> String {
    let mut max_id = 0u32;
    let mut rest = xml;
    while let Some(found) = rest.find("Id=\"rId") {
        rest = &rest[found + "Id=\"rId".len()..];
        let Some(end) = rest.find('"') else {
            break;
        };
        if let Ok(value) = rest[..end].parse::<u32>() {
            max_id = max_id.max(value);
        }
        rest = &rest[end..];
    }
    format!("rId{}", max_id + 1)
}

fn file_name(name: &str) -> String {
    name.replace('\\', "/")
        .rsplit('/')
        .next()
        .unwrap_or(name)
        .to_ascii_lowercase()
}
