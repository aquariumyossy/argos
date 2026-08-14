//! Mail sync types and indexing helpers (COM-agnostic).

use serde::{Deserialize, Serialize};

use crate::extractor;
use crate::extractor::segment_mail_body;
use crate::mail::path::make_outlook_path;
use crate::search::tantivy_backend::{EmailDocMeta, TantivyBackend};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutlookFolderInfo {
    pub store_id: String,
    pub entry_id: String,
    pub name: String,
    pub path_label: String,
    pub item_count: i32,
}

#[derive(Debug, Clone)]
pub struct OutlookMessage {
    pub store_id: String,
    pub entry_id: String,
    pub subject: String,
    pub body_text: String,
    pub from: String,
    pub conversation_id: String,
    pub folder_name: String,
    pub received_unix: i64,
    pub last_mod_unix: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MailSyncStats {
    pub indexed: u32,
    pub skipped: u32,
    pub superseded: u32,
    pub errors: u32,
    pub folders: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MailSyncProgress {
    pub phase: String,
    pub folder_label: String,
    pub current: u32,
    pub total: u32,
    pub message: String,
    /// Running total of `email_messages` with status=indexed (for live UI).
    pub indexed_total: u32,
    /// Indexed count for `folder_label` so far in this folder pass.
    pub folder_indexed: u32,
}

/// Prefer plain body; fall back to HTML stripped to text.
pub fn normalize_mail_body(plain: &str, html_body: &str) -> String {
    let plain = plain.trim();
    if !plain.is_empty() {
        return plain.to_string();
    }
    let html_body = html_body.trim();
    if html_body.is_empty() {
        return String::new();
    }
    let (_, text) = extractor::html_to_text(html_body);
    text
}

pub fn content_fingerprint(msg: &OutlookMessage) -> String {
    use xxhash_rust::xxh64::xxh64;
    let payload = format!(
        "{}\n{}\n{}\n{}\n{}",
        msg.subject, msg.from, msg.received_unix, msg.conversation_id, msg.body_text
    );
    format!("{:016x}", xxh64(payload.as_bytes(), 0))
}

/// Index one message into Tantivy (replaces any prior units for this virtual path).
pub fn index_message(
    backend: &TantivyBackend,
    msg: &OutlookMessage,
) -> Result<usize, String> {
    let path = make_outlook_path(&msg.store_id, &msg.entry_id);
    let title = if msg.subject.trim().is_empty() {
        "(件名なし)".to_string()
    } else {
        msg.subject.clone()
    };
    let body = if msg.body_text.trim().is_empty() {
        title.clone()
    } else {
        msg.body_text.clone()
    };
    let units = segment_mail_body(&body);
    let meta = EmailDocMeta {
        from: msg.from.clone(),
        date_unix: msg.received_unix,
        conversation_id: msg.conversation_id.clone(),
        folder: msg.folder_name.clone(),
    };
    backend.index_email(
        &path,
        &title,
        msg.received_unix.max(0) as u64,
        body.as_bytes().len() as u64,
        &units,
        &meta,
    )
}
