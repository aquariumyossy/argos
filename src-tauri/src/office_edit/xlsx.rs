//! Edit cell text and sheet names. Styles, columns, and formulas stay in place
//! except sheet-name references, which follow a renamed sheet.

use std::collections::HashMap;

use super::text::replace_pieces;
use super::xmlscan::{
    attr_local_value, decode_xml_bytes, group_nodes, joined, leaf_texts, map_element_attr,
    map_leaf_local, next_tag, render_nodes, replace_in_group, rewrite_group, scan_text_nodes,
    ParaGroup, ScanOpt, TextNode,
};
use super::{push_leftover, ChangeStats, EditRequest, Leftover, Probe, MAX_LEFTOVERS};

struct Story {
    xml: String,
    nodes: Vec<TextNode>,
    groups: Vec<ParaGroup>,
}

struct SheetPart {
    entry: usize,
    name: String,
    story: Story,
}

enum Store {
    Shared(usize),
    Inline { sheet: usize, group: usize },
}

struct CellTarget {
    location: String,
    text: String,
    store: Store,
}

struct Cell {
    coord: String,
    typ: String,
    value: String,
    inline_at: Option<usize>,
}

pub fn edit(entries: &mut [(String, Vec<u8>)], req: &EditRequest) -> Result<ChangeStats, String> {
    let shared_entry = find_entry(entries, |name| file_name(name) == "sharedstrings.xml");
    let workbook_entry = find_entry(entries, |name| file_name(name) == "workbook.xml");
    let rel_entry = find_entry(entries, |name| file_name(name) == "workbook.xml.rels");
    let mut stats = ChangeStats {
        hits: req
            .replacements
            .iter()
            .map(|item| (item.find.clone(), item.replace.clone(), 0))
            .collect(),
        paragraphs: 0,
        notes: Vec::new(),
        paragraph_checks: Vec::new(),
    };

    let mut shared = shared_entry.and_then(|index| load_story(&entries[index].1, shared_scan()));
    let mut sheets = Vec::new();
    let sheet_names = workbook_entry
        .and_then(|index| decode_xml_bytes(&entries[index].1))
        .map(|xml| read_sheet_names(&xml))
        .unwrap_or_default();
    let rels = rel_entry
        .and_then(|index| decode_xml_bytes(&entries[index].1))
        .map(|xml| read_sheet_rels(&xml))
        .unwrap_or_default();
    let name_by_part = sheet_part_names(&sheet_names, &rels);

    for (index, (name, bytes)) in entries.iter().enumerate() {
        if !is_worksheet(name) {
            continue;
        }
        let Some(story) = load_story(bytes, sheet_scan()) else {
            continue;
        };
        let key = norm_part(name);
        let sheet_name = name_by_part
            .get(&key)
            .cloned()
            .unwrap_or_else(|| file_name(name));
        sheets.push(SheetPart {
            entry: index,
            name: sheet_name,
            story,
        });
    }
    if shared.is_none() && sheets.is_empty() {
        return Err("セルのない .xlsx です。".into());
    }

    let targets = cell_targets(shared.as_ref(), &sheets);
    let mut chosen: Vec<(CellTarget, String)> = Vec::new();
    for edit in &req.paragraphs {
        let found: Vec<&CellTarget> = targets
            .iter()
            .filter(|target| super::text::contains_match(&target.text, &edit.match_text))
            .collect();
        if found.is_empty() {
            return Err(format!(
                "段落の指定「{}」に一致するセルがありません。そのセルだけに含まれる文字列を指定してください。",
                clip(&edit.match_text)
            ));
        }
        if found.len() > 1 {
            let sample = found
                .iter()
                .take(3)
                .map(|target| target.location.as_str())
                .collect::<Vec<_>>()
                .join("、");
            return Err(format!(
                "段落の指定「{}」が {} 件のセルにあります（{sample}）。そのセルだけの文字列を指定してください。",
                clip(&edit.match_text),
                found.len()
            ));
        }
        let target = found.into_iter().next().unwrap();
        if chosen
            .iter()
            .any(|(prev, _)| same_store(&prev.store, &target.store))
        {
            return Err(
                "段落の指定が同じセルに重なっています。1回の呼び出しでは別のセルにしてください。"
                    .into(),
            );
        }
        chosen.push((clone_target(target), edit.text.clone()));
    }

    for (target, new_text) in &chosen {
        match &target.store {
            Store::Shared(group) => {
                if let Some(story) = shared.as_mut() {
                    rewrite_group(&mut story.nodes, &story.groups[*group], new_text);
                }
            }
            Store::Inline { sheet, group } => {
                let story = &mut sheets[*sheet].story;
                rewrite_group(&mut story.nodes, &story.groups[*group], new_text);
            }
        }
        stats.paragraphs += 1;
        if target.text.chars().count() >= 8 && target.text != *new_text {
            stats
                .paragraph_checks
                .push((target.text.clone(), new_text.clone()));
        }
    }

    if let Some(story) = shared.as_mut() {
        apply_replacements(story, req, &mut stats);
    }
    for sheet in &mut sheets {
        apply_replacements(&mut sheet.story, req, &mut stats);
    }

    let mut renames = Vec::new();
    if let Some(index) = workbook_entry {
        if let Some(xml) = decode_xml_bytes(&entries[index].1) {
            let (next, sheet_renames, notes) = rename_sheets(&xml, req, &mut stats);
            stats.notes.extend(notes);
            renames = sheet_renames;
            let with_formulas = rewrite_formula_parts(&next, &renames);
            entries[index].1 = with_formulas.into_bytes();
        }
    }
    if let Some(story) = shared {
        if let Some(index) = shared_entry {
            entries[index].1 = render_nodes(&story.xml, &story.nodes).into_bytes();
        }
    }
    for sheet in sheets {
        let mut xml = render_nodes(&sheet.story.xml, &sheet.story.nodes);
        xml = rewrite_formula_parts(&xml, &renames);
        entries[sheet.entry].1 = xml.into_bytes();
    }
    Ok(stats)
}

fn apply_replacements(story: &mut Story, req: &EditRequest, stats: &mut ChangeStats) {
    for group in story.groups.clone() {
        for (index, item) in req.replacements.iter().enumerate() {
            stats.hits[index].2 +=
                replace_in_group(&mut story.nodes, &group.visible, &item.find, &item.replace);
            stats.hits[index].2 +=
                replace_in_group(&mut story.nodes, &group.deleted, &item.find, &item.replace);
        }
    }
}

fn rename_sheets(
    workbook: &str,
    req: &EditRequest,
    stats: &mut ChangeStats,
) -> (String, Vec<(String, String)>, Vec<String>) {
    let original = read_sheet_names(workbook)
        .into_iter()
        .map(|(name, _)| name)
        .collect::<Vec<_>>();
    let mut planned = original.clone();
    let mut notes = Vec::new();
    for (index, name) in planned.iter_mut().enumerate() {
        let (next, _) = apply_named(&original[index], req);
        if next == original[index] {
            continue;
        }
        if !sheet_name_ok(&next) {
            notes.push(format!(
                "シート名「{}」は「{next}」にできないため、変えていません。",
                original[index]
            ));
            continue;
        }
        *name = next;
    }
    let mut tally: HashMap<String, usize> = HashMap::new();
    for name in &planned {
        *tally.entry(name.clone()).or_insert(0) += 1;
    }
    let mut accepted = vec![false; original.len()];
    let mut accepted_counts = vec![0usize; req.replacements.len()];
    for index in 0..original.len() {
        if planned[index] == original[index] {
            continue;
        }
        if tally.get(&planned[index]).copied().unwrap_or(0) == 1 {
            accepted[index] = true;
            let (_, counts) = apply_named(&original[index], req);
            for (slot, count) in accepted_counts.iter_mut().zip(counts) {
                *slot += count;
            }
        } else {
            notes.push(format!(
                "シート名「{}」は「{}」にすると重複するため、変えていません。",
                original[index], planned[index]
            ));
            planned[index] = original[index].clone();
        }
    }
    for (slot, count) in stats.hits.iter_mut().zip(accepted_counts) {
        slot.2 += count;
    }
    let mut cursor = 0usize;
    let xml = map_element_attr(workbook, "sheet", "name", |current| {
        let index = cursor;
        cursor += 1;
        if accepted.get(index) == Some(&true) {
            planned[index].clone()
        } else {
            current.to_string()
        }
    });
    let renames = original
        .iter()
        .zip(planned.iter())
        .filter(|(old, new)| old != new)
        .map(|(old, new)| (old.clone(), new.clone()))
        .collect();
    (xml, renames, notes)
}

fn apply_named(text: &str, req: &EditRequest) -> (String, Vec<usize>) {
    let mut pieces = vec![text.to_string()];
    let mut counts = Vec::new();
    for item in &req.replacements {
        counts.push(replace_pieces(&mut pieces, &item.find, &item.replace));
    }
    (pieces.remove(0), counts)
}

fn rewrite_formula_parts(xml: &str, renames: &[(String, String)]) -> String {
    if renames.is_empty() {
        return xml.to_string();
    }
    let once = map_leaf_local(xml, "f", |text| rewrite_sheet_refs(text, renames));
    map_leaf_local(&once, "definedName", |text| {
        rewrite_sheet_refs(text, renames)
    })
}

fn rewrite_sheet_refs(text: &str, renames: &[(String, String)]) -> String {
    if renames.is_empty() || !text.contains('!') {
        return text.to_string();
    }
    let mut order: Vec<&(String, String)> = renames.iter().collect();
    order.sort_by_key(|(old, _)| std::cmp::Reverse(old.chars().count()));
    let mut out = text.to_string();
    let mut tokens = Vec::new();
    for (index, (old, new)) in order.iter().enumerate() {
        let token = format!("\u{E000}{index}\u{E001}");
        out = replace_quoted_sheet(&out, old, &token);
        out = replace_bare_sheet(&out, old, &token);
        tokens.push((token, sheet_ref(new)));
    }
    for (token, rendered) in tokens {
        out = out.replace(&token, &rendered);
    }
    out
}

fn replace_quoted_sheet(text: &str, old: &str, token: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut index = 0usize;
    while index < chars.len() {
        if chars[index] == '\'' {
            let mut name = String::new();
            let mut cursor = index + 1;
            let mut closed = false;
            while cursor < chars.len() {
                if chars[cursor] == '\'' {
                    if cursor + 1 < chars.len() && chars[cursor + 1] == '\'' {
                        name.push('\'');
                        cursor += 2;
                        continue;
                    }
                    closed = cursor + 1 < chars.len() && chars[cursor + 1] == '!';
                    break;
                }
                name.push(chars[cursor]);
                cursor += 1;
            }
            if closed && name == old {
                out.push_str(token);
                index = cursor + 2;
                continue;
            }
        }
        out.push(chars[index]);
        index += 1;
    }
    out
}

fn replace_bare_sheet(text: &str, old: &str, token: &str) -> String {
    if !is_bare_sheet_name(old) {
        return text.to_string();
    }
    let needle = format!("{old}!");
    let mut out = String::new();
    let mut rest = text;
    while let Some(pos) = rest.find(&needle) {
        let before = &rest[..pos];
        let boundary = before
            .chars()
            .next_back()
            .map(|ch| !is_name_char(ch))
            .unwrap_or(true);
        out.push_str(before);
        if boundary {
            out.push_str(token);
        } else {
            out.push_str(&needle);
        }
        rest = &rest[pos + needle.len()..];
    }
    out.push_str(rest);
    out
}

fn sheet_ref(name: &str) -> String {
    if is_bare_sheet_name(name) {
        format!("{name}!")
    } else {
        format!("'{}'!", name.replace('\'', "''"))
    }
}

fn is_bare_sheet_name(name: &str) -> bool {
    if name.is_empty() || name.chars().count() > 31 || looks_like_cell(name) {
        return false;
    }
    let Some(first) = name.chars().next() else {
        return false;
    };
    if first.is_ascii_digit() {
        return false;
    }
    name.chars().all(is_name_char)
}

fn is_name_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_' || (!ch.is_ascii() && ch != '　')
}

fn looks_like_cell(name: &str) -> bool {
    let bytes = name.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() && bytes[index].is_ascii_alphabetic() {
        index += 1;
    }
    index > 0
        && index <= 3
        && index < bytes.len()
        && bytes[index..].iter().all(|byte| byte.is_ascii_digit())
}

fn sheet_name_ok(name: &str) -> bool {
    let count = name.chars().count();
    count > 0
        && count <= 31
        && !name.starts_with('\'')
        && !name.ends_with('\'')
        && !name
            .chars()
            .any(|ch| matches!(ch, ':' | '\\' | '/' | '?' | '*' | '[' | ']'))
}

fn cell_targets(shared: Option<&Story>, sheets: &[SheetPart]) -> Vec<CellTarget> {
    let mut targets = Vec::new();
    let si_groups = shared
        .map(|story| si_group_indexes(&story.groups))
        .unwrap_or_default();
    for (sheet_index, sheet) in sheets.iter().enumerate() {
        let inline_groups = group_indexes(&sheet.story.groups, "is");
        for cell in worksheet_cells(&sheet.story.xml) {
            if cell.typ == "s" {
                let Ok(index) = cell.value.trim().parse::<usize>() else {
                    continue;
                };
                let Some(&group) = si_groups.get(index) else {
                    continue;
                };
                let text = shared
                    .map(|story| joined(&story.nodes, &story.groups[group].visible))
                    .unwrap_or_default();
                targets.push(CellTarget {
                    location: cell_location(&sheet.name, &cell.coord),
                    text,
                    store: Store::Shared(group),
                });
            } else if cell.typ == "inlineStr" {
                let Some(at) = cell.inline_at else {
                    continue;
                };
                let Some(&group) = inline_groups.get(at) else {
                    continue;
                };
                let text = joined(&sheet.story.nodes, &sheet.story.groups[group].visible);
                targets.push(CellTarget {
                    location: cell_location(&sheet.name, &cell.coord),
                    text,
                    store: Store::Inline {
                        sheet: sheet_index,
                        group,
                    },
                });
            }
        }
    }
    targets
}

fn clone_target(target: &CellTarget) -> CellTarget {
    CellTarget {
        location: target.location.clone(),
        text: target.text.clone(),
        store: match target.store {
            Store::Shared(group) => Store::Shared(group),
            Store::Inline { sheet, group } => Store::Inline { sheet, group },
        },
    }
}

fn same_store(left: &Store, right: &Store) -> bool {
    match (left, right) {
        (Store::Shared(a), Store::Shared(b)) => a == b,
        (
            Store::Inline {
                sheet: left_sheet,
                group: left_group,
            },
            Store::Inline {
                sheet: right_sheet,
                group: right_group,
            },
        ) => left_sheet == right_sheet && left_group == right_group,
        _ => false,
    }
}

pub fn check(entries: &[(String, Vec<u8>)], probes: &[Probe]) -> (Vec<Leftover>, bool) {
    let mut leftovers = Vec::new();
    let mut truncated = false;
    let shared_xml = entries.iter().find_map(|(name, bytes)| {
        (file_name(name) == "sharedstrings.xml")
            .then(|| decode_xml_bytes(bytes))
            .flatten()
    });
    let shared_story = shared_xml.as_ref().map(|xml| {
        let nodes = scan_text_nodes(xml, &shared_scan());
        let groups = group_nodes(&nodes);
        (nodes, groups)
    });
    let si_text = shared_story.as_ref().map(|(nodes, groups)| {
        group_indexes(groups, "si")
            .into_iter()
            .map(|index| joined(nodes, &groups[index].visible))
            .collect::<Vec<_>>()
    });
    let phonetics = shared_story.as_ref().map(|(nodes, groups)| {
        phonetic_by_si(
            nodes,
            groups,
            si_text.as_ref().map(|items| items.len()).unwrap_or(0),
        )
    });
    let workbook = entries.iter().find_map(|(name, bytes)| {
        (file_name(name) == "workbook.xml")
            .then(|| decode_xml_bytes(bytes))
            .flatten()
    });
    let rels = entries.iter().find_map(|(name, bytes)| {
        (file_name(name) == "workbook.xml.rels")
            .then(|| decode_xml_bytes(bytes))
            .flatten()
    });
    let sheet_names = workbook
        .as_ref()
        .map(|xml| read_sheet_names(xml))
        .unwrap_or_default();
    let name_by_part = sheet_part_names(
        &sheet_names,
        &rels.as_deref().map(read_sheet_rels).unwrap_or_default(),
    );
    let mut seen_si = vec![false; si_text.as_ref().map(|items| items.len()).unwrap_or(0)];

    for (name, bytes) in entries {
        let Some(xml) = decode_xml_bytes(bytes) else {
            continue;
        };
        if is_worksheet(name) {
            let sheet_name = name_by_part
                .get(&norm_part(name))
                .cloned()
                .unwrap_or_else(|| file_name(name));
            let nodes = scan_text_nodes(&xml, &sheet_scan());
            let groups = group_nodes(&nodes);
            let inline_groups = group_indexes(&groups, "is");
            for cell in worksheet_cells(&xml) {
                let location = cell_location(&sheet_name, &cell.coord);
                if cell.typ == "s" {
                    if let Ok(index) = cell.value.trim().parse::<usize>() {
                        if let Some(text) = si_text.as_ref().and_then(|items| items.get(index)) {
                            if index < seen_si.len() {
                                seen_si[index] = true;
                            }
                            for probe in probes {
                                push_leftover(
                                    &mut leftovers,
                                    &mut truncated,
                                    probe,
                                    text,
                                    &location,
                                    MAX_LEFTOVERS,
                                );
                            }
                        }
                        if let Some(reads) = phonetics.as_ref().and_then(|items| items.get(index)) {
                            for read in reads {
                                for probe in probes {
                                    push_leftover(
                                        &mut leftovers,
                                        &mut truncated,
                                        probe,
                                        read,
                                        &format!("{location}のふりがな"),
                                        MAX_LEFTOVERS,
                                    );
                                }
                            }
                        }
                    }
                } else if cell.typ == "inlineStr" {
                    if let Some(text) = cell
                        .inline_at
                        .and_then(|at| inline_groups.get(at))
                        .map(|&group| joined(&nodes, &groups[group].visible))
                    {
                        for probe in probes {
                            push_leftover(
                                &mut leftovers,
                                &mut truncated,
                                probe,
                                &text,
                                &location,
                                MAX_LEFTOVERS,
                            );
                        }
                    }
                } else if cell.typ == "str" {
                    for probe in probes {
                        push_leftover(
                            &mut leftovers,
                            &mut truncated,
                            probe,
                            cell.value.trim(),
                            &location,
                            MAX_LEFTOVERS,
                        );
                    }
                }
                if cell.typ != "s" && cell.typ != "inlineStr" && cell.typ != "str" {
                    // Numeric cells are values, not the replaced wording.
                }
            }
            for (local, text) in leaf_texts(&xml) {
                if !matches!(
                    local.as_str(),
                    "oddHeader"
                        | "oddFooter"
                        | "evenHeader"
                        | "evenFooter"
                        | "firstHeader"
                        | "firstFooter"
                ) {
                    continue;
                }
                let place = if local.contains("Footer") {
                    format!("シート「{sheet_name}」のページフッター")
                } else {
                    format!("シート「{sheet_name}」のページヘッダー")
                };
                for probe in probes {
                    push_leftover(
                        &mut leftovers,
                        &mut truncated,
                        probe,
                        &text,
                        &place,
                        MAX_LEFTOVERS,
                    );
                }
            }
            for (_, text) in formula_texts(&xml) {
                for probe in probes {
                    push_leftover(
                        &mut leftovers,
                        &mut truncated,
                        probe,
                        &text,
                        &format!("シート「{sheet_name}」の数式"),
                        MAX_LEFTOVERS,
                    );
                }
            }
            continue;
        }
        if file_name(name) == "workbook.xml" {
            for (sheet_name, _) in read_sheet_names(&xml) {
                for probe in probes {
                    push_leftover(
                        &mut leftovers,
                        &mut truncated,
                        probe,
                        &sheet_name,
                        &format!("シート名「{sheet_name}」"),
                        MAX_LEFTOVERS,
                    );
                }
            }
            for (_, text) in leaf_texts(&xml) {
                // defined names and other workbook text
                for probe in probes {
                    push_leftover(
                        &mut leftovers,
                        &mut truncated,
                        probe,
                        &text,
                        "ブックの定義名",
                        MAX_LEFTOVERS,
                    );
                }
            }
            continue;
        }
        if file_name(name) == "sharedstrings.xml" || file_name(name) == "workbook.xml.rels" {
            continue;
        }
        if !name.to_ascii_lowercase().ends_with(".xml") {
            continue;
        }
        let label = if name.replace('\\', "/").contains("comment") {
            "コメント".to_string()
        } else if name.replace('\\', "/").starts_with("docProps/") {
            "文書のプロパティ".to_string()
        } else {
            format!("その他（{}）", name.replace('\\', "/"))
        };
        for (_, text) in leaf_texts(&xml) {
            for probe in probes {
                push_leftover(
                    &mut leftovers,
                    &mut truncated,
                    probe,
                    &text,
                    &label,
                    MAX_LEFTOVERS,
                );
            }
        }
    }
    if let Some(texts) = si_text {
        for (index, text) in texts.iter().enumerate() {
            if seen_si.get(index).copied().unwrap_or(false) {
                continue;
            }
            for probe in probes {
                push_leftover(
                    &mut leftovers,
                    &mut truncated,
                    probe,
                    text,
                    "共有文字列",
                    MAX_LEFTOVERS,
                );
            }
        }
    }
    (leftovers, truncated)
}

fn phonetic_by_si(nodes: &[TextNode], groups: &[ParaGroup], count: usize) -> Vec<Vec<String>> {
    let mut out = vec![Vec::new(); count];
    let mut current = None;
    for group in groups {
        if group.local == "si" {
            current = Some(current.map(|index: usize| index + 1).unwrap_or(0));
        } else if group.local == "rPh" {
            if let Some(index) = current {
                if let Some(slot) = out.get_mut(index) {
                    slot.push(joined(nodes, &group.visible));
                }
            }
        }
    }
    out
}

fn formula_texts(xml: &str) -> Vec<(String, String)> {
    leaf_texts(xml)
        .into_iter()
        .filter(|(local, text)| local == "f" && text.contains('!'))
        .collect()
}

fn worksheet_cells(xml: &str) -> Vec<Cell> {
    let mut cells = Vec::new();
    let mut open: Option<Cell> = None;
    let mut inline_count = 0usize;
    let mut pos = 0usize;
    while let Some(tag) = next_tag(xml, pos) {
        if tag.is_end && tag.local == "c" {
            if let Some(cell) = open.take() {
                cells.push(cell);
            }
            pos = tag.end;
            continue;
        }
        if !tag.is_end && tag.local == "c" && !tag.is_self {
            let raw = &xml[tag.start..tag.end];
            open = Some(Cell {
                coord: attr_local_value(raw, "r").unwrap_or_else(|| "セル".into()),
                typ: attr_local_value(raw, "t").unwrap_or_default(),
                value: String::new(),
                inline_at: None,
            });
        } else if !tag.is_end && tag.local == "is" && !tag.is_self {
            if let Some(cell) = open.as_mut() {
                cell.inline_at = Some(inline_count);
            }
            inline_count += 1;
        } else if !tag.is_end && tag.local == "v" && !tag.is_self {
            let close = format!("</{}>", tag.qname);
            if let Some(rel) = xml[tag.end..].find(&close) {
                let text = super::xmlscan::unescape_text(&xml[tag.end..tag.end + rel]);
                if let Some(cell) = open.as_mut() {
                    cell.value = text;
                }
                pos = tag.end + rel + close.len();
                continue;
            }
        }
        pos = tag.end;
    }
    cells
}

fn read_sheet_names(xml: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while let Some(tag) = next_tag(xml, pos) {
        if !tag.is_end && tag.local == "sheet" {
            let raw = &xml[tag.start..tag.end];
            let name = attr_local_value(raw, "name").unwrap_or_default();
            let rel = raw_attr(raw, "r:id").unwrap_or_default();
            if !name.is_empty() {
                out.push((name, rel));
            }
        }
        pos = tag.end;
    }
    out
}

fn read_sheet_rels(xml: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut pos = 0usize;
    while let Some(tag) = next_tag(xml, pos) {
        if !tag.is_end && tag.local == "Relationship" {
            let raw = &xml[tag.start..tag.end];
            let id = raw_attr(raw, "Id").unwrap_or_default();
            let target = raw_attr(raw, "Target").unwrap_or_default();
            if !id.is_empty() && !target.is_empty() {
                out.insert(id, norm_part(&join_xl_target(&target)));
            }
        }
        pos = tag.end;
    }
    out
}

fn raw_attr(tag: &str, key: &str) -> Option<String> {
    // `r:id` is not the same attribute as `sheetId`. Match the full key.
    let pattern = format!("{key}=");
    let start = tag.find(&pattern)?;
    let rest = &tag[start + pattern.len()..];
    let quote = rest.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let value = rest[quote.len_utf8()..].split(quote).next()?;
    Some(super::xmlscan::unescape_text(value))
}

fn sheet_part_names(
    sheets: &[(String, String)],
    rels: &HashMap<String, String>,
) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for (name, rel) in sheets {
        if let Some(part) = rels.get(rel) {
            out.insert(part.clone(), name.clone());
        }
    }
    out
}

fn join_xl_target(target: &str) -> String {
    let target = target.replace('\\', "/");
    let target = target.trim_start_matches('/');
    if target.starts_with("xl/") {
        target.to_string()
    } else {
        format!("xl/{target}")
    }
}

fn load_story(bytes: &[u8], opt: ScanOpt) -> Option<Story> {
    let xml = decode_xml_bytes(bytes)?;
    let nodes = scan_text_nodes(&xml, &opt);
    let groups = group_nodes(&nodes);
    Some(Story { xml, nodes, groups })
}

fn shared_scan() -> ScanOpt {
    ScanOpt {
        para_locals: &["si", "rPh"],
        text_locals: &["t"],
        track_del: false,
        cached_string_values: false,
    }
}

fn sheet_scan() -> ScanOpt {
    ScanOpt {
        para_locals: &["is", "rPh"],
        text_locals: &["t"],
        track_del: false,
        cached_string_values: true,
    }
}

fn si_group_indexes(groups: &[ParaGroup]) -> Vec<usize> {
    group_indexes(groups, "si")
}

fn group_indexes(groups: &[ParaGroup], local: &str) -> Vec<usize> {
    groups
        .iter()
        .enumerate()
        .filter(|(_, group)| group.local == local)
        .map(|(index, _)| index)
        .collect()
}

fn find_entry(entries: &[(String, Vec<u8>)], pred: impl Fn(&str) -> bool) -> Option<usize> {
    entries.iter().position(|(name, _)| pred(name))
}

fn is_worksheet(name: &str) -> bool {
    let path = norm_part(name);
    path.starts_with("xl/worksheets/") && path.ends_with(".xml")
}

fn norm_part(name: &str) -> String {
    name.replace('\\', "/").trim_start_matches('/').to_string()
}

fn file_name(name: &str) -> String {
    norm_part(name)
        .rsplit('/')
        .next()
        .unwrap_or(name)
        .to_ascii_lowercase()
}

fn cell_location(sheet: &str, coord: &str) -> String {
    format!("シート「{sheet}」セル {coord}")
}

fn clip(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= 40 {
        return trimmed.to_string();
    }
    let mut out: String = trimmed.chars().take(40).collect();
    out.push('…');
    out
}
