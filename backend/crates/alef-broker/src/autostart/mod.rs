// SPDX-License-Identifier: MIT OR Apache-2.0
//! Broker autostart (docs/stages/m11-warm-start.md, "Автозапуск: отдельные
//! платформенные пути"): one owned record per platform, with validation,
//! ownership checks, readback confirmation and atomic writes. This slice does
//! not call `launchctl` and is never enabled implicitly.

pub mod backend;
pub mod cmdline;
pub mod desktopfmt;
pub mod entry;
pub mod plistfmt;

#[cfg(windows)]
pub use backend::WindowsRegistry;
pub use backend::{
    resolve_config_home, FileBackend, LaunchAgent, MemoryBackend, NativeBackend, XdgAutostart,
};
pub use entry::{Entry, MARKER_ARG};

use std::fmt;

/// Well-known names of our records on each platform.
pub const RUN_VALUE_NAME: &str = "AlefRuntimeBroker";
pub const LAUNCH_AGENT_LABEL: &str = "org.alef.runtime.broker";
pub const DESKTOP_FILE_NAME: &str = "alef-runtime-broker.desktop";
/// Ownership comment line inside our `.desktop` file.
pub const OWNER_MARKER: &str = "# Owner: alef-runtime-broker (alef-broker autostart)";

/// Errors of the autostart slice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutostartError {
    /// The proposed entry is not renderable (relative exe, NUL, line breaks).
    InvalidEntry(&'static str),
    /// A record exists but was not written by us; never overwritten.
    NotOwned,
    /// The record exists but cannot be parsed; fail closed.
    Damaged,
    /// After write/remove, reading back did not confirm the change.
    ReadbackMismatch,
    Io(String),
    Platform(String),
}

impl fmt::Display for AutostartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidEntry(why) => write!(f, "invalid autostart entry: {why}"),
            Self::NotOwned => {
                write!(f, "autostart record exists but is not owned by alef-broker")
            }
            Self::Damaged => write!(f, "autostart record is damaged"),
            Self::ReadbackMismatch => {
                write!(f, "autostart readback does not match the written record")
            }
            Self::Io(e) => write!(f, "autostart io error: {e}"),
            Self::Platform(e) => write!(f, "autostart platform error: {e}"),
        }
    }
}

impl std::error::Error for AutostartError {}

/// Result of a read: our record, a structurally valid record of someone else,
/// or a broken one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parsed {
    Entry(Entry),
    Foreign,
    Damaged,
}

/// Wire format of the record, tied to the backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// Windows Run value (`CommandLineToArgvW` command line).
    Run,
    /// macOS LaunchAgent plist.
    Plist,
    /// Linux XDG `.desktop`.
    Desktop,
}

impl Format {
    pub fn render(&self, entry: &Entry) -> Result<String, AutostartError> {
        match self {
            Self::Run => cmdline::render_run_value(entry),
            Self::Plist => plistfmt::render_plist(LAUNCH_AGENT_LABEL, entry),
            Self::Desktop => desktopfmt::render_desktop(OWNER_MARKER, entry),
        }
    }

    pub fn parse(&self, raw: &str) -> Result<Parsed, AutostartError> {
        Ok(match self {
            Self::Run => match cmdline::parse_run_value(raw) {
                Ok(entry) => {
                    if entry.has_marker() {
                        Parsed::Entry(entry)
                    } else {
                        Parsed::Foreign
                    }
                }
                Err(_) => Parsed::Damaged,
            },
            Self::Plist => plistfmt::parse_plist(raw, LAUNCH_AGENT_LABEL),
            Self::Desktop => desktopfmt::parse_desktop(OWNER_MARKER, raw),
        })
    }
}

/// State of our record on the platform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// No record.
    Disabled,
    /// Our record, as stored (with the marker argument on Windows).
    Enabled(Entry),
    /// Someone else's record under our name; left untouched.
    Foreign,
    /// A record we cannot parse; left untouched, fail closed.
    Damaged,
}

/// Storage for one raw record plus its format.
pub trait Backend {
    /// Raw record; `None` when there is none.
    fn read(&self) -> Result<Option<String>, AutostartError>;
    fn write(&self, raw: &str) -> Result<(), AutostartError>;
    fn remove(&self) -> Result<(), AutostartError>;
    fn format(&self) -> Format;
}

/// Owner logic over any backend: enable/disable/status with ownership checks
/// and readback confirmation.
#[derive(Debug, Clone)]
pub struct Autostart<B: Backend> {
    backend: B,
}

impl<B: Backend> Autostart<B> {
    pub fn new(backend: B) -> Self {
        Self { backend }
    }

    pub fn status(&self) -> Result<Status, AutostartError> {
        match self.backend.read()? {
            None => Ok(Status::Disabled),
            Some(raw) => Ok(match self.backend.format().parse(&raw)? {
                Parsed::Entry(entry) => Status::Enabled(entry),
                Parsed::Foreign => Status::Foreign,
                Parsed::Damaged => Status::Damaged,
            }),
        }
    }

    /// Enable with `entry`. Foreign/Damaged records are never overwritten.
    /// Enabling over our own record updates it; the write is confirmed by
    /// reading the raw record back and comparing it byte for byte.
    pub fn enable(&self, entry: &Entry) -> Result<(), AutostartError> {
        match self.status()? {
            Status::Foreign => return Err(AutostartError::NotOwned),
            Status::Damaged => return Err(AutostartError::Damaged),
            Status::Disabled | Status::Enabled(_) => {}
        }
        let marked = match self.backend.format() {
            Format::Run => entry.with_marker(),
            Format::Plist | Format::Desktop => entry.clone(),
        };
        let rendered = self.backend.format().render(&marked)?;
        self.backend.write(&rendered)?;
        match self.backend.read()? {
            Some(raw) if raw == rendered => Ok(()),
            _ => Err(AutostartError::ReadbackMismatch),
        }
    }

    /// Disable. A Foreign record is refused (`NotOwned`), a Damaged one fails
    /// closed (`Damaged`) so that a broken record is never silently deleted;
    /// removal is confirmed by readback.
    pub fn disable(&self) -> Result<(), AutostartError> {
        match self.status()? {
            Status::Disabled => Ok(()),
            Status::Foreign => Err(AutostartError::NotOwned),
            Status::Damaged => Err(AutostartError::Damaged),
            Status::Enabled(_) => {
                self.backend.remove()?;
                if self.backend.read()?.is_some() {
                    Err(AutostartError::ReadbackMismatch)
                } else {
                    Ok(())
                }
            }
        }
    }
}

/// Backend for the current platform; construct outside of tests only.
pub fn native() -> Result<Autostart<NativeBackend>, AutostartError> {
    #[cfg(windows)]
    {
        Ok(Autostart::new(NativeBackend::Windows(
            WindowsRegistry::new(),
        )))
    }
    #[cfg(not(windows))]
    {
        #[cfg(target_os = "macos")]
        {
            Ok(Autostart::new(NativeBackend::File(FileBackend::new(
                LaunchAgent::new()?.path().to_path_buf(),
            ))))
        }
        #[cfg(not(target_os = "macos"))]
        {
            Ok(Autostart::new(NativeBackend::File(FileBackend::new(
                XdgAutostart::new()?.path().to_path_buf(),
            ))))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::path::PathBuf;

    fn entry(exe: &str, args: &[&str]) -> Entry {
        // Anchor in tempdir: the exe must be absolute on every platform.
        Entry::new(
            std::env::temp_dir().join(exe.replace(['/', '\\'], "_")),
            args.iter().map(OsString::from).collect(),
        )
    }

    /// A backend whose read disagrees with its write: readback must fail.
    struct Liar(MemoryBackend);
    impl Backend for Liar {
        fn read(&self) -> Result<Option<String>, AutostartError> {
            Ok(if self.0.read()?.is_some() {
                Some("totally broken raw".to_string())
            } else {
                None
            })
        }
        fn write(&self, raw: &str) -> Result<(), AutostartError> {
            self.0.write(raw)
        }
        fn remove(&self) -> Result<(), AutostartError> {
            self.0.remove()
        }
        fn format(&self) -> Format {
            Format::Run
        }
    }

    #[test]
    fn starts_disabled() {
        let auto = Autostart::new(MemoryBackend::new());
        assert_eq!(auto.status().expect("status"), Status::Disabled);
        auto.disable().expect("disable on disabled is a no-op");
    }

    #[test]
    fn enable_makes_it_enabled_with_marker() {
        let auto = Autostart::new(MemoryBackend::new());
        let e = entry("/x/alef", &["run"]);
        auto.enable(&e).expect("enable");
        match auto.status().expect("status") {
            Status::Enabled(stored) => {
                assert!(stored.has_marker());
                assert_eq!(stored, entry("/x/alef", &["run"]).with_marker());
            }
            other => panic!("expected Enabled, got {other:?}"),
        }
    }

    #[test]
    fn re_enabling_same_entry_is_idempotent() {
        let auto = Autostart::new(MemoryBackend::new());
        let e = entry("/x/alef", &["run"]);
        auto.enable(&e).expect("enable");
        let raw = auto.backend.read().expect("read");
        auto.enable(&e).expect("re-enable");
        assert_eq!(auto.backend.read().expect("read"), raw);
    }

    #[test]
    fn enabling_new_entry_updates_the_record() {
        let auto = Autostart::new(MemoryBackend::new());
        auto.enable(&entry("/x/alef", &["run"])).expect("enable");
        auto.enable(&entry("/x/alef", &["run", "fast"]))
            .expect("update");
        match auto.status().expect("status") {
            Status::Enabled(stored) => {
                assert_eq!(
                    stored.args,
                    vec![
                        OsString::from("run"),
                        OsString::from("fast"),
                        OsString::from(MARKER_ARG)
                    ]
                );
            }
            other => panic!("expected Enabled, got {other:?}"),
        }
    }

    #[test]
    fn disable_removes_and_readback_confirms() {
        let auto = Autostart::new(MemoryBackend::new());
        auto.enable(&entry("/x/alef", &[])).expect("enable");
        auto.disable().expect("disable");
        assert_eq!(auto.status().expect("status"), Status::Disabled);
    }

    #[test]
    fn foreign_record_is_refused_and_untouched() {
        let backend = MemoryBackend::new();
        // A valid run value without our marker: someone else's record.
        backend
            .write("C:\\other.exe /silent")
            .expect("seed foreign");
        let raw = backend.read().expect("read");
        let auto = Autostart::new(backend);
        assert_eq!(auto.status().expect("status"), Status::Foreign);
        assert!(matches!(
            auto.enable(&entry("/x/a", &[])),
            Err(AutostartError::NotOwned)
        ));
        assert!(matches!(auto.disable(), Err(AutostartError::NotOwned)));
        assert_eq!(auto.backend.read().expect("read"), raw);
    }

    #[test]
    fn damaged_record_fails_closed() {
        // An empty Run value has no tokens: unparseable, hence Damaged.
        let backend = MemoryBackend::new();
        backend.write("   ").expect("seed");
        let auto = Autostart::new(backend);
        assert_eq!(auto.status().expect("status"), Status::Damaged);
        assert!(matches!(
            auto.enable(&entry("/x/a", &[])),
            Err(AutostartError::Damaged)
        ));
        assert!(matches!(auto.disable(), Err(AutostartError::Damaged)));
    }

    #[test]
    fn enable_over_foreign_leaves_the_record_unchanged() {
        let backend = MemoryBackend::new();
        backend.write("/x/other.exe").expect("seed foreign");
        let auto = Autostart::new(backend);
        let _ = auto.enable(&entry("/x/ours", &[]));
        assert_eq!(
            auto.backend.read().expect("read").as_deref(),
            Some("/x/other.exe")
        );
    }

    #[test]
    fn readback_mismatch_is_detected() {
        let auto = Autostart::new(Liar(MemoryBackend::new()));
        assert!(matches!(
            auto.enable(&entry("/x/alef", &[])),
            Err(AutostartError::ReadbackMismatch)
        ));
    }

    #[test]
    fn error_display_is_informative() {
        let e = AutostartError::NotOwned;
        assert!(e.to_string().contains("not owned"));
        assert!(AutostartError::InvalidEntry("x").to_string().contains("x"));
    }

    #[test]
    fn format_run_flags_missing_marker_as_foreign() {
        let e = entry("/x/alef", &[]);
        let rendered = Format::Run.render(&e).expect("render");
        assert_eq!(Format::Run.parse(&rendered), Ok(Parsed::Foreign));
        let marked = Format::Run.render(&e.with_marker()).expect("render");
        assert!(matches!(Format::Run.parse(&marked), Ok(Parsed::Entry(_))));
    }

    #[test]
    fn format_desktop_and_plist_agree_with_their_parsers() {
        let e = entry("/x/alef", &["run"]);
        let d = Format::Desktop.render(&e).expect("render");
        assert_eq!(Format::Desktop.parse(&d), Ok(Parsed::Entry(e.clone())));
        let p = Format::Plist.render(&e).expect("render");
        assert_eq!(Format::Plist.parse(&p), Ok(Parsed::Entry(e)));
    }

    #[test]
    fn invalid_entry_never_reaches_the_backend() {
        let backend = MemoryBackend::new();
        let auto = Autostart::new(backend);
        let bad = Entry::new(PathBuf::from("relative"), vec![]);
        assert!(matches!(
            auto.enable(&bad),
            Err(AutostartError::InvalidEntry(_))
        ));
        assert_eq!(auto.status().expect("status"), Status::Disabled);
    }

    #[test]
    fn resolve_config_home_xdg_rules() {
        // Absolute paths of the platform: "/abs" is not absolute on Windows.
        let abs = std::env::temp_dir().join("abs");
        let abs_text = abs.to_str().expect("utf-8 temp dir");
        let home = std::env::temp_dir().join("h");
        let home_text = home.to_str().expect("utf-8 temp dir");
        let fallback = home.join(".config");
        assert_eq!(
            resolve_config_home(Some(abs_text), Some(home_text)),
            Some(abs.clone())
        );
        assert_eq!(
            resolve_config_home(Some(""), Some(home_text)),
            Some(fallback.clone())
        );
        assert_eq!(
            resolve_config_home(Some("rel"), Some(home_text)),
            Some(fallback)
        );
        assert_eq!(resolve_config_home(None, None), None);
    }

    #[test]
    fn backend_trait_dispatches_via_memory() {
        let backend = MemoryBackend::new();
        backend.write("raw").expect("write");
        assert_eq!(backend.read().expect("read").as_deref(), Some("raw"));
    }
}
