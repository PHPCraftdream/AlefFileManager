// SPDX-License-Identifier: MIT OR Apache-2.0
use super::{
    record::{now_rfc3339, redact_fields, truncate_utf8},
    Level, LogRecord,
};
use alef_core::{AlefError, ErrorCode};
use serde_json::{Map, Value};
use std::{
    collections::HashMap,
    fs::{self, DirBuilder, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
    time::{Duration, Instant},
};

pub const MAX_READ_RECORDS: usize = 1000;
pub const MAX_CRASH_BYTES: usize = 64 * 1024;
const LOG_CAP: u64 = 1024 * 1024;
#[cfg(feature = "e2e")]
fn configured_cap() -> u64 {
    std::env::var("ALEF_E2E_LOG_CAP")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(|v| v.clamp(64 * 1024, 1024 * 1024))
        .unwrap_or(LOG_CAP)
}
#[cfg(not(feature = "e2e"))]
fn configured_cap() -> u64 {
    LOG_CAP
}
const READ_BYTES: usize = 5 * 1024 * 1024;
const REPLY_BYTES: usize = 192 * 1024;
const FIELD_CAP: usize = 8 * 1024;
const CRASH_COUNT: usize = 20;
const MSG_MARKER: &str = "...[truncated]";
const FIELDS_MARKER: &str = "fields dropped: serialized fields exceeded 8192 bytes";
type Shared = Arc<Locks>;
static REGISTRY: OnceLock<Mutex<HashMap<PathBuf, Weak<Locks>>>> = OnceLock::new();

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CrashInfo {
    pub id: String,
    pub time: String,
    pub size: u64,
}
#[derive(Clone, Debug)]
pub struct ReadOptions {
    pub limit: usize,
    pub level: Option<Level>,
}
impl Default for ReadOptions {
    fn default() -> Self {
        Self {
            limit: 100,
            level: None,
        }
    }
}
#[derive(Clone, Debug)]
pub struct ReadResult {
    pub records: Vec<LogRecord>,
    pub dropped_bytes: usize,
}
#[derive(Clone)]
pub struct LogStore {
    app_data: Arc<PathBuf>,
    locks: Shared,
    cap: u64,
}
struct Locks {
    log: Mutex<Rate>,
    crash: Mutex<()>,
}
struct Rate {
    window: Option<Instant>,
    count: u16,
    dropped: u64,
}

use std::sync::atomic::{AtomicU64, Ordering};
static EMERGENCY_NEXT: AtomicU64 = AtomicU64::new(0);

pub(super) fn emergency_save(crash_dir: &Path, text: &str) -> io::Result<()> {
    ensure_dir_io(crash_dir)?;
    let lock_path = crash_dir.join(".lock");
    ensure_file(&lock_path).map_err(|_| io::Error::other("unsafe crash lock file"))?;
    let mut lock_options = OpenOptions::new();
    lock_options
        .create(true)
        .read(true)
        .write(true)
        .truncate(false);
    private_open(&mut lock_options);
    let lock = lock_options.open(lock_path)?;
    lock.try_lock()?;
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let sequence = EMERGENCY_NEXT.fetch_add(1, Ordering::Relaxed);
    let id = format!("{ms:020}-{}-emergency-{sequence:016x}", std::process::id());
    let text = truncate_utf8(text, MAX_CRASH_BYTES, MSG_MARKER);
    let path = crash_dir.join(format!("{id}.txt"));
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    private_open(&mut options);
    let mut file = options.open(&path)?;
    let _ = private_file(&file);
    file.write_all(text.as_bytes())?;
    drop(file);
    let mut names: Vec<_> = fs::read_dir(crash_dir)?
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".txt"))
        .collect();
    names.sort();
    while names.len() > CRASH_COUNT {
        let name = names.remove(0);
        let old_path = crash_dir.join(name);
        ensure_file(&old_path).map_err(|_| io::Error::other("unsafe crash file"))?;
        fs::remove_file(old_path)?;
    }
    Ok(())
}
fn ensure_dir_io(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_dir() => Ok(()),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unsafe crash directory",
        )),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let mut builder = DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            match builder.create(path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    match fs::symlink_metadata(path) {
                        Ok(m) if m.file_type().is_dir() => Ok(()),
                        Ok(_) => Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "unsafe crash directory",
                        )),
                        Err(e) => Err(e),
                    }
                }
                Err(e) => Err(e),
            }
        }
        Err(e) => Err(e),
    }
}

impl LogStore {
    pub fn new(app_data: impl Into<PathBuf>) -> Result<Self, AlefError> {
        let app_data = app_data.into();
        let cap = configured_cap();
        Self::with_cap(app_data, cap)
    }
    #[cfg(test)]
    pub(super) fn with_test_cap(app_data: PathBuf, cap: u64) -> Result<Self, AlefError> {
        Self::with_cap(app_data, cap)
    }
    fn with_cap(app_data: PathBuf, cap: u64) -> Result<Self, AlefError> {
        let key = lexical_key(&app_data);
        let mut registry = REGISTRY
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        registry.retain(|_, weak| weak.strong_count() != 0);
        let locks = registry
            .get(&key)
            .and_then(Weak::upgrade)
            .unwrap_or_else(|| {
                let value = Arc::new(Locks {
                    log: Mutex::new(Rate {
                        window: None,
                        count: 0,
                        dropped: 0,
                    }),
                    crash: Mutex::new(()),
                });
                registry.insert(key, Arc::downgrade(&value));
                value
            });
        Ok(Self {
            app_data: Arc::new(app_data),
            locks,
            cap,
        })
    }
    pub(super) fn log_dir(&self) -> PathBuf {
        self.app_data.join("logs")
    }
    pub(super) fn crash_dir(&self) -> PathBuf {
        self.app_data.join("crashes")
    }
    pub(super) fn prepare(&self, crash: bool) -> Result<PathBuf, AlefError> {
        ensure_dir(self.app_data.as_path())?;
        let dir = if crash {
            self.crash_dir()
        } else {
            self.log_dir()
        };
        ensure_dir(&dir)?;
        Ok(dir)
    }
    pub fn write(&self, level: Level, message: &str, fields: Value) -> Result<bool, AlefError> {
        self.write_at(level, message, fields, Instant::now())
    }
    pub(super) fn write_at(
        &self,
        level: Level,
        message: &str,
        fields: Value,
        now: Instant,
    ) -> Result<bool, AlefError> {
        let mut rate = self.locks.log.lock().unwrap_or_else(|e| e.into_inner());
        if rate
            .window
            .is_none_or(|window| now.saturating_duration_since(window) >= Duration::from_secs(1))
        {
            rate.window = Some(now);
            rate.count = 0;
        }
        if rate.count >= 200 {
            rate.dropped = rate.dropped.saturating_add(1);
            return Ok(false);
        }
        let message = truncate_utf8(message, super::record::MAX_MESSAGE_BYTES, MSG_MARKER);
        let mut fields = match redact_fields(fields) {
            Value::Object(m) => m,
            _ => Map::new(),
        };
        if serde_json::to_vec(&fields).map_err(json_error)?.len() > FIELD_CAP {
            fields.clear();
            fields.insert("_dropped".into(), Value::String(FIELDS_MARKER.into()));
        }
        let record = LogRecord {
            time: now_rfc3339(),
            level,
            message,
            fields,
            dropped: (rate.dropped > 0).then_some(rate.dropped),
        };
        let mut line = serde_json::to_vec(&record).map_err(json_error)?;
        line.push(b'\n');
        if line.len() as u64 > self.cap {
            return Err(invalid("record exceeds log capacity"));
        }
        let dir = self.prepare(false)?;
        let path = dir.join("app.log");
        let _file_lock = process_lock(&dir)?;
        ensure_file(&path)?;
        if file_len(&path)?.saturating_add(line.len() as u64) > self.cap {
            rotate(&dir)?;
        }
        let mut options = OpenOptions::new();
        options.create(true).append(true);
        private_open(&mut options);
        let mut file = options.open(&path).map_err(io_error)?;
        private_file(&file)?;
        file.write_all(&line).map_err(io_error)?;
        rate.count += 1;
        rate.dropped = 0;
        Ok(true)
    }
    pub fn read(&self, options: ReadOptions) -> Result<ReadResult, AlefError> {
        if options.limit > MAX_READ_RECORDS {
            return Err(invalid("limit exceeds 1000"));
        }
        let dir = self.log_dir();
        let _guard = self.locks.log.lock().unwrap_or_else(|e| e.into_inner());
        if !dir.exists() {
            return Ok(ReadResult {
                records: vec![],
                dropped_bytes: 0,
            });
        }
        ensure_existing_dir(&dir)?;
        let _process_lock = process_lock(&dir)?;
        let mut newest = Vec::new();
        let mut reply_bytes = 2usize;
        let mut scanned = 0usize;
        let mut dropped_bytes = 0usize;
        if options.limit == 0 {
            return Ok(ReadResult {
                records: vec![],
                dropped_bytes: 0,
            });
        }
        'files: for n in 0..=4 {
            let path = dir.join(if n == 0 {
                "app.log".to_string()
            } else {
                format!("app.log.{n}")
            });
            match fs::symlink_metadata(&path) {
                Ok(m) if m.file_type().is_file() => (),
                Ok(_) => return Err(invalid("unsafe storage file")),
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(io_error(e)),
            }
            let remaining = READ_BYTES.saturating_sub(scanned);
            if remaining == 0 {
                break;
            }
            let f = File::open(path).map_err(io_error)?;
            let mut bytes = Vec::new();
            f.take(remaining as u64)
                .read_to_end(&mut bytes)
                .map_err(io_error)?;
            scanned += bytes.len();
            let truncated = bytes.len() == remaining;
            let complete = bytes.last() == Some(&b'\n');
            for (index, raw) in bytes.rsplit(|b| *b == b'\n').enumerate() {
                if raw.is_empty() || (index == 0 && !complete) {
                    continue;
                }
                if raw.len() > 64 * 1024 {
                    dropped_bytes = dropped_bytes.saturating_add(raw.len());
                    continue;
                }
                match serde_json::from_slice::<LogRecord>(raw) {
                    Ok(r) if options.level.is_none_or(|l| l == r.level) => {
                        let size = serde_json::to_vec(&r).map_err(json_error)?.len();
                        let added = size.saturating_add(usize::from(!newest.is_empty()));
                        if reply_bytes.saturating_add(added) > REPLY_BYTES {
                            break 'files;
                        }
                        reply_bytes += added;
                        newest.push(r);
                        if newest.len() == options.limit {
                            break 'files;
                        }
                    }
                    _ => dropped_bytes = dropped_bytes.saturating_add(raw.len()),
                }
            }
            if truncated {
                break;
            }
        }
        newest.reverse();
        Ok(ReadResult {
            records: newest,
            dropped_bytes,
        })
    }
    pub fn crashes(&self) -> Result<Vec<CrashInfo>, AlefError> {
        let _guard = self.locks.crash.lock().unwrap_or_else(|e| e.into_inner());
        let dir = self.crash_dir();
        if !dir.exists() {
            return Ok(vec![]);
        }
        ensure_existing_dir(&dir)?;
        let _process_lock = process_lock(&dir)?;
        let mut names = Vec::new();
        for item in fs::read_dir(&dir).map_err(io_error)? {
            let item = item.map_err(io_error)?;
            let name = item.file_name().to_string_lossy().into_owned();
            if let Some(id) = name.strip_suffix(".txt") {
                if valid_id(id) {
                    ensure_file(&item.path())?;
                    names.push(name);
                }
            }
        }
        names.sort();
        names
            .into_iter()
            .rev()
            .take(20)
            .map(|name| {
                let id = &name[..name.len() - 4];
                let size = fs::symlink_metadata(dir.join(&name))
                    .map_err(io_error)?
                    .len();
                let time = id
                    .split_once('-')
                    .and_then(|x| x.0.parse::<i64>().ok())
                    .map(|ms| alef_core::session::usage::timestamp(ms).replace(' ', "T"))
                    .unwrap_or_default();
                Ok(CrashInfo {
                    id: id.to_owned(),
                    time,
                    size,
                })
            })
            .collect()
    }
    pub fn crash(&self, id: &str) -> Result<String, AlefError> {
        if !valid_id(id) {
            return Err(invalid("invalid crash id"));
        }
        let _guard = self.locks.crash.lock().unwrap_or_else(|e| e.into_inner());
        let path = self.crash_dir().join(format!("{id}.txt"));
        ensure_existing_dir(&self.crash_dir())?;
        let _process_lock = process_lock(&self.crash_dir())?;
        ensure_file(&path)?;
        let f = File::open(path).map_err(|e| {
            if e.kind() == io::ErrorKind::NotFound {
                invalid("crash report not found")
            } else {
                io_error(e)
            }
        })?;
        let mut b = Vec::new();
        f.take((MAX_CRASH_BYTES + 1) as u64)
            .read_to_end(&mut b)
            .map_err(io_error)?;
        if b.len() > MAX_CRASH_BYTES {
            return Err(invalid("crash report exceeds 64 KiB"));
        }
        String::from_utf8(b).map_err(|_| invalid("crash report is not valid UTF-8"))
    }
    pub fn save_crash_auto(&self, text: &str) -> Result<CrashInfo, AlefError> {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let time = now_rfc3339();
        let id = format!(
            "{ms:020}-{}-{:016x}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let clean = truncate_utf8(text, MAX_CRASH_BYTES, "...[truncated]");
        self.save_crash(&id, &clean)?;
        Ok(CrashInfo {
            id,
            time,
            size: clean.len() as u64,
        })
    }
    pub fn save_crash(&self, id: &str, text: &str) -> Result<(), AlefError> {
        if !valid_id(id) {
            return Err(invalid("invalid crash id"));
        }
        let text = truncate_utf8(text, MAX_CRASH_BYTES, "...[truncated]");
        let _guard = self.locks.crash.lock().unwrap_or_else(|e| e.into_inner());
        let dir = self.prepare(true)?;
        let _file_lock = process_lock(&dir)?;
        let path = dir.join(format!("{id}.txt"));
        ensure_file(&path)?;
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        private_open(&mut options);
        let mut f = options.open(&path).map_err(io_error)?;
        private_file(&f)?;
        f.write_all(text.as_bytes()).map_err(io_error)?;
        let mut names: Vec<_> = fs::read_dir(&dir)
            .map_err(io_error)?
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".txt"))
            .collect();
        names.sort();
        while names.len() > CRASH_COUNT {
            let p = dir.join(names.remove(0));
            ensure_file(&p)?;
            fs::remove_file(p).map_err(io_error)?;
        }
        Ok(())
    }
}

fn lexical_key(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}
fn valid_id(id: &str) -> bool {
    let Some((stamp, rest)) = id.split_once('-') else {
        return false;
    };
    stamp.len() == 20
        && stamp.bytes().all(|b| b.is_ascii_digit())
        && !rest.is_empty()
        && rest.len() <= 48
        && rest
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
fn ensure_dir(p: &Path) -> Result<(), AlefError> {
    match fs::symlink_metadata(p) {
        Ok(m) if m.file_type().is_dir() => Ok(()),
        Ok(_) => Err(invalid("unsafe storage directory")),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let mut builder = DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            match builder.create(p) {
                Ok(()) => private_dir(p),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => ensure_existing_dir(p),
                Err(e) => Err(io_error(e)),
            }
        }
        Err(e) => Err(io_error(e)),
    }
}
fn ensure_existing_dir(p: &Path) -> Result<(), AlefError> {
    match fs::symlink_metadata(p) {
        Ok(m) if m.file_type().is_dir() => Ok(()),
        Ok(_) => Err(invalid("unsafe storage directory")),
        Err(e) => Err(io_error(e)),
    }
}
fn ensure_file(p: &Path) -> Result<(), AlefError> {
    match fs::symlink_metadata(p) {
        Ok(m) if m.file_type().is_file() => Ok(()),
        Ok(_) => Err(invalid("unsafe storage file")),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io_error(e)),
    }
}
fn file_len(p: &Path) -> Result<u64, AlefError> {
    match fs::symlink_metadata(p) {
        Ok(m) if m.file_type().is_file() => Ok(m.len()),
        Ok(_) => Err(invalid("unsafe storage file")),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(0),
        Err(e) => Err(io_error(e)),
    }
}
fn remove_if_present(p: &Path) -> Result<(), AlefError> {
    match fs::remove_file(p) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io_error(e)),
    }
}
fn rotate(dir: &Path) -> Result<(), AlefError> {
    for n in (1..=4).rev() {
        let from = dir.join(if n == 1 {
            "app.log".to_string()
        } else {
            format!("app.log.{}", n - 1)
        });
        let to = dir.join(format!("app.log.{n}"));
        ensure_file(&from)?;
        ensure_file(&to)?;
        remove_if_present(&to)?;
        if from.exists() {
            fs::rename(from, to).map_err(io_error)?;
        }
    }
    Ok(())
}
#[cfg(unix)]
fn private_file(f: &File) -> Result<(), AlefError> {
    use std::os::unix::fs::PermissionsExt;
    f.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(io_error)
}
#[cfg(not(unix))]
fn private_file(_: &File) -> Result<(), AlefError> {
    Ok(())
}
#[cfg(unix)]
fn private_dir(p: &Path) -> Result<(), AlefError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(p, fs::Permissions::from_mode(0o700)).map_err(io_error)
}
#[cfg(not(unix))]
fn private_dir(_: &Path) -> Result<(), AlefError> {
    Ok(())
}
#[cfg(unix)]
fn private_open(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(0o600);
}
#[cfg(not(unix))]
fn private_open(_: &mut OpenOptions) {}
// In-process instances share locks; hostile concurrent filesystem replacement is not race-proof.
fn process_lock(dir: &Path) -> Result<File, AlefError> {
    let path = dir.join(".lock");
    ensure_file(&path)?;
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true).truncate(false);
    private_open(&mut options);
    let file = options.open(path).map_err(io_error)?;
    file.lock().map_err(io_error)?;
    Ok(file)
}
fn invalid(s: &str) -> AlefError {
    AlefError::new(ErrorCode::InvalidArgument, s)
}
fn io_error(_: io::Error) -> AlefError {
    AlefError::new(ErrorCode::Internal, "log storage operation failed")
}
fn json_error(_: serde_json::Error) -> AlefError {
    AlefError::new(ErrorCode::Internal, "log record serialization failed")
}
