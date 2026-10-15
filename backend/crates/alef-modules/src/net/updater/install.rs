// SPDX-License-Identifier: MIT OR Apache-2.0
//! Applying a downloaded, verified update to the installed application and restarting.
use super::{storage, State, UpdaterContext};
use alef_core::{
    registry::{command::Reply, dispatch::Registry},
    AlefError, ErrorCode,
};
use serde::Deserialize;
use serde_json::json;
use std::{future::Future, path::Path, path::PathBuf, pin::Pin, sync::Arc};
use tokio::sync::Semaphore;

/// The launcher-side half of `updater.install`: the transactional store install and the
/// restart. Every failure leaves the installed old version and its artifacts untouched.
pub trait UpdateApplier: Send + Sync {
    /// Installs the verified package into the user store; returns the installed package path.
    fn apply<'a>(
        &'a self,
        package: &'a Path,
    ) -> Pin<Box<dyn Future<Output = Result<PathBuf, AlefError>> + Send + 'a>>;
    /// Asks `before-quit`, starts the new version detached and ends this process. The caller
    /// never resolves the JS promise afterwards; an error leaves the old process running.
    fn relaunch<'a>(
        &'a self,
        package: &'a Path,
    ) -> Pin<Box<dyn Future<Output = Result<(), AlefError>> + Send + 'a>>;
}

pub(super) fn register(
    registry: &mut Registry,
    context: &Option<UpdaterContext>,
    state: Arc<State>,
    gate: Arc<Semaphore>,
) -> Result<(), AlefError> {
    let config = context.clone();
    registry
        .command::<Empty>("updater.install")?
        .waits()
        .handler(move |_ctx, _args| {
            let config = config.clone();
            let state = state.clone();
            let gate = gate.clone();
            async move {
                let config = config.ok_or_else(|| {
                    AlefError::new(
                        ErrorCode::UpdateNotInstalled,
                        "the running application is not installed from the store",
                    )
                })?;
                install(config, state, gate).await?;
                // The promise never resolves: on success the process is replaced.
                std::future::pending::<()>().await;
                Err(AlefError::new(ErrorCode::Internal, "unreachable"))
            }
        })?;
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

/// Local updater-storage budget: at most two queued or running jobs.
/// This is not a global or per-session framework limit.
fn executor() -> &'static alef_core::session::BlockingExecutor {
    static EXECUTOR: std::sync::OnceLock<alef_core::session::BlockingExecutor> =
        std::sync::OnceLock::new();
    EXECUTOR.get_or_init(|| {
        alef_core::session::BlockingExecutor::new(
            std::num::NonZeroUsize::new(2).expect("non-zero updater job budget"),
        )
    })
}

pub(super) async fn install(
    config: UpdaterContext,
    state: Arc<State>,
    gate: Arc<Semaphore>,
) -> Result<Reply, AlefError> {
    install_on(executor(), config, state, gate).await
}

pub(super) async fn install_on(
    executor: &alef_core::session::BlockingExecutor,
    config: UpdaterContext,
    state: Arc<State>,
    gate: Arc<Semaphore>,
) -> Result<Reply, AlefError> {
    let _permit = gate
        .try_acquire_owned()
        .map_err(|_| AlefError::new(ErrorCode::UpdateBusy, "updater operation already active"))?;
    let _installed = config.installed.as_ref().ok_or_else(|| {
        AlefError::new(
            ErrorCode::UpdateNotInstalled,
            "the running application is not installed from the store",
        )
    })?;
    let offered = state
        .offered
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .ok_or_else(|| {
            AlefError::new(
                ErrorCode::UpdateNotDownloaded,
                "no downloaded update is offered",
            )
        })?;
    alef_core::security::manifest::validate_package_version(&offered.version)
        .map_err(|_| AlefError::new(ErrorCode::UpdateBadDocument, "invalid offered version"))?;
    // File/dir locking must not stall the BUS poll: prepare on the blocking pool.
    let data = config.data.clone();
    let storage = executor
        .submit(move || storage::Storage::prepare(&data))?
        .result()
        .await??;
    let required_runtime = storage::verify_stored_package(&storage, &config, &offered).await?;
    if let Some(range) = required_runtime {
        if !super::runtime_satisfied(&config, &range) {
            return Err(AlefError::new(
                ErrorCode::UpdateRuntimeRequired,
                format!(
                    "the update requires runtime {range}; install it with `alef runtime install <version> <path-to-alef-binary>`"
                ),
            ));
        }
    }
    let path = config
        .data
        .join("updates")
        .join(format!("{}.alef", offered.version));
    let installed_path = config.applier.apply(&path).await?;
    config.applier.relaunch(&installed_path).await?;
    Ok(Reply::Json(json!(null)))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::{install, install_on, UpdateApplier};
    use crate::net::updater::{
        document, register_updater,
        tests::{
            offered, offered_with_runtime, permissions, session, signed_context, signed_package,
            signed_package_with_runtime, verify_bytes, Server, WAIT,
        },
        State, UpdaterContext,
    };
    use alef_core::{
        registry::{command::Reply, context::CallContext, dispatch::Registry},
        AlefError, ErrorCode,
    };
    use std::{
        future::Future,
        path::PathBuf,
        pin::Pin,
        sync::{Arc, Mutex},
        time::Duration,
    };
    use tokio::sync::Semaphore;

    pub(crate) struct FakeApplier {
        calls: Mutex<Vec<(&'static str, PathBuf)>>,
        apply_result: Option<AlefError>,
        relaunch_result: Option<AlefError>,
    }
    impl Default for FakeApplier {
        fn default() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                apply_result: None,
                relaunch_result: None,
            }
        }
    }
    impl FakeApplier {
        fn calls(&self) -> Vec<(&'static str, PathBuf)> {
            self.calls.lock().unwrap_or_else(|e| e.into_inner()).clone()
        }
    }
    impl UpdateApplier for FakeApplier {
        fn apply<'a>(
            &'a self,
            package: &'a std::path::Path,
        ) -> Pin<Box<dyn Future<Output = Result<PathBuf, AlefError>> + Send + 'a>> {
            self.calls
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(("apply", package.to_path_buf()));
            let result = match &self.apply_result {
                Some(error) => Err(error.clone()),
                None => Ok(package.with_file_name("installed-by-apply.alef")),
            };
            Box::pin(async move { result })
        }
        fn relaunch<'a>(
            &'a self,
            package: &'a std::path::Path,
        ) -> Pin<Box<dyn Future<Output = Result<(), AlefError>> + Send + 'a>> {
            self.calls
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(("relaunch", package.to_path_buf()));
            let result = match &self.relaunch_result {
                Some(error) => Err(error.clone()),
                None => Ok(()),
            };
            Box::pin(async move { result })
        }
    }

    fn install_state(offered: Option<document::Document>) -> Arc<State> {
        let state = Arc::new(State::default());
        *state.offered.lock().unwrap_or_else(|e| e.into_inner()) = offered;
        state
    }

    async fn run_install(
        config: UpdaterContext,
        state: Arc<State>,
        gate: Arc<Semaphore>,
    ) -> Result<Reply, AlefError> {
        tokio::time::timeout(WAIT, install(config, state, gate))
            .await
            .expect("install timed out")
    }

    #[tokio::test]
    async fn install_refuses_an_update_whose_runtime_no_installed_version_satisfies() {
        let scratch = tempfile::tempdir().unwrap();
        let (package, key) =
            signed_package_with_runtime("2.0.0", "org.example.app", Some("^9.0.0"));
        let mut config = signed_context(scratch.path(), key);
        config.runtime_version = "1.0.0".into();
        config.installed_runtime_versions = Vec::new();
        verify_bytes(scratch.path(), &config, "2.0.0", &package)
            .await
            .unwrap();
        let fake = Arc::new(FakeApplier::default());
        config.applier = fake.clone();
        let error = run_install(
            config,
            install_state(Some(offered("2.0.0"))),
            Arc::new(Semaphore::new(1)),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::UpdateRuntimeRequired);
        assert!(error.message.contains("^9.0.0"), "{error:?}");
        assert!(error.message.contains("alef runtime install"), "{error:?}");
        assert!(fake.calls().is_empty());
        assert_eq!(
            std::fs::read(scratch.path().join("updates/2.0.0.alef")).unwrap(),
            package
        );
    }

    #[tokio::test]
    async fn install_refuses_an_update_whose_installed_versions_do_not_satisfy_the_range() {
        let scratch = tempfile::tempdir().unwrap();
        let (package, key) =
            signed_package_with_runtime("2.0.0", "org.example.app", Some("^9.0.0"));
        let mut config = signed_context(scratch.path(), key);
        config.runtime_version = "1.0.0".into();
        config.installed_runtime_versions = vec!["1.5.0".into()];
        verify_bytes(scratch.path(), &config, "2.0.0", &package)
            .await
            .unwrap();
        let fake = Arc::new(FakeApplier::default());
        config.applier = fake.clone();
        let error = run_install(
            config,
            install_state(Some(offered("2.0.0"))),
            Arc::new(Semaphore::new(1)),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::UpdateRuntimeRequired);
        assert!(error.message.contains("^9.0.0"), "{error:?}");
        assert!(error.message.contains("alef runtime install"), "{error:?}");
        assert!(fake.calls().is_empty());
        assert_eq!(
            std::fs::read(scratch.path().join("updates/2.0.0.alef")).unwrap(),
            package
        );
    }

    #[tokio::test]
    async fn install_proceeds_when_an_installed_runtime_version_satisfies_the_range() {
        let scratch = tempfile::tempdir().unwrap();
        let (package, key) =
            signed_package_with_runtime("2.0.0", "org.example.app", Some("^9.0.0"));
        let mut config = signed_context(scratch.path(), key);
        config.runtime_version = "1.0.0".into();
        config.installed_runtime_versions = vec!["9.0.0".into()];
        verify_bytes(scratch.path(), &config, "2.0.0", &package)
            .await
            .unwrap();
        let fake = Arc::new(FakeApplier::default());
        config.applier = fake.clone();
        run_install(
            config,
            install_state(Some(offered("2.0.0"))),
            Arc::new(Semaphore::new(1)),
        )
        .await
        .unwrap();
        let calls = fake.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(
            calls[0],
            ("apply", scratch.path().join("updates").join("2.0.0.alef"))
        );
    }

    #[tokio::test]
    async fn install_rejects_a_feed_range_that_differs_from_the_manifest_runtime() {
        let scratch = tempfile::tempdir().unwrap();
        let (package, key) =
            signed_package_with_runtime("2.0.0", "org.example.app", Some("^1.0.0"));
        let mut config = signed_context(scratch.path(), key);
        verify_bytes(scratch.path(), &config, "2.0.0", &package)
            .await
            .unwrap();
        let fake = Arc::new(FakeApplier::default());
        config.applier = fake.clone();
        let error = run_install(
            config,
            install_state(Some(offered_with_runtime("2.0.0", Some("^9.0.0")))),
            Arc::new(Semaphore::new(1)),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::UpdateBadDocument);
        assert!(fake.calls().is_empty());
    }

    #[tokio::test]
    async fn install_requires_an_application_installed_from_the_store() {
        let scratch = tempfile::tempdir().unwrap();
        let mut config = signed_context(scratch.path(), [7u8; 32]);
        config.installed = None;
        let fake = Arc::new(FakeApplier::default());
        config.applier = fake.clone();
        let error = run_install(
            config,
            install_state(Some(offered("2.0.0"))),
            Arc::new(Semaphore::new(1)),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::UpdateNotInstalled);
        assert!(fake.calls().is_empty());
    }

    #[tokio::test]
    async fn install_requires_a_downloaded_update_to_be_offered() {
        let scratch = tempfile::tempdir().unwrap();
        let mut config = signed_context(scratch.path(), [7u8; 32]);
        let fake = Arc::new(FakeApplier::default());
        config.applier = fake.clone();

        let error = run_install(config, install_state(None), Arc::new(Semaphore::new(1)))
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::UpdateNotDownloaded);
        assert!(fake.calls().is_empty());
    }

    #[tokio::test]
    async fn install_rejects_a_tampered_stored_package_without_touching_the_applier() {
        let scratch = tempfile::tempdir().unwrap();
        let mut config = signed_context(scratch.path(), [7u8; 32]);
        let updates = scratch.path().join("updates");
        std::fs::create_dir_all(&updates).unwrap();
        std::fs::write(updates.join("2.0.0.alef"), b"garbage bytes").unwrap();
        let fake = Arc::new(FakeApplier::default());
        config.applier = fake.clone();
        // open_snapshot fails on the garbage file, which maps to UpdateSignatureInvalid.
        let error = run_install(
            config,
            install_state(Some(offered("2.0.0"))),
            Arc::new(Semaphore::new(1)),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::UpdateSignatureInvalid);
        assert!(fake.calls().is_empty());
    }

    #[tokio::test]
    async fn install_rejects_an_offer_that_is_not_higher_than_the_running_version() {
        let scratch = tempfile::tempdir().unwrap();
        let (package, key) = signed_package("1.0.0", "org.example.app");
        let mut config = signed_context(scratch.path(), key);
        let updates = scratch.path().join("updates");
        std::fs::create_dir_all(&updates).unwrap();
        std::fs::write(updates.join("1.0.0.alef"), package).unwrap();
        let fake = Arc::new(FakeApplier::default());
        config.applier = fake.clone();
        let error = run_install(
            config,
            install_state(Some(offered("1.0.0"))),
            Arc::new(Semaphore::new(1)),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::UpdateBadDocument);
        assert!(fake.calls().is_empty());
    }

    #[tokio::test]
    async fn install_propagates_an_applier_error_without_relaunching() {
        let scratch = tempfile::tempdir().unwrap();
        let (package, key) = signed_package("2.0.0", "org.example.app");
        let mut config = signed_context(scratch.path(), key);
        verify_bytes(scratch.path(), &config, "2.0.0", &package)
            .await
            .unwrap();
        let applier = Arc::new(FakeApplier {
            apply_result: Some(AlefError::new(ErrorCode::UpdateIo, "apply failed")),
            ..FakeApplier::default()
        });
        config.applier = applier.clone();
        let error = run_install(
            config,
            install_state(Some(offered("2.0.0"))),
            Arc::new(Semaphore::new(1)),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::UpdateIo);
        let calls = applier.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "apply");
    }

    #[tokio::test]
    async fn install_applies_the_verified_package_and_relaunches_the_result_in_order() {
        let scratch = tempfile::tempdir().unwrap();
        let (package, key) = signed_package("2.0.0", "org.example.app");
        let mut config = signed_context(scratch.path(), key);
        verify_bytes(scratch.path(), &config, "2.0.0", &package)
            .await
            .unwrap();
        let fake = Arc::new(FakeApplier::default());
        config.applier = fake.clone();
        run_install(
            config,
            install_state(Some(offered("2.0.0"))),
            Arc::new(Semaphore::new(1)),
        )
        .await
        .unwrap();
        let calls = fake.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(
            calls[0],
            ("apply", scratch.path().join("updates").join("2.0.0.alef"))
        );
        // The relaunch receives the applier's output, not the verified input.
        assert_eq!(
            calls[1],
            (
                "relaunch",
                scratch
                    .path()
                    .join("updates")
                    .join("installed-by-apply.alef")
            )
        );
    }

    #[tokio::test]
    async fn a_failing_relaunch_keeps_the_error_and_reports_no_second_apply() {
        let scratch = tempfile::tempdir().unwrap();
        let (package, key) = signed_package("2.0.0", "org.example.app");
        let mut config = signed_context(scratch.path(), key);
        verify_bytes(scratch.path(), &config, "2.0.0", &package)
            .await
            .unwrap();
        let fake = Arc::new(FakeApplier {
            relaunch_result: Some(AlefError::new(ErrorCode::UpdateRelaunchFailed, "no")),
            ..FakeApplier::default()
        });
        config.applier = fake.clone();
        let error = run_install(
            config,
            install_state(Some(offered("2.0.0"))),
            Arc::new(Semaphore::new(1)),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::UpdateRelaunchFailed);
        assert_eq!(
            fake.calls(),
            vec![
                ("apply", scratch.path().join("updates").join("2.0.0.alef")),
                (
                    "relaunch",
                    scratch
                        .path()
                        .join("updates")
                        .join("installed-by-apply.alef")
                )
            ]
        );
    }

    #[tokio::test]
    async fn install_reports_a_missing_download_file_as_not_downloaded() {
        let scratch = tempfile::tempdir().unwrap();
        let mut config = signed_context(scratch.path(), [7u8; 32]);
        let fake = Arc::new(FakeApplier::default());
        config.applier = fake.clone();
        // The offer exists, but updates/2.0.0.alef was never written.
        let error = run_install(
            config,
            install_state(Some(offered("2.0.0"))),
            Arc::new(Semaphore::new(1)),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::UpdateNotDownloaded);
        assert!(fake.calls().is_empty());
    }

    #[tokio::test]
    async fn two_concurrent_installs_let_one_win_and_reject_the_other_as_busy() {
        let scratch = tempfile::tempdir().unwrap();
        let (package, key) = signed_package("2.0.0", "org.example.app");
        let mut config = signed_context(scratch.path(), key);
        verify_bytes(scratch.path(), &config, "2.0.0", &package)
            .await
            .unwrap();
        let fake = Arc::new(FakeApplier::default());
        config.applier = fake.clone();
        let config2 = config.clone();
        let state = install_state(Some(offered("2.0.0")));
        let gate = Arc::new(Semaphore::new(1));
        let (first, second) = tokio::join!(
            run_install(config, state.clone(), gate.clone()),
            run_install(config2, state, gate),
        );
        assert!(
            matches!((&first, &second), (Ok(_), Err(e)) if e.code == ErrorCode::UpdateBusy)
                || matches!((&second, &first), (Ok(_), Err(e)) if e.code == ErrorCode::UpdateBusy)
        );
    }

    #[tokio::test]
    async fn the_install_command_does_not_resolve_after_a_successful_install() {
        let scratch = tempfile::tempdir().unwrap();
        let (package, key) = signed_package("2.0.0", "org.example.app");
        let mut config = signed_context(scratch.path(), key);
        verify_bytes(scratch.path(), &config, "2.0.0", &package)
            .await
            .unwrap();
        // A feed that offers exactly the already published 2.0.0.
        let document = serde_json::json!({
            "version": "2.0.0",
            "package": "/good.alef",
            "sha256": "a".repeat(64),
        })
        .to_string()
        .into_bytes();
        let server = Server::start(document, hyper::StatusCode::OK, false).await;
        config.config.endpoint = server.url.clone();
        let fake = Arc::new(FakeApplier::default());
        config.applier = fake.clone();
        let mut registry = Registry::default();
        register_updater(&mut registry, &Some(config)).unwrap();
        let checked = tokio::time::timeout(
            WAIT,
            registry.dispatch(
                "updater.check",
                CallContext::new(session().await, permissions()),
                serde_json::json!({}),
            ),
        )
        .await
        .unwrap();
        assert!(checked.is_ok(), "the feed check must succeed: {checked:?}");
        let call = registry.dispatch(
            "updater.install",
            CallContext::new(session().await, permissions()),
            serde_json::json!({}),
        );
        tokio::pin!(call);
        // Poll the command while waiting for the applier's calls: it must never resolve first.
        let early = tokio::time::timeout(WAIT, async {
            tokio::select! {
                result = call.as_mut() => Some(result),
                () = async {
                    while fake.calls().len() < 2 {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                } => None,
            }
        })
        .await
        .expect("apply and relaunch never ran");
        assert!(
            early.is_none(),
            "the command resolved before the park: {early:?}"
        );
        // With apply and relaunch done, only the deliberate park remains.
        let pending = tokio::time::timeout(Duration::from_millis(200), call.as_mut()).await;
        assert!(
            pending.is_err(),
            "the install command must park, not resolve"
        );
    }

    #[tokio::test]
    async fn install_answers_not_installed_without_an_updater_context() {
        let mut registry = Registry::default();
        register_updater(&mut registry, &None).unwrap();
        let error = tokio::time::timeout(
            WAIT,
            registry.dispatch(
                "updater.install",
                CallContext::new(session().await, permissions()),
                serde_json::json!({}),
            ),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::UpdateNotInstalled);
    }

    #[tokio::test]
    async fn a_second_install_while_one_is_active_is_rejected_as_busy() {
        let scratch = tempfile::tempdir().unwrap();
        let config = signed_context(scratch.path(), [7u8; 32]);
        let gate: Arc<Semaphore> = Arc::new(Semaphore::new(1));
        let held = gate.clone().try_acquire_owned().unwrap();
        let error = run_install(config, install_state(Some(offered("2.0.0"))), gate)
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::UpdateBusy);
        drop(held);
    }

    #[tokio::test]
    async fn a_saturated_executor_refuses_the_install_with_busy() {
        let scratch = tempfile::tempdir().unwrap();
        let (package, key) = signed_package("2.0.0", "org.example.app");
        let mut config = signed_context(scratch.path(), key);
        verify_bytes(scratch.path(), &config, "2.0.0", &package)
            .await
            .unwrap();
        let fake = Arc::new(FakeApplier::default());
        config.applier = fake.clone();
        // A capacity-1 executor already occupied by a blocked job.
        let executor = alef_core::session::BlockingExecutor::new(
            std::num::NonZeroUsize::new(1).expect("non-zero test limit"),
        );
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel::<()>(1);
        let _blocker = executor
            .submit(move || {
                let _ = started_tx.send(());
                let _ = release_rx.recv_timeout(Duration::from_secs(5));
            })
            .expect("blocker admitted");
        tokio::time::timeout(Duration::from_secs(5), started_rx)
            .await
            .expect("blocker started")
            .expect("signal");
        // The admission line `executor.submit(...)?` must refuse with Busy.
        let error = tokio::time::timeout(
            Duration::from_secs(5),
            install_on(
                &executor,
                config,
                install_state(Some(offered("2.0.0"))),
                Arc::new(Semaphore::new(1)),
            ),
        )
        .await
        .expect("install timed out")
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::Busy);
        assert!(fake.calls().is_empty());
        release_tx.send(()).expect("blocker waiting");
    }

    #[tokio::test]
    async fn the_install_yields_to_the_poll_before_its_storage_prepare_is_admitted() {
        let scratch = tempfile::tempdir().unwrap();
        let (package, key) = signed_package("2.0.0", "org.example.app");
        let mut config = signed_context(scratch.path(), key);
        verify_bytes(scratch.path(), &config, "2.0.0", &package)
            .await
            .unwrap();
        let fake = Arc::new(FakeApplier::default());
        config.applier = fake.clone();
        // A fresh data dir: the sentinel must come from the tested call itself.
        let data = tempfile::tempdir().unwrap();
        config.data = data.path().to_path_buf();
        let executor = alef_core::session::BlockingExecutor::new(
            std::num::NonZeroUsize::new(1).expect("non-zero test limit"),
        );
        let mut fut = Box::pin(install_on(
            &executor,
            config,
            install_state(Some(offered("2.0.0"))),
            Arc::new(Semaphore::new(1)),
        ));
        let poll = fut
            .as_mut()
            .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()));
        assert!(
            matches!(poll, std::task::Poll::Pending),
            "the first poll must not prepare the storage"
        );
        assert!(
            !data.path().join("updates").exists(),
            "the first poll prepared the storage inline"
        );
    }

    #[test]
    fn a_queued_storage_prepare_dropped_with_its_call_never_prepares() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .max_blocking_threads(1)
            .enable_all()
            .build()
            .expect("test runtime");
        runtime.block_on(async {
            // Saturate the blocking pool itself: an admitted job of the executor queues behind it.
            let (block_started_tx, block_started_rx) = tokio::sync::oneshot::channel();
            let (release_tx, release_rx) = std::sync::mpsc::sync_channel::<()>(1);
            let blocker = tokio::task::spawn_blocking(move || {
                let _ = block_started_tx.send(());
                let _ = release_rx.recv_timeout(Duration::from_secs(5));
            });
            tokio::time::timeout(Duration::from_secs(5), block_started_rx)
                .await
                .expect("blocker started")
                .expect("signal");
            let data = tempfile::tempdir().unwrap();
            let (_package, key) = signed_package("2.0.0", "org.example.app");
            let mut config = signed_context(data.path(), key);
            config.applier = Arc::new(FakeApplier::default());
            let executor = alef_core::session::BlockingExecutor::new(
                std::num::NonZeroUsize::new(1).expect("non-zero test limit"),
            );
            let mut fut = Box::pin(install_on(
                &executor,
                config,
                install_state(Some(offered("2.0.0"))),
                Arc::new(Semaphore::new(1)),
            ));
            let poll = fut
                .as_mut()
                .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()));
            assert!(
                matches!(poll, std::task::Poll::Pending),
                "the install queues"
            );
            drop(fut); // the queued prepare must be skipped, never run
            release_tx.send(()).expect("blocker waiting");
            // Draining the queue proves the queued closure was skipped before anything could run.
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    match executor.submit(|| ()) {
                        Ok(job) => break job.result().await,
                        Err(error) if error.code == ErrorCode::Busy => {
                            tokio::task::yield_now().await
                        }
                        Err(error) => panic!("unexpected submit error: {error}"),
                    }
                }
            })
            .await
            .expect("queue drained")
            .expect("sentinel completes");
            assert!(
                !data.path().join("updates").exists(),
                "the queued storage prepare ran after its call was dropped"
            );
            drop(blocker);
        });
    }

    #[tokio::test]
    async fn the_install_prepares_its_storage_before_it_verifies_the_package() {
        let scratch = tempfile::tempdir().unwrap();
        let (package, key) = signed_package("2.0.0", "org.example.app");
        let mut config = signed_context(scratch.path(), key);
        verify_bytes(scratch.path(), &config, "2.0.0", &package)
            .await
            .unwrap();
        let fake = Arc::new(FakeApplier::default());
        config.applier = fake.clone();
        // A fresh data dir: the sentinel must come from this call's own prepare.
        let data = tempfile::tempdir().unwrap();
        config.data = data.path().to_path_buf();
        let executor = alef_core::session::BlockingExecutor::new(
            std::num::NonZeroUsize::new(1).expect("non-zero test limit"),
        );
        let error = tokio::time::timeout(
            Duration::from_secs(5),
            install_on(
                &executor,
                config,
                install_state(Some(offered("2.0.0"))),
                Arc::new(Semaphore::new(1)),
            ),
        )
        .await
        .expect("install timed out")
        .expect_err("the verified package is not in the fresh data dir");
        assert_eq!(error.code, ErrorCode::UpdateNotDownloaded);
        // prepare's side effect: the updates directory exists under the fresh config.data.
        assert!(data.path().join("updates").is_dir());
        assert!(fake.calls().is_empty());
    }
}
