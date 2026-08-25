//! LLM tool-calling: index search and unit preview.

use std::collections::HashSet;

use serde_json::{json, Value};

use crate::db::LlmSourceRow;
use crate::llm::context::format_sources;
use crate::search::{self, SearchHit};
use crate::state::AppState;

pub const TOOL_SEARCH: &str = "search_index";
pub const TOOL_READ: &str = "read_unit";
pub const TOOL_READ_URL: &str = "read_url";
pub const MAX_TOOL_ROUNDS: usize = 3;
pub const MAX_TOOL_ROUNDS_WEB: usize = 4;
pub const MAX_READ_URL_PER_ROUND: usize = 2;
pub const LLM_SEARCH_K_MAX: usize = 16;
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

pub fn tools_schema(web_search: bool) -> Value {
    let search_desc = if web_search {
        "Argosの索引を検索する（ファイルとメール）。ウェブ検索が有効なときは同じ語で公開ウェブも同時に検索する。添付出典で足りるときは呼ばない。\
クエリは調べたい語だけを空白区切りで並べる（例: 『解雇 有効性 裁判例』）。\
「〜を教えて」「〜について調べて」のような文ではなく単語で指定する。\
条文を引くときは『民法 第555条』のように法令名と条番号を書く。\
時期や「直近」があるときだけ after / before を YYYY-MM-DD で渡し、期間内だけで検索する（新着スコア優遇はしない）。\
「直近」はまず after を今日の30日前、sort は date。0件や件数不足なら after を90日前、次に1年前へ広げて再検索する。\
送信者は from に表示名を入れる。0件のときは語を減らすか、期間を広げて試す。\
公開情報が必要な質問ではこのツールを呼ぶ。ウェブ結果はスニペットであり（ウェブ）と付く。本文が必要な URL は read_url に渡す。"
    } else {
        "Argosの索引を検索する（ファイルとメール）。添付出典で足りるときは呼ばない。\
クエリは調べたい語だけを空白区切りで並べる（例: 『解雇 有効性 裁判例』）。\
「〜を教えて」「〜について調べて」のような文ではなく単語で指定する。\
条文を引くときは『民法 第555条』のように法令名と条番号を書く。\
時期や「直近」があるときだけ after / before を YYYY-MM-DD で渡し、期間内だけで検索する（新着スコア優遇はしない）。\
「直近」はまず after を今日の30日前、sort は date。0件や件数不足なら after を90日前、次に1年前へ広げて再検索する。\
送信者は from に表示名を入れる。0件のときは語を減らすか、期間を広げて試す。"
    };
    let mut tools = vec![
        json!({
            "type": "function",
            "function": {
                "name": TOOL_SEARCH,
                "description": search_desc,
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "検索語（単語を空白区切り。\"...\" で完全一致、-語 で除外）" },
                        "path_prefix": {
                            "type": "string",
                            "description": "優先するフォルダパス（任意）。そのフォルダを厚く返すが、結果には他フォルダの関連候補も混ざる（どれかは応答に示す）。\
ユーザーが特定の案件やフォルダを指したとき、または出典のパス配下を深掘りするときに、分かっているフルパスを渡す。\
検索語にフォルダ名が入っているだけなら渡さなくてよい。スレッドに検索範囲があるときは、その配下だけが有効。"
                        },
                        "k": { "type": "integer", "description": "件数（1〜16）" },
                        "after": {
                            "type": "string",
                            "description": "この日以降（YYYY-MM-DD、ローカル日付、含む）。直近なら今日の約30日前。"
                        },
                        "before": {
                            "type": "string",
                            "description": "この日以前（YYYY-MM-DD、ローカル日付、含む）"
                        },
                        "from": {
                            "type": "string",
                            "description": "メール送信者の表示名（部分一致）。指定時はメールのみを検索する。"
                        },
                        "sort": {
                            "type": "string",
                            "description": "relevance（既定）または date（新しい順）。直近の一覧では date。"
                        }
                    },
                    "required": ["query"]
                }
            }
        }),
        json!({
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
        }),
    ];
    if web_search {
        tools.push(json!({
            "type": "function",
            "function": {
                "name": TOOL_READ_URL,
                "description": "公開ウェブの1件の URL の本文を読む。search_index の（ウェブ）スニペットでは足りないときだけ使う。\
ユーザーがメッセージに貼った URL は既に出典に付いているので呼ばない。一度に複数呼ぶときは重要なものから最大2件。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "url": { "type": "string", "description": "http(s) の URL" }
                    },
                    "required": ["url"]
                }
            }
        }));
    }
    Value::Array(tools)
}

pub fn max_tool_rounds(web_search: bool) -> usize {
    if web_search {
        MAX_TOOL_ROUNDS_WEB
    } else {
        MAX_TOOL_ROUNDS
    }
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
    let title = source_title(&hit);
    let (mut row, created) = state
        .db
        .insert_llm_source(
            thread_id,
            "tool",
            &hit.path,
            &title,
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

const WEB_EMPTY_SNIPPET: &str = "（スニペットなし）";

fn web_body_looks_full(body: &str) -> bool {
    body.chars().count() > TOOL_BODY_CAP + TRUNCATED_MARK.chars().count()
}

#[allow(clippy::too_many_arguments)]
fn persist_web_hit(
    state: &AppState,
    thread_id: &str,
    hit: &crate::llm::searxng::WebHit,
    query: &str,
    next_cite: &mut i64,
    new_rows: &mut Vec<LlmSourceRow>,
    already: &mut Vec<String>,
    consumed: &mut Vec<(String, i64)>,
) -> Result<(), String> {
    if let Some(existing) = state
        .db
        .find_llm_source_by_path(thread_id, &hit.url)
        .map_err(|e| e.to_string())?
    {
        already.push(already_line(&existing));
        return Ok(());
    }
    let body = {
        let raw = hit.content.trim();
        if raw.is_empty() {
            WEB_EMPTY_SNIPPET.to_string()
        } else {
            cap_chars(raw, TOOL_BODY_CAP)
        }
    };
    let (mut row, created) = state
        .db
        .insert_llm_source_full(
            thread_id,
            "tool",
            &hit.url,
            &hit.title,
            "",
            &body,
            query,
            "unit",
            "web",
            "",
            "",
            None,
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

#[allow(clippy::too_many_arguments)]
fn persist_web_page(
    state: &AppState,
    thread_id: &str,
    url: &str,
    title: &str,
    body: &str,
    origin: &str,
    next_cite: &mut i64,
    new_rows: &mut Vec<LlmSourceRow>,
    already: &mut Vec<String>,
    consumed: &mut Vec<(String, i64)>,
) -> Result<(), String> {
    if let Some(existing) = state
        .db
        .find_llm_source_by_path(thread_id, url)
        .map_err(|e| e.to_string())?
    {
        if web_body_looks_full(&existing.body)
            && existing.body.chars().count() >= body.chars().count()
        {
            already.push(already_line(&existing));
            if existing.cite_no > 0 {
                consumed.push((existing.id.clone(), existing.cite_no));
            }
            return Ok(());
        }
    }
    let body = cap_chars(body.trim(), crate::llm::fetch_url::FETCH_BODY_CAP);
    if body.is_empty() {
        return Ok(());
    }
    let title = if title.trim().is_empty() { url } else { title };
    let (mut row, created) = state
        .db
        .insert_llm_source_full(
            thread_id,
            origin,
            url,
            title,
            "",
            &body,
            "",
            "unit",
            "web",
            "",
            "",
            None,
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

fn source_title(hit: &SearchHit) -> String {
    let title = if hit.title.trim().is_empty() {
        hit.path.as_str()
    } else {
        hit.title.as_str()
    };
    if hit.doc_kind != "email" && !crate::mail::is_outlook_path(&hit.path) {
        return title.to_string();
    }
    let from = hit.mail_from.trim();
    let ymd = search::format_unix_ymd(&hit.mail_date);
    match (from.is_empty(), ymd.is_empty()) {
        (true, true) => title.to_string(),
        (false, true) => format!("{title}（送信者: {from}）"),
        (true, false) => format!("{title}（{ymd}）"),
        (false, false) => format!("{title}（送信者: {from} / {ymd}）"),
    }
}

const MAIL_QUERY_NOISE: &[&str] = &[
    "メール",
    "メイル",
    "mail",
    "email",
    "e-mail",
    "直近",
    "最近",
    "最新",
    "件",
    "通",
    "送信",
    "送信者",
    "差出人",
    "探す",
    "探して",
    "検索",
    "から",
    "の",
    "を",
    "が",
    "は",
];

/// Drop sender name and listing boilerplate so they are not required in the body.
pub fn topical_query(query: &str, mail_from: Option<&str>) -> String {
    let from = mail_from.unwrap_or("").trim();
    let mut parts: Vec<String> = Vec::new();
    for raw in query.split(|c: char| c.is_whitespace() || matches!(c, ',' | '、' | '・')) {
        let t = raw
            .trim()
            .trim_matches(|c: char| "「」『』。．、,!?！？".contains(c));
        if t.is_empty() {
            continue;
        }
        if !from.is_empty() && (t == from || from.contains(t) || t.contains(from)) {
            continue;
        }
        if MAIL_QUERY_NOISE
            .iter()
            .any(|n| n.eq_ignore_ascii_case(t))
        {
            continue;
        }
        parts.push(t.to_string());
    }
    parts.join(" ")
}

pub fn format_search_date_system_line(mail_days_back: u32) -> String {
    format!(
        "\n本日は {}（ローカル日付）です。時期や「直近」「最近」があるときだけ search_index の after / before を YYYY-MM-DD で渡してください。無いときは期間で絞らないでください。送信者は from に表示名を入れてください。「直近」は after を今日の30日前、sort は date にしてください。0件や件数不足なら after を90日前、次に1年前へ広げて再検索してください。新着をスコアで優遇しないでください。メールの同期範囲は過去{}日です。",
        search::today_ymd(),
        mail_days_back.max(1)
    )
}

pub fn format_web_search_system_line() -> String {
    "\nウェブ検索が有効です。search_index を呼ぶと同じ語で公開ウェブも検索します。添付出典とインデックスを優先してください。ウェブ結果はスニペットであり判決全文ではありません（出典に（ウェブ）と付きます）。本文が必要なときはその URL を read_url に渡してください。ユーザーが貼った URL は既に出典に付いています。ウェブだけを根拠にするときは公開情報だと明示してください。公開情報が必要な質問では search_index を呼んでください。".into()
}

pub fn should_run_web_sidecar(web_search: bool, list_mail: bool, search_q: &str) -> bool {
    web_search && !list_mail && !search_q.trim().is_empty()
}

fn run_index_search(
    state: &AppState,
    query: &str,
    path_prefix: Option<&str>,
    k: usize,
    filter: &search::SearchFilter,
    list_mail: bool,
) -> Result<Vec<SearchHit>, String> {
    let settings = state.settings.read().clone();
    let user_dict = state.user_dict.read().clone();
    if list_mail {
        let Some(mail_paths) = filter.mail_paths.as_ref() else {
            return Ok(Vec::new());
        };
        return search::list_mail_hits(
            state.mail_backend.as_ref(),
            mail_paths,
            k,
            settings.mail_thread_collapse,
        );
    }
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
        filter,
    )
}

fn unit_limit_for(k: usize) -> usize {
    (k * UNITS_PER_FILE).clamp(k, 48)
}

/// Share of the result slots kept for the preferred folder.
const PREFERRED_FOLDER_SHARE: f32 = 0.6;
/// How close to the best in-folder hit an outside hit must score to spend one of the
/// remaining slots. Below this the slot returns to the preferred folder: a narrowing that
/// was right should not be diluted with weak cross-folder noise.
const OTHER_FOLDER_SCORE_RATIO: f32 = 0.6;

fn run_index_search_multi(
    state: &AppState,
    query: &str,
    scopes: &SearchScopes,
    k: usize,
    date: search::DateFilter,
    mail_from: Option<&str>,
    sort_date: bool,
    list_mail: bool,
) -> Result<Vec<SearchHit>, String> {
    let prefixes: Vec<Option<&str>> = if scopes.outer.is_empty() {
        vec![None]
    } else {
        scopes.outer.iter().map(|p| Some(p.as_str())).collect()
    };
    let mut all = Vec::new();
    for prefix in prefixes {
        let filter = search::build_search_filter(
            &state.db,
            date,
            mail_from,
            prefix,
            None,
            sort_date,
        )?;
        all.extend(run_index_search(
            state,
            query,
            prefix,
            k,
            &filter,
            list_mail,
        )?);
    }
    if sort_date || list_mail {
        all.sort_by(|a, b| {
            let da: i64 = a.mail_date.parse().unwrap_or(0);
            let db: i64 = b.mail_date.parse().unwrap_or(0);
            db.cmp(&da)
        });
        let mut seen = HashSet::new();
        all.retain(|h| seen.insert(h.path.to_ascii_lowercase()));
        all.truncate(k);
        return Ok(all);
    }

    let limit = unit_limit_for(k);
    let Some(preferred) = scopes.mixing_prefix(mail_from, sort_date, list_mail) else {
        return Ok(merge_hits_diversified(all, limit));
    };

    let base = search::build_search_filter(
        &state.db,
        date,
        mail_from,
        Some(preferred),
        None,
        sort_date,
    )?;
    let filter = search::narrow_filter_to_prefix(&state.db, &base, preferred)?;
    let scoped = run_index_search(state, query, Some(preferred), k, &filter, list_mail)?;
    Ok(merge_with_folder_quota(scoped, all, preferred, limit))
}

fn sort_and_dedupe_by_path(hits: &mut Vec<SearchHit>) {
    hits.sort_by(|x, y| y.score.total_cmp(&x.score));
    let mut seen = HashSet::new();
    hits.retain(|h| seen.insert(h.path.to_ascii_lowercase()));
}

/// Slots the tail gives up when one folder has shut everyone else out.
const DIVERSITY_MAX_SWAPS: usize = 2;
/// How close to the top hit an outside paragraph must score to claim a tail slot.
const DIVERSITY_SCORE_RATIO: f32 = 0.7;

/// Directory holding the file, or empty when the path has no folder to speak of (Outlook
/// items, which scope through `mail_folder` instead).
fn parent_folder(path: &str) -> String {
    if crate::mail::is_outlook_path(path) {
        return String::new();
    }
    let normalized = crate::pathutil::simplify_windows_path(path);
    match normalized.rfind('\\') {
        Some(i) => normalized[..i].to_string(),
        None => String::new(),
    }
}

/// Let a second folder into the tail when one folder has taken every slot.
///
/// Without a preferred folder there is nothing to state the user's intent, so a single
/// case folder can fill the results while a comparable paragraph sits one folder over. The
/// swap is deliberately small and never touches the top slots: when the answer really does
/// live in one folder, that has to keep working.
fn merge_hits_diversified(mut hits: Vec<SearchHit>, limit: usize) -> Vec<SearchHit> {
    sort_and_dedupe_by_path(&mut hits);
    let max_swaps = DIVERSITY_MAX_SWAPS.min(limit.saturating_sub(1));
    if hits.len() <= limit || max_swaps == 0 {
        hits.truncate(limit);
        return hits;
    }
    let dominant = parent_folder(&hits[0].path);
    if dominant.is_empty() {
        hits.truncate(limit);
        return hits;
    }
    let in_dominant =
        |hit: &SearchHit| parent_folder(&hit.path).eq_ignore_ascii_case(&dominant);
    // Only step in when the cut is a shutout; a mixed result needs no help.
    if !hits[..limit].iter().all(in_dominant) {
        hits.truncate(limit);
        return hits;
    }
    let floor = hits[0].score * DIVERSITY_SCORE_RATIO;
    let swaps: Vec<SearchHit> = hits[limit..]
        .iter()
        .filter(|h| h.score >= floor && !in_dominant(h))
        .take(max_swaps)
        .cloned()
        .collect();
    hits.truncate(limit);
    for (i, hit) in swaps.into_iter().enumerate() {
        let idx = hits.len() - 1 - i;
        hits[idx] = hit;
    }
    hits.sort_by(|x, y| y.score.total_cmp(&x.score));
    hits
}

/// Blend the preferred-folder search with the whole-boundary search.
///
/// The outside share is a cap rather than a reservation. Filling it unconditionally would
/// punish a correct narrowing, so an outside hit has to score near the best in-folder one;
/// otherwise its slot goes back to the preferred folder. When the folder itself comes up
/// short the outside hits fill in, which is what stops the model from having to widen the
/// scope and search again.
fn merge_with_folder_quota(
    scoped: Vec<SearchHit>,
    outer: Vec<SearchHit>,
    prefix: &str,
    limit: usize,
) -> Vec<SearchHit> {
    let mut preferred: Vec<SearchHit> = Vec::new();
    let mut others: Vec<SearchHit> = Vec::new();
    for hit in scoped.into_iter().chain(outer) {
        if crate::pathutil::path_starts_with(&hit.path, prefix) {
            preferred.push(hit);
        } else {
            others.push(hit);
        }
    }
    sort_and_dedupe_by_path(&mut preferred);
    sort_and_dedupe_by_path(&mut others);

    let floor = preferred
        .first()
        .map(|h| h.score * OTHER_FOLDER_SCORE_RATIO)
        .unwrap_or(f32::MIN);
    let other_cap = limit.saturating_sub(preferred_quota(limit));

    let mut out: Vec<SearchHit> = Vec::with_capacity(limit);
    let mut others_rest: Vec<SearchHit> = Vec::new();
    let mut taken_others = 0usize;
    for hit in others {
        if taken_others < other_cap && hit.score >= floor {
            taken_others += 1;
            out.push(hit);
        } else {
            others_rest.push(hit);
        }
    }
    let room_for_preferred = limit.saturating_sub(out.len());
    let mut preferred_iter = preferred.into_iter();
    out.extend(preferred_iter.by_ref().take(room_for_preferred));
    // Whatever the preferred folder left unused goes to the rest, best score first.
    let leftover = limit.saturating_sub(out.len());
    if leftover > 0 {
        out.extend(others_rest.into_iter().take(leftover));
    }
    out.sort_by(|x, y| y.score.total_cmp(&x.score));
    out.truncate(limit);
    out
}

fn preferred_quota(limit: usize) -> usize {
    (((limit as f32) * PREFERRED_FOLDER_SHARE).round() as usize).clamp(1, limit.max(1))
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

fn date_where_clause(after: Option<&str>, before: Option<&str>) -> String {
    match (
        after.map(str::trim).filter(|s| !s.is_empty()),
        before.map(str::trim).filter(|s| !s.is_empty()),
    ) {
        (None, None) => String::new(),
        (Some(a), None) => format!("（期間: {a}〜）"),
        (None, Some(b)) => format!("（期間: 〜{b}）"),
        (Some(a), Some(b)) => format!("（期間: {a}〜{b}）"),
    }
}

fn date_reaches_before_sync(date: search::DateFilter, mail_days_back: u32) -> bool {
    let cutoff = chrono::Local::now().timestamp() - (mail_days_back.max(1) as i64) * 86_400;
    match date.after_unix {
        Some(after) => after < cutoff,
        None => date.is_active(),
    }
}

/// Folder scoping for one search round.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SearchScopes {
    /// Hard boundary. Empty means the whole index. Only the user sets this.
    pub outer: Vec<String>,
    /// Folder the model asked to favour. Not a boundary: hits outside it still come back
    /// through the other-folder quota, because the model can pick the wrong folder and a
    /// case's answer often sits partly in shared statute or evidence folders.
    pub preferred: Option<String>,
}

impl SearchScopes {
    /// The folder to favour for this round, or `None` when the round blends nothing.
    ///
    /// Date listings and sender lookups order their results themselves, and a sender filter
    /// searches mail only, so none of them has file paths to weigh.
    fn mixing_prefix(
        &self,
        mail_from: Option<&str>,
        sort_date: bool,
        list_mail: bool,
    ) -> Option<&str> {
        if sort_date || list_mail || mail_from.is_some() {
            return None;
        }
        self.preferred_for_mixing()
    }

    /// The folder to favour, when favouring it would actually change the retrieval.
    ///
    /// A preferred folder equal to the boundary would run the same query twice, and a mail
    /// folder is not a path prefix, so both fall back to the single-search path.
    fn preferred_for_mixing(&self) -> Option<&str> {
        let preferred = self.preferred.as_deref()?;
        if preferred.starts_with("mailfolder:") {
            return None;
        }
        let same_as_boundary = self
            .outer
            .iter()
            .any(|outer| same_folder_path(outer, preferred));
        if same_as_boundary {
            return None;
        }
        Some(preferred)
    }
}

fn same_folder_path(a: &str, b: &str) -> bool {
    crate::pathutil::simplify_windows_path(a)
        .eq_ignore_ascii_case(&crate::pathutil::simplify_windows_path(b))
}

/// Resolve the folder scoping for a tool call.
///
/// The thread scope is a user instruction, so it stays a hard boundary and a model-supplied
/// `path_prefix` outside it is discarded rather than honoured — otherwise the model could
/// silently search folders the user excluded.
///
/// Inside the boundary the model's prefix is only a preference. Making it a filter trades
/// one kind of miss for another: the folder-name signal is real, but so is the answer that
/// lives one folder over.
///
/// A `mailfolder:` request is a hard scope, not a preference. Outlook folders are not a
/// path hierarchy, so there is no "nearby folder" to fall back to.
fn resolve_scopes(thread_scope: Option<&str>, requested: Option<&str>) -> SearchScopes {
    let thread = thread_scope.map(parse_thread_scopes).unwrap_or_default();
    let requested = requested.map(str::trim).filter(|s| !s.is_empty());

    let Some(requested) = requested else {
        return SearchScopes {
            outer: thread,
            preferred: None,
        };
    };
    let inside = thread.is_empty()
        || thread
            .iter()
            .any(|t| crate::pathutil::path_starts_with(requested, t));
    if !inside {
        return SearchScopes {
            outer: thread,
            preferred: None,
        };
    }
    if requested.starts_with("mailfolder:") {
        return SearchScopes {
            outer: vec![requested.to_string()],
            preferred: None,
        };
    }
    SearchScopes {
        outer: thread,
        preferred: Some(requested.to_string()),
    }
}

/// Flag the sources that came from outside the folder the model asked to favour, so it can
/// weigh them as neighbouring material rather than assume everything is in-scope.
///
/// Carried in the tool reply instead of a source column: `llm_thread_sources` would need a
/// migration for something only this round cares about.
fn outside_folder_note(prefix: &str, rows: &[crate::db::LlmSourceRow]) -> Option<String> {
    let outside: Vec<String> = rows
        .iter()
        .filter(|r| !r.is_web() && !crate::pathutil::path_starts_with(&r.path, prefix))
        .map(|r| format!("[{}]", r.cite_no))
        .collect();
    if outside.is_empty() {
        return None;
    }
    Some(format!(
        "{} は「{prefix}」の外にある関連候補です。",
        outside.join("")
    ))
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
    web_search: bool,
) -> ToolExec {
    match execute_tool_inner(
        state,
        thread_id,
        name,
        arguments,
        thread_scope,
        next_cite,
        web_search,
    ) {
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
    web_search: bool,
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
            let settings_k = state.settings.read().llm_search_top_k.clamp(1, LLM_SEARCH_K_MAX as u32)
                as usize;
            let k = args
                .get("k")
                .and_then(|v| v.as_u64())
                .map(|n| (n as usize).clamp(1, LLM_SEARCH_K_MAX))
                .unwrap_or(settings_k);
            let mail_from = args
                .get("from")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let after = args.get("after").and_then(|v| v.as_str());
            let before = args.get("before").and_then(|v| v.as_str());
            let date = match search::parse_date_range(after, before) {
                Ok(d) => d,
                Err(e) => {
                    return Ok(ToolExec {
                        content: e,
                        consumed: Vec::new(),
                    });
                }
            };
            let sort_date = args
                .get("sort")
                .and_then(|v| v.as_str())
                .is_some_and(|s| s.eq_ignore_ascii_case("date"));
            let topical = topical_query(&query, mail_from);
            let list_mail = mail_from.is_some() && topical.is_empty();
            let search_q = if list_mail || topical.is_empty() {
                query.as_str()
            } else {
                topical.as_str()
            };
            let run_web = should_run_web_sidecar(web_search, list_mail, search_q);
            let web_job = if run_web {
                let settings = state.settings.read().clone();
                let q = search_q.to_string();
                Some(std::thread::spawn(move || crate::llm::searxng::search(&settings, &q)))
            } else {
                None
            };
            let hits = run_index_search_multi(
                state,
                search_q,
                &scopes,
                k,
                date,
                mail_from,
                sort_date,
                list_mail,
            )?;
            let outside_prefix = scopes.mixing_prefix(mail_from, sort_date, list_mail);
            let web_outcome = match web_job {
                Some(job) => match job.join() {
                    Ok(r) => Some(r),
                    Err(_) => Some(Err("ウェブ検索スレッドが失敗しました。".into())),
                },
                None => None,
            };
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
            let folder_note = outside_prefix.and_then(|p| outside_folder_note(p, &new_rows));
            let mut web_note = String::new();
            match web_outcome {
                Some(Ok(web_hits)) => {
                    if web_hits.is_empty() {
                        web_note = "ウェブ検索のヒットはありませんでした。".into();
                    } else {
                        for hit in &web_hits {
                            persist_web_hit(
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
                    }
                }
                Some(Err(e)) => {
                    web_note = format!("ウェブ検索に失敗: {e}");
                }
                None => {}
            }
            let mut content = String::new();
            if hits.is_empty() {
                // Only the hard boundary explains an empty result. Naming the preferred
                // folder would read as "not in that folder", which is not what was searched.
                let where_ = scope_where_clause(&scopes.outer);
                let period = date_where_clause(after, before);
                let mut msg = format!(
                    "「{query}」に一致する索引ヒットはありません{period}{where_}。"
                );
                if date.is_active() {
                    msg.push_str("この期間にヒットがなければ after を過去へ広げて再検索してください。");
                    let days = state.settings.read().mail_days_back.max(1);
                    if date_reaches_before_sync(date, days) {
                        msg.push_str(&format!(
                            "メールの同期範囲は過去{days}日です。それより前は索引にありません。"
                        ));
                    }
                } else {
                    msg.push_str("語を減らすか別の語で言い換えてください。");
                }
                content.push_str(&msg);
            }
            if !new_rows.is_empty() {
                if !content.is_empty() {
                    content.push('\n');
                }
                content.push_str(&format_sources(&new_rows));
            }
            if !already.is_empty() {
                if !content.is_empty() {
                    content.push('\n');
                }
                content.push_str(&already.join("\n"));
            }
            if let Some(note) = folder_note {
                content.push('\n');
                content.push_str(&note);
            }
            if let Some(note) = more_matches_note(&hits) {
                content.push('\n');
                content.push_str(&note);
            }
            if !web_note.is_empty() {
                if !content.is_empty() {
                    content.push('\n');
                }
                content.push_str(&web_note);
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
        TOOL_READ_URL => {
            if !web_search {
                return Ok(ToolExec {
                    content: "ウェブ検索がオフです。".into(),
                    consumed: Vec::new(),
                });
            }
            let url = args
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            if url.is_empty() {
                return Ok(ToolExec {
                    content: "url が空です。".into(),
                    consumed: Vec::new(),
                });
            }
            if let Some(existing) = state
                .db
                .find_llm_source_by_path(thread_id, &url)
                .map_err(|e| e.to_string())?
            {
                if web_body_looks_full(&existing.body) {
                    let mut consumed = Vec::new();
                    if existing.cite_no > 0 {
                        consumed.push((existing.id.clone(), existing.cite_no));
                    }
                    return Ok(ToolExec {
                        content: already_line(&existing),
                        consumed,
                    });
                }
            }
            let settings = state.settings.read().clone();
            let page = match crate::llm::fetch_url::fetch_page(
                &settings,
                &url,
                crate::llm::fetch_url::FetchAccess::Tool,
            ) {
                Ok(p) => p,
                Err(e) => {
                    return Ok(ToolExec {
                        content: format!("URL を読めません: {e}"),
                        consumed: Vec::new(),
                    });
                }
            };
            let mut body = page.body;
            if page.thin {
                body.push_str(
                    "\n（本文がほとんど取れませんでした。JavaScript で描画されている可能性があります。）",
                );
            }
            let mut new_rows = Vec::new();
            let mut already = Vec::new();
            let mut consumed = Vec::new();
            persist_web_page(
                state,
                thread_id,
                &url,
                &page.title,
                &body,
                "tool",
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
                if !content.is_empty() {
                    content.push('\n');
                }
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

    fn scopes(outer: &[&str], preferred: Option<&str>) -> SearchScopes {
        SearchScopes {
            outer: outer.iter().map(|s| s.to_string()).collect(),
            preferred: preferred.map(|s| s.to_string()),
        }
    }

    #[test]
    fn thread_scope_is_a_hard_boundary() {
        let thread = Some(r"C:\cases\alpha");
        assert_eq!(
            resolve_scopes(thread, Some(r"C:\cases\alpha\pleadings")),
            scopes(&[r"C:\cases\alpha"], Some(r"C:\cases\alpha\pleadings")),
            "a narrower request only sets a preference; the boundary is unchanged"
        );
        assert_eq!(
            resolve_scopes(thread, Some(r"C:\cases\beta")),
            scopes(&[r"C:\cases\alpha"], None),
            "a request outside the thread scope must be discarded, not followed"
        );
        assert_eq!(
            resolve_scopes(thread, None),
            scopes(&[r"C:\cases\alpha"], None)
        );
        assert_eq!(
            resolve_scopes(None, None),
            scopes(&[], None),
            "unscoped stays unscoped"
        );
    }

    #[test]
    fn model_prefix_prefers_a_folder_without_excluding_the_rest() {
        // Without a thread scope the model may still steer, but it must not shrink the
        // searched universe: the answer often sits in a shared statute folder.
        let resolved = resolve_scopes(None, Some(r"C:\cases\beta"));
        assert_eq!(resolved, scopes(&[], Some(r"C:\cases\beta")));
        assert_eq!(resolved.preferred_for_mixing(), Some(r"C:\cases\beta"));
    }

    #[test]
    fn preferred_equal_to_the_boundary_does_not_run_twice() {
        let resolved = resolve_scopes(Some(r"C:\cases\alpha"), Some(r"C:\cases\alpha/"));
        assert_eq!(resolved.outer, vec![r"C:\cases\alpha".to_string()]);
        assert_eq!(
            resolved.preferred_for_mixing(),
            None,
            "the two halves would issue the same query"
        );
    }

    #[test]
    fn mail_folder_request_stays_a_hard_scope() {
        // Outlook folders are not a path hierarchy, so there is no neighbouring folder to
        // fall back to and nothing to blend.
        let resolved = resolve_scopes(None, Some("mailfolder:受信トレイ"));
        assert_eq!(resolved.outer, vec!["mailfolder:受信トレイ".to_string()]);
        assert_eq!(resolved.preferred_for_mixing(), None);
    }

    #[test]
    fn multi_thread_scope_allows_narrowing_into_any_folder() {
        let thread = Some("C:\\cases\\alpha\nC:\\cases\\beta");
        assert_eq!(
            resolve_scopes(thread, Some(r"C:\cases\beta\exhibits")),
            scopes(
                &[r"C:\cases\alpha", r"C:\cases\beta"],
                Some(r"C:\cases\beta\exhibits")
            ),
            "both user folders stay searchable; one is merely favoured"
        );
        assert_eq!(
            resolve_scopes(thread, Some(r"C:\cases\gamma")),
            scopes(&[r"C:\cases\alpha", r"C:\cases\beta"], None),
            "outside the union, keep every thread folder"
        );
        assert_eq!(
            resolve_scopes(thread, None),
            scopes(&[r"C:\cases\alpha", r"C:\cases\beta"], None)
        );
    }

    fn file_hit(path: &str, score: f32) -> SearchHit {
        SearchHit {
            id: format!("{path}#1"),
            title: path.rsplit('\\').next().unwrap_or(path).to_string(),
            snippet: String::new(),
            path: path.into(),
            page: None,
            chunk_id: None,
            score,
            source: "local".into(),
            preview_text: "本文".into(),
            highlight_terms: vec![],
            match_count: 1,
            paragraphs: vec![],
            unit_label: String::new(),
            mail_from: String::new(),
            mail_date: String::new(),
            mail_conversation_id: String::new(),
            mail_folder: String::new(),
            doc_kind: "file".into(),
        }
    }

    fn paths_of(hits: &[SearchHit]) -> Vec<&str> {
        hits.iter().map(|h| h.path.as_str()).collect()
    }

    #[test]
    fn quota_keeps_a_strong_hit_from_outside_the_preferred_folder() {
        // The preferred folder could fill every slot on score alone. A comparable paragraph
        // in the shared statute folder is exactly what hard scoping used to lose.
        let scoped = vec![
            file_hit(r"C:\cases\alpha\a1.txt", 9.0),
            file_hit(r"C:\cases\alpha\a2.txt", 8.0),
            file_hit(r"C:\cases\alpha\a3.txt", 7.5),
            file_hit(r"C:\cases\alpha\a4.txt", 7.0),
        ];
        let outer = vec![file_hit(r"C:\law\minpo.txt", 8.5)];
        let merged = merge_with_folder_quota(scoped, outer, r"C:\cases\alpha", 4);
        assert!(
            merged.iter().any(|h| h.path == r"C:\law\minpo.txt"),
            "outside hit above the floor must survive: {:?}",
            paths_of(&merged)
        );
        assert_eq!(merged.len(), 4);
        let inside = merged
            .iter()
            .filter(|h| h.path.starts_with(r"C:\cases\alpha"))
            .count();
        assert_eq!(inside, 3, "the preferred folder still holds the 60% share");
    }

    #[test]
    fn quota_slot_returns_to_the_preferred_folder_when_outside_is_weak() {
        // The other-folder share is a cap, not a reservation: a correct narrowing must not
        // be diluted with whatever happened to rank next.
        let scoped = vec![
            file_hit(r"C:\cases\alpha\a1.txt", 10.0),
            file_hit(r"C:\cases\alpha\a2.txt", 9.0),
            file_hit(r"C:\cases\alpha\a3.txt", 8.0),
            file_hit(r"C:\cases\alpha\a4.txt", 7.0),
        ];
        let outer = vec![file_hit(r"C:\other\weak.txt", 1.0)];
        let merged = merge_with_folder_quota(scoped, outer, r"C:\cases\alpha", 4);
        assert_eq!(
            paths_of(&merged),
            vec![
                r"C:\cases\alpha\a1.txt",
                r"C:\cases\alpha\a2.txt",
                r"C:\cases\alpha\a3.txt",
                r"C:\cases\alpha\a4.txt",
            ]
        );
    }

    #[test]
    fn quota_falls_back_to_other_folders_when_the_preferred_one_is_thin() {
        // Nothing in the folder means the model should still get material, instead of
        // widening the scope and searching again.
        let outer = vec![
            file_hit(r"C:\law\a.txt", 5.0),
            file_hit(r"C:\law\b.txt", 4.0),
        ];
        let merged = merge_with_folder_quota(Vec::new(), outer, r"C:\cases\alpha", 4);
        assert_eq!(paths_of(&merged), vec![r"C:\law\a.txt", r"C:\law\b.txt"]);
    }

    #[test]
    fn quota_dedupes_the_overlap_between_the_two_halves() {
        let scoped = vec![file_hit(r"C:\cases\alpha\a1.txt", 9.0)];
        let outer = vec![
            file_hit(r"C:\cases\alpha\a1.txt", 6.0),
            file_hit(r"C:\law\x.txt", 8.0),
        ];
        let merged = merge_with_folder_quota(scoped, outer, r"C:\cases\alpha", 4);
        assert_eq!(paths_of(&merged), vec![r"C:\cases\alpha\a1.txt", r"C:\law\x.txt"]);
    }

    #[test]
    fn diversify_breaks_a_single_folder_shutout() {
        let hits = vec![
            file_hit(r"C:\cases\alpha\a1.txt", 10.0),
            file_hit(r"C:\cases\alpha\a2.txt", 9.5),
            file_hit(r"C:\cases\alpha\a3.txt", 9.0),
            file_hit(r"C:\law\minpo.txt", 8.0),
        ];
        let out = merge_hits_diversified(hits, 3);
        assert!(
            out.iter().any(|h| h.path == r"C:\law\minpo.txt"),
            "a close second folder should reach the tail: {:?}",
            paths_of(&out)
        );
        assert_eq!(
            out[0].path,
            r"C:\cases\alpha\a1.txt",
            "the top slot is never given away"
        );
    }

    #[test]
    fn diversify_leaves_a_genuinely_single_folder_answer_alone() {
        let hits = vec![
            file_hit(r"C:\cases\alpha\a1.txt", 10.0),
            file_hit(r"C:\cases\alpha\a2.txt", 9.5),
            file_hit(r"C:\cases\alpha\a3.txt", 9.0),
            file_hit(r"C:\law\minpo.txt", 2.0),
        ];
        let out = merge_hits_diversified(hits, 3);
        assert!(
            out.iter().all(|h| h.path.starts_with(r"C:\cases\alpha")),
            "a distant hit must not displace in-folder ones: {:?}",
            paths_of(&out)
        );
    }

    #[test]
    fn diversify_ignores_outlook_paths() {
        // Mail has no parent folder in the filesystem sense; it scopes via mail_folder.
        let mut a = file_hit("outlook:store/one", 10.0);
        a.doc_kind = "email".into();
        let mut b = file_hit("outlook:store/two", 9.0);
        b.doc_kind = "email".into();
        let out = merge_hits_diversified(vec![a, b], 1);
        assert_eq!(paths_of(&out), vec!["outlook:store/one"]);
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

    #[test]
    fn topical_query_drops_sender_and_mail_noise() {
        assert_eq!(topical_query("Aさん メール 直近", Some("Aさん")), "");
        assert_eq!(topical_query("Aさん 契約", Some("Aさん")), "契約");
        assert_eq!(topical_query("解雇 有効性", None), "解雇 有効性");
    }

    #[test]
    fn web_sidecar_skips_mail_listing() {
        assert!(should_run_web_sidecar(true, false, "解雇 有効性"));
        assert!(
            !should_run_web_sidecar(true, true, "Aさん"),
            "mail listing must not hit the web"
        );
        assert!(!should_run_web_sidecar(true, false, "  "));
        assert!(!should_run_web_sidecar(false, false, "解雇"));
    }

    #[test]
    fn tools_schema_mentions_web_only_when_enabled() {
        let off = tools_schema(false).to_string();
        assert!(!off.contains("公開ウェブも同時に検索"));
        assert!(!off.contains(TOOL_READ_URL));
        let on = tools_schema(true).to_string();
        assert!(on.contains("公開ウェブも同時に検索"));
        assert!(on.contains(TOOL_READ_URL));
        assert!(on.contains("read_url"));
        assert_eq!(max_tool_rounds(false), MAX_TOOL_ROUNDS);
        assert_eq!(max_tool_rounds(true), MAX_TOOL_ROUNDS_WEB);
    }

    #[test]
    fn web_snippet_is_not_treated_as_full_body() {
        let snippet = cap_chars(&"あ".repeat(5_000), TOOL_BODY_CAP);
        assert!(!web_body_looks_full(&snippet));
        assert!(!web_body_looks_full(WEB_EMPTY_SNIPPET));
        assert!(web_body_looks_full(&"あ".repeat(2_000)));
    }

    #[test]
    fn source_title_includes_from_and_date() {
        let hit = SearchHit {
            id: "outlook:x#1".into(),
            title: "件名".into(),
            snippet: String::new(),
            path: "outlook:store/entry".into(),
            page: None,
            chunk_id: None,
            score: 1.0,
            source: "outlook".into(),
            preview_text: String::new(),
            highlight_terms: vec![],
            match_count: 1,
            paragraphs: vec![],
            unit_label: String::new(),
            mail_from: "山田太郎".into(),
            mail_date: "1755446400".into(),
            mail_conversation_id: String::new(),
            mail_folder: String::new(),
            doc_kind: "email".into(),
        };
        let title = source_title(&hit);
        assert!(title.contains("件名"));
        assert!(title.contains("山田太郎"));
        let ymd = search::format_unix_ymd(&hit.mail_date);
        assert!(!ymd.is_empty());
        assert!(title.contains(&ymd), "title={title} ymd={ymd}");
        assert!(title.contains("送信者"));
    }

    #[test]
    fn date_where_clause_and_zero_hit_sync_hint() {
        assert_eq!(date_where_clause(None, None), "");
        assert_eq!(date_where_clause(Some("2026-08-01"), None), "（期間: 2026-08-01〜）");
        assert_eq!(
            date_where_clause(Some("2026-08-01"), Some("2026-08-18")),
            "（期間: 2026-08-01〜2026-08-18）"
        );
        let recent = search::parse_date_range(Some(&search::today_ymd()), None).unwrap();
        assert!(!date_reaches_before_sync(recent, 730));
        let old = search::parse_date_range(Some("2010-01-01"), None).unwrap();
        assert!(date_reaches_before_sync(old, 730));
        let line = format_search_date_system_line(730);
        assert!(line.contains(&search::today_ymd()));
        assert!(line.contains("730"));
        assert!(line.contains("after"));
    }

    #[test]
    fn topical_query_sender_only_is_empty() {
        assert_eq!(topical_query("Aさん", Some("Aさん")), "");
        assert_eq!(topical_query("「Aさん」のメール", Some("Aさん")), "");
    }
}
