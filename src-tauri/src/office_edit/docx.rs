//! Edit WordprocessingML text without touching paragraph or run properties.

use super::xmlscan::{
    attr_local_value, collect_attr_local, decode_xml_bytes, group_nodes, joined, leaf_texts,
    map_attr_local, next_tag, render_nodes, replace_in_group, rewrite_group, scan_text_nodes,
    ParaGroup, ScanOpt, TextNode,
};
use super::{push_leftover, ChangeStats, EditRequest, Leftover, Probe, MAX_LEFTOVERS};

struct Story {
    entry: usize,
    label: String,
    xml: String,
    nodes: Vec<TextNode>,
    groups: Vec<ParaGroup>,
}

struct Hit {
    entry: usize,
    group: usize,
    old: String,
    location: String,
}

pub fn edit(entries: &mut [(String, Vec<u8>)], req: &EditRequest) -> Result<ChangeStats, String> {
    let mut stories = Vec::new();
    for (index, (name, bytes)) in entries.iter().enumerate() {
        if !is_word_story(name) {
            continue;
        }
        let xml =
            decode_xml_bytes(bytes).ok_or_else(|| format!("{name} を文字列として読めません。"))?;
        let nodes = scan_text_nodes(&xml, &word_scan());
        let groups = group_nodes(&nodes);
        stories.push(Story {
            entry: index,
            label: word_label(name),
            xml,
            nodes,
            groups,
        });
    }
    if stories.is_empty() {
        return Err("本文のない .docx です。".into());
    }

    let mut hits = Vec::new();
    for edit in &req.paragraphs {
        let found = find_paragraphs(&stories, &edit.match_text);
        if found.is_empty() {
            return Err(format!(
                "段落の指定「{}」が見つかりません。その段落だけに含まれる文字列を指定してください。",
                clip(&edit.match_text)
            ));
        }
        if found.len() > 1 {
            let sample = found
                .iter()
                .take(3)
                .map(|hit| hit.location.as_str())
                .collect::<Vec<_>>()
                .join("、");
            return Err(format!(
                "段落の指定「{}」が {} 件あります（{sample}）。その段落だけの文字列を指定してください。",
                clip(&edit.match_text),
                found.len()
            ));
        }
        let hit = found.into_iter().next().unwrap();
        if hits
            .iter()
            .any(|prev: &Hit| prev.entry == hit.entry && prev.group == hit.group)
        {
            return Err(
                "段落の指定が同じ段落に重なっています。1回の呼び出しでは別の段落にしてください。"
                    .into(),
            );
        }
        hits.push(hit);
    }

    let mut stats = ChangeStats::from_request(req);

    for (index, edit) in req.paragraphs.iter().enumerate() {
        let hit = &hits[index];
        let story = stories
            .iter_mut()
            .find(|story| story.entry == hit.entry)
            .unwrap();
        let group = story.groups[hit.group].clone();
        rewrite_group(&mut story.nodes, &group, &edit.text);
        stats.paragraphs += 1;
        if hit.old.chars().count() >= 8 && hit.old != edit.text {
            stats
                .paragraph_checks
                .push((hit.old.clone(), edit.text.clone()));
        }
    }

    for story in &mut stories {
        for group in story.groups.clone() {
            for (index, item) in req.replacements.iter().enumerate() {
                super::record_hit(
                    &mut stats,
                    index,
                    replace_in_group(&mut story.nodes, &group.visible, &item.find, &item.replace),
                );
                super::record_hit(
                    &mut stats,
                    index,
                    replace_in_group(&mut story.nodes, &group.deleted, &item.find, &item.replace),
                );
            }
        }
        story.xml = render_nodes(&story.xml, &story.nodes);
        story.xml = map_attr_local(&story.xml, "author", |value| {
            super::replace_value(value, req, &mut stats)
        });
        story.xml = map_attr_local(&story.xml, "initials", |value| {
            super::replace_value(value, req, &mut stats)
        });
        entries[story.entry].1 = std::mem::take(&mut story.xml).into_bytes();
    }
    super::edit_doc_props(entries, req, &mut stats);
    fill_near_misses(entries, &mut stats);
    super::note_unreachable_misses(entries, &mut stats);
    Ok(stats)
}

fn fill_near_misses(entries: &[(String, Vec<u8>)], stats: &mut ChangeStats) {
    let pending: Vec<(usize, String)> = stats
        .hits
        .iter()
        .enumerate()
        .filter(|(_, (_, _, count))| *count == 0)
        .map(|(index, (find, _, _))| (index, find.clone()))
        .collect();
    if pending.is_empty() {
        return;
    }
    let texts = package_texts(entries);
    for (index, find) in pending {
        if let Some(slot) = stats.near_misses.get_mut(index) {
            *slot = super::text::near_misses(&texts, &find, super::text::NEAR_MISS_LIMIT);
        }
    }
}

fn package_texts(entries: &[(String, Vec<u8>)]) -> Vec<String> {
    let mut texts = Vec::new();
    for (name, bytes) in entries {
        let Some(xml) = decode_xml_bytes(bytes) else {
            continue;
        };
        if is_word_story(name) {
            let nodes = scan_text_nodes(&xml, &word_scan());
            let groups = group_nodes(&nodes);
            for group in &groups {
                push_text(&mut texts, joined(&nodes, &group.visible));
                push_text(&mut texts, joined(&nodes, &group.deleted));
            }
            texts.extend(collect_attr_local(&xml, "author"));
            texts.extend(collect_attr_local(&xml, "initials"));
            continue;
        }
        if is_rels(name) {
            texts.extend(hyperlink_targets(&xml));
            continue;
        }
        if !is_xml_name(name) {
            continue;
        }
        for (_, text) in leaf_texts(&xml) {
            push_text(&mut texts, text);
        }
        texts.extend(collect_attr_local(&xml, "author"));
        texts.extend(collect_attr_local(&xml, "initials"));
    }
    texts
}

fn push_text(texts: &mut Vec<String>, text: String) {
    if !text.is_empty() {
        texts.push(text);
    }
}

fn find_paragraphs(stories: &[Story], needle: &str) -> Vec<Hit> {
    let mut hits = Vec::new();
    for story in stories {
        let locations = paragraph_locations(&story.label, &story.groups, &story.nodes);
        for (index, group) in story.groups.iter().enumerate() {
            if !is_body_paragraph(group) {
                continue;
            }
            let visible = joined(&story.nodes, &group.visible);
            if !super::text::contains_match(&visible, needle) {
                continue;
            }
            hits.push(Hit {
                entry: story.entry,
                group: index,
                old: visible,
                location: locations
                    .get(index)
                    .filter(|loc| !loc.is_empty())
                    .cloned()
                    .unwrap_or_else(|| story.label.clone()),
            });
        }
    }
    hits
}

fn is_body_paragraph(group: &ParaGroup) -> bool {
    group.local == "p" || group.local.is_empty()
}

fn paragraph_locations(label: &str, groups: &[ParaGroup], nodes: &[TextNode]) -> Vec<String> {
    let mut number = 0usize;
    groups
        .iter()
        .map(|group| {
            if !is_body_paragraph(group) {
                return String::new();
            }
            let visible = joined(nodes, &group.visible);
            let deleted = joined(nodes, &group.deleted);
            if visible.trim().is_empty() && deleted.trim().is_empty() {
                return String::new();
            }
            number += 1;
            format!("{label}の{number}段落目")
        })
        .collect()
}

pub fn check(entries: &[(String, Vec<u8>)], probes: &[Probe]) -> (Vec<Leftover>, bool) {
    let mut leftovers = Vec::new();
    let mut truncated = false;
    for (name, bytes) in entries {
        let Some(xml) = decode_xml_bytes(bytes) else {
            continue;
        };
        if is_word_story(name) {
            let label = word_label(name);
            let nodes = scan_text_nodes(&xml, &word_scan());
            let groups = group_nodes(&nodes);
            let locations = paragraph_locations(&label, &groups, &nodes);
            for (index, group) in groups.iter().enumerate() {
                let visible = joined(&nodes, &group.visible);
                let deleted = joined(&nodes, &group.deleted);
                let location = if group.local == "rt" {
                    format!("{label}のルビ")
                } else if is_body_paragraph(group) {
                    locations
                        .get(index)
                        .cloned()
                        .filter(|item| !item.is_empty())
                        .unwrap_or_else(|| label.clone())
                } else {
                    label.clone()
                };
                for probe in probes {
                    consider(&mut leftovers, &mut truncated, probe, &visible, &location);
                    if !deleted.trim().is_empty() {
                        consider(
                            &mut leftovers,
                            &mut truncated,
                            probe,
                            &deleted,
                            &format!("{location}（変更履歴の削除）"),
                        );
                    }
                }
            }
            for author in collect_attr_local(&xml, "author") {
                for probe in probes {
                    consider(
                        &mut leftovers,
                        &mut truncated,
                        probe,
                        &author,
                        &format!("作成者（{label}）"),
                    );
                }
            }
            for initials in collect_attr_local(&xml, "initials") {
                for probe in probes {
                    consider(
                        &mut leftovers,
                        &mut truncated,
                        probe,
                        &initials,
                        &format!("イニシャル（{label}）"),
                    );
                }
            }
            for (local, text) in leaf_texts(&xml) {
                if local != "instrText" && local != "delInstrText" {
                    continue;
                }
                for probe in probes {
                    consider(
                        &mut leftovers,
                        &mut truncated,
                        probe,
                        &text,
                        &super::unreachable_location("フィールド"),
                    );
                }
            }
            continue;
        }
        if is_rels(name) {
            for target in hyperlink_targets(&xml) {
                for probe in probes {
                    consider(
                        &mut leftovers,
                        &mut truncated,
                        probe,
                        &target,
                        &super::unreachable_location("ハイパーリンク"),
                    );
                }
            }
            continue;
        }
        if !is_xml_name(name) {
            continue;
        }
        let label = other_label(name);
        for (_, text) in leaf_texts(&xml) {
            for probe in probes {
                consider(&mut leftovers, &mut truncated, probe, &text, &label);
            }
        }
        for author in collect_attr_local(&xml, "author") {
            for probe in probes {
                consider(
                    &mut leftovers,
                    &mut truncated,
                    probe,
                    &author,
                    &format!("作成者（{label}）"),
                );
            }
        }
        for initials in collect_attr_local(&xml, "initials") {
            for probe in probes {
                consider(
                    &mut leftovers,
                    &mut truncated,
                    probe,
                    &initials,
                    &format!("イニシャル（{label}）"),
                );
            }
        }
    }
    (leftovers, truncated)
}

fn consider(
    leftovers: &mut Vec<Leftover>,
    truncated: &mut bool,
    probe: &Probe,
    text: &str,
    location: &str,
) {
    push_leftover(leftovers, truncated, probe, text, location, MAX_LEFTOVERS);
}

fn word_scan() -> ScanOpt {
    ScanOpt {
        para_locals: &["p", "rt"],
        text_locals: &["t", "delText"],
        track_del: true,
        cached_string_values: false,
    }
}

pub fn is_word_story(name: &str) -> bool {
    let file = file_name(name);
    let path = name.replace('\\', "/");
    if !path.split('/').any(|part| part == "word") {
        return false;
    }
    file == "document.xml"
        || file == "footnotes.xml"
        || file == "endnotes.xml"
        || file == "comments.xml"
        || (file.starts_with("header") && file.ends_with(".xml"))
        || (file.starts_with("footer") && file.ends_with(".xml"))
}

fn word_label(name: &str) -> String {
    let file = file_name(name);
    if file == "document.xml" {
        "本文".into()
    } else if file.starts_with("header") {
        format!("ヘッダー（{file}）")
    } else if file.starts_with("footer") {
        format!("フッター（{file}）")
    } else if file == "footnotes.xml" {
        "脚注".into()
    } else if file == "endnotes.xml" {
        "文末脚注".into()
    } else if file == "comments.xml" {
        "コメント".into()
    } else {
        format!("本文（{file}）")
    }
}

fn other_label(name: &str) -> String {
    if let Some(kind) = super::unreachable_kind(name) {
        return super::unreachable_location(kind);
    }
    let path = name.replace('\\', "/");
    if path.starts_with("docProps/") {
        "文書のプロパティ".into()
    } else if path.contains("comment") {
        "コメント".into()
    } else {
        format!("その他（{path}）")
    }
}

pub(crate) fn hyperlink_targets(xml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while let Some(tag) = next_tag(xml, pos) {
        if !tag.is_end && tag.local == "Relationship" {
            let raw = &xml[tag.start..tag.end];
            let kind = attr_local_value(raw, "Type").unwrap_or_default();
            if kind.to_ascii_lowercase().contains("hyperlink") {
                if let Some(target) = attr_local_value(raw, "Target") {
                    if !target.is_empty() {
                        out.push(target);
                    }
                }
            }
        }
        pos = tag.end;
    }
    out
}

pub(crate) fn is_rels(name: &str) -> bool {
    name.replace('\\', "/")
        .to_ascii_lowercase()
        .ends_with(".rels")
}

fn is_xml_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".xml") || lower.ends_with(".rels")
}

fn file_name(name: &str) -> String {
    name.replace('\\', "/")
        .rsplit('/')
        .next()
        .unwrap_or(name)
        .to_string()
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
