// SPDX-License-Identifier: MIT OR Apache-2.0
//! The user's per-application usage of AI: the daily quota the user sets and the append-only
//! journal of what the AI of the application did. The state lives in a folder of the runtime no
//! application reaches; the folder comes from the caller, one per application authority.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// The version of the JSON state files.
const VERSION: u32 = 1;

/// Unique temp-file suffixes for concurrent persists inside this process.
static TEMP_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// What the user allows the AI of one application per local day.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Limits {
    /// Upper bound on inference requests per day; `None` is unlimited.
    pub requests: Option<u64>,
    /// Upper bound on tokens per day; `None` is unlimited.
    pub tokens: Option<u64>,
}

impl Limits {
    /// No limits at all.
    pub fn none() -> Self {
        Self {
            requests: None,
            tokens: None,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct SavedLimits {
    version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    requests: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tokens: Option<u64>,
}

/// The usage counters of one day.
#[derive(Serialize, Deserialize)]
struct Counters {
    version: u32,
    day: String,
    stamp: i64,
    requests: u64,
    tokens: u64,
}

/// Why an attempt could not reserve its share.
#[derive(Debug)]
pub enum Quota {
    /// The user's daily bound for this application is used up.
    Exceeded,
    /// The state could not be read or written; fail closed.
    Io(io::Error),
}

/// What one attempt of the authority reserved. There are no refunds: it is kept whatever the
/// attempt does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reserved {
    pub requests: u64,
    pub tokens: u64,
}

/// Where the limits and the counters of one application authority are kept.
#[derive(Debug, Clone)]
pub struct Store {
    folder: PathBuf,
}

impl Store {
    pub fn new(folder: impl Into<PathBuf>) -> Self {
        Self {
            folder: folder.into(),
        }
    }

    fn limits_path(&self) -> PathBuf {
        self.folder.join("limits.json")
    }

    fn usage_path(&self) -> PathBuf {
        self.folder.join("usage.json")
    }

    /// The limits the user set; a missing, damaged, or wrong-version file is no limit: a quota
    /// nobody can read never makes the runtime stricter than the user asked. The counters file is
    /// the fail-closed one.
    pub fn limits(&self) -> io::Result<Limits> {
        let bytes = match std::fs::read(self.limits_path()) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Limits::none()),
            Err(error) => return Err(error),
        };
        let Ok(saved) = serde_json::from_slice::<SavedLimits>(&bytes) else {
            return Ok(Limits::none());
        };
        if saved.version != VERSION {
            return Ok(Limits::none());
        }
        Ok(Limits {
            requests: saved.requests,
            tokens: saved.tokens,
        })
    }

    /// Replaces the limits, atomically (`.part` write, then rename).
    pub fn set_limits(&self, limits: &Limits) -> io::Result<()> {
        let saved = SavedLimits {
            version: VERSION,
            requests: limits.requests,
            tokens: limits.tokens,
        };
        let text = serde_json::to_vec(&saved).map_err(io::Error::other)?;
        std::fs::create_dir_all(&self.folder)?;
        let path = self.limits_path();
        let partial = path.with_extension("json.part");
        std::fs::write(&partial, text)?;
        std::fs::rename(&partial, &path)
    }

    /// Reserves one inference attempt at the current time.
    pub fn reserve(&self, token_bound: Option<u64>) -> Result<Reserved, Quota> {
        self.reserve_at(token_bound, now_ms())
    }

    /// Reserves one inference attempt: one request plus, when the caller has a token upper bound,
    /// that many tokens, against the user's daily limits. Kept public so tests of other crates can
    /// drive it with a fixed clock.
    pub fn reserve_at(&self, token_bound: Option<u64>, now_ms: i64) -> Result<Reserved, Quota> {
        let lock = lock_exclusive(&self.folder, "usage.lock").map_err(Quota::Io)?;
        let result = self.reserve_locked(token_bound, now_ms);
        drop(lock);
        result
    }

    /// The critical section of `reserve_at`; the caller holds the usage lock.
    fn reserve_locked(&self, token_bound: Option<u64>, now_ms: i64) -> Result<Reserved, Quota> {
        let limits = self.limits().map_err(Quota::Io)?;
        let mut counters = self
            .read_counters(now_ms)
            .map_err(|error| Quota::Io(io::Error::other(error)))?;
        // The day rolls only when the calendar key changed AND the clock did not go back: a clock
        // set back keeps the old day under its old key, so a day is never handed out twice.
        if counters.day != day_key_now(now_ms) && now_ms >= counters.stamp {
            counters.day = day_key_now(now_ms);
            counters.requests = 0;
            counters.tokens = 0;
        }
        if let Some(limit) = limits.requests {
            if counters.requests.saturating_add(1) > limit {
                self.stamp_without_spending(&counters, now_ms)?;
                return Err(Quota::Exceeded);
            }
        }
        if let Some(limit) = limits.tokens {
            // Fail closed: an unknown token upper bound never starts an attempt.
            let bound = match token_bound {
                Some(bound) => bound,
                None => {
                    self.stamp_without_spending(&counters, now_ms)?;
                    return Err(Quota::Exceeded);
                }
            };
            if counters.tokens.saturating_add(bound) > limit {
                self.stamp_without_spending(&counters, now_ms)?;
                return Err(Quota::Exceeded);
            }
        }
        counters.requests = counters.requests.saturating_add(1);
        counters.tokens = counters.tokens.saturating_add(token_bound.unwrap_or(0));
        counters.stamp = counters.stamp.max(now_ms);
        self.persist(&counters)?;
        Ok(Reserved {
            requests: 1,
            tokens: token_bound.unwrap_or(0),
        })
    }

    /// Records the observed time without spending: a later day is never handed out again even when
    /// the attempt was rejected. A read failure here fails closed already, so nothing to do.
    fn stamp_without_spending(&self, counters: &Counters, now_ms: i64) -> Result<(), Quota> {
        let stamped = Counters {
            version: counters.version,
            day: counters.day.clone(),
            stamp: counters.stamp.max(now_ms),
            requests: counters.requests,
            tokens: counters.tokens,
        };
        self.persist(&stamped)
    }

    /// Writes the counters atomically (`.part` write, then rename).
    fn persist(&self, counters: &Counters) -> Result<(), Quota> {
        let saved = Counters {
            version: VERSION,
            day: counters.day.clone(),
            stamp: counters.stamp,
            requests: counters.requests,
            tokens: counters.tokens,
        };
        let text =
            serde_json::to_vec(&saved).map_err(|error| Quota::Io(io::Error::other(error)))?;
        std::fs::create_dir_all(&self.folder).map_err(Quota::Io)?;
        let path = self.usage_path();
        let unique = format!(
            "{}.{}.part",
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let partial = path.with_extension(unique);
        std::fs::write(&partial, text)
            .and_then(|()| std::fs::rename(&partial, &path))
            .map_err(Quota::Io)
    }

    /// The counters of the store; a missing file is fresh counters at the caller's time, a damaged
    /// or wrong-version file is an error (hidden usage must not read as fresh).
    fn read_counters(&self, now_ms: i64) -> io::Result<Counters> {
        let bytes = match std::fs::read(self.usage_path()) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(Counters {
                    version: VERSION,
                    day: day_key_now(now_ms),
                    stamp: now_ms,
                    requests: 0,
                    tokens: 0,
                });
            }
            Err(error) => return Err(error),
        };
        let counters: Counters = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
        if counters.version != VERSION {
            return Err(io::Error::other("usage.json of another version"));
        }
        Ok(counters)
    }
}

/// Unix milliseconds; a clock before the epoch saturates at zero instead of panicking.
pub fn now_ms() -> i64 {
    // A duration this small always fits: cast is deliberate and saturating by construction.
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// The calendar day `YYYY-MM-DD` of `unix_ms + offset_seconds * 1000`. The offset is a parameter
/// so the quota math stays offset-parametric and testable; callers go through `day_key_now`.
fn day_key(unix_ms: i64, utc_offset_seconds: i32) -> String {
    let days = (unix_ms + i64::from(utc_offset_seconds) * 1000).div_euclid(86_400_000);
    let (year, m, d) = civil(days);
    format!("{year:04}-{m:02}-{d:02}")
}

/// The moment as `YYYY-MM-DD HH:MM:SSZ` (UTC), for the journal of the user.
pub fn timestamp(unix_ms: i64) -> String {
    let seconds = unix_ms.div_euclid(1000);
    let days = seconds.div_euclid(86_400);
    let (year, m, d) = civil(days);
    let sod = seconds.rem_euclid(86_400);
    let (h, mi, s) = (sod / 3600, (sod % 3600) / 60, sod % 60);
    format!("{year:04}-{m:02}-{d:02} {h:02}:{mi:02}:{s:02}Z")
}

/// Days since 1970-01-01 as a proleptic Gregorian date; shared by `day_key` and `timestamp`, so
/// the two never disagree.
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    (year, m as u32, d as u32)
}

/// The user's UTC offset in seconds, right now (chrono reads the system timezone).
pub fn local_utc_offset_seconds() -> i32 {
    chrono::Local::now().offset().local_minus_utc()
}

/// The calendar day of `unix_ms` in the user's local timezone; the only way callers get a day key.
fn day_key_now(unix_ms: i64) -> String {
    day_key(unix_ms, local_utc_offset_seconds())
}

/// An exclusive, blocking lock on `folder/name`; the lock lasts until the file is dropped.
fn lock_exclusive(folder: &Path, name: &str) -> io::Result<std::fs::File> {
    std::fs::create_dir_all(folder)?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .open(folder.join(name))?;
    file.lock()?;
    Ok(file)
}

/// Bound on the size of `journal.log` before it rotates.
pub const MAX_JOURNAL_BYTES: u64 = 256 * 1024;

/// What kind of thing the AI of the application did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Tool,
    Builtin,
}

/// Where in the attempt the entry sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Event {
    Start,
    End,
}

/// One line of the journal: who, what, when, how it ended — and nothing else. No message
/// contents, no arguments, no error text.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    /// Unix milliseconds; the CLI formats it.
    pub at: i64,
    pub app: String,
    pub role: String,
    pub kind: Kind,
    pub name: String,
    pub event: Event,
    /// `"ok"`/`"error"`/`"cancelled"`, on `End` only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
}

/// The append-only journal of one application authority: `journal.log`, one JSON object per line.
#[derive(Debug, Clone)]
pub struct Journal {
    path: PathBuf,
}

impl Journal {
    pub fn new(folder: impl Into<PathBuf>) -> Self {
        Self {
            path: folder.into().join("journal.log"),
        }
    }

    /// Appends one entry, rotating first when the log has reached its bound: the full log moves to
    /// `journal.log.1`, replacing any older one.
    pub fn append(&self, entry: &Entry) -> io::Result<()> {
        let parent = self.path.parent().unwrap_or_else(|| Path::new(""));
        let lock = lock_exclusive(parent, "journal.lock")?;
        let result = self.append_locked(entry);
        drop(lock);
        result
    }

    /// The critical section of `append`; the caller holds the journal lock.
    fn append_locked(&self, entry: &Entry) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if let Ok(meta) = std::fs::metadata(&self.path) {
            if meta.len() >= MAX_JOURNAL_BYTES {
                let old = self.path.with_extension("log.1");
                std::fs::rename(&self.path, old)?;
            }
        }
        let line = serde_json::to_string(entry).map_err(io::Error::other)?;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        use std::io::Write as _;
        writeln!(file, "{line}")
    }

    /// The raw journal lines for the CLI: the older `journal.log.1` first, then `journal.log`.
    /// Missing files are no lines.
    pub fn read(folder: &Path) -> io::Result<Vec<String>> {
        let mut lines = Vec::new();
        for name in ["journal.log.1", "journal.log"] {
            match std::fs::read_to_string(folder.join(name)) {
                Ok(text) => lines.extend(text.lines().map(str::to_owned)),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        Ok(lines)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// 2026-01-02 12:00:00 UTC.
    const JAN2_NOON: i64 = 1_767_355_200_000;
    /// 2026-01-01 08:00:00 UTC.
    const JAN1_MORNING: i64 = 1_767_254_400_000;
    /// 2026-01-03 08:00:00 UTC.
    const JAN3_MORNING: i64 = 1_767_427_200_000;
    /// 2026-01-04 08:00:00 UTC.
    const JAN4_MORNING: i64 = JAN3_MORNING + 86_400_000;
    /// 2026-01-05 08:00:00 UTC.
    const JAN5_MORNING: i64 = JAN4_MORNING + 86_400_000;

    #[test]
    fn the_day_key_names_the_calendar_day_of_the_moment() {
        assert_eq!(day_key(0, 0), "1970-01-01");
        assert_eq!(day_key(86_400_000, 0), "1970-01-02");
        assert_eq!(day_key(-1, 0), "1969-12-31");
        // One hour east crosses midnight into the next day.
        assert_eq!(day_key(86_340_000, 3600), "1970-01-02");
    }

    #[test]
    fn a_timestamp_formats_the_utc_moment() {
        assert_eq!(timestamp(0), "1970-01-01 00:00:00Z");
        assert_eq!(timestamp(86_400_000), "1970-01-02 00:00:00Z");
        assert_eq!(timestamp(-1000), "1969-12-31 23:59:59Z");
    }

    #[test]
    fn a_request_limit_stops_the_attempt_after_the_last_one() {
        let dir = tempdir().unwrap();
        let store = Store::new(dir.path());
        store
            .set_limits(&Limits {
                requests: Some(2),
                tokens: None,
            })
            .unwrap();
        assert!(store.reserve_at(None, JAN2_NOON).is_ok());
        assert!(store.reserve_at(None, JAN2_NOON + 1).is_ok());
        match store.reserve_at(None, JAN2_NOON + 2) {
            Err(Quota::Exceeded) => {}
            other => panic!("expected Exceeded, got {other:?}"),
        }
    }

    #[test]
    fn a_token_limit_without_an_upper_bound_fails_closed() {
        let dir = tempdir().unwrap();
        let store = Store::new(dir.path());
        store
            .set_limits(&Limits {
                requests: None,
                tokens: Some(100),
            })
            .unwrap();
        match store.reserve_at(None, JAN2_NOON) {
            Err(Quota::Exceeded) => {}
            other => panic!("expected Exceeded, got {other:?}"),
        }
        // Without a token limit an unknown bound is fine.
        let free = Store::new(dir.path().join("free"));
        assert!(free.reserve_at(None, JAN2_NOON).is_ok());
    }

    #[test]
    fn a_token_limit_reserves_the_bound_the_provider_names() {
        let dir = tempdir().unwrap();
        let store = Store::new(dir.path());
        store
            .set_limits(&Limits {
                requests: None,
                tokens: Some(100),
            })
            .unwrap();
        assert!(store.reserve_at(Some(60), JAN2_NOON).is_ok());
        match store.reserve_at(Some(60), JAN2_NOON + 1) {
            Err(Quota::Exceeded) => {}
            other => panic!("expected Exceeded, got {other:?}"),
        }
        assert!(store.reserve_at(Some(40), JAN2_NOON + 2).is_ok());
    }

    #[test]
    fn the_counters_rollover_once_per_calendar_day_and_never_when_the_clock_goes_back() {
        let dir = tempdir().unwrap();
        let store = Store::new(dir.path());
        store
            .set_limits(&Limits {
                requests: Some(1),
                tokens: None,
            })
            .unwrap();
        assert!(store.reserve_at(None, JAN2_NOON).is_ok());
        // Earlier milliseconds, different day key: the clock went back, the day does not refill.
        match store.reserve_at(None, JAN1_MORNING) {
            Err(Quota::Exceeded) => {}
            other => panic!("expected Exceeded, got {other:?}"),
        }
        // Later milliseconds, different day key: the day refills once.
        assert!(store.reserve_at(None, JAN3_MORNING).is_ok());
    }

    #[test]
    fn a_damaged_usage_file_fails_closed() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("usage.json"), b"not json").unwrap();
        let store = Store::new(dir.path());
        match store.reserve_at(None, JAN2_NOON) {
            Err(Quota::Io(_)) => {}
            other => panic!("expected Io, got {other:?}"),
        }
    }

    #[test]
    fn a_missing_or_damaged_limits_file_means_no_limit() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(dir.path().join("limits.json"), b"garbage").unwrap();
        let store = Store::new(dir.path());
        assert!(store.reserve_at(None, JAN2_NOON).is_ok());
    }

    #[test]
    fn an_intervening_day_is_never_granted_once_a_later_day_was_observed() {
        let dir = tempdir().unwrap();
        let store = Store::new(dir.path());
        store
            .set_limits(&Limits {
                requests: Some(1),
                tokens: None,
            })
            .unwrap();
        assert!(store.reserve_at(None, JAN2_NOON).is_ok());
        // A later day rolls and spends the day.
        assert!(store.reserve_at(None, JAN4_MORNING).is_ok());
        // An earlier, intervening day is refused, and the refusal itself stamps the file.
        match store.reserve_at(None, JAN3_MORNING) {
            Err(Quota::Exceeded) => {}
            other => panic!("expected Exceeded, got {other:?}"),
        }
        match store.reserve_at(None, JAN3_MORNING) {
            Err(Quota::Exceeded) => {}
            other => panic!("expected Exceeded again, got {other:?}"),
        }
        // A still later day rolls again.
        assert!(store.reserve_at(None, JAN5_MORNING).is_ok());
        let text = std::fs::read_to_string(dir.path().join("usage.json")).unwrap();
        let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(saved["requests"], 1);
        assert_eq!(saved["stamp"], JAN5_MORNING);
    }

    #[test]
    fn a_rejected_attempt_on_a_later_day_stamps_the_file_so_the_clock_cannot_go_back() {
        let dir = tempdir().unwrap();
        let store = Store::new(dir.path());
        store
            .set_limits(&Limits {
                requests: None,
                tokens: Some(100),
            })
            .unwrap();
        // Jan3 spends the whole token limit of its day.
        assert!(store.reserve_at(Some(100), JAN3_MORNING).is_ok());
        // Jan5 rolls to a fresh day, but an unknown token bound fails closed: the attempt is
        // rejected before spending, and the rejection still stamps Jan5.
        match store.reserve_at(None, JAN5_MORNING) {
            Err(Quota::Exceeded) => {}
            other => panic!("expected Exceeded, got {other:?}"),
        }
        // The clock goes back to Jan4: now < stamp(Jan5), so no roll — the attempt is charged to
        // the quota of Jan5 and Jan4 does not get a fresh one.
        assert!(store.reserve_at(Some(100), JAN4_MORNING).is_ok());
        // So Jan5 itself has nothing left. Without the stamp of the rejection Jan4 would have
        // rolled a day of its own and Jan5 would have been handed out a second time.
        match store.reserve_at(Some(100), JAN5_MORNING) {
            Err(Quota::Exceeded) => {}
            other => panic!("expected Exceeded, got {other:?}"),
        }
        let text = std::fs::read_to_string(dir.path().join("usage.json")).unwrap();
        let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(saved["tokens"], 100);
        assert_eq!(saved["stamp"], JAN5_MORNING);
    }

    #[test]
    fn parallel_reservations_against_a_limit_admit_exactly_the_limit() {
        let dir = tempdir().unwrap();
        let store = std::sync::Arc::new(Store::new(dir.path()));
        store
            .set_limits(&Limits {
                requests: Some(4),
                tokens: None,
            })
            .unwrap();
        // One clock reading for every thread: no midnight straddle between workers.
        let now = now_ms();
        let workers: Vec<_> = (0..12)
            .map(|_| {
                let store = store.clone();
                std::thread::spawn(move || store.reserve_at(None, now))
            })
            .collect();
        let successes = workers
            .into_iter()
            .map(|worker| worker.join().expect("worker joined"))
            .filter(Result::is_ok)
            .count();
        assert_eq!(successes, 4);
        let text = std::fs::read_to_string(dir.path().join("usage.json")).unwrap();
        let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(saved["requests"], 4);
    }

    #[test]
    fn journal_appends_are_not_lost_across_a_rotation() {
        let dir = tempdir().unwrap();
        let journal = std::sync::Arc::new(Journal::new(dir.path()));
        // About 1.5 KiB per entry, so the 200 entries cross MAX_JOURNAL_BYTES exactly once.
        let role = "x".repeat(1536);
        let workers: Vec<_> = (0..8)
            .map(|thread| {
                let journal = journal.clone();
                let role = role.clone();
                std::thread::spawn(move || {
                    for index in 0..25 {
                        journal
                            .append(&Entry {
                                at: (thread * 1000 + index) as i64,
                                app: "app".into(),
                                role: role.clone(),
                                kind: Kind::Tool,
                                name: "t".into(),
                                event: Event::Start,
                                outcome: None,
                            })
                            .unwrap();
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        let lines = Journal::read(dir.path()).unwrap();
        assert_eq!(lines.len(), 200);
        for line in &lines {
            serde_json::from_str::<Entry>(line).unwrap();
        }
        assert!(dir.path().join("journal.log.1").exists());
    }

    #[test]
    fn the_journal_rotates_at_its_bound_and_keeps_the_old_lines_readable() {
        let dir = tempdir().unwrap();
        let journal = Journal::new(dir.path());
        std::fs::create_dir_all(dir.path()).unwrap();
        let old_line =
            r#"{"at":1,"app":"old","role":"user","kind":"tool","name":"t","event":"start"}"#;
        let mut text = old_line.to_owned();
        while text.len() < MAX_JOURNAL_BYTES as usize {
            text.push(' ');
        }
        std::fs::write(dir.path().join("journal.log"), text).unwrap();
        let entry = Entry {
            at: 2,
            app: "new".into(),
            role: "user".into(),
            kind: Kind::Tool,
            name: "t".into(),
            event: Event::Start,
            outcome: None,
        };
        journal.append(&entry).unwrap();
        assert!(dir.path().join("journal.log.1").exists());
        let fresh = std::fs::read_to_string(dir.path().join("journal.log")).unwrap();
        assert_eq!(fresh.lines().count(), 1);
        let lines = Journal::read(dir.path()).unwrap();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains(r#""app":"old""#));
        assert!(lines[1].contains(r#""app":"new""#));
    }

    #[test]
    fn a_journal_entry_carries_who_what_and_when_and_nothing_else() {
        let dir = tempdir().unwrap();
        let journal = Journal::new(dir.path());
        journal
            .append(&Entry {
                at: 1,
                app: "app".into(),
                role: "user".into(),
                kind: Kind::Builtin,
                name: "ask".into(),
                event: Event::Start,
                outcome: None,
            })
            .unwrap();
        journal
            .append(&Entry {
                at: 2,
                app: "app".into(),
                role: "user".into(),
                kind: Kind::Builtin,
                name: "ask".into(),
                event: Event::End,
                outcome: Some("ok".to_owned()),
            })
            .unwrap();
        let lines = Journal::read(dir.path()).unwrap();
        assert_eq!(lines.len(), 2);
        let first: Entry = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(first.event, Event::Start);
        assert_eq!(first.outcome, None);
        let second: Entry = serde_json::from_str(&lines[1]).unwrap();
        assert_eq!(second.outcome.as_deref(), Some("ok"));
        let first_value: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
        let first_keys: std::collections::BTreeSet<&str> = first_value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        let expected_first: std::collections::BTreeSet<&str> =
            ["at", "app", "role", "kind", "name", "event"]
                .into_iter()
                .collect();
        assert_eq!(first_keys, expected_first);
        let second_value: serde_json::Value = serde_json::from_str(&lines[1]).unwrap();
        let second_keys: std::collections::BTreeSet<&str> = second_value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        let expected_second: std::collections::BTreeSet<&str> =
            ["at", "app", "role", "kind", "name", "event", "outcome"]
                .into_iter()
                .collect();
        assert_eq!(second_keys, expected_second);
        // No payload the caller did not put in.
        assert!(!lines[0].contains("SECRET_MARKER"));
        assert!(!lines[1].contains("SECRET_MARKER"));
    }

    #[test]
    fn an_unwritable_journal_is_an_error_the_caller_can_refuse_on() {
        let dir = tempdir().unwrap();
        let folder = dir.path().join("folder");
        std::fs::write(&folder, b"i am a file").unwrap();
        let journal = Journal::new(&folder);
        let entry = Entry {
            at: 1,
            app: "app".into(),
            role: "user".into(),
            kind: Kind::Tool,
            name: "t".into(),
            event: Event::Start,
            outcome: None,
        };
        assert!(journal.append(&entry).is_err());
    }
}
