//! Dedicated STA thread for all Outlook COM work.

#![cfg(windows)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::db::Db;
use crate::mail::outlook_com;
use crate::mail::path::make_outlook_path;
use crate::mail::sync::{
    content_fingerprint, index_message, MailSyncProgress, MailSyncStats, OutlookFolderInfo,
};
use crate::search::tantivy_backend::TantivyBackend;

/// Short COM ops (detect / list / open). Outlook connect alone can take ~25s.
const SHORT_JOB_TIMEOUT: Duration = Duration::from_secs(60);
/// Full mailbox sync may run a long time; still bound so UI never waits forever.
const SYNC_JOB_TIMEOUT: Duration = Duration::from_secs(30 * 60);

enum Job {
    Detect {
        reply: Sender<Result<String, String>>,
    },
    IsRunning {
        reply: Sender<bool>,
    },
    ListFolders {
        reply: Sender<Result<Vec<OutlookFolderInfo>, String>>,
    },
    Sync {
        allow_launch: bool,
        reply: Sender<Result<MailSyncStats, String>>,
        progress: Sender<MailSyncProgress>,
    },
    Open {
        store_id: String,
        entry_id: String,
        reply: Sender<Result<(), String>>,
    },
}

#[derive(Clone)]
pub struct MailStaHandle {
    tx: Sender<Job>,
    /// True while the STA thread is inside `run_sync` (including Outlook fetch).
    syncing: Arc<AtomicBool>,
}

fn recv_result<T>(rx: Receiver<T>, timeout: Duration, label: &str) -> Result<T, String> {
    match rx.recv_timeout(timeout) {
        Ok(v) => Ok(v),
        Err(RecvTimeoutError::Timeout) => Err(format!(
            "Outlook STA 応答タイムアウト（{label}、{}秒）",
            timeout.as_secs()
        )),
        Err(RecvTimeoutError::Disconnected) => Err("Outlook STA 応答なし".to_string()),
    }
}

impl MailStaHandle {
    pub fn start(db: Arc<Db>, backend: Arc<TantivyBackend>) -> Result<Self, String> {
        let (tx, rx) = mpsc::channel::<Job>();
        let syncing = Arc::new(AtomicBool::new(false));
        let syncing_thread = syncing.clone();
        thread::Builder::new()
            .name("argos-outlook-sta".into())
            .spawn(move || sta_loop(rx, db, backend, syncing_thread))
            .map_err(|e| format!("Outlook STA スレッドを起動できません: {e}"))?;
        Ok(Self { tx, syncing })
    }

    pub fn detect(&self) -> Result<String, String> {
        let (reply, rx) = mpsc::channel();
        self.tx
            .send(Job::Detect { reply })
            .map_err(|_| "Outlook STA スレッドが停止しています".to_string())?;
        recv_result(rx, SHORT_JOB_TIMEOUT, "detect")?
    }

    pub fn is_running(&self) -> bool {
        let (reply, rx) = mpsc::channel();
        if self.tx.send(Job::IsRunning { reply }).is_err() {
            return false;
        }
        recv_result(rx, SHORT_JOB_TIMEOUT, "is_running").unwrap_or(false)
    }

    pub fn list_folders(&self) -> Result<Vec<OutlookFolderInfo>, String> {
        let (reply, rx) = mpsc::channel();
        self.tx
            .send(Job::ListFolders { reply })
            .map_err(|_| "Outlook STA スレッドが停止しています".to_string())?;
        recv_result(rx, SHORT_JOB_TIMEOUT, "list_folders")?
    }

    /// User-initiated sync may launch Outlook. Background sync should pass `false`.
    pub fn sync_all<F>(&self, allow_launch: bool, on_progress: F) -> Result<MailSyncStats, String>
    where
        F: FnMut(MailSyncProgress) + Send + 'static,
    {
        let (reply, rx) = mpsc::channel();
        let (ptx, prx) = mpsc::channel::<MailSyncProgress>();
        self.tx
            .send(Job::Sync {
                allow_launch,
                reply,
                progress: ptx,
            })
            .map_err(|_| "Outlook STA スレッドが停止しています".to_string())?;

        let progress_thread = thread::spawn(move || {
            let mut cb = on_progress;
            while let Ok(p) = prx.recv() {
                cb(p);
            }
        });

        let result = recv_result(rx, SYNC_JOB_TIMEOUT, "sync")?;
        let _ = progress_thread.join();
        result
    }

    pub fn open_item(&self, store_id: &str, entry_id: &str) -> Result<(), String> {
        if self.syncing.load(Ordering::SeqCst) {
            return Err(
                "メール同期中のため Outlook で開けません。同期の完了後に再試行してください。"
                    .into(),
            );
        }
        let (reply, rx) = mpsc::channel();
        self.tx
            .send(Job::Open {
                store_id: store_id.to_string(),
                entry_id: entry_id.to_string(),
                reply,
            })
            .map_err(|_| "Outlook STA スレッドが停止しています".to_string())?;
        recv_result(rx, SHORT_JOB_TIMEOUT, "open")?
    }
}

fn sta_loop(
    rx: Receiver<Job>,
    db: Arc<Db>,
    backend: Arc<TantivyBackend>,
    syncing: Arc<AtomicBool>,
) {
    if let Err(e) = outlook_com::com_init() {
        eprintln!("argos: Outlook STA CoInitializeEx failed: {e}");
        while let Ok(job) = rx.recv() {
            match job {
                Job::Detect { reply } => {
                    let _ = reply.send(Err(e.clone()));
                }
                Job::IsRunning { reply } => {
                    let _ = reply.send(false);
                }
                Job::ListFolders { reply } => {
                    let _ = reply.send(Err(e.clone()));
                }
                Job::Sync { reply, .. } => {
                    let _ = reply.send(Err(e.clone()));
                }
                Job::Open { reply, .. } => {
                    let _ = reply.send(Err(e.clone()));
                }
            }
        }
        return;
    }

    while let Ok(job) = rx.recv() {
        match job {
            Job::Detect { reply } => {
                let _ = reply.send(outlook_com::detect_outlook());
            }
            Job::IsRunning { reply } => {
                let _ = reply.send(outlook_com::outlook_is_running());
            }
            Job::ListFolders { reply } => {
                let _ = reply.send(outlook_com::list_mail_folders());
            }
            Job::Sync {
                allow_launch,
                reply,
                progress,
            } => {
                syncing.store(true, Ordering::SeqCst);
                let result = run_sync(&db, &backend, allow_launch, |p| {
                    let _ = progress.send(p);
                });
                syncing.store(false, Ordering::SeqCst);
                drop(progress);
                let _ = reply.send(result);
            }
            Job::Open {
                store_id,
                entry_id,
                reply,
            } => {
                let _ = reply.send(outlook_com::open_mail_item(&store_id, &entry_id));
            }
        }
    }

    outlook_com::com_uninit();
}

fn run_sync<F>(
    db: &Db,
    backend: &TantivyBackend,
    allow_launch: bool,
    mut on_progress: F,
) -> Result<MailSyncStats, String>
where
    F: FnMut(MailSyncProgress),
{
    let settings = db.load_settings();
    if !settings.mail_enabled {
        return Err("Outlook メール索引が無効です".into());
    }

    let folders = db
        .list_selected_email_folders()
        .map_err(|e| e.to_string())?;
    if folders.is_empty() {
        return Err("同期する Outlook フォルダが選択されていません".into());
    }

    // Announce before the blocking launch/retry so the UI can show it immediately.
    if allow_launch && !outlook_com::outlook_is_running() {
        on_progress(mail_progress(
            db,
            "starting",
            "",
            0,
            folders.len() as u32,
            "Outlook を起動しています…",
            0,
        ));
    }

    match outlook_com::connect_outlook(allow_launch) {
        Ok((_, _info)) => {}
        Err(e) if !allow_launch => {
            eprintln!("argos: periodic mail sync skipped: {e}");
            return Ok(MailSyncStats::default());
        }
        Err(e) => return Err(e),
    }

    let days = settings.mail_days_back.max(1) as i64;
    let since = chrono::Utc::now().timestamp() - days * 86400;
    let latest_only = settings.mail_latest_only;

    let mut stats = MailSyncStats::default();
    stats.folders = folders.len() as u32;

    on_progress(mail_progress(
        db,
        "starting",
        "",
        0,
        folders.len() as u32,
        "Outlook 同期を開始",
        0,
    ));

    for (fi, folder) in folders.iter().enumerate() {
        on_progress(mail_progress(
            db,
            "folder",
            folder.path_label.clone(),
            fi as u32 + 1,
            folders.len() as u32,
            format!("フォルダ取得中: {}", folder.path_label),
            0,
        ));

        // Outlook should already be running after connect_outlook above.
        let messages = match outlook_com::fetch_messages_in_folder(
            &folder.entry_id,
            &folder.store_id,
            since,
            false,
        ) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("argos: mail folder sync error ({}): {e}", folder.path_label);
                stats.errors += 1;
                continue;
            }
        };

        let total = messages.len() as u32;
        let mut folder_indexed = 0u32;
        for (mi, msg) in messages.into_iter().enumerate() {
            let path = make_outlook_path(&msg.store_id, &msg.entry_id);
            let mut msg = msg;
            msg.folder_name = folder.path_label.clone();
            let hash = content_fingerprint(&msg);

            let mut skip_index = false;
            if let Ok(Some(existing)) = db.get_email_message_by_path(&path) {
                if existing.content_hash == hash
                    && existing.status == "indexed"
                    && existing.folder_name == msg.folder_name
                    && (existing.date_unix > 0 || msg.received_unix <= 0)
                    && backend.has_path(&path).unwrap_or(false)
                {
                    stats.skipped += 1;
                    folder_indexed += 1;
                    skip_index = true;
                }
            }

            if !skip_index && latest_only && !msg.conversation_id.is_empty() {
                if let Ok(Some(winner)) = db.get_email_thread_winner(&msg.conversation_id) {
                    if msg.received_unix < winner.date_unix
                        || (msg.received_unix == winner.date_unix && path != winner.path)
                    {
                        let _ = db.upsert_email_message(
                            &path,
                            &msg.store_id,
                            &msg.entry_id,
                            &msg.conversation_id,
                            &msg.folder_name,
                            &msg.from,
                            &msg.subject,
                            msg.received_unix,
                            &hash,
                            "superseded",
                        );
                        stats.superseded += 1;
                        skip_index = true;
                    } else if winner.path != path {
                        let _ = backend.delete_by_path(&winner.path);
                        let _ = db.set_email_message_status(&winner.path, "superseded");
                    }
                }
            }

            if !skip_index {
                match index_message(backend, &msg) {
                    Ok(_) => {
                        let _ = db.upsert_email_message(
                            &path,
                            &msg.store_id,
                            &msg.entry_id,
                            &msg.conversation_id,
                            &msg.folder_name,
                            &msg.from,
                            &msg.subject,
                            msg.received_unix,
                            &hash,
                            "indexed",
                        );
                        if !msg.conversation_id.is_empty() {
                            let _ = db.upsert_email_thread(
                                &msg.conversation_id,
                                &path,
                                msg.received_unix,
                            );
                        }
                        stats.indexed += 1;
                        folder_indexed += 1;
                    }
                    Err(e) => {
                        eprintln!("argos: index email failed ({path}): {e}");
                        stats.errors += 1;
                    }
                }
            }

            if mi % 25 == 0 || mi + 1 == total as usize {
                on_progress(mail_progress(
                    db,
                    "indexing",
                    folder.path_label.clone(),
                    mi as u32 + 1,
                    total,
                    format!("{} ({}/{})", folder.path_label, mi + 1, total),
                    folder_indexed,
                ));
            }
        }
    }

    let _ = db.set_mail_last_sync_now();
    on_progress(mail_progress(
        db,
        "done",
        "",
        stats.folders,
        stats.folders,
        format!(
            "完了: インデックス登録 {} / スキップ {} / エラー {}",
            stats.indexed, stats.skipped, stats.errors
        ),
        0,
    ));
    Ok(stats)
}

fn mail_progress(
    db: &Db,
    phase: &str,
    folder_label: impl Into<String>,
    current: u32,
    total: u32,
    message: impl Into<String>,
    folder_indexed: u32,
) -> MailSyncProgress {
    MailSyncProgress {
        phase: phase.into(),
        folder_label: folder_label.into(),
        current,
        total,
        message: message.into(),
        indexed_total: db.count_indexed_emails().unwrap_or(0),
        folder_indexed,
    }
}
