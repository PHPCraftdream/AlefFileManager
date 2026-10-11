// SPDX-License-Identifier: MIT OR Apache-2.0
//! Process-wide launch selection. The runtime drives document-load and timeout checks; no timer is spawned here.
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use alef_core::{
    registry::{dispatch::Registry, host::Host},
    security::manifest::Mode,
    AlefError, ErrorCode,
};
use serde::Deserialize;

use crate::{json, ModuleContext};

/// Default decision period, measured only after the entry document has loaded.
pub const LAUNCH_LIMIT: Duration = Duration::from_secs(30);

#[derive(Debug)]
struct State {
    allowed: Vec<Mode>,
    mode: Option<Mode>,
    loading: bool,
    loaded_at: Option<Instant>,
    limit: Duration,
    expired: bool,
    reported: bool,
}

/// Clones share a single lock: selection, window-launch acceptance and expiry cannot race.
/// The default is a settled foreground legacy context.
/// Launchers should replace it with manifest-derived allowed and initial modes.
#[derive(Debug, Clone)]
pub struct LaunchState(Arc<Mutex<State>>);

impl Default for LaunchState {
    fn default() -> Self {
        Self::new(vec![Mode::Foreground], Some(Mode::Foreground), LAUNCH_LIMIT)
            .expect("default mode is allowed")
    }
}

impl LaunchState {
    /// `initial` is already selected (legacy startup or the window child). An undecided state
    /// must have at least one allowed mode; the launcher supplies validated manifest modes.
    pub fn new(
        allowed: Vec<Mode>,
        initial: Option<Mode>,
        limit: Duration,
    ) -> Result<Self, AlefError> {
        if allowed.is_empty() || initial.is_some_and(|mode| !allowed.contains(&mode)) {
            return Err(AlefError::new(
                ErrorCode::InvalidArgument,
                "initial mode must be allowed; allowed modes must not be empty",
            ));
        }
        Ok(Self(Arc::new(Mutex::new(State {
            allowed,
            mode: initial,
            loading: false,
            loaded_at: None,
            limit,
            expired: false,
            reported: false,
        }))))
    }

    /// Current selection; `None` serializes through app.mode as `undecided`.
    pub fn mode(&self) -> Option<Mode> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).mode
    }

    /// Entry navigation began. Repeated notifications do not restart the decision period.
    pub fn load_started(&self) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).loading = true;
    }

    /// Entry finished loading. No timeout is active before this notification.
    pub fn loaded(&self) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if state.loading && state.loaded_at.is_none() && state.mode.is_none() {
            state.loaded_at = Some(Instant::now());
        }
    }

    /// Claims an expiration exactly once, even if a late launch noticed it first. The runtime
    /// must then print its diagnostic to stderr and exit with code 2. No process action here.
    pub fn check_timeout(&self) -> bool {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        expire(&mut state);
        if state.expired && !state.reported {
            state.reported = true;
            return true;
        }
        false
    }

    pub(crate) fn stdio_allowed(&self) -> bool {
        let state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        state.mode == Some(Mode::Cli)
    }

    /// Selects once; the synchronous host acceptance is inside the same critical section.
    /// A failed window request leaves the mode undecided, never claiming a successful relaunch.
    pub fn launch(&self, mode: Mode, host: &dyn Host) -> Result<(), AlefError> {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if !state.allowed.contains(&mode) {
            return Err(AlefError::new(
                ErrorCode::InvalidArgument,
                "mode is not allowed",
            ));
        }
        if let Some(current) = state.mode {
            return if current == mode {
                Ok(())
            } else {
                Err(AlefError::new(
                    ErrorCode::AlreadyExists,
                    "launch mode is already selected",
                ))
            };
        }
        expire(&mut state);
        if state.expired {
            return Err(AlefError::new(
                ErrorCode::Timeout,
                "launch mode selection expired",
            ));
        }
        if mode == Mode::Window {
            host.launch_window()?;
        }
        state.mode = Some(mode);
        Ok(())
    }
}

fn expire(state: &mut State) {
    if state.mode.is_none()
        && state
            .loaded_at
            .is_some_and(|at| at.elapsed() >= state.limit)
    {
        state.expired = true;
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LaunchArgs {
    mode: Mode,
}

pub(in crate::desktop::app) fn register(
    registry: &mut Registry,
    host: Arc<dyn Host>,
    context: &ModuleContext,
) -> Result<(), AlefError> {
    let state = context.launch.clone();
    registry
        .command::<()>("app.mode")?
        .handler(move |_ctx, ()| {
            let state = state.clone();
            async move {
                json(&match state.mode() {
                    None => "undecided",
                    Some(Mode::Cli) => "cli",
                    Some(Mode::Foreground) => "foreground",
                    Some(Mode::Window) => "window",
                })
            }
        })?;
    let state = context.launch.clone();
    registry
        .command::<LaunchArgs>("app.launch")?
        .handler(move |_ctx, args| {
            let (state, host) = (state.clone(), host.clone());
            async move {
                state.launch(args.mode, host.as_ref())?;
                json(&())
            }
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alef_core::registry::{
        host::{HostFuture, Theme},
        window::UiCall,
    };
    use serde_json::Value;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct TestHost(AtomicUsize);
    impl Host for TestHost {
        fn quit(&self, _: i32) {}
        fn theme(&self) -> Theme {
            Theme::Light
        }
        fn ui(&self, _: u64, _: UiCall) -> HostFuture {
            Box::pin(async { Ok(Value::Null) })
        }
        fn emit(&self, _: Option<u64>, _: &str, _: Value) {}
        fn launch_window(&self) -> Result<(), AlefError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[test]
    fn the_default_state_is_settled_foreground_and_does_not_allow_stdio() {
        let state = LaunchState::default();
        assert_eq!(state.mode(), Some(Mode::Foreground));
        assert!(!state.stdio_allowed());
        state.load_started();
        state.loaded();
        assert!(!state.check_timeout());
    }

    #[test]
    fn launch_selects_once_and_repeating_the_same_mode_is_a_noop_but_a_different_mode_is_rejected()
    {
        let host = TestHost::default();
        let state = LaunchState::new(
            vec![Mode::Cli, Mode::Foreground, Mode::Window],
            None,
            LAUNCH_LIMIT,
        )
        .unwrap();
        assert_eq!(state.mode(), None);
        state.launch(Mode::Window, &host).unwrap();
        state.clone().launch(Mode::Window, &host).unwrap();
        assert_eq!(host.0.load(Ordering::SeqCst), 1);
        assert_eq!(state.mode(), Some(Mode::Window));
        assert_eq!(
            state.launch(Mode::Cli, &host).unwrap_err().code,
            ErrorCode::AlreadyExists
        );
        assert_eq!(state.mode(), Some(Mode::Window));
    }

    #[test]
    fn a_mode_outside_the_allowed_set_does_not_select_or_call_the_host() {
        let host = TestHost::default();
        let state = LaunchState::new(vec![Mode::Cli], None, LAUNCH_LIMIT).unwrap();
        assert_eq!(
            state.launch(Mode::Window, &host).unwrap_err().code,
            ErrorCode::InvalidArgument
        );
        assert_eq!(host.0.load(Ordering::SeqCst), 0);
        assert_eq!(state.mode(), None);
        assert!(LaunchState::new(vec![], None, LAUNCH_LIMIT).is_err());
        assert!(LaunchState::new(vec![Mode::Cli], Some(Mode::Window), LAUNCH_LIMIT).is_err());
    }

    #[test]
    fn expiry_starts_only_after_loading_and_is_reported_once_and_a_late_launch_cannot_win() {
        let host = TestHost::default();
        let state = LaunchState::new(vec![Mode::Cli], None, Duration::ZERO).unwrap();
        assert!(!state.check_timeout());
        state.load_started();
        assert!(!state.check_timeout());
        state.loaded();
        assert!(state.check_timeout());
        assert!(!state.check_timeout());
        assert_eq!(
            state.launch(Mode::Cli, &host).unwrap_err().code,
            ErrorCode::Timeout
        );
        assert_eq!(state.mode(), None);
        state.load_started();
        state.loaded();
        assert!(!state.check_timeout());
        let late = LaunchState::new(vec![Mode::Cli], None, Duration::ZERO).unwrap();
        late.load_started();
        late.loaded();
        assert_eq!(
            late.launch(Mode::Cli, &host).unwrap_err().code,
            ErrorCode::Timeout
        );
        assert!(late.check_timeout());
        assert!(!late.check_timeout());
    }

    #[test]
    fn selection_before_loading_disables_expiration() {
        let state = LaunchState::new(vec![Mode::Cli], None, Duration::ZERO).unwrap();
        state.launch(Mode::Cli, &TestHost::default()).unwrap();
        state.load_started();
        state.loaded();
        assert!(!state.check_timeout());
        assert!(state.stdio_allowed());
        for mode in [Mode::Foreground, Mode::Window] {
            assert!(!LaunchState::new(vec![mode], Some(mode), LAUNCH_LIMIT)
                .unwrap()
                .stdio_allowed());
        }
    }

    #[test]
    fn concurrent_choices_have_exactly_one_winner() {
        let state =
            LaunchState::new(vec![Mode::Cli, Mode::Foreground], None, LAUNCH_LIMIT).unwrap();
        std::thread::scope(|scope| {
            let a = scope.spawn(|| state.launch(Mode::Cli, &TestHost::default()));
            let b = scope.spawn(|| state.launch(Mode::Foreground, &TestHost::default()));
            let results = [a.join().unwrap(), b.join().unwrap()];
            assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
            assert_eq!(
                results.into_iter().find_map(Result::err).unwrap().code,
                ErrorCode::AlreadyExists
            );
        });
    }
}
