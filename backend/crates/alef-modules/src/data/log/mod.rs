//! Private, bounded application log and crash-report storage. All filesystem methods are synchronous;
//! callers must run them on the blocking executor, never a runtime worker.
mod commands;
mod crash;
mod record;
mod storage;
#[cfg(test)]
mod tests;

pub(crate) use commands::register;
pub use crash::{install_panic_hook, CrashReport, RuntimeInfo};
pub use record::{Level, LogRecord};
pub use storage::{
    CrashInfo, LogStore, ReadOptions, ReadResult, MAX_CRASH_BYTES, MAX_READ_RECORDS,
};
