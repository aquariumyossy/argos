//! File listing, stat, and text extraction.
//!
//! In the app this runs in a child process (`argos.exe --extract-worker`) under a
//! Windows job memory limit. A stack overflow, abort, or runaway allocation kills
//! the child; the indexer records that path and continues. Unit tests run the same
//! functions in-process, with a timeout thread, because `cargo test` is not the app exe.

use std::fs;
use std::io::{self, Read, Write};
use std::path::Path;
use std::time::{Duration, SystemTime};

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
}

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
    #[cfg(not(test))]
    worker: Option<Worker>,
    #[cfg(not(test))]
    broken: bool,
}

impl ExtractHost {
    pub fn new() -> Self {
        Self {
            #[cfg(test)]
            block_name: None,
            #[cfg(not(test))]
            worker: None,
            #[cfg(not(test))]
            broken: false,
        }
    }

    #[cfg(test)]
    pub fn set_block_name(&mut self, name: Option<String>) {
        self.block_name = name;
    }

    pub fn stat(&mut self, path: &Path, timeout: Duration) -> Result<FileStat, IoFail> {
        #[cfg(test)]
        {
            let path = path.to_path_buf();
            let block = self.block_name.clone();
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
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
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
        if self.broken {
            return Err(IoFail::Failed(
                "extractor worker could not be started".into(),
            ));
        }
        let worker = match spawn_worker() {
            Ok(worker) => worker,
            Err(err) => {
                self.broken = true;
                eprintln!("argos: extractor worker failed to start: {err}");
                return Err(IoFail::Failed(err));
            }
        };
        self.worker = Some(worker);
        match self.roundtrip(&Req::Ping, Duration::from_secs(10)) {
            Ok(Resp::Pong) => Ok(()),
            Ok(_) => {
                self.stop_worker();
                self.broken = true;
                Err(IoFail::Failed("extractor worker did not answer ping".into()))
            }
            Err(err) => {
                self.stop_worker();
                self.broken = true;
                Err(err)
            }
        }
    }

    fn stop_worker(&mut self) {
        self.worker = None;
    }
}

#[cfg(not(test))]
fn spawn_worker() -> Result<Worker, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut child = std::process::Command::new(exe)
        .arg("--extract-worker")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
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
    unsafe {
        // Length-prefixed frames must not be rewritten as text.
        extern "system" {
            fn _setmode(fd: i32, mode: i32) -> i32;
        }
        const O_BINARY: i32 = 0x8000;
        _setmode(0, O_BINARY);
        _setmode(1, O_BINARY);
    }
    let stdin = io::stdin();
    let mut stdin = stdin.lock();
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    loop {
        let frame = match read_frame(&mut stdin) {
            Ok(frame) => frame,
            Err(_) => break,
        };
        let req: Req = match serde_json::from_slice(&frame) {
            Ok(req) => req,
            Err(err) => {
                let _ = write_frame(
                    &mut stdout,
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
        if write_frame(&mut stdout, &payload).is_err() {
            break;
        }
    }
}

fn dispatch(req: Req) -> Resp {
    match req {
        Req::Ping => Resp::Pong,
        Req::Stat { path } => match stat_path(Path::new(&path), None) {
            Ok(stat) => Resp::Stat {
                is_dir: stat.is_dir,
                is_file: stat.is_file,
                size: stat.size,
                mtime: stat.mtime,
            },
            Err(IoFail::Failed(message)) | Err(IoFail::Crashed(message)) => {
                Resp::Failed { message }
            }
            Err(IoFail::TimedOut) => Resp::Failed {
                message: "timed out".into(),
            },
        },
        Req::Extract { path, max_bytes } => {
            match extract_path(Path::new(&path), max_bytes, None) {
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
                Err(IoFail::Failed(message)) | Err(IoFail::Crashed(message)) => {
                    Resp::Failed { message }
                }
                Err(IoFail::TimedOut) => Resp::Failed {
                    message: "timed out".into(),
                },
            }
        }
        Req::List { path } => match list_path(Path::new(&path), None) {
            Ok(entries) => Resp::List { entries },
            Err(IoFail::Failed(message)) | Err(IoFail::Crashed(message)) => {
                Resp::Failed { message }
            }
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

fn list_path(path: &Path, block: Option<&str>) -> Result<Vec<ListedEntry>, IoFail> {
    maybe_block(path, block);
    let rd = fs::read_dir(path).map_err(|e| IoFail::Failed(e.to_string()))?;
    let mut entries = Vec::new();
    for ent in rd {
        let ent = ent.map_err(|e| IoFail::Failed(e.to_string()))?;
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

fn maybe_block(path: &Path, block: Option<&str>) {
    if block.is_some_and(|name| path.file_name().and_then(|n| n.to_str()) == Some(name)) {
        std::thread::sleep(Duration::from_secs(3));
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
