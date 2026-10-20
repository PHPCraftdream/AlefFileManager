use super::record::truncate_utf8;
use super::storage::{emergency_save, MAX_CRASH_BYTES};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::OnceLock;

type Hook = Box<dyn for<'a> Fn(&std::panic::PanicHookInfo<'a>) + Send + Sync + 'static>;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RuntimeInfo {
    pub app_id: String,
    pub version: String,
    pub runtime: String,
    pub os: String,
    pub arch: String,
    pub home: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CrashReport {
    pub id: String,
    pub message: String,
    pub location: Option<String>,
    pub backtrace: String,
    pub runtime: RuntimeInfo,
}
impl CrashReport {
    pub fn build(&self) -> String {
        self.build_with_environment(&[])
    }

    pub fn build_with_environment(&self, _environment: &[(String, String)]) -> String {
        let home = self.runtime.home.as_deref();
        let sanitize = |value: &str| {
            let value = if let Some(home) = home.filter(|h| !h.is_empty()) {
                value.replace(home, "~")
            } else {
                value.to_owned()
            };
            truncate_utf8(&value, MAX_CRASH_BYTES, "...[truncated]")
        };
        let location = self
            .location
            .as_deref()
            .map(&sanitize)
            .unwrap_or_else(|| "unknown".into());
        let text = format!("id: {}\ntime: {}\napp: {}\nversion: {}\nruntime: {}\nos: {}\narch: {}\nmessage: {}\nlocation: {}\nbacktrace:\n{}\n",
            self.id, super::record::now_rfc3339(), sanitize(&self.runtime.app_id), sanitize(&self.runtime.version),
            sanitize(&self.runtime.runtime), sanitize(&self.runtime.os), sanitize(&self.runtime.arch), sanitize(&self.message), location, sanitize(&self.backtrace));
        truncate_utf8(&text, MAX_CRASH_BYTES, "...[truncated]")
    }
}

struct ActiveGuard<'a>(&'a AtomicBool);
impl Drop for ActiveGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

pub(super) fn make_panic_hook(folder: PathBuf, metadata: RuntimeInfo, previous: Hook) -> Hook {
    let active = AtomicBool::new(false);
    let window = AtomicU64::new(0);
    let count = AtomicU64::new(0);
    Box::new(move |info| {
        let guard = active
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| ActiveGuard(&active));
        if guard.is_some() {
            let second = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            if window.load(Ordering::Relaxed) != second {
                window.store(second, Ordering::Relaxed);
                count.store(0, Ordering::Relaxed);
            }
            let current = count.load(Ordering::Relaxed);
            if current < 2 {
                count.store(current + 1, Ordering::Relaxed);
                let message = info
                    .payload()
                    .downcast_ref::<String>()
                    .map(String::as_str)
                    .or_else(|| info.payload().downcast_ref::<&str>().copied())
                    .unwrap_or("non-string panic");
                let location = info
                    .location()
                    .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()));
                let report = CrashReport {
                    id: String::new(),
                    message: message.to_owned(),
                    location,
                    backtrace: std::backtrace::Backtrace::force_capture().to_string(),
                    runtime: metadata.clone(),
                };
                let _ = emergency_save(&folder, &report.build());
            }
        }
        previous(info);
        drop(guard);
    })
}

static INSTALLED: OnceLock<()> = OnceLock::new();

/// Installs a process-wide synchronous emergency writer. Ordinary store locks/executor are not used.
pub fn install_panic_hook(context: &crate::ModuleContext) -> Result<(), alef_core::AlefError> {
    let paths = context.effective_paths()?;
    let folder = paths.app_data.join("crashes");
    let metadata = RuntimeInfo {
        app_id: context.app.id.clone(),
        version: context.app.version.clone(),
        runtime: context.app.runtime_version.clone(),
        os: std::env::consts::OS.to_owned(),
        arch: std::env::consts::ARCH.to_owned(),
        home: Some(context.paths.home.to_string_lossy().into_owned()),
    };
    INSTALLED.get_or_init(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(make_panic_hook(folder, metadata, previous));
    });
    Ok(())
}
