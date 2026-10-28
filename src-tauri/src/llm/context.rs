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

/// Room for assembled messages after the reply, the in-turn tool allowance, and the schema.
pub fn assembly_budget(limit_tokens: usize, schema_tokens: usize) -> usize {
    limit_tokens
        .saturating_sub(reply_headroom_tokens(limit_tokens))
        .saturating_sub(tool_result_allowance_tokens(limit_tokens))
        .saturating_sub(schema_tokens)
        .max(1)
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
    pub consumed: Vec<ConsumedSource>,
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
            push_cite_prefix(&mut out, n);
            out.push_str(&title);
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
        push_cite_prefix(&mut out, n);
        out.push_str(title);
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
    sources: Vec<LlmSourceRow>,
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
            sources: attached,
            question,
            message_id: m.id.clone(),
        });
    }

    let rendered = sources_on_turns(&built);
    dedupe_tool_bodies(&mut built, &rendered);

    let mut truncated = false;
    // Drop old exchanges while source bodies are still present, so chatter goes
    // before those bodies. Then stub anything outside the recent window.
    trim_history(&mut built, max_tokens, &mut truncated);
    apply_age_stubs(&mut built, pool);
    if tokens_of(&built) > max_tokens && refit_sources(&mut built, pool, max_tokens, &pending_ids) {
        truncated = true;
        apply_age_stubs(&mut built, pool);
    }
    if tokens_of(&built) > max_tokens {
        trim_history(&mut built, max_tokens, &mut truncated);
    }
    if tokens_of(&built) > max_tokens && force_under_budget(&mut built, max_tokens) {
        truncated = true;
    }

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

fn sources_on_turns(built: &[BuiltTurn]) -> Vec<LlmSourceRow> {
    built
        .iter()
        .flat_map(|row| row.sources.iter().cloned())
        .collect()
}

fn tokens_of(built: &[BuiltTurn]) -> usize {
    built.iter().map(|row| turn_tokens(&row.turn)).sum()
}

/// Drop source bodies from tool turns when those bodies are also placed on a user turn.
pub fn omit_source_bodies_from_tool_turns(turns: &mut [ChatTurn], sources: &[LlmSourceRow]) {
    if sources.is_empty() {
        return;
    }
    for turn in turns.iter_mut() {
        if turn.role == "tool" {
            turn.content = strip_rendered_sources(&turn.content, sources);
        }
    }
}

/// A source rendered on a user turn must not also appear in full inside a tool result.
fn dedupe_tool_bodies(built: &mut [BuiltTurn], rendered: &[LlmSourceRow]) {
    if rendered.is_empty() {
        return;
    }
    for row in built.iter_mut() {
        if row.turn.role != "tool" {
            continue;
        }
        row.turn.content = strip_rendered_sources(&row.turn.content, rendered);
    }
}

fn strip_rendered_sources(content: &str, rendered: &[LlmSourceRow]) -> String {
    let mut out = content.to_string();
    let mut stubs = Vec::new();
    for source in rendered {
        let next = strip_one_source(&out, source);
        if next != out {
            stubs.push(source_stub_line(source));
            out = next;
        }
    }
    out = tidy_empty_citation(&out);
    if out.trim().is_empty() {
        return stubs.join("\n");
    }
    out
}

fn strip_one_source(content: &str, source: &LlmSourceRow) -> String {
    let entry = citation_entry(source);
    if !entry.trim().is_empty() && content.contains(&entry) {
        return content.replacen(&entry, "", 1);
    }
    let body = source.body.trim();
    if body.chars().count() >= 8 && content.contains(body) {
        let cite = if source.cite_no > 0 {
            format!("[{}]", source.cite_no)
        } else {
            String::new()
        };
        let pid = source.paragraph_id.trim();
        if (!cite.is_empty() && content.contains(&cite))
            || (!pid.is_empty() && content.contains(pid))
        {
            return content.replacen(body, "", 1);
        }
    }
    content.to_string()
}

fn citation_entry(row: &LlmSourceRow) -> String {
    let block = format_sources(std::slice::from_ref(row));
    block
        .strip_prefix("【出典】\n")
        .unwrap_or(&block)
        .to_string()
}

fn tidy_empty_citation(content: &str) -> String {
    if !content.contains("【出典】") {
        return content.trim().to_string();
    }
    let rest = content.replace("【出典】", "");
    if rest.contains('[') || rest.contains("既読") {
        return content.to_string();
    }
    rest.trim().to_string()
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
            row.turn.content = stub_old_tool(&row.turn.content, pool);
        }
    }
}

fn stub_old_tool(content: &str, sources: &[LlmSourceRow]) -> String {
    let mut out = content.to_string();
    let mut any = false;
    for source in sources {
        let entry = citation_entry(source);
        if !entry.trim().is_empty() && out.contains(&entry) {
            out = out.replacen(&entry, &format!("{}\n\n", source_stub_line(source)), 1);
            any = true;
            continue;
        }
        let body = source.body.trim();
        if body.chars().count() >= 8 && out.contains(body) {
            out = out.replacen(body, &source_stub_line(source), 1);
            any = true;
        }
    }
    if any {
        let tidied = tidy_empty_citation(&out);
        return if tidied.trim().is_empty() {
            OLD_TOOL_STUB.to_string()
        } else {
            tidied
        };
    }
    if content.contains("既読 [") || estimate_tokens(content) <= 48 {
        return content.to_string();
    }
    OLD_TOOL_STUB.to_string()
}

fn source_char_room(built: &[BuiltTurn], max_tokens: usize) -> usize {
    let base: usize = built
        .iter()
        .map(|row| {
            if row.turn.role == "user" && !row.sources.is_empty() {
                turn_tokens(&ChatTurn::text("user", &row.question))
            } else {
                turn_tokens(&row.turn)
            }
        })
        .sum();
    tokens_to_chars(max_tokens.saturating_sub(base))
}

/// Drop source bodies that still do not fit. Attachments are removed last, matching
/// the previous source priority, and only after old exchanges have already been dropped.
fn refit_sources(
    built: &mut [BuiltTurn],
    pool: &[LlmSourceRow],
    max_tokens: usize,
    pending_ids: &HashSet<String>,
) -> bool {
    let current: Vec<LlmSourceRow> = sources_on_turns(built);
    if current.is_empty() {
        return false;
    }
    let (kept, dropped, shortened) =
        select_sources(&current, source_char_room(built, max_tokens), pending_ids);
    if dropped == 0 && !shortened {
        return false;
    }
    let kept_by_id: HashMap<String, LlmSourceRow> =
        kept.into_iter().map(|row| (row.id.clone(), row)).collect();
    let recent = recent_user_ids(built);
    for row in built.iter_mut() {
        if row.sources.is_empty() {
            continue;
        }
        row.sources = row
            .sources
            .iter()
            .filter_map(|source| kept_by_id.get(&source.id).cloned())
            .collect();
        if row.turn.role == "user" {
            let stub = !recent.contains(&row.message_id);
            row.turn.content = render_user_content(&row.question, &row.sources, pool, stub);
        }
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
                    && char_len(&row.turn.content) > 80
            })
            .max_by_key(|(_, row)| char_len(&row.turn.content))
            .map(|(index, _)| index)
            .or_else(|| {
                built
                    .iter()
                    .enumerate()
                    .filter(|(_, row)| {
                        row.turn.role != "system" && char_len(&row.turn.content) > 40
                    })
                    .max_by_key(|(_, row)| char_len(&row.turn.content))
                    .map(|(index, _)| index)
            });
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

/// Shrink an already assembled request. Used when the server rejects the first send.
pub fn shrink_turns(turns: &mut Vec<ChatTurn>, budget: usize) -> bool {
    let before: usize = turns.iter().map(turn_tokens).sum();
    let budget = budget.max(1);
    loop {
        let used: usize = turns.iter().map(turn_tokens).sum();
        if used <= budget {
            break;
        }
        let mut dropped = 0;
        for _ in 0..TRIM_EXCHANGE_BATCH {
            if !drop_oldest_chat_exchange(turns) {
                break;
            }
            dropped += 1;
        }
        if dropped == 0 {
            break;
        }
    }
    let mut guard = 0;
    while turns.iter().map(turn_tokens).sum::<usize>() > budget && guard < 24 {
        guard += 1;
        let latest = turns.iter().rposition(|turn| turn.role == "user");
        let idx = turns
            .iter()
            .enumerate()
            .filter(|(index, turn)| {
                turn.role != "system" && Some(*index) != latest && char_len(&turn.content) > 40
            })
            .max_by_key(|(_, turn)| char_len(&turn.content))
            .map(|(index, _)| index);
        let Some(idx) = idx else {
            break;
        };
        let len = char_len(&turns[idx].content);
        let next = (len / 2).max(20);
        if next >= len {
            break;
        }
        turns[idx].content = cap_chars(&turns[idx].content, next);
    }
    turns.iter().map(turn_tokens).sum::<usize>() < before
}

fn drop_oldest_chat_exchange(turns: &mut Vec<ChatTurn>) -> bool {
    let start = usize::from(turns.first().is_some_and(|turn| turn.role == "system"));
    let latest = turns.iter().rposition(|turn| turn.role == "user");
    let end = latest.unwrap_or(turns.len());
    if start >= end {
        return false;
    }
    let mut remove_until = start + 1;
    while remove_until < end && turns[remove_until].role != "user" {
        remove_until += 1;
    }
    turns.drain(start..remove_until);
    true
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

/// Drop old exchanges in batches until the request fits.
/// The system prompt and the latest user turn stay. A user turn takes its reply
/// and that reply's tool results with it, so a tool call is not left unpaired.
fn trim_history(built: &mut Vec<BuiltTurn>, max_tokens: usize, truncated: &mut bool) {
    while tokens_of(built) > max_tokens {
        let mut dropped = 0;
        for _ in 0..TRIM_EXCHANGE_BATCH {
            if !drop_oldest_exchange(built) {
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

fn drop_oldest_exchange(built: &mut Vec<BuiltTurn>) -> bool {
    let start = usize::from(built.first().is_some_and(|row| row.turn.role == "system"));
    let latest = built.iter().rposition(|row| row.turn.role == "user");
    let end = latest.unwrap_or(built.len());
    if start >= end {
        return false;
    }
    let mut remove_until = start + 1;
    while remove_until < end && built[remove_until].turn.role != "user" {
        remove_until += 1;
    }
    built.drain(start..remove_until);
    true
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
    fn old_tool_results_leave_with_their_exchange_before_the_latest_question() {
        let mut tool = msg("t1", "tool", &"あ".repeat(20_000));
        tool.tool_call_id = "c1".into();
        let history = vec![msg("u1", "user", "調べて"), tool, msg("u2", "user", "続き")];
        let (turns, stats) = assemble_turns("役割", &[], &history, 1_000);
        assert!(stats.truncated);
        assert!(turns.iter().all(|t| t.role != "tool"));
        assert!(turns.iter().any(|t| t.content == "続き"));
        assert!(!turns.iter().any(|t| t.content.contains(&"あ".repeat(100))));
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
        assert!(
            users.iter().all(|text| !text.contains(&"あ".repeat(80))),
            "the oldest chatter is the part that leaves: {users:?}"
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
        let budget = assembly_budget(limit, schema);
        let reply = reply_headroom_tokens(limit);
        let tools = tool_result_allowance_tokens(limit);
        assert_eq!(reply, 26_000);
        assert_eq!(tools, 65_000);
        assert_eq!(budget, limit - reply - tools - schema);
        assert!(budget + reply + tools + schema <= limit);
        let small = assembly_budget(1_000, 5_000);
        assert_eq!(small, 1);
        let source = src("a", "attach", 0, "本文");
        let history = vec![msg("u1", "user", "質問")];
        let (turns, stats) = assemble_turns("役割", &[source], &history, budget);
        assert!(!stats.truncated);
        assert!(turns.iter().any(|turn| turn.content.contains("本文")));
    }

    #[test]
    fn shrink_turns_drops_several_exchanges_and_keeps_the_latest_question() {
        let mut turns = vec![ChatTurn::text("system", "役割")];
        for n in 1..6 {
            turns.push(ChatTurn::text("user", format!("質問{n}")));
            turns.push(ChatTurn::text("assistant", format!("答え{n}")));
        }
        turns.push(ChatTurn::text("user", "最新の質問"));
        let before = turns.len();
        assert!(shrink_turns(&mut turns, 30));
        assert!(turns.len() + TRIM_EXCHANGE_BATCH <= before);
        assert_eq!(turns[0].role, "system");
        assert!(turns.iter().any(|turn| turn.content == "最新の質問"));
        assert!(turns.iter().all(|turn| turn.content != "質問1"));
    }
}
