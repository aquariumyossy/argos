//! Assemble one LLM request. A source body is sent once; older ones become stubs.

use std::collections::{HashMap, HashSet};

use crate::db::{LlmMessageRow, LlmSourceRow};

pub const PER_SOURCE_CAP: usize = 40_000;

pub const CITATION_GUIDE: &str = "\n\n出典ブロックがあるときは、その本文だけを根拠にしてください。出典にない事実は推測だと明示し、根拠箇所には [n]（出典番号）を付けてください。";
pub const IMAGE_TRANSCRIPT_NOTE: &str = "（画像の書き起こし。原本の画素ではない）";

pub const REASONING_CARRY_CAP: usize = 4_000;
/// Japanese text is about 1.6 characters per token.
pub const CHARS_PER_TOKEN: f64 = 1.6;
/// In-turn tool results may take this share of the context window.
/// The allowance is reserved up front so history plus a later tool round still fit.
pub const TOOL_RESULT_BUDGET_RATIO: f64 = 0.25;
/// Share of the context window reserved for the model's reply.
pub const REPLY_HEADROOM_RATIO: f64 = 0.10;
/// User turns, counting back from the latest, whose source bodies stay in full.
pub const RECENT_USER_TURNS: usize = 2;
/// Oldest exchanges removed together so the next sends keep the same prefix.
pub const TRIM_EXCHANGE_BATCH: usize = 4;
/// Old tool output that is not itself a cited source.
pub const OLD_TOOL_STUB: &str = "既読 （ツール結果の本文は省略しました）";

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

pub fn reply_headroom_tokens(limit_tokens: usize) -> usize {
    ((limit_tokens as f64) * REPLY_HEADROOM_RATIO).floor() as usize
}

pub fn tool_result_allowance_tokens(limit_tokens: usize) -> usize {
    ((limit_tokens as f64) * TOOL_RESULT_BUDGET_RATIO).floor() as usize
}

/// Tokens of the compact JSON tool schema sent beside the messages.
pub fn tool_schema_tokens(schema: &serde_json::Value) -> usize {
    estimate_tokens(&schema.to_string())
}

/// How many tokens the messages may use, and how many of those are the in-turn tool allowance.
/// Reply and tool reserves shrink when the system prompt and this question need the room.
/// `None` when the question and the schema cannot fit even with both reserves at zero.
pub struct ContextBudget {
    pub messages: usize,
    pub tool_allowance: usize,
}

pub fn context_budget(
    limit_tokens: usize,
    schema_tokens: usize,
    required_tokens: usize,
) -> Option<ContextBudget> {
    if required_tokens.saturating_add(schema_tokens) > limit_tokens {
        return None;
    }
    let spare = limit_tokens - schema_tokens - required_tokens;
    let want_reply = reply_headroom_tokens(limit_tokens);
    let want_tools = tool_result_allowance_tokens(limit_tokens);
    let want = want_reply.saturating_add(want_tools);
    let reserve = want.min(spare);
    let tool_allowance = if want == 0 {
        0
    } else {
        reserve.saturating_mul(want_tools) / want
    };
    let reply = reserve.saturating_sub(tool_allowance);
    Some(ContextBudget {
        messages: limit_tokens - schema_tokens - reply - tool_allowance,
        tool_allowance,
    })
}

/// Fixed line that replaces an older source body. Citation number, file name, and
/// location stay; the text does not depend on the body.
pub fn source_stub_line(row: &LlmSourceRow) -> String {
    let name = stub_file_name(row);
    let loc = stub_location(row);
    match (row.cite_no > 0, loc.is_empty()) {
        (true, false) => format!("既読 [{}] {} ({})", row.cite_no, name, loc),
        (true, true) => format!("既読 [{}] {}", row.cite_no, name),
        (false, false) => format!("既読 {} ({})", name, loc),
        (false, true) => format!("既読 {}", name),
    }
}

pub fn format_source_stubs(sources: &[LlmSourceRow]) -> String {
    if sources.is_empty() {
        return String::new();
    }
    let mut out = String::from("【出典】\n");
    for source in sources {
        out.push_str(&source_stub_line(source));
        out.push_str("\n\n");
    }
    out
}

fn stub_file_name(row: &LlmSourceRow) -> String {
    let path = crate::pathutil::simplify_windows_path(row.path.trim());
    if !path.is_empty() {
        if let Some(name) = path.rsplit(['\\', '/']).find(|part| !part.is_empty()) {
            return name.to_string();
        }
    }
    let title = row.title.trim();
    if title.is_empty() {
        "添付ファイル".into()
    } else if let Some((head, _)) = title.split_once('（') {
        let head = head.trim();
        if head.is_empty() {
            title.to_string()
        } else {
            head.to_string()
        }
    } else {
        title.to_string()
    }
}

fn stub_location(row: &LlmSourceRow) -> String {
    if let Some(page) = crate::llm::transcript::parse_pdf_page_no(&row.paragraph_id) {
        return format!("{page}ページ目");
    }
    if crate::llm::grain::is_file_grain(&row.grain) {
        return String::new();
    }
    row.paragraph_id.trim().to_string()
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
    if let Some(reasoning) = turn.reasoning.as_deref() {
        total += estimate_tokens(reasoning);
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

/// Before a later tool round, keep the request inside `ceiling`.
/// `ceiling` already leaves the reply reserve: the normal headroom, or the
/// smaller one a long question required. Carried reasoning is shortened first,
/// oldest first. Tool results then take what remains.
pub fn fit_followup_request(turns: &mut [ChatTurn], ceiling: usize) {
    let ceiling = ceiling.max(1);
    // Each pass shortens or clears the oldest reasoning, so this ends without a cap.
    while turn_list_tokens(turns) > ceiling {
        let Some(idx) = turns.iter().position(|turn| {
            turn.role == "assistant"
                && turn
                    .reasoning
                    .as_deref()
                    .is_some_and(|text| !text.trim().is_empty())
        }) else {
            break;
        };
        let text = turns[idx].reasoning.take().unwrap_or_default();
        let next = char_len(&text) / 2;
        if next >= 40 {
            turns[idx].reasoning = Some(cap_chars(&text, next));
        }
    }
    if turn_list_tokens(turns) <= ceiling {
        return;
    }
    let non_tool: usize = turns
        .iter()
        .filter(|turn| turn.role != "tool")
        .map(turn_tokens)
        .sum();
    let room = ceiling.saturating_sub(non_tool);
    // A read_note body is the whole note. A shortened prefix looks complete, so a
    // later headingless write can delete the tail. Drop whole notes until the rest
    // can fit, then give the freed room to the other tool results.
    let mut guard = 0;
    while read_notes_exceed_room(turns, room) && guard < 64 {
        guard += 1;
        let Some(idx) = next_read_note_to_omit(turns) else {
            break;
        };
        turns[idx].content = READ_NOTE_OMITTED.into();
    }
    let mut originals: Vec<(usize, String)> = turns
        .iter()
        .enumerate()
        .filter(|(_, turn)| turn.role == "tool" && !is_read_note_turn(turn))
        .map(|(index, turn)| (index, turn.content.clone()))
        .collect();
    let mut guard = 0;
    loop {
        let note_cost: usize = turns
            .iter()
            .filter(|turn| is_read_note_turn(turn))
            .map(turn_tokens)
            .sum();
        fit_other_tool_results(turns, room.saturating_sub(note_cost), &originals);
        if turn_list_tokens(turns) <= ceiling || guard >= 64 {
            break;
        }
        guard += 1;
        if let Some(idx) = next_read_note_to_omit(turns) {
            turns[idx].content = READ_NOTE_OMITTED.into();
            continue;
        }
        let Some(idx) = next_other_result_to_omit(turns) else {
            break;
        };
        turns[idx].content = TOOL_RESULT_OMITTED.into();
        if let Some(slot) = originals.iter_mut().find(|(index, _)| *index == idx) {
            slot.1 = TOOL_RESULT_OMITTED.into();
        }
    }
}

/// Fit non-note tool results into `budget`, leaving each later result its floor.
fn fit_other_tool_results(turns: &mut [ChatTurn], budget: usize, originals: &[(usize, String)]) {
    for (index, content) in originals {
        turns[*index].content = content.clone();
    }
    let indexes: Vec<usize> = originals.iter().map(|(index, _)| *index).collect();
    let mut others = budget;
    for (pos, &idx) in indexes.iter().enumerate() {
        let later_floor: usize = indexes[pos + 1..]
            .iter()
            .map(|&later| other_result_floor(&turns[later]))
            .sum();
        let avail = others.saturating_sub(later_floor);
        let cost = turn_tokens(&turns[idx]);
        if cost <= avail {
            others = others.saturating_sub(cost);
            continue;
        }
        let note_tokens = estimate_tokens(TOOL_RESULT_TRUNCATION_NOTE);
        let body_budget = avail.saturating_sub(4 + note_tokens + 1).max(1);
        let truncated = truncate_to_tokens(&turns[idx].content, body_budget);
        let truncated_cost = turn_tokens(&ChatTurn::text("tool", &truncated));
        let placeholder = turn_tokens(&ChatTurn::text("tool", TOOL_RESULT_OMITTED));
        // A prefix that fits beats the original. The omission text replaces that
        // prefix only when it is shorter and still inside the remaining room.
        if truncated_cost <= avail && truncated_cost < cost {
            turns[idx].content = truncated;
        } else if placeholder < truncated_cost && placeholder <= avail {
            turns[idx].content = TOOL_RESULT_OMITTED.into();
        } else if truncated_cost < cost {
            turns[idx].content = truncated;
        }
        others = others.saturating_sub(turn_tokens(&turns[idx]));
    }
}

fn read_notes_exceed_room(turns: &[ChatTurn], room: usize) -> bool {
    let notes: usize = turns
        .iter()
        .filter(|turn| is_read_note_turn(turn))
        .map(turn_tokens)
        .sum();
    let others: usize = turns
        .iter()
        .filter(|turn| turn.role == "tool" && !is_read_note_turn(turn))
        .map(other_result_floor)
        .sum();
    notes.saturating_add(others) > room
}

/// Largest note that is still longer than the omission text. Equal sizes keep the oldest.
fn next_read_note_to_omit(turns: &[ChatTurn]) -> Option<usize> {
    let placeholder = turn_tokens(&ChatTurn::text("tool", READ_NOTE_OMITTED));
    let mut best: Option<(usize, usize)> = None;
    for (index, turn) in turns.iter().enumerate() {
        if !is_read_note_turn(turn) {
            continue;
        }
        let cost = turn_tokens(turn);
        if cost <= placeholder {
            continue;
        }
        let take = match best {
            None => true,
            Some((best_cost, best_index)) => {
                cost > best_cost || (cost == best_cost && index < best_index)
            }
        };
        if take {
            best = Some((cost, index));
        }
    }
    best.map(|(_, index)| index)
}

fn other_result_floor(turn: &ChatTurn) -> usize {
    let cost = turn_tokens(turn);
    let minimal = turn_tokens(&ChatTurn::text(
        "tool",
        &truncate_to_tokens(&turn.content, 1),
    ));
    let placeholder = turn_tokens(&ChatTurn::text("tool", TOOL_RESULT_OMITTED));
    cost.min(minimal).min(placeholder)
}

fn next_other_result_to_omit(turns: &[ChatTurn]) -> Option<usize> {
    let placeholder = turn_tokens(&ChatTurn::text("tool", TOOL_RESULT_OMITTED));
    let mut best: Option<(usize, usize)> = None;
    for (index, turn) in turns.iter().enumerate() {
        if turn.role != "tool" || is_read_note_turn(turn) {
            continue;
        }
        let cost = turn_tokens(turn);
        if cost <= placeholder {
            continue;
        }
        let take = match best {
            None => true,
            Some((best_cost, best_index)) => {
                cost > best_cost || (cost == best_cost && index < best_index)
            }
        };
        if take {
            best = Some((cost, index));
        }
    }
    best.map(|(_, index)| index)
}

fn is_read_note_turn(turn: &ChatTurn) -> bool {
    turn.role == "tool" && turn.name.as_deref() == Some(crate::llm::tools::TOOL_READ_NOTE)
}

const READ_NOTE_OMITTED: &str = "（文脈の上限に達したため、ノート本文は省略しました。全文置換はせず、heading を指定して読み直してください）";

const TOOL_RESULT_OMITTED: &str = "（文脈の上限に達したため、この結果は省略しました）";

const TOOL_RESULT_TRUNCATION_NOTE: &str = "\n…（長いので省略しました）";

fn truncate_to_tokens(text: &str, max_tokens: usize) -> String {
    let max_chars = tokens_to_chars(max_tokens);
    if char_len(text) <= max_chars {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max_chars).collect();
    out.push_str(TOOL_RESULT_TRUNCATION_NOTE);
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
    let mut used: usize = indexes
        .iter()
        .map(|index| turn_tokens(&turns[*index]))
        .sum();
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
    /// The latest question was left intact because cutting it was the only way to fit.
    pub question_exceeds: bool,
    pub consumed: Vec<ConsumedSource>,
}

/// The single line `format_sources` writes above a source body.
fn citation_header_line(seed: &LlmSourceRow, cite: i64, grouped_image: bool) -> String {
    let mut line = String::new();
    push_cite_prefix(&mut line, cite);
    if grouped_image {
        let title = crate::llm::transcript::file_label(&seed.path, &seed.title);
        line.push_str(&title);
        if !seed.path.trim().is_empty() && seed.path.trim() != title {
            line.push_str(" — ");
            line.push_str(seed.path.trim());
        }
        return line;
    }
    let title = if seed.title.trim().is_empty() {
        seed.path.as_str()
    } else {
        seed.title.as_str()
    };
    line.push_str(title);
    if !seed.path.trim().is_empty() && seed.path.trim() != title.trim() {
        line.push_str(" — ");
        line.push_str(seed.path.trim());
    }
    if seed.is_web() {
        line.push_str("（ウェブ）");
    } else if seed.is_remote() {
        line.push_str("（リモート）");
    }
    let pid = seed.paragraph_id.trim();
    if !pid.is_empty() && !crate::llm::grain::is_file_grain(&seed.grain) {
        line.push_str(&format!(" (paragraph_id: {pid})"));
    }
    line
}

pub(crate) fn source_header_lines(source: &LlmSourceRow) -> [String; 2] {
    [
        citation_header_line(source, source.cite_no, false),
        citation_header_line(source, source.cite_no, true),
    ]
}

fn push_cite_prefix(out: &mut String, n: i64) {
    if n > 0 {
        out.push_str(&format!("[{n}] "));
    }
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
    s.cite_no
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
/// Same retry when tools are already off. Asking for a tool call here makes some models
/// emit `<tool_call>` text, which is stripped and leaves an empty reply.
pub const PHANTOM_CITE_HINT_NO_TOOLS: &str =
    "存在しない出典番号があります。ツールは使えません。出典がないと明示して書き直してください。";

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
    compose_final_turn(sources, stop_tools, false)
}

/// `cite_hint` adds [`FINAL_CITE_HINT`] even when `sources` is empty, so a request
/// that already holds its source bodies still tells the model to cite them.
pub fn compose_final_turn(
    sources: &[LlmSourceRow],
    stop_tools: bool,
    cite_hint: bool,
) -> Option<ChatTurn> {
    let block = format_sources(sources);
    if block.is_empty() && !stop_tools && !cite_hint {
        return None;
    }
    let mut content = block;
    if !content.is_empty() || cite_hint {
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
            out.push_str(&citation_header_line(seed, n, true));
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
        out.push_str(&citation_header_line(seed, n, false));
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
    pool: &[LlmSourceRow],
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

    let over = |rows: &[LlmSourceRow]| char_len(&format_sources_with_pool(rows, pool)) > budget;
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
        let pending_attach =
            pending_ids.contains(&rows[idx].id) && rows[idx].origin == "attach";
        if pending_attach {
            let before = rows[idx].body.clone();
            fit_pending_prefix(&mut rows, idx, pool, budget);
            if rows[idx].body == before {
                break;
            }
        } else if next < 400 {
            rows.remove(idx);
        } else {
            rows[idx].body = cap_chars(&rows[idx].body, next);
        }
    }

    drop_empty_image_pages(&mut rows);
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

/// Longest body prefix that keeps the rendered block inside `budget`.
/// An empty body leaves the citation header. The row itself stays.
fn fit_pending_prefix(rows: &mut [LlmSourceRow], idx: usize, pool: &[LlmSourceRow], budget: usize) {
    let full = rows[idx].body.clone();
    let mut lo = 0usize;
    let mut hi = char_len(&full);
    let mut best = 0usize;
    loop {
        let mid = lo + (hi - lo) / 2;
        rows[idx].body = if mid == 0 {
            String::new()
        } else {
            cap_chars(&full, mid)
        };
        let fits = char_len(&format_sources_with_pool(rows, pool)) <= budget;
        if fits {
            best = mid;
            if mid == hi {
                break;
            }
            lo = mid + 1;
        } else if mid == 0 {
            break;
        } else {
            hi = mid - 1;
        }
        if lo > hi {
            break;
        }
    }
    rows[idx].body = if best == 0 {
        String::new()
    } else {
        cap_chars(&full, best)
    };
}

/// Tokens to take from the tool reserve so a pending attachment still has a short excerpt.
/// Zero when the message budget already holds that excerpt.
pub fn pending_attachment_borrow(
    system: &str,
    question: &str,
    sources: &[LlmSourceRow],
    message_budget: usize,
) -> usize {
    let mut excerpt: Vec<LlmSourceRow> = sources
        .iter()
        .filter(|source| {
            source.is_pending() && source.origin == "attach" && source.is_injectable()
        })
        .cloned()
        .collect();
    if excerpt.is_empty() {
        return 0;
    }
    for row in &mut excerpt {
        row.body = cap_chars(row.body.trim(), 400);
    }
    prompt_tokens_with_sources(system, question, &excerpt).saturating_sub(message_budget)
}

fn prompt_tokens_with_sources(system: &str, question: &str, sources: &[LlmSourceRow]) -> usize {
    let mut sys = system.trim().to_string();
    if !sources.is_empty() && !sys.contains("[n]") {
        sys.push_str(CITATION_GUIDE);
    }
    let sys_tokens = if sys.is_empty() {
        0
    } else {
        turn_tokens(&ChatTurn::text("system", sys))
    };
    let user = wrap_user(&format_sources_with_pool(sources, sources), question);
    sys_tokens + turn_tokens(&ChatTurn::text("user", user))
}

struct BuiltTurn {
    turn: ChatTurn,
    /// Sources rendered on this turn right now. Refit may drop or shorten them.
    sources: Vec<LlmSourceRow>,
    /// Sources attached before fitting, so a later trim can put them back.
    original_sources: Vec<LlmSourceRow>,
    /// User text without the 出典 block, so a later stub can be rendered again.
    question: String,
    message_id: String,
}

/// Build the message list sent to the model.
/// Pending sources (empty `injected_user_message_id`) attach to the latest user turn.
/// Already-consumed sources attach only to the user message that first read them.
///
/// When the request is over budget, old exchanges are dropped before source bodies.
/// Sources outside the recent window are then sent as stubs. A body that is already
/// on a user turn is removed from earlier tool results.
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
    let pool = &injectable;
    let max_tokens = max_tokens.max(1);
    let pending_ids: HashSet<String> = pool
        .iter()
        .filter(|s| s.is_pending())
        .map(|s| s.id.clone())
        .collect();

    let mut sys = system.trim().to_string();
    if !pool.is_empty() && !sys.contains("[n]") {
        sys.push_str(CITATION_GUIDE);
    }

    let hist: Vec<&LlmMessageRow> = history.iter().filter(|m| message_is_replayed(m)).collect();

    let mut kept: Vec<LlmSourceRow> = pool
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
    crate::llm::transcript::assign_group_cites(&mut kept, pool);

    let latest_user_id = hist
        .iter()
        .rev()
        .find(|m| m.role == "user")
        .map(|m| m.id.as_str());

    let mut built: Vec<BuiltTurn> = Vec::new();
    if !sys.is_empty() {
        built.push(BuiltTurn {
            turn: ChatTurn::text("system", sys),
            sources: Vec::new(),
            original_sources: Vec::new(),
            question: String::new(),
            message_id: String::new(),
        });
    }

    for m in &hist {
        let mut turn = history_turn(m);
        let question = if m.role == "user" {
            m.content.clone()
        } else {
            String::new()
        };
        let attached = if m.role == "user" {
            kept.iter()
                .filter(|s| {
                    if pending_ids.contains(&s.id) {
                        latest_user_id == Some(m.id.as_str())
                    } else {
                        s.injected_user_message_id == m.id
                    }
                })
                .cloned()
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        if !attached.is_empty() {
            turn.content = render_user_content(&question, &attached, pool, false);
        }
        built.push(BuiltTurn {
            turn,
            original_sources: attached.clone(),
            sources: attached,
            question,
            message_id: m.id.clone(),
        });
    }

    // Stub first so trim measures the text that will actually be sent. A tool entry
    // whose cite is already on a user turn loses its body. A recent tool read keeps
    // the body when the older owner was stubbed.
    apply_age_stubs(&mut built, pool);
    let full = full_sources_on_recent_turns(&built);
    dedupe_tool_bodies(&mut built, &full);

    let mut truncated = false;
    // Keep the last two user turns while they fit. If a recent source is what
    // overflows, shorten it before dropping the previous question and answer.
    trim_keeping(&mut built, max_tokens, RECENT_USER_TURNS, &mut truncated);
    if tokens_of(&built) > max_tokens && refit_sources(&mut built, pool, max_tokens, &pending_ids) {
        truncated = true;
    }
    if tokens_of(&built) > max_tokens && shrink_recent_sources(&mut built, pool, max_tokens) {
        truncated = true;
    }
    if tokens_of(&built) > max_tokens {
        trim_keeping(&mut built, max_tokens, 1, &mut truncated);
    }
    if tokens_of(&built) > max_tokens && refit_sources(&mut built, pool, max_tokens, &pending_ids) {
        truncated = true;
    }
    if tokens_of(&built) > max_tokens {
        trim_keeping(&mut built, max_tokens, 1, &mut truncated);
    }
    if tokens_of(&built) > max_tokens && force_under_budget(&mut built, max_tokens) {
        truncated = true;
    }
    // The first refit can drop a pending attachment while an older exchange is
    // still protected. Put the original sources back into the room that trim freed.
    if refit_freed_budget(&mut built, pool, max_tokens, &pending_ids) {
        truncated = true;
    }
    // Only the system prompt and the typed question (plus the 【質問】 wrapper) can
    // refuse the send. Attachments are truncated. Earlier turns are dropped only
    // after recent source bodies have been shortened.
    let question_exceeds = must_fit_tokens(&built) > max_tokens;

    let shown_ids: HashSet<String> = built
        .iter()
        .flat_map(|row| row.sources.iter().map(|s| s.id.clone()))
        .collect();
    let dropped_sources = pool.len().saturating_sub(shown_ids.len());
    let mut consumed = Vec::new();
    let mut seen_consumed = HashSet::new();
    for source in built.iter().flat_map(|row| row.sources.iter()) {
        if pending_ids.contains(&source.id) && seen_consumed.insert(source.id.clone()) {
            consumed.push(ConsumedSource {
                id: source.id.clone(),
                cite_no: source.cite_no,
            });
        }
    }
    let shown: Vec<LlmSourceRow> = built
        .iter()
        .flat_map(|row| row.sources.iter().cloned())
        .collect();
    let source_chars = char_len(&format_sources_with_pool(&shown, pool));
    let shortened = shown.iter().any(|s| {
        pool.iter()
            .find(|o| o.id == s.id)
            .is_some_and(|o| char_len(o.body.trim()) > char_len(&s.body))
    });

    let turns: Vec<ChatTurn> = built.into_iter().map(|b| b.turn).collect();
    let stats = AssembleStats {
        total_chars: turns_chars(&turns),
        total_tokens: turns.iter().map(turn_tokens).sum(),
        source_chars,
        dropped_sources,
        truncated: truncated || dropped_sources > 0 || shortened,
        question_exceeds,
        consumed,
    };
    (turns, stats)
}

fn render_user_content(
    question: &str,
    sources: &[LlmSourceRow],
    pool: &[LlmSourceRow],
    stub: bool,
) -> String {
    if sources.is_empty() {
        return question.to_string();
    }
    let block = if stub {
        format_source_stubs(sources)
    } else {
        format_sources_with_pool(sources, pool)
    };
    wrap_user(&block, question)
}

fn tokens_of(built: &[BuiltTurn]) -> usize {
    built.iter().map(|row| turn_tokens(&row.turn)).sum()
}

/// Cap a trailing synthetic 出典 turn so the message list stays within `budget`.
/// Source text is shortened. The cite hint and the stop-tools hint stay whole.
pub fn fit_injected_turn(turns: &mut [ChatTurn], budget: usize) {
    if turns.is_empty() || budget == 0 {
        return;
    }
    let last = turns.len() - 1;
    if !is_synthetic_user_turn(&turns[last].content) {
        return;
    }
    let mut guard = 0;
    while turn_list_tokens(turns) > budget && guard < 24 {
        guard += 1;
        let (body, hints) = split_injected_hints(&turns[last].content);
        let len = char_len(&body);
        let next = (len / 2).max(40);
        if next >= len {
            break;
        }
        turns[last].content = format!("{}{}", cap_chars(&body, next), hints);
    }
}

fn split_injected_hints(content: &str) -> (String, String) {
    let mut cut = content.len();
    for hint in [FINAL_CITE_HINT, STOP_TOOLS_HINT] {
        if let Some(pos) = content.find(hint) {
            cut = cut.min(pos);
        }
    }
    if cut == content.len() {
        return (content.to_string(), String::new());
    }
    (content[..cut].to_string(), content[cut..].to_string())
}

fn turn_list_tokens(turns: &[ChatTurn]) -> usize {
    turns.iter().map(turn_tokens).sum()
}

/// Drop source bodies from tool turns when those cites are also placed on a user turn.
pub fn omit_source_bodies_from_tool_turns(turns: &mut [ChatTurn], sources: &[LlmSourceRow]) {
    let drop = cite_nos_of(sources);
    if drop.is_empty() {
        return;
    }
    for turn in turns.iter_mut() {
        if turn.role != "tool" || !tool_result_lists_sources(turn.name.as_deref()) {
            continue;
        }
        turn.content = drop_cited_bodies(&turn.content, &drop, sources);
    }
}

fn tool_result_lists_sources(name: Option<&str>) -> bool {
    matches!(
        name,
        Some(
            crate::llm::tools::TOOL_SEARCH
                | crate::llm::tools::TOOL_READ
                | crate::llm::tools::TOOL_READ_URL
        )
    )
}

/// Header line → cite, for rows this ledger can actually format.
fn exact_header_cites(ledger: &[LlmSourceRow]) -> HashMap<String, i64> {
    let mut map = HashMap::new();
    for unit in crate::llm::transcript::walk_format_units(ledger) {
        let seed = unit[0];
        let grouped = seed.is_image() && crate::llm::transcript::image_group_key(seed).is_some();
        let cite = if grouped {
            crate::llm::transcript::group_cite_no(&unit)
        } else {
            cite_no_of(seed)
        };
        if cite > 0 {
            map.entry(citation_header_line(seed, cite, grouped))
                .or_insert(cite);
        }
    }
    for row in ledger {
        let cite = cite_no_of(row);
        if cite > 0 {
            map.entry(citation_header_line(row, cite, false))
                .or_insert(cite);
        }
    }
    map
}

/// `(cite, header line start, header line end)` for headers this tool call emitted.
/// A body line that merely starts with `[k] ` is not an entry.
fn emitted_header_spans(content: &str, ledger: &[LlmSourceRow]) -> Vec<(i64, usize, usize)> {
    let headers = exact_header_cites(ledger);
    if headers.is_empty() {
        return Vec::new();
    }
    let mut spans = Vec::new();
    let mut offset = 0;
    for line in content.split_inclusive('\n') {
        let start = offset;
        offset += line.len();
        let text = line.trim_end_matches(['\n', '\r']);
        if let Some(cite) = headers.get(text) {
            spans.push((*cite, start, offset));
        }
    }
    spans
}

/// Remove the body of each real entry whose cite is in `drop`. The header line stays.
fn drop_cited_bodies(content: &str, drop: &HashSet<i64>, ledger: &[LlmSourceRow]) -> String {
    let spans = emitted_header_spans(content, ledger);
    if spans.is_empty() || !spans.iter().any(|(cite, _, _)| drop.contains(cite)) {
        return content.to_string();
    }
    let mut out = String::new();
    let mut pos = 0;
    for (index, (cite, line_start, line_end)) in spans.iter().enumerate() {
        let entry_end = spans
            .get(index + 1)
            .map(|(_, next, _)| *next)
            .unwrap_or(content.len());
        if pos < *line_start {
            out.push_str(&content[pos..*line_start]);
        }
        out.push_str(&content[*line_start..*line_end]);
        if !drop.contains(cite) {
            out.push_str(&content[*line_end..entry_end]);
        }
        pos = entry_end;
    }
    if pos < content.len() {
        out.push_str(&content[pos..]);
    }
    out
}

fn cite_nos_of(sources: &[LlmSourceRow]) -> HashSet<i64> {
    sources
        .iter()
        .map(|source| source.cite_no)
        .filter(|cite| *cite > 0)
        .collect()
}

fn full_sources_on_recent_turns(built: &[BuiltTurn]) -> Vec<LlmSourceRow> {
    let recent = recent_user_ids(built);
    built.iter()
        .filter(|row| row.turn.role == "user" && recent.contains(&row.message_id))
        .flat_map(|row| row.sources.iter().cloned())
        .collect()
}

/// A source body on a user turn must not also sit in a tool result, even when the
/// tool copy was truncated. Matching is the cite number on the header line.
fn dedupe_tool_bodies(built: &mut [BuiltTurn], rendered: &[LlmSourceRow]) {
    let drop = cite_nos_of(rendered);
    if drop.is_empty() {
        return;
    }
    for row in built.iter_mut() {
        if row.turn.role != "tool" {
            continue;
        }
        if !tool_result_lists_sources(row.turn.name.as_deref()) {
            continue;
        }
        row.turn.content = drop_cited_bodies(&row.turn.content, &drop, rendered);
    }
}

fn recent_user_ids(built: &[BuiltTurn]) -> HashSet<String> {
    let ids: Vec<&str> = built
        .iter()
        .filter(|row| row.turn.role == "user" && !row.message_id.is_empty())
        .map(|row| row.message_id.as_str())
        .collect();
    let start = ids.len().saturating_sub(RECENT_USER_TURNS);
    ids[start..].iter().map(|id| (*id).to_string()).collect()
}

fn first_recent_index(built: &[BuiltTurn]) -> usize {
    let users: Vec<usize> = built
        .iter()
        .enumerate()
        .filter(|(_, row)| row.turn.role == "user")
        .map(|(index, _)| index)
        .collect();
    if users.is_empty() {
        return built.len();
    }
    let start = users.len().saturating_sub(RECENT_USER_TURNS);
    users[start]
}

fn apply_age_stubs(built: &mut [BuiltTurn], pool: &[LlmSourceRow]) {
    let recent = recent_user_ids(built);
    let recent_from = first_recent_index(built);
    for (index, row) in built.iter_mut().enumerate() {
        if row.turn.role == "user" && !row.sources.is_empty() && !recent.contains(&row.message_id) {
            row.turn.content = render_user_content(&row.question, &row.sources, pool, true);
        } else if row.turn.role == "tool" && index < recent_from {
            row.turn.content = stub_old_tool(&row.turn.content, pool, row.turn.name.as_deref());
        }
    }
}

/// Replace an old search/read result with one stub line per cite that result
/// actually emitted and that is still in the ledger. Notes and other tools are
/// not split. Cites removed from the ledger are left out.
fn stub_old_tool(content: &str, sources: &[LlmSourceRow], tool_name: Option<&str>) -> String {
    if !tool_result_lists_sources(tool_name) {
        return OLD_TOOL_STUB.to_string();
    }
    let spans = emitted_header_spans(content, sources);
    if spans.is_empty() {
        return OLD_TOOL_STUB.to_string();
    }
    let mut ledger: HashMap<i64, &LlmSourceRow> = HashMap::new();
    for source in sources {
        if source.cite_no > 0 {
            ledger.entry(source.cite_no).or_insert(source);
        }
    }
    let mut lines = Vec::new();
    let mut seen = HashSet::new();
    for (cite, _, _) in spans {
        if !seen.insert(cite) {
            continue;
        }
        if let Some(row) = ledger.get(&cite) {
            lines.push(source_stub_line(row));
        }
    }
    if lines.is_empty() {
        return String::new();
    }
    lines.join("\n")
}

/// After trim has freed room, render the original sources again and fit them.
/// Current-turn attachments are tried first inside [`select_sources`].
fn refit_freed_budget(
    built: &mut [BuiltTurn],
    pool: &[LlmSourceRow],
    max_tokens: usize,
    pending_ids: &HashSet<String>,
) -> bool {
    let recent = recent_user_ids(built);
    let mut changed = false;
    for row in built.iter_mut() {
        if row.turn.role != "user" || !recent.contains(&row.message_id) || row.original_sources.is_empty()
        {
            continue;
        }
        let same = row.sources.len() == row.original_sources.len()
            && row
                .sources
                .iter()
                .zip(row.original_sources.iter())
                .all(|(shown, original)| shown.id == original.id && shown.body == original.body);
        if same {
            continue;
        }
        row.sources = row.original_sources.clone();
        row.turn.content = render_user_content(&row.question, &row.sources, pool, false);
        changed = true;
    }
    if !changed {
        return false;
    }
    let fitted = refit_sources(built, pool, max_tokens, pending_ids);
    if tokens_of(built) > max_tokens {
        shrink_recent_sources(built, pool, max_tokens);
    }
    changed || fitted
}

/// Replace remaining recent source bodies with stubs so the previous question can stay.
/// The latest turn's unsent attachment is shortened instead of stubbed.
fn shrink_recent_sources(built: &mut [BuiltTurn], pool: &[LlmSourceRow], max_tokens: usize) -> bool {
    if tokens_of(built) <= max_tokens {
        return false;
    }
    let recent = recent_user_ids(built);
    let latest = built
        .iter()
        .rev()
        .find(|row| row.turn.role == "user")
        .map(|row| row.message_id.clone());
    let mut changed = false;
    for row in built.iter_mut() {
        if row.turn.role != "user" || !recent.contains(&row.message_id) || row.sources.is_empty() {
            continue;
        }
        if latest.as_deref() == Some(row.message_id.as_str()) && has_unsent_attachment(row) {
            continue;
        }
        let next = render_user_content(&row.question, &row.sources, pool, true);
        if next != row.turn.content {
            row.turn.content = next;
            changed = true;
        }
    }
    changed || shrink_unsent_attachment(built, pool, max_tokens)
}

fn has_unsent_attachment(row: &BuiltTurn) -> bool {
    row.sources
        .iter()
        .any(|source| source.is_pending() && source.origin == "attach")
}

fn shrink_unsent_attachment(
    built: &mut [BuiltTurn],
    pool: &[LlmSourceRow],
    max_tokens: usize,
) -> bool {
    let mut changed = false;
    let mut guard = 0;
    while tokens_of(built) > max_tokens && guard < 24 {
        guard += 1;
        let Some(turn_idx) = built.iter().rposition(|row| row.turn.role == "user") else {
            break;
        };
        if !has_unsent_attachment(&built[turn_idx]) {
            break;
        }
        let Some(src_idx) = built[turn_idx]
            .sources
            .iter()
            .enumerate()
            .filter(|(_, source)| {
                source.is_pending() && source.origin == "attach" && !source.body.is_empty()
            })
            .max_by_key(|(_, source)| char_len(&source.body))
            .map(|(index, _)| index)
        else {
            break;
        };
        let len = char_len(&built[turn_idx].sources[src_idx].body);
        let next = len / 2;
        built[turn_idx].sources[src_idx].body = if next == 0 {
            String::new()
        } else {
            cap_chars(&built[turn_idx].sources[src_idx].body, next)
        };
        drop_empty_image_pages(&mut built[turn_idx].sources);
        let question = built[turn_idx].question.clone();
        let sources = built[turn_idx].sources.clone();
        built[turn_idx].turn.content = render_user_content(&question, &sources, pool, false);
        changed = true;
    }
    changed
}

/// An emptied scan page is not part of the send when another page of the same
/// file still has text. The group header stays on the page that remains.
fn drop_empty_image_pages(rows: &mut Vec<LlmSourceRow>) {
    let mut filled = HashSet::new();
    for row in rows.iter() {
        if row.body.trim().is_empty() {
            continue;
        }
        if let Some(key) = crate::llm::transcript::image_group_key(row) {
            filled.insert(key);
        }
    }
    if filled.is_empty() {
        return;
    }
    rows.retain(|row| {
        if !row.body.trim().is_empty() {
            return true;
        }
        match crate::llm::transcript::image_group_key(row) {
            Some(key) => !filled.contains(&key),
            None => true,
        }
    });
}

fn source_char_room(built: &[BuiltTurn], max_tokens: usize) -> usize {
    let recent = recent_user_ids(built);
    let base: usize = built
        .iter()
        .map(|row| {
            // Source bodies are fitted separately. Count the wrapper that render adds
            // around them, not the question text alone.
            if row.turn.role == "user"
                && !row.sources.is_empty()
                && recent.contains(&row.message_id)
            {
                let shell = wrap_user("【出典】\n", &row.question);
                turn_tokens(&ChatTurn::text("user", shell))
            } else {
                turn_tokens(&row.turn)
            }
        })
        .sum();
    // A few tokens of slack, so omission notes and rounding cannot stub the attachment.
    tokens_to_chars(max_tokens.saturating_sub(base).saturating_sub(16))
}

/// Drop source bodies that still do not fit. Attachments are removed last, matching
/// the previous source priority, and only after old exchanges have already been dropped.
fn refit_sources(
    built: &mut [BuiltTurn],
    pool: &[LlmSourceRow],
    max_tokens: usize,
    pending_ids: &HashSet<String>,
) -> bool {
    let recent = recent_user_ids(built);
    let current = full_sources_on_recent_turns(built);
    if current.is_empty() {
        return false;
    }
    let (kept, dropped, shortened) = select_sources(
        &current,
        pool,
        source_char_room(built, max_tokens),
        pending_ids,
    );
    if dropped == 0 && !shortened {
        return false;
    }
    let kept_by_id: HashMap<String, LlmSourceRow> =
        kept.into_iter().map(|row| (row.id.clone(), row)).collect();
    for row in built.iter_mut() {
        if row.turn.role != "user" || !recent.contains(&row.message_id) || row.sources.is_empty() {
            continue;
        }
        row.sources = row
            .sources
            .iter()
            .filter_map(|source| kept_by_id.get(&source.id).cloned())
            .collect();
        row.turn.content = render_user_content(&row.question, &row.sources, pool, false);
    }
    true
}

fn force_under_budget(built: &mut [BuiltTurn], max_tokens: usize) -> bool {
    let mut changed = false;
    let mut guard = 0;
    while tokens_of(built) > max_tokens && guard < 24 {
        guard += 1;
        let latest = built.iter().rposition(|row| row.turn.role == "user");
        let idx = built
            .iter()
            .enumerate()
            .filter(|(index, row)| {
                row.turn.role != "system"
                    && Some(*index) != latest
                    && char_len(&row.turn.content) > 40
            })
            .max_by_key(|(_, row)| char_len(&row.turn.content))
            .map(|(index, _)| index);
        let Some(idx) = idx else {
            break;
        };
        let len = char_len(&built[idx].turn.content);
        let next = (len / 2).max(40);
        if next >= len {
            break;
        }
        built[idx].turn.content = cap_chars(&built[idx].turn.content, next);
        changed = true;
    }
    changed
}

/// A user turn added after the real question: a 出典 block, the stop-tools line, or a cite hint.
pub fn is_synthetic_user_turn(content: &str) -> bool {
    let text = content.trim();
    text == STOP_TOOLS_HINT
        || text == PHANTOM_CITE_HINT
        || text == PHANTOM_CITE_HINT_NO_TOOLS
        || (text.contains("【出典】") && !text.contains("【質問】"))
}

/// Past turns keep tool calls and tool results, and drop the reasoning channel.
/// A tool message with an empty body is still a reply the server expects.
fn message_is_replayed(message: &LlmMessageRow) -> bool {
    if message.kind == crate::db::LLM_MESSAGE_NOTICE {
        return false;
    }
    match message.role.as_str() {
        "user" | "system" => !message.content.is_empty(),
        "assistant" => !message.content.is_empty() || !message.tool_calls.trim().is_empty(),
        "tool" => !message.content.is_empty() || !message.tool_call_id.trim().is_empty(),
        _ => false,
    }
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

fn trim_keeping(
    built: &mut Vec<BuiltTurn>,
    max_tokens: usize,
    keep_users: usize,
    truncated: &mut bool,
) {
    while tokens_of(built) > max_tokens {
        let mut dropped = 0;
        for _ in 0..TRIM_EXCHANGE_BATCH {
            if !drop_oldest_exchange(built, keep_users) {
                break;
            }
            dropped += 1;
        }
        if dropped == 0 {
            break;
        }
        *truncated = true;
    }
}

fn oldest_protected_user(built: &[BuiltTurn], keep_users: usize) -> Option<usize> {
    let users: Vec<usize> = built
        .iter()
        .enumerate()
        .filter(|(_, row)| row.turn.role == "user")
        .map(|(index, _)| index)
        .collect();
    if users.is_empty() {
        return None;
    }
    let start = users.len().saturating_sub(keep_users.max(1));
    Some(users[start])
}

fn drop_oldest_exchange(built: &mut Vec<BuiltTurn>, keep_users: usize) -> bool {
    let start = usize::from(built.first().is_some_and(|row| row.turn.role == "system"));
    let Some(protected) = oldest_protected_user(built, keep_users) else {
        return false;
    };
    if start >= protected {
        return false;
    }
    let mut remove_until = start + 1;
    while remove_until < protected && built[remove_until].turn.role != "user" {
        remove_until += 1;
    }
    if remove_until > protected {
        remove_until = protected;
    }
    if start >= remove_until {
        return false;
    }
    built.drain(start..remove_until);
    true
}

/// System prompt plus the typed question. Source bodies are not part of this.
fn must_fit_tokens(built: &[BuiltTurn]) -> usize {
    let system = built
        .iter()
        .find(|row| row.turn.role == "system")
        .map(|row| turn_tokens(&row.turn))
        .unwrap_or(0);
    let Some(latest) = built.iter().rev().find(|row| row.turn.role == "user") else {
        return system;
    };
    let text = if !latest.sources.is_empty() {
        format!("【質問】\n{}", latest.question)
    } else {
        latest.question.clone()
    };
    system + turn_tokens(&ChatTurn::text("user", text))
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
            kind: String::new(),
        }
    }

    #[test]
    fn notices_stay_out_of_later_turns() {
        let mut short = msg(
            "n1",
            "assistant",
            "ツール往復が上限の 8 回に達したため、続きの操作はせず、ここまでの資料で答えています。",
        );
        short.kind = crate::db::LLM_MESSAGE_NOTICE.into();
        let mut long = msg(
            "n2",
            "assistant",
            "ツール往復が上限の 8 回に達したため、残りの操作はしていません。",
        );
        long.kind = crate::db::LLM_MESSAGE_NOTICE.into();
        let mut unset = msg("n3", "assistant", "上限未設定の案内");
        unset.kind = crate::db::LLM_MESSAGE_NOTICE.into();
        let history = vec![
            msg("u1", "user", "調べて"),
            msg("a1", "assistant", "途中の答え"),
            short,
            long,
            unset,
            msg("u2", "user", "続き"),
        ];
        let (turns, _) = assemble_turns("役割", &[], &history, 80_000);
        let roles: Vec<&str> = turns.iter().map(|turn| turn.role.as_str()).collect();
        assert_eq!(roles, ["system", "user", "assistant", "user"]);
        assert_eq!(turns[2].content, "途中の答え");
        assert!(turns[3].content.contains("続き"));
        let blob = turns
            .iter()
            .map(|turn| turn.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!blob.contains("ツール往復"));
        assert!(!blob.contains("上限未設定"));
        let (_, stats) = assemble_turns("役割", &[], &history, 80_000);
        let without = vec![
            msg("u1", "user", "調べて"),
            msg("a1", "assistant", "途中の答え"),
            msg("u2", "user", "続き"),
        ];
        let (_, plain) = assemble_turns("役割", &[], &without, 80_000);
        assert_eq!(stats.total_tokens, plain.total_tokens);
    }

    #[test]
    fn notice_wording_without_the_flag_stays_in_context() {
        let history = vec![
            msg("u1", "user", "調べて"),
            msg(
                "a1",
                "assistant",
                "ツール往復が上限の 8 回に達したため、残りの操作はしていません。",
            ),
            msg("u2", "user", "続き"),
        ];
        let (turns, _) = assemble_turns("役割", &[], &history, 80_000);
        assert!(turns.iter().any(|turn| turn.content.contains("ツール往復")));
    }

    #[test]
    fn later_turns_replay_tool_results_without_reasoning() {
        let mut assistant = msg("a1", "assistant", "");
        assistant.reasoning = "これは思考です".into();
        assistant.tool_calls = r#"[{"id":"c1","type":"function","function":{"name":"search_index","arguments":"{\"query\":\"民法\"}"}}]"#.into();
        let mut tool = msg("t1", "tool", "検索結果の本文");
        tool.tool_call_id = "c1".into();
        tool.tool_name = "search_index".into();
        let history = vec![
            msg("u1", "user", "調べて"),
            assistant,
            tool,
            msg("u2", "user", "続き"),
        ];
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
    fn recent_window_gives_way_when_a_tool_result_does_not_fit() {
        let mut tool = msg("t1", "tool", &"あ".repeat(20_000));
        tool.tool_call_id = "c1".into();
        let history = vec![msg("u1", "user", "調べて"), tool, msg("u2", "user", "続き")];
        let (turns, stats) = assemble_turns("役割", &[], &history, 1_000);
        assert!(!stats.question_exceeds);
        assert!(turns.iter().any(|t| t.content == "続き"));
        assert!(turns.iter().all(|t| t.content != "調べて"));
        assert!(!turns.iter().any(|t| t.content.contains(&"あ".repeat(20_000))));
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
        let mut pending = src("b", "attach", 1, "追送本文");
        pending.cite_no = 1;
        let sources = vec![pending];
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
        let mut pending = src("b", "attach", 1, "追加");
        pending.cite_no = 2;
        let sources = vec![consumed("a", "attach", 0, "最初", "u1", 1), pending];
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
        ]
        .into_iter()
        .map(|mut page| {
            page.cite_no = 1;
            page
        })
        .collect::<Vec<_>>();
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

    fn exchange(n: u32, user_body: &str) -> Vec<LlmMessageRow> {
        vec![
            msg(&format!("u{n}"), "user", user_body),
            msg(&format!("a{n}"), "assistant", "答"),
        ]
    }

    #[test]
    fn drops_old_turns_in_a_batch_before_the_latest_source_body() {
        let body = "出".repeat(400);
        let mut attach = src("keep", "attach", 0, &body);
        attach.cite_no = 9;
        attach.path = r"C:\docs\添付.txt".into();
        attach.title = "添付.txt".into();
        attach.paragraph_id.clear();
        let mut history = Vec::new();
        for n in 1..8 {
            history.extend(exchange(n, &"あ".repeat(80)));
        }
        history.push(msg("u8", "user", "今回の質問"));
        let (turns, stats) = assemble_turns("役割", &[attach], &history, 400);
        let users: Vec<&str> = turns
            .iter()
            .filter(|turn| turn.role == "user")
            .map(|turn| turn.content.as_str())
            .collect();
        assert!(stats.truncated, "turns={users:?}");
        assert!(
            users.len() <= 4,
            "a batch of old exchanges should leave: {users:?}"
        );
        assert!(users.iter().any(|text| text.contains("今回の質問")));
        assert!(
            users.iter().any(|text| text.contains(&body)),
            "the latest attachment stays while old turns go: {users:?}"
        );
        assert!(users.len() >= 2, "the last two user turns stay: {users:?}");
        let filler = users
            .iter()
            .filter(|text| text.contains(&"あ".repeat(80)))
            .count();
        assert!(
            filler <= 1,
            "only a protected recent turn may still carry the filler: {users:?}"
        );
    }

    #[test]
    fn trim_batch_keeps_the_same_prefix_on_the_next_send() {
        let mut history = Vec::new();
        for n in 1..8 {
            history.extend(exchange(n, &"あ".repeat(80)));
        }
        history.push(msg("u8", "user", &"あ".repeat(80)));
        let budget = 300;
        let (first, _) = assemble_turns("役割", &[], &history, budget);
        history.push(msg("u9", "user", "追加"));
        let (second, _) = assemble_turns("役割", &[], &history, budget);
        assert!(
            second.len() >= first.len(),
            "the next send must not drop another batch: first {} second {}",
            first.len(),
            second.len()
        );
        for (index, turn) in first.iter().enumerate() {
            assert_eq!(second[index].role, turn.role);
            assert_eq!(second[index].content, turn.content);
        }
        assert!(second.last().unwrap().content.contains("追加"));
    }

    #[test]
    fn search_hit_body_is_sent_once() {
        let body = "民法第1条の本文です。重複してはいけません。";
        let mut hit = consumed("a", "tool", 0, body, "u1", 12);
        hit.path = r"C:\law\民法.txt".into();
        hit.title = "民法.txt".into();
        hit.paragraph_id = r"C:\law\民法.txt#1".into();
        let tool_body = format_sources(&[hit.clone()]);
        let mut assistant = msg("a1", "assistant", "");
        assistant.tool_calls = r#"[{"id":"c1","type":"function","function":{"name":"search_index","arguments":"{}"}}]"#.into();
        let mut tool = msg("t1", "tool", &tool_body);
        tool.tool_call_id = "c1".into();
        tool.tool_name = "search_index".into();
        let history = vec![
            msg("u1", "user", "調べて"),
            assistant,
            tool,
            msg("u2", "user", "続き"),
        ];
        let (turns, _) = assemble_turns("役割", &[hit], &history, 80_000);
        let blob = turns
            .iter()
            .map(|turn| turn.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(blob.matches(body).count(), 1, "{blob}");
        assert!(blob.contains("[12]"), "{blob}");
        let user = turns.iter().find(|turn| turn.role == "user").unwrap();
        assert!(user.content.contains(body), "{}", user.content);
    }

    #[test]
    fn old_sources_become_deterministic_stubs_without_new_cite_numbers() {
        let body = "これは古い本文でスタブに置き換わる";
        let mut hit = consumed("a", "tool", 0, body, "u1", 12);
        hit.path = r"C:\law\民法.txt".into();
        hit.title = "民法.txt".into();
        hit.paragraph_id = r"C:\law\民法.txt#4".into();
        let history = vec![
            msg("u1", "user", "最初"),
            msg("a1", "assistant", "答え"),
            msg("u2", "user", "途中"),
            msg("a2", "assistant", "続きの答え"),
            msg("u3", "user", "最新"),
        ];
        let (turns, stats) = assemble_turns("役割", &[hit.clone()], &history, 80_000);
        assert_eq!(stats.dropped_sources, 0);
        let first = turns.iter().find(|turn| turn.role == "user").unwrap();
        let stub = source_stub_line(&hit);
        assert_eq!(stub, "既読 [12] 民法.txt (C:\\law\\民法.txt#4)");
        assert!(first.content.contains(&stub), "{}", first.content);
        assert!(!first.content.contains(body), "{}", first.content);
        assert!(turns.iter().any(|turn| turn.content.contains("最新")));
        let (again, _) = assemble_turns("役割", &[hit], &history, 80_000);
        assert_eq!(again[1].content, first.content);
    }

    #[test]
    fn old_tool_result_outside_the_recent_window_is_a_fixed_stub() {
        let mut tool = msg("t1", "tool", &"検索メモ".repeat(80));
        tool.tool_call_id = "c1".into();
        let history = vec![
            msg("u1", "user", "最初"),
            tool,
            msg("a1", "assistant", "答え"),
            msg("u2", "user", "途中"),
            msg("a2", "assistant", "続き"),
            msg("u3", "user", "最新"),
        ];
        let (turns, _) = assemble_turns("役割", &[], &history, 80_000);
        let old = turns.iter().find(|turn| turn.role == "tool").unwrap();
        assert_eq!(old.content, OLD_TOOL_STUB);
        assert!(turns.iter().any(|turn| turn.content == "最新"));
    }

    #[test]
    fn pdf_page_stub_uses_the_page_and_keeps_the_cite_number() {
        let mut page = src("p3", "attach", 3, "三枚目の本文");
        page.path = r"C:\docs\契約.pdf".into();
        page.title = "契約.pdf（3ページ目）".into();
        page.paragraph_id = "pdf-page:3".into();
        page.kind = "image".into();
        page.grain = "file".into();
        page.cite_no = 12;
        page.injected_user_message_id = "u1".into();
        assert_eq!(source_stub_line(&page), "既読 [12] 契約.pdf (3ページ目)");
    }

    #[test]
    fn assembly_budget_reserves_reply_tools_and_schema() {
        let limit = 260_000;
        let schema = 2_000;
        let reply = reply_headroom_tokens(limit);
        let tools = tool_result_allowance_tokens(limit);
        let budget = limit - reply - tools - schema;
        assert_eq!(reply, 26_000);
        assert_eq!(tools, 65_000);
        assert_eq!(budget, limit - reply - tools - schema);
        assert!(budget + reply + tools + schema <= limit);
        let small = 1_000usize
            .saturating_sub(reply_headroom_tokens(1_000))
            .saturating_sub(tool_result_allowance_tokens(1_000))
            .saturating_sub(5_000)
            .max(1);
        assert_eq!(small, 1);
        let source = src("a", "attach", 0, "本文");
        let history = vec![msg("u1", "user", "質問")];
        let (turns, stats) = assemble_turns("役割", &[source], &history, budget);
        assert!(!stats.truncated);
        assert!(turns.iter().any(|turn| turn.content.contains("本文")));
    }

    #[test]
    fn large_old_file_is_stubbed_before_trim_so_fitting_turns_stay() {
        let body = "あ".repeat(40_000);
        let mut file = src_file("big", "attach", 0, &body);
        file.cite_no = 12;
        file.injected_user_message_id = "u1".into();
        file.path = r"C:\docs\big.txt".into();
        file.title = "big.txt".into();
        file.paragraph_id.clear();
        let mut history = vec![msg("u1", "user", "最初"), msg("a1", "assistant", "答え")];
        for n in 2..8 {
            history.push(msg(&format!("u{n}"), "user", &format!("質問{n}")));
            history.push(msg(&format!("a{n}"), "assistant", "答"));
        }
        history.push(msg("u8", "user", "質問8"));
        let (turns, stats) = assemble_turns("役割", &[file], &history, 32_000);
        let users: Vec<&str> = turns
            .iter()
            .filter(|turn| turn.role == "user")
            .map(|turn| turn.content.as_str())
            .collect();
        assert!(
            users.iter().any(|text| text.contains("質問2")),
            "stubbed file must not push out turns that fit: {users:?}"
        );
        assert!(users.iter().any(|text| text.contains("質問7")));
        assert!(users.iter().any(|text| *text == "質問8" || text.contains("質問8")));
        assert!(users.iter().any(|text| text.contains("既読 [12]")));
        assert!(!users.iter().any(|text| text.contains(&body)));
        assert!(stats.total_tokens < 32_000, "{}", stats.total_tokens);
        assert!(!stats.question_exceeds);
    }

    #[test]
    fn tiny_budget_keeps_the_recent_window_and_the_stub() {
        let mut hit = consumed("a", "tool", 0, &"本文".repeat(500), "u1", 12);
        hit.path = r"C:\law\民法.txt".into();
        hit.title = "民法.txt".into();
        hit.paragraph_id = r"C:\law\民法.txt#1".into();
        let mut history = Vec::new();
        for n in 1..8 {
            history.push(msg(&format!("u{n}"), "user", &format!("質問{n}")));
            history.push(msg(&format!("a{n}"), "assistant", "答"));
        }
        history.push(msg("u8", "user", "質問8"));
        let (turns, _) = assemble_turns("役割", &[hit], &history, 195);
        let users: Vec<&str> = turns
            .iter()
            .filter(|turn| turn.role == "user")
            .map(|turn| turn.content.as_str())
            .collect();
        assert!(
            users.len() >= 2,
            "the last two user turns stay: {users:?}"
        );
        assert!(users.iter().any(|text| text.contains("質問8")));
        assert!(users.iter().any(|text| text.contains("質問7")));
        let blob = users.join("\n");
        assert!(blob.contains("既読 [12]"), "{blob}");
    }

    #[test]
    fn recent_read_keeps_the_body_when_the_older_owner_is_stubbed() {
        let body = "再読した段落の本文です。消えてはいけません。";
        let mut hit = consumed("a", "tool", 0, body, "u1", 4);
        hit.path = r"C:\law\民法.txt".into();
        hit.title = "民法.txt".into();
        hit.paragraph_id = r"C:\law\民法.txt#9".into();
        let tool_body = format_sources(&[hit.clone()]);
        let mut assistant = msg("a2", "assistant", "");
        assistant.tool_calls = r#"[{"id":"c2","type":"function","function":{"name":"read_unit","arguments":"{}"}}]"#.into();
        let mut tool = msg("t2", "tool", &tool_body);
        tool.tool_call_id = "c2".into();
        tool.tool_name = "read_unit".into();
        let history = vec![
            msg("u1", "user", "最初"),
            msg("a1", "assistant", "答え"),
            msg("u2", "user", "読み直して"),
            assistant,
            tool,
            msg("u3", "user", "最新"),
        ];
        let (turns, _) = assemble_turns("役割", &[hit], &history, 80_000);
        let blob = turns
            .iter()
            .map(|turn| turn.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(blob.matches(body).count(), 1, "{blob}");
        let tool_turn = turns.iter().find(|turn| turn.role == "tool").unwrap();
        assert!(tool_turn.content.contains(body), "{}", tool_turn.content);
        let first = turns.iter().find(|turn| turn.role == "user").unwrap();
        assert!(!first.content.contains(body), "{}", first.content);
        assert!(first.content.contains("既読 [4]"), "{}", first.content);
    }

    #[test]
    fn huge_pending_attachment_is_truncated_and_still_sent() {
        let body = "添".repeat(20_000);
        let mut file = src_file("big", "attach", 0, &body);
        file.cite_no = 5;
        file.path = r"C:\docs\添付.txt".into();
        file.title = "添付.txt".into();
        file.paragraph_id.clear();
        let (turns, stats) = assemble_turns("役割", &[file], &[msg("u1", "user", "見て")], 2_000);
        assert!(!stats.question_exceeds);
        assert!(stats.truncated);
        let user = turns.iter().find(|turn| turn.role == "user").unwrap();
        assert!(user.content.contains("見て"), "{}", user.content);
        assert!(user.content.contains("【質問】"), "{}", user.content);
        assert!(!user.content.contains(&body), "the attachment is cut to fit");
        assert!(
            user.content.contains("[5]") || user.content.contains("添"),
            "a header or a body prefix stays whenever the question fits: {}",
            user.content.chars().take(240).collect::<String>()
        );
        assert!(!user.content.contains("既読 [5]"), "{}", user.content);
    }

    #[test]
    fn near_budget_attachment_is_truncated_not_stubbed() {
        let body = "あ".repeat(1_471);
        let mut file = src_file("f", "attach", 0, &body);
        file.cite_no = 1;
        file.path = r"C:\docs\a.txt".into();
        file.title = "a.txt".into();
        file.paragraph_id.clear();
        let (turns, stats) = assemble_turns("役割", &[file], &[msg("u1", "user", "見て")], 1_000);
        assert!(!stats.question_exceeds);
        let user = turns.iter().find(|turn| turn.role == "user").unwrap();
        assert!(user.content.contains("見て"), "{}", user.content);
        assert!(user.content.contains("[1]"), "{}", user.content);
        assert!(
            !user.content.contains("既読 [1]"),
            "a near-budget attachment stays truncated, not a stub: {}",
            user.content
        );
        assert!(stats.consumed.iter().any(|source| source.id == "f"));
    }

    #[test]
    fn attachment_near_the_16k_window_is_not_stubbed() {
        let split = context_budget(16_384, 2_105, 200).expect("short question fits");
        for n in [12_931usize, 12_934] {
            let body = "文".repeat(n);
            let mut file = src_file("f", "attach", 0, &body);
            file.cite_no = 1;
            file.path = r"C:\docs\scan.pdf".into();
            file.title = "scan.pdf".into();
            file.paragraph_id.clear();
            let (turns, stats) =
                assemble_turns("役割", &[file], &[msg("u1", "user", "見て")], split.messages);
            assert!(!stats.question_exceeds, "{n}");
            let user = turns.iter().find(|turn| turn.role == "user").unwrap();
            assert!(
                !user.content.contains("既読 [1]"),
                "{n} chars became a stub: {}",
                user.content.chars().take(160).collect::<String>()
            );
            assert!(user.content.contains("[1]"), "{n}");
            assert!(user.content.contains("文"), "{n}");
        }
    }

    #[test]
    fn question_that_fills_the_budget_keeps_an_attachment_prefix() {
        let system = crate::db::DEFAULT_LLM_SYSTEM_PROMPT;
        let question = "あ".repeat(12_600);
        let body = "添".repeat(3_000);
        let mut file = src_file("big", "attach", 0, &body);
        file.cite_no = 1;
        file.path = r"C:\docs\memo.txt".into();
        file.title = "memo.txt".into();
        file.paragraph_id.clear();
        let question_turn = format!("【質問】\n{question}");
        let required = turn_tokens(&ChatTurn::text("system", system))
            + turn_tokens(&ChatTurn::text("user", question_turn));
        let split = context_budget(16_384, 2_105, required).expect("the question fits");
        let borrow =
            pending_attachment_borrow(system, &question, std::slice::from_ref(&file), split.messages);
        let budget = split.messages + borrow.min(split.tool_allowance);
        let (turns, stats) = assemble_turns(
            system,
            &[file],
            &[msg("u1", "user", &question)],
            budget,
        );
        assert!(!stats.question_exceeds);
        assert!(stats.consumed.iter().any(|source| source.id == "big"));
        let user = turns.iter().find(|turn| turn.role == "user").unwrap();
        assert!(user.content.contains("[1]"), "{}", user.content.chars().take(240).collect::<String>());
        assert!(!user.content.contains("既読 [1]"), "{}", user.content);
        assert!(
            user.content.contains("添") || user.content.contains("memo.txt"),
            "header or a body prefix stays: {}",
            user.content.chars().take(240).collect::<String>()
        );
    }

    #[test]
    fn previous_protected_turn_is_dropped_instead_of_refusing() {
        let mut history = vec![msg("u1", "user", &"あ".repeat(8_000)), msg("a1", "assistant", "答え")];
        history.push(msg("u2", "user", "今回の質問"));
        let (turns, stats) = assemble_turns("役割", &[], &history, 400);
        assert!(!stats.question_exceeds);
        assert!(turns.iter().any(|turn| turn.content == "今回の質問"));
        assert!(turns.iter().all(|turn| !turn.content.contains(&"あ".repeat(8_000))));
    }

    #[test]
    fn reserves_shrink_so_a_fitting_question_is_not_halved() {
        let limit = 4_096;
        let schema = 2_105;
        let system = "あ".repeat(1_000);
        let question = "短い質問です";
        let required = estimate_tokens(&system) + 4 + estimate_tokens(question) + 4;
        let split = context_budget(limit, schema, required).expect("question fits");
        assert!(split.messages >= required);
        assert!(split.messages + schema <= limit);
        let (turns, stats) =
            assemble_turns(&system, &[], &[msg("u1", "user", question)], split.messages);
        assert!(!stats.question_exceeds);
        let user = turns.iter().find(|turn| turn.role == "user").unwrap();
        assert_eq!(user.content, question);
    }

    #[test]
    fn question_that_cannot_fit_is_not_silently_halved() {
        let question = "あ".repeat(20_000);
        let (turns, stats) = assemble_turns("役割", &[], &[msg("u1", "user", &question)], 500);
        let user = turns.iter().find(|turn| turn.role == "user").unwrap();
        assert!(stats.question_exceeds);
        assert_eq!(user.content, question);
    }

    #[test]
    fn paste_that_fits_beside_the_schema_keeps_its_full_text() {
        let limit = 131_072;
        let schema = 2_105;
        let question = "あ".repeat(200_000);
        let system = "役割";
        let required = estimate_tokens(system) + 4 + estimate_tokens(&question) + 4;
        assert!(required + schema <= limit);
        let split = context_budget(limit, schema, required).expect("fits");
        let (turns, stats) =
            assemble_turns(system, &[], &[msg("u1", "user", &question)], split.messages);
        assert!(!stats.question_exceeds);
        let user = turns.iter().find(|turn| turn.role == "user").unwrap();
        assert_eq!(user.content, question);
        assert!(split.tool_allowance < tool_result_allowance_tokens(limit));
    }

    #[test]
    fn old_tool_result_lists_only_cites_still_in_the_ledger() {
        let keep = "甲の本文です。台帳に残る。";
        let drop_b = "乙の本文です。台帳から消えた。";
        let drop_c = "丙の本文です。台帳から消えた。";
        let drop_d = "丁の本文です。台帳から消えた。";
        let mut kept = src("a", "tool", 1, keep);
        kept.cite_no = 1;
        kept.path = r"C:\law\甲.txt".into();
        kept.title = "甲.txt".into();
        kept.paragraph_id = r"C:\law\甲.txt#1".into();
        kept.injected_user_message_id = "u1".into();
        let gone = |id: &str, cite: i64, body: &str| {
            let mut row = src(id, "tool", cite, body);
            row.cite_no = cite;
            row.path = format!(r"C:\law\{id}.txt");
            row.title = format!("{id}.txt");
            row.paragraph_id = format!(r"C:\law\{id}.txt#1");
            row
        };
        let tool_body = format_sources(&[
            kept.clone(),
            gone("b", 2, drop_b),
            gone("c", 3, drop_c),
            gone("d", 4, drop_d),
        ]);
        let mut tool = msg("t1", "tool", &tool_body);
        tool.tool_call_id = "c1".into();
        tool.tool_name = "search_index".into();
        let history = vec![
            msg("u1", "user", "最初"),
            tool,
            msg("a1", "assistant", "答え"),
            msg("u2", "user", "途中"),
            msg("a2", "assistant", "続き"),
            msg("u3", "user", "最新"),
        ];
        let (turns, _) = assemble_turns("役割", &[kept], &history, 80_000);
        let old = turns.iter().find(|turn| turn.role == "tool").unwrap();
        assert!(old.content.contains("既読 [1]"), "{}", old.content);
        assert!(!old.content.contains("[2]"), "{}", old.content);
        assert!(!old.content.contains(keep), "{}", old.content);
        for body in [drop_b, drop_c, drop_d] {
            assert!(!old.content.contains(body), "{}", old.content);
        }
        let blob = turns
            .iter()
            .map(|turn| turn.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        for body in [drop_b, drop_c, drop_d] {
            assert!(!blob.contains(body), "{blob}");
        }
    }

    #[test]
    fn truncated_reread_body_is_not_sent_again_from_the_tool_result() {
        let marker = "再読本文ユニーク";
        let body = format!("{marker}{}", "条".repeat(60_000));
        let mut hit = consumed("a", "tool", 0, &body, "u2", 3);
        hit.path = r"C:\law\民法.txt".into();
        hit.title = "民法.txt".into();
        hit.paragraph_id = r"C:\law\民法.txt#3".into();
        let stored = fit_tool_text(format_sources(std::slice::from_ref(&hit)), 8_000, 0);
        assert!(estimate_tokens(&stored) > 1_000, "{}", estimate_tokens(&stored));
        assert!(stored.contains(marker));
        assert!(!stored.contains(&body));
        let mut assistant = msg("a2", "assistant", "");
        assistant.tool_calls = r#"[{"id":"c2","type":"function","function":{"name":"read_unit","arguments":"{}"}}]"#.into();
        let mut tool = msg("t2", "tool", &stored);
        tool.tool_call_id = "c2".into();
        tool.tool_name = "read_unit".into();
        let history = vec![
            msg("u1", "user", "最初"),
            msg("a1", "assistant", "答え"),
            msg("u2", "user", "読み直して"),
            assistant,
            tool,
            msg("u3", "user", "根拠は"),
        ];
        let (turns, _) = assemble_turns("役割", &[hit], &history, 80_000);
        let blob = turns
            .iter()
            .map(|turn| turn.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(blob.matches(marker).count(), 1, "one copy of the re-read");
        let tool_turn = turns.iter().find(|turn| turn.role == "tool").unwrap();
        assert!(
            !tool_turn.content.contains(marker),
            "the truncated tool copy must not stay: {}",
            tool_turn.content.chars().take(200).collect::<String>()
        );
        assert!(turns.iter().any(|turn| {
            turn.role == "user" && turn.content.contains("読み直して") && turn.content.contains(marker)
        }));
    }

    #[test]
    fn followup_after_a_large_attachment_keeps_the_previous_exchange() {
        let body = format!("第5条 {}", "条".repeat(20_000));
        let mut file = src_file("big", "attach", 0, &body);
        file.cite_no = 5;
        file.path = r"C:\docs\契約.txt".into();
        file.title = "契約.txt".into();
        file.paragraph_id.clear();
        file.injected_user_message_id = "u1".into();
        let history = vec![
            msg("u1", "user", "この条文を見て"),
            msg("a1", "assistant", "第5条は契約の解釈です"),
            msg("u2", "user", "第5条をもう少し詳しく"),
        ];
        for limit in [16_384usize, 32_768, 131_072] {
            let split = context_budget(limit, 2_105, 200).expect("question fits");
            let (turns, stats) = assemble_turns("役割", &[file.clone()], &history, split.messages);
            assert!(!stats.question_exceeds, "{limit}");
            assert!(
                turns.iter().any(|turn| turn.content.contains("この条文を見て")),
                "{limit}"
            );
            assert!(
                turns
                    .iter()
                    .any(|turn| turn.content.contains("第5条は契約の解釈です")),
                "{limit}"
            );
            assert!(
                turns
                    .iter()
                    .any(|turn| turn.content.contains("第5条をもう少し詳しく")),
                "{limit}"
            );
            if limit == 16_384 {
                assert!(
                    turns.iter().all(|turn| !turn.content.contains(&body)),
                    "the attachment is shortened so the previous turn can stay"
                );
            }
        }
    }

    #[test]
    fn body_lines_that_look_like_cites_are_not_headers() {
        let tail = "後文ユニークは残してはいけない";
        let mut kept = src("a", "tool", 1, &format!("前文\n[4] 注記\n{tail}"));
        kept.cite_no = 1;
        kept.path = r"C:\law\甲.txt".into();
        kept.title = "甲.txt".into();
        kept.paragraph_id = r"C:\law\甲.txt#1".into();
        kept.injected_user_message_id = "u2".into();
        let mut other = src("d", "tool", 4, "別の出典です");
        other.cite_no = 4;
        other.path = r"C:\law\丁.txt".into();
        other.title = "丁.txt".into();
        other.paragraph_id = r"C:\law\丁.txt#1".into();
        other.injected_user_message_id = "u1".into();
        let tool_body = format_sources(std::slice::from_ref(&kept));
        assert!(tool_body.contains("[4] 注記"), "{tool_body}");
        let mut tool = msg("t2", "tool", &tool_body);
        tool.tool_call_id = "c2".into();
        tool.tool_name = "search_index".into();
        let history = vec![
            msg("u1", "user", "最初"),
            msg("a1", "assistant", "答え"),
            msg("u2", "user", "読んで"),
            tool,
            msg("u3", "user", "続き"),
        ];
        let (turns, _) = assemble_turns("役割", &[kept, other.clone()], &history, 80_000);
        let tool_turn = turns.iter().find(|turn| turn.role == "tool").unwrap();
        assert!(
            !tool_turn.content.contains(tail),
            "the fake header must not keep the rest of the entry: {}",
            tool_turn.content
        );
        assert!(!tool_turn.content.contains("既読 [4]"), "{}", tool_turn.content);
        let old = {
            let mut aged = msg("t0", "tool", &tool_body);
            aged.tool_call_id = "c0".into();
            aged.tool_name = "search_index".into();
            let history = vec![
                msg("u1", "user", "最初"),
                aged,
                msg("a1", "assistant", "答え"),
                msg("u2", "user", "途中"),
                msg("a2", "assistant", "続き"),
                msg("u3", "user", "最新"),
            ];
            let (turns, _) = assemble_turns("役割", &[other.clone()], &history, 80_000);
            turns.into_iter().find(|turn| turn.role == "tool").unwrap()
        };
        assert!(!old.content.contains("既読 [4]"), "{}", old.content);
        assert!(!old.content.contains(tail), "{}", old.content);
    }

    #[test]
    fn read_note_is_not_split_on_citation_lines() {
        let mut hit = src("a", "tool", 1, "検索本文");
        hit.cite_no = 1;
        hit.path = r"C:\law\甲.txt".into();
        hit.title = "甲.txt".into();
        hit.paragraph_id = r"C:\law\甲.txt#1".into();
        hit.injected_user_message_id = "u2".into();
        let header = format_sources(std::slice::from_ref(&hit));
        let header_line = header
            .lines()
            .find(|line| line.starts_with("[1] "))
            .unwrap()
            .to_string();
        let note = format!("ノート\n{header_line}\nノート本文が残る");
        let mut tool = msg("t2", "tool", &note);
        tool.tool_call_id = "c2".into();
        tool.tool_name = "read_note".into();
        let history = vec![
            msg("u1", "user", "最初"),
            msg("a1", "assistant", "答え"),
            msg("u2", "user", "読んで"),
            tool,
            msg("u3", "user", "続き"),
        ];
        let (turns, _) = assemble_turns("役割", &[hit.clone()], &history, 80_000);
        let tool_turn = turns.iter().find(|turn| turn.role == "tool").unwrap();
        assert!(
            tool_turn.content.contains("ノート本文が残る"),
            "{}",
            tool_turn.content
        );
    }

    #[test]
    fn pending_attachment_comes_back_after_the_previous_exchange_is_dropped() {
        let split = context_budget(16_384, 2_105, 200).expect("question fits");
        let question = "問".repeat(2_000);
        let mut tool = msg("t1", "tool", &"読".repeat(5_990));
        tool.tool_call_id = "c1".into();
        tool.tool_name = "read_unit".into();
        let answer = "答".repeat(6_000);
        let body = "添".repeat(20_000);
        let mut file = src_file("big", "attach", 0, &body);
        file.cite_no = 3;
        file.path = r"C:\docs\添付.txt".into();
        file.title = "添付.txt".into();
        file.paragraph_id.clear();
        let history = vec![
            msg("u1", "user", &question),
            tool,
            msg("a1", "assistant", &answer),
            msg("u2", "user", "短い質問"),
        ];
        let (turns, stats) = assemble_turns("役割", &[file], &history, split.messages);
        assert!(!stats.question_exceeds);
        assert!(
            stats.consumed.iter().any(|source| source.id == "big"),
            "consumed={:?}",
            stats.consumed
        );
        assert!(stats.truncated);
        let user = turns.iter().rev().find(|turn| turn.role == "user").unwrap();
        assert!(user.content.contains("短い質問"), "{}", user.content);
        assert!(user.content.contains("添"), "{}", user.content);
        assert!(!user.content.contains(&body), "the attachment is truncated");
        assert!(stats.total_tokens > 1_000, "{}", stats.total_tokens);
    }

    #[test]
    fn scanned_pdf_omits_empty_pages_without_counting_them() {
        let split = context_budget(16_384, 2_105, 80).expect("short question fits");
        let mut pages = Vec::new();
        for n in 1..=10 {
            let mut page = img_page(&format!("p{n}"), n, &"頁".repeat(1_500));
            page.cite_no = 1;
            pages.push(page);
        }
        let (turns, stats) = assemble_turns(
            "役割",
            &pages,
            &[msg("u1", "user", "要約して")],
            split.messages,
        );
        assert!(!stats.question_exceeds);
        assert!(
            stats.consumed.len() < pages.len(),
            "unsent pages stay pending, consumed={}",
            stats.consumed.len()
        );
        assert!(!stats.consumed.is_empty());
        let user = turns.iter().find(|turn| turn.role == "user").unwrap();
        assert!(user.content.contains("頁"), "{}", user.content);
        assert!(user.content.contains("契約.pdf"), "{}", user.content);
        let kept: Vec<u32> = stats
            .consumed
            .iter()
            .filter_map(|source| source.id.strip_prefix('p')?.parse().ok())
            .collect();
        let last = *kept.iter().max().unwrap();
        assert!(
            user.content
                .contains(&format!("（{}ページ以降は文字数のため省略）", last + 1)),
            "kept {kept:?}: {}",
            user.content
        );
        for page in 1..=10 {
            let heading = format!("## {page}ページ目");
            if kept.contains(&page) {
                assert!(user.content.contains(&heading), "{heading} missing");
            } else {
                assert!(!user.content.contains(&heading), "{heading} was emptied but kept");
            }
        }
    }

    #[test]
    fn question_text_that_says_question_is_not_refused_when_it_fits() {
        let question = format!("前文【質問】{}", "あ".repeat(1_500));
        let budget = turn_tokens(&ChatTurn::text("system", "役割"))
            + turn_tokens(&ChatTurn::text("user", &question));
        let wrapped = format!("【質問】\n{question}");
        assert!(
            turn_tokens(&ChatTurn::text("system", "役割"))
                + turn_tokens(&ChatTurn::text("user", wrapped))
                > budget
        );
        let (turns, stats) = assemble_turns("役割", &[], &[msg("u1", "user", &question)], budget);
        assert!(!stats.question_exceeds);
        let user = turns.iter().find(|turn| turn.role == "user").unwrap();
        assert_eq!(user.content, question);
    }

    #[test]
    fn followup_shortens_carried_reasoning_before_the_tool_result() {
        let limit = 16_384;
        let schema = 2_105;
        let reply = reply_headroom_tokens(limit);
        let ceiling = limit - schema - reply;
        let tool_body = "読".repeat(6_000);
        let reasoning = "考".repeat(4_000);
        let user = "問".repeat(tokens_to_chars(7_000));
        let mut assistant = ChatTurn::text("assistant", "検索します");
        assistant.reasoning = Some(reasoning);
        assistant.tool_calls = Some(serde_json::json!([{
            "id": "c1",
            "type": "function",
            "function": { "name": "read_unit", "arguments": "{}" }
        }]));
        let mut tool = ChatTurn::text("tool", tool_body.clone());
        tool.tool_call_id = Some("c1".into());
        tool.name = Some("read_unit".into());
        let mut turns = vec![
            ChatTurn::text("system", "役割"),
            ChatTurn::text("user", user),
            assistant,
            tool,
        ];
        let before = turn_list_tokens(&turns);
        assert!(
            before + schema + reply > limit,
            "fixture must overshoot: {before} ceiling {ceiling}"
        );
        fit_followup_request(&mut turns, ceiling);
        let after = turn_list_tokens(&turns);
        assert!(after + schema + reply <= limit, "{after}");
        let carried = turns[2].reasoning.as_deref().unwrap_or("");
        assert!(char_len(carried) < 4_000, "{}", char_len(carried));
        assert_eq!(turns[3].content, tool_body);
    }

    #[test]
    fn read_note_followup_is_replaced_whole_not_prefix_cut() {
        let limit = 16_384;
        let schema = 2_105;
        let reply = reply_headroom_tokens(limit);
        let ceiling = limit - schema - reply;
        let search_body = "索".repeat(8_000);
        let note_body = format!("見出し\n{}", "記".repeat(30_000));
        let mut search = ChatTurn::text("tool", search_body.clone());
        search.name = Some("search_index".into());
        let mut note = ChatTurn::text("tool", note_body);
        note.name = Some(crate::llm::tools::TOOL_READ_NOTE.into());
        let mut turns = vec![
            ChatTurn::text("system", "役割"),
            ChatTurn::text("user", "読んでから書いて"),
            search,
            note,
        ];
        assert!(turn_list_tokens(&turns) > ceiling);
        fit_followup_request(&mut turns, ceiling);
        let after = turn_list_tokens(&turns);
        assert!(after + schema + reply <= limit, "{after}");
        assert_eq!(
            turns[2].content, search_body,
            "a search that fits beside the note placeholder keeps its text"
        );
        assert_eq!(turns[3].content, READ_NOTE_OMITTED);
        assert!(!turns[3].content.contains("見出し"));
        assert!(!turns[3].content.contains("…（長いので省略しました）"));
    }

    #[test]
    fn only_the_largest_read_note_is_replaced() {
        let limit = 16_384;
        let schema = 2_105;
        let reply = reply_headroom_tokens(limit);
        let ceiling = limit - schema - reply;
        let small_a = format!("甲見出し\n{}", "甲".repeat(2_000));
        let small_b = format!("乙見出し\n{}", "乙".repeat(2_000));
        let large = format!("丙見出し\n{}", "丙".repeat(20_000));
        let mut notes = [
            ChatTurn::text("tool", small_a.clone()),
            ChatTurn::text("tool", small_b.clone()),
            ChatTurn::text("tool", large),
        ];
        for note in &mut notes {
            note.name = Some(crate::llm::tools::TOOL_READ_NOTE.into());
        }
        let mut turns = vec![
            ChatTurn::text("system", "役割"),
            ChatTurn::text("user", "三つ読んで"),
            notes[0].clone(),
            notes[1].clone(),
            notes[2].clone(),
        ];
        assert!(turn_list_tokens(&turns) > ceiling);
        fit_followup_request(&mut turns, ceiling);
        let after = turn_list_tokens(&turns);
        assert!(after + schema + reply <= limit, "{after}");
        assert_eq!(turns[2].content, small_a);
        assert_eq!(turns[3].content, small_b);
        assert_eq!(turns[4].content, READ_NOTE_OMITTED);
        assert!(!turns[4].content.contains("丙見出し"));
    }

    #[test]
    fn oversized_read_note_keeps_the_search_result() {
        let limit = 16_384;
        let schema = 2_105;
        let reply = reply_headroom_tokens(limit);
        let ceiling = limit - schema - reply;
        let search_body = "索".repeat(2_004);
        let note_body = "記".repeat(30_000);
        let mut search = ChatTurn::text("tool", search_body.clone());
        search.name = Some("search_index".into());
        let mut note = ChatTurn::text("tool", note_body);
        note.name = Some(crate::llm::tools::TOOL_READ_NOTE.into());
        let mut turns = vec![
            ChatTurn::text("system", "役割"),
            ChatTurn::text("user", "検索してから読んで"),
            search,
            note,
        ];
        let note_alone = turn_tokens(&turns[0]) + turn_tokens(&turns[1]) + turn_tokens(&turns[3]);
        assert!(note_alone > ceiling, "the note alone must exceed the room");
        fit_followup_request(&mut turns, ceiling);
        let after = turn_list_tokens(&turns);
        assert!(after + schema + reply <= limit, "{after}");
        assert_eq!(turns[2].content, search_body);
        assert_eq!(turns[3].content, READ_NOTE_OMITTED);
        assert!(!turns[3].content.contains('記'));
    }

    #[test]
    fn slightly_over_tool_result_keeps_a_body_prefix() {
        let limit = 260_000;
        let schema = 2_105;
        let reply = reply_headroom_tokens(limit);
        let ceiling = limit - schema - reply;
        let tool_body = "読".repeat(tokens_to_chars(25_001));
        let tool_cost = turn_tokens(&ChatTurn::text("tool", &tool_body));
        let system = ChatTurn::text("system", "役割");
        let user_tokens = ceiling + 77 - tool_cost - turn_tokens(&system);
        let user = ChatTurn::text("user", "問".repeat(tokens_to_chars(user_tokens)));
        let mut tool = ChatTurn::text("tool", tool_body);
        tool.name = Some("read_unit".into());
        let mut turns = vec![system, user, tool];
        let before = turn_list_tokens(&turns);
        let over = before as isize - ceiling as isize;
        assert!(
            (1..200).contains(&over),
            "fixture should be slightly over the room: before {before} ceiling {ceiling} over {over}"
        );
        fit_followup_request(&mut turns, ceiling);
        let after = turn_list_tokens(&turns);
        assert!(after + schema + reply <= limit, "{after}");
        let kept = &turns[2].content;
        assert!(
            kept.starts_with(&"読".repeat(1_000)),
            "a slightly long result keeps its prefix: {}",
            kept.chars().take(40).collect::<String>()
        );
        assert!(
            !kept.starts_with("（文脈の上限に達したため"),
            "{kept}"
        );
    }

    #[test]
    fn fitting_note_and_following_small_result_stay_intact() {
        let limit = 260_000;
        let schema = 2_105;
        let reply = reply_headroom_tokens(limit);
        let ceiling = limit - schema - reply;
        let search_body = "索".repeat(tokens_to_chars(20_000));
        let note_body = "記".repeat(tokens_to_chars(30_000));
        let saved = "保存しました";
        let mut search = ChatTurn::text("tool", search_body.clone());
        search.name = Some("search_index".into());
        let mut note = ChatTurn::text("tool", note_body.clone());
        note.name = Some(crate::llm::tools::TOOL_READ_NOTE.into());
        let mut write = ChatTurn::text("tool", saved);
        write.name = Some(crate::llm::tools::TOOL_WRITE_NOTE.into());
        let search_cost = turn_tokens(&search);
        let note_cost = turn_tokens(&note);
        let system = ChatTurn::text("system", "役割");
        // The search fills the non-note budget exactly, so a later confirmation
        // has no leftover unless its floor was reserved up front.
        let room = note_cost + search_cost;
        let non_tool = ceiling - room;
        let user = ChatTurn::text(
            "user",
            "問".repeat(tokens_to_chars(non_tool - turn_tokens(&system) - 4)),
        );
        assert_eq!(turn_tokens(&system) + turn_tokens(&user), non_tool);
        let mut turns = vec![system, user, search, note, write];
        assert!(turn_list_tokens(&turns) > ceiling);
        fit_followup_request(&mut turns, ceiling);
        let after = turn_list_tokens(&turns);
        assert!(after + schema + reply <= limit, "{after}");
        assert_eq!(turns[3].content, note_body);
        assert_eq!(turns[4].content, saved);
        assert!(
            turns[2].content.starts_with(&"索".repeat(1_000)),
            "{}",
            turns[2].content.chars().take(40).collect::<String>()
        );
        assert_ne!(turns[2].content, TOOL_RESULT_OMITTED);
    }

    #[test]
    fn slightly_over_result_keeps_prefix_beside_an_empty_search() {
        let limit = 260_000;
        let schema = 2_105;
        let reply = reply_headroom_tokens(limit);
        let ceiling = limit - schema - reply;
        let tool_body = "読".repeat(tokens_to_chars(25_001));
        let tool_cost = turn_tokens(&ChatTurn::text("tool", &tool_body));
        let system = ChatTurn::text("system", "役割");
        let user_tokens = ceiling + 77 - tool_cost - turn_tokens(&system);
        let user = ChatTurn::text("user", "問".repeat(tokens_to_chars(user_tokens)));
        let mut tool = ChatTurn::text("tool", tool_body);
        tool.name = Some("read_unit".into());
        let mut empty = ChatTurn::text("tool", "");
        empty.name = Some("search_index".into());
        let mut turns = vec![system, user, tool, empty];
        let before = turn_list_tokens(&turns);
        let over = before as isize - ceiling as isize;
        assert!(
            (1..200).contains(&over),
            "fixture should be slightly over the room: before {before} ceiling {ceiling} over {over}"
        );
        fit_followup_request(&mut turns, ceiling);
        let after = turn_list_tokens(&turns);
        assert!(after + schema + reply <= limit, "{after}");
        let kept = &turns[2].content;
        assert!(
            kept.starts_with(&"読".repeat(1_000)),
            "a slightly long result keeps its prefix: {}",
            kept.chars().take(40).collect::<String>()
        );
        assert!(!kept.starts_with("（文脈の上限に達したため"), "{kept}");
        assert_eq!(turns[3].content, "");
    }

    #[test]
    fn two_large_searches_keep_the_fitting_note() {
        let limit = 260_000;
        let schema = 2_105;
        let reply = reply_headroom_tokens(limit);
        let ceiling = limit - schema - reply;
        let room = 60_000;
        let search_a_body = "索".repeat(tokens_to_chars(39_000));
        let search_b_body = "検".repeat(tokens_to_chars(26_000));
        let note_body = "記".repeat(tokens_to_chars(7_503));
        let unit_body = "件";
        let mut search_a = ChatTurn::text("tool", search_a_body.clone());
        search_a.name = Some("search_index".into());
        let mut search_b = ChatTurn::text("tool", search_b_body.clone());
        search_b.name = Some("search_index".into());
        let mut unit = ChatTurn::text("tool", unit_body);
        unit.name = Some("read_unit".into());
        let mut note = ChatTurn::text("tool", note_body.clone());
        note.name = Some(crate::llm::tools::TOOL_READ_NOTE.into());
        assert_eq!(turn_tokens(&note), 7_507);
        let system = ChatTurn::text("system", "役割");
        let non_tool = ceiling - room;
        let user = ChatTurn::text(
            "user",
            "問".repeat(tokens_to_chars(non_tool - turn_tokens(&system) - 4)),
        );
        assert_eq!(turn_tokens(&system) + turn_tokens(&user), non_tool);
        let mut turns = vec![system, user, search_a, search_b, unit, note];
        assert!(turn_list_tokens(&turns) > ceiling);
        fit_followup_request(&mut turns, ceiling);
        let after = turn_list_tokens(&turns);
        assert!(after + schema + reply <= limit, "{after}");
        assert_eq!(turns[5].content, note_body);
        assert_eq!(turns[2].content, search_a_body);
        assert_eq!(turns[4].content, unit_body);
        assert!(
            turns[3].content.starts_with(&"検".repeat(1_000)),
            "{}",
            turns[3].content.chars().take(40).collect::<String>()
        );
        assert_ne!(turns[3].content, TOOL_RESULT_OMITTED);
    }

    #[test]
    fn carried_reasoning_is_cleared_before_a_fitting_read_note() {
        let limit = 260_000;
        let schema = 2_105;
        let reply = reply_headroom_tokens(limit);
        let ceiling = limit - schema - reply;
        assert_eq!(ceiling, 231_895);
        let note_body = "記".repeat(tokens_to_chars(29_996));
        let mut note = ChatTurn::text("tool", note_body.clone());
        note.name = Some(crate::llm::tools::TOOL_READ_NOTE.into());
        assert_eq!(turn_tokens(&note), 30_000);
        let mut turns = vec![ChatTurn::text("system", "役割")];
        for i in 0..7 {
            let mut assistant = ChatTurn::text("assistant", "検索");
            assistant.reasoning = Some("考".repeat(REASONING_CARRY_CAP));
            turns.push(assistant);
            if i < 6 {
                let mut small = ChatTurn::text("tool", "了");
                small.name = Some("search_index".into());
                turns.push(small);
            }
        }
        turns.push(note);
        let reasoning_at: Vec<usize> = turns
            .iter()
            .map(|turn| {
                turn.reasoning
                    .as_deref()
                    .map(estimate_tokens)
                    .unwrap_or(0)
            })
            .filter(|tokens| *tokens > 0)
            .collect();
        assert_eq!(reasoning_at, vec![2_500; 7]);
        let fixed = turn_list_tokens(&turns);
        let user_tokens = ceiling + 14_000 - fixed;
        let user = ChatTurn::text(
            "user",
            "問".repeat(tokens_to_chars(user_tokens - 4)),
        );
        assert_eq!(turn_tokens(&user), user_tokens);
        turns.insert(1, user);
        let before = turn_list_tokens(&turns);
        assert_eq!(before, ceiling + 14_000);
        assert!(before - 17_500 <= ceiling);
        fit_followup_request(&mut turns, ceiling);
        let after = turn_list_tokens(&turns);
        assert!(after + schema + reply <= limit, "{after}");
        let kept = turns
            .iter()
            .find(|turn| turn.name.as_deref() == Some(crate::llm::tools::TOOL_READ_NOTE))
            .unwrap();
        assert_eq!(kept.content, note_body);
        let reasoning_left: usize = turns
            .iter()
            .map(|turn| {
                turn.reasoning
                    .as_deref()
                    .map(estimate_tokens)
                    .unwrap_or(0)
            })
            .sum();
        assert!(
            reasoning_left < 5_157,
            "reasoning still holding the note out: {reasoning_left}"
        );
    }

    #[test]
    fn result_shorter_than_the_placeholder_is_still_prefix_cut() {
        let limit = 16_384;
        let schema = 2_105;
        let reply = reply_headroom_tokens(limit);
        let ceiling = limit - schema - reply;
        assert_eq!(ceiling, 12_641);
        let body = "あ".repeat(25);
        let mut first = ChatTurn::text("tool", body.clone());
        first.name = Some("search_index".into());
        let mut second = ChatTurn::text("tool", body.clone());
        second.name = Some("search_index".into());
        assert_eq!(turn_tokens(&first), 20);
        let system = ChatTurn::text("system", "役割");
        let room = 34;
        let user = ChatTurn::text(
            "user",
            "問".repeat(tokens_to_chars(
                ceiling - room - turn_tokens(&system) - 4,
            )),
        );
        assert_eq!(
            ceiling - turn_tokens(&system) - turn_tokens(&user),
            room
        );
        let mut turns = vec![system, user, first, second];
        assert_eq!(turn_list_tokens(&turns) - ceiling, 6);
        fit_followup_request(&mut turns, ceiling);
        let after = turn_list_tokens(&turns);
        assert!(after + schema + reply <= limit, "{after}");
        assert_eq!(turns[2].content, body);
        assert!(turns[3].content.starts_with('あ'));
        assert!(turns[3].content.contains("…（長いので省略しました）"));
        assert_ne!(turns[3].content, TOOL_RESULT_OMITTED);
        assert_eq!(turn_tokens(&turns[3]), 14);
    }

    #[test]
    fn shorter_prefix_is_kept_when_the_placeholder_does_not_fit() {
        let limit = 16_384;
        let schema = 2_105;
        let reply = reply_headroom_tokens(limit);
        let ceiling = limit - schema - reply;
        let body = "読".repeat(200);
        let mut first = ChatTurn::text("tool", body.clone());
        first.name = Some("read_unit".into());
        let mut second = ChatTurn::text("tool", body);
        second.name = Some("read_unit".into());
        assert_eq!(turn_tokens(&first), 129);
        let system = ChatTurn::text("system", "役割");
        let room = 30;
        let user = ChatTurn::text(
            "user",
            "問".repeat(tokens_to_chars(
                ceiling - room - turn_tokens(&system) - 4,
            )),
        );
        assert_eq!(
            ceiling - turn_tokens(&system) - turn_tokens(&user),
            room
        );
        let mut turns = vec![system, user, first, second];
        fit_followup_request(&mut turns, ceiling);
        let after = turn_list_tokens(&turns);
        assert!(after + schema + reply <= limit, "{after}");
        assert_eq!(turn_tokens(&turns[2]), 15);
        assert_eq!(turn_tokens(&turns[3]), 14);
        assert!(turns[2].content.starts_with('読'));
        assert!(turns[3].content.starts_with('読'));
        assert_ne!(turns[2].content, TOOL_RESULT_OMITTED);
        assert_ne!(turns[3].content, TOOL_RESULT_OMITTED);
        assert!(turns[2].content.contains("…（長いので省略しました）"));
        assert!(turns[3].content.contains("…（長いので省略しました）"));
    }

    #[test]
    fn shrunk_reply_reserve_keeps_results_that_fit_the_tool_allowance() {
        let limit = 16_384;
        let schema = 2_105;
        let split = context_budget(limit, schema, 12_700).expect("question fits");
        assert_eq!(split.messages, 12_700);
        assert_eq!(split.tool_allowance, 1_127);
        let inject_budget = split.messages + split.tool_allowance;
        assert_eq!(inject_budget, 13_827);
        let note_body = "記".repeat(tokens_to_chars(996));
        let search_body = "索".repeat(tokens_to_chars(96));
        let mut note = ChatTurn::text("tool", note_body.clone());
        note.name = Some(crate::llm::tools::TOOL_READ_NOTE.into());
        let mut search = ChatTurn::text("tool", search_body.clone());
        search.name = Some("search_index".into());
        assert_eq!(turn_tokens(&note), 1_000);
        assert_eq!(turn_tokens(&search), 100);
        let system = ChatTurn::text("system", "役割");
        let user = ChatTurn::text(
            "user",
            "問".repeat(tokens_to_chars(12_700 - turn_tokens(&system) - 4)),
        );
        let assistant = ChatTurn::text("assistant", "助".repeat(tokens_to_chars(15)));
        assert_eq!(turn_tokens(&system) + turn_tokens(&user), 12_700);
        assert_eq!(turn_tokens(&assistant), 19);
        let mut turns = vec![system, user, assistant, search, note];
        assert_eq!(turn_list_tokens(&turns), 13_819);
        assert!(turn_list_tokens(&turns) > limit - schema - reply_headroom_tokens(limit));
        fit_followup_request(&mut turns, inject_budget);
        assert!(turn_list_tokens(&turns) <= inject_budget);
        assert_eq!(turns[3].content, search_body);
        assert_eq!(turns[4].content, note_body);
    }

    #[test]
    fn shrunk_reply_reserve_keeps_a_large_note_at_260k() {
        let limit = 260_000;
        let schema = 2_105;
        let split = context_budget(limit, schema, 200_000).expect("question fits");
        assert_eq!(split.messages, 200_000);
        assert_eq!(split.tool_allowance, 41_353);
        let inject_budget = split.messages + split.tool_allowance;
        assert_eq!(inject_budget, 241_353);
        assert_eq!(
            limit - schema - split.messages - split.tool_allowance,
            16_542
        );
        let note_body = "記".repeat(tokens_to_chars(34_996));
        let mut note = ChatTurn::text("tool", note_body.clone());
        note.name = Some(crate::llm::tools::TOOL_READ_NOTE.into());
        assert_eq!(turn_tokens(&note), 35_000);
        let system = ChatTurn::text("system", "役割");
        let user = ChatTurn::text(
            "user",
            "問".repeat(tokens_to_chars(200_000 - turn_tokens(&system) - 4)),
        );
        let assistant = ChatTurn::text("assistant", "助".repeat(tokens_to_chars(15)));
        assert_eq!(turn_tokens(&system) + turn_tokens(&user), 200_000);
        assert_eq!(turn_tokens(&assistant), 19);
        let mut turns = vec![system, user, assistant, note];
        assert_eq!(turn_list_tokens(&turns), 235_019);
        assert!(turn_list_tokens(&turns) > limit - schema - reply_headroom_tokens(limit));
        fit_followup_request(&mut turns, inject_budget);
        assert!(turn_list_tokens(&turns) <= inject_budget);
        assert_eq!(turns[3].content, note_body);
    }

    #[test]
    fn truncation_floor_keeps_a_note_beside_two_searches() {
        let limit = 260_000;
        let schema = 2_105;
        let reply = reply_headroom_tokens(limit);
        let ceiling = limit - schema - reply;
        let room = 30_033;
        let search_body = "索".repeat(tokens_to_chars(1_000));
        let note_body = "記".repeat(tokens_to_chars(29_996));
        let mut first = ChatTurn::text("tool", search_body.clone());
        first.name = Some("search_index".into());
        let mut second = ChatTurn::text("tool", search_body);
        second.name = Some("search_index".into());
        let mut note = ChatTurn::text("tool", note_body.clone());
        note.name = Some(crate::llm::tools::TOOL_READ_NOTE.into());
        assert_eq!(turn_tokens(&first), 1_004);
        assert_eq!(turn_tokens(&note), 30_000);
        let system = ChatTurn::text("system", "役割");
        let non_tool = ceiling - room;
        let user = ChatTurn::text(
            "user",
            "問".repeat(tokens_to_chars(non_tool - turn_tokens(&system) - 4)),
        );
        assert_eq!(turn_tokens(&system) + turn_tokens(&user), non_tool);
        let mut turns = vec![system, user, first, second, note];
        assert!(turn_list_tokens(&turns) > ceiling);
        fit_followup_request(&mut turns, ceiling);
        let after = turn_list_tokens(&turns);
        assert!(after + schema + reply <= limit, "{after}");
        assert_eq!(turns[4].content, note_body);
        assert_eq!(turn_tokens(&turns[2]), 18);
        assert_eq!(turn_tokens(&turns[3]), 14);
        assert!(turns[2].content.starts_with('索'));
        assert!(turns[3].content.starts_with('索'));
        assert!(turns[2].content.contains("…（長いので省略しました）"));
        assert!(turns[3].content.contains("…（長いので省略しました）"));
        assert_ne!(turns[2].content, TOOL_RESULT_OMITTED);
        assert_ne!(turns[3].content, TOOL_RESULT_OMITTED);
    }
}
