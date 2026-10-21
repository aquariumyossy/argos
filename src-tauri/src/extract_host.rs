//! File listing, stat, and text extraction.
//!
//! In the app this runs in a child process (`argos.exe --extract-worker`) under a
//! Windows job memory limit. A stack overflow, abort, or runaway allocation kills
//! the child; the indexer records that path and continues. Unit tests run the same
//! functions in-process, with a timeout thread, because `cargo test` is not the app exe.

use std::fs;
use std::io::{self, Read, Write};
use std::path::Path;
#[cfg(not(test))]
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
#[cfg(not(test))]
use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime};
#[cfg(not(test))]
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::extractor::{self, content_hash};

/// Hard cap for a job-object child. One parsed PDF can expand well past the file
/// size; past this Windows kills the child instead of the indexer.
#[cfg(not(test))]
const CHILD_MEMORY_LIMIT: usize = 1024 * 1024 * 1024;
const FRAME_LIMIT: usize = 128 * 1024 * 1024;

#[derive(Debug)]
pub enum IoFail {
    Failed(String),
    TimedOut,
    Crashed(String),
    /// The worker process could not be started or is in backoff. Not a property of the file.
    Unavailable(String),
}

/// Set when a killed worker is still alive after the bounded wait.
pub static WORKER_DID_NOT_EXIT: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone)]
pub struct FileStat {
    pub size: u64,
    pub mtime: Option<u64>,
    pub is_dir: bool,
    pub is_file: bool,
}

#[derive(Debug)]
pub enum ExtractOutcome {
    Text { title: String, pages: Vec<String> },
    NoText,
    TooBig,
    Failed(String),
}

#[derive(Debug)]
pub struct LoadedFile {
    pub size: u64,
    pub mtime: Option<u64>,
    /// Empty when the file was not read.
    pub hash: String,
    pub outcome: ExtractOutcome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListedEntry {
    pub name: String,
    pub kind: EntryKind,
}

#[derive(Debug, Serialize, Deserialize)]
enum Req {
    Ping,
    Stat { path: String },
    Extract { path: String, max_bytes: u64 },
    List { path: String },
}

#[derive(Debug, Serialize, Deserialize)]
enum Resp {
    Pong,
    Stat {
        is_dir: bool,
        is_file: bool,
        size: u64,
        mtime: Option<u64>,
    },
    Extracted {
        size: u64,
        mtime: Option<u64>,
        hash: String,
        outcome: WireOutcome,
    },
    List { entries: Vec<ListedEntry> },
    Failed { message: String },
}

#[derive(Debug, Serialize, Deserialize)]
enum WireOutcome {
    Text { title: String, pages: Vec<String> },
    NoText,
    TooBig,
    Failed(String),
    Panicked,
}

pub struct ExtractHost {
    #[cfg(test)]
    block_name: Option<String>,
    /// When set, `stat` does not sleep. Tests use this to hang extract only.
    #[cfg(test)]
    block_extract_only: bool,
    /// Next `extract` returns `IoFail::Crashed` without reading the file.
    #[cfg(test)]
    extract_crash: bool,
    #[cfg(not(test))]
    worker: Option<Worker>,
    /// Next time a failed spawn may be tried. Unset while a worker is healthy.
    #[cfg(not(test))]
    retry_at: Option<Instant>,
    #[cfg(not(test))]
    spawn_failures: u32,
}

impl ExtractHost {
    pub fn new() -> Self {
        Self {
            #[cfg(test)]
            block_name: None,
            #[cfg(test)]
            block_extract_only: false,
            #[cfg(test)]
            extract_crash: false,
            #[cfg(not(test))]
            worker: None,
            #[cfg(not(test))]
            retry_at: None,
            #[cfg(not(test))]
            spawn_failures: 0,
        }
    }

    #[cfg(test)]
    pub fn set_block_name(&mut self, name: Option<String>) {
        self.block_name = name;
    }

    #[cfg(test)]
    pub fn set_block_extract_only(&mut self, only: bool) {
        self.block_extract_only = only;
    }

    #[cfg(test)]
    pub fn set_extract_crash(&mut self, crash: bool) {
        self.extract_crash = crash;
    }

    pub fn stat(&mut self, path: &Path, timeout: Duration) -> Result<FileStat, IoFail> {
        #[cfg(test)]
        {
            let path = path.to_path_buf();
            let block = if self.block_extract_only {
                None
            } else {
                self.block_name.clone()
            };
            return run_timeout(timeout, move || stat_path(&path, block.as_deref()));
        }
        #[cfg(not(test))]
        {
            let resp = self.roundtrip(
                &Req::Stat {
                    path: path.to_string_lossy().into_owned(),
                },
                timeout,
            )?;
            match resp {
                Resp::Stat {
                    is_dir,
                    is_file,
                    size,
                    mtime,
                } => Ok(FileStat {
                    size,
                    mtime,
                    is_dir,
                    is_file,
                }),
                Resp::Failed { message } => Err(IoFail::Failed(message)),
                _ => Err(IoFail::Failed("unexpected stat response".into())),
            }
        }
    }

    pub fn extract(
        &mut self,
        path: &Path,
        timeout: Duration,
        max_bytes: u64,
    ) -> Result<LoadedFile, IoFail> {
        #[cfg(test)]
        {
            if self.extract_crash {
                return Err(IoFail::Crashed("test crash".into()));
            }
            let path = path.to_path_buf();
            let block = self.block_name.clone();
            return run_timeout(timeout, move || {
                extract_path(&path, max_bytes, block.as_deref())
            });
        }
        #[cfg(not(test))]
        {
            let resp = self.roundtrip(
                &Req::Extract {
                    path: path.to_string_lossy().into_owned(),
                    max_bytes,
                },
                timeout,
            )?;
            match resp {
                Resp::Extracted {
                    size,
                    mtime,
                    hash,
                    outcome,
                } => Ok(LoadedFile {
                    size,
                    mtime,
                    hash,
                    outcome: match outcome {
                        WireOutcome::Text { title, pages } => ExtractOutcome::Text { title, pages },
                        WireOutcome::NoText => ExtractOutcome::NoText,
                        WireOutcome::TooBig => ExtractOutcome::TooBig,
                        WireOutcome::Failed(message) => ExtractOutcome::Failed(message),
                        WireOutcome::Panicked => {
                            ExtractOutcome::Failed("extract panicked".into())
                        }
                    },
                }),
                Resp::Failed { message } => Err(IoFail::Failed(message)),
                _ => Err(IoFail::Failed("unexpected extract response".into())),
            }
        }
    }

    pub fn list(&mut self, path: &Path, timeout: Duration) -> Result<Vec<ListedEntry>, IoFail> {
        #[cfg(test)]
        {
            let path = path.to_path_buf();
            let block = self.block_name.clone();
            return run_timeout(timeout, move || list_path(&path, block.as_deref()));
        }
        #[cfg(not(test))]
        {
            let resp = self.roundtrip(
                &Req::List {
                    path: path.to_string_lossy().into_owned(),
                },
                timeout,
            )?;
            match resp {
                Resp::List { entries } => Ok(entries),
                Resp::Failed { message } => Err(IoFail::Failed(message)),
                _ => Err(IoFail::Failed("unexpected list response".into())),
            }
        }
    }
}

/// Job-object handle. Owned by the host and only touched while the host mutex is held.
#[cfg(all(windows, not(test)))]
struct SendHandle(*mut core::ffi::c_void);
#[cfg(all(windows, not(test)))]
unsafe impl Send for SendHandle {}

#[cfg(not(test))]
struct Worker {
    child: std::process::Child,
    stdin: io::BufWriter<std::process::ChildStdin>,
    rx: std::sync::mpsc::Receiver<io::Result<Vec<u8>>>,
    reader: Option<std::thread::JoinHandle<()>>,
    #[cfg(windows)]
    job: Option<SendHandle>,
}

#[cfg(not(test))]
impl Drop for Worker {
    fn drop(&mut self) {
        let exited = reap_child(&mut self.child);
        if exited {
            if let Some(reader) = self.reader.take() {
                let _ = reader.join();
            }
        } else if let Some(reader) = self.reader.take() {
            // The reader is blocked on the pipe. Joining would wait for SMB I/O.
            std::mem::forget(reader);
        }
        #[cfg(windows)]
        if let Some(job) = self.job.take() {
            unsafe {
                let _ = windows::Win32::Foundation::CloseHandle(
                    windows::Win32::Foundation::HANDLE(job.0),
                );
            }
        }
    }
}

#[cfg(not(test))]
impl ExtractHost {
    fn roundtrip(&mut self, req: &Req, timeout: Duration) -> Result<Resp, IoFail> {
        self.ensure()?;
        let payload = serde_json::to_vec(req).map_err(|e| IoFail::Failed(e.to_string()))?;
        let worker = self.worker.as_mut().expect("worker started");
        if let Err(err) = write_frame(&mut worker.stdin, &payload) {
            self.stop_worker();
            return Err(IoFail::Crashed(format!("extractor write failed: {err}")));
        }
        let rx = &self.worker.as_ref().expect("worker started").rx;
        match rx.recv_timeout(timeout) {
            Ok(Ok(buf)) => serde_json::from_slice(&buf).map_err(|e| IoFail::Failed(e.to_string())),
            Ok(Err(err)) => {
                let detail = format!("extractor exited: {err}");
                self.stop_worker();
                Err(IoFail::Crashed(detail))
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                self.stop_worker();
                Err(IoFail::TimedOut)
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                self.stop_worker();
                Err(IoFail::Crashed("extractor pipe closed".into()))
            }
        }
    }

    fn ensure(&mut self) -> Result<(), IoFail> {
        if self.worker.is_some() {
            return Ok(());
        }
        if let Some(when) = self.retry_at {
            if Instant::now() < when {
                return Err(IoFail::Unavailable(
                    "extractor worker is backing off after a failed start".into(),
                ));
            }
        }
        let worker = match spawn_worker() {
            Ok(worker) => worker,
            Err(err) => {
                self.note_spawn_failure();
                eprintln!("argos: extractor worker failed to start: {err}");
                return Err(IoFail::Unavailable(err));
            }
        };
        self.worker = Some(worker);
        match self.roundtrip(&Req::Ping, Duration::from_secs(10)) {
            Ok(Resp::Pong) => {
                self.spawn_failures = 0;
                self.retry_at = None;
                Ok(())
            }
            Ok(_) => {
                self.stop_worker();
                self.note_spawn_failure();
                Err(IoFail::Unavailable(
                    "extractor worker did not answer ping".into(),
                ))
            }
            Err(err) => {
                self.stop_worker();
                self.note_spawn_failure();
                Err(IoFail::Unavailable(fail_text(err)))
            }
        }
    }

    fn note_spawn_failure(&mut self) {
        self.spawn_failures = self.spawn_failures.saturating_add(1);
        let shift = self.spawn_failures.saturating_sub(1).min(4);
        let delay = Duration::from_secs(1_u64 << shift);
        self.retry_at = Some(Instant::now() + delay);
        eprintln!(
            "argos: extractor worker will retry in {}s",
            delay.as_secs()
        );
    }

    fn stop_worker(&mut self) {
        self.worker = None;
    }
}

#[cfg(not(test))]
fn worker_program() -> Result<PathBuf, String> {
    // Debug and `cargo test` only. A release build always uses this executable.
    #[cfg(debug_assertions)]
    if let Some(path) = std::env::var_os("ARGOS_EXTRACT_WORKER") {
        if !path.is_empty() {
            return Ok(PathBuf::from(path));
        }
    }
    std::env::current_exe().map_err(|e| e.to_string())
}

#[cfg(not(test))]
fn spawn_worker() -> Result<Worker, String> {
    let exe = worker_program()?;
    // A GUI process has no stderr handle. Inheriting it passes NULL, and the
    // child then has nowhere safe to send pdf-extract's println!. NUL is a real
    // handle, so those warnings cannot land on the frame pipe.
    let mut child = std::process::Command::new(exe)
        .arg("--extract-worker")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| "extractor stdin missing".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "extractor stdout missing".to_string())?;
    #[cfg(windows)]
    let job = assign_memory_job(&child);
    let (tx, rx) = std::sync::mpsc::channel();
    let reader = std::thread::Builder::new()
        .name("argos-extract-io".into())
        .spawn(move || {
            let mut stdout = stdout;
            loop {
                match read_frame(&mut stdout) {
                    Ok(buf) => {
                        if tx.send(Ok(buf)).is_err() {
                            break;
                        }
                    }
                    Err(err) => {
                        let _ = tx.send(Err(err));
                        break;
                    }
                }
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(Worker {
        child,
        stdin: io::BufWriter::new(stdin),
        rx,
        reader: Some(reader),
        #[cfg(windows)]
        job,
    })
}

#[cfg(all(windows, not(test)))]
fn assign_memory_job(child: &std::process::Child) -> Option<SendHandle> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_PROCESS_MEMORY,
        JobObjectExtendedLimitInformation,
    };

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateJobObjectW(
            lp_job_attributes: *mut core::ffi::c_void,
            lp_name: *const u16,
        ) -> *mut core::ffi::c_void;
    }

    unsafe {
        let raw = CreateJobObjectW(std::ptr::null_mut(), std::ptr::null());
        if raw.is_null() {
            return None;
        }
        let job = HANDLE(raw);
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags =
            JOB_OBJECT_LIMIT_PROCESS_MEMORY | JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        info.ProcessMemoryLimit = CHILD_MEMORY_LIMIT;
        if SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const _,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
        .is_err()
        {
            let _ = CloseHandle(job);
            return None;
        }
        if AssignProcessToJobObject(job, HANDLE(child.as_raw_handle())).is_err() {
            eprintln!("argos: could not apply the extractor memory cap");
            let _ = CloseHandle(job);
            return None;
        }
        Some(SendHandle(raw))
    }
}

/// Entry point for `argos.exe --extract-worker`.
pub fn serve() {
    #[cfg(windows)]
    {
        let frame_out = match claim_frame_stdout() {
            Ok(out) => out,
            Err(err) => {
                eprintln!("argos: could not separate extractor frames from stdout: {err}");
                return;
            }
        };
        serve_on(frame_out);
    }
    #[cfg(not(windows))]
    serve_on(io::stdout());
}

fn serve_on(mut frame_out: impl Write) {
    let mut stdin = io::stdin().lock();
    loop {
        let frame = match read_frame(&mut stdin) {
            Ok(frame) => frame,
            Err(_) => break,
        };
        let req: Req = match serde_json::from_slice(&frame) {
            Ok(req) => req,
            Err(err) => {
                let _ = write_frame(
                    &mut frame_out,
                    &serde_json::to_vec(&Resp::Failed {
                        message: err.to_string(),
                    })
                    .unwrap_or_default(),
                );
                continue;
            }
        };
        let resp = dispatch(req);
        let payload = match serde_json::to_vec(&resp) {
            Ok(payload) => payload,
            Err(err) => serde_json::to_vec(&Resp::Failed {
                message: err.to_string(),
            })
            .unwrap_or_else(|_| br#"{"Failed":{"message":"encode"}}"#.to_vec()),
        };
        if write_frame(&mut frame_out, &payload).is_err() {
            break;
        }
    }
}

/// Keep the pipe the parent reads, and send `println!` (pdf-extract, lopdf) elsewhere.
///
/// A release GUI process has no stderr: `GetStdHandle(STD_ERROR_HANDLE)` is NULL.
/// `_dup2(2, 1)` is undefined in that case, so this never uses it. Stdout is
/// pointed at the real stderr handle, or at a `NUL` handle opened here.
#[cfg(windows)]
fn claim_frame_stdout() -> io::Result<FrameOut> {
    const STD_OUTPUT_HANDLE: u32 = 0xFFFF_FFF5;
    const STD_ERROR_HANDLE: u32 = 0xFFFF_FFF4;
    const O_BINARY: i32 = 0x8000;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_SHARE_WRITE: u32 = 0x0000_0002;
    const OPEN_EXISTING: u32 = 3;
    unsafe {
        let frame_fd = _dup(1);
        if frame_fd < 0 {
            return Err(io::Error::last_os_error());
        }
        _setmode(frame_fd, O_BINARY);
        _setmode(0, O_BINARY);
        let invalid = -1isize as *mut core::ffi::c_void;
        let stderr_handle = GetStdHandle(STD_ERROR_HANDLE);
        let sink = if stderr_handle.is_null() || stderr_handle == invalid {
            let mut name: Vec<u16> = "NUL".encode_utf16().collect();
            name.push(0);
            let nul = CreateFileW(
                name.as_ptr(),
                GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                core::ptr::null_mut(),
                OPEN_EXISTING,
                0,
                core::ptr::null_mut(),
            );
            if nul.is_null() || nul == invalid {
                let err = io::Error::last_os_error();
                _close(frame_fd);
                return Err(err);
            }
            // eprintln uses this handle. Leave it open for the process lifetime.
            SetStdHandle(STD_ERROR_HANDLE, nul);
            nul
        } else {
            stderr_handle
        };
        if SetStdHandle(STD_OUTPUT_HANDLE, sink) == 0 {
            let err = io::Error::last_os_error();
            _close(frame_fd);
            return Err(err);
        }
        Ok(FrameOut { fd: frame_fd })
    }
}

#[cfg(windows)]
struct FrameOut {
    fd: i32,
}

#[cfg(windows)]
impl Write for FrameOut {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let n = unsafe { _write(self.fd, buf.as_ptr().cast(), buf.len() as u32) };
        if n < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(n as usize)
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(windows)]
impl Drop for FrameOut {
    fn drop(&mut self) {
        unsafe {
            let _ = _close(self.fd);
        }
    }
}

#[cfg(windows)]
extern "system" {
    fn GetStdHandle(n_std_handle: u32) -> *mut core::ffi::c_void;
    fn SetStdHandle(n_std_handle: u32, handle: *mut core::ffi::c_void) -> i32;
    fn CreateFileW(
        file_name: *const u16,
        desired_access: u32,
        share_mode: u32,
        security_attributes: *mut core::ffi::c_void,
        creation_disposition: u32,
        flags_and_attributes: u32,
        template_file: *mut core::ffi::c_void,
    ) -> *mut core::ffi::c_void;
}

#[cfg(windows)]
extern "C" {
    fn _dup(fd: i32) -> i32;
    fn _setmode(fd: i32, mode: i32) -> i32;
    fn _write(fd: i32, buf: *const core::ffi::c_void, count: u32) -> i32;
    fn _close(fd: i32) -> i32;
}

fn dispatch(req: Req) -> Resp {
    match req {
        Req::Ping => Resp::Pong,
        Req::Stat { path } => match stat_path(Path::new(&path), child_block_name().as_deref()) {
            Ok(stat) => Resp::Stat {
                is_dir: stat.is_dir,
                is_file: stat.is_file,
                size: stat.size,
                mtime: stat.mtime,
            },
            Err(IoFail::Failed(message))
            | Err(IoFail::Crashed(message))
            | Err(IoFail::Unavailable(message)) => Resp::Failed { message },
            Err(IoFail::TimedOut) => Resp::Failed {
                message: "timed out".into(),
            },
        },
        Req::Extract { path, max_bytes } => {
            match extract_path(Path::new(&path), max_bytes, child_block_name().as_deref()) {
                Ok(loaded) => Resp::Extracted {
                    size: loaded.size,
                    mtime: loaded.mtime,
                    hash: loaded.hash,
                    outcome: match loaded.outcome {
                        ExtractOutcome::Text { title, pages } => WireOutcome::Text { title, pages },
                        ExtractOutcome::NoText => WireOutcome::NoText,
                        ExtractOutcome::TooBig => WireOutcome::TooBig,
                        ExtractOutcome::Failed(message) => WireOutcome::Failed(message),
                    },
                },
                Err(IoFail::Failed(message))
                | Err(IoFail::Crashed(message))
                | Err(IoFail::Unavailable(message)) => Resp::Failed { message },
                Err(IoFail::TimedOut) => Resp::Failed {
                    message: "timed out".into(),
                },
            }
        }
        Req::List { path } => match list_path(Path::new(&path), child_block_name().as_deref()) {
            Ok(entries) => Resp::List { entries },
            Err(IoFail::Failed(message))
            | Err(IoFail::Crashed(message))
            | Err(IoFail::Unavailable(message)) => Resp::Failed { message },
            Err(IoFail::TimedOut) => Resp::Failed {
                message: "timed out".into(),
            },
        },
    }
}

fn stat_path(path: &Path, block: Option<&str>) -> Result<FileStat, IoFail> {
    maybe_block(path, block);
    let meta = fs::metadata(path).map_err(|e| IoFail::Failed(e.to_string()))?;
    Ok(FileStat {
        is_dir: meta.is_dir(),
        is_file: meta.is_file(),
        size: meta.len(),
        mtime: mtime_secs(&meta),
    })
}

fn extract_path(path: &Path, max_bytes: u64, block: Option<&str>) -> Result<LoadedFile, IoFail> {
    maybe_block(path, block);
    let meta = fs::metadata(path).map_err(|e| IoFail::Failed(e.to_string()))?;
    let size = meta.len();
    let mtime = mtime_secs(&meta);
    if size > max_bytes {
        return Ok(LoadedFile {
            size,
            mtime,
            hash: String::new(),
            outcome: ExtractOutcome::TooBig,
        });
    }
    eprintln!("argos: reading {}", path.display());
    let bytes = fs::read(path).map_err(|e| IoFail::Failed(format!("read: {e}")))?;
    let hash = content_hash(&bytes);
    eprintln!("argos: extracting {}", path.display());
    let extracted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        extractor::extract_loaded(path, &bytes)
    }));
    let outcome = match extracted {
        Ok(Ok(doc)) => ExtractOutcome::Text {
            title: doc.title,
            pages: doc.pages,
        },
        Ok(Err(err)) if is_no_text(&err) => ExtractOutcome::NoText,
        Ok(Err(err)) => ExtractOutcome::Failed(err),
        Err(_) => ExtractOutcome::Failed("extract panicked".into()),
    };
    Ok(LoadedFile {
        size,
        mtime,
        hash,
        outcome,
    })
}

pub(crate) fn list_path(path: &Path, block: Option<&str>) -> Result<Vec<ListedEntry>, IoFail> {
    maybe_block(path, block);
    let rd = fs::read_dir(path).map_err(|e| IoFail::Failed(e.to_string()))?;
    let mut entries = Vec::new();
    for ent in rd {
        // `ReadDir::next` fails only when `FindNextFileW` fails. That is the
        // enumeration itself, not one bad name. A partial list would look complete
        // and the orphan purge would delete every file after the break.
        let ent = match ent {
            Ok(ent) => ent,
            Err(err) => {
                eprintln!(
                    "argos: directory listing failed under {}: {err}",
                    path.display()
                );
                return Err(IoFail::Failed(err.to_string()));
            }
        };
        let kind = match ent.file_type() {
            Ok(ft) if ft.is_symlink() => EntryKind::Symlink,
            Ok(ft) if ft.is_dir() => EntryKind::Dir,
            Ok(ft) if ft.is_file() => EntryKind::File,
            _ => EntryKind::Other,
        };
        entries.push(ListedEntry {
            name: ent.file_name().to_string_lossy().into_owned(),
            kind,
        });
    }
    Ok(entries)
}

fn child_block_name() -> Option<String> {
    // Release builds ignore the hook so a launched app cannot be stalled by the environment.
    #[cfg(debug_assertions)]
    {
        return std::env::var("ARGOS_EXTRACT_BLOCK")
            .ok()
            .filter(|name| !name.is_empty());
    }
    #[cfg(not(debug_assertions))]
    {
        None
    }
}

fn maybe_block(path: &Path, block: Option<&str>) {
    let Some(block) = block else {
        return;
    };
    #[cfg(not(debug_assertions))]
    {
        let _ = (path, block);
        return;
    }
    #[cfg(debug_assertions)]
    {
        if !blocked_name(path, block) {
            return;
        }
        let secs = std::env::var("ARGOS_EXTRACT_BLOCK_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(3);
        std::thread::sleep(Duration::from_secs(secs));
    }
}

/// `name*` matches a prefix. Used by tests that hang several files in one directory.
#[cfg(debug_assertions)]
fn blocked_name(path: &Path, block: &str) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    match block.strip_suffix('*') {
        Some(prefix) if !prefix.is_empty() => name.starts_with(prefix),
        _ => name == block,
    }
}

#[cfg(not(test))]
fn fail_text(err: IoFail) -> String {
    match err {
        IoFail::Failed(message) | IoFail::Crashed(message) | IoFail::Unavailable(message) => {
            message
        }
        IoFail::TimedOut => "timed out".into(),
    }
}

/// Kill `child` and wait at most two seconds. A child stuck in SMB I/O must not
/// hold the indexer on `wait`.
#[cfg(not(test))]
fn reap_child(child: &mut std::process::Child) -> bool {
    WORKER_DID_NOT_EXIT.store(false, Ordering::SeqCst);
    let _ = child.kill();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Err(_) => return true,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let exited = matches!(child.try_wait(), Ok(Some(_)));
                if !exited {
                    WORKER_DID_NOT_EXIT.store(true, Ordering::SeqCst);
                    eprintln!("argos: extractor worker did not exit after it was killed");
                }
                return exited;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

fn is_no_text(err: &str) -> bool {
    err == extractor::SKIP_NO_TEXT || err.contains("no extractable text")
}

fn mtime_secs(meta: &fs::Metadata) -> Option<u64> {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
}

fn read_frame(reader: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf)?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > FRAME_LIMIT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "extractor frame too large",
        ));
    }
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf)?;
    Ok(buf)
}

fn write_frame(writer: &mut impl Write, payload: &[u8]) -> io::Result<()> {
    if payload.len() > FRAME_LIMIT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "extractor frame too large",
        ));
    }
    writer.write_all(&(payload.len() as u32).to_le_bytes())?;
    writer.write_all(payload)?;
    writer.flush()
}

#[cfg(test)]
fn run_timeout<T: Send + 'static>(
    timeout: Duration,
    work: impl FnOnce() -> Result<T, IoFail> + Send + 'static,
) -> Result<T, IoFail> {
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = tx.send(work());
    });
    match rx.recv_timeout(timeout) {
        Ok(result) => result,
        Err(_) => Err(IoFail::TimedOut),
    }
}

/// Commit charge of this process, when the OS reports it. Used by the soak test.
pub fn committed_bytes() -> Option<u64> {
    #[cfg(windows)]
    {
        use std::mem::size_of;
        use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
        use windows::Win32::System::Threading::GetCurrentProcess;
        unsafe {
            let mut counters = PROCESS_MEMORY_COUNTERS::default();
            GetProcessMemoryInfo(
                GetCurrentProcess(),
                &mut counters,
                size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
            )
            .ok()?;
            Some(counters.PagefileUsage as u64)
        }
    }
    #[cfg(not(windows))]
    {
        None
    }
}
