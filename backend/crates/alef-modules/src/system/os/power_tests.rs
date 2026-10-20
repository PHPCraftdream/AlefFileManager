// SPDX-License-Identifier: MIT OR Apache-2.0
use super::forwarding_sink;
use super::power::{
    self, MissingPower, PowerBackend, PowerEvent, PowerSink, PowerWatch, PretendPower,
};
use alef_core::session::BlockingExecutor;
use alef_core::{registry::host::Host, AlefError, ErrorCode};
use std::{
    num::NonZeroUsize,
    sync::{mpsc, Arc, Mutex},
    time::Duration,
};
use tokio::sync::OnceCell;

const LIMIT: Duration = Duration::from_secs(5);

#[test]
fn power_event_names_map_to_exact_os_event_names() {
    assert_eq!(PowerEvent::Suspend.name(), "os.suspend");
    assert_eq!(PowerEvent::Resume.name(), "os.resume");
}

#[test]
fn missing_power_returns_the_exact_unavailable_error() {
    let error = MissingPower.watch(Arc::new(|_| {})).unwrap_err();
    assert_eq!(error.code, ErrorCode::NotAvailable);
    assert_eq!(error.message, "no power notifications here");
}

#[test]
fn power_backend_simulation_defaults_to_false() {
    #[derive(Debug)]
    struct DefaultSimulation;
    impl PowerBackend for DefaultSimulation {
        fn watch(&self, _: PowerSink) -> Result<super::power::PowerWatch, alef_core::AlefError> {
            unreachable!()
        }
    }
    assert!(!DefaultSimulation.simulate(PowerEvent::Suspend));
}

#[test]
fn pretend_power_simulation_is_available_without_sinks() {
    assert!(PretendPower::default().simulate(PowerEvent::Suspend));
}

#[test]
fn pretend_power_delivers_both_events_to_multiple_sinks() {
    let power = PretendPower::default();
    let (tx, rx) = mpsc::channel();
    let first_tx = tx.clone();
    let _first = power
        .watch(Arc::new(move |event| {
            first_tx.send((1, event)).unwrap();
        }))
        .unwrap();
    let _second = power
        .watch(Arc::new(move |event| {
            tx.send((2, event)).unwrap();
        }))
        .unwrap();
    assert!(power.simulate(PowerEvent::Suspend));
    assert!(power.simulate(PowerEvent::Resume));
    let mut received = (0..4)
        .map(|_| rx.recv_timeout(Duration::from_secs(1)).unwrap())
        .collect::<Vec<_>>();
    received.sort_by_key(|(id, event)| (*id, event.name()));
    assert_eq!(
        received,
        vec![
            (1, PowerEvent::Resume),
            (1, PowerEvent::Suspend),
            (2, PowerEvent::Resume),
            (2, PowerEvent::Suspend)
        ]
    );
}

#[test]
fn dropping_guards_unregisters_registrations_independently() {
    let power = PretendPower::default();
    let (tx, rx) = mpsc::channel();
    let first_tx = tx.clone();
    let first = power
        .watch(Arc::new(move |_| {
            first_tx.send(1).unwrap();
        }))
        .unwrap();
    let _second = power
        .watch(Arc::new(move |_| {
            tx.send(2).unwrap();
        }))
        .unwrap();
    drop(first);
    assert!(power.simulate(PowerEvent::Resume));
    assert_eq!(rx.recv_timeout(Duration::from_secs(1)).unwrap(), 2);
    drop(_second);
    assert!(power.simulate(PowerEvent::Resume));
    assert!(rx.try_recv().is_err());
}

#[test]
fn sink_reenters_watch_without_deadlocking() {
    let power = Arc::new(PretendPower::default());
    let (tx, rx) = mpsc::channel();
    let nested_power = Arc::clone(&power);
    let sink = Arc::new(move |_| {
        let nested_tx = tx.clone();
        let nested = nested_power
            .watch(Arc::new(move |_| {
                let _ = nested_tx.send(());
            }))
            .unwrap();
        let _ = tx.send(());
        drop(nested);
    });
    let _outer = power.watch(sink).unwrap();
    let dispatch_power = Arc::clone(&power);
    let dispatch = std::thread::spawn(move || {
        dispatch_power.simulate(PowerEvent::Suspend);
    });
    rx.recv_timeout(Duration::from_secs(2))
        .expect("reentrant watch should complete");
    dispatch.join().expect("dispatch thread should complete");
}

/// A `Host` double that records `emit` calls; everything else fails or is inert.
#[derive(Debug, Default)]
struct RecordingHost {
    emitted: std::sync::Mutex<Vec<(Option<u64>, String, serde_json::Value)>>,
}

impl Host for RecordingHost {
    fn quit(&self, _code: i32) {}
    fn theme(&self) -> alef_core::registry::host::Theme {
        alef_core::registry::host::Theme::Light
    }
    fn ui(
        &self,
        _caller: u64,
        _call: alef_core::registry::window::UiCall,
    ) -> alef_core::registry::host::HostFuture {
        Box::pin(async {
            Err(AlefError::new(
                ErrorCode::NotAvailable,
                "no ui in this test",
            ))
        })
    }
    fn emit(&self, window: Option<u64>, name: &str, payload: serde_json::Value) {
        self.emitted
            .lock()
            .expect("emit recording lock poisoned")
            .push((window, name.to_owned(), payload));
    }
}

/// A backend that counts `watch` attempts and successful registrations separately and can
/// fail on demand. Only successful registrations increment `watches`.
#[derive(Debug, Default)]
struct CountingPower {
    attempts: std::sync::atomic::AtomicUsize,
    watches: std::sync::atomic::AtomicUsize,
    fail: std::sync::atomic::AtomicBool,
}

impl PowerBackend for CountingPower {
    fn watch(&self, _sink: PowerSink) -> Result<PowerWatch, AlefError> {
        self.attempts
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.fail.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(AlefError::new(ErrorCode::NotAvailable, "watch refused"));
        }
        self.watches
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(PowerWatch::new(|| {}))
    }
}

/// A backend whose first `watch` signals `entered` right before it blocks on the gate; only
/// a registration that passes the gate counts as successful.
#[derive(Debug, Default)]
struct GatedPower {
    gate: Mutex<Option<mpsc::Receiver<()>>>,
    entered: Mutex<Option<mpsc::Sender<()>>>,
    watches: std::sync::atomic::AtomicUsize,
}

impl GatedPower {
    fn new(gate: mpsc::Receiver<()>, entered: mpsc::Sender<()>) -> Self {
        Self {
            gate: Mutex::new(Some(gate)),
            entered: Mutex::new(Some(entered)),
            watches: Default::default(),
        }
    }
}

impl PowerBackend for GatedPower {
    fn watch(&self, _sink: PowerSink) -> Result<PowerWatch, AlefError> {
        let gate = self.gate.lock().expect("gate lock poisoned").take();
        if let Some(gate) = gate {
            if let Some(entered) = self.entered.lock().expect("gate lock poisoned").take() {
                // Report entry before blocking; a lost signal is harmless for the tests.
                let _ = entered.send(());
            }
            // The gate is a test fixture: a five second bound turns a lost release into an
            // ordinary failure instead of a hang.
            match gate.recv_timeout(Duration::from_secs(5)) {
                Ok(()) => {}
                Err(_) => {
                    // The gate closed or timed out before releasing: this attempt does not
                    // count.
                    return Err(AlefError::new(ErrorCode::NotAvailable, "gate closed"));
                }
            }
        }
        self.watches
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(PowerWatch::new(|| {}))
    }
}

fn inert_sink() -> PowerSink {
    Arc::new(|_| {})
}

fn executor() -> BlockingExecutor {
    // Capacity two: one abandoned (detached) blocking job can coexist with the next
    // registration attempt without tripping `Busy`.
    BlockingExecutor::new(NonZeroUsize::new(2).expect("non-zero test limit"))
}

#[tokio::test]
async fn two_watch_power_calls_start_exactly_one_underlying_watch() {
    let backend = Arc::new(CountingPower::default());
    let cell = OnceCell::new();
    for _ in 0..2 {
        tokio::time::timeout(
            LIMIT,
            power::ensure_watching_in(&cell, executor(), backend.clone(), inert_sink()),
        )
        .await
        .expect("test finishes within the deadline")
        .expect("watching starts");
    }
    assert_eq!(backend.watches.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_failed_watch_frees_the_singleton_for_a_later_retry() {
    let backend = Arc::new(CountingPower::default());
    backend
        .fail
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let cell = OnceCell::new();
    let first = tokio::time::timeout(
        LIMIT,
        power::ensure_watching_in(&cell, executor(), backend.clone(), inert_sink()),
    )
    .await
    .expect("test finishes within the deadline");
    assert_eq!(first.unwrap_err().code, ErrorCode::NotAvailable);
    backend
        .fail
        .store(false, std::sync::atomic::Ordering::SeqCst);
    tokio::time::timeout(
        LIMIT,
        power::ensure_watching_in(&cell, executor(), backend.clone(), inert_sink()),
    )
    .await
    .expect("test finishes within the deadline")
    .expect("the retry starts watching");
    assert_eq!(backend.watches.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test]
async fn power_events_reach_the_host_with_the_exact_event_payloads() {
    let backend = Arc::new(PretendPower::default());
    let host = Arc::new(RecordingHost::default());
    let cell = OnceCell::new();
    tokio::time::timeout(
        LIMIT,
        power::ensure_watching_in(&cell, executor(), backend.clone(), {
            let host_dyn: Arc<dyn Host> = host.clone();
            forwarding_sink(&host_dyn)
        }),
    )
    .await
    .expect("test finishes within the deadline")
    .expect("watching starts");
    assert!(backend.simulate(PowerEvent::Suspend));
    assert!(backend.simulate(PowerEvent::Resume));
    let emitted = host.emitted.lock().expect("emit recording lock poisoned");
    assert_eq!(
        *emitted,
        vec![
            (None, "os.suspend".to_owned(), serde_json::json!({})),
            (None, "os.resume".to_owned(), serde_json::json!({})),
        ]
    );
}

#[test]
fn simulate_power_arguments_accept_only_suspend_and_resume() {
    assert_eq!(
        power::parse_power_event("hibernate").unwrap_err().code,
        ErrorCode::InvalidArgument
    );
    assert_eq!(
        power::parse_power_event("suspend").unwrap(),
        PowerEvent::Suspend
    );
    assert_eq!(
        power::parse_power_event("resume").unwrap(),
        PowerEvent::Resume
    );
}

#[test]
fn simulate_availability_follows_the_backend_and_missing_power_simulates_nothing() {
    assert!(PretendPower::default().simulate(PowerEvent::Resume));
    assert!(!MissingPower.simulate(PowerEvent::Resume));
}

/// Waits (bounded) for the gated backend to report that its registration entered the gate.
async fn wait_entered(entered: &mpsc::Receiver<()>, message: &str) {
    let deadline = tokio::time::Instant::now() + LIMIT;
    loop {
        if entered.try_recv().is_ok() {
            return;
        }
        assert!(tokio::time::Instant::now() < deadline, "{message}");
        tokio::task::yield_now().await;
    }
}

#[tokio::test]
async fn a_slow_first_registration_keeps_the_second_caller_waiting_until_it_finishes() {
    let (release_tx, release_rx) = mpsc::channel();
    let (entered_tx, entered_rx) = mpsc::channel();
    let backend = Arc::new(GatedPower::new(release_rx, entered_tx));
    let cell: &'static OnceCell<PowerWatch> = Box::leak(Box::new(OnceCell::new()));
    let sink = inert_sink();
    let first = tokio::spawn(power::ensure_watching_in(
        cell,
        executor(),
        backend.clone(),
        sink.clone(),
    ));
    // The second caller starts only after the first registration is inside the gate, so the
    // first caller is the initializer and the second one is certainly the waiter.
    wait_entered(&entered_rx, "the first registration never reached the gate").await;
    let mut second = tokio::spawn(power::ensure_watching_in(
        cell,
        executor(),
        backend.clone(),
        sink,
    ));
    // The second caller shares the cell and cannot finish before the first registration
    // passes its gate, so a short bounded poll must time out.
    let not_finished = tokio::time::timeout(Duration::from_millis(50), &mut second).await;
    assert!(
        not_finished.is_err(),
        "the second caller finished before the first registration was released"
    );
    release_tx.send(()).expect("the gated watch is waiting");
    tokio::time::timeout(LIMIT, first)
        .await
        .expect("test finishes within the deadline")
        .expect("first task joins")
        .expect("first registration succeeds");
    tokio::time::timeout(LIMIT, second)
        .await
        .expect("test finishes within the deadline")
        .expect("second task joins")
        .expect("second caller shares the result");
    assert_eq!(backend.watches.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_failing_registration_gives_both_concurrent_callers_the_error_and_a_later_call_retries() {
    let backend = Arc::new(CountingPower::default());
    backend
        .fail
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let cell = OnceCell::new();
    let sink = inert_sink();
    let (first, second) = tokio::time::timeout(LIMIT, async {
        tokio::join!(
            power::ensure_watching_in(&cell, executor(), backend.clone(), sink.clone()),
            power::ensure_watching_in(&cell, executor(), backend.clone(), sink),
        )
    })
    .await
    .expect("both concurrent calls finish within the deadline");
    assert_eq!(first.unwrap_err().code, ErrorCode::NotAvailable);
    assert_eq!(second.unwrap_err().code, ErrorCode::NotAvailable);
    backend
        .fail
        .store(false, std::sync::atomic::Ordering::SeqCst);
    tokio::time::timeout(
        LIMIT,
        power::ensure_watching_in(&cell, executor(), backend.clone(), inert_sink()),
    )
    .await
    .expect("test finishes within the deadline")
    .expect("the retried registration succeeds");
    // Three `watch` invocations happened: the first caller's attempt, the waiting caller's
    // own retry (tokio's `OnceCell` does not share failures — after the first attempt fails,
    // a waiter runs its own initialization, which fails again while `fail` is still set),
    // and the later successful retry. Only the last one is a real registration.
    assert_eq!(
        backend.attempts.load(std::sync::atomic::Ordering::SeqCst),
        3
    );
    assert_eq!(backend.watches.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test]
async fn an_aborted_first_call_leaves_the_watcher_startable_and_a_retry_is_busy_until_the_abandoned_job_ends(
) {
    let (release_tx, release_rx) = mpsc::channel();
    let (entered_tx, entered_rx) = mpsc::channel();
    let backend = Arc::new(GatedPower::new(release_rx, entered_tx));
    let cell: &'static OnceCell<PowerWatch> = Box::leak(Box::new(OnceCell::new()));
    // One shared capacity-one executor, as in production: the abandoned blocking job keeps
    // the only slot until it returns, whatever happened to the call that submitted it.
    let shared = BlockingExecutor::new(NonZeroUsize::new(1).expect("non-zero test limit"));
    let task = tokio::spawn(power::ensure_watching_in(
        cell,
        shared.clone(),
        backend.clone(),
        inert_sink(),
    ));
    wait_entered(&entered_rx, "the first call never reached the gate").await;
    task.abort();
    // The job is still blocked on the gate and holds the slot: a retry is refused with the
    // retryable Busy (it may first wait for the cell of the dropped initializer).
    let busy = tokio::time::timeout(
        LIMIT,
        power::ensure_watching_in(cell, shared.clone(), backend.clone(), inert_sink()),
    )
    .await
    .expect("the retry finishes within the deadline");
    assert_eq!(busy.unwrap_err().code, ErrorCode::Busy);
    // Let the abandoned job go; its slot frees when it returns, which only the retry loop can
    // observe, so retry (bounded) until the answer is no longer Busy.
    release_tx.send(()).expect("the gated watch is waiting");
    let started = tokio::time::timeout(LIMIT, async {
        loop {
            match power::ensure_watching_in(cell, shared.clone(), backend.clone(), inert_sink())
                .await
            {
                Err(error) if error.code == ErrorCode::Busy => {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                other => break other,
            }
        }
    })
    .await
    .expect("the slot of the abandoned job is freed within the deadline");
    started.expect("the watcher starts after the aborted first call");
    assert!(backend.watches.load(std::sync::atomic::Ordering::SeqCst) >= 1);
}
