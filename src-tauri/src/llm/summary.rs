//! One stored summary per thread. It replaces covered messages in the model request.
//! The original messages stay in the database.

use crate::db::{LlmMessageRow, LlmSourceRow};
use crate::llm::context::{
    estimate_tokens, message_is_replayed, reply_headroom_tokens, summary_stub_line,
    summary_stubs_for_tool, tokens_to_chars, turn_tokens, ChatTurn, SUMMARY_TURN_ID,
};

pub const SUMMARY_PREFIX: &str = "【ここまでの要約】";
pub const SUMMARY_DISCLAIMER: &str =
    "この要約には誤りが含まれることがあります。重要な日付・金額・当事者・条番号は原文で確認してください。";
pub const SUMMARY_STATUS_TEXT: &str = "会話が長くなったため、古いやり取りを要約しています…";
pub const SUMMARY_FAILURE_WARNING: &str = "要約できなかったため、古い部分を省いて送りました";
/// Model output cap for one summary call. The disclaimer is appended after this.
pub const SUMMARY_OUTPUT_TOKENS: usize = 2_000;
/// One send summarizes at most this many fitting chunks. The rest waits for later sends.
pub const SUMMARY_CHUNKS_PER_SEND: usize = 3;
const SUMMARY_CLIP_NOTE: &str = "…（長いので省略しました）";

/// Tokens the summary request may spend on input: the context limit, minus the reply
/// reserve, minus the summary output cap. Counted with the same tokenizer as the meter.
pub fn summary_request_budget(limit_tokens: usize) -> usize {
    limit_tokens
        .saturating_sub(reply_headroom_tokens(limit_tokens))
        .saturating_sub(SUMMARY_OUTPUT_TOKENS)
}

pub fn summary_system_prompt() -> String {
    format!(
        "あなたは会話の要約係です。渡されたやり取りを、この後の質問に答えるためのメモにまとめてください。\n\
         \n\
         - 日付、金額、人数、当事者名、法令名、条番号は、原文の表記のまま残す。丸めたり、言い換えたりしない。\n\
         - 本文に現れる出典番号 [n] だけを残す。本文にない番号は書かない。\n\
         - 出典は「既読 [n] ファイル名」の形だけを残し、本文は書かない。\n\
         - 約2,000トークン以内に収める。\n\
         - 最後は必ず次の文で終える: {SUMMARY_DISCLAIMER}"
    )
}

pub struct SummaryCall {
    pub turns: Vec<ChatTurn>,
    /// `Some(false)` forces thinking off, whatever the chat setting is.
    pub thinking: Option<bool>,
    /// Hard cap sent as `max_tokens`. The stored text is clipped to this and then
    /// the disclaimer is appended.
    pub max_tokens: u32,
}

pub fn summary_call(transcript: &str) -> SummaryCall {
    SummaryCall {
        turns: vec![
            ChatTurn::text("system", summary_system_prompt()),
            ChatTurn::text(
                "user",
                format!(
                    "次のやり取りを規則に従って要約してください。\n\n{}",
                    transcript.trim()
                ),
            ),
        ],
        thinking: Some(false),
        max_tokens: SUMMARY_OUTPUT_TOKENS as u32,
    }
}

/// Tokens of the summary request, system prompt included, with the meter tokenizer.
pub fn summary_request_tokens(transcript: &str) -> usize {
    summary_call(transcript)
        .turns
        .iter()
        .map(turn_tokens)
        .sum()
}

pub fn finalize_summary(raw: &str) -> Result<String, String> {
    let text = raw.trim().trim_start_matches(SUMMARY_PREFIX).trim();
    let body = text.replace(SUMMARY_DISCLAIMER, "");
    let body = body.trim();
    if body.is_empty() {
        return Err("要約が空です".into());
    }
    let body = clip_summary_body(body, SUMMARY_OUTPUT_TOKENS);
    if body.is_empty() {
        return Err("要約が空です".into());
    }
    Ok(format!("{body}\n{SUMMARY_DISCLAIMER}"))
}

fn clip_summary_body(body: &str, max_tokens: usize) -> String {
    if estimate_tokens(body) <= max_tokens {
        return body.to_string();
    }
    let max_chars = tokens_to_chars(max_tokens).max(1);
    body.chars().take(max_chars).collect::<String>().trim_end().to_string()
}

/// Shorten a summary turn by clipping its body. The prefix and the disclaimer stay.
pub(crate) fn clip_injected_summary(content: &str, max_chars: usize) -> String {
    let (prose, stubs) = split_trailing_stubs(content.trim());
    let without_prefix = prose
        .trim()
        .strip_prefix(SUMMARY_PREFIX)
        .unwrap_or(prose.trim())
        .trim();
    let body = without_prefix.replace(SUMMARY_DISCLAIMER, "");
    let body = body.trim();
    let fixed = format!("{SUMMARY_PREFIX}\n{SUMMARY_DISCLAIMER}");
    let fixed_len = fixed.chars().count();
    let prose = if max_chars <= fixed_len || body.is_empty() {
        fixed
    } else {
        let room = max_chars.saturating_sub(fixed_len);
        let head: String = body.chars().take(room).collect();
        let head = head.trim();
        if head.is_empty() {
            fixed
        } else {
            format!("{SUMMARY_PREFIX}\n{head}\n{SUMMARY_DISCLAIMER}")
        }
    };
    if stubs.trim().is_empty() {
        prose
    } else {
        format!("{prose}\n\n{}", stubs.trim())
    }
}

/// Summary text stored in the thread and shown in the UI: the injected prose, without stubs.
pub fn stored_text_from_turn(content: &str) -> String {
    let (prose, _) = split_trailing_stubs(content.trim());
    prose
        .trim()
        .strip_prefix(SUMMARY_PREFIX)
        .unwrap_or(prose.trim())
        .trim()
        .to_string()
}

fn split_trailing_stubs(content: &str) -> (String, String) {
    let mut lines: Vec<&str> = content.split_inclusive('\n').collect();
    let mut stub_lines = Vec::new();
    while let Some(line) = lines.last().copied() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("既読 ") {
            stub_lines.push(lines.pop().unwrap());
            continue;
        }
        break;
    }
    stub_lines.reverse();
    (lines.concat(), stub_lines.concat())
}

pub fn summary_turn_content(summary: &str) -> String {
    format!("{SUMMARY_PREFIX}\n{}", summary.trim())
}

/// Split a user turn that starts with the summary block. The block is the summary;
/// the rest is that user's own text.
pub(crate) fn split_merged_user(content: &str) -> (String, String) {
    let content = content.trim();
    if let Some(pos) = content.find(SUMMARY_DISCLAIMER) {
        let end = pos + SUMMARY_DISCLAIMER.len();
        let block = content[..end].trim().to_string();
        let user = content[end..].trim().to_string();
        return (block, user);
    }
    let Some(rest) = content.strip_prefix(SUMMARY_PREFIX) else {
        return (content.to_string(), String::new());
    };
    let rest = rest.trim_start_matches(['\n', '\r']);
    if let Some((summary, user)) = rest.split_once("\n\n") {
        (
            format!("{SUMMARY_PREFIX}\n{}", summary.trim()),
            user.trim().to_string(),
        )
    } else {
        (content.to_string(), String::new())
    }
}

pub struct SummaryChunk {
    pub through_id: String,
    pub transcript: String,
}

/// The next fitting slice of the covered range. A message longer than the budget is
/// clipped to a head plus a stub note, and `through_id` still advances past it.
pub fn next_summary_chunk(
    history: &[LlmMessageRow],
    sources: &[LlmSourceRow],
    previous_summary: &str,
    previous_through_id: &str,
    budget_tokens: usize,
) -> Option<SummaryChunk> {
    let fresh = fresh_messages(history, previous_summary, previous_through_id)?;
    if !fresh.iter().any(message_is_replayed) {
        return None;
    }
    let budget_tokens = budget_tokens.max(1);
    let mut parts: Vec<String> = Vec::new();
    let mut through_id: Option<String> = None;
    let mut index = 0;
    while index < fresh.len() {
        let end = tool_group_end(fresh, index);
        let group = &fresh[index..=end];
        let rendered = render_summary_input(group, sources);
        let group_through = fresh[end].id.clone();
        if rendered.trim().is_empty() {
            // Empty tool results and notice rows still move the cursor. A notice is
            // not replayed, so leaving `through_id` in front of it stalls the next chunk.
            let tool_tail = fresh[index].role == "tool"
                || (fresh[index].role == "assistant" && !fresh[index].tool_calls.trim().is_empty());
            let skipped = group.iter().any(|message| !message_is_replayed(message));
            if tool_tail || skipped {
                through_id = Some(group_through);
            }
            index = end + 1;
            continue;
        }
        let mut trial = parts.clone();
        trial.push(rendered.clone());
        if summary_request_tokens(&join_transcript(previous_summary, &trial)) <= budget_tokens {
            parts.push(rendered);
            through_id = Some(group_through);
            index = end + 1;
            continue;
        }
        if through_id.is_some() && parts.iter().any(|part| !part.trim().is_empty()) {
            break;
        }
        parts.push(clip_message_into_budget(
            previous_summary,
            &rendered,
            budget_tokens,
        ));
        through_id = Some(group_through);
        break;
    }
    let through_id = through_id?;
    let transcript = join_transcript(previous_summary, &parts);
    if transcript.trim().is_empty() {
        return None;
    }
    Some(SummaryChunk {
        through_id,
        transcript,
    })
}

/// Messages before the two most recent user turns, when the whole slice fits.
/// Callers that must stay inside the model window use [`next_summary_chunk`].
pub fn summary_job(
    history: &[LlmMessageRow],
    sources: &[LlmSourceRow],
    previous_summary: &str,
    previous_through_id: &str,
) -> Option<SummaryChunk> {
    next_summary_chunk(
        history,
        sources,
        previous_summary,
        previous_through_id,
        usize::MAX / 4,
    )
}

fn fresh_messages<'a>(
    history: &'a [LlmMessageRow],
    previous_summary: &str,
    previous_through_id: &str,
) -> Option<&'a [LlmMessageRow]> {
    let user_indexes: Vec<usize> = history
        .iter()
        .enumerate()
        .filter(|(_, message)| message.role == "user" && message_is_replayed(message))
        .map(|(index, _)| index)
        .collect();
    if user_indexes.len() < 2 {
        return None;
    }
    let cut = user_indexes[user_indexes.len() - 2];
    if cut == 0 {
        return None;
    }
    let covered = &history[..cut];
    let previous_summary = previous_summary.trim();
    let previous_through_id = previous_through_id.trim();
    let fresh: &[LlmMessageRow] = if previous_summary.is_empty() || previous_through_id.is_empty() {
        covered
    } else if let Some(pos) = covered
        .iter()
        .position(|message| message.id == previous_through_id)
    {
        &covered[pos + 1..]
    } else {
        covered
    };
    if fresh.is_empty() {
        None
    } else {
        Some(fresh)
    }
}

/// Assistant tool call plus the tool results that follow it. A chunk never ends between them.
fn tool_group_end(messages: &[LlmMessageRow], start: usize) -> usize {
    if start >= messages.len() {
        return start;
    }
    let opens_tools =
        messages[start].role == "assistant" && !messages[start].tool_calls.trim().is_empty();
    if !opens_tools && messages[start].role != "tool" {
        return start;
    }
    let mut end = start;
    while end + 1 < messages.len() && messages[end + 1].role == "tool" {
        end += 1;
    }
    end
}

fn join_transcript(previous_summary: &str, parts: &[String]) -> String {
    let mut transcript = String::new();
    let previous_summary = previous_summary.trim();
    if !previous_summary.is_empty() {
        transcript.push_str("これまでの要約:\n");
        transcript.push_str(previous_summary);
        transcript.push_str("\n\n");
    }
    for part in parts {
        transcript.push_str(part);
    }
    transcript.trim().to_string()
}

fn clip_message_into_budget(previous_summary: &str, rendered: &str, budget_tokens: usize) -> String {
    let fits = |head_chars: usize| {
        let head: String = rendered.chars().take(head_chars).collect();
        let body = if head.is_empty() {
            SUMMARY_CLIP_NOTE.to_string()
        } else {
            format!("{head}{SUMMARY_CLIP_NOTE}")
        };
        summary_request_tokens(&join_transcript(previous_summary, &[body])) <= budget_tokens
    };
    if !fits(0) {
        return SUMMARY_CLIP_NOTE.to_string();
    }
    let mut lo = 0usize;
    let mut hi = rendered.chars().count();
    while lo < hi {
        let mid = lo + (hi - lo + 1) / 2;
        if fits(mid) {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    let head: String = rendered.chars().take(lo).collect();
    if head.is_empty() {
        SUMMARY_CLIP_NOTE.to_string()
    } else {
        format!("{head}{SUMMARY_CLIP_NOTE}")
    }
}

fn render_summary_input(messages: &[LlmMessageRow], sources: &[LlmSourceRow]) -> String {
    let mut out = String::new();
    for message in messages {
        if !message_is_replayed(message) {
            continue;
        }
        match message.role.as_str() {
            "user" => {
                let text = message.content.trim();
                let stubs = user_stubs(&message.id, sources);
                if text.is_empty() && stubs.is_empty() {
                    continue;
                }
                out.push_str("ユーザー:\n");
                if !text.is_empty() {
                    out.push_str(text);
                    out.push('\n');
                }
                for stub in &stubs {
                    out.push_str(stub);
                    out.push('\n');
                }
                out.push('\n');
            }
            "assistant" => {
                let text = message.content.trim();
                if text.is_empty() {
                    continue;
                }
                out.push_str("アシスタント:\n");
                out.push_str(text);
                out.push_str("\n\n");
            }
            "tool" => {
                let name = message.tool_name.trim();
                let stubs = summary_stubs_for_tool(
                    &message.content,
                    sources,
                    if name.is_empty() { None } else { Some(name) },
                );
                if stubs.is_empty() {
                    continue;
                }
                for stub in &stubs {
                    out.push_str(stub);
                    out.push('\n');
                }
                out.push('\n');
            }
            _ => {}
        }
    }
    out
}

fn user_stubs(message_id: &str, sources: &[LlmSourceRow]) -> Vec<String> {
    sources
        .iter()
        .filter(|source| source.injected_user_message_id == message_id && source.is_injectable())
        .map(summary_stub_line)
        .collect()
}

/// Replace messages through `through_id` with the summary, without two user turns in a row.
/// The summary is merged into the next user turn. It is its own user turn only when the
/// next kept turn is not a user (typically an assistant), so roles still alternate.
pub fn prepare_model_history(
    history: &[LlmMessageRow],
    summary: &str,
    through_id: &str,
) -> Vec<LlmMessageRow> {
    let summary = summary.trim();
    let through_id = through_id.trim();
    if summary.is_empty() || through_id.is_empty() {
        return history.to_vec();
    }
    let Some(pos) = history.iter().position(|message| message.id == through_id) else {
        return history.to_vec();
    };
    let mut rest: Vec<LlmMessageRow> = history[pos + 1..].to_vec();
    // Tool rows whose assistant was already summarized cannot open the request,
    // even when a notice sits in front of them. Notices stay; they are not replayed.
    let mut seen_anchor = false;
    rest.retain(|message| {
        if !seen_anchor && message.role == "tool" {
            return false;
        }
        if message_is_replayed(message) && message.role != "tool" {
            seen_anchor = true;
        }
        true
    });
    let block = summary_turn_content(summary);
    let summary_row = LlmMessageRow {
        id: SUMMARY_TURN_ID.into(),
        thread_id: history[pos].thread_id.clone(),
        role: "user".into(),
        content: block.clone(),
        created_at: history[pos].created_at,
        reasoning: String::new(),
        tool_calls: String::new(),
        tool_call_id: String::new(),
        tool_name: String::new(),
        kind: String::new(),
    };
    // Notices are not replayed. The next row that is actually sent decides merge vs standalone.
    let Some(index) = rest.iter().position(message_is_replayed) else {
        let mut out = vec![summary_row];
        out.extend(rest);
        return out;
    };
    if rest[index].role == "user" {
        let user_text = rest[index].content.trim().to_string();
        rest[index].content = if user_text.is_empty() {
            block
        } else {
            format!("{block}\n\n{user_text}")
        };
        return rest;
    }
    rest.insert(index, summary_row);
    rest
}

/// Injection clipping stays in the request. The stored summary is the canonical text.
pub fn summary_saved_after_request(stored: &str, _request_turn: &str) -> String {
    stored.trim().to_string()
}

pub enum SummaryAttempt {
    NotRun,
    Saved { summary: String, through_id: String },
    Failed,
}

pub struct FoldedHistory {
    pub messages: Vec<LlmMessageRow>,
    pub summary: String,
    pub through_id: String,
    pub failed: bool,
}

/// Apply a successful summary, or keep the previous one and let the existing trim run.
pub fn fold_summary(
    history: &[LlmMessageRow],
    previous_summary: &str,
    previous_through_id: &str,
    attempt: SummaryAttempt,
) -> FoldedHistory {
    let (summary, through_id, failed) = match attempt {
        SummaryAttempt::Saved {
            summary,
            through_id,
        } => (summary, through_id, false),
        SummaryAttempt::NotRun => (
            previous_summary.trim().to_string(),
            previous_through_id.trim().to_string(),
            false,
        ),
        SummaryAttempt::Failed => (
            previous_summary.trim().to_string(),
            previous_through_id.trim().to_string(),
            true,
        ),
    };
    let messages = prepare_model_history(history, &summary, &through_id);
    FoldedHistory {
        messages,
        summary,
        through_id,
        failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::LLM_MESSAGE_NOTICE;
    use crate::llm::context::{
        assemble_turns, context_budget, estimate_tokens, meter_denominator, meter_usage_tokens,
        reply_headroom_tokens, summary_trigger_reached, turn_tokens, SUMMARY_TRIGGER_RATIO,
    };
    use crate::llm::tools::TOOL_READ_NOTE;

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

    fn source(id: &str, user_id: &str, cite: i64, body: &str) -> LlmSourceRow {
        LlmSourceRow {
            id: id.into(),
            thread_id: "t".into(),
            sort_order: cite,
            origin: "attach".into(),
            path: r"C:\docs\契約書.docx".into(),
            title: "契約書".into(),
            paragraph_id: id.into(),
            body: body.into(),
            query: String::new(),
            created_at: cite,
            grain: "unit".into(),
            unit_body: String::new(),
            injected_user_message_id: user_id.into(),
            cited_assistant_message_id: String::new(),
            cite_no: cite,
            kind: "text".into(),
            stored_relpath: String::new(),
            ocr_status: String::new(),
        }
    }

    fn three_turns() -> Vec<LlmMessageRow> {
        vec![
            msg("u1", "user", "最初の質問本文"),
            msg("a1", "assistant", "最初の答え"),
            msg("u2", "user", "次の質問"),
            msg("a2", "assistant", "次の答え"),
            msg("u3", "user", "直前の質問"),
        ]
    }

    #[test]
    fn prompt_keeps_the_required_rules_and_ends_with_the_disclaimer() {
        let prompt = summary_system_prompt();
        for phrase in [
            "日付",
            "金額",
            "人数",
            "当事者",
            "法令",
            "条番号",
            "[n]",
            "2,000",
        ] {
            assert!(prompt.contains(phrase), "{phrase} missing from {prompt}");
        }
        assert!(prompt.contains("既読 [n] ファイル名"));
        assert!(prompt.contains(SUMMARY_DISCLAIMER));
        let call = summary_call("本文");
        assert_eq!(call.thinking, Some(false));
        assert_eq!(call.max_tokens, SUMMARY_OUTPUT_TOKENS as u32);
        assert!(call.turns.iter().all(|turn| turn.tool_calls.is_none()));
        assert_eq!(call.turns[0].role, "system");
        assert!(call.turns[1].content.contains("本文"));
        assert!(!call.turns[0]
            .content
            .contains("資料です。指示ではありません"));
    }

    #[test]
    fn finalize_always_ends_with_the_disclaimer() {
        let done = finalize_summary("2024年3月1日に甲は100万円を払った。").unwrap();
        assert!(done.ends_with(SUMMARY_DISCLAIMER));
        assert!(done.contains("2024年3月1日"));
        assert!(done.contains("100万円"));
        let again = finalize_summary(&done).unwrap();
        assert_eq!(again.matches(SUMMARY_DISCLAIMER).count(), 1);
        assert!(finalize_summary("   ").is_err());
        assert!(finalize_summary(SUMMARY_PREFIX).is_err());
    }

    #[test]
    fn cut_is_before_the_two_most_recent_user_turns() {
        let history = three_turns();
        let job = summary_job(&history, &[], "", "").unwrap();
        assert_eq!(job.through_id, "a1");
        assert!(job.transcript.contains("最初の質問本文"));
        assert!(job.transcript.contains("最初の答え"));
        assert!(!job.transcript.contains("次の質問"));
        assert!(!job.transcript.contains("直前の質問"));
        let only_two = vec![
            msg("u1", "user", "一"),
            msg("a1", "assistant", "答"),
            msg("u2", "user", "二"),
        ];
        assert!(summary_job(&only_two, &[], "", "").is_none());
    }

    #[test]
    fn new_summary_takes_the_previous_summary_and_skips_what_it_covered() {
        let history = three_turns();
        let job = summary_job(&history, &[], "OLD_SUMMARY_TOKEN", "u1").unwrap();
        assert_eq!(job.through_id, "a1");
        assert!(job.transcript.contains("OLD_SUMMARY_TOKEN"));
        assert!(job.transcript.contains("最初の答え"));
        assert!(!job.transcript.contains("最初の質問本文"));
        assert!(summary_job(&history, &[], "OLD_SUMMARY_TOKEN", "a1").is_none());
    }

    #[test]
    fn summary_input_uses_stubs_and_not_source_bodies() {
        let body = "SOURCE_BODY_SECRET_9f3a";
        let sources = vec![source("s1", "u1", 1, body)];
        let mut history = three_turns();
        let mut tool = msg("t1", "tool", &crate::llm::context::format_sources(&sources));
        tool.tool_call_id = "c1".into();
        tool.tool_name = crate::llm::tools::TOOL_SEARCH.into();
        history.insert(1, tool);
        let mut note = msg("n1", "tool", "ノート本文SECRET");
        note.tool_call_id = "c2".into();
        note.tool_name = TOOL_READ_NOTE.into();
        history.insert(2, note);
        let job = summary_job(&history, &sources, "", "").unwrap();
        assert!(
            job.transcript.contains("既読 [1] 契約書.docx"),
            "{}",
            job.transcript
        );
        assert!(!job.transcript.contains(body), "{}", job.transcript);
        assert!(
            !job.transcript.contains("ノート本文SECRET"),
            "{}",
            job.transcript
        );
        assert!(job.transcript.contains("最初の質問本文"));
    }

    #[test]
    fn failed_summary_keeps_the_history_for_the_existing_trim() {
        let history = vec![
            msg("u1", "user", "消える古い質問"),
            msg("a1", "assistant", &"あ".repeat(8_000)),
            msg("u2", "user", "残る質問"),
        ];
        let folded = fold_summary(&history, "", "", SummaryAttempt::Failed);
        assert!(folded.failed);
        assert!(folded.summary.is_empty());
        assert_eq!(folded.messages.len(), history.len());
        let (turns, stats) = assemble_turns("役割", &[], &folded.messages, 400);
        assert!(stats.truncated);
        assert!(!stats.question_exceeds);
        let blob = turns
            .iter()
            .map(|turn| turn.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!blob.contains("消える古い質問"), "{blob}");
        assert!(blob.contains("残る質問"), "{blob}");
        assert!(!blob.contains(SUMMARY_PREFIX), "{blob}");
    }

    #[test]
    fn failed_resummary_keeps_the_previous_summary() {
        let history = three_turns();
        let folded = fold_summary(&history, "OLD_SUMMARY_TOKEN", "a1", SummaryAttempt::Failed);
        assert!(folded.failed);
        assert_eq!(folded.through_id, "a1");
        assert!(folded.messages[0].content.contains("OLD_SUMMARY_TOKEN"));
        assert!(folded.messages[0].content.contains("次の質問"));
        assert!(folded.messages.windows(2).all(|pair| {
            !(pair[0].role == "user" && pair[1].role == "user")
        }));
        assert!(folded
            .messages
            .iter()
            .all(|message| message.content != "最初の質問本文"));
    }

    #[test]
    fn injection_keeps_the_summary_when_an_older_exchange_is_trimmed() {
        let history = vec![
            msg("u0", "user", "カバーされる質問"),
            msg("a0", "assistant", "カバー答え"),
            msg("u1", "user", "消える質問"),
            msg("a1", "assistant", &"あ".repeat(8_000)),
            msg("u2", "user", "残る質問"),
        ];
        let summary = format!("要約は残る\n{SUMMARY_DISCLAIMER}");
        // The next kept turn is an assistant, so the summary stays its own turn and trim skips it.
        let prepared = prepare_model_history(&history, &summary, "u0");
        assert_eq!(prepared[0].id, SUMMARY_TURN_ID);
        assert_eq!(prepared[1].role, "assistant");
        let (full, _) = assemble_turns("役割", &[], &prepared, 80_000);
        let summary_tokens = turn_tokens(
            full.iter()
                .find(|turn| turn.content.contains("要約は残る"))
                .unwrap(),
        );
        let keep_tokens = turn_tokens(
            full.iter()
                .find(|turn| turn.content.contains("残る質問"))
                .unwrap(),
        );
        let system_tokens = turn_tokens(&full[0]);
        let budget = system_tokens + summary_tokens + keep_tokens + 8;
        let (turns, stats) = assemble_turns("役割", &[], &prepared, budget);
        assert!(stats.truncated);
        let blob = turns
            .iter()
            .map(|turn| turn.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(blob.contains("要約は残る"), "{blob}");
        assert!(blob.contains("残る質問"), "{blob}");
        assert!(!blob.contains("消える"), "{blob}");
        assert!(!blob.contains("カバーされる"), "{blob}");
        assert!(!turns[0].content.contains("要約は残る"));
    }

    #[test]
    fn summary_is_not_the_question_that_must_fit() {
        let summary = "要".repeat(20_000);
        let history = vec![msg("u1", "user", "質問")];
        let prepared = prepare_model_history(&history, &summary, "u1");
        assert_eq!(prepared.len(), 1);
        assert_eq!(prepared[0].id, SUMMARY_TURN_ID);
        let (_, stats) = assemble_turns("役割", &[], &prepared, 500);
        assert!(!stats.question_exceeds);
        assert!(estimate_tokens(&summary) > 500);
    }

    #[test]
    fn summary_does_not_count_as_a_recent_user_turn() {
        let mut tool = msg("t1", "tool", "ツール本文SECRET");
        tool.tool_call_id = "c1".into();
        tool.tool_name = TOOL_READ_NOTE.into();
        let history = vec![
            msg("u0", "user", "古い"),
            msg("a0", "assistant", "答え"),
            tool,
            msg("u1", "user", "残す質問"),
        ];
        let prepared = prepare_model_history(&history, "要約だけ", "a0");
        let (turns, _) = assemble_turns("役割", &[], &prepared, 80_000);
        let blob = turns
            .iter()
            .map(|turn| turn.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(blob.contains("要約だけ"), "{blob}");
        assert!(blob.contains("残す質問"), "{blob}");
        assert!(!blob.contains("ツール本文SECRET"), "{blob}");
        assert!(!turns[0].content.contains("要約だけ"));
    }

    #[test]
    fn meter_counts_the_summary_and_stubs_instead_of_covered_bodies() {
        let body = "出典本文SECRET".repeat(2_000);
        let sources = vec![source("s1", "u1", 1, &body)];
        let history = three_turns();
        let summary = format!("要点です。\n{SUMMARY_DISCLAIMER}");
        let prepared = prepare_model_history(&history, &summary, "a1");
        let schema = 100;
        let budget = 80_000;
        let draft = "新しい質問";
        let used = meter_usage_tokens("役割", &sources, &prepared, draft, schema, budget);
        let mut with_draft = prepared.clone();
        with_draft.push(msg("draft", "user", draft));
        let (turns, stats) = assemble_turns("役割", &sources, &with_draft, budget);
        let assembled = turns
            .iter()
            .filter(|turn| turn.role != "system")
            .map(turn_tokens)
            .sum::<usize>()
            + schema;
        assert_eq!(used, assembled);
        let blob = turns
            .iter()
            .map(|turn| turn.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(blob.contains(SUMMARY_PREFIX), "{blob}");
        assert!(blob.contains("要点です"), "{blob}");
        assert!(blob.contains("既読 [1] 契約書.docx"), "{blob}");
        assert!(!blob.contains("出典本文SECRET"), "{blob}");
        assert!(!blob.contains("最初の質問本文"), "{blob}");
        assert!(blob.contains("次の質問"), "{blob}");
        assert!(blob.contains("直前の質問"), "{blob}");
        assert!(blob.contains(draft), "{blob}");
        assert!(!stats.question_exceeds);
        let full = meter_usage_tokens("役割", &sources, &history, draft, schema, budget);
        assert!(
            used < full,
            "summary {used} should be under the covered body {full}"
        );
        let limit = 120_000;
        assert_eq!(meter_denominator(limit), 78_000);
        assert_eq!(SUMMARY_TRIGGER_RATIO, 0.8);
        assert_eq!(
            summary_trigger_reached(used, limit),
            (used as f64) / (meter_denominator(limit) as f64) >= 0.8
        );
    }

    #[test]
    fn long_thread_advances_in_fitting_chunks_and_keeps_the_point() {
        let limit = 120_000;
        assert_eq!(
            summary_request_budget(limit),
            limit - reply_headroom_tokens(limit) - SUMMARY_OUTPUT_TOKENS
        );
        let mut history = Vec::new();
        for i in 0..8 {
            history.push(msg(
                &format!("u{i}"),
                "user",
                &format!("質問{i}{}", "あ".repeat(4_000)),
            ));
            history.push(msg(
                &format!("a{i}"),
                "assistant",
                &format!("答え{i}{}", "い".repeat(4_000)),
            ));
        }
        history.push(msg("recent-user", "user", "直近の質問"));
        history.push(msg("latest-user", "user", "今の質問"));
        let budget = summary_request_tokens("") + 900;
        let whole = summary_request_tokens(&summary_job(&history, &[], "", "").unwrap().transcript);
        assert!(whole > budget, "the covered range must not fit in one call");

        let mut summary = String::new();
        let mut through = String::new();
        let mut seen = Vec::new();
        for _ in 0..SUMMARY_CHUNKS_PER_SEND {
            let Some(chunk) = next_summary_chunk(&history, &[], &summary, &through, budget) else {
                break;
            };
            assert!(
                summary_request_tokens(&chunk.transcript) <= budget,
                "chunk {} tokens over {budget}",
                summary_request_tokens(&chunk.transcript)
            );
            assert_ne!(chunk.through_id, through);
            seen.push(chunk.through_id.clone());
            summary = finalize_summary(&format!("ここまで{}", chunk.through_id)).unwrap();
            through = chunk.through_id;
        }
        assert_eq!(seen.len(), SUMMARY_CHUNKS_PER_SEND);
        assert!(seen.iter().all(|id| id != "recent-user" && id != "latest-user"));
        let last_covered = "a7";
        assert_ne!(through, last_covered);
        let again = next_summary_chunk(&history, &[], &summary, &through, budget).unwrap();
        assert_ne!(again.through_id, through);
        assert!(summary_request_tokens(&again.transcript) <= budget);

        let huge = format!("{}TAIL_MARKER", "う".repeat(30_000));
        let clipped_history = vec![
            msg("big", "user", &huge),
            msg("big-a", "assistant", "短い答え"),
            msg("r1", "user", "残す"),
            msg("r2", "user", "今"),
        ];
        let clip_budget = summary_request_tokens("") + 120;
        let chunk = next_summary_chunk(&clipped_history, &[], "", "", clip_budget).unwrap();
        assert_eq!(chunk.through_id, "big");
        assert!(chunk.transcript.contains("…（長いので省略しました）"));
        assert!(!chunk.transcript.contains("TAIL_MARKER"));
        assert!(summary_request_tokens(&chunk.transcript) <= clip_budget);
    }

    #[test]
    fn oversized_summary_is_clipped_and_the_disclaimer_is_kept_once() {
        let raw = format!(
            "{}{SUMMARY_DISCLAIMER}\n{}",
            "あ".repeat(80_000),
            SUMMARY_DISCLAIMER
        );
        let stored = finalize_summary(&raw).unwrap();
        assert!(stored.ends_with(SUMMARY_DISCLAIMER));
        assert_eq!(stored.matches(SUMMARY_DISCLAIMER).count(), 1);
        let body = stored.trim_end_matches(SUMMARY_DISCLAIMER).trim();
        assert!(estimate_tokens(body) <= SUMMARY_OUTPUT_TOKENS);
        assert!(body.chars().count() < 80_000);
        let injected = summary_turn_content(&stored);
        assert_eq!(stored_text_from_turn(&injected), stored);
        assert_eq!(injected.matches(SUMMARY_DISCLAIMER).count(), 1);
        let clipped = clip_injected_summary(&injected, 180);
        assert!(clipped.ends_with(SUMMARY_DISCLAIMER));
        assert_eq!(clipped.matches(SUMMARY_DISCLAIMER).count(), 1);
        assert!(clipped.starts_with(SUMMARY_PREFIX));
        let persisted = stored_text_from_turn(&clipped);
        assert!(persisted.ends_with(SUMMARY_DISCLAIMER));
        assert_eq!(persisted.matches(SUMMARY_DISCLAIMER).count(), 1);
        assert_eq!(stored_text_from_turn(&summary_turn_content(&persisted)), persisted);
        assert!(estimate_tokens(persisted.trim_end_matches(SUMMARY_DISCLAIMER).trim()) < estimate_tokens(body));
    }

    #[test]
    fn long_question_clips_the_request_and_keeps_the_stored_summary() {
        let body = "要".repeat(tokens_to_chars(1_924));
        assert_eq!(estimate_tokens(&body), 1_924);
        let stored = format!("{body}\n{SUMMARY_DISCLAIMER}");
        let question = "問".repeat(45_000);
        assert_eq!(estimate_tokens(&question), 28_125);
        let history = vec![
            msg("u0", "user", "古い質問"),
            msg("a0", "assistant", "古い答え"),
            msg("u1", "user", "途中の質問"),
            msg("a1", "assistant", "途中の答え"),
            msg("q", "user", &question),
        ];
        let prepared = prepare_model_history(&history, &stored, "u1");
        assert_eq!(prepared[0].id, SUMMARY_TURN_ID);
        let system = "役割";
        let required = turn_tokens(&ChatTurn::text("system", system))
            + turn_tokens(&ChatTurn::text("user", &question));
        let split = context_budget(32_768, 0, required).expect("question fits");
        let (turns, stats) = assemble_turns(system, &[], &prepared, split.messages);
        assert!(!stats.question_exceeds);
        assert!(
            stats.total_tokens <= split.messages,
            "{} > {}",
            stats.total_tokens,
            split.messages
        );
        let blob = turns
            .iter()
            .map(|turn| turn.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!blob.contains(SUMMARY_PREFIX), "{blob}");
        assert!(blob.contains(&"問".repeat(100)), "{blob}");
        assert_eq!(summary_saved_after_request(&stored, ""), stored.trim());
        assert!(stored.contains(&"要".repeat(1_000)));
    }

    #[test]
    fn summary_merges_into_the_next_user_so_roles_alternate() {
        let summary = format!("要点です。\n{SUMMARY_DISCLAIMER}");
        let history = three_turns();
        let merged = prepare_model_history(&history, &summary, "a1");
        assert_ne!(merged[0].id, SUMMARY_TURN_ID);
        assert!(merged[0].content.starts_with(SUMMARY_PREFIX));
        assert!(merged[0].content.contains("要点です"));
        assert!(merged[0].content.contains("次の質問"));
        assert_eq!(merged[1].role, "assistant");
        assert!(merged.windows(2).all(|pair| {
            !(pair[0].role == "user" && pair[1].role == "user")
        }));
        let (turns, _) = assemble_turns("役割", &[], &merged, 80_000);
        let roles: Vec<&str> = turns
            .iter()
            .filter(|turn| turn.role != "system")
            .map(|turn| turn.role.as_str())
            .collect();
        assert!(
            roles.windows(2).all(|pair| pair[0] != pair[1] || pair[0] == "tool"),
            "{roles:?}"
        );

        let own = prepare_model_history(&history, &summary, "u1");
        assert_eq!(own[0].id, SUMMARY_TURN_ID);
        assert_eq!(own[0].role, "user");
        assert_eq!(own[1].role, "assistant");
        assert!(own.windows(2).all(|pair| {
            !(pair[0].role == "user" && pair[1].role == "user")
        }));
    }

    #[test]
    fn read_note_stop_advances_past_the_tool_and_leaves_no_orphan() {
        let mut call = msg("a-call", "assistant", "ノートを読みます");
        call.tool_calls = r#"[{"id":"c1","type":"function","function":{"name":"read_note","arguments":"{}"}}]"#.into();
        let mut note = msg("t-note", "tool", "ノート本文SECRET");
        note.tool_call_id = "c1".into();
        note.tool_name = TOOL_READ_NOTE.into();
        let history = vec![
            msg("u0", "user", "最初"),
            call,
            note,
            msg("a-long", "assistant", &"あ".repeat(20_000)),
            msg("u-recent", "user", "残す"),
            msg("u-now", "user", "今"),
        ];
        let head = "ユーザー:\n最初\n\nアシスタント:\nノートを読みます\n\n";
        let budget = summary_request_tokens(head) + 30;
        let chunk = next_summary_chunk(&history, &[], "", "", budget).unwrap();
        assert_eq!(chunk.through_id, "t-note");
        assert!(chunk.transcript.contains("ノートを読みます"), "{}", chunk.transcript);
        assert!(!chunk.transcript.contains("ノート本文SECRET"), "{}", chunk.transcript);
        assert!(!chunk.transcript.contains(&"あ".repeat(80)), "{}", chunk.transcript);
        assert!(summary_request_tokens(&chunk.transcript) <= budget);

        let summary = finalize_summary("要点だけ").unwrap();
        let prepared = prepare_model_history(&history, &summary, &chunk.through_id);
        assert!(prepared.iter().all(|message| message.role != "tool"));
        assert_eq!(prepared[0].id, SUMMARY_TURN_ID);
        assert_eq!(prepared[1].role, "assistant");
        let (turns, _) = assemble_turns("役割", &[], &prepared, 80_000);
        assert_no_orphan_tool(&turns);
        let again = next_summary_chunk(&history, &[], &summary, &chunk.through_id, budget)
            .expect("the cursor moves past the tool result");
        assert_eq!(again.through_id, "a-long");
        assert!(!again.transcript.contains("ノート本文SECRET"));
    }

    #[test]
    fn long_question_keeps_a_shrunk_summary_in_the_spare_room() {
        let body = "要".repeat(tokens_to_chars(1_924));
        assert_eq!(estimate_tokens(&body), 1_924);
        let stored = format!("{body}\n{SUMMARY_DISCLAIMER}");
        let question = "問".repeat(32_000);
        let history = vec![
            msg("u0", "user", "古い質問"),
            msg("a1", "assistant", "古い答え"),
            msg("u-mid", "user", "残す質問"),
            msg("a-mid", "assistant", "途中の答え"),
            msg("q", "user", &question),
        ];
        let system = "役割";
        let required = turn_tokens(&ChatTurn::text("system", system))
            + turn_tokens(&ChatTurn::text("user", &question));
        let split = context_budget(32_768, 0, required).expect("question fits");
        let spare = split.messages.saturating_sub(required);
        assert_eq!(spare, 1_290, "message budget {}", split.messages);
        let prepared = prepare_model_history(&history, &stored, "a1");
        assert!(prepared[0].content.contains("残す質問"));
        let (turns, stats) = assemble_turns(system, &[], &prepared, split.messages);
        assert!(!stats.question_exceeds);
        assert!(stats.total_tokens <= split.messages, "{}", stats.total_tokens);
        let blob = turns
            .iter()
            .map(|turn| turn.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(blob.contains(SUMMARY_PREFIX), "{blob}");
        assert!(blob.contains(SUMMARY_DISCLAIMER), "{blob}");
        assert!(blob.contains(&"要".repeat(100)), "{blob}");
        assert!(blob.contains(&"問".repeat(100)), "{blob}");
        let request = turns
            .iter()
            .find(|turn| turn.content.starts_with(SUMMARY_PREFIX))
            .expect("summary stays on the kept user");
        assert_eq!(summary_saved_after_request(&stored, &request.content), stored.trim());
        let roles: Vec<&str> = turns
            .iter()
            .filter(|turn| turn.role != "system")
            .map(|turn| turn.role.as_str())
            .collect();
        assert!(roles.windows(2).all(|pair| pair[0] != pair[1]), "{roles:?}");

        let folded = fold_summary(&history, &stored, "a1", SummaryAttempt::Failed);
        assert!(folded.failed);
        let (again, again_stats) = assemble_turns(system, &[], &folded.messages, split.messages);
        assert!(!again_stats.question_exceeds);
        assert!(again_stats.total_tokens <= split.messages);
        let failed_blob = again
            .iter()
            .map(|turn| turn.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(failed_blob.contains(SUMMARY_PREFIX), "{failed_blob}");
        assert!(failed_blob.contains(SUMMARY_DISCLAIMER), "{failed_blob}");
        assert!(failed_blob.contains(&"要".repeat(100)), "{failed_blob}");
        assert_eq!(folded.summary, stored);
    }

    #[test]
    fn notice_between_summary_and_user_does_not_stack_users_and_the_cursor_advances() {
        let mut empty = msg(
            "n-empty",
            "assistant",
            "モデルが本文も操作も返さずに終わりました（終了理由: stop）。",
        );
        empty.kind = LLM_MESSAGE_NOTICE.into();
        let mut limited = msg(
            "n-limit",
            "assistant",
            "ツール往復が上限の 8 回に達したため、残りの操作はしていません。",
        );
        limited.kind = LLM_MESSAGE_NOTICE.into();
        let history = vec![
            msg("u0", "user", "古い"),
            msg("a1", "assistant", "答え"),
            empty.clone(),
            limited.clone(),
            msg("u1", "user", "次の質問"),
            msg("a2", "assistant", "次の答え"),
            msg("u2", "user", "今の質問"),
        ];
        let summary = format!("要点です。\n{SUMMARY_DISCLAIMER}");
        let prepared = prepare_model_history(&history, &summary, "a1");
        let replayed: Vec<&LlmMessageRow> = prepared.iter().filter(|message| message_is_replayed(message)).collect();
        assert!(replayed[0].content.starts_with(SUMMARY_PREFIX));
        assert!(replayed[0].content.contains("次の質問"));
        assert!(replayed.windows(2).all(|pair| {
            !(pair[0].role == "user" && pair[1].role == "user")
        }));
        let (turns, _) = assemble_turns("役割", &[], &prepared, 80_000);
        let roles: Vec<&str> = turns
            .iter()
            .filter(|turn| turn.role != "system")
            .map(|turn| turn.role.as_str())
            .collect();
        assert!(roles.windows(2).all(|pair| pair[0] != pair[1]), "{roles:?}");
        let blob = turns
            .iter()
            .map(|turn| turn.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(blob.contains("要点です"), "{blob}");
        assert!(!blob.contains("終わりました"), "{blob}");
        assert!(!blob.contains("ツール往復"), "{blob}");

        let covered = vec![
            msg("u0", "user", "最初"),
            empty,
            limited,
            msg("a-long", "assistant", &"あ".repeat(20_000)),
            msg("u-recent", "user", "残す"),
            msg("u-now", "user", "今"),
        ];
        let head = "ユーザー:\n最初\n\n";
        let budget = summary_request_tokens(head) + 40;
        let chunk = next_summary_chunk(&covered, &[], "", "", budget).unwrap();
        assert_eq!(chunk.through_id, "n-limit");
        assert!(chunk.transcript.contains("最初"), "{}", chunk.transcript);
        assert!(!chunk.transcript.contains("終わりました"), "{}", chunk.transcript);
        assert!(!chunk.transcript.contains("ツール往復"), "{}", chunk.transcript);
        assert!(!chunk.transcript.contains(&"あ".repeat(80)), "{}", chunk.transcript);
        let next = next_summary_chunk(&covered, &[], "要点", &chunk.through_id, budget);
        assert!(next.is_none() || next.unwrap().through_id != "n-empty");
    }

    #[test]
    fn notice_and_unanswered_user_never_leave_two_user_turns() {
        let mut notice = msg(
            "n-empty",
            "assistant",
            "モデルが本文も操作も返さずに終わりました（終了理由: stop）。",
        );
        notice.kind = LLM_MESSAGE_NOTICE.into();
        let history = vec![
            msg("u1", "user", "最初"),
            msg("a1", "assistant", "一つ目"),
            msg("u2", "user", "次"),
            msg("a2", "assistant", "二つ目"),
            msg("u3", "user", "未回答"),
            notice,
            msg("u4", "user", "続き"),
        ];
        let (turns, _) = assemble_turns("役割", &[], &history, 131_072);
        let roles: Vec<&str> = turns
            .iter()
            .filter(|turn| turn.role != "system")
            .map(|turn| turn.role.as_str())
            .collect();
        assert_eq!(roles, ["user", "assistant", "user", "assistant", "user"]);
        let last = turns.last().unwrap();
        assert!(last.content.contains("未回答"), "{}", last.content);
        assert!(last.content.contains("続き"), "{}", last.content);
        assert!(turns.iter().all(|turn| !turn.content.contains("終わりました")));
    }

    #[test]
    fn dropped_summary_host_does_not_leave_adjacent_users() {
        let body = "要".repeat(tokens_to_chars(1_924));
        let stored = format!("{body}\n{SUMMARY_DISCLAIMER}");
        for (limit, question_chars) in [(32_768usize, 32_000usize), (16_384, 15_000)] {
            let question = "問".repeat(question_chars);
            let history = vec![
                msg("u0", "user", "古い"),
                msg("a1", "assistant", "答え"),
                msg("u-host", "user", "ホスト"),
                msg("u-next", "user", "隣の質問"),
                msg("q", "user", &question),
            ];
            let system = "役割";
            let required = turn_tokens(&ChatTurn::text("system", system))
                + turn_tokens(&ChatTurn::text("user", &question));
            let split = context_budget(limit, 0, required).expect("question fits");
            let prepared = prepare_model_history(&history, &stored, "a1");
            let (turns, stats) = assemble_turns(system, &[], &prepared, split.messages);
            assert!(!stats.question_exceeds, "{limit}");
            assert!(stats.total_tokens <= split.messages, "{limit} {}", stats.total_tokens);
            let roles: Vec<&str> = turns
                .iter()
                .filter(|turn| turn.role != "system")
                .map(|turn| turn.role.as_str())
                .collect();
            assert!(
                roles.windows(2).all(|pair| pair[0] != pair[1]),
                "{limit} {roles:?}"
            );
            let blob = turns
                .iter()
                .map(|turn| turn.content.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            assert!(blob.contains(SUMMARY_PREFIX), "{limit} {blob}");
            assert!(blob.contains("隣の質問"), "{limit} {blob}");
            assert!(blob.contains(&"問".repeat(40)), "{limit}");
            assert_eq!(summary_saved_after_request(&stored, &blob), stored.trim());

            let folded = fold_summary(&history, &stored, "a1", SummaryAttempt::Failed);
            let (again, again_stats) = assemble_turns(system, &[], &folded.messages, split.messages);
            assert!(again_stats.total_tokens <= split.messages, "{limit}");
            let again_roles: Vec<&str> = again
                .iter()
                .filter(|turn| turn.role != "system")
                .map(|turn| turn.role.as_str())
                .collect();
            assert!(
                again_roles.windows(2).all(|pair| pair[0] != pair[1]),
                "failure {limit} {again_roles:?}"
            );
            assert!(again.iter().any(|turn| turn.content.contains(SUMMARY_PREFIX)));
        }
    }

    #[test]
    fn minimum_summary_that_does_not_fit_is_omitted_from_the_request() {
        let body = "要".repeat(tokens_to_chars(1_924));
        let stored = format!("{body}\n{SUMMARY_DISCLAIMER}");
        let question = "問".repeat(36_000);
        let history = vec![
            msg("u0", "user", "古い"),
            msg("a1", "assistant", "答え"),
            msg("u-mid", "user", "途中"),
            msg("a-mid", "assistant", "途中の答え"),
            msg("q", "user", &question),
        ];
        let system = "役割";
        let required = turn_tokens(&ChatTurn::text("system", system))
            + turn_tokens(&ChatTurn::text("user", &question));
        let split = context_budget(32_768, 0, required).expect("question fits");
        assert_eq!(split.messages, 22_510);
        let prepared = prepare_model_history(&history, &stored, "a1");
        let (turns, stats) = assemble_turns(system, &[], &prepared, split.messages);
        assert!(!stats.question_exceeds);
        assert!(stats.total_tokens <= split.messages, "{}", stats.total_tokens);
        let chars: usize = turns.iter().map(|turn| turn.content.chars().count()).sum();
        let blob = turns
            .iter()
            .map(|turn| turn.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!blob.contains(SUMMARY_PREFIX), "{blob}");
        assert!(!blob.contains(SUMMARY_DISCLAIMER), "{blob}");
        assert!(blob.contains(&"問".repeat(80)), "{chars}");
        assert_eq!(summary_saved_after_request(&stored, ""), stored.trim());

        let mut sources = Vec::new();
        for n in 1..=12 {
            let mut row = source(&format!("s{n}"), "u0", n, "出典本文SECRET");
            row.path = format!(r"C:\docs\資料{n}.docx");
            sources.push(row);
        }
        let wide = "問".repeat(33_800);
        let wide_history = vec![
            msg("u0", "user", "古い"),
            msg("a1", "assistant", "答え"),
            msg("a-keep", "assistant", "残る答え"),
            msg("u-recent", "user", "残す"),
            msg("q", "user", &wide),
        ];
        let wide_required = turn_tokens(&ChatTurn::text("system", system))
            + turn_tokens(&ChatTurn::text("user", &wide));
        let wide_split = context_budget(32_768, 0, wide_required).unwrap();
        assert_eq!(wide_split.messages, 21_300);
        let wide_prepared = prepare_model_history(&wide_history, &stored, "a1");
        let (wide_turns, wide_stats) =
            assemble_turns(system, &sources, &wide_prepared, wide_split.messages);
        assert!(!wide_stats.question_exceeds);
        assert!(
            wide_stats.total_tokens <= wide_split.messages,
            "{}",
            wide_stats.total_tokens
        );
        let wide_blob = wide_turns
            .iter()
            .map(|turn| turn.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!wide_blob.contains(SUMMARY_PREFIX), "{wide_blob}");
        assert!(!wide_blob.contains("既読 ["), "{wide_blob}");
        assert!(!wide_blob.contains("出典本文SECRET"), "{wide_blob}");
        assert!(wide_blob.contains(&"問".repeat(40)), "{wide_blob}");
        assert_eq!(summary_saved_after_request(&stored, &wide_blob), stored.trim());
    }

    #[test]
    fn read_stub_moves_with_the_summary_when_the_host_turn_is_dropped() {
        let body = "要".repeat(tokens_to_chars(1_924));
        let stored = format!("{body}\n{SUMMARY_DISCLAIMER}");
        let question = "問".repeat(32_000);
        let history = vec![
            msg("u0", "user", "古い"),
            msg("a1", "assistant", "答え"),
            msg("u-mid", "user", "途中"),
            msg("a-mid", "assistant", "途中の答え"),
            msg("q", "user", &question),
        ];
        let sources = vec![source("s1", "u0", 1, "出典本文SECRET")];
        let system = "役割";
        let required = turn_tokens(&ChatTurn::text("system", system))
            + turn_tokens(&ChatTurn::text("user", &question));
        let split = context_budget(32_768, 0, required).unwrap();
        let prepared = prepare_model_history(&history, &stored, "a1");
        let (turns, stats) = assemble_turns(system, &sources, &prepared, split.messages);
        assert!(stats.total_tokens <= split.messages, "{}", stats.total_tokens);
        let blob = turns
            .iter()
            .map(|turn| turn.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(blob.contains(SUMMARY_PREFIX), "{blob}");
        assert!(blob.contains("既読 [1] 契約書.docx"), "{blob}");
        assert!(!blob.contains("途中"), "{blob}");
        assert!(!blob.contains("出典本文SECRET"), "{blob}");
        assert_eq!(summary_saved_after_request(&stored, &blob), stored.trim());
    }

    fn assert_no_orphan_tool(turns: &[ChatTurn]) {
        let mut open = false;
        for turn in turns {
            if turn.role == "tool" {
                assert!(open, "orphan tool turn");
            }
            open = (turn.role == "assistant" && turn.tool_calls.is_some()) || (turn.role == "tool" && open);
        }
    }
}
