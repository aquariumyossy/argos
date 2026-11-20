//! Edit WordprocessingML text without touching paragraph or run properties.

use super::xmlscan::{
    apply_word_regions, attr_local_value, collect_attr_local, decode_xml_bytes,
    duplicate_fallback_ranges, group_nodes, joined, leaf_texts, lock_at, locked_story_ranges,
    next_tag, range_contains, render_nodes, replace_in_group, rewrite_attr_local, rewrite_group,
    scan_text_nodes, ParaGroup, ScanOpt, TextNode,
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

pub fn edit(
    entries: &mut Vec<(String, Vec<u8>)>,
    req: &EditRequest,
) -> Result<ChangeStats, String> {
    let mut owned = req.clone();
    if owned.revision_author.trim().is_empty() {
        owned.revision_author = super::office_revision_author("");
    }
    if owned.revision_date.trim().is_empty() {
        owned.revision_date = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    }
    let req = &owned;
    let mut stories = Vec::new();
    for (index, (name, bytes)) in entries.iter().enumerate() {
        if !is_word_story(name) {
            continue;
        }
        let xml =
            decode_xml_bytes(bytes).ok_or_else(|| format!("{name} を文字列として読めません。"))?;
        let (nodes, groups) = scan_story(&xml);
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
        let mirrors: Vec<ParaGroup> = story
            .groups
            .iter()
            .filter(|other| {
                other.fallback
                    && other.locked.is_none()
                    && editable_joined(&story.nodes, &other.visible) == hit.old
            })
            .cloned()
            .collect();
        rewrite_group(&mut story.nodes, &group, &edit.text);
        for mirror in &mirrors {
            rewrite_group(&mut story.nodes, mirror, &edit.text);
        }
        stats.paragraphs += 1;
        if hit.old.chars().count() >= 8 && hit.old != edit.text {
            stats
                .paragraph_checks
                .push((hit.old.clone(), edit.text.clone()));
        }
    }

    let markup = req.track_changes
        || req
            .replacements
            .iter()
            .any(|item| !item.comment.trim().is_empty());
    let mut next_id = super::revise::max_revision_id(entries).saturating_add(1);
    let mut made_comments = Vec::new();
    let mut shape_direct = false;
    let mut tracked_hits = 0usize;
    let mut untracked_span = false;
    let mut skipped_comments = false;
    for story in &mut stories {
        if markup {
            if story.nodes.iter().any(|node| node.dirty) {
                story.xml = render_nodes(&story.xml, &story.nodes);
                let (nodes, groups) = scan_story(&story.xml);
                story.nodes = nodes;
                story.groups = groups;
            }
            let revised = super::revise::revise_story(
                &story.xml,
                &story.nodes,
                &story.groups,
                req,
                &mut next_id,
                comments_allowed(&entries[story.entry].0),
            );
            for hit in revised.hits {
                if hit.record {
                    super::record_hit(
                        &mut stats,
                        hit.index,
                        super::text::ReplaceHit {
                            count: hit.count,
                            exact: hit.exact,
                            sources: hit.sources,
                        },
                    );
                    if hit.tracked {
                        tracked_hits += hit.count;
                    }
                }
            }
            if !revised.comments.is_empty() {
                made_comments.extend(revised.comments);
            }
            shape_direct |= revised.shape_direct;
            untracked_span |= revised.untracked_span;
            skipped_comments |= revised.skipped_comments;
            story.xml = revised.xml;
        } else {
            for group in story.groups.clone() {
                let visible = editable_indexes(&story.nodes, &group.visible);
                let deleted = editable_indexes(&story.nodes, &group.deleted);
                for (index, item) in req.replacements.iter().enumerate() {
                    let in_visible =
                        replace_in_group(&mut story.nodes, &visible, &item.find, &item.replace);
                    let in_deleted =
                        replace_in_group(&mut story.nodes, &deleted, &item.find, &item.replace);
                    if !group.fallback {
                        super::record_hit(&mut stats, index, in_visible);
                        super::record_hit(&mut stats, index, in_deleted);
                    }
                }
            }
            story.xml = render_nodes(&story.xml, &story.nodes);
        }
        story.xml = rewrite_story_attrs(&story.xml, req, &mut stats);
        entries[story.entry].1 = std::mem::take(&mut story.xml).into_bytes();
    }
    if !made_comments.is_empty() {
        super::revise::install_comments(
            entries,
            &made_comments,
            &req.revision_author,
            &req.revision_date,
        );
    }
    if tracked_hits > 0 {
        stats.notes.push(format!(
            "変更履歴: 作成者「{}」で記録しました。",
            req.revision_author
        ));
    }
    if !made_comments.is_empty() {
        stats
            .notes
            .push(format!("コメント: {} 件", made_comments.len()));
    }
    if shape_direct && req.track_changes {
        stats
            .notes
            .push("図形の文字は変更履歴にできないため、文字だけ置換しました。".into());
    }
    if untracked_span {
        stats.notes.push(
            "途中で別の要素をまたぐ置換は変更履歴にできないため、その箇所は文字だけ置換しました。"
                .into(),
        );
    }
    if skipped_comments {
        stats.notes.push(
            "ヘッダー、フッター、脚注、文末脚注にはコメントを付けられないため、その箇所のコメントは付けませんでした。"
                .into(),
        );
    }
    super::edit_doc_props(entries, req, &mut stats);
    super::authors::apply(entries, &req.core_authors, &mut stats)?;
    fill_near_misses(entries, &mut stats);
    super::note_unreachable_misses(entries, &mut stats);
    Ok(stats)
}

fn fill_near_misses(entries: &[(String, Vec<u8>)], stats: &mut ChangeStats) {
    let finds: Vec<(usize, String)> = stats
        .hits
        .iter()
        .enumerate()
        .map(|(index, (find, _, _))| (index, find.clone()))
        .collect();
    if finds.is_empty() {
        return;
    }
    let texts = package_texts(entries);
    for (index, find) in finds {
        if let Some(slot) = stats.near_misses.get_mut(index) {
            *slot = super::text::near_misses(&texts, &find, super::text::NEAR_MISS_LIMIT);
        }
    }
}

fn package_texts(entries: &[(String, Vec<u8>)]) -> Vec<(String, String)> {
    let mut texts = Vec::new();
    for (name, bytes) in entries {
        let Some(xml) = decode_xml_bytes(bytes) else {
            continue;
        };
        if is_word_story(name) {
            let (nodes, groups) = scan_story(&xml);
            for group in &groups {
                if group.fallback {
                    continue;
                }
                push_text(&mut texts, editable_joined(&nodes, &group.visible), "");
                push_text(&mut texts, editable_joined(&nodes, &group.deleted), "");
                if let Some((kind, text)) = locked_joined(&nodes, &group.visible) {
                    push_text(&mut texts, text, &super::unreachable_location(kind));
                }
                if let Some((kind, text)) = locked_joined(&nodes, &group.deleted) {
                    push_text(&mut texts, text, &super::unreachable_location(kind));
                }
            }
            for (text, place) in story_attr_texts(&xml, "author") {
                push_text(&mut texts, text, &place);
            }
            for (text, place) in story_attr_texts(&xml, "initials") {
                push_text(&mut texts, text, &place);
            }
            let field = super::unreachable_location("フィールド");
            for (local, text) in leaf_texts(&xml) {
                if matches!(local.as_str(), "instrText" | "delInstrText") {
                    push_text(&mut texts, text, &field);
                }
            }
            continue;
        }
        if is_rels(name) {
            let link = super::unreachable_location("ハイパーリンク");
            for target in hyperlink_targets(&xml) {
                push_text(&mut texts, target, &link);
            }
            continue;
        }
        if !is_xml_name(name) {
            continue;
        }
        let place = super::unreachable_kind(name)
            .map(super::unreachable_location)
            .unwrap_or_default();
        for (local, text) in leaf_texts(&xml) {
            if super::is_document_author_leaf(name, &local) {
                continue;
            }
            push_text(&mut texts, text, &place);
        }
        for author in collect_attr_local(&xml, "author") {
            push_text(&mut texts, author, &place);
        }
        for initials in collect_attr_local(&xml, "initials") {
            push_text(&mut texts, initials, &place);
        }
    }
    texts
}

fn push_text(texts: &mut Vec<(String, String)>, text: String, location: &str) {
    if !text.is_empty() {
        texts.push((text, location.to_string()));
    }
}

fn find_paragraphs(stories: &[Story], needle: &str) -> Vec<Hit> {
    let mut hits = Vec::new();
    for story in stories {
        let locations = paragraph_locations(&story.label, &story.groups, &story.nodes);
        for (index, group) in story.groups.iter().enumerate() {
            if group.fallback || !is_body_paragraph(group) {
                continue;
            }
            let visible = editable_joined(&story.nodes, &group.visible);
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
            if group.fallback || !is_body_paragraph(group) {
                return String::new();
            }
            let visible = editable_joined(nodes, &group.visible);
            let deleted = editable_joined(nodes, &group.deleted);
            if visible.trim().is_empty() && deleted.trim().is_empty() {
                return String::new();
            }
            number += 1;
            format!("{label}の{number}段落目")
        })
        .collect()
}

pub fn check(
    entries: &[(String, Vec<u8>)],
    probes: &[Probe],
    own_author: Option<&str>,
) -> (Vec<Leftover>, bool) {
    let mut leftovers = Vec::new();
    let mut truncated = false;
    for (name, bytes) in entries {
        let Some(xml) = decode_xml_bytes(bytes) else {
            continue;
        };
        if is_word_story(name) {
            let label = word_label(name);
            let (nodes, groups) = scan_story(&xml);
            let locations = paragraph_locations(&label, &groups, &nodes);
            for (index, group) in groups.iter().enumerate() {
                if group.fallback {
                    continue;
                }
                let visible = editable_joined(&nodes, &group.visible);
                let own_ranges = own_author
                    .map(|author| super::revise::own_del_ranges(&xml, author))
                    .unwrap_or_default();
                let deleted_ids: Vec<usize> = group
                    .deleted
                    .iter()
                    .copied()
                    .filter(|&index| {
                        nodes[index].locked.is_none()
                            && !own_ranges.iter().any(|(start, end)| {
                                *start <= nodes[index].open_start && nodes[index].open_start < *end
                            })
                    })
                    .collect();
                let deleted = joined(&nodes, &deleted_ids);
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
                    if let Some((kind, text)) = locked_joined(&nodes, &group.visible) {
                        consider(
                            &mut leftovers,
                            &mut truncated,
                            probe,
                            &text,
                            &super::unreachable_location(kind),
                        );
                    }
                    if let Some((kind, text)) = locked_joined(&nodes, &group.deleted) {
                        consider(
                            &mut leftovers,
                            &mut truncated,
                            probe,
                            &text,
                            &super::unreachable_location(kind),
                        );
                    }
                }
            }
            for (author, place) in story_attr_texts(&xml, "author") {
                let location = if place.is_empty() {
                    format!("作成者（{label}）")
                } else {
                    place
                };
                for probe in probes {
                    consider(&mut leftovers, &mut truncated, probe, &author, &location);
                }
            }
            for (initials, place) in story_attr_texts(&xml, "initials") {
                let location = if place.is_empty() {
                    format!("イニシャル（{label}）")
                } else {
                    place
                };
                for probe in probes {
                    consider(&mut leftovers, &mut truncated, probe, &initials, &location);
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
        for (local, text) in leaf_texts(&xml) {
            if super::is_document_author_leaf(name, &local) {
                continue;
            }
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

fn scan_story(xml: &str) -> (Vec<TextNode>, Vec<ParaGroup>) {
    let mut nodes = scan_text_nodes(xml, &word_scan());
    apply_word_regions(xml, &mut nodes);
    let groups = group_nodes(&nodes);
    (nodes, groups)
}

fn editable_indexes(nodes: &[TextNode], indexes: &[usize]) -> Vec<usize> {
    indexes
        .iter()
        .copied()
        .filter(|&index| nodes[index].locked.is_none())
        .collect()
}

fn editable_joined(nodes: &[TextNode], indexes: &[usize]) -> String {
    joined(nodes, &editable_indexes(nodes, indexes))
}

fn locked_joined(nodes: &[TextNode], indexes: &[usize]) -> Option<(&'static str, String)> {
    let mut kind = None;
    let mut text = String::new();
    for &index in indexes {
        let Some(lock) = nodes[index].locked else {
            continue;
        };
        if kind.is_none() {
            kind = Some(lock);
        }
        text.push_str(&nodes[index].text);
    }
    let kind = kind?;
    if text.is_empty() {
        None
    } else {
        Some((kind, text))
    }
}

/// Locked chart / SmartArt text inside a story part, for the unreachable report.
pub(crate) fn locked_group_texts(xml: &str) -> Vec<(&'static str, String)> {
    let (nodes, groups) = scan_story(xml);
    let mut out = Vec::new();
    for group in &groups {
        if group.fallback {
            continue;
        }
        if let Some((kind, text)) = locked_joined(&nodes, &group.visible) {
            out.push((kind, text));
        }
        if let Some((kind, text)) = locked_joined(&nodes, &group.deleted) {
            out.push((kind, text));
        }
    }
    out
}

fn story_attr_texts(xml: &str, local: &str) -> Vec<(String, String)> {
    let fallbacks = duplicate_fallback_ranges(xml);
    let locks = locked_story_ranges(xml);
    let mut out = Vec::new();
    let mut pos = 0usize;
    while let Some(tag) = next_tag(xml, pos) {
        if !tag.is_end {
            if range_contains(&fallbacks, tag.start) {
                pos = tag.end;
                continue;
            }
            if let Some(value) = attr_local_value(&xml[tag.start..tag.end], local) {
                if !value.is_empty() {
                    let place = lock_at(&locks, tag.start)
                        .map(super::unreachable_location)
                        .unwrap_or_default();
                    out.push((value, place));
                }
            }
        }
        pos = tag.end;
    }
    out
}

fn rewrite_story_attrs(
    xml: &str,
    req: &super::EditRequest,
    stats: &mut super::ChangeStats,
) -> String {
    let fallbacks = duplicate_fallback_ranges(xml);
    let locks = locked_story_ranges(xml);
    let mut out = String::with_capacity(xml.len());
    let mut pos = 0usize;
    while let Some(tag) = next_tag(xml, pos) {
        out.push_str(&xml[pos..tag.start]);
        let raw = &xml[tag.start..tag.end];
        if tag.is_end || lock_at(&locks, tag.start).is_some() || authored_by_this_call(raw, req) {
            out.push_str(raw);
        } else if range_contains(&fallbacks, tag.start) {
            out.push_str(&rewrite_names(raw, |value| quiet_replace(value, req)));
        } else {
            out.push_str(&rewrite_names(raw, |value| {
                super::replace_value(value, req, stats)
            }));
        }
        pos = tag.end;
    }
    out.push_str(&xml[pos..]);
    out
}

fn authored_by_this_call(raw: &str, req: &super::EditRequest) -> bool {
    let Some(tag) = next_tag(raw, 0) else {
        return false;
    };
    if tag.is_end || !matches!(tag.local.as_str(), "del" | "ins" | "comment") {
        return false;
    }
    let author = attr_local_value(raw, "author").unwrap_or_default();
    let date = attr_local_value(raw, "date").unwrap_or_default();
    !req.revision_author.is_empty()
        && author == req.revision_author
        && !req.revision_date.is_empty()
        && date == req.revision_date
}

fn comments_allowed(path: &str) -> bool {
    file_name(path).eq_ignore_ascii_case("document.xml")
}

fn rewrite_names(tag: &str, mut edit: impl FnMut(&str) -> String) -> String {
    let mut piece = tag.to_string();
    for local in ["author", "initials"] {
        piece = rewrite_attr_local(&piece, local, &mut edit);
    }
    piece
}

fn quiet_replace(text: &str, req: &super::EditRequest) -> String {
    let mut pieces = vec![text.to_string()];
    for item in &req.replacements {
        let _ = super::text::replace_pieces_flex(&mut pieces, &item.find, &item.replace);
    }
    pieces.remove(0)
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
    } else if file == "footnotes.xml" {
        "脚注".into()
    } else if file == "endnotes.xml" {
        "文末脚注".into()
    } else if file.starts_with("header") {
        format!("ヘッダー（{file}）")
    } else if file.starts_with("footer") {
        format!("フッター（{file}）")
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
