//! Edit WordprocessingML text without touching paragraph or run properties.

use super::xmlscan::{
    collect_attr_local, decode_xml_bytes, group_nodes, joined, leaf_texts, map_attr_local,
    render_nodes, replace_in_group, rewrite_group, scan_text_nodes, ParaGroup, ScanOpt, TextNode,
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
                stats.hits[index].2 +=
                    replace_in_group(&mut story.nodes, &group.visible, &item.find, &item.replace);
                stats.hits[index].2 +=
                    replace_in_group(&mut story.nodes, &group.deleted, &item.find, &item.replace);
            }
        }
        story.xml = render_nodes(&story.xml, &story.nodes);
        let reps = &req.replacements;
        story.xml = map_attr_local(&story.xml, "author", |value| {
            let mut pieces = vec![value.to_string()];
            for (index, item) in reps.iter().enumerate() {
                stats.hits[index].2 +=
                    super::text::replace_pieces(&mut pieces, &item.find, &item.replace);
            }
            pieces.remove(0)
        });
        entries[story.entry].1 = std::mem::take(&mut story.xml).into_bytes();
    }
    Ok(stats)
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
    let path = name.replace('\\', "/");
    if path.starts_with("docProps/") {
        "文書のプロパティ".into()
    } else if path.contains("comment") {
        "コメント".into()
    } else {
        format!("その他（{path}）")
    }
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
