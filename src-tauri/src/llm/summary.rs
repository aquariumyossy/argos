//! One stored summary per thread. It replaces covered messages in the model request.
//! The original messages stay in the database.

use crate::db::{LlmMessageRow, LlmSourceRow};
use crate::llm::context::{
    message_is_replayed, summary_stub_line, summary_stubs_for_tool, ChatTurn, SUMMARY_TURN_ID,
};

pub const SUMMARY_PREFIX: &str = "【ここまでの要約】";
pub const SUMMARY_DISCLAIMER: &str =
    "この要約には誤りが含まれることがあります。重要な日付・金額・当事者・条番号は原文で確認してください。";
pub const SUMMARY_STATUS_TEXT: &str = "会話が長くなったため、古いやり取りを要約しています…";
pub const SUMMARY_FAILURE_WARNING: &str = "要約できなかったため、古い部分を省いて送りました";

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
    }
}

pub fn finalize_summary(raw: &str) -> Result<String, String> {
    let text = raw.trim().trim_start_matches(SUMMARY_PREFIX).trim();
    if text.is_empty() {
        return Err("要約が空です".into());
    }
    let mut text = text.to_string();
    if !text.ends_with(SUMMARY_DISCLAIMER) {
        if !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(SUMMARY_DISCLAIMER);
    }
    Ok(text)
}

pub fn summary_turn_content(summary: &str) -> String {
    format!("{SUMMARY_PREFIX}\n{}", summary.trim())
}

pub struct SummaryJob {
    pub through_id: String,
    pub transcript: String,
}

/// Messages before the two most recent user turns. `None` when that slice has nothing new.
pub fn summary_job(
    history: &[LlmMessageRow],
    sources: &[LlmSourceRow],
    previous_summary: &str,
    previous_through_id: &str,
) -> Option<SummaryJob> {
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
    let through_id = covered.last()?.id.clone();
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
    if !fresh.iter().any(message_is_replayed) {
        return None;
    }
    let mut transcript = String::new();
    if !previous_summary.is_empty() {
        transcript.push_str("これまでの要約:\n");
        transcript.push_str(previous_summary);
        transcript.push_str("\n\n");
    }
    transcript.push_str(&render_summary_input(fresh, sources));
    let transcript = transcript.trim().to_string();
    if transcript.is_empty() {
        return None;
    }
    Some(SummaryJob {
        through_id,
        transcript,
    })
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

/// Replace messages through `through_id` with one summary turn. Later messages stay.
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
    let mut out = Vec::with_capacity(history.len() - pos);
    out.push(LlmMessageRow {
        id: SUMMARY_TURN_ID.into(),
        thread_id: history[pos].thread_id.clone(),
        role: "user".into(),
        content: summary_turn_content(summary),
        created_at: history[pos].created_at,
        reasoning: String::new(),
        tool_calls: String::new(),
        tool_call_id: String::new(),
        tool_name: String::new(),
        kind: String::new(),
    });
    out.extend(history[pos + 1..].iter().cloned());
    out
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
    use crate::llm::context::{
        assemble_turns, estimate_tokens, meter_denominator, meter_usage_tokens,
        summary_trigger_reached, turn_tokens, SUMMARY_TRIGGER_RATIO,
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
        assert!(folded
            .messages
            .iter()
            .any(|message| message.content == "次の質問"));
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
        let prepared = prepare_model_history(&history, &summary, "a0");
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
}
