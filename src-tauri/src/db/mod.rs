use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub shortcut: String,
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
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            shortcut: "Ctrl+Alt+A".into(),
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
        }
    }
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
              created_at TEXT NOT NULL
            );
            "#,
        )?;
        // Migrate older DBs that lack public_path
        let _ = conn.execute(
            "ALTER TABLE folders ADD COLUMN public_path TEXT NOT NULL DEFAULT ''",
            [],
        );
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
                    "remote_server_port" => {
                        s.remote_server_port = row.1.parse().unwrap_or(17890)
                    }
                    "remote_server_token" => s.remote_server_token = row.1,
                    "search_mode" => {
                        s.search_mode = match row.1.as_str() {
                            "remote" | "hybrid" | "local" => row.1,
                            _ => "local".into(),
                        }
                    }
                    "remote_url" => s.remote_url = row.1,
                    "remote_token" => s.remote_token = row.1,
                    "remote_timeout_ms" => {
                        s.remote_timeout_ms = row.1.parse().unwrap_or(3000)
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
        let rows = stmt.query_map([], |row| {
            Ok(FolderRow {
                id: row.get(0)?,
                path: row.get(1)?,
                public_path: row.get(2)?,
                enabled: row.get::<_, i64>(3)? != 0,
                indexed_count: row.get::<_, i64>(4)? as u32,
            })
        })?;
        Ok(rows.flatten().collect())
    }

    pub fn add_folder(
        &self,
        path: &str,
        public_path: &str,
    ) -> Result<FolderRow, rusqlite::Error> {
        let conn = self.conn.lock();
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT OR IGNORE INTO folders(path, public_path, enabled, created_at) VALUES(?1, ?2, 1, ?3)",
            rusqlite::params![path, public_path, now],
        )?;
        let id: i64 = conn.query_row(
            "SELECT id FROM folders WHERE path=?1",
            [path],
            |r| r.get(0),
        )?;
        let public_path: String = conn.query_row(
            "SELECT COALESCE(public_path, '') FROM folders WHERE id=?1",
            [id],
            |r| r.get(0),
        )?;
        Ok(FolderRow {
            id,
            path: path.to_string(),
            public_path,
            enabled: true,
            indexed_count: 0,
        })
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

    pub fn remove_folder(&self, id: i64) -> Result<Option<String>, rusqlite::Error> {
        let conn = self.conn.lock();
        let _ = conn.execute_batch("PRAGMA foreign_keys=ON;")?;
        let path: Option<String> = conn
            .query_row("SELECT path FROM folders WHERE id=?1", [id], |r| r.get(0))
            .ok();
        // Explicitly remove file rows (CASCADE may be off on older sessions)
        conn.execute("DELETE FROM files WHERE folder_id=?1", [id])?;
        conn.execute("DELETE FROM folders WHERE id=?1", [id])?;
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
        let mut rows = stmt.query_map([id], |row| {
            Ok(FolderRow {
                id: row.get(0)?,
                path: row.get(1)?,
                public_path: row.get(2)?,
                enabled: row.get::<_, i64>(3)? != 0,
                indexed_count: row.get::<_, i64>(4)? as u32,
            })
        })?;
        if let Some(Ok(row)) = rows.next() {
            return Ok(Some(row));
        }
        Ok(None)
    }

    pub fn get_folder_by_path(&self, path: &str) -> Result<Option<FolderRow>, rusqlite::Error> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT f.id, f.path, COALESCE(f.public_path, ''), f.enabled,
                    COALESCE((SELECT COUNT(*) FROM files WHERE folder_id = f.id), 0)
             FROM folders f
             WHERE f.path=?1",
        )?;
        let mut rows = stmt.query_map([path], |row| {
            Ok(FolderRow {
                id: row.get(0)?,
                path: row.get(1)?,
                public_path: row.get(2)?,
                enabled: row.get::<_, i64>(3)? != 0,
                indexed_count: row.get::<_, i64>(4)? as u32,
            })
        })?;
        if let Some(Ok(row)) = rows.next() {
            return Ok(Some(row));
        }
        Ok(None)
    }

    pub fn list_file_paths_by_folder(&self, folder_id: i64) -> Result<Vec<String>, rusqlite::Error> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare("SELECT path FROM files WHERE folder_id=?1")?;
        let rows = stmt.query_map([folder_id], |row| row.get(0))?;
        Ok(rows.flatten().collect())
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
        let id: i64 = conn.query_row(
            "SELECT id FROM exclude_paths WHERE path=?1",
            [path],
            |r| r.get(0),
        )?;
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
        let mut stmt = conn.prepare("SELECT id, word FROM search_words ORDER BY id")?;
        let rows = stmt.query_map([], |row| {
            Ok(SearchWordRow {
                id: row.get(0)?,
                word: row.get(1)?,
            })
        })?;
        Ok(rows.flatten().collect())
    }

    pub fn add_search_word(&self, word: &str) -> Result<SearchWordRow, rusqlite::Error> {
        let conn = self.conn.lock();
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT OR IGNORE INTO search_words(word, created_at) VALUES(?1, ?2)",
            rusqlite::params![word, now],
        )?;
        let id: i64 = conn.query_row(
            "SELECT id FROM search_words WHERE word=?1",
            [word],
            |r| r.get(0),
        )?;
        Ok(SearchWordRow {
            id,
            word: word.to_string(),
        })
    }

    pub fn update_search_word(
        &self,
        id: i64,
        word: &str,
    ) -> Result<Option<SearchWordRow>, rusqlite::Error> {
        let conn = self.conn.lock();
        let n = conn.execute(
            "UPDATE search_words SET word=?1 WHERE id=?2",
            rusqlite::params![word, id],
        )?;
        if n == 0 {
            return Ok(None);
        }
        Ok(Some(SearchWordRow {
            id,
            word: word.to_string(),
        }))
    }

    pub fn remove_search_word(&self, id: i64) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock();
        conn.execute("DELETE FROM search_words WHERE id=?1", [id])?;
        Ok(())
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
        let mut stmt = conn.prepare(
            "SELECT size, mtime, content_hash FROM files WHERE path=?1",
        )?;
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
}

#[derive(Debug, Clone)]
pub struct FileMeta {
    pub size: i64,
    pub mtime: i64,
    pub content_hash: String,
}
