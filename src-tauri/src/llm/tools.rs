//! LLM tool-calling: index search and unit preview.

use std::collections::HashSet;

use serde_json::{json, Value};

use crate::db::LlmSourceRow;
use crate::llm::context::format_sources;
use crate::search::{self, SearchHit};
use crate::state::AppState;

pub const TOOL_SEARCH: &str = "search_index";
pub const TOOL_LIST_CALENDAR: &str = "list_calendar";
pub const TOOL_READ: &str = "read_unit";
pub const TOOL_READ_URL: &str = "read_url";
pub const TOOL_LIST_NOTES: &str = "list_notes";
pub const TOOL_READ_NOTE: &str = "read_note";
pub const TOOL_CREATE_NOTE: &str = "create_note";
pub const TOOL_WRITE_NOTE: &str = "write_note";
pub const MAX_TOOL_ROUNDS: usize = 8;

/// Shown alone when the no-tools final call fails or comes back empty.
pub fn tool_round_limit_notice(max_rounds: usize) -> String {
    format!(
        "ツール往復が上限の {max_rounds} 回に達したため、残りの操作はしていません。続きはもう一度指示するか、設定の「ツール往復の上限」を上げてください。"
    )
}

/// Shown with the answer after that final call succeeds.
pub fn tool_round_limit_short_notice(max_rounds: usize) -> String {
    format!(
        "ツール往復が上限の {max_rounds} 回に達したため、続きの操作はせず、ここまでの資料で答えています。"
    )
}

pub struct RoundLimitAnswer {
    pub text: String,
    /// Set only when `text` is the model's answer, not the fallback notice.
    pub notice: Option<String>,
}

/// Keep a real answer and a short notice. An empty or missing body is the long notice alone.
pub fn round_limit_answer(body: Option<&str>, max_rounds: usize) -> RoundLimitAnswer {
    if let Some(text) = body.filter(|text| !text.trim().is_empty()) {
        RoundLimitAnswer {
            text: text.to_string(),
            notice: Some(tool_round_limit_short_notice(max_rounds)),
        }
    } else {
        RoundLimitAnswer {
            text: tool_round_limit_notice(max_rounds),
            notice: None,
        }
    }
}

/// The no-tools final call failed. Name the cause in Japanese, without secrets from `error`.
pub fn round_limit_failure(max_rounds: usize, error: &str) -> RoundLimitAnswer {
    let reason = round_limit_failure_reason(error);
    RoundLimitAnswer {
        text: format!(
            "ツール往復が上限の {max_rounds} 回に達したため、残りの操作はしていません。続きの回答は失敗しました（{reason}）。続きはもう一度指示するか、設定の「ツール往復の上限」を上げてください。"
        ),
        notice: None,
    }
}

fn round_limit_failure_reason(error: &str) -> String {
    let l = error.to_lowercase();
    // `error` is already a fixed Japanese message. A status is named only when that
    // message contains our own `（HTTP n）` clause. Raw server text is not parsed.
    if let Some(clause) = crate::llm::http_error::owned_status_clause(error) {
        return clause;
    }
    if error.contains("API キーが拒否") {
        return "サーバが認証を拒否しました".into();
    }
    if error.contains("指定したモデルが見つかりません") {
        return "指定したモデルが見つかりません".into();
    }
    if error.contains("字に達しても答えが出ない") {
        return "思考が長く続いたため止めました".into();
    }
    if error.contains("思考を閉じられず") {
        return "思考を閉じられず止めました".into();
    }
    if error == crate::llm::http_error::CONTEXT_TOO_LONG_ERROR
        || l.contains("コンテキスト長")
        || l.contains("コンテキストに対して長すぎ")
        || l.contains("context length")
        || l.contains("context window")
        || l.contains("n_ctx")
        || l.contains("too many tokens")
        || l.contains("prompt is too long")
        || l.contains("maximum context")
    {
        return "コンテキストが長すぎます".into();
    }
    if l.contains("タイムアウト") || l.contains("timed out") || l.contains("timeout") {
        return "応答が時間切れになりました".into();
    }
    if l.contains("接続できません")
        || l.contains("届きません")
        || l.contains("connection refused")
        || l.contains("connect")
        || l.contains("dns")
        || l.contains("os error 10060")
        || l.contains("os error 10061")
    {
        return "サーバに接続できません".into();
    }
    "応答を受け取れませんでした".into()
}

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

pub fn tools_schema(web_search: bool, remote_index: bool, remote_only: bool) -> Value {
    let mut search_desc = if remote_only && web_search {
        "別PCの共有ファイルだけを検索する。このPCのファイルとメールは対象外。ウェブ検索が有効なときは同じ語で公開ウェブも同時に検索する。添付出典で足りるときは呼ばない。\
クエリは調べたい語だけを空白区切りで並べる（例: 『解雇 有効性 裁判例』）。\
「〜を教えて」「〜について調べて」のような文ではなく単語で指定する。\
条文を引くときは『民法 第555条』のように法令名と条番号を書く。\
時期や「直近」があるときだけ after / before を YYYY-MM-DD で渡し、期間内だけで検索する（新着スコア優遇はしない）。\
「直近」はまず after を今日の30日前、sort は date。0件や件数不足なら after を90日前、次に1年前へ広げて再検索する。\
送信者は from に表示名を入れる。0件のときは語を減らすか、期間を広げて試す。\
公開情報が必要な質問ではこのツールを呼ぶ。ウェブ結果はスニペットであり（ウェブ）と付く。本文が必要な URL は read_url に渡す。"
    } else if remote_only {
        "別PCの共有ファイルだけを検索する。このPCのファイルとメールは対象外。添付出典で足りるときは呼ばない。\
クエリは調べたい語だけを空白区切りで並べる（例: 『解雇 有効性 裁判例』）。\
「〜を教えて」「〜について調べて」のような文ではなく単語で指定する。\
条文を引くときは『民法 第555条』のように法令名と条番号を書く。\
時期や「直近」があるときだけ after / before を YYYY-MM-DD で渡し、期間内だけで検索する（新着スコア優遇はしない）。\
「直近」はまず after を今日の30日前、sort は date。0件や件数不足なら after を90日前、次に1年前へ広げて再検索する。\
送信者は from に表示名を入れる。0件のときは語を減らすか、期間を広げて試す。"
    } else if web_search {
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
    }
    .to_string();
    if remote_index && !remote_only {
        search_desc.push_str(
            "別PCの共有フォルダも検索する。その出典には（リモート）が付く。メールと送信者はこのPCだけ。期間はホストが対応しているときだけ別PCにも効く。",
        );
    }
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
                "name": TOOL_LIST_CALENDAR,
                "description": "このPCの予定表から、期間内の予定を日付の早い順に返す。予定・期日・会議・今日・今週・明日はこちらを使う。search_index の「直近は過去」は予定に使わない。資料が要るときは、返った件名や当事者名を search_index の query にする。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "after": { "type": "string", "description": "この日以降（YYYY-MM-DD）。省略時は今日。" },
                        "before": { "type": "string", "description": "この日以前（YYYY-MM-DD）。省略時は今日から14日後。" },
                        "query": { "type": "string", "description": "任意。空白区切りの語をすべて含む予定だけ（件名・場所・参加者・本文）。" }
                    }
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": TOOL_READ,
                "description": "段落の本文を読む。既読 [n] の原文は cite_no に n を渡すと、この会話に保存された本文を読む。\
添付された契約書などの文書について答えるときは、根拠にする条を articles で指定し、答える前に読み直す。\
条の見出しが無い文書は start と end で文字範囲を指定する。articles も範囲も省略すると保存された全文を返す。\
出典に (paragraph_id: ...) と示された段落や、末尾が […] で切れている出典の続きは paragraph_id で索引から読む。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "cite_no": {
                            "type": "integer",
                            "description": "出典番号 [n]。既読スタブや添付の原文を、この会話の保存分から読み直す。"
                        },
                        "articles": {
                            "type": "array",
                            "description": "読み直す条番号。例: [6, 7]。第6条の2 は \"6の2\"。文書にある条だけを原文のまま、文書の順で返す。無い番号では、ある条番号の一覧を返す。",
                            "items": { "type": ["integer", "string"], "description": "条番号。6 または \"6の2\"。" }
                        },
                        "start": {
                            "type": "integer",
                            "description": "文字範囲の開始（1文字目から、含む）。条の見出しが無い文書で使う。複数ページは ## nページ目 を含む書き起こしで数える。"
                        },
                        "end": {
                            "type": "integer",
                            "description": "文字範囲の終了（含む）。省略すると末尾まで。"
                        },
                        "paragraph_id": {
                            "type": "string",
                            "description": "段落ID。出典番号が無いとき、索引からその段落を読む。"
                        },
                        "with_neighbors": {
                            "type": "boolean",
                            "description": "前後の段落も併せて読む（既定 false）"
                        }
                    }
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
    tools.push(json!({
        "type": "function",
        "function": {
            "name": TOOL_LIST_NOTES,
            "description": "ノートの id・題名・更新日時。対象ノートが未設定のとき、どれを対象にするか確認するために使う。メモ本文は返さない。",
            "parameters": { "type": "object", "properties": {} }
        }
    }));
    tools.push(json!({
        "type": "function",
        "function": {
            "name": TOOL_READ_NOTE,
            "description": "ノートの見出し一覧と本文を読む。note_id を省略したら対象ノート。heading を指定するとその区画だけ。長いノートは全文が返らないので heading を指定して読む。",
            "parameters": {
                "type": "object",
                "properties": {
                    "note_id": { "type": "string", "description": "ノート id。空なら対象ノート。" },
                    "heading": {
                        "type": "string",
                        "description": "ATX見出し（# の行）。空なら全文（短いノートのみ）。"
                    }
                }
            }
        }
    }));
    tools.push(json!({
        "type": "function",
        "function": {
            "name": TOOL_CREATE_NOTE,
            "description": "ノートを新規作成してメモを書き、この会話の対象にする。書いた内容はすぐ反映される。",
            "parameters": {
                "type": "object",
                "properties": {
                    "title": { "type": "string", "description": "題名" },
                    "memo": { "type": "string", "description": "メモ本文（Markdown）" }
                },
                "required": ["memo"]
            }
        }
    }));
    tools.push(json!({
        "type": "function",
        "function": {
            "name": TOOL_WRITE_NOTE,
            "description": "ノートのメモを書き換える。書いた内容はすぐ反映される。採否はユーザーが行う。note_id を省略したら対象ノート。heading があればその区画だけ置換（無ければ新設）。全文置換するときは変えない箇所も残してメモ全体を渡す。長いノートは heading 必須。",
            "parameters": {
                "type": "object",
                "properties": {
                    "note_id": { "type": "string", "description": "ノート id。空なら対象ノート。" },
                    "title": { "type": "string", "description": "題名を変えるとき" },
                    "heading": { "type": "string", "description": "対象の見出し。空なら全文置換。" },
                    "memo": { "type": "string", "description": "新しい本文（Markdown）。全文置換ならメモ全体。" }
                },
                "required": ["memo"]
            }
        }
    }));
    Value::Array(tools)
}

pub fn max_tool_rounds(rounds: u32) -> usize {
    crate::db::normalize_tool_rounds(rounds) as usize
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
    pub wrote_note_id: Option<String>,
}

impl ToolExec {
    fn text(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            consumed: Vec::new(),
            wrote_note_id: None,
        }
    }
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
    let also_remote = hit.source.eq_ignore_ascii_case("remote");
    match preview_hit(state, &hit.id, also_remote) {
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
    let kind = if hit.source.eq_ignore_ascii_case("remote") {
        "remote"
    } else {
        "text"
    };
    let (row, _) = state
        .db
        .insert_llm_source_full(
            thread_id,
            "tool",
            &hit.path,
            &title,
            &hit.id,
            &body,
            query,
            "unit",
            kind,
            "",
            "",
            None,
        )
        .map_err(|e| e.to_string())?;
    consumed.push((row.id.clone(), row.cite_no));
    new_rows.push(row);
    Ok(())
}

const WEB_EMPTY_SNIPPET: &str = "（スニペットなし）";

fn web_body_looks_full(body: &str) -> bool {
    body.chars().count() > TOOL_BODY_CAP + TRUNCATED_MARK.chars().count()
}

fn persist_web_hit(
    state: &AppState,
    thread_id: &str,
    hit: &crate::llm::searxng::WebHit,
    query: &str,
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
    let (row, _) = state
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
    let (row, _) = state
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

pub fn format_calendar_system_line() -> String {
    "\n予定・期日・会議・今日・今週・明日は list_calendar を使う。資料が要るときだけ、返った件名や当事者名を search_index の query にする。search_index の「直近は30日前」はファイルとメール用で、予定には使わない。".into()
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

pub fn format_remote_index_system_line() -> String {
    "\n別PCの共有フォルダも検索対象です。ファイル出典に（リモート）が付くことがあります。メールと送信者はこのPCだけです。期間はホストが対応しているときだけ別PCにも効きます。".into()
}

/// Hybrid and host-only modes add the "shared folders are also in scope" line.
/// A remote-only thread scope already says this PC is excluded, so that line would contradict it.
pub fn include_hybrid_remote_system_line(search_mode: &str, thread_prefix: &str) -> bool {
    matches!(search_mode, "hybrid" | "remote") && !is_remote_only_thread(thread_prefix)
}

pub fn scopes_are_remote_only(scopes: &[String]) -> bool {
    scopes.iter().any(|s| search::is_remote_only_scope(s))
}

pub fn is_remote_only_thread(raw: &str) -> bool {
    scopes_are_remote_only(&parse_thread_scopes(raw))
}

/// Host index search runs beside the local one. Mail listing and a mail-folder scope stay here.
/// A remote-only thread scope queries the host even when settings are local-only.
pub fn should_run_remote_sidecar(
    search_mode: &str,
    list_mail: bool,
    mail_from: Option<&str>,
    outer_scopes: &[String],
) -> bool {
    if list_mail {
        return false;
    }
    if mail_from.map(str::trim).is_some_and(|s| !s.is_empty()) {
        return false;
    }
    if scopes_are_remote_only(outer_scopes) {
        return true;
    }
    if search_mode != "hybrid" && search_mode != "remote" {
        return false;
    }
    if !outer_scopes.is_empty()
        && outer_scopes
            .iter()
            .all(|p| p.trim().starts_with("mailfolder:"))
    {
        return false;
    }
    true
}

/// Filesystem prefixes to send to the host. Mail folders and the remote-only mark are not paths.
pub fn remote_chat_prefixes(outer: &[String]) -> Vec<String> {
    outer
        .iter()
        .map(|s| s.trim())
        .filter(|s| {
            !s.is_empty() && !s.starts_with("mailfolder:") && !search::is_remote_only_scope(s)
        })
        .map(|s| s.to_string())
        .collect()
}

pub const REMOTE_DATE_UNSUPPORTED: &str =
    "ホストが期間指定に未対応のため、別 PC の結果は使わなかった。";

/// Drop host hits when this query had a date and the host did not apply it.
pub fn remote_date_rejection(asked_date: bool, date_applied: bool) -> Option<&'static str> {
    if asked_date && !date_applied {
        Some(REMOTE_DATE_UNSUPPORTED)
    } else {
        None
    }
}

/// Keep the host's rank. Paths already returned locally are omitted. Cap is paragraphs, not files.
pub fn select_remote_chat_hits(local: &[SearchHit], remote: &[SearchHit], k: usize) -> Vec<SearchHit> {
    if k == 0 {
        return Vec::new();
    }
    let local_paths: HashSet<String> = local
        .iter()
        .map(|h| h.path.to_ascii_lowercase())
        .collect();
    let mut seen_ids = HashSet::new();
    let mut out = Vec::new();
    for hit in remote {
        if local_paths.contains(&hit.path.to_ascii_lowercase()) {
            continue;
        }
        if !seen_ids.insert(hit.id.to_ascii_lowercase()) {
            continue;
        }
        out.push(hit.clone());
        if out.len() >= k {
            break;
        }
    }
    out
}

/// Chat always retrieves locally with [`search::SearchOpts::for_llm`].
/// `remote` mode still skips this PC's files. Mail stays.
fn prepare_chat_local_search(mode: &str, mut filter: search::SearchFilter) -> search::SearchFilter {
    if mode == "remote" {
        filter.file_paths = Some(Vec::new());
    }
    filter
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
    let mut settings = state.settings.read().clone();
    let mode = settings.search_mode.clone();
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
    // Hybrid and remote modes would drop SearchOpts inside run_search_with_opts.
    // Force local so this half stays on for_llm. The host is a separate call.
    settings.search_mode = "local".into();
    let filter = prepare_chat_local_search(&mode, filter.clone());
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
        &filter,
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
    // The mark is not a folder. Searching it locally would miss, and mail stays off too.
    if scopes_are_remote_only(&scopes.outer) {
        return Ok(Vec::new());
    }
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
    if scopes_are_remote_only(prefixes) {
        return vec![search::REMOTE_ONLY_SCOPE.to_string()];
    }
    crate::pathutil::collapse_path_prefixes(prefixes)
}

/// System-prompt line so the model does not treat a scoped miss as "nothing exists".
pub fn format_thread_scope_system_line(raw: &str) -> Option<String> {
    let scopes = parse_thread_scopes(raw);
    if scopes.is_empty() {
        return None;
    }
    if scopes_are_remote_only(&scopes) {
        return Some(
            "\nこの会話の索引検索は別PCの共有フォルダだけです。このPCのファイルとメールは対象外です。"
                .into(),
        );
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

pub fn format_note_target_system_line(title: &str, memo: &str) -> String {
    let heads = crate::notes_md::outline(memo);
    let outline = if heads.is_empty() {
        "見出しはまだありません。".to_string()
    } else {
        format!("見出し: {}。", heads.join("、"))
    };
    format!(
        "\n対象ノートは「{title}」。{outline}create_note / read_note / write_note でメモを読めます。書いた内容はすぐ反映されます。採否はユーザーがノートで行います。"
    )
}

fn scope_display(scope: &str) -> String {
    if search::is_remote_only_scope(scope) {
        "リモート".into()
    } else {
        scope.to_string()
    }
}

fn scope_where_clause(scopes: &[String]) -> String {
    if scopes.is_empty() {
        String::new()
    } else {
        let shown: Vec<String> = scopes.iter().map(|s| scope_display(s)).collect();
        format!("（検索範囲: {}）", shown.join("、"))
    }
}

/// Subject of an empty search_index line. Remote-only did not search this PC.
fn empty_hit_subject(remote_only: bool, remote_note: Option<&str>) -> &'static str {
    if !remote_only && remote_note == Some(REMOTE_DATE_UNSUPPORTED) {
        "このPCの索引"
    } else {
        "索引"
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
    let mut thread = thread_scope.map(parse_thread_scopes).unwrap_or_default();
    if scopes_are_remote_only(&thread) {
        thread = vec![search::REMOTE_ONLY_SCOPE.to_string()];
    }
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
        .filter(|r| {
            !r.is_web()
                && !r.is_remote()
                && !crate::pathutil::path_starts_with(&r.path, prefix)
        })
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

fn preview_hit(
    state: &AppState,
    paragraph_id: &str,
    also_remote: bool,
) -> Result<Option<SearchHit>, String> {
    let settings = state.settings.read().clone();
    if also_remote {
        search::run_preview_also_remote(
            &settings,
            state.backend.as_ref(),
            Some(state.mail_backend.as_ref()),
            paragraph_id,
        )
    } else {
        search::run_preview(
            &settings,
            state.backend.as_ref(),
            Some(state.mail_backend.as_ref()),
            paragraph_id,
        )
    }
}

fn bound_note(state: &AppState, thread_id: &str) -> Result<crate::db::NoteRow, String> {
    let thread = state
        .db
        .get_llm_thread(thread_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "会話が見つかりません".to_string())?;
    let id = thread.note_id.trim();
    if id.is_empty() {
        return Err("対象ノートがありません。list_notes か create_note を使ってください。".into());
    }
    state
        .db
        .get_note(id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "対象ノートがありません。list_notes か create_note を使ってください。".into())
}

fn resolve_note(
    state: &AppState,
    thread_id: &str,
    args: &Value,
) -> Result<crate::db::NoteRow, String> {
    let id = args
        .get("note_id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if let Some(id) = id {
        return state
            .db
            .get_note(id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "ノートが見つかりません。".into());
    }
    bound_note(state, thread_id)
}

fn bind_thread_note(state: &AppState, thread_id: &str, note_id: &str) {
    let _ = state.db.set_llm_thread_note(thread_id, note_id);
}

fn tool_list_notes(state: &AppState) -> ToolExec {
    match state.db.list_notes() {
        Ok(list) if list.is_empty() => ToolExec::text("ノートはありません。"),
        Ok(list) => {
            let mut lines = vec!["id\t題名\tupdated_at".to_string()];
            for n in list {
                lines.push(format!(
                    "{}\t{}\t{}",
                    n.id,
                    n.title.replace(['\t', '\n'], " "),
                    n.updated_at
                ));
            }
            ToolExec::text(lines.join("\n"))
        }
        Err(e) => ToolExec::text(format!("ツールエラー: {e}")),
    }
}

fn tool_read_note(state: &AppState, thread_id: &str, args: &Value) -> ToolExec {
    let note = match resolve_note(state, thread_id, args) {
        Ok(n) => n,
        Err(e) => return ToolExec::text(e),
    };
    let heading = args
        .get("heading")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let chars = note.memo.chars().count();
    let outline = crate::notes_md::outline(&note.memo);
    let outline_line = if outline.is_empty() {
        "見出し: （なし）".to_string()
    } else {
        format!("見出し: {}", outline.join("、"))
    };
    let head = format!(
        "ノート『{}』 id={}\n{outline_line}\n文字数: {chars}",
        note.title, note.id
    );
    if let Some(h) = heading {
        let secs = crate::notes_md::split_sections(&note.memo);
        let text = match crate::notes_md::find_section(&secs, h) {
            Ok(s) => s.text.clone(),
            Err(e) => {
                return ToolExec::text(format!("{}\n{head}", e.message(h)));
            }
        };
        let body = cap_chars(&text, crate::notes_md::NOTE_FULL_CHAR_CAP);
        return ToolExec::text(format!("{head}\n\n{body}"));
    }
    if chars > crate::notes_md::NOTE_FULL_CHAR_CAP {
        return ToolExec::text(format!(
            "{head}\n\nノートが長いため全文は返しません。heading を指定して区画ごとに読み、write_note も heading 付きで書いてください。"
        ));
    }
    ToolExec::text(format!("{head}\n\n{}", note.memo))
}

fn tool_create_note(state: &AppState, thread_id: &str, args: &Value) -> ToolExec {
    let memo = args
        .get("memo")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim_end();
    if memo.trim().is_empty() {
        return ToolExec::text("memo が空です。ノートは作りません。");
    }
    let title = args
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let note = match state.db.create_note(title) {
        Ok(n) => n,
        Err(e) => return ToolExec::text(format!("ノートの作成に失敗しました: {e}")),
    };
    match state
        .db
        .apply_llm_note_write(&note.id, memo, None, None)
    {
        Ok(note) => {
            bind_thread_note(state, thread_id, &note.id);
            ToolExec {
                content: format!(
                    "ノート『{}』を作成してメモに書きました。id={}。反映済みです。採否はユーザーがノートで行います。",
                    note.title, note.id
                ),
                consumed: Vec::new(),
                wrote_note_id: Some(note.id),
            }
        }
        Err(e) => ToolExec::text(format!("ノートの作成に失敗しました: {e}")),
    }
}

fn tool_write_note(state: &AppState, thread_id: &str, args: &Value) -> ToolExec {
    let note = match resolve_note(state, thread_id, args) {
        Ok(n) => n,
        Err(e) => return ToolExec::text(e),
    };
    let memo = args
        .get("memo")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim_end();
    if memo.trim().is_empty() {
        return ToolExec::text("memo が空です。書き込みません。");
    }
    let heading = args
        .get("heading")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let title = args
        .get("title")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    match state
        .db
        .apply_llm_note_write(&note.id, memo, heading, title)
    {
        Ok(note) => {
            bind_thread_note(state, thread_id, &note.id);
            let content = format!(
                "ノート『{}』のメモを更新しました。id={}。反映済みです。採否はユーザーがノートで行います。",
                note.title, note.id
            );
            ToolExec {
                content,
                consumed: Vec::new(),
                wrote_note_id: Some(note.id),
            }
        }
        Err(e) => ToolExec::text(e),
    }
}

fn tool_list_calendar(state: &AppState, args: &Value) -> ToolExec {
    let settings = state.settings.read().clone();
    if !settings.calendar_enabled {
        return ToolExec::text("予定表は設定でオフです。設定の予定表をオンにして同期してください。");
    }
    let now = chrono::Local::now();
    let synced = crate::mail::calendar::sync_window_filter(
        settings.calendar_days_back,
        settings.calendar_days_ahead,
        now,
    );
    let after = args.get("after").and_then(|v| v.as_str());
    let before = args.get("before").and_then(|v| v.as_str());
    let requested = if after.map(str::trim).unwrap_or("").is_empty()
        && before.map(str::trim).unwrap_or("").is_empty()
    {
        crate::mail::calendar::default_list_filter(now)
    } else {
        match crate::search::parse_date_range(after, before) {
            Ok(f) => f,
            Err(e) => return ToolExec::text(e),
        }
    };
    let terms = crate::mail::calendar::whitespace_terms(
        args.get("query").and_then(|v| v.as_str()).unwrap_or(""),
    );
    if settings.calendar_last_sync_at.trim().is_empty() {
        return ToolExec::text("予定表はまだ同期されていません。設定から同期してください。");
    }
    let rows = match state.db.list_calendar_events() {
        Ok(r) => r,
        Err(e) => return ToolExec::text(format!("予定表を読めません: {e}")),
    };
    let mut views: Vec<_> = rows
        .iter()
        .map(|row| {
            let appt = crate::mail::calendar::OutlookAppointment {
                store_id: row.store_id.clone(),
                entry_id: row.entry_id.clone(),
                folder_entry_id: row.folder_entry_id.clone(),
                calendar_name: row.calendar_name.clone(),
                subject: row.subject.clone(),
                location: row.location.clone(),
                organizer: row.organizer.clone(),
                attendees: row.attendees.clone(),
                categories: row.categories.clone(),
                body: row.body.clone(),
                start_unix: row.start_unix,
                end_unix: row.end_unix,
                all_day: row.all_day,
                busy_status: row.busy_status,
                private: row.private,
            };
            crate::mail::calendar::to_view(&appt)
        })
        .filter(|v| crate::mail::calendar::in_date_filter(v.start_unix, requested))
        .filter(|v| crate::mail::calendar::event_matches_terms(v, &terms))
        .collect();
    views = crate::mail::calendar::sort_by_start(views);
    let mut head = String::new();
    if crate::mail::calendar::range_outside_sync(requested, synced) {
        let from = synced
            .after_unix
            .map(crate::mail::calendar::ymd_of_unix)
            .unwrap_or_default();
        let to = synced
            .before_unix
            .map(crate::mail::calendar::ymd_of_unix)
            .unwrap_or_default();
        head.push_str(&format!(
            "問い合わせ範囲の一部は同期範囲（{from}〜{to}）の外です。範囲内の予定だけを返します。\n"
        ));
    }
    if settings.calendar_truncated {
        head.push_str("一部の予定表は件数上限で打ち切られています。\n");
    }
    if views.is_empty() {
        head.push_str("この期間の予定はありません。");
        return ToolExec::text(head);
    }
    let mut lines = Vec::new();
    for v in views {
        let when = if v.all_day {
            format!("{} 終日", v.start)
        } else if v.end.is_empty() {
            v.start.clone()
        } else {
            format!("{}–{}", v.start, v.end)
        };
        let mut line = format!("- {when} {}", v.subject);
        if !v.private {
            let mut extra = Vec::new();
            if !v.location.is_empty() {
                extra.push(v.location.clone());
            }
            if !v.organizer.is_empty() {
                extra.push(format!("主催 {}", v.organizer));
            }
            if !v.attendees.is_empty() {
                extra.push(v.attendees.clone());
            }
            if !v.categories.is_empty() {
                extra.push(v.categories.clone());
            }
            if !v.calendar_name.is_empty() {
                extra.push(v.calendar_name.clone());
            }
            if !extra.is_empty() {
                line.push_str(&format!("（{}）", extra.join("、")));
            }
            if !v.body.is_empty() {
                line.push('\n');
                line.push_str(&v.body);
            }
        } else {
            line.push_str("（非公開）");
        }
        lines.push(line);
    }
    head.push_str(&lines.join("\n"));
    ToolExec::text(head)
}

fn arg_i64(value: Option<&serde_json::Value>) -> Option<i64> {
    let value = value?;
    if let Some(n) = value.as_i64() {
        return Some(n);
    }
    if let Some(n) = value.as_u64() {
        return i64::try_from(n).ok();
    }
    value.as_str().and_then(|text| text.trim().parse::<i64>().ok())
}

/// Re-read a source already stored on this thread. Does not allocate a citation number.
pub fn read_saved_cite(db: &crate::db::Db, thread_id: &str, cite_no: i64) -> ToolExec {
    read_saved_selection(db, thread_id, cite_no, &json!({}))
}

/// `cite_no` plus optional `articles` or `start`/`end`. Omitting both returns the saved text.
///
/// Article and range slices are returned whole. The caller applies the tool-result
/// budget (`fit_tool_text`) the same way as a full re-read.
fn read_saved_selection(
    db: &crate::db::Db,
    thread_id: &str,
    cite_no: i64,
    args: &Value,
) -> ToolExec {
    if cite_no <= 0 {
        return ToolExec::text("出典番号が不正です。");
    }
    let articles = match parse_article_args(args.get("articles")) {
        Ok(articles) => articles,
        Err(message) => return ToolExec::text(message),
    };
    let rows = match db.list_llm_sources_by_cite(thread_id, cite_no) {
        Ok(rows) => rows,
        Err(e) => return ToolExec::text(format!("出典を読めません: {e}")),
    };
    if rows.is_empty() {
        return ToolExec::text(format!("出典 [{cite_no}] はこの会話にありません。"));
    }
    let consumed = rows
        .iter()
        .filter(|row| row.is_pending())
        .map(|row| (row.id.clone(), row.cite_no))
        .collect();
    if !articles.is_empty() {
        return read_saved_articles(cite_no, &rows, &articles, consumed);
    }
    if range_arg_active(args.get("start")) || range_arg_active(args.get("end")) {
        return read_saved_range(cite_no, &rows, args, consumed);
    }
    ToolExec {
        content: format_sources(&rows),
        consumed,
        wrote_note_id: None,
    }
}

fn parse_article_args(value: Option<&Value>) -> Result<Vec<crate::llm::articles::ArticleNo>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    if value.is_null() {
        return Ok(Vec::new());
    }
    let items: Vec<&Value> = match value {
        Value::Array(items) => items.iter().collect(),
        other => vec![other],
    };
    let mut out = Vec::new();
    for item in items {
        let token = article_item_token(item)?;
        if token.trim().is_empty() {
            continue;
        }
        let Some(no) = crate::llm::articles::parse_article_token(&token) else {
            return Err(format!(
                "条番号として読めません: {token}。6 や \"6の2\" のように指定してください。"
            ));
        };
        if !out.contains(&no) {
            out.push(no);
        }
    }
    Ok(out)
}

fn article_item_token(value: &Value) -> Result<String, String> {
    if value.is_null() {
        return Ok(String::new());
    }
    if let Some(n) = value.as_i64() {
        if n <= 0 {
            return Ok(String::new());
        }
        return Ok(n.to_string());
    }
    if let Some(n) = value.as_u64() {
        if n == 0 || n > u32::MAX as u64 {
            return Ok(String::new());
        }
        return Ok(n.to_string());
    }
    if let Some(text) = value.as_str() {
        return Ok(text.to_string());
    }
    Err("条番号は整数か文字列で指定してください。".into())
}

fn saved_plain_text(rows: &[LlmSourceRow]) -> String {
    rows.iter()
        .map(|row| row.body.trim())
        .filter(|body| !body.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Text the model sees inside a 出典 block. Multi-page OCR includes `## nページ目`.
fn saved_visible_text(rows: &[LlmSourceRow]) -> String {
    let refs: Vec<&LlmSourceRow> = rows.iter().collect();
    let pages = refs
        .iter()
        .filter(|row| crate::llm::transcript::parse_pdf_page_no(&row.paragraph_id).is_some())
        .count();
    if pages >= 2 {
        return crate::llm::transcript::transcript_body(&refs, true);
    }
    saved_plain_text(rows)
}

fn range_arg_active(value: Option<&Value>) -> bool {
    let Some(value) = value else {
        return false;
    };
    if value.is_null() {
        return false;
    }
    match arg_i64(Some(value)) {
        Some(n) => n > 0,
        None => !value.as_str().is_some_and(|text| text.trim().is_empty()),
    }
}

#[derive(Debug)]
pub(crate) enum ReadRoute {
    Saved(i64),
    Paragraph(String),
    NeedCite,
    Empty,
}

pub(crate) fn route_read_unit(args: &Value) -> ReadRoute {
    let cite = arg_i64(args.get("cite_no")).filter(|n| *n > 0);
    let articles_on = match parse_article_args(args.get("articles")) {
        Ok(list) => !list.is_empty(),
        Err(_) => true,
    };
    let range_on = range_arg_active(args.get("start")) || range_arg_active(args.get("end"));
    if cite.is_some() || articles_on || range_on {
        return match cite {
            Some(n) => ReadRoute::Saved(n),
            None => ReadRoute::NeedCite,
        };
    }
    let paragraph_id = args
        .get("paragraph_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if paragraph_id.is_empty() {
        ReadRoute::Empty
    } else {
        ReadRoute::Paragraph(paragraph_id)
    }
}

fn format_saved_body(row: &LlmSourceRow, body: &str) -> String {
    let mut shown = row.clone();
    shown.body = body.to_string();
    format_sources(std::slice::from_ref(&shown))
}

fn finish_read(content: impl Into<String>, consumed: Vec<(String, i64)>) -> ToolExec {
    ToolExec {
        content: content.into(),
        consumed,
        wrote_note_id: None,
    }
}

fn read_saved_articles(
    cite_no: i64,
    rows: &[LlmSourceRow],
    articles: &[crate::llm::articles::ArticleNo],
    consumed: Vec<(String, i64)>,
) -> ToolExec {
    let text = saved_visible_text(rows);
    if text.is_empty() {
        return finish_read(format!("出典 [{cite_no}] の本文は空です。"), consumed);
    }
    let spans = crate::llm::articles::article_spans(&text);
    if spans.is_empty() {
        return finish_read(
            format!(
                "出典 [{cite_no}] に条の見出し（第n条）がありません。start と end で文字範囲を指定してください（1文字目から、end は含みます。全{}文字）。",
                text.chars().count()
            ),
            consumed,
        );
    }
    let present: HashSet<_> = spans.iter().map(|span| span.no).collect();
    let missing: Vec<_> = articles
        .iter()
        .copied()
        .filter(|no| !present.contains(no))
        .collect();
    let found: Vec<_> = spans
        .iter()
        .filter(|span| articles.contains(&span.no))
        .collect();
    let available = crate::llm::articles::available_labels(&spans).join("、");
    if found.is_empty() {
        let missing_labels = missing
            .iter()
            .map(|no| no.label())
            .collect::<Vec<_>>()
            .join("、");
        return finish_read(
            format!("出典 [{cite_no}] に{missing_labels}はありません。ある条番号: {available}。"),
            consumed,
        );
    }
    let mut content = String::new();
    if !missing.is_empty() {
        let missing_labels = missing
            .iter()
            .map(|no| no.label())
            .collect::<Vec<_>>()
            .join("、");
        content.push_str(&format!(
            "出典 [{cite_no}] に{missing_labels}はありません。ある条番号: {available}。\n"
        ));
    }
    content.push_str(&format_saved_body(&rows[0], &pack_article_blocks(&found)));
    finish_read(content, consumed)
}

fn pack_article_blocks(found: &[&crate::llm::articles::ArticleSpan]) -> String {
    let mut used = 0usize;
    let mut out = String::new();
    for span in found {
        let label = span.display_label();
        let span_len = span.text.chars().count();
        let full_end = span.start_char + span_len.saturating_sub(1);
        let header = format!(
            "［再読］{label}。{}文字目から{full_end}文字目。全{span_len}文字\n",
            span.start_char
        );
        let block_len = header.chars().count() + span_len + 1;
        if used > 0 && used + block_len > READ_BODY_CAP {
            let room = READ_BODY_CAP.saturating_sub(used);
            if room < header.chars().count() + 8 {
                break;
            }
            let take = room.saturating_sub(header.chars().count() + 24).max(1);
            let take = take.min(span_len);
            let prefix: String = span.text.chars().take(take).collect();
            let end = span.start_char + prefix.chars().count() - 1;
            out.push_str(&format!(
                "［再読］{label}の先頭。{}文字目から{end}文字目。全{span_len}文字。続きは start={}\n{prefix}\n",
                span.start_char,
                end + 1
            ));
            break;
        }
        if span_len > READ_BODY_CAP && used == 0 {
            let prefix: String = span.text.chars().take(READ_BODY_CAP).collect();
            let end = span.start_char + prefix.chars().count() - 1;
            out.push_str(&format!(
                "［再読］{label}の先頭。{}文字目から{end}文字目。全{span_len}文字。続きは start={}\n{prefix}\n",
                span.start_char,
                end + 1
            ));
            break;
        }
        out.push_str(&header);
        out.push_str(&span.text);
        out.push('\n');
        used += block_len;
    }
    out
}

fn read_saved_range(
    cite_no: i64,
    rows: &[LlmSourceRow],
    args: &Value,
    consumed: Vec<(String, i64)>,
) -> ToolExec {
    let text = saved_visible_text(rows);
    if text.is_empty() {
        return finish_read(format!("出典 [{cite_no}] の本文は空です。"), consumed);
    }
    let start = match optional_char_pos(args.get("start"), "start") {
        Ok(Some(start)) => start,
        Ok(None) => 1,
        Err(message) => return finish_read(message, consumed),
    };
    let end = match optional_char_pos(args.get("end"), "end") {
        Ok(end) => end,
        Err(message) => return finish_read(message, consumed),
    };
    match crate::llm::articles::char_range(&text, start, end) {
        Ok(range) => {
            let pages = text.contains("## ") && text.contains("ページ目");
            let (body, _, capped) = cap_delivered_slice(&range.text, range.start, READ_BODY_CAP);
            let delivered_end = range.start + body.chars().count().saturating_sub(1);
            let note = range_note(range.start, delivered_end, range.len, capped, pages);
            finish_read(
                format_saved_body(&rows[0], &format!("{note}\n{body}")),
                consumed,
            )
        }
        Err(err) => finish_read(char_range_message(err), consumed),
    }
}

fn cap_delivered_slice(text: &str, start: usize, cap: usize) -> (String, usize, bool) {
    let len = text.chars().count();
    if len <= cap {
        return (text.to_string(), start + len.saturating_sub(1), false);
    }
    let body: String = text.chars().take(cap).collect();
    (body, start + cap - 1, true)
}

fn range_note(start: usize, end: usize, total: usize, more: bool, pages: bool) -> String {
    let mut note = format!("［再読］{start}文字目から{end}文字目。全{total}文字");
    if pages {
        note.push_str("。ページ見出しを含む");
    }
    if more {
        note.push_str(&format!("。続きは start={}", end.saturating_add(1)));
    }
    note
}

fn optional_char_pos(value: Option<&Value>, name: &str) -> Result<Option<usize>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let Some(n) = arg_i64(Some(value)) else {
        return Err(format!(
            "{name} は 1 以上の文字位置です（1文字目から数えます）。"
        ));
    };
    if n <= 0 {
        return Ok(None);
    }
    let n = usize::try_from(n).map_err(|_| format!("{name} が大きすぎます。"))?;
    Ok(Some(n))
}

fn char_range_message(err: crate::llm::articles::CharRangeError) -> String {
    use crate::llm::articles::CharRangeError;
    match err {
        CharRangeError::Start => "start は 1 以上の文字位置です（1文字目から数えます）。".into(),
        CharRangeError::Order => "end は start 以上にしてください。".into(),
        CharRangeError::PastEnd { len } => {
            format!("start が文書の長さ（{len}文字）を超えています。")
        }
    }
}

/// If `text` is a re-read result that does not fit in `max_chars`, rewrite the
/// `［再読］` claim so it describes only the characters that remain.
pub(crate) fn refit_reread_text(text: &str, max_chars: usize) -> Option<String> {
    let marker = "［再読］";
    let pos = text.find(marker)?;
    if max_chars == 0 {
        return Some(String::new());
    }
    if text.chars().count() <= max_chars {
        return Some(text.to_string());
    }
    let preamble = &text[..pos];
    let sections = split_reread_sections(&text[pos..]);
    if sections.is_empty() {
        return None;
    }
    if sections.len() == 1 && section_is_range(&sections[0].header) {
        return Some(refit_range_section(preamble, &sections[0], max_chars));
    }
    Some(refit_article_sections(preamble, &sections, max_chars))
}

struct RereadSection {
    header: String,
    body: String,
}

fn split_reread_sections(text: &str) -> Vec<RereadSection> {
    let mut sections = Vec::new();
    let mut current: Option<RereadSection> = None;
    for line in text.split_inclusive('\n') {
        if line.starts_with("［再読］") {
            if let Some(done) = current.take() {
                sections.push(done);
            }
            let header = line.trim_end_matches(['\n', '\r']).to_string();
            current = Some(RereadSection {
                header,
                body: String::new(),
            });
        } else if let Some(section) = current.as_mut() {
            section.body.push_str(line);
        }
    }
    if let Some(done) = current {
        sections.push(done);
    }
    for section in &mut sections {
        let trimmed = section.body.trim_end();
        section.body = trimmed.to_string();
    }
    sections
}

fn section_is_range(header: &str) -> bool {
    header
        .trim_start_matches("［再読］")
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_digit())
}

fn refit_range_section(preamble: &str, section: &RereadSection, max_chars: usize) -> String {
    let Some((start, _end, total, pages)) = parse_range_header(&section.header) else {
        return trim_plain(preamble, &section.header, &section.body, max_chars);
    };
    let body: Vec<char> = section.body.chars().collect();
    let mut best = String::new();
    for k in 0..=body.len() {
        let end = if k == 0 { start.saturating_sub(1) } else { start + k - 1 };
        let more = k < body.len() || end < total;
        let note = if k == 0 {
            format!("［再読］全{total}文字。続きは start={start}")
        } else {
            range_note(start, end, total, more, pages)
        };
        let text_body: String = body.iter().take(k).collect();
        let rendered = if k == 0 {
            format!("{preamble}{note}")
        } else {
            format!("{preamble}{note}\n{text_body}")
        };
        if rendered.chars().count() <= max_chars {
            best = rendered;
        } else {
            break;
        }
    }
    if best.is_empty() {
        let note = format!("［再読］全{total}文字。続きは start={start}");
        let note_len = note.chars().count();
        if note_len >= max_chars {
            return note.chars().take(max_chars).collect();
        }
        let head: String = preamble.chars().take(max_chars - note_len).collect();
        format!("{head}{note}")
    } else {
        best
    }
}

fn parse_range_header(header: &str) -> Option<(usize, usize, usize, bool)> {
    let rest = header.trim_start_matches("［再読］");
    let (start_s, rest) = rest.split_once("文字目から")?;
    let (end_s, rest) = rest.split_once("文字目。全")?;
    let total_s = rest.split("文字").next()?;
    let start = start_s.trim().parse().ok()?;
    let end = end_s.trim().parse().ok()?;
    let total = total_s.trim().parse().ok()?;
    let pages = header.contains("ページ見出し");
    Some((start, end, total, pages))
}

fn refit_article_sections(preamble: &str, sections: &[RereadSection], max_chars: usize) -> String {
    let mut out = preamble.to_string();
    for section in sections {
        let full = format!("{}\n{}\n", section.header, section.body);
        if out.chars().count() + full.chars().count() <= max_chars {
            out.push_str(&full);
            continue;
        }
        if let Some((start, _end, total, label)) = parse_article_header(&section.header) {
            let body: Vec<char> = section.body.chars().collect();
            let mut best: Option<String> = None;
            for k in 0..=body.len() {
                let piece = render_article_piece(&label, start, &body, k, total);
                let rendered = format!("{out}{piece}");
                if rendered.chars().count() <= max_chars {
                    best = Some(rendered);
                } else {
                    break;
                }
            }
            if let Some(best) = best {
                return best;
            }
            let note = format!("［再読］{label}。続きは start={start}\n");
            return append_notice_without_lying(out, &note, max_chars);
        }
        break;
    }
    if out.chars().count() > max_chars {
        append_notice_without_lying(out, "", max_chars)
    } else {
        out
    }
}

/// The next article's notice did not fit. Shrink the last accepted article and
/// rewrite its header, instead of cutting its body while it still claims the full range.
fn append_notice_without_lying(accepted: String, next_note: &str, max_chars: usize) -> String {
    let note = if next_note.is_empty() {
        String::new()
    } else if accepted.ends_with('\n') {
        next_note.to_string()
    } else {
        format!("\n{next_note}")
    };
    let room = max_chars.saturating_sub(note.chars().count());
    if let Some(shrunk) = shrink_last_article(&accepted, room) {
        let joined = format!("{shrunk}{note}");
        if joined.chars().count() <= max_chars {
            return joined;
        }
    }
    if accepted.chars().count() <= max_chars {
        return accepted;
    }
    shrink_last_article(&accepted, max_chars)
        .unwrap_or_else(|| accepted.chars().take(max_chars).collect())
}

fn shrink_last_article(text: &str, room: usize) -> Option<String> {
    if room == 0 {
        return Some(String::new());
    }
    let marker_at = text.rfind("［再読］")?;
    if marker_at > 0 && !text[..marker_at].ends_with('\n') {
        return None;
    }
    let prefix = &text[..marker_at];
    let section = &text[marker_at..];
    let (header, body) = section.split_once('\n')?;
    let (start, _end, total, label) = parse_article_header(header)?;
    let chars: Vec<char> = body.trim_end().chars().collect();
    let prefix_len = prefix.chars().count();
    if prefix_len > room {
        return None;
    }
    let mut best: Option<String> = None;
    for k in 0..=chars.len() {
        let piece = render_article_piece(&label, start, &chars, k, total);
        if prefix_len + piece.chars().count() <= room {
            best = Some(format!("{prefix}{piece}"));
        } else {
            break;
        }
    }
    best
}

fn render_article_piece(label: &str, start: usize, body: &[char], k: usize, total: usize) -> String {
    if k == 0 {
        return format!("［再読］{label}。続きは start={start}\n");
    }
    let end = start + k - 1;
    let text: String = body.iter().take(k).collect();
    if k >= total {
        format!("［再読］{label}。{start}文字目から{end}文字目。全{total}文字\n{text}\n")
    } else {
        format!(
            "［再読］{label}の先頭。{start}文字目から{end}文字目。全{total}文字。続きは start={}\n{text}\n",
            end + 1
        )
    }
}

fn parse_article_header(header: &str) -> Option<(usize, usize, usize, String)> {
    let rest = header.trim_start_matches("［再読］");
    let (label, rest) = rest.split_once('。')?;
    let label = label.trim_end_matches("の先頭").to_string();
    let (start_s, rest) = rest.split_once("文字目から")?;
    let (end_s, rest) = rest.split_once("文字目。全")?;
    let total_s = rest.split("文字").next()?;
    Some((
        start_s.trim().parse().ok()?,
        end_s.trim().parse().ok()?,
        total_s.trim().parse().ok()?,
        label,
    ))
}

fn trim_plain(preamble: &str, header: &str, body: &str, max_chars: usize) -> String {
    let mut out = format!("{preamble}{header}\n{body}");
    if out.chars().count() > max_chars {
        out = out.chars().take(max_chars).collect();
    }
    out
}

pub fn execute_tool(
    state: &AppState,
    thread_id: &str,
    name: &str,
    arguments: &str,
    thread_scope: Option<&str>,
    web_search: bool,
    request_id: &str,
) -> ToolExec {
    match execute_tool_inner(
        state,
        thread_id,
        name,
        arguments,
        thread_scope,
        web_search,
        request_id,
    ) {
        Ok(exec) => exec,
        Err(e) => ToolExec {
            content: format!("ツールエラー: {e}"),
            consumed: Vec::new(),
            wrote_note_id: None,
        },
    }
}

fn execute_tool_inner(
    state: &AppState,
    thread_id: &str,
    name: &str,
    arguments: &str,
    thread_scope: Option<&str>,
    web_search: bool,
    _request_id: &str,
) -> Result<ToolExec, String> {
    let args: Value = serde_json::from_str(arguments).unwrap_or_else(|_| json!({}));
    match name {
        TOOL_LIST_CALENDAR => Ok(tool_list_calendar(state, &args)),
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
                    wrote_note_id: None,
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
                        wrote_note_id: None,
                    });
                }
            };
            let sort_date = args
                .get("sort")
                .and_then(|v| v.as_str())
                .is_some_and(|s| s.eq_ignore_ascii_case("date"));
            let topical = topical_query(&query, mail_from);
            let list_mail = mail_from.is_some() && topical.is_empty();
            if scopes_are_remote_only(&scopes.outer) && (list_mail || mail_from.is_some()) {
                return Ok(ToolExec::text(
                    "メールはこの検索範囲の対象外です。別PCの共有ファイルだけが対象です。",
                ));
            }
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
            let run_remote = should_run_remote_sidecar(
                &state.settings.read().search_mode,
                list_mail,
                mail_from,
                &scopes.outer,
            );
            let remote_job = if run_remote {
                let settings = state.settings.read().clone();
                let q = search_q.to_string();
                let prefixes = remote_chat_prefixes(&scopes.outer);
                let user_dict = state.user_dict.read().clone();
                let after_s = after
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string);
                let before_s = before
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string);
                let k_remote = k;
                Some(std::thread::spawn(move || {
                    let rewritten = search::apply_user_dictionary(&q, &user_dict);
                    let remote = search::RemoteArgosBackend::from_settings(&settings)?;
                    remote.search_chat(
                        &rewritten,
                        k_remote,
                        &prefixes,
                        search::SearchOpts::for_llm(UNITS_PER_FILE),
                        after_s.as_deref(),
                        before_s.as_deref(),
                        None,
                    )
                }))
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
            let (remote_hits, remote_note) = match remote_job {
                Some(job) => match job.join() {
                    Ok(Ok(found)) => {
                        let note = remote_date_rejection(date.is_active(), found.date_applied)
                            .map(str::to_string);
                        let kept = if note.is_some() {
                            Vec::new()
                        } else {
                            select_remote_chat_hits(&hits, &found.hits, k)
                        };
                        (kept, note)
                    }
                    Ok(Err(e)) => (Vec::new(), Some(format!("リモート検索に失敗: {e}"))),
                    Err(_) => (
                        Vec::new(),
                        Some("リモート検索スレッドが失敗しました。".into()),
                    ),
                },
                None => (Vec::new(), None),
            };
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
                    &mut new_rows,
                    &mut already,
                    &mut consumed,
                )?;
            }
            for hit in &remote_hits {
                persist_hit(
                    state,
                    thread_id,
                    hit,
                    &query,
                    TOOL_BODY_CAP,
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
            if hits.is_empty() && remote_hits.is_empty() {
                // Only the hard boundary explains an empty result. Naming the preferred
                // folder would read as "not in that folder", which is not what was searched.
                // A host that ignored the date is not "zero hits on the other PC".
                let where_ = scope_where_clause(&scopes.outer);
                let period = date_where_clause(after, before);
                let remote_only = scopes_are_remote_only(&scopes.outer);
                let which = empty_hit_subject(remote_only, remote_note.as_deref());
                let mut msg = format!(
                    "「{query}」に一致する{which}ヒットはありません{period}{where_}。"
                );
                if date.is_active() {
                    msg.push_str("この期間にヒットがなければ after を過去へ広げて再検索してください。");
                    if !remote_only {
                        let days = state.settings.read().mail_days_back.max(1);
                        if date_reaches_before_sync(date, days) {
                            msg.push_str(&format!(
                                "メールの同期範囲は過去{days}日です。それより前は索引にありません。"
                            ));
                        }
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
            if let Some(note) = more_matches_note(&remote_hits) {
                content.push('\n');
                content.push_str(&note);
            }
            if let Some(note) = remote_note {
                if !content.is_empty() {
                    content.push('\n');
                }
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
            Ok(ToolExec {
                content,
                consumed,
                wrote_note_id: None,
            })
        }
        TOOL_READ => {
            let paragraph_id = match route_read_unit(&args) {
                ReadRoute::NeedCite => {
                    return Ok(ToolExec::text(
                        "cite_no に出典番号 [n] を指定してください。",
                    ));
                }
                ReadRoute::Saved(cite_no) => {
                    return Ok(read_saved_selection(&state.db, thread_id, cite_no, &args));
                }
                ReadRoute::Empty => {
                    return Ok(ToolExec {
                        content: "paragraph_id が空です。".into(),
                        consumed: Vec::new(),
                        wrote_note_id: None,
                    });
                }
                ReadRoute::Paragraph(id) => id,
            };
            let also_remote = thread_scope.is_some_and(is_remote_only_thread);
            let Some(hit) = preview_hit(state, &paragraph_id, also_remote)? else {
                return Ok(ToolExec {
                    content: format!("段落 {paragraph_id} は索引にありません。"),
                    consumed: Vec::new(),
                    wrote_note_id: None,
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
                    if let Some(n) = preview_hit(state, &nid, also_remote)? {
                        persist_hit(
                            state,
                            thread_id,
                            &n,
                            "",
                            READ_BODY_CAP,
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
            Ok(ToolExec {
                content,
                consumed,
                wrote_note_id: None,
            })
        }
        TOOL_READ_URL => {
            if !web_search {
                return Ok(ToolExec {
                    content: "ウェブ検索がオフです。".into(),
                    consumed: Vec::new(),
                    wrote_note_id: None,
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
                    wrote_note_id: None,
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
                        wrote_note_id: None,
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
                        wrote_note_id: None,
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
            Ok(ToolExec {
                content,
                consumed,
                wrote_note_id: None,
            })
        }
        TOOL_LIST_NOTES => Ok(tool_list_notes(state)),
        TOOL_READ_NOTE => Ok(tool_read_note(state, thread_id, &args)),
        TOOL_CREATE_NOTE => Ok(tool_create_note(state, thread_id, &args)),
        TOOL_WRITE_NOTE => Ok(tool_write_note(state, thread_id, &args)),
        other => Ok(ToolExec {
            content: format!("未知のツールです: {other}"),
            consumed: Vec::new(),
            wrote_note_id: None,
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
    fn remote_sidecar_skips_local_mode_mail_and_mail_folders() {
        assert!(!should_run_remote_sidecar("local", false, None, &[]));
        assert!(should_run_remote_sidecar("hybrid", false, None, &[]));
        assert!(should_run_remote_sidecar("remote", false, None, &[]));
        assert!(!should_run_remote_sidecar(
            "hybrid",
            false,
            Some("Aさん"),
            &[]
        ));
        assert!(!should_run_remote_sidecar("hybrid", true, None, &[]));
        assert!(!should_run_remote_sidecar(
            "hybrid",
            false,
            None,
            &["mailfolder:受信トレイ".into()]
        ));
        assert_eq!(
            remote_chat_prefixes(&["mailfolder:受信トレイ".into(), r"C:\cases".into()]),
            vec![r"C:\cases".to_string()]
        );
    }

    #[test]
    fn remote_only_scope_is_exclusive_and_not_a_host_folder() {
        let mark = search::REMOTE_ONLY_SCOPE;
        let joined = join_thread_scopes(&[
            mark.into(),
            r"C:\cases".into(),
            "mailfolder:受信トレイ".into(),
        ]);
        assert_eq!(joined, mark);
        assert!(
            remote_chat_prefixes(&[mark.to_string()]).is_empty(),
            "the mark must not be sent as a path prefix"
        );
        assert!(should_run_remote_sidecar(
            "local",
            false,
            None,
            &[mark.to_string()]
        ));
        assert!(!should_run_remote_sidecar(
            "local",
            true,
            None,
            &[mark.to_string()]
        ));
        assert!(!should_run_remote_sidecar(
            "local",
            false,
            Some("Aさん"),
            &[mark.to_string()]
        ));
        assert_eq!(
            resolve_scopes(Some(mark), Some(r"C:\cases")),
            scopes(&[mark], None),
            "a local folder from the model is outside this scope"
        );
        let line = format_thread_scope_system_line(mark).expect("line");
        assert!(line.contains("別PCの共有フォルダだけ"));
        assert!(!line.contains(mark), "do not quote the mark as a folder");
        assert!(!include_hybrid_remote_system_line("hybrid", mark));
        assert!(include_hybrid_remote_system_line("hybrid", r"C:\cases"));
        assert_eq!(
            scope_where_clause(&[mark.to_string()]),
            "（検索範囲: リモート）"
        );
        assert_eq!(
            empty_hit_subject(true, Some(REMOTE_DATE_UNSUPPORTED)),
            "索引"
        );
        assert_eq!(
            empty_hit_subject(false, Some(REMOTE_DATE_UNSUPPORTED)),
            "このPCの索引"
        );
        let schema = tools_schema(false, true, true).to_string();
        assert!(schema.contains("別PCの共有ファイルだけ"));
        assert!(!schema.contains("（ファイルとメール）"));
        assert!(!schema.contains("メールと送信者はこのPCだけ"));
    }

    #[test]
    fn dated_remote_without_flag_is_dropped_with_a_note() {
        assert_eq!(
            remote_date_rejection(true, false),
            Some(REMOTE_DATE_UNSUPPORTED)
        );
        assert!(remote_date_rejection(true, true).is_none());
        assert!(remote_date_rejection(false, false).is_none());
    }

    #[test]
    fn remote_hits_drop_local_paths_and_cap_at_k() {
        let local = vec![file_hit(r"C:\cases\a.txt", 9.0)];
        let mut remote_a = file_hit(r"C:\cases\a.txt", 8.0);
        remote_a.source = "remote".into();
        let mut remote_b = file_hit(r"D:\share\b.txt", 7.0);
        remote_b.source = "remote".into();
        let mut remote_b2 = file_hit(r"D:\share\b.txt", 6.0);
        remote_b2.id = r"D:\share\b.txt#2".into();
        remote_b2.source = "remote".into();
        let mut remote_c = file_hit(r"D:\share\c.txt", 5.0);
        remote_c.source = "remote".into();
        let kept = select_remote_chat_hits(&local, &[remote_a, remote_b, remote_b2, remote_c], 2);
        assert_eq!(
            kept.iter().map(|h| h.id.as_str()).collect::<Vec<_>>(),
            vec![r"D:\share\b.txt#1", r"D:\share\b.txt#2"]
        );
    }

    #[test]
    fn hybrid_chat_search_keeps_local_files_on_the_precise_path() {
        let filter = prepare_chat_local_search("hybrid", search::SearchFilter::default());
        assert!(filter.file_paths.is_none());
        let remote_only = prepare_chat_local_search("remote", search::SearchFilter::default());
        assert_eq!(remote_only.file_paths.as_deref(), Some(&[][..]));
    }

    #[test]
    fn outside_folder_note_skips_remote_sources() {
        let rows = vec![
            source_row("text", r"C:\other\a.txt", 1),
            source_row("remote", r"D:\host\b.txt", 2),
            source_row("web", "https://example.com", 3),
        ];
        let note = outside_folder_note(r"C:\cases", &rows).expect("local outside");
        assert!(note.contains("[1]"), "{note}");
        assert!(!note.contains("[2]"), "{note}");
        assert!(!note.contains("[3]"), "{note}");
    }

    fn source_row(kind: &str, path: &str, cite: i64) -> LlmSourceRow {
        LlmSourceRow {
            id: format!("id{cite}"),
            thread_id: "t".into(),
            sort_order: 0,
            origin: "tool".into(),
            path: path.into(),
            title: "t".into(),
            paragraph_id: "p".into(),
            body: "b".into(),
            query: String::new(),
            created_at: 0,
            grain: "unit".into(),
            unit_body: String::new(),
            injected_user_message_id: String::new(),
            cited_assistant_message_id: String::new(),
            cite_no: cite,
            kind: kind.into(),
            stored_relpath: String::new(),
            ocr_status: String::new(),
        }
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
    fn read_unit_schema_accepts_a_citation_number() {
        let schema = tools_schema(false, false, false).to_string();
        assert!(schema.contains("cite_no"), "{schema}");
        assert!(schema.contains("既読"), "{schema}");
        assert!(schema.contains("articles"), "{schema}");
        assert!(schema.contains("6の2"), "{schema}");
        assert!(schema.contains("答える前に読み直す"), "{schema}");
        assert!(schema.contains("文字範囲の開始"), "{schema}");
        assert!(schema.contains("文字範囲の終了"), "{schema}");
        assert!(schema.contains("[\"integer\",\"string\"]"), "{schema}");
    }

    #[test]
    fn read_saved_cite_returns_the_stored_body_and_does_not_move_the_counter() {
        let dir = std::env::temp_dir().join(format!("argos-cite-read-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = crate::db::Db::open(&dir.join("argos.db")).unwrap();
        let thread = db.create_llm_thread("t", true).unwrap();
        let (row, _) = db
            .insert_llm_source(
                &thread.id,
                "attach",
                r"C:\law\民法.txt",
                "民法.txt",
                r"C:\law\民法.txt#4",
                "第一条の原文",
                "",
            )
            .unwrap();
        let before = db.llm_cite_counter(&thread.id).unwrap();
        assert!(row.cite_no > 0);
        let exec = read_saved_cite(&db, &thread.id, row.cite_no);
        assert!(exec.content.contains("第一条の原文"), "{}", exec.content);
        assert!(
            exec.content.contains(&format!("[{}]", row.cite_no)),
            "{}",
            exec.content
        );
        assert_eq!(exec.consumed, vec![(row.id.clone(), row.cite_no)]);
        assert_eq!(db.llm_cite_counter(&thread.id).unwrap(), before);
        let again = read_saved_cite(&db, &thread.id, row.cite_no);
        assert_eq!(again.content, exec.content);
        assert_eq!(db.llm_cite_counter(&thread.id).unwrap(), before);
        let missing = read_saved_cite(&db, &thread.id, row.cite_no + 50);
        assert!(missing.content.contains("ありません"), "{}", missing.content);
        assert!(missing.consumed.is_empty());
        assert_eq!(db.llm_cite_counter(&thread.id).unwrap(), before);
        let user = db.insert_llm_message(&thread.id, "user", "最初").unwrap();
        let assistant = db
            .insert_llm_message(&thread.id, "assistant", "答え [1]")
            .unwrap();
        db.consume_llm_sources(&[(row.id.clone(), row.cite_no)], &user.id, &assistant.id)
            .unwrap();
        let reread = read_saved_cite(&db, &thread.id, row.cite_no);
        assert!(reread.consumed.is_empty(), "{:?}", reread.consumed);
        let later = db.insert_llm_message(&thread.id, "user", "もう一度").unwrap();
        let later_answer = db
            .insert_llm_message(&thread.id, "assistant", "再掲")
            .unwrap();
        db.consume_llm_sources(
            &[(row.id.clone(), row.cite_no)],
            &later.id,
            &later_answer.id,
        )
        .unwrap();
        let owned = db
            .list_llm_sources(&thread.id)
            .unwrap()
            .into_iter()
            .find(|source| source.id == row.id)
            .unwrap();
        assert_eq!(owned.injected_user_message_id, user.id);
        assert_eq!(owned.cited_assistant_message_id, assistant.id);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn temp_db(name: &str) -> (std::path::PathBuf, crate::db::Db, crate::db::LlmThreadRow) {
        let dir = std::env::temp_dir().join(format!("argos-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = crate::db::Db::open(&dir.join("argos.db")).unwrap();
        let thread = db.create_llm_thread("t", true).unwrap();
        (dir, db, thread)
    }

    fn attach_text(db: &crate::db::Db, thread_id: &str, body: &str) -> crate::db::LlmSourceRow {
        db.insert_llm_source_full(
            thread_id,
            "attach",
            r"C:\docs\契約書.txt",
            "契約書.txt",
            "",
            body,
            "",
            "file",
            "text",
            "",
            "",
            None,
        )
        .unwrap()
        .0
    }

    const CONTRACT: &str = "\
前文。第3条の規定は前文の中にある。

第6条（解除）
甲は、乙が義務に違反したときは解除できる。

第6条の2（中途解約）
甲は30日前に通知して解約できる。

第7条（準拠法）
日本法による。
";

    #[test]
    fn read_unit_returns_requested_articles_verbatim_and_lists_missing_ones() {
        let (dir, db, thread) = temp_db("articles");
        let row = attach_text(&db, &thread.id, CONTRACT);
        let whole = read_saved_selection(&db, &thread.id, row.cite_no, &json!({}));
        assert!(whole.content.contains("前文。"));
        assert!(whole.content.contains("日本法による。"));
        assert!(whole.content.contains("30日前に通知して解約できる。"));

        let spans = crate::llm::articles::article_spans(CONTRACT.trim());
        let article6 = spans
            .iter()
            .find(|span| span.no.main == 6 && span.no.branch.is_none())
            .unwrap();
        let both = read_saved_selection(&db, &thread.id, row.cite_no, &json!({"articles": [6, 7]}));
        assert!(both.content.contains(&article6.text), "{}", both.content);
        assert!(both.content.contains("日本法による。"), "{}", both.content);
        assert!(!both.content.contains("30日前"), "{}", both.content);
        assert!(both.content.contains(&format!("[{}]", row.cite_no)), "{}", both.content);
        assert_eq!(both.consumed, vec![(row.id.clone(), row.cite_no)]);

        let branch = read_saved_selection(
            &db,
            &thread.id,
            row.cite_no,
            &json!({"articles": ["6の2", "第六条の二"]}),
        );
        assert!(branch.content.contains("甲は30日前に通知して解約できる。"), "{}", branch.content);
        assert!(!branch.content.contains("解除できる"), "{}", branch.content);

        let missing = read_saved_selection(&db, &thread.id, row.cite_no, &json!({"articles": [9]}));
        assert!(missing.content.contains("第9条はありません"), "{}", missing.content);
        assert!(
            missing.content.contains("ある条番号: 第6条、第6条の2、第7条。"),
            "{}",
            missing.content
        );
        assert!(!missing.content.contains("解除できる"), "{}", missing.content);

        let partial = read_saved_selection(
            &db,
            &thread.id,
            row.cite_no,
            &json!({"articles": [6, 99]}),
        );
        assert!(partial.content.contains("第99条はありません"), "{}", partial.content);
        assert!(partial.content.contains(&article6.text), "{}", partial.content);
        assert!(!partial.content.contains("30日前"), "{}", partial.content);

        let bad = read_saved_selection(&db, &thread.id, row.cite_no, &json!({"articles": ["解除"]}));
        assert!(bad.content.contains("条番号として読めません"), "{}", bad.content);
        assert!(bad.consumed.is_empty(), "{:?}", bad.consumed);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_unit_reads_a_character_range_when_the_document_has_no_articles() {
        let (dir, db, thread) = temp_db("range");
        let memo = "これは条立てではないメモです。日付は令和2年。";
        let row = attach_text(&db, &thread.id, memo);
        let no_articles = read_saved_selection(&db, &thread.id, row.cite_no, &json!({"articles": [1]}));
        assert!(no_articles.content.contains("条の見出し"), "{}", no_articles.content);
        assert!(no_articles.content.contains("start と end"), "{}", no_articles.content);
        assert!(
            no_articles.content.contains(&format!("全{}文字", memo.chars().count())),
            "{}",
            no_articles.content
        );
        assert!(!no_articles.content.contains("令和2年"), "{}", no_articles.content);

        let range = read_saved_selection(&db, &thread.id, row.cite_no, &json!({"start": 1, "end": 4}));
        assert!(range.content.contains("これは条"), "{}", range.content);
        assert!(!range.content.contains("立てではない"), "{}", range.content);
        assert!(range.content.contains("1文字目から4文字目"), "{}", range.content);

        let tail = read_saved_selection(&db, &thread.id, row.cite_no, &json!({"start": 5}));
        assert!(tail.content.contains("立てではないメモです。"), "{}", tail.content);
        assert!(!tail.content.contains("これは条"), "{}", tail.content);

        let clamped = read_saved_selection(
            &db,
            &thread.id,
            row.cite_no,
            &json!({"start": 1, "end": 1000}),
        );
        assert!(clamped.content.contains(memo), "{}", clamped.content);
        assert!(
            clamped.content.contains(&format!("全{}文字", memo.chars().count())),
            "{}",
            clamped.content
        );
        assert!(!clamped.content.contains("1000文字目"), "{}", clamped.content);

        let backwards = read_saved_selection(&db, &thread.id, row.cite_no, &json!({"start": 4, "end": 2}));
        assert!(backwards.content.contains("end は start 以上"), "{}", backwards.content);
        let past = read_saved_selection(&db, &thread.id, row.cite_no, &json!({"start": 80}));
        assert!(past.content.contains("文書の長さ"), "{}", past.content);
        assert!(past.content.contains(&format!("{}文字", memo.chars().count())));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn truncated_attachment_keeps_the_full_text_so_a_later_article_can_be_reread() {
        let (dir, db, thread) = temp_db("trunc");
        let mut body = String::from("第1条（目的）\n");
        body.push_str(&"甲".repeat(12_000));
        body.push_str("\n\n");
        for n in 2..=8 {
            body.push_str(&format!("第{n}条（第{n}条の見出し）\n第{n}条だけの固有文言。\n\n"));
        }
        let row = attach_text(&db, &thread.id, &body);
        let history = vec![crate::db::LlmMessageRow {
            id: "u1".into(),
            thread_id: thread.id.clone(),
            role: "user".into(),
            content: "第8条の解除事由は何ですか。".into(),
            created_at: 0,
            reasoning: String::new(),
            tool_calls: String::new(),
            tool_call_id: String::new(),
            tool_name: String::new(),
            kind: String::new(),
            carries_summary: false,
        }];
        let (turns, stats) = crate::llm::context::assemble_turns(
            "役割",
            std::slice::from_ref(&row),
            &history,
            1_000,
        );
        assert!(stats.truncated);
        assert!(!stats.question_exceeds);
        let user = turns.iter().find(|turn| turn.role == "user").unwrap();
        assert!(
            user.content.contains("文字のみ") && user.content.contains("続きは start="),
            "{}",
            user.content
        );
        assert!(
            user.content.contains(&format!("全{}文字", body.trim().chars().count())),
            "{}",
            user.content
        );
        assert!(user.content.contains("第1条"), "{}", user.content);
        assert!(!user.content.contains("第8条だけの固有文言"));
        let stored = db
            .list_llm_sources(&thread.id)
            .unwrap()
            .into_iter()
            .find(|source| source.id == row.id)
            .unwrap();
        assert_eq!(stored.body, body, "the stored copy stays the full text");
        let reread = read_saved_selection(&db, &thread.id, row.cite_no, &json!({"articles": [8]}));
        assert!(
            reread.content.contains("第8条（第8条の見出し）\n第8条だけの固有文言。"),
            "{}",
            reread.content
        );
        assert!(!reread.content.contains("第7条だけの固有文言"), "{}", reread.content);
        assert!(!reread.content.contains(&"甲".repeat(12_000)));
        let whole = read_saved_cite(&db, &thread.id, row.cite_no);
        assert!(whole.content.contains("第8条だけの固有文言"));
        assert!(whole.content.contains(&"甲".repeat(100)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_placeholders_still_read_the_paragraph_and_zero_start_is_the_whole_text() {
        for args in [
            json!({"paragraph_id": "unit-9", "articles": null}),
            json!({"paragraph_id": "unit-9", "articles": []}),
            json!({"paragraph_id": "unit-9", "start": 0}),
            json!({"paragraph_id": "unit-9", "end": 0}),
            json!({"paragraph_id": "unit-9", "articles": [0], "start": null, "end": 0}),
        ] {
            match route_read_unit(&args) {
                ReadRoute::Paragraph(id) => assert_eq!(id, "unit-9"),
                other => panic!("paragraph read was refused: {args} -> {other:?}"),
            }
        }
        assert!(matches!(
            route_read_unit(&json!({"articles": [6]})),
            ReadRoute::NeedCite
        ));
        assert!(matches!(
            route_read_unit(&json!({"cite_no": 3, "start": 0})),
            ReadRoute::Saved(3)
        ));

        let (dir, db, thread) = temp_db("zero-start");
        let row = attach_text(&db, &thread.id, CONTRACT);
        let whole = read_saved_selection(
            &db,
            &thread.id,
            row.cite_no,
            &json!({"start": 0, "end": 0, "articles": []}),
        );
        assert!(whole.content.contains("前文。"), "{}", whole.content);
        assert!(whole.content.contains("日本法による。"), "{}", whole.content);
        assert!(!whole.content.contains("1 以上"), "{}", whole.content);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn range_cap_reports_the_delivered_end_and_the_next_start() {
        let (dir, db, thread) = temp_db("cap");
        let body = "あ".repeat(READ_BODY_CAP + 500);
        let row = attach_text(&db, &thread.id, &body);
        let exec = read_saved_selection(
            &db,
            &thread.id,
            row.cite_no,
            &json!({"start": 1, "end": 100_000}),
        );
        let delivered = READ_BODY_CAP;
        assert!(
            exec.content.contains(&format!("1文字目から{delivered}文字目")),
            "{}",
            exec.content
        );
        assert!(
            exec.content.contains(&format!("続きは start={}", delivered + 1)),
            "{}",
            exec.content
        );
        assert!(
            exec.content.contains(&format!("全{}文字", body.chars().count())),
            "{}",
            exec.content
        );
        assert_eq!(exec.content.matches('あ').count(), delivered, "{}", exec.content.chars().take(80).collect::<String>());
        let trimmed = crate::llm::context::fit_tool_text(exec.content.clone(), 200, 0);
        assert!(trimmed.contains("続きは start="), "{trimmed}");
        let claimed = claimed_span(&trimmed);
        let a_count = trimmed.matches('あ').count();
        assert_eq!(claimed, a_count, "claim {claimed} vs delivered {a_count}: {trimmed}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn claimed_span(text: &str) -> usize {
        let line = text.lines().find(|line| line.contains("文字目から")).unwrap();
        let start: usize = line
            .split("文字目から")
            .next()
            .unwrap()
            .rsplit(|c: char| !c.is_ascii_digit())
            .next()
            .unwrap()
            .parse()
            .unwrap();
        let end: usize = line
            .split("文字目から")
            .nth(1)
            .unwrap()
            .split("文字目")
            .next()
            .unwrap()
            .parse()
            .unwrap();
        end - start + 1
    }

    #[test]
    fn multi_article_read_does_not_claim_a_cut_off_article() {
        let (dir, db, thread) = temp_db("multi");
        let mut body = String::new();
        for n in 1..=3 {
            body.push_str(&format!("第{n}条（条）\n"));
            body.push_str(&format!("UNIQUE{n}"));
            body.push_str(&"文".repeat(4_000));
            body.push_str("\n\n");
        }
        let row = attach_text(&db, &thread.id, &body);
        let exec = read_saved_selection(
            &db,
            &thread.id,
            row.cite_no,
            &json!({"articles": [1, 2, 3]}),
        );
        assert!(exec.content.contains("UNIQUE1"), "{}", exec.content);
        assert!(exec.content.contains("続きは start="), "{}", exec.content);
        assert!(!exec.content.contains("UNIQUE3"), "{}", exec.content);
        let trimmed = crate::llm::context::fit_tool_text(exec.content, 90, 0);
        assert!(
            !trimmed.lines().any(|line| line.starts_with("［再読］第3条")),
            "{trimmed}"
        );
        assert!(!trimmed.contains("UNIQUE3"), "{trimmed}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn multi_page_range_uses_the_page_headings_the_prompt_shows() {
        let (dir, db, thread) = temp_db("pages");
        let path = r"C:\docs\scan.pdf";
        let _first = db
            .insert_llm_source_full(
                &thread.id,
                "attach",
                path,
                "scan.pdf",
                "pdf-page:1",
                "甲ページ本文",
                "",
                "file",
                "image",
                "",
                "",
                None,
            )
            .unwrap()
            .0;
        let second = db
            .insert_llm_source_full(
                &thread.id,
                "attach",
                path,
                "scan.pdf",
                "pdf-page:2",
                "乙ページ本文",
                "",
                "file",
                "image",
                "",
                "",
                None,
            )
            .unwrap()
            .0;
        let rows = db.list_llm_sources_by_cite(&thread.id, second.cite_no).unwrap();
        assert!(rows.len() >= 2, "pages share a cite: {rows:?}");
        let visible = crate::llm::transcript::transcript_body(
            &rows.iter().collect::<Vec<_>>(),
            true,
        );
        assert!(visible.contains("## 1ページ目"), "{visible}");
        assert!(visible.contains("## 2ページ目"), "{visible}");
        let start = visible.chars().position(|c| c == '乙').unwrap() + 1;
        let exec = read_saved_selection(
            &db,
            &thread.id,
            second.cite_no,
            &json!({"start": start, "end": start + 2}),
        );
        assert!(exec.content.contains("乙ペ"), "{}", exec.content);
        assert!(exec.content.contains("ページ見出しを含む"), "{}", exec.content);
        let joined = format!("{}\n\n{}", "甲ページ本文", "乙ページ本文");
        let wrong: String = joined.chars().skip(start - 1).take(3).collect();
        assert_ne!(wrong, "乙ペ", "the no-heading join must not be the index");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refit_of_a_full_article_header_keeps_a_reread_line_at_a_tight_budget() {
        let body = "あ".repeat(2_550);
        let preamble = "【出典】\n[1] 契約.txt — C:\\docs\\契約.txt\n";
        let text = format!("{preamble}［再読］第1条。1文字目から2550文字目。全2550文字\n{body}\n");
        let budget = preamble.chars().count() + 36;
        let out = refit_reread_text(&text, budget).expect("refit");
        assert!(
            out.contains("［再読］") || out.contains("続きは start="),
            "{out}"
        );
        assert!(!out.contains("1文字目から0文字目"), "{out}");
        assert!(!out.contains(&"あ".repeat(2_550)), "{out}");
        let partial = format!(
            "{preamble}［再読］第1条の先頭。1文字目から10文字目。全2550文字。続きは start=11\n{}\n",
            "あ".repeat(10)
        );
        let again = refit_reread_text(&partial, budget).expect("refit partial header");
        assert!(
            again.contains("［再読］") || again.contains("続きは start="),
            "{again}"
        );
        assert!(!again.contains("1文字目から0文字目"), "{again}");
    }

    #[test]
    fn shortened_ocr_page_notice_start_reaches_the_later_page() {
        let (dir, db, thread) = temp_db("ocr-cut");
        let path = r"C:\docs\scan.pdf";
        let page1 = "甲".repeat(2_400);
        let marker = "乙PAGE_MARKER";
        db.insert_llm_source_full(
            &thread.id,
            "attach",
            path,
            "scan.pdf",
            "pdf-page:1",
            &page1,
            "",
            "file",
            "image",
            "",
            "",
            None,
        )
        .unwrap();
        let second = db
            .insert_llm_source_full(
                &thread.id,
                "attach",
                path,
                "scan.pdf",
                "pdf-page:2",
                marker,
                "",
                "file",
                "image",
                "",
                "",
                None,
            )
            .unwrap()
            .0;
        let rows = db.list_llm_sources_by_cite(&thread.id, second.cite_no).unwrap();
        assert!(rows.len() >= 2, "{rows:?}");
        let history = vec![crate::db::LlmMessageRow {
            id: "u1".into(),
            thread_id: thread.id.clone(),
            role: "user".into(),
            content: "読んで".into(),
            created_at: 0,
            reasoning: String::new(),
            tool_calls: String::new(),
            tool_call_id: String::new(),
            tool_name: String::new(),
            kind: String::new(),
            carries_summary: false,
        }];
        let mut saw = false;
        for budget in [500usize, 700, 900, 1_100, 1_400] {
            let (turns, _) = crate::llm::context::assemble_turns(
                "役割",
                &rows,
                &history,
                budget,
            );
            let user = turns.iter().find(|turn| turn.role == "user").unwrap();
            if user.content.contains(&page1) {
                continue;
            }
            let Some(start) = notice_start(&user.content) else {
                continue;
            };
            saw = true;
            let exec = read_saved_selection(
                &db,
                &thread.id,
                second.cite_no,
                &json!({"start": start, "end": start + 4_000}),
            );
            assert!(
                exec.content.contains(marker),
                "budget {budget} start={start} missed the marker: {}",
                exec.content.chars().take(240).collect::<String>()
            );
        }
        assert!(saw, "no budget shortened page 1 while hiding the page 2 marker");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn notice_start(text: &str) -> Option<i64> {
        let line = text.lines().find(|line| line.contains("続きは start="))?;
        let digits: String = line
            .split("続きは start=")
            .nth(1)?
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        digits.parse().ok()
    }

    #[test]
    fn multi_article_refit_rewrites_a_trimmed_header_and_keeps_the_next_notice_on_its_own_line() {
        for labels in [
            vec!["第2条", "第3条", "第5条"],
            vec!["第6条", "第7条"],
            vec!["第1条", "第1条（別紙1）"],
        ] {
            let mut blocks = String::from("【出典】\n[1] 契約.txt — C:\\docs\\契約.txt\n");
            let mut start = 1usize;
            for label in &labels {
                let body = format!("{}END{label}", "あ".repeat(72));
                let n = body.chars().count();
                let end = start + n - 1;
                blocks.push_str(&format!(
                    "［再読］{label}。{start}文字目から{end}文字目。全{n}文字\n{body}\n"
                ));
                start = end + 1;
            }
            // The first article ends at its trailing newline. Cut 8 characters into the next notice.
            let first_article_end = blocks.match_indices("［再読］").nth(1).map(|(i, _)| i);
            let cut_at = match first_article_end {
                Some(next) => blocks[..next].chars().count(),
                None => blocks.chars().count(),
            };
            for extra in [8usize, 0] {
                let budget = cut_at + extra;
                let out = refit_reread_text(&blocks, budget).expect("refit");
                assert!(
                    out.contains("続きは start="),
                    "{labels:?} +{extra} dropped the overflow without a continuation: {out}"
                );
                assert_reread_claims_are_honest(&out, &format!("{labels:?} +{extra}"));
            }
            if labels.len() >= 3 {
                let third = blocks.match_indices("［再読］").nth(2).map(|(i, _)| i).unwrap();
                let budget = blocks[..third].chars().count() + 8;
                let out = refit_reread_text(&blocks, budget).expect("refit third");
                assert_reread_claims_are_honest(&out, &format!("{labels:?} third"));
            }
        }
    }

    fn assert_reread_claims_are_honest(text: &str, what: &str) {
        let mut rest = text;
        while let Some(pos) = rest.find("［再読］") {
            if pos > 0 {
                assert!(
                    rest[..pos].ends_with('\n'),
                    "{what} glued a notice onto the body: {text}"
                );
            }
            rest = &rest[pos + "［再読］".len()..];
        }
        let lines: Vec<&str> = text.lines().collect();
        let mut i = 0;
        while i < lines.len() {
            if !lines[i].starts_with("［再読］") {
                i += 1;
                continue;
            }
            let header = lines[i];
            let mut body = String::new();
            let mut j = i + 1;
            while j < lines.len() && !lines[j].starts_with("［再読］") {
                if !body.is_empty() {
                    body.push('\n');
                }
                body.push_str(lines[j]);
                j += 1;
            }
            if let Some((start, end)) = claimed_range(header) {
                let delivered = body.chars().count();
                assert_eq!(
                    delivered, end - start + 1,
                    "{what} header claims {start}..={end} but body is {delivered}: {header}"
                );
                if !header.contains("続きは") {
                    assert!(
                        header.contains(&format!("全{delivered}文字")),
                        "{what} full claim does not match the body: {header}"
                    );
                }
            } else {
                assert!(
                    header.contains("続きは start="),
                    "{what} empty section has neither a range nor 続きは: {header}"
                );
            }
            i = j;
        }
    }

    fn claimed_range(header: &str) -> Option<(usize, usize)> {
        let (start_s, rest) = header.split_once("文字目から")?;
        let start: usize = start_s
            .rsplit(|c: char| !c.is_ascii_digit())
            .next()?
            .parse()
            .ok()?;
        let end: usize = rest.split("文字目").next()?.parse().ok()?;
        Some((start, end))
    }

    #[test]
    fn one_page_scan_start_does_not_skip_the_heading_that_is_not_shown() {
        let (dir, db, thread) = temp_db("one-page");
        let page = format!("{}MARK{}", "甲".repeat(400), "甲".repeat(2_000));
        let row = db
            .insert_llm_source_full(
                &thread.id,
                "attach",
                r"C:\docs\one.pdf",
                "one.pdf",
                "pdf-page:1",
                &page,
                "",
                "file",
                "image",
                "",
                "",
                None,
            )
            .unwrap()
            .0;
        let history = vec![crate::db::LlmMessageRow {
            id: "u1".into(),
            thread_id: thread.id.clone(),
            role: "user".into(),
            content: "読んで".into(),
            created_at: 0,
            reasoning: String::new(),
            tool_calls: String::new(),
            tool_call_id: String::new(),
            tool_name: String::new(),
            kind: String::new(),
            carries_summary: false,
        }];
        let mut saw = false;
        for budget in [400usize, 600, 800, 1_000, 1_400] {
            let (turns, _) =
                crate::llm::context::assemble_turns("役割", &[row.clone()], &history, budget);
            let user = turns.iter().find(|turn| turn.role == "user").unwrap();
            if user.content.contains("MARK") || !user.content.contains("このページは先頭") {
                continue;
            }
            let start = notice_start(&user.content).expect("start");
            let shown = shown_page_chars(&user.content).expect("shown");
            assert_eq!(
                start as usize, shown + 1,
                "budget {budget} advertised start={start} shown={shown}: {}",
                user.content.chars().rev().take(80).collect::<String>()
            );
            assert!(
                user.content.contains(&format!("書き起こし全{}文字", page.chars().count())),
                "{}",
                user.content
            );
            saw = true;
            let exec = read_saved_selection(
                &db,
                &thread.id,
                row.cite_no,
                &json!({"start": start, "end": start + 20}),
            );
            let expect: String = page.chars().skip(shown).take(8).collect();
            assert!(
                exec.content.contains(&expect),
                "start={start} expected {expect}: {}",
                exec.content.chars().take(180).collect::<String>()
            );
        }
        assert!(saw, "no budget shortened the single page");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_sibling_page_keeps_the_read_index_on_the_stored_transcript() {
        let glyphs: Vec<char> = "あいうえおかきくけこさしすせそたちつてとなにぬねのはひふへほまみむめも"
            .chars()
            .collect();
        let page: String = (0..2_400).map(|i| glyphs[i % glyphs.len()]).collect();
        for status in ["error", ""] {
            let (dir, db, thread) = temp_db(&format!("sib-{status}"));
            let path = r"C:\docs\scan.pdf";
            let filled = insert_page(&db, &thread.id, path, 1, &page, "");
            insert_page(&db, &thread.id, path, 2, "", status);
            assert_page_cut_matches_stored(&db, &thread.id, &filled, &page, &[400, 800, 1_200]);
            let _ = std::fs::remove_dir_all(&dir);
        }
        let (dir, db, thread) = temp_db("page10");
        let path = r"C:\docs\scan.pdf";
        insert_page(&db, &thread.id, path, 1, "", "error");
        let filled = insert_page(&db, &thread.id, path, 10, &page, "");
        assert_page_cut_matches_stored(&db, &thread.id, &filled, &page, &[400, 800, 1_200]);
        let _ = std::fs::remove_dir_all(&dir);

        let (dir, db, thread) = temp_db("between");
        let path = r"C:\docs\scan.pdf";
        let page1 = "甲".repeat(2_400);
        insert_page(&db, &thread.id, path, 1, &page1, "");
        insert_page(&db, &thread.id, path, 2, "", "error");
        let third = insert_page(&db, &thread.id, path, 3, "乙PAGE_MARKER", "");
        let rows = db.list_llm_sources_by_cite(&thread.id, third.cite_no).unwrap();
        let history = vec![user_msg(&thread.id)];
        let mut saw = false;
        for budget in [500usize, 900, 1_400] {
            let (turns, _) = crate::llm::context::assemble_turns("役割", &rows, &history, budget);
            let user = turns.iter().find(|turn| turn.role == "user").unwrap();
            if user.content.contains(&page1) {
                continue;
            }
            let Some(start) = notice_start(&user.content) else {
                continue;
            };
            saw = true;
            let exec = read_saved_selection(
                &db,
                &thread.id,
                third.cite_no,
                &json!({"start": start, "end": start + 4_000}),
            );
            assert!(
                exec.content.contains("乙PAGE_MARKER"),
                "budget {budget} start={start} missed the later page: {}",
                exec.content.chars().take(180).collect::<String>()
            );
        }
        assert!(saw, "no budget shortened page 1 around the empty page");
        let _ = std::fs::remove_dir_all(&dir);

        let (dir, db, thread) = temp_db("p10p11");
        let path = r"C:\docs\ten.pdf";
        let page10 = "甲".repeat(2_400);
        insert_page(&db, &thread.id, path, 10, &page10, "");
        let eleventh = insert_page(&db, &thread.id, path, 11, "乙PAGE_MARKER", "");
        let rows = db.list_llm_sources_by_cite(&thread.id, eleventh.cite_no).unwrap();
        let history = vec![user_msg(&thread.id)];
        let mut saw = false;
        for budget in [500usize, 900, 1_400] {
            let (turns, _) = crate::llm::context::assemble_turns("役割", &rows, &history, budget);
            let user = turns.iter().find(|turn| turn.role == "user").unwrap();
            if user.content.contains(&page10) {
                continue;
            }
            let Some(start) = notice_start(&user.content) else {
                continue;
            };
            saw = true;
            let exec = read_saved_selection(
                &db,
                &thread.id,
                eleventh.cite_no,
                &json!({"start": start, "end": start + 4_000}),
            );
            assert!(
                exec.content.contains("乙PAGE_MARKER"),
                "budget {budget} start={start} missed page 11: {}",
                exec.content.chars().take(180).collect::<String>()
            );
        }
        assert!(saw, "no budget shortened page 10");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn insert_page(
        db: &crate::db::Db,
        thread_id: &str,
        path: &str,
        page: u32,
        body: &str,
        ocr_status: &str,
    ) -> crate::db::LlmSourceRow {
        db.insert_llm_source_full(
            thread_id,
            "attach",
            path,
            "scan.pdf",
            &format!("pdf-page:{page}"),
            body,
            "",
            "file",
            "image",
            "",
            ocr_status,
            None,
        )
        .unwrap()
        .0
    }

    fn user_msg(thread_id: &str) -> crate::db::LlmMessageRow {
        crate::db::LlmMessageRow {
            id: "u1".into(),
            thread_id: thread_id.into(),
            role: "user".into(),
            content: "読んで".into(),
            created_at: 0,
            reasoning: String::new(),
            tool_calls: String::new(),
            tool_call_id: String::new(),
            tool_name: String::new(),
            kind: String::new(),
            carries_summary: false,
        }
    }

    fn assert_page_cut_matches_stored(
        db: &crate::db::Db,
        thread_id: &str,
        filled: &crate::db::LlmSourceRow,
        page: &str,
        budgets: &[usize],
    ) {
        let rows = db.list_llm_sources_by_cite(thread_id, filled.cite_no).unwrap();
        let stored = saved_visible_text(&rows);
        let history = vec![user_msg(thread_id)];
        let mut saw = false;
        for budget in budgets {
            let (turns, _) =
                crate::llm::context::assemble_turns("役割", &rows, &history, *budget);
            let user = turns.iter().find(|turn| turn.role == "user").unwrap();
            if !user.content.contains("このページは先頭") {
                continue;
            }
            let start = notice_start(&user.content).expect("start");
            let shown = shown_page_chars(&user.content).expect("shown");
            let from_stored: String = stored.chars().skip(start as usize - 1).take(8).collect();
            let from_page: String = page.chars().skip(shown).take(8).collect();
            assert_eq!(
                from_stored, from_page,
                "budget {budget} start={start} shown={shown} stored={}",
                stored.chars().take(24).collect::<String>()
            );
            assert!(
                user.content.contains(&format!("書き起こし全{}文字", stored.chars().count())),
                "{}",
                user.content
            );
            saw = true;
            let exec = read_saved_selection(
                db,
                thread_id,
                filled.cite_no,
                &json!({"start": start, "end": start + 20}),
            );
            assert!(
                exec.content.contains(&from_page),
                "start={start} expected {from_page}: {}",
                exec.content.chars().take(180).collect::<String>()
            );
        }
        assert!(saw, "no budget shortened the surviving page");
    }

    fn shown_page_chars(text: &str) -> Option<usize> {
        let line = text.lines().find(|line| line.contains("このページは先頭"))?;
        let digits: String = line
            .split("このページは先頭")
            .nth(1)?
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        digits.parse().ok()
    }

    #[test]
    fn arg_i64_reads_numbers_and_numeric_strings() {
        assert_eq!(arg_i64(Some(&json!(12))), Some(12));
        assert_eq!(arg_i64(Some(&json!("12"))), Some(12));
        assert_eq!(arg_i64(Some(&json!("no"))), None);
        assert_eq!(arg_i64(None), None);
    }

    #[test]
    fn tools_schema_mentions_web_only_when_enabled() {
        let off = tools_schema(false, false, false).to_string();
        assert!(!off.contains("公開ウェブも同時に検索"));
        assert!(!off.contains("（リモート）"));
        assert!(tools_schema(false, true, false).to_string().contains("（リモート）"));
        assert!(!off.contains(TOOL_READ_URL));
        assert!(off.contains(TOOL_LIST_NOTES));
        assert!(off.contains(TOOL_READ_NOTE));
        assert!(off.contains(TOOL_CREATE_NOTE));
        assert!(off.contains(TOOL_WRITE_NOTE));
        assert!(!off.contains("propose_note_"));
        let on = tools_schema(true, false, false).to_string();
        assert!(on.contains("公開ウェブも同時に検索"));
        assert!(on.contains(TOOL_READ_URL));
        assert!(on.contains("read_url"));
        assert_eq!(max_tool_rounds(8), MAX_TOOL_ROUNDS);
        assert_eq!(max_tool_rounds(0), 0);
        assert_eq!(max_tool_rounds(3), MAX_TOOL_ROUNDS);
        assert!(tool_round_limit_notice(8).contains("上限の 8 回"));
        assert!(tool_round_limit_notice(8).contains("ツール往復の上限"));
        let answered = round_limit_answer(Some("ここまでの結論です。"), 8);
        assert_eq!(answered.text, "ここまでの結論です。");
        let short = answered.notice.expect("short notice");
        assert!(short.contains("上限の 8 回"));
        assert!(short.contains("ここまでの資料"));
        assert_ne!(short, tool_round_limit_notice(8));
        assert!(short.chars().count() < tool_round_limit_notice(8).chars().count());
        for body in [None, Some(""), Some("  \n")] {
            let fallback = round_limit_answer(body, 16);
            assert_eq!(fallback.text, tool_round_limit_notice(16));
            assert!(fallback.notice.is_none());
            assert!(!fallback.text.contains("失敗しました"));
        }
        let secret = "sk-live-secret";
        let runaway = round_limit_failure(
            8,
            &format!("思考が 24576 字に達しても答えが出ないため止めました。 key={secret}"),
        );
        assert!(runaway.text.contains("上限の 8 回"));
        assert!(runaway.text.contains("思考が長く続いたため止めました"));
        assert!(runaway.notice.is_none());
        assert!(!runaway.text.contains(secret));
        let too_long = round_limit_failure(
            8,
            &format!("context length exceeded Bearer {secret}"),
        );
        assert!(too_long.text.contains("コンテキストが長すぎます"));
        assert!(!too_long.text.contains(secret));
        assert!(!too_long.text.contains("Bearer"));
        let bridge = round_limit_failure(8, &crate::llm::thinking::thinking_bridge_error());
        assert!(bridge.text.contains("思考を閉じられず止めました"));
        assert!(bridge.notice.is_none());
        let runaway_mapped = round_limit_failure(
            8,
            &crate::llm::thinking::runaway_thinking_error(24_576),
        );
        assert!(runaway_mapped.text.contains("思考が長く続いたため止めました"));
        assert!(!runaway_mapped.text.contains("24576"));
        assert!(!runaway_mapped.text.contains("HTTP"));
        let too_long_mapped = round_limit_failure(
            8,
            "コンテキスト長が足りません。Ollama の num_ctx などを 64k 以上にしてください。",
        );
        assert!(too_long_mapped.text.contains("コンテキストが長すぎます"));
        let timed_out = round_limit_failure(
            8,
            "応答が止まりました（タイムアウト）。指示を分けるか、設定の「思考」をオフにしてください。",
        );
        assert!(timed_out.text.contains("応答が時間切れになりました"));
        let tailscale = round_limit_failure(
            8,
            "LLM サーバのポートに届きません。例: mtplx serve --host 0.0.0.0 --port 8000 --api-key sk-live-secret",
        );
        assert!(tailscale.text.contains("サーバに接続できません"));
        assert!(!tailscale.text.contains(secret));
        assert!(!tailscale.text.contains("api-key"));
        assert!(!tailscale.text.contains("HTTP"));
        let auth = round_limit_failure(8, "API キーが拒否されました。キー欄を確認してください。");
        assert!(auth.text.contains("サーバが認証を拒否しました"));
        assert!(!auth.text.contains("キー欄"));
        let unknown = round_limit_failure(8, &format!("unexpected blob api_key={secret}"));
        assert!(unknown.text.contains("応答を受け取れませんでした"));
        assert!(!unknown.text.contains(secret));
        assert!(!unknown.text.contains("api_key"));
        assert!(on.contains(TOOL_WRITE_NOTE));
        assert!(off.contains("ノートの id・題名・更新日時"));
    }

    #[test]
    fn round_limit_names_only_a_status_already_in_our_message() {
        let secret = "sk-live-secret";
        let raw = round_limit_failure(8, &format!("500 internal api_key={secret}"));
        assert!(raw.text.contains("応答を受け取れませんでした"), "{}", raw.text);
        assert!(!raw.text.contains("HTTP"), "{}", raw.text);
        assert!(!raw.text.contains(secret), "{}", raw.text);
        assert!(!raw.text.contains("api_key"), "{}", raw.text);
        let mapped = round_limit_failure(
            8,
            &crate::llm::http_error::chat_failure(500, &format!("internal api_key={secret}")),
        );
        assert!(mapped.text.contains("HTTP 500"), "{}", mapped.text);
        assert!(!mapped.text.contains(secret), "{}", mapped.text);
        assert!(!mapped.text.contains("api_key"), "{}", mapped.text);
        for port in ["8080", "11434", "1234", "500", "404"] {
            let refused =
                round_limit_failure(8, &format!("connection refused to http://127.0.0.1:{port}"));
            assert!(
                refused.text.contains("サーバに接続できません"),
                "{port}: {}",
                refused.text
            );
            assert!(!refused.text.contains("HTTP"), "{port}: {}", refused.text);
            let bare = round_limit_failure(8, &format!("http://127.0.0.1:{port}/v1/chat"));
            assert!(!bare.text.contains("HTTP"), "{port}: {}", bare.text);
            assert!(!bare.text.contains("認証"), "{port}: {}", bare.text);
        }
        let auth_status = round_limit_failure(8, "401 Unauthorized");
        assert!(auth_status.text.contains("応答を受け取れませんでした"), "{}", auth_status.text);
        assert!(!auth_status.text.contains("認証"), "{}", auth_status.text);
        let http_line = round_limit_failure(8, "HTTP/1.1 404 Not Found");
        assert!(http_line.text.contains("応答を受け取れませんでした"), "{}", http_line.text);
        assert!(!http_line.text.contains("HTTP"), "{}", http_line.text);
        let status_field = round_limit_failure(8, "status_code=503");
        assert!(status_field.text.contains("応答を受け取れませんでした"), "{}", status_field.text);
        assert!(!status_field.text.contains("HTTP"), "{}", status_field.text);
        assert!(!status_field.text.contains("503"), "{}", status_field.text);
    }

    #[test]
    fn mapped_http_status_is_the_only_status_round_limit_names() {
        let wrapped = crate::llm::http_error::chat_failure(
            500,
            r#"{"message":"request failed: HTTP 404 Not Found"}"#,
        );
        let wrapped_reason = round_limit_failure(8, &wrapped);
        assert!(wrapped_reason.text.contains("HTTP 500"), "{}", wrapped_reason.text);
        assert!(!wrapped_reason.text.contains("404"), "{}", wrapped_reason.text);
        let gateway = crate::llm::http_error::chat_failure(502, r#"{"error":"upstream HTTP 500"}"#);
        let gateway_reason = round_limit_failure(8, &gateway);
        assert!(gateway_reason.text.contains("HTTP 502"), "{}", gateway_reason.text);
        assert!(!gateway_reason.text.contains("500"), "{}", gateway_reason.text);
        let missing = round_limit_failure(
            8,
            &crate::llm::http_error::chat_failure(404, "HTTP/1.1 404 Not Found"),
        );
        assert!(
            missing.text.contains("指定したモデルが見つかりません"),
            "{}",
            missing.text
        );
        assert!(!missing.text.contains("HTTP"), "{}", missing.text);
        assert!(!missing.text.contains("Not Found"), "{}", missing.text);
    }

    #[test]
    fn round_limit_treats_403_the_same_as_401() {
        for code in [401_u16, 403] {
            let msg = round_limit_failure(
                8,
                &crate::llm::http_error::chat_failure(code, "Forbidden req_10061"),
            );
            assert!(
                msg.text.contains("サーバが認証を拒否しました"),
                "{code}: {}",
                msg.text
            );
            assert!(!msg.text.contains("HTTP"), "{code}: {}", msg.text);
            assert!(!msg.text.contains("req_10061"), "{code}: {}", msg.text);
            assert!(!msg.text.contains("Forbidden"), "{code}: {}", msg.text);
        }
    }

    #[test]
    fn round_limit_server_error_quoting_10061_tokens_is_not_a_connection_error() {
        let msg = round_limit_failure(
            8,
            &crate::llm::http_error::chat_failure(500, "prompt is 10061 tokens"),
        );
        assert!(msg.text.contains("HTTP 500"), "{}", msg.text);
        assert!(!msg.text.contains("接続"), "{}", msg.text);
        assert!(!msg.text.contains("10061"), "{}", msg.text);
    }

    #[test]
    fn round_limit_auth_status_wins_when_the_body_quotes_a_socket_code() {
        let msg = round_limit_failure(
            8,
            &crate::llm::http_error::chat_failure(401, r#"{"request_id":"req_10061"}"#),
        );
        assert!(msg.text.contains("サーバが認証を拒否しました"), "{}", msg.text);
        assert!(!msg.text.contains("接続"), "{}", msg.text);
        assert!(!msg.text.contains("req_10061"), "{}", msg.text);
    }

    #[test]
    fn round_limit_prompt_token_count_inside_a_server_error_is_not_auth() {
        let msg = round_limit_failure(
            8,
            &crate::llm::http_error::chat_failure(500, "prompt_tokens 14049"),
        );
        assert!(msg.text.contains("HTTP 500"), "{}", msg.text);
        assert!(!msg.text.contains("認証"), "{}", msg.text);
        assert!(!msg.text.contains("14049"), "{}", msg.text);
    }

    #[test]
    fn round_limit_quoted_404_inside_a_server_error_stays_a_server_error() {
        let msg = round_limit_failure(
            8,
            &crate::llm::http_error::chat_failure(500, "HTTP 404 Not Found"),
        );
        assert!(msg.text.contains("HTTP 500"), "{}", msg.text);
        assert!(!msg.text.contains("404"), "{}", msg.text);
    }

    #[test]
    fn round_limit_unauthorized_phrase_inside_a_server_error_stays_a_server_error() {
        let raw = round_limit_failure(8, "500 Internal Server Error Unauthorized for url");
        assert!(raw.text.contains("応答を受け取れませんでした"), "{}", raw.text);
        assert!(!raw.text.contains("認証"), "{}", raw.text);
        assert!(!raw.text.contains("Unauthorized"), "{}", raw.text);
        let msg = round_limit_failure(
            8,
            &crate::llm::http_error::chat_failure(500, "Unauthorized for url"),
        );
        assert!(msg.text.contains("HTTP 500"), "{}", msg.text);
        assert!(!msg.text.contains("認証"), "{}", msg.text);
        assert!(!msg.text.contains("Unauthorized"), "{}", msg.text);
    }

    #[test]
    fn round_limit_socket_error_without_an_http_status_is_a_connection_error() {
        let msg = round_limit_failure(8, "connection failed: os error 10061");
        assert!(msg.text.contains("サーバに接続できません"), "{}", msg.text);
        assert!(!msg.text.contains("HTTP"), "{}", msg.text);
        let timed_out = round_limit_failure(8, "os error 10060");
        assert!(timed_out.text.contains("サーバに接続できません"), "{}", timed_out.text);
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
