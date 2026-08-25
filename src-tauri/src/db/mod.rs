use serde::{Deserialize, Serialize};

use crate::notes_md;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub shortcut: String,
    /// Global shortcut to show the notes window.
    pub notes_shortcut: String,
    pub max_results: usize,
    pub font_size: u32,
    pub index_interval_secs: u64,
    pub autostart: bool,
    pub popup_width: u32,
    pub popup_height: u32,
    /// "left" | "center" | "right"
    pub popup_position: String,
    /// Host: expose local Tantivy search over LAN.
    pub remote_server_enabled: bool,
    pub remote_server_port: u32,
    pub remote_server_token: String,
    /// Client: "local" | "remote" | "hybrid"
    pub search_mode: String,
    pub remote_url: String,
    pub remote_token: String,
    pub remote_timeout_ms: u32,
    /// Drop 助詞/助動詞 from free query tokens (default true).
    pub pos_filter_enabled: bool,
    /// Index Outlook Classic mail via COM.
    pub mail_enabled: bool,
    /// How far back to sync (days).
    pub mail_days_back: u32,
    /// Periodic mail sync interval (seconds). 0 = manual only.
    pub mail_sync_interval_secs: u64,
    /// If true, only the newest message per ConversationID is kept in the index.
    pub mail_latest_only: bool,
    /// If true, search results collapse hits that share a conversation id.
    pub mail_thread_collapse: bool,
    /// Last successful mail sync (RFC3339), empty if never.
    pub mail_last_sync_at: String,
    /// OpenAI-compatible base URL, e.g. http://127.0.0.1:11434/v1
    pub llm_base_url: String,
    pub llm_api_key: String,
    pub llm_model: String,
    pub llm_timeout_ms: u32,
    /// Soft cap for source + history characters sent in one request.
    pub llm_max_context_chars: u32,
    pub llm_system_prompt: String,
    /// "auto" | "brief" | "off" — Qwen thinking / enable_thinking.
    pub llm_thinking: String,
    /// Token cap for thinking when mode is brief (also sent if auto and > 0).
    pub llm_thinking_budget: u32,
    pub llm_search_top_k: u32,
    /// SearXNG base URL (no /search). Empty = web search disabled.
    pub searxng_url: String,
    pub searxng_timeout_ms: u32,
    pub llm_web_search_top_k: u32,
}

pub const DEFAULT_LLM_BASE_URL: &str = "http://127.0.0.1:11434/v1";
pub const LEGACY_LLM_SYSTEM_PROMPT: &str =
    "あなたは法律事務所の調査補助です。日本語で簡潔に答えてください。出典ブロックがあるときはその本文だけを根拠にし、根拠箇所には [n] を付けてください。根拠がないことは推測だと明示し、分からないことは分からないと言ってください。";
/// Previous default after tools were added (「索引」表記). Unedited copies are replaced.
pub const LEGACY_TOOL_LLM_SYSTEM_PROMPT: &str =
    "あなたは法律事務所の調査補助です。日本語で簡潔に答えてください。出典ブロックがあるときはその本文だけを根拠にし、根拠箇所には [n] を付けてください。根拠がないことは推測だと明示し、分からないことは分からないと言ってください。索引を検索するツールがあります。添付出典で足りるときは検索しないでください。検索したら結果を [n] で引用してください。";
pub const LLM_FORMAT_HINT: &str =
    "回答はMarkdownで書いてください。見出し・箇条書き・表を使ってよいです。生のHTMLは書かないでください。ユーザーに選ばせるときは ```choices フェンスに選択肢を1行ずつ書いてください。";
pub const LLM_FORMAT_SENTINEL: &str = "生のHTMLは書かないでください";
pub const LLM_NOTE_HINT: &str =
    "ノートのメモを変えるツールは提案だけを作ります。採用までメモは変わりません。書いたと嘘をつかないでください。対象ノートが無いときは一覧だけ使えます。";
pub const LLM_NOTE_SENTINEL: &str = "採用までメモは変わりません";
pub const DEFAULT_LLM_SYSTEM_PROMPT: &str =
    "あなたは法律事務所の調査補助です。日本語で簡潔に答えてください。出典ブロックがあるときはその本文だけを根拠にし、根拠箇所には [n] を付けてください。根拠がないことは推測だと明示し、分からないことは分からないと言ってください。インデックスを検索するツールがあります。添付出典で足りるときは検索しないでください。検索したら結果を [n] で引用してください。\n回答はMarkdownで書いてください。見出し・箇条書き・表を使ってよいです。生のHTMLは書かないでください。ユーザーに選ばせるときは ```choices フェンスに選択肢を1行ずつ書いてください。";
pub const DEFAULT_LLM_TIMEOUT_MS: u32 = 120_000;
pub const DEFAULT_LLM_MAX_CONTEXT_CHARS: u32 = 80_000;
pub const DEFAULT_LLM_THINKING: &str = "brief";
pub const DEFAULT_LLM_THINKING_BUDGET: u32 = 2_048;
pub const DEFAULT_SEARXNG_TIMEOUT_MS: u32 = 8_000;
pub const DEFAULT_LLM_WEB_SEARCH_TOP_K: u32 = 5;

impl Default for Settings {
    fn default() -> Self {
        Self {
            shortcut: "Ctrl+Alt+A".into(),
            notes_shortcut: "Ctrl+Alt+N".into(),
            max_results: 10,
            font_size: 14,
            index_interval_secs: 3600,
            autostart: false,
            popup_width: 640,
            popup_height: 520,
            popup_position: "center".into(),
            remote_server_enabled: false,
            remote_server_port: 17890,
            remote_server_token: String::new(),
            search_mode: "local".into(),
            remote_url: String::new(),
            remote_token: String::new(),
            remote_timeout_ms: 3000,
            pos_filter_enabled: true,
            mail_enabled: false,
            mail_days_back: 730,
            mail_sync_interval_secs: 3600,
            mail_latest_only: false,
            mail_thread_collapse: true,
            mail_last_sync_at: String::new(),
            llm_base_url: DEFAULT_LLM_BASE_URL.into(),
            llm_api_key: String::new(),
            llm_model: String::new(),
            llm_timeout_ms: DEFAULT_LLM_TIMEOUT_MS,
            llm_max_context_chars: DEFAULT_LLM_MAX_CONTEXT_CHARS,
            llm_system_prompt: DEFAULT_LLM_SYSTEM_PROMPT.into(),
            llm_thinking: DEFAULT_LLM_THINKING.into(),
            llm_thinking_budget: DEFAULT_LLM_THINKING_BUDGET,
            llm_search_top_k: 4,
            searxng_url: String::new(),
            searxng_timeout_ms: DEFAULT_SEARXNG_TIMEOUT_MS,
            llm_web_search_top_k: DEFAULT_LLM_WEB_SEARCH_TOP_K,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmThreadRow {
    pub id: String,
    pub title: String,
    pub search_enabled: bool,
    pub path_prefix: String,
    #[serde(default)]
    pub note_id: String,
    pub sort_order: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmMessageRow {
    pub id: String,
    pub thread_id: String,
    pub role: String,
    pub content: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmSourceRow {
    pub id: String,
    pub thread_id: String,
    pub sort_order: i64,
    pub origin: String,
    pub path: String,
    pub title: String,
    pub paragraph_id: String,
    pub body: String,
    pub query: String,
    pub created_at: i64,
    #[serde(default = "default_llm_source_grain")]
    pub grain: String,
    #[serde(default)]
    pub unit_body: String,
    #[serde(default)]
    pub injected_user_message_id: String,
    #[serde(default)]
    pub cited_assistant_message_id: String,
    #[serde(default)]
    pub cite_no: i64,
    #[serde(default = "default_llm_source_kind")]
    pub kind: String,
    #[serde(default)]
    pub stored_relpath: String,
    #[serde(default)]
    pub ocr_status: String,
}

impl LlmSourceRow {
    pub fn is_pending(&self) -> bool {
        self.injected_user_message_id.trim().is_empty()
    }

    pub fn is_tool_origin(&self) -> bool {
        self.origin.eq_ignore_ascii_case("tool")
    }

    pub fn is_image(&self) -> bool {
        self.kind.eq_ignore_ascii_case("image")
    }

    pub fn is_web(&self) -> bool {
        self.kind.eq_ignore_ascii_case("web") || looks_like_http_url(&self.path)
    }

    /// Ready to put into an LLM 出典 block (OCR finished, body present).
    pub fn is_injectable(&self) -> bool {
        let st = self.ocr_status.trim();
        if st.eq_ignore_ascii_case("pending") || st.eq_ignore_ascii_case("error") {
            return false;
        }
        if self.is_image() && self.body.trim().is_empty() {
            return false;
        }
        true
    }
}

fn default_llm_source_grain() -> String {
    "unit".into()
}

fn default_llm_source_kind() -> String {
    "text".into()
}

fn looks_like_http_url(raw: &str) -> bool {
    let t = raw.trim();
    if t.is_empty() || t.chars().any(char::is_whitespace) {
        return false;
    }
    let lower = t.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

const LLM_SOURCE_COLS: &str =
    "id, thread_id, sort_order, origin, path, title, paragraph_id, body, query, created_at, grain, unit_body, injected_user_message_id, cited_assistant_message_id, cite_no, kind, stored_relpath, ocr_status";

const LLM_THREAD_COLS: &str =
    "id, title, search_enabled, path_prefix, note_id, sort_order, created_at, updated_at";

fn map_llm_thread(row: &rusqlite::Row<'_>) -> rusqlite::Result<LlmThreadRow> {
    Ok(LlmThreadRow {
        id: row.get(0)?,
        title: row.get(1)?,
        search_enabled: row.get::<_, i64>(2)? != 0,
        path_prefix: row.get(3)?,
        note_id: row.get::<_, String>(4).unwrap_or_default(),
        sort_order: row.get(5)?,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
    })
}

fn map_llm_source(row: &rusqlite::Row<'_>) -> rusqlite::Result<LlmSourceRow> {
    Ok(LlmSourceRow {
        id: row.get(0)?,
        thread_id: row.get(1)?,
        sort_order: row.get(2)?,
        origin: row.get(3)?,
        path: row.get(4)?,
        title: row.get(5)?,
        paragraph_id: row.get(6)?,
        body: row.get(7)?,
        query: row.get(8)?,
        created_at: row.get(9)?,
        grain: row.get::<_, String>(10).unwrap_or_else(|_| "unit".into()),
        unit_body: row.get::<_, String>(11).unwrap_or_default(),
        injected_user_message_id: row.get::<_, String>(12).unwrap_or_default(),
        cited_assistant_message_id: row.get::<_, String>(13).unwrap_or_default(),
        cite_no: row.get::<_, i64>(14).unwrap_or(0),
        kind: row.get::<_, String>(15).unwrap_or_else(|_| "text".into()),
        stored_relpath: row.get::<_, String>(16).unwrap_or_default(),
        ocr_status: row.get::<_, String>(17).unwrap_or_default(),
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailFolderRow {
    pub id: i64,
    pub store_id: String,
    pub entry_id: String,
    pub name: String,
    pub path_label: String,
    pub selected: bool,
    /// Outlook Items.Count at last catalog refresh (approximate).
    #[serde(default)]
    pub item_count: i32,
    /// Messages with status=indexed for this folder (searchable in Argos).
    #[serde(default)]
    pub indexed_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailMessageRow {
    pub path: String,
    pub store_id: String,
    pub entry_id: String,
    pub conversation_id: String,
    pub folder_name: String,
    pub from_addr: String,
    pub subject: String,
    pub date_unix: i64,
    pub content_hash: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailThreadWinner {
    pub conversation_id: String,
    pub path: String,
    pub date_unix: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderRow {
    pub id: i64,
    pub path: String,
    /// Optional UNC / LAN-visible root. Empty means indexed paths use `path` as-is.
    pub public_path: String,
    pub enabled: bool,
    /// Number of files registered in the index for this folder.
    pub indexed_count: u32,
    /// Live filesystem check (not persisted). True when `path` is an existing directory.
    #[serde(default)]
    pub exists: bool,
    /// LAN remote share (from settings KV, not a folders column).
    #[serde(default)]
    pub share_remote: bool,
}

/// Neutralize `LIKE` wildcards in a literal operand. Uses `|` as the escape character
/// because Windows forbids it in a path, unlike the backslash that fills every path.
fn escape_like_operand(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if matches!(ch, '%' | '_' | '|') {
            out.push('|');
        }
        out.push(ch);
    }
    out
}

fn parse_remote_share_folder_ids(raw: Option<&str>) -> Vec<i64> {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Vec::new();
    };
    let Ok(ids) = serde_json::from_str::<Vec<i64>>(raw) else {
        return Vec::new();
    };
    let mut out: Vec<i64> = Vec::new();
    for id in ids {
        if id > 0 && !out.contains(&id) {
            out.push(id);
        }
    }
    out
}

fn folder_path_exists(path: &str) -> bool {
    std::path::Path::new(path).is_dir()
}

impl FolderRow {
    pub fn with_exists(mut self) -> Self {
        self.exists = folder_path_exists(&self.path);
        self
    }
}

fn folder_row_from_sql(row: &rusqlite::Row<'_>) -> rusqlite::Result<FolderRow> {
    Ok(FolderRow {
        id: row.get(0)?,
        path: row.get(1)?,
        public_path: row.get(2)?,
        enabled: row.get::<_, i64>(3)? != 0,
        indexed_count: row.get::<_, i64>(4)? as u32,
        exists: false,
        share_remote: false,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExcludePathRow {
    pub id: i64,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchWordRow {
    pub id: i64,
    pub word: String,
    #[serde(default)]
    pub reading: String,
    #[serde(default)]
    pub pos_label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchWordImport {
    pub word: String,
    #[serde(default)]
    pub reading: String,
    #[serde(default)]
    pub pos_label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchWordImportResult {
    pub added: u32,
    pub updated: u32,
    pub skipped: u32,
}

/// Recently used search folder scopes (persisted in settings key-value).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentSearchScope {
    pub path: String,
    pub label: String,
}

pub const MAX_RECENT_SEARCH_SCOPES: usize = 3;
const RECENT_SEARCH_SCOPES_KEY: &str = "recent_search_scopes";
const REMOTE_SHARE_FOLDER_IDS_KEY: &str = "remote_share_folder_ids";

/// Cap on remembered search events (co-occurrence source).
pub const MAX_SEARCH_TERM_EVENTS: usize = 30;
/// Cap on distinct terms kept in stats.
pub const MAX_SEARCH_TERM_STATS: usize = 100;
const SEARCH_TERM_HISTORY_KEY: &str = "search_term_history";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SearchTermStat {
    pub count: u32,
    pub last: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchTermEvent {
    pub terms: Vec<String>,
    pub t: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SearchTermHistory {
    #[serde(default)]
    pub events: Vec<SearchTermEvent>,
    #[serde(default)]
    pub stats: std::collections::HashMap<String, SearchTermStat>,
}

/// Row for the + picker history section (MRU by last used).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHistoryTermRow {
    pub term: String,
    pub count: u32,
    pub last: i64,
}

pub struct Db {
    conn: parking_lot::Mutex<rusqlite::Connection>,
}

impl Db {
    pub fn open(path: &std::path::Path) -> Result<Self, rusqlite::Error> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let conn = rusqlite::Connection::open(path)?;
        conn.execute_batch(
            r#"
            PRAGMA journal_mode=WAL;
            PRAGMA foreign_keys=ON;
            CREATE TABLE IF NOT EXISTS settings (
              key TEXT PRIMARY KEY,
              value TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS folders (
              id INTEGER PRIMARY KEY AUTOINCREMENT,
              path TEXT NOT NULL UNIQUE,
              public_path TEXT NOT NULL DEFAULT '',
              enabled INTEGER NOT NULL DEFAULT 1,
              created_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS files (
              id INTEGER PRIMARY KEY AUTOINCREMENT,
              folder_id INTEGER NOT NULL,
              path TEXT NOT NULL UNIQUE,
              ext TEXT,
              size INTEGER,
              mtime INTEGER,
              content_hash TEXT,
              indexed_at TEXT,
              status TEXT NOT NULL DEFAULT 'pending',
              FOREIGN KEY(folder_id) REFERENCES folders(id) ON DELETE CASCADE
            );
            CREATE TABLE IF NOT EXISTS exclude_paths (
              id INTEGER PRIMARY KEY AUTOINCREMENT,
              path TEXT NOT NULL UNIQUE
            );
            CREATE TABLE IF NOT EXISTS search_words (
              id INTEGER PRIMARY KEY AUTOINCREMENT,
              word TEXT NOT NULL UNIQUE,
              reading TEXT NOT NULL DEFAULT '',
              pos_label TEXT NOT NULL DEFAULT '',
              created_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS email_folders (
              id INTEGER PRIMARY KEY AUTOINCREMENT,
              store_id TEXT NOT NULL,
              entry_id TEXT NOT NULL,
              name TEXT NOT NULL DEFAULT '',
              path_label TEXT NOT NULL DEFAULT '',
              selected INTEGER NOT NULL DEFAULT 0,
              item_count INTEGER NOT NULL DEFAULT 0,
              UNIQUE(store_id, entry_id)
            );
            CREATE TABLE IF NOT EXISTS email_messages (
              path TEXT PRIMARY KEY,
              store_id TEXT NOT NULL,
              entry_id TEXT NOT NULL,
              conversation_id TEXT NOT NULL DEFAULT '',
              folder_name TEXT NOT NULL DEFAULT '',
              from_addr TEXT NOT NULL DEFAULT '',
              subject TEXT NOT NULL DEFAULT '',
              date_unix INTEGER NOT NULL DEFAULT 0,
              content_hash TEXT NOT NULL DEFAULT '',
              status TEXT NOT NULL DEFAULT 'pending',
              indexed_at TEXT
            );
            CREATE TABLE IF NOT EXISTS email_threads (
              conversation_id TEXT PRIMARY KEY,
              path TEXT NOT NULL,
              date_unix INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS notes (
              id TEXT PRIMARY KEY,
              title TEXT NOT NULL DEFAULT '',
              memo TEXT NOT NULL DEFAULT '',
              view_mode TEXT NOT NULL DEFAULT 'list',
              sort_order INTEGER NOT NULL DEFAULT 0,
              created_at INTEGER NOT NULL,
              updated_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS note_items (
              id TEXT PRIMARY KEY,
              note_id TEXT NOT NULL,
              sort_order INTEGER NOT NULL DEFAULT 0,
              query TEXT NOT NULL DEFAULT '',
              paragraph_id TEXT NOT NULL DEFAULT '',
              item_json TEXT NOT NULL DEFAULT '{}',
              memo TEXT NOT NULL DEFAULT '',
              created_at INTEGER NOT NULL,
              FOREIGN KEY(note_id) REFERENCES notes(id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_note_items_note_id
              ON note_items(note_id, sort_order);
            CREATE TABLE IF NOT EXISTS llm_threads (
              id TEXT PRIMARY KEY,
              title TEXT NOT NULL DEFAULT '',
              search_enabled INTEGER NOT NULL DEFAULT 1,
              path_prefix TEXT NOT NULL DEFAULT '',
              sort_order INTEGER NOT NULL DEFAULT 0,
              created_at INTEGER NOT NULL,
              updated_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS llm_messages (
              id TEXT PRIMARY KEY,
              thread_id TEXT NOT NULL,
              role TEXT NOT NULL,
              content TEXT NOT NULL DEFAULT '',
              created_at INTEGER NOT NULL,
              FOREIGN KEY(thread_id) REFERENCES llm_threads(id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_llm_messages_thread
              ON llm_messages(thread_id, created_at);
            CREATE TABLE IF NOT EXISTS llm_thread_sources (
              id TEXT PRIMARY KEY,
              thread_id TEXT NOT NULL,
              sort_order INTEGER NOT NULL DEFAULT 0,
              origin TEXT NOT NULL DEFAULT 'attach',
              path TEXT NOT NULL DEFAULT '',
              title TEXT NOT NULL DEFAULT '',
              paragraph_id TEXT NOT NULL DEFAULT '',
              body TEXT NOT NULL DEFAULT '',
              query TEXT NOT NULL DEFAULT '',
              grain TEXT NOT NULL DEFAULT 'unit',
              unit_body TEXT NOT NULL DEFAULT '',
              injected_user_message_id TEXT NOT NULL DEFAULT '',
              cited_assistant_message_id TEXT NOT NULL DEFAULT '',
              cite_no INTEGER NOT NULL DEFAULT 0,
              kind TEXT NOT NULL DEFAULT 'text',
              stored_relpath TEXT NOT NULL DEFAULT '',
              ocr_status TEXT NOT NULL DEFAULT '',
              created_at INTEGER NOT NULL,
              FOREIGN KEY(thread_id) REFERENCES llm_threads(id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_llm_thread_sources_thread
              ON llm_thread_sources(thread_id, sort_order);
            "#,
        )?;
        // Migrate older DBs that lack public_path
        let _ = conn.execute(
            "ALTER TABLE folders ADD COLUMN public_path TEXT NOT NULL DEFAULT ''",
            [],
        );
        // User-dictionary metadata columns (CSV import).
        let _ = conn.execute(
            "ALTER TABLE search_words ADD COLUMN reading TEXT NOT NULL DEFAULT ''",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE search_words ADD COLUMN pos_label TEXT NOT NULL DEFAULT ''",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE email_folders ADD COLUMN item_count INTEGER NOT NULL DEFAULT 0",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE llm_thread_sources ADD COLUMN grain TEXT NOT NULL DEFAULT 'unit'",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE llm_thread_sources ADD COLUMN unit_body TEXT NOT NULL DEFAULT ''",
            [],
        );
        let consume_cols_added = conn
            .execute(
                "ALTER TABLE llm_thread_sources ADD COLUMN injected_user_message_id TEXT NOT NULL DEFAULT ''",
                [],
            )
            .is_ok();
        let _ = conn.execute(
            "ALTER TABLE llm_thread_sources ADD COLUMN cited_assistant_message_id TEXT NOT NULL DEFAULT ''",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE llm_thread_sources ADD COLUMN cite_no INTEGER NOT NULL DEFAULT 0",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE llm_thread_sources ADD COLUMN kind TEXT NOT NULL DEFAULT 'text'",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE llm_thread_sources ADD COLUMN stored_relpath TEXT NOT NULL DEFAULT ''",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE llm_thread_sources ADD COLUMN ocr_status TEXT NOT NULL DEFAULT ''",
            [],
        );
        if consume_cols_added {
            conn.execute_batch(
                "UPDATE llm_thread_sources
                 SET
                   injected_user_message_id = COALESCE((
                     SELECT id FROM llm_messages
                     WHERE thread_id = llm_thread_sources.thread_id AND role = 'user'
                     ORDER BY created_at ASC, id ASC LIMIT 1
                   ), ''),
                   cited_assistant_message_id = COALESCE((
                     SELECT id FROM llm_messages
                     WHERE thread_id = llm_thread_sources.thread_id AND role = 'assistant'
                     ORDER BY created_at ASC, id ASC LIMIT 1
                   ), ''),
                   cite_no = CASE WHEN cite_no = 0 THEN sort_order + 1 ELSE cite_no END
                 WHERE injected_user_message_id = ''
                   AND EXISTS (
                     SELECT 1 FROM llm_messages
                     WHERE thread_id = llm_thread_sources.thread_id AND role = 'assistant'
                   );",
            )?;
        }
        // Note list manual ordering (sidebar drag).
        let notes_sort_added = conn
            .execute(
                "ALTER TABLE notes ADD COLUMN sort_order INTEGER NOT NULL DEFAULT 0",
                [],
            )
            .is_ok();
        if notes_sort_added {
            let ids: Vec<String> = {
                let mut stmt =
                    conn.prepare("SELECT id FROM notes ORDER BY updated_at DESC, created_at DESC")?;
                let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
                rows.flatten().collect()
            };
            for (i, id) in ids.iter().enumerate() {
                conn.execute(
                    "UPDATE notes SET sort_order=?1 WHERE id=?2",
                    rusqlite::params![i as i64, id],
                )?;
            }
        }
        // Chat thread list manual ordering (sidebar drag).
        let threads_sort_added = conn
            .execute(
                "ALTER TABLE llm_threads ADD COLUMN sort_order INTEGER NOT NULL DEFAULT 0",
                [],
            )
            .is_ok();
        if threads_sort_added {
            let ids: Vec<String> = {
                let mut stmt = conn.prepare(
                    "SELECT id FROM llm_threads ORDER BY updated_at DESC, created_at DESC",
                )?;
                let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
                rows.flatten().collect()
            };
            for (i, id) in ids.iter().enumerate() {
                conn.execute(
                    "UPDATE llm_threads SET sort_order=?1 WHERE id=?2",
                    rusqlite::params![i as i64, id],
                )?;
            }
        }
        let _ = conn.execute(
            "ALTER TABLE llm_threads ADD COLUMN note_id TEXT NOT NULL DEFAULT ''",
            [],
        );
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS llm_note_proposals (
              id TEXT PRIMARY KEY,
              thread_id TEXT NOT NULL,
              note_id TEXT NOT NULL,
              request_id TEXT NOT NULL DEFAULT '',
              assistant_message_id TEXT NOT NULL DEFAULT '',
              kind TEXT NOT NULL,
              heading TEXT NOT NULL DEFAULT '',
              old_text TEXT NOT NULL DEFAULT '',
              new_text TEXT NOT NULL DEFAULT '',
              chunk TEXT NOT NULL DEFAULT '',
              note_updated_at INTEGER NOT NULL DEFAULT 0,
              status TEXT NOT NULL DEFAULT 'pending',
              created_at INTEGER NOT NULL,
              applied_at INTEGER,
              FOREIGN KEY(thread_id) REFERENCES llm_threads(id) ON DELETE CASCADE,
              FOREIGN KEY(note_id) REFERENCES notes(id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_llm_note_proposals_thread
              ON llm_note_proposals(thread_id, created_at);
            CREATE INDEX IF NOT EXISTS idx_llm_note_proposals_note
              ON llm_note_proposals(note_id, status);
            CREATE INDEX IF NOT EXISTS idx_llm_note_proposals_request
              ON llm_note_proposals(request_id);
            ",
        )?;
        // Ensure FK is on for this connection (WAL batch may not stick across all cases)
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;
        Ok(Self {
            conn: parking_lot::Mutex::new(conn),
        })
    }

    pub fn load_settings(&self) -> Settings {
        let mut s = Settings::default();
        let conn = self.conn.lock();
        let mut stmt = match conn.prepare("SELECT key, value FROM settings") {
            Ok(st) => st,
            Err(_) => return s,
        };
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        });
        if let Ok(rows) = rows {
            for row in rows.flatten() {
                match row.0.as_str() {
                    "shortcut" => s.shortcut = row.1,
                    "notes_shortcut" => s.notes_shortcut = row.1,
                    "max_results" => s.max_results = row.1.parse().unwrap_or(10),
                    "font_size" => s.font_size = row.1.parse().unwrap_or(14),
                    "index_interval_secs" => s.index_interval_secs = row.1.parse().unwrap_or(3600),
                    "autostart" => s.autostart = row.1 == "1" || row.1 == "true",
                    "popup_width" => s.popup_width = row.1.parse().unwrap_or(640),
                    "popup_height" => s.popup_height = row.1.parse().unwrap_or(520),
                    "popup_position" => {
                        s.popup_position = match row.1.as_str() {
                            "left" | "right" | "center" => row.1,
                            _ => "center".into(),
                        }
                    }
                    "remote_server_enabled" => {
                        s.remote_server_enabled = row.1 == "1" || row.1 == "true"
                    }
                    "remote_server_port" => s.remote_server_port = row.1.parse().unwrap_or(17890),
                    "remote_server_token" => s.remote_server_token = row.1,
                    "search_mode" => {
                        s.search_mode = match row.1.as_str() {
                            "remote" | "hybrid" | "local" => row.1,
                            _ => "local".into(),
                        }
                    }
                    "remote_url" => s.remote_url = row.1,
                    "remote_token" => s.remote_token = row.1,
                    "remote_timeout_ms" => s.remote_timeout_ms = row.1.parse().unwrap_or(3000),
                    "pos_filter_enabled" => s.pos_filter_enabled = row.1 == "1" || row.1 == "true",
                    "mail_enabled" => s.mail_enabled = row.1 == "1" || row.1 == "true",
                    "mail_days_back" => s.mail_days_back = row.1.parse().unwrap_or(730),
                    "mail_sync_interval_secs" => {
                        s.mail_sync_interval_secs = row.1.parse().unwrap_or(3600)
                    }
                    "mail_latest_only" => s.mail_latest_only = row.1 == "1" || row.1 == "true",
                    "mail_thread_collapse" => {
                        s.mail_thread_collapse = !(row.1 == "0" || row.1 == "false")
                    }
                    "mail_last_sync_at" => s.mail_last_sync_at = row.1,
                    "llm_base_url" => {
                        s.llm_base_url = if row.1.trim().is_empty() {
                            DEFAULT_LLM_BASE_URL.into()
                        } else {
                            row.1
                        }
                    }
                    "llm_api_key" => s.llm_api_key = row.1,
                    "llm_model" => s.llm_model = row.1,
                    "llm_timeout_ms" => {
                        s.llm_timeout_ms = row.1.parse().unwrap_or(DEFAULT_LLM_TIMEOUT_MS)
                    }
                    "llm_max_context_chars" => {
                        s.llm_max_context_chars =
                            row.1.parse().unwrap_or(DEFAULT_LLM_MAX_CONTEXT_CHARS)
                    }
                    "llm_system_prompt" => {
                        s.llm_system_prompt = if row.1.is_empty()
                            || row.1 == LEGACY_LLM_SYSTEM_PROMPT
                            || row.1 == LEGACY_TOOL_LLM_SYSTEM_PROMPT
                        {
                            DEFAULT_LLM_SYSTEM_PROMPT.into()
                        } else {
                            row.1
                        }
                    }
                    "llm_search_top_k" => {
                        s.llm_search_top_k = row.1.parse().unwrap_or(4).clamp(1, 16)
                    }
                    "llm_thinking" => {
                        s.llm_thinking = match row.1.as_str() {
                            "auto" | "brief" | "off" => row.1,
                            _ => DEFAULT_LLM_THINKING.into(),
                        }
                    }
                    "llm_thinking_budget" => {
                        s.llm_thinking_budget = row
                            .1
                            .parse()
                            .unwrap_or(DEFAULT_LLM_THINKING_BUDGET)
                            .min(32_000)
                    }
                    "searxng_url" => s.searxng_url = row.1,
                    "searxng_timeout_ms" => {
                        s.searxng_timeout_ms = row
                            .1
                            .parse()
                            .unwrap_or(DEFAULT_SEARXNG_TIMEOUT_MS)
                            .clamp(5_000, 30_000)
                    }
                    "llm_web_search_top_k" => {
                        s.llm_web_search_top_k = row
                            .1
                            .parse()
                            .unwrap_or(DEFAULT_LLM_WEB_SEARCH_TOP_K)
                            .clamp(1, 8)
                    }
                    _ => {}
                }
            }
        }
        s
    }

    pub fn save_settings(&self, s: &Settings) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock();
        let pairs = [
            ("shortcut", s.shortcut.clone()),
            ("notes_shortcut", s.notes_shortcut.clone()),
            ("max_results", s.max_results.to_string()),
            ("font_size", s.font_size.to_string()),
            ("index_interval_secs", s.index_interval_secs.to_string()),
            ("autostart", if s.autostart { "1" } else { "0" }.to_string()),
            ("popup_width", s.popup_width.to_string()),
            ("popup_height", s.popup_height.to_string()),
            ("popup_position", s.popup_position.clone()),
            (
                "remote_server_enabled",
                if s.remote_server_enabled { "1" } else { "0" }.to_string(),
            ),
            ("remote_server_port", s.remote_server_port.to_string()),
            ("remote_server_token", s.remote_server_token.clone()),
            ("search_mode", s.search_mode.clone()),
            ("remote_url", s.remote_url.clone()),
            ("remote_token", s.remote_token.clone()),
            ("remote_timeout_ms", s.remote_timeout_ms.to_string()),
            (
                "pos_filter_enabled",
                if s.pos_filter_enabled { "1" } else { "0" }.to_string(),
            ),
            (
                "mail_enabled",
                if s.mail_enabled { "1" } else { "0" }.to_string(),
            ),
            ("mail_days_back", s.mail_days_back.to_string()),
            (
                "mail_sync_interval_secs",
                s.mail_sync_interval_secs.to_string(),
            ),
            (
                "mail_latest_only",
                if s.mail_latest_only { "1" } else { "0" }.to_string(),
            ),
            (
                "mail_thread_collapse",
                if s.mail_thread_collapse { "1" } else { "0" }.to_string(),
            ),
            ("mail_last_sync_at", s.mail_last_sync_at.clone()),
            ("llm_base_url", s.llm_base_url.clone()),
            ("llm_api_key", s.llm_api_key.clone()),
            ("llm_model", s.llm_model.clone()),
            ("llm_timeout_ms", s.llm_timeout_ms.to_string()),
            ("llm_max_context_chars", s.llm_max_context_chars.to_string()),
            ("llm_system_prompt", s.llm_system_prompt.clone()),
            ("llm_thinking", s.llm_thinking.clone()),
            ("llm_thinking_budget", s.llm_thinking_budget.to_string()),
            ("llm_search_top_k", s.llm_search_top_k.to_string()),
            ("searxng_url", s.searxng_url.clone()),
            ("searxng_timeout_ms", s.searxng_timeout_ms.to_string()),
            ("llm_web_search_top_k", s.llm_web_search_top_k.to_string()),
        ];
        for (k, v) in pairs {
            conn.execute(
                "INSERT INTO settings(key, value) VALUES(?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                rusqlite::params![k, v],
            )?;
        }
        Ok(())
    }

    pub fn list_folders(&self) -> Result<Vec<FolderRow>, rusqlite::Error> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT f.id, f.path, COALESCE(f.public_path, ''), f.enabled,
                    COALESCE((SELECT COUNT(*) FROM files WHERE folder_id = f.id), 0)
             FROM folders f
             ORDER BY f.id",
        )?;
        let rows = stmt.query_map([], folder_row_from_sql)?;
        let folders: Vec<FolderRow> = rows.flatten().map(FolderRow::with_exists).collect();
        drop(stmt);
        drop(conn);
        Ok(self.overlay_share_remote_all(folders))
    }

    pub fn add_folder(&self, path: &str, public_path: &str) -> Result<FolderRow, rusqlite::Error> {
        let conn = self.conn.lock();
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT OR IGNORE INTO folders(path, public_path, enabled, created_at) VALUES(?1, ?2, 1, ?3)",
            rusqlite::params![path, public_path, now],
        )?;
        let id: i64 =
            conn.query_row("SELECT id FROM folders WHERE path=?1", [path], |r| r.get(0))?;
        let public_path: String = conn.query_row(
            "SELECT COALESCE(public_path, '') FROM folders WHERE id=?1",
            [id],
            |r| r.get(0),
        )?;
        drop(conn);
        Ok(self.overlay_share_remote_one(
            FolderRow {
                id,
                path: path.to_string(),
                public_path,
                enabled: true,
                indexed_count: 0,
                exists: false,
                share_remote: false,
            }
            .with_exists(),
        ))
    }

    pub fn update_folder_public_path(
        &self,
        id: i64,
        public_path: &str,
    ) -> Result<Option<FolderRow>, rusqlite::Error> {
        let conn = self.conn.lock();
        let n = conn.execute(
            "UPDATE folders SET public_path=?1 WHERE id=?2",
            rusqlite::params![public_path, id],
        )?;
        if n == 0 {
            return Ok(None);
        }
        drop(conn);
        self.get_folder(id)
    }

    /// Update the registered filesystem path for a folder (rebind after rename/move).
    pub fn update_folder_path(
        &self,
        id: i64,
        new_path: &str,
    ) -> Result<Option<FolderRow>, rusqlite::Error> {
        let conn = self.conn.lock();
        let n = conn.execute(
            "UPDATE folders SET path=?1 WHERE id=?2",
            rusqlite::params![new_path, id],
        )?;
        if n == 0 {
            return Ok(None);
        }
        drop(conn);
        self.get_folder(id)
    }

    /// Rewrite `files.path` prefix for one folder. Uses a two-phase update to avoid UNIQUE clashes.
    pub fn remap_file_paths_prefix(
        &self,
        folder_id: i64,
        from_prefix: &str,
        to_prefix: &str,
    ) -> Result<u32, rusqlite::Error> {
        let from = crate::pathutil::simplify_windows_path(from_prefix);
        let to = crate::pathutil::simplify_windows_path(to_prefix);
        if from.is_empty() || to.is_empty() || from.eq_ignore_ascii_case(&to) {
            return Ok(0);
        }
        let paths = self.list_file_paths_by_folder(folder_id)?;
        if paths.is_empty() {
            return Ok(0);
        }
        let conn = self.conn.lock();
        let tx = conn.unchecked_transaction()?;
        const TEMP_MARK: &str = "\u{0001}argos_remap\u{0001}";
        let mut pairs: Vec<(String, String)> = Vec::new();
        for path in &paths {
            let new_path = crate::pathutil::rewrite_prefix(path, &from, &to);
            if new_path == *path {
                continue;
            }
            pairs.push((path.clone(), new_path));
        }
        for (old, _) in &pairs {
            tx.execute(
                "UPDATE files SET path=?1 WHERE path=?2 AND folder_id=?3",
                rusqlite::params![format!("{TEMP_MARK}{old}"), old, folder_id],
            )?;
        }
        let mut updated = 0u32;
        for (old, new_path) in &pairs {
            tx.execute(
                "UPDATE files SET path=?1 WHERE path=?2 AND folder_id=?3",
                rusqlite::params![new_path, format!("{TEMP_MARK}{old}"), folder_id],
            )?;
            updated += 1;
        }
        tx.commit()?;
        Ok(updated)
    }

    pub fn remove_folder(&self, id: i64) -> Result<Option<String>, rusqlite::Error> {
        let conn = self.conn.lock();
        let _ = conn.execute_batch("PRAGMA foreign_keys=ON;")?;
        let path: Option<String> = conn
            .query_row("SELECT path FROM folders WHERE id=?1", [id], |r| r.get(0))
            .ok();
        // Explicitly remove file rows (CASCADE may be off on older sessions)
        conn.execute("DELETE FROM files WHERE folder_id=?1", [id])?;
        conn.execute("DELETE FROM folders WHERE id=?1", [id])?;
        drop(conn);
        self.prune_remote_share_folder_id(id)?;
        Ok(path)
    }

    pub fn get_folder(&self, id: i64) -> Result<Option<FolderRow>, rusqlite::Error> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT f.id, f.path, COALESCE(f.public_path, ''), f.enabled,
                    COALESCE((SELECT COUNT(*) FROM files WHERE folder_id = f.id), 0)
             FROM folders f
             WHERE f.id=?1",
        )?;
        let mut rows = stmt.query_map([id], folder_row_from_sql)?;
        let row = rows.next().transpose()?;
        drop(rows);
        drop(stmt);
        drop(conn);
        Ok(row.map(|r| self.overlay_share_remote_one(r.with_exists())))
    }

    pub fn get_folder_by_path(&self, path: &str) -> Result<Option<FolderRow>, rusqlite::Error> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT f.id, f.path, COALESCE(f.public_path, ''), f.enabled,
                    COALESCE((SELECT COUNT(*) FROM files WHERE folder_id = f.id), 0)
             FROM folders f
             WHERE f.path=?1",
        )?;
        let mut rows = stmt.query_map([path], folder_row_from_sql)?;
        let row = rows.next().transpose()?;
        drop(rows);
        drop(stmt);
        drop(conn);
        Ok(row.map(|r| self.overlay_share_remote_one(r.with_exists())))
    }

    pub fn list_file_paths_by_folder(
        &self,
        folder_id: i64,
    ) -> Result<Vec<String>, rusqlite::Error> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare("SELECT path FROM files WHERE folder_id=?1")?;
        let rows = stmt.query_map([folder_id], |row| row.get(0))?;
        Ok(rows.flatten().collect())
    }

    pub fn list_remote_share_folder_ids(&self) -> Vec<i64> {
        let conn = self.conn.lock();
        let value: Result<String, _> = conn.query_row(
            "SELECT value FROM settings WHERE key=?1",
            [REMOTE_SHARE_FOLDER_IDS_KEY],
            |r| r.get(0),
        );
        drop(conn);
        parse_remote_share_folder_ids(value.ok().as_deref())
    }

    fn save_remote_share_folder_ids(&self, ids: &[i64]) -> Result<(), rusqlite::Error> {
        let raw = serde_json::to_string(ids).unwrap_or_else(|_| "[]".into());
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO settings(key, value) VALUES(?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            rusqlite::params![REMOTE_SHARE_FOLDER_IDS_KEY, raw],
        )?;
        Ok(())
    }

    fn overlay_share_remote_one(&self, mut row: FolderRow) -> FolderRow {
        row.share_remote = self.list_remote_share_folder_ids().contains(&row.id);
        row
    }

    fn overlay_share_remote_all(&self, mut folders: Vec<FolderRow>) -> Vec<FolderRow> {
        let ids: std::collections::HashSet<i64> =
            self.list_remote_share_folder_ids().into_iter().collect();
        for folder in &mut folders {
            folder.share_remote = ids.contains(&folder.id);
        }
        folders
    }

    pub fn set_folder_share_remote(
        &self,
        id: i64,
        share_remote: bool,
    ) -> Result<Option<FolderRow>, rusqlite::Error> {
        let Some(_) = self.get_folder(id)? else {
            return Ok(None);
        };
        let mut ids = self.list_remote_share_folder_ids();
        if share_remote {
            if !ids.contains(&id) {
                ids.push(id);
            }
        } else {
            ids.retain(|&existing| existing != id);
        }
        self.save_remote_share_folder_ids(&ids)?;
        self.get_folder(id)
    }

    fn prune_remote_share_folder_id(&self, id: i64) -> Result<(), rusqlite::Error> {
        let ids = self.list_remote_share_folder_ids();
        if !ids.contains(&id) {
            return Ok(());
        }
        let next: Vec<i64> = ids.into_iter().filter(|&existing| existing != id).collect();
        self.save_remote_share_folder_ids(&next)
    }

    /// Indexed files whose `mtime` falls in `[after_unix, before_unix]` (inclusive).
    /// Unbounded sides are `None`. Status is `ok` (not `indexed`).
    pub fn list_ok_file_paths_in_range(
        &self,
        after_unix: Option<i64>,
        before_unix: Option<i64>,
        limit: usize,
    ) -> Result<Vec<String>, rusqlite::Error> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT path FROM files
             WHERE status='ok' AND mtime > 0
               AND (?1 IS NULL OR mtime >= ?1)
               AND (?2 IS NULL OR mtime <= ?2)
             ORDER BY mtime DESC
             LIMIT ?3",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![after_unix, before_unix, limit as i64],
            |row| row.get(0),
        )?;
        Ok(rows.flatten().collect())
    }

    /// Indexed files under `prefix`, for a folder-scoped path allowlist.
    ///
    /// `LIKE` narrows in SQL, then [`pathutil::path_starts_with`] enforces the separator
    /// boundary so the scope `C:\cases\alpha` never picks up `C:\cases\alpha2`. The escape
    /// character is `|`, which Windows forbids in a path, so a folder literally named
    /// `100%` cannot turn into a wildcard.
    pub fn list_ok_file_paths_under_prefix(
        &self,
        prefix: &str,
        limit: usize,
    ) -> Result<Vec<String>, rusqlite::Error> {
        let normalized = crate::pathutil::simplify_windows_path(prefix);
        if normalized.is_empty() {
            return Ok(Vec::new());
        }
        let pattern = format!("{}%", escape_like_operand(&normalized));
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT path FROM files
             WHERE status='ok' AND path LIKE ?1 ESCAPE '|'
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![pattern, limit as i64], |row| {
            row.get::<_, String>(0)
        })?;
        Ok(rows
            .flatten()
            .filter(|path| crate::pathutil::path_starts_with(path, &normalized))
            .collect())
    }

    pub fn list_exclude_paths(&self) -> Result<Vec<ExcludePathRow>, rusqlite::Error> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare("SELECT id, path FROM exclude_paths ORDER BY id")?;
        let rows = stmt.query_map([], |row| {
            Ok(ExcludePathRow {
                id: row.get(0)?,
                path: row.get(1)?,
            })
        })?;
        Ok(rows.flatten().collect())
    }

    pub fn add_exclude_path(&self, path: &str) -> Result<ExcludePathRow, rusqlite::Error> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT OR IGNORE INTO exclude_paths(path) VALUES(?1)",
            [path],
        )?;
        let id: i64 =
            conn.query_row("SELECT id FROM exclude_paths WHERE path=?1", [path], |r| {
                r.get(0)
            })?;
        Ok(ExcludePathRow {
            id,
            path: path.to_string(),
        })
    }

    pub fn remove_exclude_path(&self, id: i64) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock();
        conn.execute("DELETE FROM exclude_paths WHERE id=?1", [id])?;
        Ok(())
    }

    pub fn list_search_words(&self) -> Result<Vec<SearchWordRow>, rusqlite::Error> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id, word, COALESCE(reading, ''), COALESCE(pos_label, '')
             FROM search_words ORDER BY id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(SearchWordRow {
                id: row.get(0)?,
                word: row.get(1)?,
                reading: row.get(2)?,
                pos_label: row.get(3)?,
            })
        })?;
        Ok(rows.flatten().collect())
    }

    pub fn add_search_word(
        &self,
        word: &str,
        reading: &str,
        pos_label: &str,
    ) -> Result<SearchWordRow, rusqlite::Error> {
        let conn = self.conn.lock();
        let now = chrono::Utc::now().to_rfc3339();
        let reading = reading.trim();
        let pos_label = if pos_label.trim().is_empty() {
            "ユーザ辞書"
        } else {
            pos_label.trim()
        };
        conn.execute(
            "INSERT INTO search_words(word, reading, pos_label, created_at)
             VALUES(?1, ?2, ?3, ?4)
             ON CONFLICT(word) DO UPDATE SET
               reading=excluded.reading,
               pos_label=excluded.pos_label",
            rusqlite::params![word, reading, pos_label, now],
        )?;
        let id: i64 = conn.query_row("SELECT id FROM search_words WHERE word=?1", [word], |r| {
            r.get(0)
        })?;
        Ok(SearchWordRow {
            id,
            word: word.to_string(),
            reading: reading.to_string(),
            pos_label: pos_label.to_string(),
        })
    }

    pub fn update_search_word(
        &self,
        id: i64,
        word: &str,
        reading: Option<&str>,
        pos_label: Option<&str>,
    ) -> Result<Option<SearchWordRow>, rusqlite::Error> {
        let conn = self.conn.lock();
        let existing: Option<(String, String)> = conn
            .query_row(
                "SELECT COALESCE(reading, ''), COALESCE(pos_label, '') FROM search_words WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok();
        let Some((old_reading, old_pos)) = existing else {
            return Ok(None);
        };
        let reading = reading.unwrap_or(old_reading.as_str()).trim();
        let pos_label = pos_label
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(if old_pos.is_empty() {
                "ユーザ辞書"
            } else {
                old_pos.as_str()
            });
        let n = conn.execute(
            "UPDATE search_words SET word=?1, reading=?2, pos_label=?3 WHERE id=?4",
            rusqlite::params![word, reading, pos_label, id],
        )?;
        if n == 0 {
            return Ok(None);
        }
        Ok(Some(SearchWordRow {
            id,
            word: word.to_string(),
            reading: reading.to_string(),
            pos_label: pos_label.to_string(),
        }))
    }

    pub fn remove_search_word(&self, id: i64) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock();
        conn.execute("DELETE FROM search_words WHERE id=?1", [id])?;
        Ok(())
    }

    pub fn clear_search_words(&self) -> Result<u64, rusqlite::Error> {
        let conn = self.conn.lock();
        let n = conn.execute("DELETE FROM search_words", [])?;
        Ok(n as u64)
    }

    pub fn import_search_words(
        &self,
        entries: &[SearchWordImport],
    ) -> Result<SearchWordImportResult, rusqlite::Error> {
        let mut added = 0u32;
        let mut updated = 0u32;
        let mut skipped = 0u32;
        for entry in entries {
            let word = entry.word.trim();
            if word.is_empty() {
                skipped += 1;
                continue;
            }
            let existed: bool = {
                let conn = self.conn.lock();
                conn.query_row("SELECT 1 FROM search_words WHERE word=?1", [word], |_| {
                    Ok(true)
                })
                .unwrap_or(false)
            };
            self.add_search_word(word, &entry.reading, &entry.pos_label)?;
            if existed {
                updated += 1;
            } else {
                added += 1;
            }
        }
        Ok(SearchWordImportResult {
            added,
            updated,
            skipped,
        })
    }

    pub fn list_recent_search_scopes(&self) -> Vec<RecentSearchScope> {
        let conn = self.conn.lock();
        let value: Result<String, _> = conn.query_row(
            "SELECT value FROM settings WHERE key=?1",
            [RECENT_SEARCH_SCOPES_KEY],
            |r| r.get(0),
        );
        let Ok(raw) = value else {
            return Vec::new();
        };
        serde_json::from_str::<Vec<RecentSearchScope>>(&raw)
            .unwrap_or_default()
            .into_iter()
            .filter(|s| !s.path.trim().is_empty())
            .take(MAX_RECENT_SEARCH_SCOPES)
            .collect()
    }

    pub fn push_recent_search_scope(
        &self,
        path: &str,
        label: &str,
    ) -> Result<Vec<RecentSearchScope>, rusqlite::Error> {
        let path = path.trim();
        if path.is_empty() {
            return Ok(self.list_recent_search_scopes());
        }
        let label = {
            let t = label.trim();
            if t.is_empty() {
                path.to_string()
            } else {
                t.to_string()
            }
        };

        let mut next: Vec<RecentSearchScope> = Vec::with_capacity(MAX_RECENT_SEARCH_SCOPES);
        next.push(RecentSearchScope {
            path: path.to_string(),
            label,
        });
        for s in self.list_recent_search_scopes() {
            if s.path.eq_ignore_ascii_case(path) {
                continue;
            }
            next.push(s);
            if next.len() >= MAX_RECENT_SEARCH_SCOPES {
                break;
            }
        }

        let raw = serde_json::to_string(&next).unwrap_or_else(|_| "[]".into());
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO settings(key, value) VALUES(?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            rusqlite::params![RECENT_SEARCH_SCOPES_KEY, raw],
        )?;
        Ok(next)
    }

    fn load_search_term_history_locked(conn: &rusqlite::Connection) -> SearchTermHistory {
        let value: Result<String, _> = conn.query_row(
            "SELECT value FROM settings WHERE key=?1",
            [SEARCH_TERM_HISTORY_KEY],
            |r| r.get(0),
        );
        let Ok(raw) = value else {
            return SearchTermHistory::default();
        };
        serde_json::from_str(&raw).unwrap_or_default()
    }

    fn save_search_term_history_locked(
        conn: &rusqlite::Connection,
        history: &SearchTermHistory,
    ) -> Result<(), rusqlite::Error> {
        let raw = serde_json::to_string(history).unwrap_or_else(|_| {
            serde_json::to_string(&SearchTermHistory::default()).unwrap_or_else(|_| "{}".into())
        });
        conn.execute(
            "INSERT INTO settings(key, value) VALUES(?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            rusqlite::params![SEARCH_TERM_HISTORY_KEY, raw],
        )?;
        Ok(())
    }

    pub fn get_search_term_history(&self) -> SearchTermHistory {
        let conn = self.conn.lock();
        Self::load_search_term_history_locked(&conn)
    }

    /// MRU terms for the + picker (newest `last` first).
    pub fn list_search_history_terms(&self) -> Vec<SearchHistoryTermRow> {
        let history = self.get_search_term_history();
        let mut rows: Vec<SearchHistoryTermRow> = history
            .stats
            .into_iter()
            .filter(|(term, _)| !term.trim().is_empty())
            .map(|(term, st)| SearchHistoryTermRow {
                term,
                count: st.count,
                last: st.last,
            })
            .collect();
        rows.sort_by(|a, b| b.last.cmp(&a.last).then_with(|| b.count.cmp(&a.count)));
        rows
    }

    pub fn record_search_terms(&self, terms: &[String]) -> Result<(), rusqlite::Error> {
        let mut cleaned: Vec<String> = Vec::new();
        for t in terms {
            let term = t.trim();
            if term.is_empty() {
                continue;
            }
            if cleaned.iter().any(|x| x == term) {
                continue;
            }
            cleaned.push(term.to_string());
        }
        if cleaned.is_empty() {
            return Ok(());
        }

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        let conn = self.conn.lock();
        let mut history = Self::load_search_term_history_locked(&conn);

        history.events.insert(
            0,
            SearchTermEvent {
                terms: cleaned.clone(),
                t: now,
            },
        );
        history.events.truncate(MAX_SEARCH_TERM_EVENTS);

        for term in &cleaned {
            let entry = history
                .stats
                .entry(term.clone())
                .or_insert(SearchTermStat { count: 0, last: 0 });
            entry.count = entry.count.saturating_add(1);
            entry.last = now;
        }

        if history.stats.len() > MAX_SEARCH_TERM_STATS {
            let mut by_last: Vec<(String, i64, u32)> = history
                .stats
                .iter()
                .map(|(k, v)| (k.clone(), v.last, v.count))
                .collect();
            by_last.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| b.2.cmp(&a.2)));
            by_last.truncate(MAX_SEARCH_TERM_STATS);
            let keep: std::collections::HashSet<String> =
                by_last.into_iter().map(|(k, _, _)| k).collect();
            history.stats.retain(|k, _| keep.contains(k));
        }

        Self::save_search_term_history_locked(&conn, &history)
    }

    pub fn clear_search_term_history(&self) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock();
        Self::save_search_term_history_locked(&conn, &SearchTermHistory::default())
    }

    pub fn folder_id_by_path(&self, path: &str) -> Result<Option<i64>, rusqlite::Error> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare("SELECT id FROM folders WHERE path=?1")?;
        let mut rows = stmt.query_map([path], |r| r.get(0))?;
        if let Some(Ok(id)) = rows.next() {
            return Ok(Some(id));
        }
        Ok(None)
    }

    pub fn get_file_meta(&self, path: &str) -> Result<Option<FileMeta>, rusqlite::Error> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare("SELECT size, mtime, content_hash FROM files WHERE path=?1")?;
        let mut rows = stmt.query_map([path], |row| {
            Ok(FileMeta {
                size: row.get(0)?,
                mtime: row.get(1)?,
                content_hash: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
            })
        })?;
        if let Some(Ok(m)) = rows.next() {
            return Ok(Some(m));
        }
        Ok(None)
    }

    pub fn upsert_file(
        &self,
        folder_id: i64,
        path: &str,
        ext: &str,
        size: i64,
        mtime: i64,
        content_hash: &str,
    ) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock();
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO files(folder_id, path, ext, size, mtime, content_hash, indexed_at, status)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, 'ok')
             ON CONFLICT(path) DO UPDATE SET
               folder_id=excluded.folder_id,
               ext=excluded.ext,
               size=excluded.size,
               mtime=excluded.mtime,
               content_hash=excluded.content_hash,
               indexed_at=excluded.indexed_at,
               status='ok'",
            rusqlite::params![folder_id, path, ext, size, mtime, content_hash, now],
        )?;
        Ok(())
    }

    pub fn mark_file_deleted(&self, path: &str) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock();
        conn.execute("DELETE FROM files WHERE path=?1", [path])?;
        Ok(())
    }

    pub fn clear_all_files(&self) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock();
        conn.execute("DELETE FROM files", [])?;
        Ok(())
    }

    pub fn clear_files_by_folder(&self, folder_id: i64) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock();
        conn.execute("DELETE FROM files WHERE folder_id=?1", [folder_id])?;
        Ok(())
    }

    // --- Outlook mail metadata (not tied to FS folders) ---

    pub fn replace_email_folder_catalog(
        &self,
        folders: &[EmailFolderRow],
    ) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock();
        let selected: std::collections::HashSet<(String, String)> = {
            let mut stmt =
                conn.prepare("SELECT store_id, entry_id FROM email_folders WHERE selected=1")?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.flatten().collect()
        };
        conn.execute("DELETE FROM email_folders", [])?;
        for f in folders {
            let is_selected = selected.contains(&(f.store_id.clone(), f.entry_id.clone()));
            conn.execute(
                "INSERT INTO email_folders(store_id, entry_id, name, path_label, selected, item_count)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    f.store_id,
                    f.entry_id,
                    f.name,
                    f.path_label,
                    if is_selected { 1 } else { 0 },
                    f.item_count
                ],
            )?;
        }
        Ok(())
    }

    pub fn list_email_folders(&self) -> Result<Vec<EmailFolderRow>, rusqlite::Error> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT f.id, f.store_id, f.entry_id, f.name, f.path_label, f.selected, f.item_count,
                    (SELECT COUNT(*) FROM email_messages m
                     WHERE m.status='indexed'
                       AND m.store_id = f.store_id
                       AND (m.folder_name = f.path_label OR m.folder_name = f.name)) AS indexed_count
             FROM email_folders f
             ORDER BY f.path_label COLLATE NOCASE",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(EmailFolderRow {
                id: row.get(0)?,
                store_id: row.get(1)?,
                entry_id: row.get(2)?,
                name: row.get(3)?,
                path_label: row.get(4)?,
                selected: row.get::<_, i64>(5)? != 0,
                item_count: row.get(6)?,
                indexed_count: row.get::<_, i64>(7)? as u32,
            })
        })?;
        Ok(rows.flatten().collect())
    }

    pub fn list_selected_email_folders(&self) -> Result<Vec<EmailFolderRow>, rusqlite::Error> {
        Ok(self
            .list_email_folders()?
            .into_iter()
            .filter(|f| f.selected)
            .collect())
    }

    pub fn set_email_folders_selected(
        &self,
        keys: &[(String, String)],
    ) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock();
        conn.execute("UPDATE email_folders SET selected=0", [])?;
        for (store_id, entry_id) in keys {
            conn.execute(
                "UPDATE email_folders SET selected=1 WHERE store_id=?1 AND entry_id=?2",
                rusqlite::params![store_id, entry_id],
            )?;
        }
        Ok(())
    }

    pub fn get_email_message_by_path(
        &self,
        path: &str,
    ) -> Result<Option<EmailMessageRow>, rusqlite::Error> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT path, store_id, entry_id, conversation_id, folder_name, from_addr,
                    subject, date_unix, content_hash, status
             FROM email_messages WHERE path=?1",
        )?;
        let mut rows = stmt.query_map([path], |row| {
            Ok(EmailMessageRow {
                path: row.get(0)?,
                store_id: row.get(1)?,
                entry_id: row.get(2)?,
                conversation_id: row.get(3)?,
                folder_name: row.get(4)?,
                from_addr: row.get(5)?,
                subject: row.get(6)?,
                date_unix: row.get(7)?,
                content_hash: row.get(8)?,
                status: row.get(9)?,
            })
        })?;
        if let Some(Ok(m)) = rows.next() {
            return Ok(Some(m));
        }
        Ok(None)
    }

    pub fn upsert_email_message(
        &self,
        path: &str,
        store_id: &str,
        entry_id: &str,
        conversation_id: &str,
        folder_name: &str,
        from_addr: &str,
        subject: &str,
        date_unix: i64,
        content_hash: &str,
        status: &str,
    ) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock();
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO email_messages(
                path, store_id, entry_id, conversation_id, folder_name, from_addr,
                subject, date_unix, content_hash, status, indexed_at
             ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)
             ON CONFLICT(path) DO UPDATE SET
                store_id=excluded.store_id,
                entry_id=excluded.entry_id,
                conversation_id=excluded.conversation_id,
                folder_name=excluded.folder_name,
                from_addr=excluded.from_addr,
                subject=excluded.subject,
                date_unix=excluded.date_unix,
                content_hash=excluded.content_hash,
                status=excluded.status,
                indexed_at=excluded.indexed_at",
            rusqlite::params![
                path,
                store_id,
                entry_id,
                conversation_id,
                folder_name,
                from_addr,
                subject,
                date_unix,
                content_hash,
                status,
                now
            ],
        )?;
        Ok(())
    }

    pub fn set_email_message_status(
        &self,
        path: &str,
        status: &str,
    ) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock();
        conn.execute(
            "UPDATE email_messages SET status=?1 WHERE path=?2",
            rusqlite::params![status, path],
        )?;
        Ok(())
    }

    pub fn upsert_email_thread(
        &self,
        conversation_id: &str,
        path: &str,
        date_unix: i64,
    ) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock();
        let existing: Option<(String, i64)> = conn
            .query_row(
                "SELECT path, date_unix FROM email_threads WHERE conversation_id=?1",
                [conversation_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok();
        if let Some((_, old_date)) = existing {
            if date_unix < old_date {
                return Ok(());
            }
        }
        conn.execute(
            "INSERT INTO email_threads(conversation_id, path, date_unix) VALUES(?1,?2,?3)
             ON CONFLICT(conversation_id) DO UPDATE SET
               path=excluded.path,
               date_unix=excluded.date_unix",
            rusqlite::params![conversation_id, path, date_unix],
        )?;
        Ok(())
    }

    pub fn get_email_thread_winner(
        &self,
        conversation_id: &str,
    ) -> Result<Option<EmailThreadWinner>, rusqlite::Error> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT conversation_id, path, date_unix FROM email_threads WHERE conversation_id=?1",
        )?;
        let mut rows = stmt.query_map([conversation_id], |row| {
            Ok(EmailThreadWinner {
                conversation_id: row.get(0)?,
                path: row.get(1)?,
                date_unix: row.get(2)?,
            })
        })?;
        if let Some(Ok(w)) = rows.next() {
            return Ok(Some(w));
        }
        Ok(None)
    }

    pub fn list_indexed_email_folder_names(&self) -> Result<Vec<String>, rusqlite::Error> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT DISTINCT folder_name FROM email_messages
             WHERE status='indexed' AND folder_name != ''
             ORDER BY folder_name COLLATE NOCASE",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.flatten().collect())
    }

    /// Indexed emails in `[after_unix, before_unix]` (inclusive). `from_substr` is a
    /// case-insensitive substring of Outlook `SenderName` (`from_addr`).
    pub fn list_indexed_email_paths_in_range(
        &self,
        after_unix: Option<i64>,
        before_unix: Option<i64>,
        from_substr: Option<&str>,
        folder_name: Option<&str>,
        limit: usize,
    ) -> Result<Vec<String>, rusqlite::Error> {
        let from = from_substr
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| format!("%{s}%"));
        let folder = folder_name
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT path FROM email_messages
             WHERE status='indexed' AND date_unix > 0
               AND (?1 IS NULL OR date_unix >= ?1)
               AND (?2 IS NULL OR date_unix <= ?2)
               AND (?3 IS NULL OR from_addr LIKE ?3 COLLATE NOCASE)
               AND (?4 IS NULL OR folder_name = ?4 COLLATE NOCASE)
             ORDER BY date_unix DESC
             LIMIT ?5",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![after_unix, before_unix, from, folder, limit as i64],
            |row| row.get(0),
        )?;
        Ok(rows.flatten().collect())
    }

    pub fn clear_all_email_messages(&self) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock();
        conn.execute("DELETE FROM email_messages", [])?;
        conn.execute("DELETE FROM email_threads", [])?;
        Ok(())
    }

    pub fn set_mail_last_sync_now(&self) -> Result<(), rusqlite::Error> {
        let now = chrono::Utc::now().to_rfc3339();
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO settings(key, value) VALUES('mail_last_sync_at', ?1)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            rusqlite::params![now],
        )?;
        Ok(())
    }

    pub fn count_indexed_emails(&self) -> Result<u32, rusqlite::Error> {
        let conn = self.conn.lock();
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM email_messages WHERE status='indexed'",
            [],
            |r| r.get(0),
        )?;
        Ok(n as u32)
    }

    // --- Notes ---

    pub fn get_active_note_id(&self) -> Option<String> {
        let conn = self.conn.lock();
        conn.query_row(
            "SELECT value FROM settings WHERE key=?1",
            [ACTIVE_NOTE_ID_KEY],
            |r| r.get::<_, String>(0),
        )
        .ok()
        .filter(|s| !s.is_empty())
    }

    pub fn set_active_note_id(&self, id: Option<&str>) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock();
        match id {
            Some(id) if !id.is_empty() => {
                conn.execute(
                    "INSERT INTO settings(key, value) VALUES(?1, ?2)
                     ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                    rusqlite::params![ACTIVE_NOTE_ID_KEY, id],
                )?;
            }
            _ => {
                conn.execute("DELETE FROM settings WHERE key=?1", [ACTIVE_NOTE_ID_KEY])?;
            }
        }
        Ok(())
    }

    pub fn list_notes(&self) -> Result<Vec<NoteRow>, rusqlite::Error> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id, title, memo, view_mode, sort_order, created_at, updated_at
             FROM notes ORDER BY sort_order ASC, updated_at DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(NoteRow {
                id: row.get(0)?,
                title: row.get(1)?,
                memo: row.get(2)?,
                view_mode: row.get(3)?,
                sort_order: row.get(4)?,
                created_at: row.get(5)?,
                updated_at: row.get(6)?,
            })
        })?;
        Ok(rows.flatten().collect())
    }

    pub fn search_note_ids(&self, query: &str) -> Result<Vec<String>, rusqlite::Error> {
        let needle = query.trim().to_lowercase();
        if needle.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT n.id FROM notes n
             WHERE instr(lower(n.title), ?1) > 0
                OR instr(lower(n.memo), ?1) > 0
                OR EXISTS (
                     SELECT 1 FROM note_items i
                     WHERE i.note_id = n.id
                       AND (
                         instr(lower(i.query), ?1) > 0
                         OR instr(lower(i.memo), ?1) > 0
                         OR instr(lower(i.item_json), ?1) > 0
                       )
                   )
             ORDER BY n.sort_order ASC, n.updated_at DESC",
        )?;
        let rows = stmt.query_map(rusqlite::params![needle], |row| row.get::<_, String>(0))?;
        Ok(rows.flatten().collect())
    }

    pub fn get_note(&self, id: &str) -> Result<Option<NoteRow>, rusqlite::Error> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id, title, memo, view_mode, sort_order, created_at, updated_at
             FROM notes WHERE id=?1",
        )?;
        let mut rows = stmt.query_map([id], |row| {
            Ok(NoteRow {
                id: row.get(0)?,
                title: row.get(1)?,
                memo: row.get(2)?,
                view_mode: row.get(3)?,
                sort_order: row.get(4)?,
                created_at: row.get(5)?,
                updated_at: row.get(6)?,
            })
        })?;
        match rows.next() {
            Some(Ok(n)) => Ok(Some(n)),
            Some(Err(e)) => Err(e),
            None => Ok(None),
        }
    }

    fn next_note_sort_order(conn: &rusqlite::Connection) -> Result<i64, rusqlite::Error> {
        let max: Option<i64> =
            conn.query_row("SELECT MAX(sort_order) FROM notes", [], |row| row.get(0))?;
        Ok(max.map(|m| m + 1).unwrap_or(0))
    }

    pub fn create_note(&self, title: &str) -> Result<NoteRow, rusqlite::Error> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().timestamp();
        let title = if title.trim().is_empty() {
            "無題のノート"
        } else {
            title.trim()
        };
        let conn = self.conn.lock();
        let sort_order = Self::next_note_sort_order(&conn)?;
        conn.execute(
            "INSERT INTO notes(id, title, memo, view_mode, sort_order, created_at, updated_at)
             VALUES(?1, ?2, '', 'list', ?3, ?4, ?4)",
            rusqlite::params![id, title, sort_order, now],
        )?;
        Ok(NoteRow {
            id,
            title: title.to_string(),
            memo: String::new(),
            view_mode: "list".into(),
            sort_order,
            created_at: now,
            updated_at: now,
        })
    }

    pub fn rename_note(&self, id: &str, title: &str) -> Result<Option<NoteRow>, rusqlite::Error> {
        let title = if title.trim().is_empty() {
            "無題のノート"
        } else {
            title.trim()
        };
        let now = chrono::Utc::now().timestamp();
        let conn = self.conn.lock();
        let n = conn.execute(
            "UPDATE notes SET title=?1, updated_at=?2 WHERE id=?3",
            rusqlite::params![title, now, id],
        )?;
        if n == 0 {
            return Ok(None);
        }
        drop(conn);
        self.get_note(id)
    }

    pub fn update_note_memo(
        &self,
        id: &str,
        memo: &str,
    ) -> Result<Option<NoteRow>, rusqlite::Error> {
        let now = chrono::Utc::now().timestamp();
        let conn = self.conn.lock();
        let n = conn.execute(
            "UPDATE notes SET memo=?1, updated_at=?2 WHERE id=?3",
            rusqlite::params![memo, now, id],
        )?;
        if n == 0 {
            return Ok(None);
        }
        drop(conn);
        self.get_note(id)
    }

    pub fn set_note_view_mode(
        &self,
        id: &str,
        view_mode: &str,
    ) -> Result<Option<NoteRow>, rusqlite::Error> {
        let view_mode = match view_mode {
            "grid" => "grid",
            _ => "list",
        };
        let now = chrono::Utc::now().timestamp();
        let conn = self.conn.lock();
        let n = conn.execute(
            "UPDATE notes SET view_mode=?1, updated_at=?2 WHERE id=?3",
            rusqlite::params![view_mode, now, id],
        )?;
        if n == 0 {
            return Ok(None);
        }
        drop(conn);
        self.get_note(id)
    }

    pub fn touch_note(&self, id: &str) -> Result<(), rusqlite::Error> {
        let now = chrono::Utc::now().timestamp();
        let conn = self.conn.lock();
        conn.execute(
            "UPDATE notes SET updated_at=?1 WHERE id=?2",
            rusqlite::params![now, id],
        )?;
        Ok(())
    }

    pub fn delete_note(&self, id: &str) -> Result<bool, rusqlite::Error> {
        let conn = self.conn.lock();
        let n = conn.execute("DELETE FROM notes WHERE id=?1", [id])?;
        Ok(n > 0)
    }

    pub fn list_note_items(&self, note_id: &str) -> Result<Vec<NoteItemRow>, rusqlite::Error> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id, note_id, sort_order, query, paragraph_id, item_json, memo, created_at
             FROM note_items WHERE note_id=?1 ORDER BY sort_order ASC, created_at ASC",
        )?;
        let rows = stmt.query_map([note_id], |row| {
            Ok(NoteItemRow {
                id: row.get(0)?,
                note_id: row.get(1)?,
                sort_order: row.get(2)?,
                query: row.get(3)?,
                paragraph_id: row.get(4)?,
                item_json: row.get(5)?,
                memo: row.get(6)?,
                created_at: row.get(7)?,
            })
        })?;
        Ok(rows.flatten().collect())
    }

    pub fn find_note_item_by_paragraph(
        &self,
        note_id: &str,
        paragraph_id: &str,
    ) -> Result<Option<NoteItemRow>, rusqlite::Error> {
        if paragraph_id.is_empty() {
            return Ok(None);
        }
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id, note_id, sort_order, query, paragraph_id, item_json, memo, created_at
             FROM note_items WHERE note_id=?1 AND paragraph_id=?2 LIMIT 1",
        )?;
        let mut rows = stmt.query_map(rusqlite::params![note_id, paragraph_id], |row| {
            Ok(NoteItemRow {
                id: row.get(0)?,
                note_id: row.get(1)?,
                sort_order: row.get(2)?,
                query: row.get(3)?,
                paragraph_id: row.get(4)?,
                item_json: row.get(5)?,
                memo: row.get(6)?,
                created_at: row.get(7)?,
            })
        })?;
        match rows.next() {
            Some(Ok(n)) => Ok(Some(n)),
            Some(Err(e)) => Err(e),
            None => Ok(None),
        }
    }

    pub fn next_note_item_sort_order(&self, note_id: &str) -> Result<i64, rusqlite::Error> {
        let conn = self.conn.lock();
        let max: Option<i64> = conn.query_row(
            "SELECT MAX(sort_order) FROM note_items WHERE note_id=?1",
            [note_id],
            |r| r.get(0),
        )?;
        Ok(max.unwrap_or(-1) + 1)
    }

    pub fn insert_note_item(
        &self,
        note_id: &str,
        query: &str,
        paragraph_id: &str,
        item_json: &str,
    ) -> Result<NoteItemRow, rusqlite::Error> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().timestamp();
        let sort_order = self.next_note_item_sort_order(note_id)?;
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO note_items(id, note_id, sort_order, query, paragraph_id, item_json, memo, created_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, '', ?7)",
            rusqlite::params![id, note_id, sort_order, query, paragraph_id, item_json, now],
        )?;
        drop(conn);
        self.touch_note(note_id)?;
        Ok(NoteItemRow {
            id,
            note_id: note_id.to_string(),
            sort_order,
            query: query.to_string(),
            paragraph_id: paragraph_id.to_string(),
            item_json: item_json.to_string(),
            memo: String::new(),
            created_at: now,
        })
    }

    pub fn update_note_item_memo(
        &self,
        id: &str,
        memo: &str,
    ) -> Result<Option<NoteItemRow>, rusqlite::Error> {
        let conn = self.conn.lock();
        let n = conn.execute(
            "UPDATE note_items SET memo=?1 WHERE id=?2",
            rusqlite::params![memo, id],
        )?;
        if n == 0 {
            return Ok(None);
        }
        let note_id: String =
            conn.query_row("SELECT note_id FROM note_items WHERE id=?1", [id], |r| {
                r.get(0)
            })?;
        drop(conn);
        self.touch_note(&note_id)?;
        self.get_note_item(id)
    }

    pub fn get_note_item(&self, id: &str) -> Result<Option<NoteItemRow>, rusqlite::Error> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id, note_id, sort_order, query, paragraph_id, item_json, memo, created_at
             FROM note_items WHERE id=?1",
        )?;
        let mut rows = stmt.query_map([id], |row| {
            Ok(NoteItemRow {
                id: row.get(0)?,
                note_id: row.get(1)?,
                sort_order: row.get(2)?,
                query: row.get(3)?,
                paragraph_id: row.get(4)?,
                item_json: row.get(5)?,
                memo: row.get(6)?,
                created_at: row.get(7)?,
            })
        })?;
        match rows.next() {
            Some(Ok(n)) => Ok(Some(n)),
            Some(Err(e)) => Err(e),
            None => Ok(None),
        }
    }

    pub fn remove_note_item(&self, id: &str) -> Result<bool, rusqlite::Error> {
        let conn = self.conn.lock();
        let note_id: Option<String> = conn
            .query_row("SELECT note_id FROM note_items WHERE id=?1", [id], |r| {
                r.get(0)
            })
            .ok();
        let n = conn.execute("DELETE FROM note_items WHERE id=?1", [id])?;
        drop(conn);
        if let Some(note_id) = note_id {
            self.touch_note(&note_id)?;
        }
        Ok(n > 0)
    }

    pub fn reorder_note_items(
        &self,
        note_id: &str,
        ordered_ids: &[String],
    ) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock();
        let tx = conn.unchecked_transaction()?;
        for (i, item_id) in ordered_ids.iter().enumerate() {
            tx.execute(
                "UPDATE note_items SET sort_order=?1 WHERE id=?2 AND note_id=?3",
                rusqlite::params![i as i64, item_id, note_id],
            )?;
        }
        let now = chrono::Utc::now().timestamp();
        tx.execute(
            "UPDATE notes SET updated_at=?1 WHERE id=?2",
            rusqlite::params![now, note_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn reorder_notes(&self, ordered_ids: &[String]) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock();
        let tx = conn.unchecked_transaction()?;
        for (i, note_id) in ordered_ids.iter().enumerate() {
            tx.execute(
                "UPDATE notes SET sort_order=?1 WHERE id=?2",
                rusqlite::params![i as i64, note_id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn list_llm_threads(&self) -> Result<Vec<LlmThreadRow>, rusqlite::Error> {
        let conn = self.conn.lock();
        let sql = format!(
            "SELECT {LLM_THREAD_COLS} FROM llm_threads ORDER BY sort_order ASC, updated_at DESC"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], map_llm_thread)?;
        Ok(rows.flatten().collect())
    }

    pub fn search_llm_thread_ids(&self, query: &str) -> Result<Vec<String>, rusqlite::Error> {
        let needle = query.trim().to_lowercase();
        if needle.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT t.id FROM llm_threads t
             WHERE instr(lower(t.title), ?1) > 0
                OR EXISTS (
                     SELECT 1 FROM llm_messages m
                     WHERE m.thread_id = t.id AND instr(lower(m.content), ?1) > 0
                   )
             ORDER BY t.sort_order ASC, t.updated_at DESC",
        )?;
        let rows = stmt.query_map(rusqlite::params![needle], |row| row.get::<_, String>(0))?;
        Ok(rows.flatten().collect())
    }

    fn next_llm_thread_sort_order(conn: &rusqlite::Connection) -> Result<i64, rusqlite::Error> {
        let min: Option<i64> =
            conn.query_row("SELECT MIN(sort_order) FROM llm_threads", [], |row| {
                row.get(0)
            })?;
        Ok(min.map(|m| m - 1).unwrap_or(0))
    }

    pub fn reorder_llm_threads(&self, ordered_ids: &[String]) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock();
        let tx = conn.unchecked_transaction()?;
        for (i, thread_id) in ordered_ids.iter().enumerate() {
            tx.execute(
                "UPDATE llm_threads SET sort_order=?1 WHERE id=?2",
                rusqlite::params![i as i64, thread_id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn get_llm_thread(&self, id: &str) -> Result<Option<LlmThreadRow>, rusqlite::Error> {
        let conn = self.conn.lock();
        let sql = format!("SELECT {LLM_THREAD_COLS} FROM llm_threads WHERE id=?1");
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query_map([id], map_llm_thread)?;
        match rows.next() {
            Some(Ok(n)) => Ok(Some(n)),
            Some(Err(e)) => Err(e),
            None => Ok(None),
        }
    }

    pub fn create_llm_thread(
        &self,
        title: &str,
        search_enabled: bool,
    ) -> Result<LlmThreadRow, rusqlite::Error> {
        let conn = self.conn.lock();
        let id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().timestamp();
        let enabled = if search_enabled { 1 } else { 0 };
        let sort_order = Self::next_llm_thread_sort_order(&conn)?;
        conn.execute(
            "INSERT INTO llm_threads(id, title, search_enabled, path_prefix, note_id, sort_order, created_at, updated_at)
             VALUES(?1, ?2, ?3, '', '', ?4, ?5, ?5)",
            rusqlite::params![id, title, enabled, sort_order, now],
        )?;
        Ok(LlmThreadRow {
            id,
            title: title.to_string(),
            search_enabled,
            path_prefix: String::new(),
            note_id: String::new(),
            sort_order,
            created_at: now,
            updated_at: now,
        })
    }

    pub fn rename_llm_thread(
        &self,
        id: &str,
        title: &str,
    ) -> Result<Option<LlmThreadRow>, rusqlite::Error> {
        let conn = self.conn.lock();
        let now = chrono::Utc::now().timestamp();
        let n = conn.execute(
            "UPDATE llm_threads SET title=?1, updated_at=?2 WHERE id=?3",
            rusqlite::params![title, now, id],
        )?;
        drop(conn);
        if n == 0 {
            Ok(None)
        } else {
            self.get_llm_thread(id)
        }
    }

    /// Folder scope for this thread's index searches. Empty means the whole index.
    /// Multiple folders are stored as newline-separated paths.
    pub fn set_llm_thread_scope(
        &self,
        id: &str,
        path_prefix: &str,
    ) -> Result<Option<LlmThreadRow>, rusqlite::Error> {
        let conn = self.conn.lock();
        let now = chrono::Utc::now().timestamp();
        let n = conn.execute(
            "UPDATE llm_threads SET path_prefix=?1, updated_at=?2 WHERE id=?3",
            rusqlite::params![path_prefix.trim(), now, id],
        )?;
        drop(conn);
        if n == 0 {
            Ok(None)
        } else {
            self.get_llm_thread(id)
        }
    }

    /// Bind this thread to a note for read/propose tools. Empty clears the binding.
    /// No FK: a deleted note is treated as unbound by callers.
    pub fn set_llm_thread_note(
        &self,
        id: &str,
        note_id: &str,
    ) -> Result<Option<LlmThreadRow>, rusqlite::Error> {
        let conn = self.conn.lock();
        let now = chrono::Utc::now().timestamp();
        let n = conn.execute(
            "UPDATE llm_threads SET note_id=?1, updated_at=?2 WHERE id=?3",
            rusqlite::params![note_id.trim(), now, id],
        )?;
        drop(conn);
        if n == 0 {
            Ok(None)
        } else {
            self.get_llm_thread(id)
        }
    }

    pub fn delete_llm_thread(&self, id: &str) -> Result<bool, rusqlite::Error> {
        let conn = self.conn.lock();
        let n = conn.execute("DELETE FROM llm_threads WHERE id=?1", [id])?;
        Ok(n > 0)
    }

    pub fn touch_llm_thread(&self, id: &str) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock();
        let now = chrono::Utc::now().timestamp();
        conn.execute(
            "UPDATE llm_threads SET updated_at=?1 WHERE id=?2",
            rusqlite::params![now, id],
        )?;
        Ok(())
    }

    pub fn list_llm_messages(
        &self,
        thread_id: &str,
    ) -> Result<Vec<LlmMessageRow>, rusqlite::Error> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id, thread_id, role, content, created_at
             FROM llm_messages WHERE thread_id=?1 ORDER BY created_at ASC, id ASC",
        )?;
        let rows = stmt.query_map([thread_id], |row| {
            Ok(LlmMessageRow {
                id: row.get(0)?,
                thread_id: row.get(1)?,
                role: row.get(2)?,
                content: row.get(3)?,
                created_at: row.get(4)?,
            })
        })?;
        Ok(rows.flatten().collect())
    }

    pub fn insert_llm_message(
        &self,
        thread_id: &str,
        role: &str,
        content: &str,
    ) -> Result<LlmMessageRow, rusqlite::Error> {
        let conn = self.conn.lock();
        let id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().timestamp();
        conn.execute(
            "INSERT INTO llm_messages(id, thread_id, role, content, created_at)
             VALUES(?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![id, thread_id, role, content, now],
        )?;
        let now2 = now;
        conn.execute(
            "UPDATE llm_threads SET updated_at=?1 WHERE id=?2",
            rusqlite::params![now2, thread_id],
        )?;
        Ok(LlmMessageRow {
            id,
            thread_id: thread_id.to_string(),
            role: role.to_string(),
            content: content.to_string(),
            created_at: now,
        })
    }

    pub fn list_llm_sources(&self, thread_id: &str) -> Result<Vec<LlmSourceRow>, rusqlite::Error> {
        let conn = self.conn.lock();
        let sql = format!(
            "SELECT {LLM_SOURCE_COLS}
             FROM llm_thread_sources WHERE thread_id=?1 ORDER BY sort_order ASC, created_at ASC"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([thread_id], map_llm_source)?;
        Ok(rows.flatten().collect())
    }

    pub fn get_llm_source(&self, id: &str) -> Result<Option<LlmSourceRow>, rusqlite::Error> {
        let conn = self.conn.lock();
        let sql = format!("SELECT {LLM_SOURCE_COLS} FROM llm_thread_sources WHERE id=?1");
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query_map([id], map_llm_source)?;
        match rows.next() {
            Some(Ok(n)) => Ok(Some(n)),
            Some(Err(e)) => Err(e),
            None => Ok(None),
        }
    }

    pub fn insert_llm_source(
        &self,
        thread_id: &str,
        origin: &str,
        path: &str,
        title: &str,
        paragraph_id: &str,
        body: &str,
        query: &str,
    ) -> Result<(LlmSourceRow, bool), rusqlite::Error> {
        let conn = self.conn.lock();
        let path = path.trim();
        let paragraph_id = paragraph_id.trim();
        let origin = if origin.trim().is_empty() {
            "attach"
        } else {
            origin.trim()
        };
        let title = title.trim();
        let query = query.trim();
        let mut find = conn.prepare(
            "SELECT id FROM llm_thread_sources
             WHERE thread_id=?1 AND path=?2 AND paragraph_id=?3
               AND injected_user_message_id = ''
             LIMIT 1",
        )?;
        let pending_id: Option<String> = find
            .query_map(rusqlite::params![thread_id, path, paragraph_id], |row| {
                row.get(0)
            })?
            .flatten()
            .next();
        drop(find);
        if let Some(id) = pending_id {
            let sql = format!("SELECT {LLM_SOURCE_COLS} FROM llm_thread_sources WHERE id=?1");
            let existing = conn.query_row(&sql, [&id], map_llm_source)?;
            if existing.body.trim() == body.trim()
                && existing.title == title
                && existing.query == query
            {
                return Ok((existing, false));
            }
            let now = chrono::Utc::now().timestamp();
            conn.execute(
                "UPDATE llm_thread_sources SET body=?1, title=?2, query=?3 WHERE id=?4",
                rusqlite::params![body, title, query, id],
            )?;
            conn.execute(
                "UPDATE llm_threads SET updated_at=?1 WHERE id=?2",
                rusqlite::params![now, thread_id],
            )?;
            let sql = format!("SELECT {LLM_SOURCE_COLS} FROM llm_thread_sources WHERE id=?1");
            let row = conn.query_row(&sql, [id], map_llm_source)?;
            return Ok((row, true));
        }
        let next_order: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(sort_order), -1) + 1 FROM llm_thread_sources WHERE thread_id=?1",
                [thread_id],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().timestamp();
        conn.execute(
            "INSERT INTO llm_thread_sources(
                id, thread_id, sort_order, origin, path, title, paragraph_id, body, query,
                created_at, grain, unit_body, injected_user_message_id, cited_assistant_message_id, cite_no
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'unit', '', '', '', 0)",
            rusqlite::params![
                id,
                thread_id,
                next_order,
                origin,
                path,
                title,
                paragraph_id,
                body,
                query,
                now
            ],
        )?;
        conn.execute(
            "UPDATE llm_threads SET updated_at=?1 WHERE id=?2",
            rusqlite::params![now, thread_id],
        )?;
        Ok((
            LlmSourceRow {
                id,
                thread_id: thread_id.to_string(),
                sort_order: next_order,
                origin: origin.to_string(),
                path: path.to_string(),
                title: title.to_string(),
                paragraph_id: paragraph_id.to_string(),
                body: body.to_string(),
                query: query.to_string(),
                created_at: now,
                grain: "unit".into(),
                unit_body: String::new(),
                injected_user_message_id: String::new(),
                cited_assistant_message_id: String::new(),
                cite_no: 0,
                kind: "text".into(),
                stored_relpath: String::new(),
                ocr_status: String::new(),
            },
            true,
        ))
    }

    pub fn insert_llm_source_full(
        &self,
        thread_id: &str,
        origin: &str,
        path: &str,
        title: &str,
        paragraph_id: &str,
        body: &str,
        query: &str,
        grain: &str,
        kind: &str,
        stored_relpath: &str,
        ocr_status: &str,
        id: Option<&str>,
    ) -> Result<(LlmSourceRow, bool), rusqlite::Error> {
        let conn = self.conn.lock();
        let path = path.trim();
        let paragraph_id = paragraph_id.trim();
        let origin = if origin.trim().is_empty() {
            "attach"
        } else {
            origin.trim()
        };
        let title = title.trim();
        let query = query.trim();
        let grain = if grain.trim().eq_ignore_ascii_case("file") {
            "file"
        } else {
            "unit"
        };
        let kind = match kind.trim().to_ascii_lowercase().as_str() {
            "image" => "image",
            "web" => "web",
            _ => "text",
        };
        let stored_relpath = stored_relpath.trim();
        let ocr_status = ocr_status.trim();
        let mut find = conn.prepare(
            "SELECT id FROM llm_thread_sources
             WHERE thread_id=?1 AND lower(path)=lower(?2) AND paragraph_id=?3
               AND injected_user_message_id = ''
             LIMIT 1",
        )?;
        let pending_id: Option<String> = find
            .query_map(rusqlite::params![thread_id, path, paragraph_id], |row| {
                row.get(0)
            })?
            .flatten()
            .next();
        drop(find);
        if let Some(existing_id) = pending_id {
            let sql = format!("SELECT {LLM_SOURCE_COLS} FROM llm_thread_sources WHERE id=?1");
            let existing = conn.query_row(&sql, [&existing_id], map_llm_source)?;
            if existing.body.trim() == body.trim()
                && existing.title == title
                && existing.query == query
                && existing.grain == grain
                && existing.kind == kind
                && existing.ocr_status == ocr_status
                && existing.stored_relpath == stored_relpath
            {
                return Ok((existing, false));
            }
            let now = chrono::Utc::now().timestamp();
            conn.execute(
                "UPDATE llm_thread_sources
                 SET body=?1, title=?2, query=?3, grain=?4, kind=?5,
                     stored_relpath=?6, ocr_status=?7
                 WHERE id=?8",
                rusqlite::params![
                    body,
                    title,
                    query,
                    grain,
                    kind,
                    stored_relpath,
                    ocr_status,
                    existing_id
                ],
            )?;
            conn.execute(
                "UPDATE llm_threads SET updated_at=?1 WHERE id=?2",
                rusqlite::params![now, thread_id],
            )?;
            let sql = format!("SELECT {LLM_SOURCE_COLS} FROM llm_thread_sources WHERE id=?1");
            let row = conn.query_row(&sql, [existing_id], map_llm_source)?;
            return Ok((row, true));
        }
        let next_order: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(sort_order), -1) + 1 FROM llm_thread_sources WHERE thread_id=?1",
                [thread_id],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let id = id
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let now = chrono::Utc::now().timestamp();
        conn.execute(
            "INSERT INTO llm_thread_sources(
                id, thread_id, sort_order, origin, path, title, paragraph_id, body, query,
                created_at, grain, unit_body, injected_user_message_id, cited_assistant_message_id,
                cite_no, kind, stored_relpath, ocr_status
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, '', '', '', 0, ?12, ?13, ?14)",
            rusqlite::params![
                id,
                thread_id,
                next_order,
                origin,
                path,
                title,
                paragraph_id,
                body,
                query,
                now,
                grain,
                kind,
                stored_relpath,
                ocr_status
            ],
        )?;
        conn.execute(
            "UPDATE llm_threads SET updated_at=?1 WHERE id=?2",
            rusqlite::params![now, thread_id],
        )?;
        Ok((
            LlmSourceRow {
                id,
                thread_id: thread_id.to_string(),
                sort_order: next_order,
                origin: origin.to_string(),
                path: path.to_string(),
                title: title.to_string(),
                paragraph_id: paragraph_id.to_string(),
                body: body.to_string(),
                query: query.to_string(),
                created_at: now,
                grain: grain.into(),
                unit_body: String::new(),
                injected_user_message_id: String::new(),
                cited_assistant_message_id: String::new(),
                cite_no: 0,
                kind: kind.into(),
                stored_relpath: stored_relpath.to_string(),
                ocr_status: ocr_status.to_string(),
            },
            true,
        ))
    }

    pub fn find_pending_llm_source(
        &self,
        thread_id: &str,
        path: &str,
        paragraph_id: &str,
    ) -> Result<Option<LlmSourceRow>, rusqlite::Error> {
        let conn = self.conn.lock();
        let sql = format!(
            "SELECT {LLM_SOURCE_COLS} FROM llm_thread_sources
             WHERE thread_id=?1 AND lower(path)=lower(?2) AND paragraph_id=?3
               AND injected_user_message_id = ''
             LIMIT 1"
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query_map(
            rusqlite::params![thread_id, path.trim(), paragraph_id.trim()],
            map_llm_source,
        )?;
        match rows.next() {
            Some(Ok(n)) => Ok(Some(n)),
            Some(Err(e)) => Err(e),
            None => Ok(None),
        }
    }

    pub fn find_any_pending_llm_source_by_path(
        &self,
        thread_id: &str,
        path: &str,
    ) -> Result<Option<LlmSourceRow>, rusqlite::Error> {
        let conn = self.conn.lock();
        let sql = format!(
            "SELECT {LLM_SOURCE_COLS} FROM llm_thread_sources
             WHERE thread_id=?1 AND lower(path)=lower(?2)
               AND injected_user_message_id = ''
             ORDER BY sort_order ASC LIMIT 1"
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query_map(rusqlite::params![thread_id, path.trim()], map_llm_source)?;
        match rows.next() {
            Some(Ok(n)) => Ok(Some(n)),
            Some(Err(e)) => Err(e),
            None => Ok(None),
        }
    }

    pub fn next_pending_ocr_source(&self) -> Result<Option<LlmSourceRow>, rusqlite::Error> {
        let conn = self.conn.lock();
        let sql = format!(
            "SELECT {LLM_SOURCE_COLS} FROM llm_thread_sources
             WHERE kind='image' AND ocr_status='pending'
             ORDER BY created_at ASC, sort_order ASC LIMIT 1"
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query_map([], map_llm_source)?;
        match rows.next() {
            Some(Ok(n)) => Ok(Some(n)),
            Some(Err(e)) => Err(e),
            None => Ok(None),
        }
    }

    pub fn next_pending_ocr_in_thread(
        &self,
        thread_id: &str,
    ) -> Result<Option<LlmSourceRow>, rusqlite::Error> {
        let conn = self.conn.lock();
        let sql = format!(
            "SELECT {LLM_SOURCE_COLS} FROM llm_thread_sources
             WHERE thread_id=?1 AND kind='image' AND ocr_status='pending'
             ORDER BY created_at ASC, sort_order ASC LIMIT 1"
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query_map([thread_id], map_llm_source)?;
        match rows.next() {
            Some(Ok(n)) => Ok(Some(n)),
            Some(Err(e)) => Err(e),
            None => Ok(None),
        }
    }

    pub fn fail_pending_ocr(&self, thread_id: &str) -> Result<usize, rusqlite::Error> {
        let conn = self.conn.lock();
        let n = conn.execute(
            "UPDATE llm_thread_sources SET ocr_status='error'
             WHERE thread_id=?1 AND kind='image' AND ocr_status='pending'",
            [thread_id],
        )?;
        Ok(n)
    }

    pub fn update_llm_source_ocr(
        &self,
        id: &str,
        body: &str,
        ocr_status: &str,
    ) -> Result<Option<LlmSourceRow>, rusqlite::Error> {
        let conn = self.conn.lock();
        let now = chrono::Utc::now().timestamp();
        let n = conn.execute(
            "UPDATE llm_thread_sources SET body=?1, ocr_status=?2 WHERE id=?3",
            rusqlite::params![body, ocr_status.trim(), id],
        )?;
        if n == 0 {
            return Ok(None);
        }
        let thread_id: String = conn.query_row(
            "SELECT thread_id FROM llm_thread_sources WHERE id=?1",
            [id],
            |r| r.get(0),
        )?;
        conn.execute(
            "UPDATE llm_threads SET updated_at=?1 WHERE id=?2",
            rusqlite::params![now, thread_id],
        )?;
        drop(conn);
        self.get_llm_source(id)
    }

    pub fn max_llm_cite_no(&self, thread_id: &str) -> Result<i64, rusqlite::Error> {
        let conn = self.conn.lock();
        conn.query_row(
            "SELECT COALESCE(MAX(cite_no), 0) FROM llm_thread_sources WHERE thread_id=?1",
            [thread_id],
            |r| r.get(0),
        )
    }

    pub fn find_cited_llm_source(
        &self,
        thread_id: &str,
        path: &str,
        paragraph_id: &str,
    ) -> Result<Option<LlmSourceRow>, rusqlite::Error> {
        let conn = self.conn.lock();
        let sql = format!(
            "SELECT {LLM_SOURCE_COLS} FROM llm_thread_sources
             WHERE thread_id=?1 AND path=?2 AND paragraph_id=?3
               AND injected_user_message_id != ''
             ORDER BY cite_no ASC, created_at ASC LIMIT 1"
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query_map(
            rusqlite::params![thread_id, path.trim(), paragraph_id.trim()],
            map_llm_source,
        )?;
        match rows.next() {
            Some(Ok(n)) => Ok(Some(n)),
            Some(Err(e)) => Err(e),
            None => Ok(None),
        }
    }

    /// Any web/file source with this path (pending first, then cited). Case-insensitive.
    pub fn find_llm_source_by_path(
        &self,
        thread_id: &str,
        path: &str,
    ) -> Result<Option<LlmSourceRow>, rusqlite::Error> {
        let conn = self.conn.lock();
        let sql = format!(
            "SELECT {LLM_SOURCE_COLS} FROM llm_thread_sources
             WHERE thread_id=?1 AND lower(path)=lower(?2) AND paragraph_id=''
             ORDER BY CASE WHEN injected_user_message_id = '' THEN 0 ELSE 1 END,
                      created_at DESC
             LIMIT 1"
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query_map(rusqlite::params![thread_id, path.trim()], map_llm_source)?;
        match rows.next() {
            Some(Ok(n)) => Ok(Some(n)),
            Some(Err(e)) => Err(e),
            None => Ok(None),
        }
    }

    pub fn consume_llm_sources(
        &self,
        items: &[(String, i64)],
        user_message_id: &str,
        assistant_message_id: &str,
    ) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock();
        for (id, cite_no) in items {
            conn.execute(
                "UPDATE llm_thread_sources
                 SET injected_user_message_id=?1, cited_assistant_message_id=?2, cite_no=?3
                 WHERE id=?4",
                rusqlite::params![user_message_id, assistant_message_id, cite_no, id],
            )?;
        }
        Ok(())
    }

    pub fn delete_uncited_tool_sources(&self, thread_id: &str) -> Result<usize, rusqlite::Error> {
        let conn = self.conn.lock();
        let n = conn.execute(
            "DELETE FROM llm_thread_sources
             WHERE thread_id=?1 AND origin='tool' AND injected_user_message_id=''",
            [thread_id],
        )?;
        Ok(n)
    }

    pub fn set_llm_source_cite_no(&self, id: &str, cite_no: i64) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock();
        conn.execute(
            "UPDATE llm_thread_sources SET cite_no=?1 WHERE id=?2",
            rusqlite::params![cite_no, id],
        )?;
        Ok(())
    }

    /// Image rows in this thread that share `path` (case-insensitive). Empty path → none.
    pub fn list_llm_image_group(
        &self,
        thread_id: &str,
        path: &str,
    ) -> Result<Vec<LlmSourceRow>, rusqlite::Error> {
        let want = crate::pathutil::simplify_windows_path(path.trim());
        if want.is_empty() {
            return Ok(Vec::new());
        }
        let mut rows: Vec<LlmSourceRow> = self
            .list_llm_sources(thread_id)?
            .into_iter()
            .filter(|s| {
                s.is_image()
                    && crate::pathutil::simplify_windows_path(s.path.trim())
                        .eq_ignore_ascii_case(&want)
            })
            .collect();
        rows.sort_by(|a, b| {
            fn page(s: &LlmSourceRow) -> Option<u32> {
                s.paragraph_id
                    .trim()
                    .strip_prefix("pdf-page:")
                    .and_then(|r| r.parse().ok())
            }
            match (page(a), page(b)) {
                (Some(x), Some(y)) => x.cmp(&y).then(a.sort_order.cmp(&b.sort_order)),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => a
                    .sort_order
                    .cmp(&b.sort_order)
                    .then(a.created_at.cmp(&b.created_at)),
            }
        });
        Ok(rows)
    }

    /// Write `cite_no` onto siblings that still have none, so the next send does not mint a new [n].
    pub fn apply_image_group_cite(
        &self,
        thread_id: &str,
        path: &str,
        cite_no: i64,
    ) -> Result<usize, rusqlite::Error> {
        if cite_no <= 0 {
            return Ok(0);
        }
        let rows = self.list_llm_image_group(thread_id, path)?;
        let conn = self.conn.lock();
        let mut n = 0usize;
        for row in rows {
            if row.cite_no > 0 {
                continue;
            }
            conn.execute(
                "UPDATE llm_thread_sources SET cite_no=?1 WHERE id=?2 AND cite_no=0",
                rusqlite::params![cite_no, row.id],
            )?;
            n += 1;
        }
        Ok(n)
    }

    /// After OCR finishes, copy cite / injected ids from a sibling so the page stays on the same turn.
    pub fn inherit_image_group_meta(&self, id: &str) -> Result<(), rusqlite::Error> {
        let Some(row) = self.get_llm_source(id)? else {
            return Ok(());
        };
        if !row.is_image() {
            return Ok(());
        }
        let want = crate::pathutil::simplify_windows_path(row.path.trim());
        if want.is_empty() {
            return Ok(());
        }
        let siblings = self.list_llm_image_group(&row.thread_id, &row.path)?;
        let donor = siblings
            .iter()
            .filter(|s| s.id != row.id)
            .find(|s| s.cite_no > 0 || !s.injected_user_message_id.trim().is_empty());
        let Some(d) = donor else {
            return Ok(());
        };
        let cite = if row.cite_no > 0 {
            row.cite_no
        } else {
            d.cite_no
        };
        let injected = if row.injected_user_message_id.trim().is_empty() {
            d.injected_user_message_id.as_str()
        } else {
            row.injected_user_message_id.as_str()
        };
        let cited = if row.cited_assistant_message_id.trim().is_empty() {
            d.cited_assistant_message_id.as_str()
        } else {
            row.cited_assistant_message_id.as_str()
        };
        let conn = self.conn.lock();
        conn.execute(
            "UPDATE llm_thread_sources
             SET cite_no=?1, injected_user_message_id=?2, cited_assistant_message_id=?3
             WHERE id=?4",
            rusqlite::params![cite, injected, cited, id],
        )?;
        Ok(())
    }

    pub fn update_llm_source_grain(
        &self,
        id: &str,
        grain: &str,
        body: &str,
        unit_body: Option<&str>,
    ) -> Result<Option<LlmSourceRow>, rusqlite::Error> {
        let grain = if grain.trim().eq_ignore_ascii_case("file") {
            "file"
        } else {
            "unit"
        };
        let conn = self.conn.lock();
        let now = chrono::Utc::now().timestamp();
        let n = if let Some(saved) = unit_body {
            conn.execute(
                "UPDATE llm_thread_sources SET grain=?1, body=?2, unit_body=?3 WHERE id=?4",
                rusqlite::params![grain, body, saved, id],
            )?
        } else {
            conn.execute(
                "UPDATE llm_thread_sources SET grain=?1, body=?2 WHERE id=?3",
                rusqlite::params![grain, body, id],
            )?
        };
        if n == 0 {
            return Ok(None);
        }
        let thread_id: String = conn.query_row(
            "SELECT thread_id FROM llm_thread_sources WHERE id=?1",
            [id],
            |r| r.get(0),
        )?;
        conn.execute(
            "UPDATE llm_threads SET updated_at=?1 WHERE id=?2",
            rusqlite::params![now, thread_id],
        )?;
        drop(conn);
        self.get_llm_source(id)
    }

    pub fn delete_other_llm_sources_for_path(
        &self,
        thread_id: &str,
        keep_id: &str,
        path: &str,
    ) -> Result<usize, rusqlite::Error> {
        let path = path.trim();
        if path.is_empty() {
            return Ok(0);
        }
        let rows = self.list_llm_sources(thread_id)?;
        let mut n = 0usize;
        for row in rows {
            if row.id == keep_id {
                continue;
            }
            if !row.is_pending() {
                continue;
            }
            if !row.path.eq_ignore_ascii_case(path) {
                continue;
            }
            if self.delete_llm_source(&row.id)?.is_some() {
                n += 1;
            }
        }
        Ok(n)
    }

    pub fn delete_llm_source(&self, id: &str) -> Result<Option<String>, rusqlite::Error> {
        let conn = self.conn.lock();
        let thread_id = match conn.query_row(
            "SELECT thread_id FROM llm_thread_sources WHERE id=?1",
            [id],
            |r| r.get::<_, String>(0),
        ) {
            Ok(tid) => tid,
            Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
            Err(e) => return Err(e),
        };
        conn.execute("DELETE FROM llm_thread_sources WHERE id=?1", [id])?;
        Ok(Some(thread_id))
    }

    pub fn get_active_llm_thread_id(&self) -> Option<String> {
        let conn = self.conn.lock();
        conn.query_row(
            "SELECT value FROM settings WHERE key=?1",
            [ACTIVE_LLM_THREAD_ID_KEY],
            |r| r.get::<_, String>(0),
        )
        .ok()
        .filter(|s| !s.is_empty())
    }

    pub fn set_active_llm_thread_id(&self, id: Option<&str>) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock();
        match id {
            Some(id) if !id.is_empty() => {
                conn.execute(
                    "INSERT INTO settings(key, value) VALUES(?1, ?2)
                     ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                    rusqlite::params![ACTIVE_LLM_THREAD_ID_KEY, id],
                )?;
            }
            _ => {
                conn.execute(
                    "DELETE FROM settings WHERE key=?1",
                    [ACTIVE_LLM_THREAD_ID_KEY],
                )?;
            }
        }
        Ok(())
    }

    pub fn insert_note_proposal(
        &self,
        thread_id: &str,
        note_id: &str,
        request_id: &str,
        kind: &str,
        heading: &str,
        old_text: &str,
        new_text: &str,
        chunk: &str,
        note_updated_at: i64,
    ) -> Result<LlmNoteProposalRow, rusqlite::Error> {
        let heading = notes_md::normalize_heading(heading);
        let id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().timestamp();
        let conn = self.conn.lock();
        conn.execute(
            "UPDATE llm_note_proposals SET status='superseded'
             WHERE note_id=?1 AND heading=?2 AND status='pending'",
            rusqlite::params![note_id, heading],
        )?;
        conn.execute(
            "INSERT INTO llm_note_proposals(
               id, thread_id, note_id, request_id, assistant_message_id, kind, heading,
               old_text, new_text, chunk, note_updated_at, status, created_at, applied_at
             ) VALUES(?1, ?2, ?3, ?4, '', ?5, ?6, ?7, ?8, ?9, ?10, 'pending', ?11, NULL)",
            rusqlite::params![
                id,
                thread_id,
                note_id,
                request_id,
                kind,
                heading,
                old_text,
                new_text,
                chunk,
                note_updated_at,
                now
            ],
        )?;
        drop(conn);
        self.get_note_proposal(&id)?
            .ok_or(rusqlite::Error::QueryReturnedNoRows)
    }

    fn proposal_select_sql(where_clause: &str) -> String {
        format!(
            "SELECT p.id, p.thread_id, p.note_id, p.request_id, p.assistant_message_id,
                    p.kind, p.heading, p.old_text, p.new_text, p.chunk, p.note_updated_at,
                    p.status, p.created_at, p.applied_at, COALESCE(n.title, '')
             FROM llm_note_proposals p
             LEFT JOIN notes n ON n.id = p.note_id
             {where_clause}"
        )
    }

    fn map_note_proposal(row: &rusqlite::Row<'_>) -> rusqlite::Result<LlmNoteProposalRow> {
        let kind: String = row.get(5)?;
        let old_text: String = row.get(7)?;
        let new_text: String = row.get(8)?;
        let chunk: String = row.get(9)?;
        let diff = proposal_line_diff(&kind, &old_text, &new_text, &chunk);
        Ok(LlmNoteProposalRow {
            id: row.get(0)?,
            thread_id: row.get(1)?,
            note_id: row.get(2)?,
            request_id: row.get(3)?,
            assistant_message_id: row.get(4)?,
            kind,
            heading: row.get(6)?,
            old_text,
            new_text,
            chunk,
            note_updated_at: row.get(10)?,
            status: row.get(11)?,
            created_at: row.get(12)?,
            applied_at: row.get(13)?,
            note_title: row.get(14)?,
            diff,
        })
    }

    pub fn get_note_proposal(
        &self,
        id: &str,
    ) -> Result<Option<LlmNoteProposalRow>, rusqlite::Error> {
        let conn = self.conn.lock();
        let sql = Self::proposal_select_sql("WHERE p.id=?1");
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query_map([id], Self::map_note_proposal)?;
        match rows.next() {
            Some(Ok(n)) => Ok(Some(n)),
            Some(Err(e)) => Err(e),
            None => Ok(None),
        }
    }

    pub fn list_note_proposals_for_thread(
        &self,
        thread_id: &str,
    ) -> Result<Vec<LlmNoteProposalRow>, rusqlite::Error> {
        let conn = self.conn.lock();
        let sql = Self::proposal_select_sql(
            "WHERE p.thread_id=?1
             AND (p.assistant_message_id != '' OR p.status='pending')
             ORDER BY p.created_at ASC",
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([thread_id], Self::map_note_proposal)?;
        Ok(rows.flatten().collect())
    }

    pub fn list_orphan_note_proposals(
        &self,
        thread_id: &str,
        request_id: &str,
    ) -> Result<Vec<LlmNoteProposalRow>, rusqlite::Error> {
        let conn = self.conn.lock();
        let sql = Self::proposal_select_sql(
            "WHERE p.thread_id=?1 AND p.request_id=?2 AND p.assistant_message_id = ''
             ORDER BY p.created_at ASC",
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params![thread_id, request_id], Self::map_note_proposal)?;
        Ok(rows.flatten().collect())
    }

    pub fn list_note_proposals_for_note(
        &self,
        note_id: &str,
    ) -> Result<Vec<LlmNoteProposalRow>, rusqlite::Error> {
        let conn = self.conn.lock();
        let sql = Self::proposal_select_sql(
            "WHERE p.note_id=?1 ORDER BY p.created_at DESC",
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([note_id], Self::map_note_proposal)?;
        Ok(rows.flatten().collect())
    }

    pub fn attach_note_proposals_to_assistant(
        &self,
        request_id: &str,
        assistant_message_id: &str,
    ) -> Result<usize, rusqlite::Error> {
        let conn = self.conn.lock();
        let n = conn.execute(
            "UPDATE llm_note_proposals SET assistant_message_id=?1
             WHERE request_id=?2 AND assistant_message_id = '' AND status='pending'",
            rusqlite::params![assistant_message_id, request_id],
        )?;
        Ok(n)
    }

    pub fn discard_orphan_note_proposals(
        &self,
        request_id: &str,
    ) -> Result<usize, rusqlite::Error> {
        let conn = self.conn.lock();
        let n = conn.execute(
            "DELETE FROM llm_note_proposals
             WHERE request_id=?1 AND assistant_message_id = ''",
            [request_id],
        )?;
        Ok(n)
    }

    pub fn dismiss_note_proposal(&self, id: &str) -> Result<Option<LlmNoteProposalRow>, String> {
        let row = self
            .get_note_proposal(id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "提案が見つかりません".to_string())?;
        if row.status != "pending" {
            return Err("この提案は操作できません。".into());
        }
        let conn = self.conn.lock();
        conn.execute(
            "UPDATE llm_note_proposals SET status='dismissed' WHERE id=?1 AND status='pending'",
            [id],
        )
        .map_err(|e| e.to_string())?;
        drop(conn);
        self.get_note_proposal(id)
            .map_err(|e| e.to_string())
    }

    pub fn apply_note_proposal(&self, id: &str) -> Result<NoteRow, String> {
        let row = self
            .get_note_proposal(id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "提案が見つかりません".to_string())?;
        if row.status != "pending" {
            return Err("この提案は採用できません。".into());
        }
        let note = self
            .get_note(&row.note_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "ノートが見つかりません".to_string())?;
        let next = match apply_proposal_to_memo(&note.memo, &row) {
            Ok(s) => s,
            Err(stale) => {
                let conn = self.conn.lock();
                let _ = conn.execute(
                    "UPDATE llm_note_proposals SET status=?1 WHERE id=?2 AND status='pending'",
                    rusqlite::params![stale, id],
                );
                return Err(if stale == "stale" {
                    "メモが変わっています。却下して再度依頼してください。".into()
                } else {
                    "この提案は採用できません。".into()
                });
            }
        };
        self.update_note_memo(&note.id, &next)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "ノートが見つかりません".to_string())?;
        let now = chrono::Utc::now().timestamp();
        let conn = self.conn.lock();
        conn.execute(
            "UPDATE llm_note_proposals SET status='applied', applied_at=?1
             WHERE id=?2 AND status='pending'",
            rusqlite::params![now, id],
        )
        .map_err(|e| e.to_string())?;
        drop(conn);
        self.get_note(&note.id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "ノートが見つかりません".to_string())
    }

    pub fn undo_note_proposal(&self, id: &str) -> Result<NoteRow, String> {
        let row = self
            .get_note_proposal(id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "提案が見つかりません".to_string())?;
        if row.status != "applied" {
            return Err("採用済みの提案だけ取り消せます。".into());
        }
        let note = self
            .get_note(&row.note_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "ノートが見つかりません".to_string())?;
        let next = undo_proposal_on_memo(&note.memo, &row).ok_or_else(|| {
            "メモが人がさらに変えたため取り消せません。".to_string()
        })?;
        self.update_note_memo(&note.id, &next)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "ノートが見つかりません".to_string())?;
        let conn = self.conn.lock();
        conn.execute(
            "UPDATE llm_note_proposals SET status='dismissed', applied_at=NULL WHERE id=?1",
            [id],
        )
        .map_err(|e| e.to_string())?;
        drop(conn);
        self.get_note(&note.id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "ノートが見つかりません".to_string())
    }
}

fn proposal_line_diff(
    kind: &str,
    old_text: &str,
    new_text: &str,
    chunk: &str,
) -> Vec<notes_md::DiffLine> {
    if kind == "append" {
        notes_md::line_diff("", chunk)
    } else {
        notes_md::line_diff(old_text, new_text)
    }
}

fn apply_proposal_to_memo(memo: &str, row: &LlmNoteProposalRow) -> Result<String, &'static str> {
    match row.kind.as_str() {
        "replace" => {
            let secs = notes_md::split_sections(memo);
            match notes_md::find_section(&secs, &row.heading) {
                Ok(s) if s.text == row.old_text => notes_md::replace_section(
                    memo,
                    &row.heading,
                    &row.new_text,
                )
                .map_err(|_| "stale"),
                _ => Err("stale"),
            }
        }
        "insert" => {
            let secs = notes_md::split_sections(memo);
            match notes_md::find_section(&secs, &row.heading) {
                Err(notes_md::SectionError::Missing) => Ok(notes_md::insert_section(
                    memo,
                    &row.heading,
                    insert_body_from_new_text(&row.heading, &row.new_text),
                )),
                _ => Err("stale"),
            }
        }
        "append" => {
            if row.heading.is_empty() {
                if memo != row.old_text {
                    return Err("stale");
                }
                notes_md::append_chunk(memo, None, &row.chunk).map_err(|_| "stale")
            } else {
                let secs = notes_md::split_sections(memo);
                match notes_md::find_section(&secs, &row.heading) {
                    Ok(s) if s.text == row.old_text => notes_md::append_chunk(
                        memo,
                        Some(row.heading.as_str()),
                        &row.chunk,
                    )
                    .map_err(|_| "stale"),
                    _ => Err("stale"),
                }
            }
        }
        _ => Err("bad"),
    }
}

fn insert_body_from_new_text<'a>(heading: &str, new_text: &'a str) -> &'a str {
    let want = notes_md::normalize_heading(heading);
    let mut rest = new_text;
    if let Some(first) = rest.lines().next() {
        if let Some((_, title)) = notes_md::parse_atx_heading(first) {
            if notes_md::normalize_heading(&title) == want {
                rest = rest[first.len()..].trim_start_matches(['\r', '\n']);
                return rest;
            }
        }
    }
    new_text
}

fn undo_proposal_on_memo(memo: &str, row: &LlmNoteProposalRow) -> Option<String> {
    match row.kind.as_str() {
        "replace" => {
            let secs = notes_md::split_sections(memo);
            let s = notes_md::find_section(&secs, &row.heading).ok()?;
            if s.text != row.new_text {
                return None;
            }
            notes_md::replace_section(memo, &row.heading, &row.old_text).ok()
        }
        "insert" => {
            let piece = if memo.ends_with(&row.new_text) {
                row.new_text.clone()
            } else {
                let formatted = notes_md::format_insert_section(&row.heading, insert_body_from_new_text(&row.heading, &row.new_text));
                if memo.ends_with(&formatted) {
                    formatted
                } else {
                    return None;
                }
            };
            Some(memo[..memo.len() - piece.len()].to_string())
        }
        "append" => notes_md::undo_append(memo, &row.chunk).ok(),
        _ => None,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmNoteProposalRow {
    pub id: String,
    pub thread_id: String,
    pub note_id: String,
    pub request_id: String,
    pub assistant_message_id: String,
    pub kind: String,
    pub heading: String,
    pub old_text: String,
    pub new_text: String,
    pub chunk: String,
    pub note_updated_at: i64,
    pub status: String,
    pub created_at: i64,
    pub applied_at: Option<i64>,
    pub note_title: String,
    #[serde(default, skip_deserializing)]
    pub diff: Vec<notes_md::DiffLine>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteRow {
    pub id: String,
    pub title: String,
    pub memo: String,
    pub view_mode: String,
    pub sort_order: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteItemRow {
    pub id: String,
    pub note_id: String,
    pub sort_order: i64,
    pub query: String,
    pub paragraph_id: String,
    /// JSON string of NoteItemSnapshot.
    pub item_json: String,
    pub memo: String,
    pub created_at: i64,
}

const ACTIVE_NOTE_ID_KEY: &str = "active_note_id";
const ACTIVE_LLM_THREAD_ID_KEY: &str = "active_llm_thread_id";

#[derive(Debug, Clone)]
pub struct FileMeta {
    pub size: i64,
    pub mtime: i64,
    pub content_hash: String,
}

#[cfg(test)]
mod date_range_tests {
    use super::*;

    fn temp_db() -> (std::path::PathBuf, Db) {
        let dir = std::env::temp_dir().join(format!(
            "argos-db-date-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("argos.db")).unwrap();
        (dir, db)
    }

    fn insert_file_status(db: &Db, folder_id: i64, path: &str, mtime: i64, status: &str) {
        let conn = db.conn.lock();
        conn.execute(
            "INSERT INTO files(folder_id, path, ext, size, mtime, content_hash, indexed_at, status)
             VALUES(?1, ?2, 'txt', 1, ?3, '', NULL, ?4)",
            rusqlite::params![folder_id, path, mtime, status],
        )
        .unwrap();
    }

    #[test]
    fn file_range_keeps_ok_and_drops_other_status() {
        let (dir, db) = temp_db();
        let folder = db.add_folder(r"C:\docs", "").unwrap();
        let in_range = 1_700_000_100;
        let out_range = 1_600_000_000;
        db.upsert_file(folder.id, r"C:\docs\ok.txt", "txt", 1, in_range, "h")
            .unwrap();
        insert_file_status(&db, folder.id, r"C:\docs\pending.txt", in_range, "pending");
        insert_file_status(&db, folder.id, r"C:\docs\old.txt", out_range, "ok");
        insert_file_status(&db, folder.id, r"C:\docs\zero.txt", 0, "ok");

        let paths = db
            .list_ok_file_paths_in_range(Some(1_700_000_000), Some(1_700_000_200), 100)
            .unwrap();
        assert_eq!(paths, vec![r"C:\docs\ok.txt".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn email_range_keeps_indexed_and_from_substr() {
        let (dir, db) = temp_db();
        let in_range = 1_700_000_100;
        let out_range = 1_600_000_000;
        db.upsert_email_message(
            "outlook:ok",
            "s",
            "e1",
            "",
            "受信トレイ",
            "山田太郎",
            "件名",
            in_range,
            "h",
            "indexed",
        )
        .unwrap();
        db.upsert_email_message(
            "outlook:pending",
            "s",
            "e2",
            "",
            "受信トレイ",
            "山田太郎",
            "件名",
            in_range,
            "h",
            "pending",
        )
        .unwrap();
        db.upsert_email_message(
            "outlook:superseded",
            "s",
            "e3",
            "",
            "受信トレイ",
            "山田太郎",
            "件名",
            in_range,
            "h",
            "superseded",
        )
        .unwrap();
        db.upsert_email_message(
            "outlook:old",
            "s",
            "e4",
            "",
            "受信トレイ",
            "山田太郎",
            "件名",
            out_range,
            "h",
            "indexed",
        )
        .unwrap();
        db.upsert_email_message(
            "outlook:other",
            "s",
            "e5",
            "",
            "受信トレイ",
            "佐藤",
            "件名",
            in_range,
            "h",
            "indexed",
        )
        .unwrap();

        let paths = db
            .list_indexed_email_paths_in_range(
                Some(1_700_000_000),
                Some(1_700_000_200),
                Some("山田"),
                None,
                100,
            )
            .unwrap();
        assert_eq!(paths, vec!["outlook:ok".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod remote_share_ids_tests {
    use super::*;

    fn temp_db() -> (std::path::PathBuf, Db) {
        let dir = std::env::temp_dir().join(format!(
            "argos-db-share-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("argos.db")).unwrap();
        (dir, db)
    }

    #[test]
    fn missing_and_broken_json_are_empty() {
        assert!(parse_remote_share_folder_ids(None).is_empty());
        assert!(parse_remote_share_folder_ids(Some("")).is_empty());
        assert!(parse_remote_share_folder_ids(Some("not-json")).is_empty());
        assert!(parse_remote_share_folder_ids(Some("{}")).is_empty());
    }

    #[test]
    fn default_folder_is_not_shared() {
        let (dir, db) = temp_db();
        let folder = db.add_folder(r"C:\docs", "").unwrap();
        assert!(!folder.share_remote);
        assert!(db.list_remote_share_folder_ids().is_empty());
        let listed = db.list_folders().unwrap();
        assert_eq!(listed.len(), 1);
        assert!(!listed[0].share_remote);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn toggle_and_list_overlay() {
        let (dir, db) = temp_db();
        let folder = db.add_folder(r"C:\docs", "").unwrap();
        let on = db
            .set_folder_share_remote(folder.id, true)
            .unwrap()
            .unwrap();
        assert!(on.share_remote);
        assert_eq!(db.list_remote_share_folder_ids(), vec![folder.id]);
        let listed = db.list_folders().unwrap();
        assert!(listed[0].share_remote);
        let off = db
            .set_folder_share_remote(folder.id, false)
            .unwrap()
            .unwrap();
        assert!(!off.share_remote);
        assert!(db.list_remote_share_folder_ids().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn remove_folder_prunes_share_ids() {
        let (dir, db) = temp_db();
        let folder = db.add_folder(r"C:\docs", "").unwrap();
        db.set_folder_share_remote(folder.id, true).unwrap();
        db.remove_folder(folder.id).unwrap();
        assert!(db.list_remote_share_folder_ids().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_folder_toggle_is_none() {
        let (dir, db) = temp_db();
        assert!(db.set_folder_share_remote(99, true).unwrap().is_none());
        assert!(db.list_remote_share_folder_ids().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod llm_source_pending_tests {
    use super::*;

    fn temp_db() -> (std::path::PathBuf, Db) {
        let dir = std::env::temp_dir().join(format!(
            "argos-db-src-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("argos.db")).unwrap();
        (dir, db)
    }

    #[test]
    fn pending_by_path_finds_pdf_pages() {
        let (dir, db) = temp_db();
        let thread = db.create_llm_thread("t", false).unwrap();
        db.insert_llm_source_full(
            &thread.id,
            "attach",
            r"C:\scan.pdf",
            "scan.pdf（1ページ目）",
            "pdf-page:1",
            "",
            "",
            "file",
            "image",
            "chat-files/t/a.jpg",
            "pending",
            None,
        )
        .unwrap();
        assert!(db
            .find_pending_llm_source(&thread.id, r"C:\scan.pdf", "")
            .unwrap()
            .is_none());
        let found = db
            .find_any_pending_llm_source_by_path(&thread.id, r"C:\scan.pdf")
            .unwrap()
            .expect("page source");
        assert_eq!(found.paragraph_id, "pdf-page:1");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn image_group_cite_fills_zero_only() {
        let (dir, db) = temp_db();
        let thread = db.create_llm_thread("t", false).unwrap();
        let (p1, _) = db
            .insert_llm_source_full(
                &thread.id,
                "attach",
                r"C:\scan.pdf",
                "scan.pdf（1ページ目）",
                "pdf-page:1",
                "a",
                "",
                "file",
                "image",
                "a.jpg",
                "",
                Some("p1"),
            )
            .unwrap();
        let (p2, _) = db
            .insert_llm_source_full(
                &thread.id,
                "attach",
                r"C:\SCAN.PDF",
                "scan.pdf（2ページ目）",
                "pdf-page:2",
                "b",
                "",
                "file",
                "image",
                "b.jpg",
                "",
                Some("p2"),
            )
            .unwrap();
        db.set_llm_source_cite_no(&p1.id, 3).unwrap();
        db.apply_image_group_cite(&thread.id, r"C:\scan.pdf", 3)
            .unwrap();
        let g = db.list_llm_image_group(&thread.id, r"C:\scan.pdf").unwrap();
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].id, p1.id);
        assert_eq!(g[0].cite_no, 3);
        assert_eq!(g[1].id, p2.id);
        assert_eq!(g[1].cite_no, 3);
        db.set_llm_source_cite_no(&p2.id, 5).unwrap();
        db.apply_image_group_cite(&thread.id, r"C:\scan.pdf", 3)
            .unwrap();
        let p2b = db.get_llm_source(&p2.id).unwrap().unwrap();
        assert_eq!(p2b.cite_no, 5);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn inherit_image_group_meta_copies_injected() {
        let (dir, db) = temp_db();
        let thread = db.create_llm_thread("t", false).unwrap();
        let (p1, _) = db
            .insert_llm_source_full(
                &thread.id,
                "attach",
                r"C:\scan.pdf",
                "scan.pdf（1ページ目）",
                "pdf-page:1",
                "a",
                "",
                "file",
                "image",
                "a.jpg",
                "",
                Some("p1"),
            )
            .unwrap();
        let (p2, _) = db
            .insert_llm_source_full(
                &thread.id,
                "attach",
                r"C:\scan.pdf",
                "scan.pdf（2ページ目）",
                "pdf-page:2",
                "b",
                "",
                "file",
                "image",
                "b.jpg",
                "",
                Some("p2"),
            )
            .unwrap();
        db.consume_llm_sources(&[(p1.id.clone(), 4)], "u1", "a1")
            .unwrap();
        db.inherit_image_group_meta(&p2.id).unwrap();
        let p2b = db.get_llm_source(&p2.id).unwrap().unwrap();
        assert_eq!(p2b.cite_no, 4);
        assert_eq!(p2b.injected_user_message_id, "u1");
        assert_eq!(p2b.cited_assistant_message_id, "a1");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn delete_image_group_removes_all_pages() {
        let (dir, db) = temp_db();
        let thread = db.create_llm_thread("t", false).unwrap();
        db.insert_llm_source_full(
            &thread.id,
            "attach",
            r"C:\scan.pdf",
            "scan.pdf（1ページ目）",
            "pdf-page:1",
            "a",
            "",
            "file",
            "image",
            "a.jpg",
            "",
            Some("p1"),
        )
        .unwrap();
        db.insert_llm_source_full(
            &thread.id,
            "attach",
            r"C:\scan.pdf",
            "scan.pdf（2ページ目）",
            "pdf-page:2",
            "b",
            "",
            "file",
            "image",
            "b.jpg",
            "",
            Some("p2"),
        )
        .unwrap();
        db.insert_llm_source_full(
            &thread.id,
            "tool",
            r"C:\scan.pdf",
            "scan.pdf",
            "p99",
            "hit",
            "q",
            "unit",
            "text",
            "",
            "",
            Some("t1"),
        )
        .unwrap();
        let g = db.list_llm_image_group(&thread.id, r"C:\scan.pdf").unwrap();
        assert_eq!(g.len(), 2);
        for row in g {
            db.delete_llm_source(&row.id).unwrap();
        }
        assert!(db
            .list_llm_image_group(&thread.id, r"C:\scan.pdf")
            .unwrap()
            .is_empty());
        assert_eq!(db.list_llm_sources(&thread.id).unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn insert_full_keeps_web_kind() {
        let (dir, db) = temp_db();
        let thread = db.create_llm_thread("t", false).unwrap();
        let (row, created) = db
            .insert_llm_source_full(
                &thread.id,
                "tool",
                "https://example.com/a",
                "例",
                "",
                "スニペット",
                "解雇",
                "unit",
                "web",
                "",
                "",
                None,
            )
            .unwrap();
        assert!(created);
        assert_eq!(row.kind, "web");
        assert!(row.is_web());
        let loaded = db.list_llm_sources(&thread.id).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].kind, "web");
        let found = db
            .find_llm_source_by_path(&thread.id, "HTTPS://EXAMPLE.COM/a")
            .unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().kind, "web");
        let (updated, _) = db
            .insert_llm_source_full(
                &thread.id,
                "tool",
                "https://example.com/a",
                "例",
                "",
                "短い",
                "解雇",
                "unit",
                "web",
                "",
                "",
                None,
            )
            .unwrap();
        assert_eq!(
            updated.body, "短い",
            "pending は上書きされるので、スニペット保存は既存行を避ける"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod note_proposal_tests {
    use super::*;

    fn temp_db() -> (std::path::PathBuf, Db) {
        let dir = std::env::temp_dir().join(format!(
            "argos-db-prop-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("argos.db")).unwrap();
        (dir, db)
    }

    #[test]
    fn create_note_starts_empty() {
        let (dir, db) = temp_db();
        let note = db.create_note("事件A").unwrap();
        assert!(note.memo.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_replace_and_stale_and_supersede() {
        let (dir, db) = temp_db();
        let note = db.create_note("事件A").unwrap();
        db.update_note_memo(&note.id, "# 争点\nA\n\n# 日程\nB\n")
            .unwrap();
        let note = db.get_note(&note.id).unwrap().unwrap();
        let thread = db.create_llm_thread("会話", true).unwrap();
        db.set_llm_thread_note(&thread.id, &note.id).unwrap();

        let first = db
            .insert_note_proposal(
                &thread.id,
                &note.id,
                "req1",
                "replace",
                "日程",
                "# 日程\nB\n",
                "# 日程\nC\n",
                "",
                note.updated_at,
            )
            .unwrap();
        let second = db
            .insert_note_proposal(
                &thread.id,
                &note.id,
                "req2",
                "replace",
                "日程",
                "# 日程\nB\n",
                "# 日程\nD\n",
                "",
                note.updated_at,
            )
            .unwrap();
        let first = db.get_note_proposal(&first.id).unwrap().unwrap();
        assert_eq!(first.status, "superseded");
        assert_eq!(second.status, "pending");

        db.apply_note_proposal(&second.id).unwrap();
        let note = db.get_note(&note.id).unwrap().unwrap();
        assert!(note.memo.contains("# 日程\nD"));
        assert!(note.memo.contains("# 争点\nA"));

        db.update_note_memo(&note.id, "# 争点\nA\n\n# 日程\nB\n")
            .unwrap();
        let note = db.get_note(&note.id).unwrap().unwrap();
        let stale = db
            .insert_note_proposal(
                &thread.id,
                &note.id,
                "req3",
                "replace",
                "日程",
                "# 日程\nOLD\n",
                "# 日程\nNEW\n",
                "",
                note.updated_at,
            )
            .unwrap();
        let err = db.apply_note_proposal(&stale.id).unwrap_err();
        assert!(err.contains("変わって"));
        let stale = db.get_note_proposal(&stale.id).unwrap().unwrap();
        assert_eq!(stale.status, "stale");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn orphan_proposals_are_discarded() {
        let (dir, db) = temp_db();
        let note = db.create_note("n").unwrap();
        let thread = db.create_llm_thread("t", true).unwrap();
        db.insert_note_proposal(
            &thread.id,
            &note.id,
            "req-x",
            "append",
            "",
            &note.memo,
            "x",
            "x",
            note.updated_at,
        )
        .unwrap();
        assert_eq!(db.discard_orphan_note_proposals("req-x").unwrap(), 1);
        assert!(db
            .list_note_proposals_for_thread(&thread.id)
            .unwrap()
            .is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
