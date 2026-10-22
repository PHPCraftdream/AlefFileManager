// SPDX-License-Identifier: MIT OR Apache-2.0
//! Where the windows were. A window whose definition says `restore: true` opens where it was when
//! the application last ran. What the system reports of such a window is noted as it changes and
//! written to a small Ktav file at most a second after the first change, when a window closes and
//! when the application ends.
use std::{
    collections::{BTreeMap, HashSet},
    fs, io,
    path::PathBuf,
    time::{Duration, Instant},
};

use alef_core::{
    registry::window::{
        geometry::{self, Remembered},
        MonitorInfo, WindowInfo,
    },
    security::permissions::store,
    security::window::WindowDef,
};
use serde::{Deserialize, Serialize};

/// How long a change waits for the next ones before it is written.
const WRITE_AFTER: Duration = Duration::from_secs(1);
const VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
struct Saved {
    version: u32,
    windows: BTreeMap<String, Remembered>,
}

pub(in crate::window) struct Restore {
    path: PathBuf,
    windows: BTreeMap<String, Remembered>,
    tracked: HashSet<String>,
    changed_at: Option<Instant>,
}

/// Numbers a hand-edited or damaged file could hold are not trusted.
fn sane(remembered: &Remembered) -> bool {
    let finite = |value: f64| value.is_finite() && value.abs() < 1.0e6;
    finite(remembered.width)
        && finite(remembered.height)
        && remembered.width >= 1.0
        && remembered.height >= 1.0
        && remembered.x.is_none_or(finite)
        && remembered.y.is_none_or(finite)
}

impl Restore {
    /// Reads the file; one that is missing, damaged or of another version is as good as none.
    pub(in crate::window) fn load(path: PathBuf) -> Self {
        let windows = match fs::read(&path) {
            Ok(bytes) => match store::from_bytes::<Saved>(&bytes) {
                Ok(saved) if saved.version == VERSION => saved
                    .windows
                    .into_iter()
                    .filter(|(_, remembered)| sane(remembered))
                    .collect(),
                Ok(saved) => {
                    eprintln!(
                        "Window state: ignoring {} (version {})",
                        path.display(),
                        saved.version
                    );
                    BTreeMap::new()
                }
                Err(error) => {
                    eprintln!("Window state: ignoring {}: {error}", path.display());
                    BTreeMap::new()
                }
            },
            Err(_) => BTreeMap::new(),
        };
        Self {
            path,
            windows,
            tracked: HashSet::new(),
            changed_at: None,
        }
    }

    /// The window `definition` describes, placed where it was, and whether it was maximized.
    pub(in crate::window) fn apply(
        &self,
        definition: &WindowDef,
        monitors: &[MonitorInfo],
    ) -> Option<(WindowDef, bool)> {
        let remembered = self.windows.get(&definition.label)?;
        Some((
            geometry::restore(definition, remembered, monitors),
            remembered.maximized,
        ))
    }

    /// From now on the window is remembered.
    pub(in crate::window) fn track(&mut self, label: &str) {
        self.tracked.insert(label.to_owned());
    }

    /// Takes note of what the system reports of a window. A window that is minimized or full
    /// screen says nothing about where it belongs; a maximized one only that it was maximized.
    pub(in crate::window) fn observe(&mut self, info: &WindowInfo, now: Instant) {
        if !self.tracked.contains(&info.label) || info.minimized == Some(true) || info.fullscreen {
            return;
        }
        let next = if info.maximized {
            let mut kept = self
                .windows
                .get(&info.label)
                .copied()
                .unwrap_or(Remembered {
                    x: info.x,
                    y: info.y,
                    width: info.width,
                    height: info.height,
                    maximized: true,
                });
            kept.maximized = true;
            kept
        } else {
            Remembered {
                x: info.x,
                y: info.y,
                width: info.width,
                height: info.height,
                maximized: false,
            }
        };
        if self.windows.get(&info.label) != Some(&next) {
            self.windows.insert(info.label.clone(), next);
            self.changed_at.get_or_insert(now);
        }
    }

    /// When the changes noted so far are to be written, if there are any.
    pub(in crate::window) fn due_at(&self) -> Option<Instant> {
        self.changed_at.map(|since| since + WRITE_AFTER)
    }

    pub(in crate::window) fn save_if_due(&mut self, now: Instant) {
        if self.due_at().is_some_and(|due| now >= due) {
            self.flush();
        }
    }

    /// Writes the changes now. A failure is reported and not retried: the next change tries again.
    pub(in crate::window) fn flush(&mut self) {
        if self.changed_at.take().is_none() {
            return;
        }
        if let Err(error) = self.write() {
            eprintln!(
                "Window state: cannot write {}: {error}",
                self.path.display()
            );
        }
    }

    fn write(&self) -> io::Result<()> {
        let saved = Saved {
            version: VERSION,
            windows: self.windows.clone(),
        };
        let bytes = store::to_bytes(&saved)?;
        if let Some(folder) = self.path.parent() {
            fs::create_dir_all(folder)?;
        }
        let partial = self.path.with_extension("ktav.part");
        fs::write(&partial, bytes)?;
        fs::rename(&partial, &self.path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alef_core::registry::window::Rect;
    use serde_json::json;

    fn info(label: &str, x: Option<f64>, width: f64, maximized: bool) -> WindowInfo {
        WindowInfo {
            label: label.to_owned(),
            revision: 1,
            title: label.to_owned(),
            width,
            height: 500.0,
            x,
            y: x.map(|x| x / 2.0),
            scale_factor: 1.0,
            focused: false,
            maximized,
            minimized: Some(false),
            visible: Some(true),
            decorated: true,
            resizable: true,
            fullscreen: false,
            always_on_top: false,
            zoom: 1.0,
            supports_drag_resize: true,
        }
    }

    fn display() -> Vec<MonitorInfo> {
        let area = Rect {
            x: 0.0,
            y: 0.0,
            width: 1920.0,
            height: 1040.0,
        };
        vec![MonitorInfo {
            name: None,
            bounds: area,
            work_area: area,
            scale_factor: 1.0,
            primary: true,
        }]
    }

    fn definition(label: &str) -> WindowDef {
        serde_json::from_value(json!({
            "label": label, "url": "/", "width": 800, "height": 600, "restore": true
        }))
        .unwrap()
    }

    fn store() -> (tempfile::TempDir, PathBuf) {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("state").join("windows.ktav");
        (folder, path)
    }

    #[test]
    fn what_is_noted_is_written_and_read_back_and_opens_the_window_there() {
        let (_folder, path) = store();
        let mut first = Restore::load(path.clone());
        first.track("main");
        let start = Instant::now();
        first.observe(&info("main", Some(300.0), 900.0, false), start);
        assert!(!path.exists(), "nothing is written before its time");
        first.flush();
        assert!(path.exists());
        let second = Restore::load(path);
        let (restored, maximized) = second
            .apply(&definition("main"), &display())
            .expect("known window");
        assert!(!maximized);
        assert_eq!(
            restored.width,
            alef_core::security::window::Length::Px(900.0)
        );
        assert!(
            second.apply(&definition("other"), &display()).is_none(),
            "an unknown window is placed as defined"
        );
    }

    #[test]
    fn only_windows_that_asked_are_remembered() {
        let (_folder, path) = store();
        let mut store = Restore::load(path.clone());
        store.track("main");
        store.observe(&info("tool", Some(10.0), 400.0, false), Instant::now());
        assert!(
            store.due_at().is_none(),
            "a window that did not ask changes nothing"
        );
        store.flush();
        assert!(!path.exists());
    }

    #[test]
    fn writing_waits_for_the_quiet_after_the_first_change_and_a_flush_ends_the_wait() {
        let (_folder, path) = store();
        let mut store = Restore::load(path.clone());
        store.track("main");
        let start = Instant::now();
        store.observe(&info("main", Some(10.0), 400.0, false), start);
        assert_eq!(store.due_at(), Some(start + WRITE_AFTER));
        store.observe(
            &info("main", Some(20.0), 410.0, false),
            start + Duration::from_millis(300),
        );
        assert_eq!(
            store.due_at(),
            Some(start + WRITE_AFTER),
            "a later change does not push the time back"
        );
        store.save_if_due(start + Duration::from_millis(900));
        assert!(!path.exists());
        store.save_if_due(start + WRITE_AFTER);
        assert!(path.exists());
        assert!(store.due_at().is_none());
        let written = fs::read_to_string(&path).unwrap();
        assert!(
            written.contains("410"),
            "the last note is what is written: {written}"
        );
        store.observe(
            &info("main", Some(20.0), 410.0, false),
            start + Duration::from_secs(5),
        );
        assert!(
            store.due_at().is_none(),
            "the same place again is not a change"
        );
    }

    #[test]
    fn a_maximized_window_keeps_the_size_it_had_and_a_minimized_or_full_screen_one_says_nothing() {
        let (_folder, path) = store();
        let mut store = Restore::load(path.clone());
        store.track("main");
        let now = Instant::now();
        store.observe(&info("main", Some(100.0), 700.0, false), now);
        store.observe(&info("main", Some(0.0), 1920.0, true), now);
        let mut minimized = info("main", Some(-32000.0), 160.0, false);
        minimized.minimized = Some(true);
        store.observe(&minimized, now);
        let mut full = info("main", Some(0.0), 1920.0, false);
        full.fullscreen = true;
        store.observe(&full, now);
        store.flush();
        let (restored, maximized) = Restore::load(path)
            .apply(&definition("main"), &display())
            .unwrap();
        assert!(maximized);
        assert_eq!(
            restored.width,
            alef_core::security::window::Length::Px(700.0),
            "the size to come back to"
        );
        assert_eq!(
            restored.position,
            alef_core::security::window::WindowPosition::At {
                x: alef_core::security::window::Length::Px(100.0),
                y: alef_core::security::window::Length::Px(50.0),
            }
        );
    }

    #[test]
    fn saved_and_remembered_round_trip_through_ktav() {
        let remembered = Remembered {
            x: Some(3.0),
            y: None,
            width: 800.0,
            height: 600.0,
            maximized: false,
        };
        let remembered_bytes = store::to_bytes(&remembered).unwrap();
        let back: Remembered = store::from_bytes(&remembered_bytes).unwrap();
        assert_eq!(back.x, Some(3.0));
        assert_eq!(
            back.y, None,
            "a place the system does not tell stays untold"
        );

        let saved = Saved {
            version: VERSION,
            windows: BTreeMap::from([("app.main".to_owned(), remembered)]),
        };
        let saved_bytes = store::to_bytes(&saved).unwrap();
        let text = std::str::from_utf8(&saved_bytes).unwrap();
        let value = store::parse(text).unwrap();
        let object = value.as_object().expect("an object");
        assert_eq!(
            object.get("version").and_then(|v| v.as_integer()),
            Some("1"),
            "the version is written as an integer"
        );
        let windows = object
            .get("windows")
            .and_then(|v| v.as_object())
            .expect("the windows map");
        let entry = windows.get("app.main").expect("a key with a dot survives");
        assert_eq!(
            entry.as_object().unwrap().get("y"),
            Some(&store::Value::Null),
            "Option::None is written as null"
        );

        let back: Saved = store::from_bytes(&saved_bytes).unwrap();
        assert_eq!(back.version, VERSION);
        assert_eq!(back.windows.get("app.main"), Some(&remembered));
    }

    #[test]
    fn a_key_with_a_dot_is_written_quoted_and_reads_back_whole() {
        let (_folder, path) = store();
        let mut store = Restore::load(path.clone());
        store.track("app.main");
        store.observe(&info("app.main", Some(30.0), 500.0, false), Instant::now());
        store.flush();
        let written = fs::read_to_string(&path).unwrap();
        assert!(
            written.contains("\"app.main\""),
            "a dot in a key is quoted, not read as nesting: {written}"
        );
        let second = Restore::load(path);
        assert!(second.apply(&definition("app.main"), &display()).is_some());
    }

    #[test]
    fn a_duplicated_key_is_refused() {
        let text = "version: 1\nwindows: {\n    main: {\n        x: 1.0\n        y: 2.0\n        width: 800.0\n        height: 600.0\n        maximized: false\n    }\n    main: {\n        x: 3.0\n        y: 4.0\n        width: 500.0\n        height: 400.0\n        maximized: true\n    }\n}";
        assert!(store::parse(text).is_err(), "ktav refuses a repeated key");
        assert!(store::from_bytes::<Saved>(text.as_bytes()).is_err());
    }

    #[test]
    fn an_unknown_field_is_ignored() {
        let text = "version: 1\nnote: written by hand\nwindows: {}";
        let saved: Saved = store::from_bytes(text.as_bytes()).unwrap();
        assert_eq!(saved.version, 1);
        assert!(saved.windows.is_empty());
    }

    #[test]
    fn a_file_that_is_missing_damaged_old_or_absurd_is_as_good_as_none() {
        let (_folder, path) = store();
        assert!(Restore::load(path.clone()).windows.is_empty());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        for text in [
            "not a document {".to_owned(),
            "version: 7\nwindows: {}".to_owned(),
            "version: 1".to_owned(),
        ] {
            fs::write(&path, text).unwrap();
            assert!(Restore::load(path.clone()).windows.is_empty());
        }
        fs::write(
            &path,
            "version: 1\nwindows: {\n    good: {\n        x: 1.0\n        y: 2.0\n        width: 800.0\n        height: 600.0\n        maximized: false\n    }\n    zero: {\n        x: 1.0\n        y: 2.0\n        width: 0.0\n        height: 600.0\n        maximized: false\n    }\n    huge: {\n        x: 1.0\n        y: 2.0\n        width: 8000000.0\n        height: 600.0\n        maximized: false\n    }\n    far: {\n        x: 1e300\n        y: 2.0\n        width: 800.0\n        height: 600.0\n        maximized: false\n    }\n}",
        )
        .unwrap();
        let loaded = Restore::load(path);
        assert_eq!(loaded.windows.keys().collect::<Vec<_>>(), ["good"]);
    }

    #[test]
    fn a_place_the_system_does_not_tell_is_remembered_by_size_alone() {
        let (_folder, path) = store();
        let mut store = Restore::load(path.clone());
        store.track("main");
        store.observe(&info("main", None, 640.0, false), Instant::now());
        store.flush();
        let (restored, _) = Restore::load(path)
            .apply(&definition("main"), &display())
            .unwrap();
        assert_eq!(
            restored.width,
            alef_core::security::window::Length::Px(640.0)
        );
        assert_eq!(
            restored.position,
            alef_core::security::window::WindowPosition::Center
        );
    }
}
