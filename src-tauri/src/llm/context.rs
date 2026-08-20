//! Assemble one LLM request: source bodies once per originating user turn.

use std::collections::{HashMap, HashSet};

use crate::db::{LlmMessageRow, LlmSourceRow};

pub const PER_SOURCE_CAP: usize = 40_000;
const MIN_KEEP_TURNS: usize = 4;

pub const CITATION_GUIDE: &str = "\n\n出典ブロックがあるときは、その本文だけを根拠にしてください。出典にない事実は推測だと明示し、根拠箇所には [n]（出典番号）を付けてください。";
pub const IMAGE_TRANSCRIPT_NOTE: &str = "（画像の書き起こし。原本の画素ではない）";

#[derive(Debug, Clone)]
pub struct ChatTurn {
    pub role: String,
    pub content: String,
    pub name: Option<String>,
    pub tool_call_id: Option<String>,
    pub tool_calls: Option<serde_json::Value>,
}

impl ChatTurn {
    pub fn text(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: content.into(),
            name: None,
            tool_call_id: None,
            tool_calls: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ConsumedSource {
    pub id: String,
    pub cite_no: i64,
}

#[derive(Debug, Clone, Default)]
pub struct AssembleStats {
    pub total_chars: usize,
    pub source_chars: usize,
    pub dropped_sources: usize,
    pub truncated: bool,
    pub consumed: Vec<ConsumedSource>,
}

fn char_len(s: &str) -> usize {
    s.chars().count()
}

fn cap_chars(s: &str, max: usize) -> String {
    if char_len(s) <= max {
        return s.to_string();
    }
    let take = max.saturating_sub(1);
    let mut out: String = s.chars().take(take).collect();
    out.push('…');
    out
}

fn cite_no_of(s: &LlmSourceRow) -> i64 {
    if s.cite_no > 0 {
        s.cite_no
    } else {
        s.sort_order + 1
    }
}

/// `[n]` in the assistant answer (prose, mermaid labels, code fences).
/// Markdown links `[1](url)` are not citations.
pub fn parse_cited_nos(text: &str) -> HashSet<i64> {
    let mut out = HashSet::new();
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'[' {
            let start = i + 1;
            let mut j = start;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            if j > start && j < b.len() && b[j] == b']' {
                if b.get(j + 1) != Some(&b'(') {
                    if let Ok(n) = std::str::from_utf8(&b[start..j])
                        .unwrap_or("")
                        .parse::<i64>()
                    {
                        if n > 0 {
                            out.insert(n);
                        }
                    }
                }
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Keep only sources whose cite number appears as `[n]` in `answer`.
pub fn consumed_cited_in_answer(consumed: &[(String, i64)], answer: &str) -> Vec<(String, i64)> {
    let nos = parse_cited_nos(answer);
    consumed
        .iter()
        .filter(|(_, n)| nos.contains(n))
        .cloned()
        .collect()
}

pub const FINAL_CITE_HINT: &str =
    "使った出典にだけ [n] を付けて答えてください。使っていない番号は本文に並べないでください。";
pub const STOP_TOOLS_HINT: &str =
    "ツールはこれ以上使わず、これまでに得た出典だけで答えてください。";

/// Rows from this turn's tool/attach consume list, in cite-number order.
pub fn sources_for_consumed(all: &[LlmSourceRow], consumed: &[(String, i64)]) -> Vec<LlmSourceRow> {
    let mut nos = HashMap::new();
    for (id, n) in consumed {
        nos.entry(id.clone()).or_insert(*n);
    }
    let mut rows: Vec<LlmSourceRow> = all
        .iter()
        .filter(|s| nos.contains_key(&s.id))
        .cloned()
        .collect();
    rows.sort_by_key(|s| {
        nos.get(&s.id)
            .copied()
            .filter(|n| *n > 0)
            .unwrap_or(s.cite_no)
    });
    rows
}

/// User turn that makes this-turn hits look like an attached 出典 block.
pub fn final_source_turn(sources: &[LlmSourceRow], stop_tools: bool) -> Option<ChatTurn> {
    let block = format_sources(sources);
    if block.is_empty() && !stop_tools {
        return None;
    }
    let mut content = block;
    if !content.is_empty() {
        content.push_str(FINAL_CITE_HINT);
        content.push('\n');
    }
    if stop_tools {
        content.push_str(STOP_TOOLS_HINT);
    }
    if content.is_empty() {
        return None;
    }
    Some(ChatTurn::text("user", content))
}

pub fn format_sources(sources: &[LlmSourceRow]) -> String {
    if sources.is_empty() {
        return String::new();
    }
    let mut out = String::from("【出典】\n");
    for s in sources {
        let n = cite_no_of(s);
        let title = if s.title.trim().is_empty() {
            s.path.as_str()
        } else {
            s.title.as_str()
        };
        out.push_str(&format!("[{n}] {title}"));
        if !s.path.trim().is_empty() && s.path.trim() != title.trim() {
            out.push_str(" — ");
            out.push_str(s.path.trim());
        }
        // Without the id the model cannot call read_unit, so a shortened tool hit would
        // be a dead end. Only shown where it is actionable: paragraph grain with an id.
        let pid = s.paragraph_id.trim();
        if !pid.is_empty() && !crate::llm::grain::is_file_grain(&s.grain) {
            out.push_str(&format!(" (paragraph_id: {pid})"));
        }
        out.push('\n');
        if s.is_image() {
            out.push_str(IMAGE_TRANSCRIPT_NOTE);
            out.push('\n');
        }
        out.push_str(s.body.trim());
        out.push_str("\n\n");
    }
    out
}

fn wrap_user(sources_block: &str, question: &str) -> String {
    if sources_block.is_empty() {
        return question.to_string();
    }
    format!("{sources_block}【質問】\n{question}")
}

fn turns_chars(turns: &[ChatTurn]) -> usize {
    turns
        .iter()
        .map(|t| char_len(&t.content) + char_len(&t.role) + 8)
        .sum()
}

fn select_sources(
    sources: &[LlmSourceRow],
    budget: usize,
    pending_ids: &HashSet<String>,
) -> (Vec<LlmSourceRow>, usize, bool) {
    let original = sources.len();
    let mut rows: Vec<LlmSourceRow> = sources
        .iter()
        .map(|s| {
            let mut c = s.clone();
            if !crate::llm::grain::is_file_grain(&c.grain) {
                c.body = cap_chars(s.body.trim(), PER_SOURCE_CAP);
            } else {
                c.body = s.body.trim().to_string();
            }
            c
        })
        .collect();

    let over = |rows: &[LlmSourceRow]| char_len(&format_sources(rows)) > budget;
    let is_protected = |s: &LlmSourceRow| pending_ids.contains(&s.id) && s.origin == "attach";

    while over(&rows) {
        if let Some(idx) = rows
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                !is_protected(s)
                    && s.origin != "attach"
                    && !crate::llm::grain::is_file_grain(&s.grain)
            })
            .min_by_key(|(_, s)| s.sort_order)
            .map(|(i, _)| i)
        {
            rows.remove(idx);
            continue;
        }
        break;
    }

    while over(&rows) {
        if let Some(idx) = rows
            .iter()
            .enumerate()
            .filter(|(_, s)| !is_protected(s))
            .min_by_key(|(_, s)| s.sort_order)
            .map(|(i, _)| i)
        {
            rows.remove(idx);
            continue;
        }
        break;
    }

    while over(&rows) && !rows.is_empty() {
        let idx = rows
            .iter()
            .enumerate()
            .filter(|(_, s)| !is_protected(s))
            .max_by_key(|(_, s)| char_len(&s.body))
            .or_else(|| {
                rows.iter()
                    .enumerate()
                    .max_by_key(|(_, s)| char_len(&s.body))
            })
            .map(|(i, _)| i)
            .unwrap_or(0);
        let next = char_len(&rows[idx].body) / 2;
        if next < 400 {
            rows.remove(idx);
        } else {
            rows[idx].body = cap_chars(&rows[idx].body, next);
        }
    }

    let dropped = original.saturating_sub(rows.len());
    let truncated = dropped > 0
        || rows.iter().any(|s| {
            sources
                .iter()
                .find(|o| o.id == s.id)
                .is_some_and(|o| char_len(o.body.trim()) > char_len(&s.body))
        });
    (rows, dropped, truncated)
}

struct BuiltTurn {
    turn: ChatTurn,
    has_sources: bool,
}

/// Build the message list sent to the model.
/// Pending sources (empty `injected_user_message_id`) attach to the latest user turn.
/// Already-consumed sources attach only to the user message that first read them.
pub fn assemble_turns(
    system: &str,
    sources: &[LlmSourceRow],
    history: &[LlmMessageRow],
    max_chars: usize,
) -> (Vec<ChatTurn>, AssembleStats) {
    let injectable: Vec<LlmSourceRow> = sources
        .iter()
        .filter(|s| s.is_injectable())
        .cloned()
        .collect();
    let sources = &injectable;
    let max_chars = max_chars.max(4_000);
    let pending_ids: HashSet<String> = sources
        .iter()
        .filter(|s| s.is_pending())
        .map(|s| s.id.clone())
        .collect();

    let mut sys = system.trim().to_string();
    if !sources.is_empty() && !sys.contains("[n]") {
        sys.push_str(CITATION_GUIDE);
    }

    let hist: Vec<&LlmMessageRow> = history
        .iter()
        .filter(|m| {
            (m.role == "user" || m.role == "assistant" || m.role == "system")
                && !m.content.is_empty()
        })
        .collect();

    let hist_chars: usize = hist.iter().map(|m| char_len(&m.content)).sum();
    let sys_chars = char_len(&sys);
    let source_budget = max_chars
        .saturating_sub(sys_chars)
        .saturating_sub(hist_chars)
        .saturating_sub(32);

    let (mut kept, dropped_sources, src_trunc) =
        select_sources(sources, source_budget, &pending_ids);

    let mut next_cite = sources.iter().map(|s| s.cite_no).max().unwrap_or(0);
    for s in &mut kept {
        if pending_ids.contains(&s.id) && s.cite_no <= 0 {
            next_cite += 1;
            s.cite_no = next_cite;
        }
    }

    let consumed: Vec<ConsumedSource> = kept
        .iter()
        .filter(|s| pending_ids.contains(&s.id))
        .map(|s| ConsumedSource {
            id: s.id.clone(),
            cite_no: s.cite_no,
        })
        .collect();

    let source_chars = char_len(&format_sources(&kept));
    let latest_user_id = hist
        .iter()
        .rev()
        .find(|m| m.role == "user")
        .map(|m| m.id.as_str());

    let mut built: Vec<BuiltTurn> = Vec::new();
    if !sys.is_empty() {
        built.push(BuiltTurn {
            turn: ChatTurn::text("system", sys),
            has_sources: false,
        });
    }

    for m in &hist {
        let mut content = m.content.clone();
        let mut has_sources = false;
        if m.role == "user" {
            let for_turn: Vec<LlmSourceRow> = kept
                .iter()
                .filter(|s| {
                    if pending_ids.contains(&s.id) {
                        latest_user_id == Some(m.id.as_str())
                    } else {
                        s.injected_user_message_id == m.id
                    }
                })
                .cloned()
                .collect();
            if !for_turn.is_empty() {
                has_sources = true;
                content = wrap_user(&format_sources(&for_turn), &content);
            }
        }
        built.push(BuiltTurn {
            turn: ChatTurn::text(m.role.clone(), content),
            has_sources,
        });
    }

    let mut truncated = src_trunc;
    trim_history(&mut built, max_chars, &mut truncated);

    let turns: Vec<ChatTurn> = built.into_iter().map(|b| b.turn).collect();
    let stats = AssembleStats {
        total_chars: turns_chars(&turns),
        source_chars,
        dropped_sources,
        truncated,
        consumed,
    };
    (turns, stats)
}

fn trim_history(built: &mut Vec<BuiltTurn>, max_chars: usize, truncated: &mut bool) {
    let chars_of = |rows: &[BuiltTurn]| -> usize {
        rows.iter()
            .map(|b| char_len(&b.turn.content) + char_len(&b.turn.role) + 8)
            .sum()
    };
    while chars_of(built) > max_chars && built.len() > MIN_KEEP_TURNS + 1 {
        let sys = built.first().is_some_and(|t| t.turn.role == "system");
        let start = if sys { 1 } else { 0 };
        let end = built.len().saturating_sub(MIN_KEEP_TURNS);
        if start >= end {
            break;
        }
        let latest_user = built.iter().rposition(|t| t.turn.role == "user");
        let mut drop_idx = built[start..end]
            .iter()
            .enumerate()
            .find(|(i, t)| {
                let abs = start + *i;
                Some(abs) != latest_user && !t.has_sources
            })
            .map(|(i, _)| start + i);
        if drop_idx.is_none() {
            drop_idx = built[start..end]
                .iter()
                .enumerate()
                .find(|(i, t)| {
                    let abs = start + *i;
                    Some(abs) != latest_user && t.turn.role == "user"
                })
                .map(|(i, _)| start + i);
        }
        let Some(i) = drop_idx else {
            break;
        };
        let drop_following_assistant = built[i].turn.role == "user"
            && built.get(i + 1).is_some_and(|t| t.turn.role == "assistant");
        built.remove(i);
        if drop_following_assistant && i < built.len() && built[i].turn.role == "assistant" {
            built.remove(i);
        }
        *truncated = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(id: &str, origin: &str, sort: i64, body: &str) -> LlmSourceRow {
        LlmSourceRow {
            id: id.into(),
            thread_id: "t".into(),
            sort_order: sort,
            origin: origin.into(),
            path: format!("C:\\{id}"),
            title: id.into(),
            paragraph_id: id.into(),
            body: body.into(),
            query: String::new(),
            created_at: sort,
            grain: "unit".into(),
            unit_body: String::new(),
            injected_user_message_id: String::new(),
            cited_assistant_message_id: String::new(),
            cite_no: 0,
            kind: "text".into(),
            stored_relpath: String::new(),
            ocr_status: String::new(),
        }
    }

    fn src_file(id: &str, origin: &str, sort: i64, body: &str) -> LlmSourceRow {
        let mut s = src(id, origin, sort, body);
        s.grain = "file".into();
        s
    }

    /// Without the id in the block, `read_unit` cannot be called and a shortened tool hit
    /// becomes a dead end.
    #[test]
    fn paragraph_sources_expose_their_id() {
        let out = format_sources(&[src("u1", "tool", 0, "本文")]);
        assert!(
            out.contains("(paragraph_id: u1)"),
            "paragraph grain must be readable via read_unit: {out}"
        );
        // A whole file has no single paragraph to re-read.
        let file = format_sources(&[src_file("f1", "attach", 0, "本文")]);
        assert!(
            !file.contains("paragraph_id"),
            "file grain must not advertise a paragraph id: {file}"
        );
    }

    fn consumed(
        id: &str,
        origin: &str,
        sort: i64,
        body: &str,
        user_id: &str,
        cite: i64,
    ) -> LlmSourceRow {
        let mut s = src(id, origin, sort, body);
        s.injected_user_message_id = user_id.into();
        s.cited_assistant_message_id = "a1".into();
        s.cite_no = cite;
        s
    }

    fn msg(id: &str, role: &str, content: &str) -> LlmMessageRow {
        LlmMessageRow {
            id: id.into(),
            thread_id: "t".into(),
            role: role.into(),
            content: content.into(),
            created_at: 0,
        }
    }

    #[test]
    fn consumed_stays_on_originating_user() {
        let sources = vec![consumed("a", "attach", 0, "民法第1条", "u1", 1)];
        let history = vec![
            msg("u1", "user", "要約して"),
            msg("a1", "assistant", "要約です"),
            msg("u2", "user", "短く"),
        ];
        let (turns, stats) = assemble_turns("役割", &sources, &history, 80_000);
        assert!(turns[0].role == "system");
        assert!(turns[1].content.contains("【出典】"));
        assert!(turns[1].content.contains("[1]"));
        assert!(turns[1].content.contains("要約して"));
        assert!(!turns[3].content.contains("【出典】"));
        assert!(turns[3].content.contains("短く"));
        assert_eq!(stats.dropped_sources, 0);
        assert!(stats.consumed.is_empty());
    }

    #[test]
    fn pending_attaches_to_latest_user() {
        let sources = vec![src("b", "attach", 1, "追送本文")];
        let history = vec![
            msg("u1", "user", "要約して"),
            msg("a1", "assistant", "要約です"),
            msg("u2", "user", "これも見て"),
        ];
        let (turns, stats) = assemble_turns("役割", &sources, &history, 80_000);
        assert!(!turns[1].content.contains("【出典】"));
        assert!(turns[3].content.contains("【出典】"));
        assert!(turns[3].content.contains("追送本文"));
        assert!(turns[3].content.contains("[1]"));
        assert_eq!(stats.consumed.len(), 1);
        assert_eq!(stats.consumed[0].cite_no, 1);
    }

    #[test]
    fn followup_pending_continues_cite_no() {
        let sources = vec![
            consumed("a", "attach", 0, "最初", "u1", 1),
            src("b", "attach", 1, "追加"),
        ];
        let history = vec![
            msg("u1", "user", "要約して"),
            msg("a1", "assistant", "要約です"),
            msg("u2", "user", "これも"),
        ];
        let (turns, stats) = assemble_turns("役割", &sources, &history, 80_000);
        let first_user = turns.iter().find(|t| t.role == "user").unwrap();
        let second_user = turns.iter().rev().find(|t| t.role == "user").unwrap();
        assert!(first_user.content.contains("[1]"));
        assert!(second_user.content.contains("[2]"));
        assert!(!second_user.content.contains("最初"));
        assert_eq!(stats.consumed[0].cite_no, 2);
    }

    #[test]
    fn drops_old_search_before_attach() {
        let long = "あ".repeat(5000);
        let sources = vec![
            src("keep", "attach", 0, "添付本文"),
            src("old", "search", 1, &long),
            src("new", "search", 2, "新しいヒット"),
        ];
        let history = vec![msg("u1", "user", "Q")];
        let (turns, stats) = assemble_turns("", &sources, &history, 4_000);
        let blob = turns
            .iter()
            .find(|t| t.role == "user")
            .map(|t| t.content.as_str())
            .unwrap_or("");
        assert!(blob.contains("添付本文"), "{blob}");
        assert_eq!(stats.dropped_sources, 1);
        assert!(!blob.contains(&long));
    }

    #[test]
    fn keeps_file_grain_search_before_unit_search() {
        let long = "あ".repeat(5000);
        let sources = vec![
            src_file("full", "search", 0, "契約書全文"),
            src("old", "search", 1, &long),
        ];
        let history = vec![msg("u1", "user", "Q")];
        let (turns, stats) = assemble_turns("", &sources, &history, 4_000);
        let blob = turns
            .iter()
            .find(|t| t.role == "user")
            .map(|t| t.content.as_str())
            .unwrap_or("");
        assert!(blob.contains("契約書全文"), "{blob}");
        assert_eq!(stats.dropped_sources, 1);
        assert!(!blob.contains(&long));
    }

    #[test]
    fn no_sources_is_plain_history() {
        let history = vec![msg("u1", "user", "hello")];
        let (turns, _) = assemble_turns("sys", &[], &history, 8_000);
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[1].content, "hello");
        assert!(!turns[1].content.contains("【出典】"));
    }

    #[test]
    fn citation_guide_when_sources() {
        let sources = vec![src("a", "attach", 0, "本文")];
        let history = vec![msg("u1", "user", "Q")];
        let (turns, _) = assemble_turns("役割", &sources, &history, 80_000);
        assert!(turns[0].role == "system");
        assert!(turns[0].content.contains("[n]"));
        assert!(turns[1].content.contains("【出典】"));
    }

    #[test]
    fn parse_cited_nos_from_prose() {
        let nos = parse_cited_nos("契約は成立している [2]。解除は [5] による。");
        assert_eq!(nos, HashSet::from([2, 5]));
    }

    #[test]
    fn parse_cited_nos_in_mermaid_label() {
        let nos = parse_cited_nos(
            "```mermaid\nflowchart LR\n  Kg[\"請求原因：契約成立 [2]\"]\n  K1[\"抗弁 [13]\"]\n```",
        );
        assert!(nos.contains(&2), "{nos:?}");
        assert!(nos.contains(&13), "{nos:?}");
    }

    #[test]
    fn parse_cited_nos_skips_markdown_links() {
        let nos = parse_cited_nos("see [1](https://example.com) and cite [2].");
        assert_eq!(nos, HashSet::from([2]));
    }

    #[test]
    fn parse_cited_nos_ignores_fullwidth_brackets() {
        let nos = parse_cited_nos("全角［3］と半角[4]");
        assert_eq!(nos, HashSet::from([4]));
    }

    #[test]
    fn consumed_cited_keeps_only_numbers_in_answer() {
        let consumed = vec![
            ("tool-a".into(), 1),
            ("tool-b".into(), 2),
            ("attach-c".into(), 3),
        ];
        let kept = consumed_cited_in_answer(&consumed, "根拠は [2] です。");
        assert_eq!(kept, vec![("tool-b".to_string(), 2)]);
    }

    #[test]
    fn consumed_cited_empty_when_answer_has_no_brackets() {
        let consumed = vec![("tool-a".into(), 1), ("attach-b".into(), 2)];
        let kept = consumed_cited_in_answer(&consumed, "検索しましたが該当なし。");
        assert!(kept.is_empty());
    }

    #[test]
    fn sources_for_consumed_keeps_cite_order() {
        let mut a = src("a", "tool", 0, "A");
        a.cite_no = 3;
        let mut b = src("b", "tool", 1, "B");
        b.cite_no = 1;
        let kept = sources_for_consumed(&[a, b], &[("b".into(), 1), ("a".into(), 3)]);
        assert_eq!(
            kept.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            ["b", "a"]
        );
    }

    #[test]
    fn final_source_turn_is_user_role_with_cite_hint() {
        let mut s = src("hit", "tool", 0, "民法第936条");
        s.cite_no = 2;
        s.title = "民法.md".into();
        let turn = final_source_turn(&[s], false).expect("block");
        assert_eq!(turn.role, "user");
        assert!(turn.content.contains("【出典】"), "{}", turn.content);
        assert!(turn.content.contains("[2]"), "{}", turn.content);
        assert!(turn.content.contains("民法第936条"), "{}", turn.content);
        assert!(turn.content.contains("[n]"), "{}", turn.content);
        assert!(
            !turn.content.contains("ツールはこれ以上"),
            "{}",
            turn.content
        );
    }

    #[test]
    fn final_source_turn_stop_tools_without_hits() {
        let turn = final_source_turn(&[], true).expect("stop");
        assert_eq!(turn.role, "user");
        assert!(turn.content.contains("ツールはこれ以上"));
        assert!(!turn.content.contains("【出典】"));
    }

    #[test]
    fn pending_ocr_is_not_injected() {
        let mut pending = src("img", "attach", 0, "");
        pending.kind = "image".into();
        pending.ocr_status = "pending".into();
        pending.grain = "file".into();
        let history = vec![msg("u1", "user", "要約して")];
        let (turns, stats) = assemble_turns("役割", &[pending], &history, 80_000);
        let user = turns.iter().find(|t| t.role == "user").unwrap();
        assert!(!user.content.contains("【出典】"), "{}", user.content);
        assert_eq!(stats.consumed.len(), 0);
    }

    #[test]
    fn image_transcript_note_in_block() {
        let mut img = src("img", "attach", 0, "契約書の写真");
        img.kind = "image".into();
        img.grain = "file".into();
        let out = format_sources(&[img]);
        assert!(out.contains(IMAGE_TRANSCRIPT_NOTE), "{out}");
        assert!(out.contains("契約書の写真"), "{out}");
    }
}
