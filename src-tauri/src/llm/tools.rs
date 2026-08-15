//! LLM tool-calling: index search and unit preview.

use serde_json::{json, Value};

use crate::db::LlmSourceRow;
use crate::llm::context::format_sources;
use crate::search::{self, SearchHit};
use crate::state::AppState;

pub const TOOL_SEARCH: &str = "search_index";
pub const TOOL_READ: &str = "read_unit";
pub const MAX_TOOL_ROUNDS: usize = 3;
const TOOL_BODY_CAP: usize = 1_200;

pub fn tools_schema() -> Value {
    json!([
        {
            "type": "function",
            "function": {
                "name": TOOL_SEARCH,
                "description": "Argosの索引を検索する（ファイルとメール）。添付出典で足りるときは呼ばない。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "検索語" },
                        "path_prefix": {
                            "type": "string",
                            "description": "フォルダパスで結果を絞る（任意）"
                        },
                        "k": { "type": "integer", "description": "件数（1〜8）" }
                    },
                    "required": ["query"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": TOOL_READ,
                "description": "段落IDの本文を索引から読む。search_indexのヒットを詳しく読むときに使う。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "paragraph_id": { "type": "string" }
                    },
                    "required": ["paragraph_id"]
                }
            }
        }
    ])
}

fn cap_chars(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    let take = max.saturating_sub(1);
    let mut out: String = s.chars().take(take).collect();
    out.push('…');
    out
}

fn hit_body(hit: &SearchHit) -> String {
    let preview = hit.preview_text.trim();
    let snippet = hit.snippet.trim();
    let body = if preview.len() >= snippet.len() {
        preview
    } else if !snippet.is_empty() {
        snippet
    } else {
        preview
    };
    cap_chars(body, TOOL_BODY_CAP)
}

pub struct ToolExec {
    pub content: String,
    pub consumed: Vec<(String, i64)>,
}

fn already_line(row: &LlmSourceRow) -> String {
    let n = if row.cite_no > 0 { row.cite_no } else { 0 };
    let title = if row.title.trim().is_empty() {
        row.path.as_str()
    } else {
        row.title.as_str()
    };
    format!("既読 [{n}] {title}")
}

fn looks_thin(body: &str, snippet: &str) -> bool {
    let n = body.chars().count();
    n < 80 || (!snippet.is_empty() && body.trim() == snippet.trim())
}

/// Search hits often carry a short snippet until preview is loaded.
fn enrich_hit(state: &AppState, hit: &SearchHit) -> SearchHit {
    let body = hit_body(hit);
    if !looks_thin(&body, &hit.snippet) || hit.id.trim().is_empty() {
        return hit.clone();
    }
    match preview_hit(state, &hit.id) {
        Ok(Some(preview)) => {
            let p = hit_body(&preview);
            if p.chars().count() > body.chars().count() {
                let mut out = hit.clone();
                out.preview_text = preview.preview_text;
                if out.title.trim().is_empty() {
                    out.title = preview.title;
                }
                out
            } else {
                hit.clone()
            }
        }
        _ => hit.clone(),
    }
}

fn persist_hit(
    state: &AppState,
    thread_id: &str,
    hit: &SearchHit,
    query: &str,
    next_cite: &mut i64,
    new_rows: &mut Vec<LlmSourceRow>,
    already: &mut Vec<String>,
    consumed: &mut Vec<(String, i64)>,
) -> Result<(), String> {
    if let Some(cited) = state
        .db
        .find_cited_llm_source(thread_id, &hit.path, &hit.id)
        .map_err(|e| e.to_string())?
    {
        already.push(already_line(&cited));
        return Ok(());
    }
    let hit = enrich_hit(state, hit);
    let body = hit_body(&hit);
    if body.trim().is_empty() {
        return Ok(());
    }
    let title = if hit.title.trim().is_empty() {
        hit.path.as_str()
    } else {
        hit.title.as_str()
    };
    let (mut row, created) = state
        .db
        .insert_llm_source(
            thread_id,
            "tool",
            &hit.path,
            title,
            &hit.id,
            &body,
            query,
        )
        .map_err(|e| e.to_string())?;
    if created || row.cite_no <= 0 {
        *next_cite += 1;
        row.cite_no = *next_cite;
        state
            .db
            .set_llm_source_cite_no(&row.id, row.cite_no)
            .map_err(|e| e.to_string())?;
    }
    consumed.push((row.id.clone(), row.cite_no));
    new_rows.push(row);
    Ok(())
}

fn run_index_search(
    state: &AppState,
    query: &str,
    path_prefix: Option<&str>,
    k: usize,
) -> Result<Vec<SearchHit>, String> {
    let settings = state.settings.read().clone();
    let user_dict = state.user_dict.read().clone();
    search::run_search_with_mail_options(
        &settings,
        state.backend.as_ref(),
        Some(state.mail_backend.as_ref()),
        query,
        k,
        path_prefix,
        None,
        &user_dict,
    )
}

fn preview_hit(state: &AppState, paragraph_id: &str) -> Result<Option<SearchHit>, String> {
    let settings = state.settings.read().clone();
    search::run_preview(
        &settings,
        state.backend.as_ref(),
        Some(state.mail_backend.as_ref()),
        paragraph_id,
    )
}

pub fn execute_tool(
    state: &AppState,
    thread_id: &str,
    name: &str,
    arguments: &str,
    next_cite: &mut i64,
) -> ToolExec {
    match execute_tool_inner(state, thread_id, name, arguments, next_cite) {
        Ok(exec) => exec,
        Err(e) => ToolExec {
            content: format!("ツールエラー: {e}"),
            consumed: Vec::new(),
        },
    }
}

fn execute_tool_inner(
    state: &AppState,
    thread_id: &str,
    name: &str,
    arguments: &str,
    next_cite: &mut i64,
) -> Result<ToolExec, String> {
    let args: Value = serde_json::from_str(arguments).unwrap_or_else(|_| json!({}));
    match name {
        TOOL_SEARCH => {
            let query = args
                .get("query")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            if query.is_empty() {
                return Ok(ToolExec {
                    content: "query が空です。".into(),
                    consumed: Vec::new(),
                });
            }
            let prefix = args
                .get("path_prefix")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let settings_k = state.settings.read().llm_search_top_k.clamp(1, 8) as usize;
            let k = args
                .get("k")
                .and_then(|v| v.as_u64())
                .map(|n| (n as usize).clamp(1, 8))
                .unwrap_or(settings_k);
            let hits = run_index_search(state, &query, prefix, k)?;
            if hits.is_empty() {
                return Ok(ToolExec {
                    content: format!("「{query}」に一致する索引ヒットはありません。"),
                    consumed: Vec::new(),
                });
            }
            let mut new_rows = Vec::new();
            let mut already = Vec::new();
            let mut consumed = Vec::new();
            for hit in &hits {
                persist_hit(
                    state,
                    thread_id,
                    hit,
                    &query,
                    next_cite,
                    &mut new_rows,
                    &mut already,
                    &mut consumed,
                )?;
            }
            let mut content = String::new();
            if !new_rows.is_empty() {
                content.push_str(&format_sources(&new_rows));
            }
            if !already.is_empty() {
                if !content.is_empty() {
                    content.push('\n');
                }
                content.push_str(&already.join("\n"));
            }
            if content.trim().is_empty() {
                content = "使える本文のあるヒットはありませんでした。".into();
            }
            Ok(ToolExec { content, consumed })
        }
        TOOL_READ => {
            let paragraph_id = args
                .get("paragraph_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            if paragraph_id.is_empty() {
                return Ok(ToolExec {
                    content: "paragraph_id が空です。".into(),
                    consumed: Vec::new(),
                });
            }
            let Some(hit) = preview_hit(state, &paragraph_id)? else {
                return Ok(ToolExec {
                    content: format!("段落 {paragraph_id} は索引にありません。"),
                    consumed: Vec::new(),
                });
            };
            let mut new_rows = Vec::new();
            let mut already = Vec::new();
            let mut consumed = Vec::new();
            persist_hit(
                state,
                thread_id,
                &hit,
                "",
                next_cite,
                &mut new_rows,
                &mut already,
                &mut consumed,
            )?;
            let mut content = String::new();
            if !new_rows.is_empty() {
                content.push_str(&format_sources(&new_rows));
            }
            if !already.is_empty() {
                content.push_str(&already.join("\n"));
            }
            if content.trim().is_empty() {
                content = "本文が空です。".into();
            }
            Ok(ToolExec { content, consumed })
        }
        other => Ok(ToolExec {
            content: format!("未知のツールです: {other}"),
            consumed: Vec::new(),
        }),
    }
}
