//! 「ノートへ」 writes a summary of the conversation so far into a note.
//! The write is an ordinary AI edit: it is final, versioned, and marked.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use crate::db::{Db, LlmMessageRow, NoteRow};
use crate::llm::context::ChatTurn;
use crate::state::AppState;

pub const SUMMARY_HEADING: &str = "会話の要約";
const SUMMARY_MAX_TOKENS: u32 = 1_500;

pub fn transcript_through(
    messages: &[LlmMessageRow],
    through_id: Option<&str>,
) -> Result<String, String> {
    let through_id = through_id.map(str::trim).filter(|s| !s.is_empty());
    let mut lines = Vec::new();
    let mut saw = through_id.is_none();
    for message in messages {
        if message.kind == crate::db::LLM_MESSAGE_NOTICE {
            continue;
        }
        if message.role != "user" && message.role != "assistant" {
            continue;
        }
        let text = message.content.trim();
        if text.is_empty() {
            if through_id == Some(message.id.as_str()) {
                saw = true;
                break;
            }
            continue;
        }
        let who = if message.role == "user" {
            "利用者"
        } else {
            "回答"
        };
        lines.push(format!("{who}: {text}"));
        if through_id == Some(message.id.as_str()) {
            saw = true;
            break;
        }
    }
    if !saw {
        return Err("その回答が会話にありません。".into());
    }
    if lines.is_empty() {
        return Err("要約する会話がありません。".into());
    }
    Ok(lines.join("\n\n"))
}

pub fn summary_prompt(transcript: &str) -> String {
    format!(
        "次の会話を、ノートに残す要約にしてください。\n\
         \n\
         - 日本語の Markdown で書く\n\
         - 前置きや断り書きは書かない。本文だけを返す\n\
         - 日付、金額、当事者名、法令名、条番号は、会話の表記のまま残す\n\
         - 見出しと箇条書きで短くまとめる\n\
         \n\
         会話:\n{transcript}"
    )
}

pub fn clean_summary(raw: &str) -> Result<String, String> {
    let mut text = raw.trim();
    if let Some(rest) = text.strip_prefix("```") {
        let rest = rest.trim_start();
        let rest = rest
            .strip_prefix("markdown")
            .or_else(|| rest.strip_prefix("md"))
            .unwrap_or(rest)
            .trim_start();
        text = rest.strip_suffix("```").unwrap_or(rest).trim();
    }
    let text = text.trim();
    if text.is_empty() {
        return Err("要約が空です。".into());
    }
    Ok(text.to_string())
}

pub fn summary_section(body: &str) -> String {
    format!("## {SUMMARY_HEADING}\n\n{}\n", body.trim())
}

fn append_summary(memo: &str, section: &str) -> Result<String, String> {
    if memo.trim().is_empty() {
        return Ok(section.trim_start_matches('\n').to_string());
    }
    crate::notes_md::append_chunk(memo, None, section.trim_end()).map_err(|e| e.message(""))
}

pub fn write_summary_memo(db: &Db, note_id: &str, summary: &str) -> Result<NoteRow, String> {
    let body = clean_summary(summary)?;
    let note = db
        .get_note(note_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "ノートが見つかりません".to_string())?;
    let next = append_summary(&note.memo, &summary_section(&body))?;
    db.apply_direct_note_text(&note.id, &next, None, "ai")
}

pub async fn summarize_into_note(
    state: &AppState,
    thread_id: &str,
    note_id: &str,
    through_message_id: Option<&str>,
) -> Result<NoteRow, String> {
    let messages = state
        .db
        .list_llm_messages(thread_id)
        .map_err(|e| e.to_string())?;
    let transcript = transcript_through(&messages, through_message_id)?;
    let settings = state.settings.read().clone();
    let turns = vec![ChatTurn::text("user", summary_prompt(&transcript))];
    let cancel = Arc::new(AtomicBool::new(false));
    let outcome = crate::llm::stream_chat(
        &settings,
        &turns,
        cancel,
        None,
        crate::llm::runaway_chars_for(&settings.llm_base_url),
        Some(false),
        Some(SUMMARY_MAX_TOKENS),
        |_kind, _delta| {},
    )
    .await?;
    let note_id = resolve_summary_note(&state.db, thread_id, note_id)?;
    write_summary_memo(&state.db, &note_id, &outcome.content)
}

fn resolve_summary_note(db: &Db, thread_id: &str, note_id: &str) -> Result<String, String> {
    let note_id = note_id.trim();
    if note_id.is_empty() {
        return Err("送り先のノートがありません。".into());
    }
    if note_id == "new" {
        let title = db
            .get_llm_thread(thread_id)
            .map_err(|e| e.to_string())?
            .map(|t| t.title)
            .unwrap_or_default();
        let title = if title.trim().is_empty() {
            SUMMARY_HEADING
        } else {
            title.trim()
        };
        let note = db.create_note(title).map_err(|e| e.to_string())?;
        db.set_active_note_id(Some(&note.id))
            .map_err(|e| e.to_string())?;
        return Ok(note.id);
    }
    db.get_note(note_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "ノートが見つかりません".to_string())?;
    Ok(note_id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(id: &str, role: &str, content: &str, kind: &str) -> LlmMessageRow {
        LlmMessageRow {
            id: id.into(),
            thread_id: "t".into(),
            role: role.into(),
            content: content.into(),
            reasoning: String::new(),
            tool_calls: String::new(),
            tool_call_id: String::new(),
            tool_name: String::new(),
            kind: kind.into(),
            original_content: String::new(),
            script_fix: String::new(),
            carries_summary: false,
            created_at: 1,
        }
    }

    #[test]
    fn transcript_keeps_the_conversation_up_to_the_answer_and_skips_tools() {
        let messages = vec![
            message("u1", "user", "解除の要件は？", ""),
            message("tool", "tool", "検索結果", ""),
            message("a1", "assistant", "重大な不履行のときです。", ""),
            message("n", "assistant", "更新しました", crate::db::LLM_MESSAGE_NOTICE),
            message("u2", "user", "この後の質問", ""),
        ];
        let text = transcript_through(&messages, Some("a1")).unwrap();
        assert!(text.contains("利用者: 解除の要件は？"), "{text}");
        assert!(text.contains("回答: 重大な不履行のときです。"), "{text}");
        assert!(!text.contains("検索結果"), "{text}");
        assert!(!text.contains("この後の質問"), "{text}");
        assert!(!text.contains("更新しました"), "{text}");
        assert!(transcript_through(&messages, Some("missing")).is_err());
    }

    #[test]
    fn clean_summary_drops_a_fence_and_rejects_empty() {
        assert_eq!(
            clean_summary("```markdown\n要点です。\n```").unwrap(),
            "要点です。"
        );
        assert!(clean_summary("  \n").is_err());
    }

    #[test]
    fn writing_a_summary_appends_an_ai_version() {
        let dir = std::env::temp_dir().join(format!(
            "argos-sum-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("argos.db")).unwrap();
        let note = db.create_note("事件").unwrap();
        db.update_note_memo(&note.id, "既存のメモ").unwrap();
        let saved = write_summary_memo(&db, &note.id, "重大な不履行なら解除できる。").unwrap();
        assert!(saved.memo.contains("既存のメモ"), "{}", saved.memo);
        assert!(saved.memo.contains("## 会話の要約"), "{}", saved.memo);
        assert!(
            saved.memo.contains("重大な不履行なら解除できる。"),
            "{}",
            saved.memo
        );
        let history = db.list_note_history(&note.id).unwrap().unwrap();
        assert_eq!(history.versions.last().unwrap().source, "ai");
        assert!(history.ai_updated_at.is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
