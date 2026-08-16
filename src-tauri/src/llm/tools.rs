//! LLM tool-calling: index search and unit preview.

use std::collections::HashSet;

use serde_json::{json, Value};

use crate::db::LlmSourceRow;
use crate::llm::context::format_sources;
use crate::search::{self, SearchHit};
use crate::state::AppState;

pub const TOOL_SEARCH: &str = "search_index";
pub const TOOL_READ: &str = "read_unit";
pub const MAX_TOOL_ROUNDS: usize = 3;
/// Body length per search hit. Deliberately small: a search round returns up to `k`
/// hits, and the model is expected to call `read_unit` on the one it actually needs.
const TOOL_BODY_CAP: usize = 1_200;
/// `read_unit` is a deliberate request for one specific paragraph, so it may be longer.
const READ_BODY_CAP: usize = 6_000;
/// Paragraphs of the same file returned per search round. A statute file holds hundreds
/// of articles and a contract many clauses, so one unit per file loses the rest.
const UNITS_PER_FILE: usize = 3;
/// Cap on simultaneous thread folder scopes (each prefix is a separate index search).
pub const MAX_THREAD_SCOPES: usize = 8;

pub fn tools_schema() -> Value {
    json!([
        {
            "type": "function",
            "function": {
                "name": TOOL_SEARCH,
                "description": "Argosの索引を検索する（ファイルとメール）。添付出典で足りるときは呼ばない。\
クエリは調べたい語だけを空白区切りで並べる（例: 『解雇 有効性 裁判例』）。\
「〜を教えて」「〜について調べて」のような文ではなく単語で指定する。\
条文を引くときは『民法 第555条』のように法令名と条番号を書く。\
0件のときは語を減らすか言い換えて1回だけ試す。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "検索語（単語を空白区切り。\"...\" で完全一致、-語 で除外）" },
                        "path_prefix": {
                            "type": "string",
                            "description": "フォルダパスで結果を絞る（任意）。スレッドに検索範囲が設定されている場合、その配下への絞り込みだけが有効。"
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
                "description": "段落IDの本文を索引から読む。出典に (paragraph_id: ...) と示された段落や、\
末尾が […] で切れている出典の続きを読むときに使う。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "paragraph_id": { "type": "string" },
                        "with_neighbors": {
                            "type": "boolean",
                            "description": "前後の段落も併せて読む（既定 false）"
                        }
                    },
                    "required": ["paragraph_id"]
                }
            }
        }
    ])
}

/// Marker the tool description points at, so the model knows `read_unit` can fetch the
/// rest instead of answering from a sentence that was cut mid-way.
const TRUNCATED_MARK: &str = " […]";

fn cap_chars(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push_str(TRUNCATED_MARK);
    out
}

fn hit_body_capped(hit: &SearchHit, cap: usize) -> String {
    let preview = hit.preview_text.trim();
    let snippet = hit.snippet.trim();
    let body = if preview.len() >= snippet.len() {
        preview
    } else if !snippet.is_empty() {
        snippet
    } else {
        preview
    };
    cap_chars(body, cap)
}

fn hit_body(hit: &SearchHit) -> String {
    hit_body_capped(hit, TOOL_BODY_CAP)
}

pub struct ToolExec {
    pub content: String,
    pub consumed: Vec<(String, i64)>,
}

/// A hit already cited earlier in the thread is not re-sent in full. Carry a short
/// snippet anyway: context assembly may have evicted the original body, in which case a
/// bare title would leave the model with nothing to work from.
fn already_line(row: &LlmSourceRow) -> String {
    let n = if row.cite_no > 0 { row.cite_no } else { 0 };
    let title = if row.title.trim().is_empty() {
        row.path.as_str()
    } else {
        row.title.as_str()
    };
    let pid = row.paragraph_id.trim();
    let head: String = row.body.trim().chars().take(120).collect();
    let mut line = format!("既読 [{n}] {title}");
    if !pid.is_empty() {
        line.push_str(&format!(" (paragraph_id: {pid})"));
    }
    if !head.is_empty() {
        line.push_str(&format!("\n  {head}…"));
    }
    line
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

#[allow(clippy::too_many_arguments)]
fn persist_hit(
    state: &AppState,
    thread_id: &str,
    hit: &SearchHit,
    query: &str,
    body_cap: usize,
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
    let body = hit_body_capped(&hit, body_cap);
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
    // `k` counts files as far as the model is concerned, but paragraph fan-out means one
    // file can occupy several slots, so widen the unit budget to keep k files reachable.
    search::run_search_precise(
        &settings,
        state.backend.as_ref(),
        Some(state.mail_backend.as_ref()),
        query,
        unit_limit_for(k),
        path_prefix,
        None,
        &user_dict,
        UNITS_PER_FILE,
    )
}

fn unit_limit_for(k: usize) -> usize {
    (k * UNITS_PER_FILE).clamp(k, 24)
}

fn run_index_search_multi(
    state: &AppState,
    query: &str,
    prefixes: &[String],
    k: usize,
) -> Result<Vec<SearchHit>, String> {
    if prefixes.is_empty() {
        return run_index_search(state, query, None, k);
    }
    if prefixes.len() == 1 {
        return run_index_search(state, query, Some(&prefixes[0]), k);
    }
    let mut all = Vec::new();
    for p in prefixes {
        all.extend(run_index_search(state, query, Some(p), k)?);
    }
    Ok(merge_hits_by_score(all, unit_limit_for(k)))
}

fn merge_hits_by_score(mut hits: Vec<SearchHit>, limit: usize) -> Vec<SearchHit> {
    hits.sort_by(|x, y| y.score.total_cmp(&x.score));
    let mut seen = HashSet::new();
    hits.retain(|h| seen.insert(h.path.to_ascii_lowercase()));
    hits.truncate(limit);
    hits
}

/// Split a persisted `path_prefix` (newline-separated) into folder paths.
pub fn parse_thread_scopes(raw: &str) -> Vec<String> {
    raw.split('\n')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

/// Drop children when a parent is already selected, then cap the count.
pub fn join_thread_scopes(prefixes: &[String]) -> String {
    collapse_thread_scopes(prefixes)
        .into_iter()
        .take(MAX_THREAD_SCOPES)
        .collect::<Vec<_>>()
        .join("\n")
}

fn collapse_thread_scopes(prefixes: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in prefixes {
        let p = raw.trim();
        if p.is_empty() {
            continue;
        }
        if out
            .iter()
            .any(|kept| crate::pathutil::path_starts_with(p, kept))
        {
            continue;
        }
        out.retain(|kept| !crate::pathutil::path_starts_with(kept, p));
        out.push(p.to_string());
    }
    out
}

/// System-prompt line so the model does not treat a scoped miss as "nothing exists".
pub fn format_thread_scope_system_line(raw: &str) -> Option<String> {
    let scopes = parse_thread_scopes(raw);
    if scopes.is_empty() {
        return None;
    }
    let quoted = scopes
        .iter()
        .map(|s| format!("「{s}」"))
        .collect::<Vec<_>>()
        .join("、");
    Some(format!(
        "\nインデックス検索は{quoted}配下に限定されています。ここに無いものは索引外として扱ってください。"
    ))
}

fn scope_where_clause(scopes: &[String]) -> String {
    if scopes.is_empty() {
        String::new()
    } else {
        format!("（検索範囲: {}）", scopes.join("、"))
    }
}

/// Resolve the folder scope for a tool call.
///
/// The thread scope is a user instruction, so a model-supplied `path_prefix` may only
/// narrow it further. Anything outside is discarded rather than honoured, otherwise the
/// model could silently search folders the user excluded.
fn resolve_scopes(thread_scope: Option<&str>, requested: Option<&str>) -> Vec<String> {
    let thread = thread_scope
        .map(parse_thread_scopes)
        .unwrap_or_default();
    let requested = requested.map(str::trim).filter(|s| !s.is_empty());
    match (thread.is_empty(), requested) {
        (true, r) => r.map(|s| vec![s.to_string()]).unwrap_or_default(),
        (false, None) => thread,
        (false, Some(r)) => {
            if thread
                .iter()
                .any(|t| crate::pathutil::path_starts_with(r, t))
            {
                vec![r.to_string()]
            } else {
                thread
            }
        }
    }
}

/// Tell the model when a file has matching paragraphs beyond the ones returned, so it can
/// narrow the query instead of concluding the file only says this much.
///
/// `match_count` is the number of matching units in that file; the returned rows are
/// capped at `UNITS_PER_FILE`.
fn more_matches_note(hits: &[SearchHit]) -> Option<String> {
    let mut seen: Vec<(&str, u32)> = Vec::new();
    for hit in hits {
        let label = if hit.title.trim().is_empty() {
            hit.path.as_str()
        } else {
            hit.title.as_str()
        };
        if hit.match_count as usize > UNITS_PER_FILE && !seen.iter().any(|(l, _)| *l == label) {
            seen.push((label, hit.match_count));
        }
    }
    if seen.is_empty() {
        return None;
    }
    let list = seen
        .iter()
        .map(|(l, n)| format!("{l}（{n}件）"))
        .collect::<Vec<_>>()
        .join("、");
    Some(format!(
        "他にも一致段落があります: {list}。必要なら検索語を絞るか read_unit で前後を読んでください。"
    ))
}

/// Paragraph ids are `<path>#<unit_id>`; Windows paths can contain `#`, so split on the
/// last one. Neighbours are the adjacent units of the same file.
fn neighbor_ids(paragraph_id: &str) -> Vec<String> {
    let Some(idx) = paragraph_id.rfind('#') else {
        return Vec::new();
    };
    let (path, unit) = (&paragraph_id[..idx], &paragraph_id[idx + 1..]);
    let Ok(n) = unit.parse::<i64>() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if n > 0 {
        out.push(format!("{path}#{}", n - 1));
    }
    out.push(format!("{path}#{}", n + 1));
    out
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
    thread_scope: Option<&str>,
    next_cite: &mut i64,
) -> ToolExec {
    match execute_tool_inner(state, thread_id, name, arguments, thread_scope, next_cite) {
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
    thread_scope: Option<&str>,
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
            let requested = args.get("path_prefix").and_then(|v| v.as_str());
            let scopes = resolve_scopes(thread_scope, requested);
            let settings_k = state.settings.read().llm_search_top_k.clamp(1, 8) as usize;
            let k = args
                .get("k")
                .and_then(|v| v.as_u64())
                .map(|n| (n as usize).clamp(1, 8))
                .unwrap_or(settings_k);
            let hits = run_index_search_multi(state, &query, &scopes, k)?;
            if hits.is_empty() {
                let where_ = scope_where_clause(&scopes);
                return Ok(ToolExec {
                    content: format!(
                        "「{query}」に一致する索引ヒットはありません{where_}。語を減らすか別の語で言い換えてください。"
                    ),
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
                    TOOL_BODY_CAP,
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
            if let Some(note) = more_matches_note(&hits) {
                content.push('\n');
                content.push_str(&note);
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
                READ_BODY_CAP,
                next_cite,
                &mut new_rows,
                &mut already,
                &mut consumed,
            )?;
            // An article split across units, or a clause whose meaning sits in the
            // preceding sentence, needs the neighbours to be readable at all.
            let with_neighbors = args
                .get("with_neighbors")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            // One email is one unit, so its neighbours are unrelated messages.
            if with_neighbors && !crate::mail::is_outlook_path(&hit.path) {
                for nid in neighbor_ids(&paragraph_id) {
                    if let Some(n) = preview_hit(state, &nid)? {
                        persist_hit(
                            state,
                            thread_id,
                            &n,
                            "",
                            READ_BODY_CAP,
                            next_cite,
                            &mut new_rows,
                            &mut already,
                            &mut consumed,
                        )?;
                    }
                }
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thread_scope_is_a_hard_boundary() {
        let thread = Some(r"C:\cases\alpha");
        assert_eq!(
            resolve_scopes(thread, Some(r"C:\cases\alpha\pleadings")),
            vec![r"C:\cases\alpha\pleadings".to_string()],
            "a narrower request is honoured"
        );
        assert_eq!(
            resolve_scopes(thread, Some(r"C:\cases\beta")),
            vec![r"C:\cases\alpha".to_string()],
            "a request outside the thread scope must be discarded, not followed"
        );
        assert_eq!(
            resolve_scopes(thread, None),
            vec![r"C:\cases\alpha".to_string()]
        );
        assert!(
            resolve_scopes(None, None).is_empty(),
            "unscoped stays unscoped"
        );
        assert_eq!(
            resolve_scopes(None, Some(r"C:\cases\beta")),
            vec![r"C:\cases\beta".to_string()],
            "without a thread scope the model may narrow freely"
        );
    }

    #[test]
    fn multi_thread_scope_allows_narrowing_into_any_folder() {
        let thread = Some("C:\\cases\\alpha\nC:\\cases\\beta");
        assert_eq!(
            resolve_scopes(thread, Some(r"C:\cases\beta\exhibits")),
            vec![r"C:\cases\beta\exhibits".to_string()],
        );
        assert_eq!(
            resolve_scopes(thread, Some(r"C:\cases\gamma")),
            vec![
                r"C:\cases\alpha".to_string(),
                r"C:\cases\beta".to_string()
            ],
            "outside the union, keep every thread folder"
        );
        assert_eq!(
            resolve_scopes(thread, None),
            vec![
                r"C:\cases\alpha".to_string(),
                r"C:\cases\beta".to_string()
            ]
        );
    }

    #[test]
    fn join_thread_scopes_drops_children_and_caps() {
        let joined = join_thread_scopes(&[
            r"C:\cases\alpha".into(),
            r"C:\cases\alpha\pleadings".into(),
            r"C:\cases\beta".into(),
        ]);
        assert_eq!(joined, "C:\\cases\\alpha\nC:\\cases\\beta");
        assert!(parse_thread_scopes("").is_empty());
        assert_eq!(
            parse_thread_scopes("C:\\a\n\nC:\\b"),
            vec![r"C:\a".to_string(), r"C:\b".to_string()]
        );
        let many: Vec<String> = (0..12).map(|i| format!(r"C:\cases\{i}")).collect();
        assert_eq!(parse_thread_scopes(&join_thread_scopes(&many)).len(), 8);
        assert_eq!(
            format_thread_scope_system_line("C:\\a\nC:\\b").as_deref(),
            Some(
                "\nインデックス検索は「C:\\a」、「C:\\b」配下に限定されています。ここに無いものは索引外として扱ってください。"
            )
        );
        assert!(format_thread_scope_system_line("").is_none());
    }

    #[test]
    fn neighbor_ids_split_on_the_last_hash() {
        // Windows paths can contain `#`, so only the trailing segment is the unit id.
        let ids = neighbor_ids(r"C:\docs\a#1 note.txt#3");
        assert_eq!(
            ids,
            vec![
                r"C:\docs\a#1 note.txt#2".to_string(),
                r"C:\docs\a#1 note.txt#4".to_string()
            ]
        );
        assert_eq!(
            neighbor_ids(r"C:\docs\x.txt#0"),
            vec![r"C:\docs\x.txt#1".to_string()],
            "unit 0 has no predecessor"
        );
        assert!(neighbor_ids("outlook:abc").is_empty(), "no unit id, no neighbours");
    }

    #[test]
    fn truncated_body_is_marked_so_the_model_can_read_on() {
        let long: String = "あ".repeat(TOOL_BODY_CAP + 50);
        let capped = cap_chars(&long, TOOL_BODY_CAP);
        assert!(capped.ends_with(TRUNCATED_MARK));
        assert_eq!(capped.chars().count(), TOOL_BODY_CAP + TRUNCATED_MARK.chars().count());
        let short = "短い本文";
        assert_eq!(cap_chars(short, TOOL_BODY_CAP), short, "no marker when it fits");
    }
}
