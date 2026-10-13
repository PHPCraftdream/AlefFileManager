// SPDX-License-Identifier: MIT OR Apache-2.0
//! The relaunch decision: when the launcher itself cannot satisfy the script's runtime
//! requirement, it starts the highest installed binary that does, once. A marker variable
//! bounds the depth, so a store binary never relaunches again. The child keeps the caller's
//! argv and environment; the marker is removed from this process so application children
//! never inherit it. Failure reasons stay the diagnostics of the command module.
use std::{
    ffi::OsString,
    future::Future,
    io,
    path::Path,
    pin::Pin,
    sync::atomic::{AtomicU32, Ordering},
};

use crate::runtimes::{command, store};

pub const MARKER_VARIABLE: &str = "ALEF_RELAUNCHED";
pub const FROM_VARIABLE: &str = "ALEF_RELAUNCHED_FROM";
const MAX_DEPTH: u32 = 1;
static DEPTH: AtomicU32 = AtomicU32::new(0);

fn parse_depth(value: Option<&std::ffi::OsStr>) -> u32 {
    match value {
        None => 0,
        Some(value) => value
            .to_str()
            .and_then(|text| text.parse::<u32>().ok())
            .map_or(MAX_DEPTH, |depth| depth.min(MAX_DEPTH)),
    }
}

// Process-global; exercised by the integration test in tests/.
/// Called once at process start, before threads exist.
pub fn take_marker() -> u32 {
    let depth = parse_depth(std::env::var_os(MARKER_VARIABLE).as_deref());
    DEPTH.store(depth, Ordering::Relaxed);
    std::env::remove_var(MARKER_VARIABLE);
    std::env::remove_var(FROM_VARIABLE);
    depth
}

pub fn depth() -> u32 {
    DEPTH.load(Ordering::Relaxed)
}

/// Why a relaunch did not happen.
#[derive(Debug, PartialEq, Eq)]
pub enum AttemptFailure {
    /// The requirement stays unsatisfied: today's `RUNTIME_INCOMPATIBLE` diagnostic, exit code 2.
    Incompatible(String),
    /// The selected binary could not be started: `RUNTIME_RELAUNCH_FAILED`, exit code 1.
    Start(String),
}

pub type Spawn<'a> = Pin<Box<dyn Future<Output = io::Result<u8>> + Send + 'a>>;

/// Starts the stored binary and waits for it; injected so tests never start a process.
pub trait Spawner: Send + Sync {
    fn spawn_and_wait<'a>(
        &'a self,
        binary: &'a Path,
        arguments: &'a [OsString],
        extra: &'a [(String, String)],
    ) -> Spawn<'a>;
}

/// The real child: inherited environment and standard handles, the marker added on top.
pub struct Process;

impl Spawner for Process {
    fn spawn_and_wait<'a>(
        &'a self,
        binary: &'a Path,
        arguments: &'a [OsString],
        extra: &'a [(String, String)],
    ) -> Spawn<'a> {
        Box::pin(async move {
            let mut child = super::platform::spawn_with(binary, arguments, extra)?;
            Ok(super::exit_code(child.wait().await?))
        })
    }
}

/// True only for the very file this process runs, never for a copy in the store.
fn is_running_binary(binary: &Path) -> bool {
    let running = std::env::current_exe().and_then(std::fs::canonicalize);
    match (running, std::fs::canonicalize(binary)) {
        (Ok(running), Ok(selected)) => running == selected,
        _ => false,
    }
}

/// Relaunches into the highest installed version that satisfies `requirement`, if the marker
/// depth allows one hop and the stored binary is a plain regular file that is not this
/// process. The child keeps the caller's argv and environment.
pub async fn attempt_in(
    root: &Path,
    requirement: &str,
    arguments: &[OsString],
    depth: u32,
    spawner: &dyn Spawner,
) -> Result<u8, AttemptFailure> {
    let unsatisfied =
        || AttemptFailure::Incompatible(command::incompatible_detail_in(root, requirement));
    if depth >= MAX_DEPTH {
        return Err(unsatisfied());
    }
    let selected = match store::select_in(root, requirement) {
        Ok(Some(selected)) => selected,
        _ => return Err(unsatisfied()),
    };
    // Re-checked after the selection: the store may have changed underneath.
    if !store::is_regular_file(&selected.binary) || is_running_binary(&selected.binary) {
        return Err(unsatisfied());
    }
    let extra = [
        (MARKER_VARIABLE.to_owned(), MAX_DEPTH.to_string()),
        (
            FROM_VARIABLE.to_owned(),
            env!("CARGO_PKG_VERSION").to_owned(),
        ),
    ];
    spawner
        .spawn_and_wait(&selected.binary, arguments, &extra)
        .await
        .map_err(|error| {
            AttemptFailure::Start(format!(
                "{} at {}: {error}",
                selected.version,
                selected.binary.display()
            ))
        })
}

pub async fn attempt(requirement: &str, arguments: &[OsString]) -> Result<u8, AttemptFailure> {
    attempt_in(&store::root(), requirement, arguments, depth(), &Process).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn stored(root: &Path, version: &str) {
        std::fs::create_dir_all(root.join(version)).unwrap();
        std::fs::write(root.join(version).join(store::BINARY_NAME), b"binary").unwrap();
    }

    struct Recorded {
        calls: Mutex<Vec<(std::path::PathBuf, Vec<String>, Vec<(String, String)>)>>,
        result: io::Result<u8>,
    }
    impl Spawner for Recorded {
        fn spawn_and_wait<'a>(
            &'a self,
            binary: &'a Path,
            arguments: &'a [OsString],
            extra: &'a [(String, String)],
        ) -> Spawn<'a> {
            self.calls.lock().unwrap().push((
                binary.to_owned(),
                arguments
                    .iter()
                    .map(|a| a.to_string_lossy().into_owned())
                    .collect(),
                extra.to_owned(),
            ));
            let result = match &self.result {
                Ok(code) => Ok(*code),
                Err(error) => Err(io::Error::new(error.kind(), error.to_string())),
            };
            Box::pin(async move { result })
        }
    }
    impl Recorded {
        fn new(result: io::Result<u8>) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                result,
            }
        }
    }

    #[tokio::test]
    async fn the_highest_satisfying_stored_binary_is_started_with_the_original_arguments_and_the_marker(
    ) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        for version in ["1.0.0", "1.2.0", "2.0.0"] {
            stored(root, version);
        }
        let arguments: Vec<OsString> = ["run", "pkg.alef"].map(OsString::from).to_vec();
        let spawner = Recorded::new(Ok(7));
        let result = attempt_in(root, "^1", &arguments, 0, &spawner).await;
        assert_eq!(result, Ok(7));
        let calls = spawner.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, root.join("1.2.0").join(store::BINARY_NAME));
        assert_eq!(calls[0].1, ["run", "pkg.alef"]);
        assert_eq!(
            calls[0].2,
            [
                (MARKER_VARIABLE.to_owned(), "1".to_owned()),
                (
                    FROM_VARIABLE.to_owned(),
                    env!("CARGO_PKG_VERSION").to_owned()
                ),
            ]
        );
    }

    #[tokio::test]
    async fn every_child_exit_code_is_forwarded_including_zero() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        stored(root, "1.2.0");
        let arguments: Vec<OsString> = Vec::new();
        for code in [0u8, 2, 143] {
            let spawner = Recorded::new(Ok(code));
            let result = attempt_in(root, "^1", &arguments, 0, &spawner).await;
            assert_eq!(result, Ok(code));
        }
    }

    #[tokio::test]
    async fn the_marker_depth_stops_a_second_hop_and_keeps_the_old_diagnostic() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        stored(root, "1.2.0");
        let arguments: Vec<OsString> = Vec::new();
        let spawner = Recorded::new(Ok(0));
        let result = attempt_in(root, "^1", &arguments, 1, &spawner).await;
        let Err(AttemptFailure::Incompatible(detail)) = result else {
            panic!("expected Incompatible")
        };
        assert!(detail.contains("requires ^1"));
        assert!(detail.contains("this executable provides"));
        assert!(detail.contains("selected installed runtime: 1.2.0 at"));
        assert!(!detail.contains("no installed runtime satisfies"));
        assert!(spawner.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_requirement_nobody_satisfies_keeps_the_install_hint_and_starts_nothing() {
        let temp = tempfile::tempdir().unwrap();
        let arguments: Vec<OsString> = Vec::new();
        let spawner = Recorded::new(Ok(0));
        let result = attempt_in(temp.path(), "^1", &arguments, 0, &spawner).await;
        let Err(AttemptFailure::Incompatible(detail)) = result else {
            panic!("expected Incompatible")
        };
        assert!(detail.contains("no installed runtime satisfies ^1"));
        assert!(detail.contains("alef runtime install <version> <path-to-alef-binary>"));
        assert!(spawner.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_unreadable_store_keeps_its_separate_reason_and_starts_nothing() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("store");
        std::fs::write(&root, b"not a directory").unwrap();
        let arguments: Vec<OsString> = Vec::new();
        let spawner = Recorded::new(Ok(0));
        let result = attempt_in(&root, "^1", &arguments, 0, &spawner).await;
        let Err(AttemptFailure::Incompatible(detail)) = result else {
            panic!("expected Incompatible")
        };
        assert!(detail.contains("installed runtimes: unreadable at"));
        assert!(spawner.calls.lock().unwrap().is_empty());
    }

    // The depth marker is the second line of defense behind this identity check.
    #[test]
    fn the_running_binary_itself_is_never_selected_for_the_relaunch() {
        let running = std::env::current_exe().unwrap();
        assert!(is_running_binary(&running));
        let temp = tempfile::tempdir().unwrap();
        let copy = temp.path().join(store::BINARY_NAME);
        std::fs::copy(&running, &copy).unwrap();
        assert!(!is_running_binary(&copy));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_link_in_place_of_the_selected_binary_is_refused_before_any_start() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        stored(root, "1.0.0");
        let target = temp.path().join("elsewhere");
        std::fs::write(&target, b"binary").unwrap();
        std::fs::remove_file(root.join("1.0.0").join(store::BINARY_NAME)).unwrap();
        std::os::unix::fs::symlink(&target, root.join("1.0.0").join(store::BINARY_NAME)).unwrap();
        let arguments: Vec<OsString> = Vec::new();
        let spawner = Recorded::new(Ok(0));
        let result = attempt_in(root, "^1", &arguments, 0, &spawner).await;
        let Err(AttemptFailure::Incompatible(detail)) = result else {
            panic!("expected Incompatible")
        };
        assert!(detail.contains("no installed runtime satisfies"));
        assert!(spawner.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_failed_start_becomes_runtime_relaunch_failed_with_the_version_and_path() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        stored(root, "1.2.0");
        let arguments: Vec<OsString> = Vec::new();
        let spawner = Recorded::new(Err(io::Error::other("access denied")));
        let result = attempt_in(root, "^1", &arguments, 0, &spawner).await;
        let Err(AttemptFailure::Start(message)) = result else {
            panic!("expected Start")
        };
        assert!(message.contains("1.2.0"));
        assert!(message.contains(
            &root
                .join("1.2.0")
                .join(store::BINARY_NAME)
                .display()
                .to_string()
        ));
        assert!(message.contains("access denied"));
    }

    #[test]
    fn the_marker_depth_is_fail_closed_for_unreadable_values() {
        use std::ffi::OsStr;
        assert_eq!(parse_depth(None), 0);
        assert_eq!(parse_depth(Some(OsStr::new("0"))), 0);
        assert_eq!(parse_depth(Some(OsStr::new("1"))), 1);
        assert_eq!(parse_depth(Some(OsStr::new("5"))), 1);
        assert_eq!(parse_depth(Some(OsStr::new("x"))), 1);
        assert_eq!(parse_depth(Some(OsStr::new("-1"))), 1);
    }
}
