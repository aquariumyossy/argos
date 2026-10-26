//! Assemble one LLM request: source bodies once per originating user turn.

use std::collections::{HashMap, HashSet};

use crate::db::{LlmMessageRow, LlmSourceRow};

pub const PER_SOURCE_CAP: usize = 40_000;

pub const CITATION_GUIDE: &str = "\n\n出典ブロックがあるときは、その本文だけを根拠にしてください。出典にない事実は推測だと明示し、根拠箇所には [n]（出典番号）を付けてください。";
pub const IMAGE_TRANSCRIPT_NOTE: &str = "（画像の書き起こし。原本の画素ではない）";

pub const REASONING_CARRY_CAP: usize = 4_000;
/// Japanese text is about 1.6 characters per token.
pub const CHARS_PER_TOKEN: f64 = 1.6;
/// Tool results may take this share of the context window. Older ones shrink first.
pub const TOOL_RESULT_BUDGET_RATIO: f64 = 0.25;

pub fn estimate_tokens(text: &str) -> usize {
    let chars = char_len(text);
    if chars == 0 {
        return 0;
    }
    ((chars as f64) / CHARS_PER_TOKEN).ceil() as usize
}

pub fn tokens_to_chars(tokens: usize) -> usize {
    ((tokens as f64) * CHARS_PER_TOKEN).floor() as usize
}

/// Role and delimiters cost a few tokens per message.
pub fn turn_tokens(turn: &ChatTurn) -> usize {
    let mut total = estimate_tokens(&turn.content);
    if let Some(calls) = &turn.tool_calls {
        if let Some(arr) = calls.as_array() {
            for call in arr {
                let name = call
                    .pointer("/function/name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let args = call
                    .pointer("/function/arguments")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                total += estimate_tokens(name) + estimate_tokens(args);
            }
        } else {
            total += estimate_tokens(&calls.to_string());
        }
    }
    total + 4
}

pub fn silent_end_notice(finish_reason: &str) -> String {
    let reason = finish_reason.trim();
    let reason = if reason.is_empty() { "不明" } else { reason };
    format!(
        "モデルが本文も操作も返さずに終わりました（終了理由: {reason}）。もう一度送るか、指示を分けてください。"
    )
}

/// Trim one tool result to the tokens still left in the quarter-window reserve.
pub fn fit_tool_text(content: String, budget_tokens: usize, used_tokens: usize) -> String {
    let left = budget_tokens.saturating_sub(used_tokens);
    if left == 0 {
        return "（文脈の上限に達したため、この結果は省略しました）".into();
    }
    if estimate_tokens(&content) <= left {
        return content;
    }
    truncate_to_tokens(&content, left.saturating_sub(8).max(1))
}

fn truncate_to_tokens(text: &str, max_tokens: usize) -> String {
    let max_chars = tokens_to_chars(max_tokens);
    if char_len(text) <= max_chars {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max_chars).collect();
    out.push_str("\n…（長いので省略しました）");
    out
}

/// Shrink the oldest tool results once they take more than their share of the window.
pub fn cap_tool_results(turns: &mut [ChatTurn], limit_tokens: usize) {
    let budget = ((limit_tokens as f64) * TOOL_RESULT_BUDGET_RATIO).floor() as usize;
    let indexes: Vec<usize> = turns
        .iter()
        .enumerate()
        .filter(|(_, turn)| turn.role == "tool")
        .map(|(index, _)| index)
        .collect();
    let mut used: usize = indexes.iter().map(|index| turn_tokens(&turns[*index])).sum();
    if used <= budget {
        return;
    }
    for index in indexes {
        if used <= budget {
            break;
        }
        let before = turn_tokens(&turns[index]);
        turns[index].content = truncate_to_tokens(&turns[index].content, 40);
        let after = turn_tokens(&turns[index]);
        used = used.saturating_sub(before.saturating_sub(after));
    }
}

#[derive(Debug, Clone)]
pub struct ChatTurn {
    pub role: String,
    pub content: String,
    pub name: Option<String>,
    pub tool_call_id: Option<String>,
    pub tool_calls: Option<serde_json::Value>,
    pub reasoning: Option<String>,
}

impl ChatTurn {
    pub fn text(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: content.into(),
            name: None,
            tool_call_id: None,
            tool_calls: None,
            reasoning: None,
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
    pub total_tokens: usize,
    pub source_chars: usize,
    pub dropped_sources: usize,
    pub truncated: bool,
    pub consumed: Vec<ConsumedSource>,
}

fn char_len(s: &str) -> usize {
    s.chars().count()
}

pub(crate) fn cap_chars(s: &str, max: usize) -> String {
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
pub const PHANTOM_CITE_HINT: &str =
    "存在しない出典番号があります。実際にツールで裏を取るか、出典がないと明示して書き直してください。";

/// True when the answer cites `[n]` that is not among this thread's sources.
pub fn answer_has_phantom_cites(answer: &str, sources: &[LlmSourceRow]) -> bool {
    let cited = parse_cited_nos(answer);
    if cited.is_empty() {
        return false;
    }
    let known: HashSet<i64> = sources.iter().map(cite_no_of).filter(|n| *n > 0).collect();
    cited.iter().any(|n| !known.contains(n))
}

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
    format_sources_with_pool(sources, sources)
}

/// Like [`format_sources`], but `pool` is the pre-trim set used to detect omitted pages.
pub fn format_sources_with_pool(shown: &[LlmSourceRow], pool: &[LlmSourceRow]) -> String {
    if shown.is_empty() {
        return String::new();
    }
    let mut out = String::from("【出典】\n");
    for unit in crate::llm::transcript::walk_format_units(shown) {
        let seed = unit[0];
        if seed.is_image() && crate::llm::transcript::image_group_key(seed).is_some() {
            let n = crate::llm::transcript::group_cite_no(&unit);
            let title = crate::llm::transcript::file_label(&seed.path, &seed.title);
            out.push_str(&format!("[{n}] {title}"));
            if !seed.path.trim().is_empty() && seed.path.trim() != title {
                out.push_str(" — ");
                out.push_str(seed.path.trim());
            }
            out.push('\n');
            out.push_str(IMAGE_TRANSCRIPT_NOTE);
            out.push('\n');
            if let Some(note) = crate::llm::transcript::omission_note(&unit, pool) {
                out.push_str(&note);
                out.push('\n');
            }
            let multi = unit.len() > 1;
            out.push_str(&crate::llm::transcript::transcript_body(&unit, multi));
            out.push_str("\n\n");
            continue;
        }

        let n = cite_no_of(seed);
        let title = if seed.title.trim().is_empty() {
            seed.path.as_str()
        } else {
            seed.title.as_str()
        };
        out.push_str(&format!("[{n}] {title}"));
        if !seed.path.trim().is_empty() && seed.path.trim() != title.trim() {
            out.push_str(" — ");
            out.push_str(seed.path.trim());
        }
        if seed.is_web() {
            out.push_str("（ウェブ）");
        } else if seed.is_remote() {
            out.push_str("（リモート）");
        }
        let pid = seed.paragraph_id.trim();
        if !pid.is_empty() && !crate::llm::grain::is_file_grain(&seed.grain) {
            out.push_str(&format!(" (paragraph_id: {pid})"));
        }
        out.push('\n');
        if seed.is_image() {
            out.push_str(IMAGE_TRANSCRIPT_NOTE);
            out.push('\n');
        }
        out.push_str(seed.body.trim());
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
    max_tokens: usize,
) -> (Vec<ChatTurn>, AssembleStats) {
    let injectable: Vec<LlmSourceRow> = sources
        .iter()
        .filter(|s| s.is_injectable())
        .cloned()
        .collect();
    let sources = &injectable;
    let max_tokens = max_tokens.max(1);
    let pending_ids: HashSet<String> = sources
        .iter()
        .filter(|s| s.is_pending())
        .map(|s| s.id.clone())
        .collect();

    let mut sys = system.trim().to_string();
    if !sources.is_empty() && !sys.contains("[n]") {
        sys.push_str(CITATION_GUIDE);
    }

    let hist: Vec<&LlmMessageRow> = history.iter().filter(|m| message_is_replayed(m)).collect();

    let hist_tokens: usize = hist.iter().map(|m| message_tokens(m)).sum();
    let sys_tokens = if sys.is_empty() {
        0
    } else {
        estimate_tokens(&sys) + 4
    };
    let source_token_budget = max_tokens
        .saturating_sub(sys_tokens)
        .saturating_sub(hist_tokens)
        .saturating_sub(8);
    let source_budget = tokens_to_chars(source_token_budget).max(1);

    let (mut kept, dropped_sources, src_trunc) =
        select_sources(sources, source_budget, &pending_ids);

    let mut next_cite = sources.iter().map(|s| s.cite_no).max().unwrap_or(0);
    crate::llm::transcript::assign_group_cites(&mut kept, sources, &pending_ids, &mut next_cite);

    let consumed: Vec<ConsumedSource> = kept
        .iter()
        .filter(|s| pending_ids.contains(&s.id))
        .map(|s| ConsumedSource {
            id: s.id.clone(),
            cite_no: s.cite_no,
        })
        .collect();

    let source_chars = char_len(&format_sources_with_pool(&kept, sources));
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
        let mut turn = history_turn(m);
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
                turn.content = wrap_user(&format_sources_with_pool(&for_turn, sources), &turn.content);
            }
        }
        built.push(BuiltTurn { turn, has_sources });
    }

    let mut truncated = src_trunc;
    cap_built_tool_results(&mut built, max_tokens);
    trim_history(&mut built, max_tokens, &mut truncated);

    let turns: Vec<ChatTurn> = built.into_iter().map(|b| b.turn).collect();
    let stats = AssembleStats {
        total_chars: turns_chars(&turns),
        total_tokens: turns.iter().map(turn_tokens).sum(),
        source_chars,
        dropped_sources,
        truncated,
        consumed,
    };
    (turns, stats)
}

/// Past turns keep tool calls and tool results, and drop the reasoning channel.
/// A tool message with an empty body is still a reply the server expects.
fn message_is_replayed(message: &LlmMessageRow) -> bool {
    match message.role.as_str() {
        "user" | "system" => !message.content.is_empty(),
        "assistant" => !message.content.is_empty() || !message.tool_calls.trim().is_empty(),
        "tool" => !message.content.is_empty() || !message.tool_call_id.trim().is_empty(),
        _ => false,
    }
}

fn message_tokens(message: &LlmMessageRow) -> usize {
    turn_tokens(&history_turn(message))
}

/// Reasoning is stored for the chat pane. It is not sent again on a later turn.
fn history_turn(message: &LlmMessageRow) -> ChatTurn {
    let tool_calls = if message.tool_calls.trim().is_empty() {
        None
    } else {
        serde_json::from_str::<serde_json::Value>(&message.tool_calls).ok()
    };
    ChatTurn {
        role: message.role.clone(),
        content: message.content.clone(),
        name: if message.tool_name.trim().is_empty() {
            None
        } else {
            Some(message.tool_name.clone())
        },
        tool_call_id: if message.tool_call_id.trim().is_empty() {
            None
        } else {
            Some(message.tool_call_id.clone())
        },
        tool_calls,
        reasoning: None,
    }
}

fn cap_built_tool_results(built: &mut [BuiltTurn], limit_tokens: usize) {
    let mut turns: Vec<ChatTurn> = built.iter().map(|row| row.turn.clone()).collect();
    cap_tool_results(&mut turns, limit_tokens);
    for (row, turn) in built.iter_mut().zip(turns) {
        row.turn = turn;
    }
}

fn following_tools(built: &[BuiltTurn], assistant_index: usize) -> usize {
    let mut count = 0;
    let mut index = assistant_index + 1;
    while built.get(index).is_some_and(|row| row.turn.role == "tool") {
        count += 1;
        index += 1;
    }
    count
}

/// Drop the oldest exchanges until the request fits. An assistant turn leaves
/// with the tool replies that follow it. The latest user turn stays.
fn trim_history(built: &mut Vec<BuiltTurn>, max_tokens: usize, truncated: &mut bool) {
    let tokens_of = |rows: &[BuiltTurn]| -> usize { rows.iter().map(|row| turn_tokens(&row.turn)).sum() };
    while tokens_of(built) > max_tokens {
        let start = usize::from(built.first().is_some_and(|row| row.turn.role == "system"));
        let latest_user = built.iter().rposition(|row| row.turn.role == "user");
        let end = latest_user.unwrap_or(built.len());
        if start >= end {
            break;
        }
        let drop_idx = (start..end)
            .find(|index| built[*index].turn.role != "tool" && !built[*index].has_sources)
            .or_else(|| (start..end).find(|index| built[*index].turn.role != "tool"));
        let Some(index) = drop_idx else {
            break;
        };
        let mut tool_count = if built[index].turn.role == "assistant" {
            following_tools(built, index)
        } else {
            0
        };
        let drop_following_assistant = built[index].turn.role == "user"
            && built
                .get(index + 1)
                .is_some_and(|row| row.turn.role == "assistant");
        built.remove(index);
        if drop_following_assistant && built.get(index).is_some_and(|row| row.turn.role == "assistant")
        {
            tool_count = following_tools(built, index);
            built.remove(index);
        }
        for _ in 0..tool_count {
            if built.get(index).is_some_and(|row| row.turn.role == "tool") {
                built.remove(index);
            }
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

    #[test]
    fn web_sources_are_labeled_and_have_no_paragraph_id() {
        let mut w = src("w1", "tool", 0, "スニペット");
        w.kind = "web".into();
        w.path = "https://example.com/case".into();
        w.title = "裁判例".into();
        w.paragraph_id.clear();
        let out = format_sources(&[w]);
        assert!(out.contains("（ウェブ）"), "{out}");
        assert!(out.contains("https://example.com/case"), "{out}");
        assert!(!out.contains("paragraph_id"), "{out}");
    }

    #[test]
    fn remote_sources_are_labeled_and_keep_paragraph_id() {
        let mut row = src("u9", "tool", 0, "本文");
        row.kind = "remote".into();
        row.path = r"\\host\share\a.txt".into();
        row.title = "a.txt".into();
        row.paragraph_id = r"\\host\share\a.txt#1".into();
        let out = format_sources(&[row]);
        assert!(out.contains("（リモート）"), "{out}");
        assert!(out.contains("(paragraph_id: "), "{out}");
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
            reasoning: String::new(),
            tool_calls: String::new(),
            tool_call_id: String::new(),
            tool_name: String::new(),
        }
    }

    #[test]
    fn later_turns_replay_tool_results_without_reasoning() {
        let mut assistant = msg("a1", "assistant", "");
        assistant.reasoning = "これは思考です".into();
        assistant.tool_calls = r#"[{"id":"c1","type":"function","function":{"name":"search_index","arguments":"{\"query\":\"民法\"}"}}]"#.into();
        let mut tool = msg("t1", "tool", "検索結果の本文");
        tool.tool_call_id = "c1".into();
        tool.tool_name = "search_index".into();
        let history = vec![msg("u1", "user", "調べて"), assistant, tool, msg("u2", "user", "続き")];
        let (turns, _) = assemble_turns("役割", &[], &history, 80_000);
        let replayed = turns.iter().find(|t| t.role == "assistant").unwrap();
        assert!(replayed.reasoning.is_none());
        assert!(replayed.tool_calls.is_some());
        let tool_turn = turns.iter().find(|t| t.role == "tool").unwrap();
        assert_eq!(tool_turn.content, "検索結果の本文");
        assert_eq!(tool_turn.tool_call_id.as_deref(), Some("c1"));
        assert!(turns.iter().any(|t| t.content.contains("続き")));
    }

    #[test]
    fn old_tool_results_shrink_to_their_share() {
        let mut tool = msg("t1", "tool", &"あ".repeat(20_000));
        tool.tool_call_id = "c1".into();
        let history = vec![msg("u1", "user", "調べて"), tool, msg("u2", "user", "続き")];
        let (turns, _) = assemble_turns("役割", &[], &history, 1_000);
        let tool_turn = turns.iter().find(|t| t.role == "tool").unwrap();
        assert!(tool_turn.content.contains("省略"));
        assert!(tool_turn.content.chars().count() < 2_000);
        assert!(turns.iter().any(|t| t.content == "続き"));
    }

    #[test]
    fn silent_end_notice_includes_the_finish_reason() {
        let notice = silent_end_notice("length");
        assert!(notice.contains("length"));
        assert!(silent_end_notice("  ").contains("不明"));
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
        let (turns, stats) = assemble_turns("", &sources, &history, 2_000);
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
        let (turns, stats) = assemble_turns("", &sources, &history, 2_000);
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
    fn phantom_cites_when_answer_uses_unknown_number() {
        let mut s = src("a", "attach", 0, "本文");
        s.cite_no = 1;
        assert!(answer_has_phantom_cites("根拠は [3] です。", &[s]));
    }

    #[test]
    fn phantom_cites_false_when_number_exists() {
        let mut s = src("a", "attach", 0, "本文");
        s.cite_no = 1;
        assert!(!answer_has_phantom_cites("根拠は [1] です。", &[s]));
    }

    #[test]
    fn phantom_cites_skips_markdown_links() {
        let mut s = src("a", "attach", 0, "本文");
        s.cite_no = 2;
        assert!(!answer_has_phantom_cites(
            "see [1](https://example.com) and cite [2].",
            &[s]
        ));
    }

    #[test]
    fn phantom_cites_false_when_no_brackets() {
        assert!(!answer_has_phantom_cites("検索しました。", &[]));
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

    fn img_page(id: &str, page: u32, body: &str) -> LlmSourceRow {
        let mut s = src(id, "attach", page as i64, body);
        s.path = r"C:\docs\契約.pdf".into();
        s.title = format!("契約.pdf（{page}ページ目）");
        s.paragraph_id = format!("pdf-page:{page}");
        s.kind = "image".into();
        s.grain = "file".into();
        s
    }

    #[test]
    fn format_sources_merges_scan_pdf_pages() {
        let mut a = img_page("p1", 1, "一枚目");
        a.cite_no = 2;
        let mut b = img_page("p2", 2, "二枚目");
        b.cite_no = 3;
        let mut c = img_page("p3", 3, "三枚目");
        c.cite_no = 4;
        let out = format_sources(&[a, b, c]);
        assert_eq!(out.matches("[2]").count(), 1, "{out}");
        assert!(!out.contains("[3]"), "{out}");
        assert!(out.contains("契約.pdf"), "{out}");
        assert!(!out.contains("（1ページ目）"), "{out}");
        assert!(out.contains("## 1ページ目"), "{out}");
        assert!(out.contains("## 2ページ目"), "{out}");
        assert!(out.contains("一枚目"), "{out}");
        assert!(!out.contains("paragraph_id"), "{out}");
        assert_eq!(out.matches(IMAGE_TRANSCRIPT_NOTE).count(), 1, "{out}");
    }

    #[test]
    fn format_sources_skips_pending_page_body() {
        let a = img_page("p1", 1, "一枚目");
        let mut pending = img_page("p2", 2, "");
        pending.ocr_status = "pending".into();
        let out = format_sources(&[a, pending]);
        assert!(out.contains("一枚目"), "{out}");
        assert!(out.contains("## 1ページ目"), "{out}");
        assert!(!out.contains("## 2ページ目"), "{out}");
    }

    #[test]
    fn format_sources_omits_dropped_pages() {
        let pages = vec![
            img_page("p1", 1, "一枚目"),
            img_page("p2", 2, "二枚目"),
            img_page("p3", 3, "三枚目"),
        ];
        let shown = vec![pages[0].clone(), pages[1].clone()];
        let out = format_sources_with_pool(&shown, &pages);
        assert!(out.contains("（3ページ以降は文字数のため省略）"), "{out}");
        assert!(!out.contains("三枚目"), "{out}");
    }

    #[test]
    fn format_sources_single_image_has_no_page_heading() {
        let mut photo = src("pic", "attach", 0, "写真");
        photo.kind = "image".into();
        photo.grain = "file".into();
        photo.path = r"C:\img\写真.jpg".into();
        photo.title = "写真.jpg".into();
        photo.paragraph_id.clear();
        let out = format_sources(&[photo]);
        assert!(!out.contains("ページ目"), "{out}");
        assert!(out.contains("写真"), "{out}");
    }

    #[test]
    fn format_sources_does_not_merge_text_hit_with_image() {
        let mut img = img_page("p1", 1, "書き起こし");
        img.cite_no = 1;
        let mut hit = src("hit", "tool", 1, "検索ヒット");
        hit.path = r"C:\docs\契約.pdf".into();
        hit.title = "契約.pdf".into();
        hit.cite_no = 5;
        let out = format_sources(&[img, hit]);
        assert!(out.contains("[1]"), "{out}");
        assert!(out.contains("[5]"), "{out}");
        assert!(out.contains("検索ヒット"), "{out}");
        assert!(out.contains("書き起こし"), "{out}");
        assert_eq!(out.matches("【出典】").count(), 1);
        assert!(out.contains("paragraph_id: hit"), "{out}");
    }

    #[test]
    fn assemble_consumes_all_pages_same_cite() {
        let pages = vec![
            img_page("p1", 1, "一枚目"),
            img_page("p2", 2, "二枚目"),
            img_page("p3", 3, "三枚目"),
        ];
        let history = vec![msg("u1", "user", "要約して")];
        let (turns, stats) = assemble_turns("役割", &pages, &history, 80_000);
        assert_eq!(stats.consumed.len(), 3);
        assert!(stats.consumed.iter().all(|c| c.cite_no == 1));
        let user = turns.iter().find(|t| t.role == "user").unwrap();
        assert_eq!(user.content.matches("[1]").count(), 1, "{}", user.content);
        assert!(user.content.contains("## 1ページ目"), "{}", user.content);
        assert!(user.content.contains("三枚目"), "{}", user.content);
    }

    #[test]
    fn assemble_inherits_sibling_cite_no() {
        let mut first = img_page("p1", 1, "一枚目");
        first.cite_no = 7;
        first.injected_user_message_id = "u1".into();
        let second = img_page("p2", 2, "二枚目");
        let history = vec![
            msg("u1", "user", "要約して"),
            msg("a1", "assistant", "要約です"),
            msg("u2", "user", "続き"),
        ];
        let (_, stats) = assemble_turns("役割", &[first, second], &history, 80_000);
        assert_eq!(stats.consumed.len(), 1);
        assert_eq!(stats.consumed[0].id, "p2");
        assert_eq!(stats.consumed[0].cite_no, 7);
    }
}
