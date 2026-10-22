// SPDX-License-Identifier: MIT OR Apache-2.0
//! Startup phase marks: `ALEF_TIMING=1` prints one stderr line per named milestone of the cold
//! start (`ALEF_TIMING <name> <fractional milliseconds since the process-wide origin>`). Every
//! name prints at most once per process; without the variable the cost is a single boolean read.
use std::{
    collections::HashSet,
    sync::{Mutex, OnceLock},
    time::Instant,
};

/// Whether `ALEF_TIMING=1` was given; read once, then a plain boolean.
static ENABLED: OnceLock<bool> = OnceLock::new();
/// The origin of every mark: the first time anything asks (the first `main` line).
static START: OnceLock<Instant> = OnceLock::new();
/// Names already printed: repeated calls for the same name are ignored.
static MARKED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

/// The one line of a mark: `ALEF_TIMING <name> <fractional ms from start>`, or nothing when off.
/// Pure so the format and the dedup policy stay testable without the environment.
pub fn format_mark(enabled: bool, name: &str, start: Instant, now: Instant) -> Option<String> {
    if !enabled {
        return None;
    }
    let milliseconds = now.duration_since(start).as_secs_f64() * 1000.0;
    Some(format!("ALEF_TIMING {name} {milliseconds:.3}"))
}

/// An injectable recorder: the pure core of [`mark`] — an origin, the flag, and the set of
/// names already printed. `record` returns the line to print, or `None` (off, or repeated).
#[cfg(test)]
pub(crate) struct Recorder {
    enabled: bool,
    start: Instant,
    marked: HashSet<String>,
}

#[cfg(test)]
impl Recorder {
    pub(crate) fn new(enabled: bool, start: Instant) -> Self {
        Self {
            enabled,
            start,
            marked: HashSet::new(),
        }
    }

    pub(crate) fn record(&mut self, name: &str, now: Instant) -> Option<String> {
        if !self.marked.insert(name.to_owned()) {
            return None;
        }
        format_mark(self.enabled, name, self.start, now)
    }
}

/// Prints one startup mark, at most once per name per process, when `ALEF_TIMING=1`.
pub fn mark(name: &str) {
    if !*ENABLED.get_or_init(|| std::env::var("ALEF_TIMING").as_deref() == Ok("1")) {
        return;
    }
    let start = *START.get_or_init(Instant::now);
    let mut marked = MARKED
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if marked.insert(name.to_owned()) {
        eprintln!(
            "{}",
            format_mark(true, name, start, Instant::now()).unwrap_or_default()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mark_formats_the_name_and_fractional_milliseconds() {
        let start = Instant::now();
        let now =
            start + std::time::Duration::from_millis(1234) + std::time::Duration::from_micros(567);
        assert_eq!(
            format_mark(true, "main", start, now),
            Some("ALEF_TIMING main 1234.567".to_owned())
        );
    }

    #[test]
    fn without_the_variable_the_formatter_stays_silent() {
        let start = Instant::now();
        assert_eq!(format_mark(false, "main", start, start), None);
        let mut recorder = Recorder::new(false, start);
        assert_eq!(recorder.record("main", start), None);
    }

    #[test]
    fn a_name_prints_only_once_per_recorder() {
        let start = Instant::now();
        let mut recorder = Recorder::new(true, start);
        let line = recorder.record("args-parsed", start + std::time::Duration::from_millis(1));
        assert!(line
            .as_deref()
            .is_some_and(|line| line.starts_with("ALEF_TIMING args-parsed ")));
        assert_eq!(recorder.record("args-parsed", start), None);
        assert!(recorder
            .record("settle-begin", start)
            .is_some_and(|line| line.starts_with("ALEF_TIMING settle-begin ")));
    }

    #[test]
    fn durations_before_the_origin_are_not_expected_but_do_not_panic() {
        let now = Instant::now();
        let before = now - std::time::Duration::from_millis(5);
        assert_eq!(
            format_mark(true, "x", before, now),
            Some("ALEF_TIMING x 5.000".to_owned())
        );
    }
}
