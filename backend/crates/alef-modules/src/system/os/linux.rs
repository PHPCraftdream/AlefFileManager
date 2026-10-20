// SPDX-License-Identifier: MIT OR Apache-2.0
//! Linux logind power notifications on a dedicated thread driven by `zbus::block_on`.
use super::super::power::{PowerBackend, PowerEvent, PowerSink, PowerWatch};
use alef_core::{AlefError, ErrorCode};
use std::{sync::mpsc, thread, time::Duration};

/// Races `setup` against the stop signal: `Ok(value)` when setup wins, `Err(None)` when the
/// stop signal arrived first (dropped sender or explicit send), `Err(Some(error))` when
/// setup failed on its own. Pure and timer-free: the caller bounds the wait with
/// `recv_timeout` on its own readiness channel.
pub(super) async fn setup_raced_against_stop<T, E>(
    setup: impl std::future::Future<Output = Result<T, E>>,
    stop: tokio::sync::oneshot::Receiver<()>,
) -> Result<T, Option<E>> {
    use futures_util::future::{self, Either};
    match future::select(Box::pin(setup), stop).await {
        Either::Left((Ok(value), _)) => Ok(value),
        Either::Left((Err(error), _)) => Err(Some(error)),
        Either::Right((_, _)) => Err(None),
    }
}

#[derive(Debug, Default)]
pub struct LinuxPower;
impl PowerBackend for LinuxPower {
    fn watch(&self, sink: PowerSink) -> Result<PowerWatch, AlefError> {
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        // Stop scheme: TWO oneshot pairs. `setup_stop` is consumed by the setup race —
        // dropping (or sending on) it interrupts a hung D-Bus connect at once. If setup
        // wins the race, the signal stream is guarded by the second pair, `stream_stop`.
        // Cleanup (the `PowerWatch` closure and the timeout branch) drops both senders;
        // whichever loop the thread is currently in observes its channel close and exits,
        // so a stop request always reaches the thread.
        let (setup_stop_tx, setup_stop_rx) = tokio::sync::oneshot::channel();
        let (stream_stop_tx, stream_stop_rx) = tokio::sync::oneshot::channel();
        let thread = thread::Builder::new()
            .name("alef-power-logind".into())
            .spawn(move || {
                zbus::block_on(async move {
                    use futures_util::future::{self, Either};
                    use futures_util::StreamExt;
                    use zbus::{Connection, Proxy};
                    // There is no timer inside `block_on`: the setup bound lives on the caller
                    // (`ready_rx.recv_timeout` below), so a hung `Connection::system()` would
                    // keep this thread — the stop race below is what ends it.
                    let setup = async {
                        let connection = Connection::system().await?;
                        let dbus = Proxy::new(
                            &connection,
                            "org.freedesktop.DBus",
                            "/org/freedesktop/DBus",
                            "org.freedesktop.DBus",
                        )
                        .await?;
                        let has_owner: bool = dbus
                            .call("NameHasOwner", &("org.freedesktop.login1",))
                            .await?;
                        if !has_owner {
                            return Err(zbus::Error::Failure("logind has no owner".into()));
                        }
                        let manager = Proxy::new(
                            &connection,
                            "org.freedesktop.login1",
                            "/org/freedesktop/login1",
                            "org.freedesktop.login1.Manager",
                        )
                        .await?;
                        let signals = manager.receive_signal("PrepareForSleep").await?;
                        Ok::<_, zbus::Error>((connection, signals))
                    };
                    match setup_raced_against_stop(setup, setup_stop_rx).await {
                        Ok(values) => {
                            let _ = ready_tx.send(Ok(()));
                            let (_connection, mut signals) = values;
                            // `tokio::sync::oneshot` receivers are futures without a runtime,
                            // so the stop signal races the signal stream with `future::select`.
                            // The stop future is consumed only when it wins; after each message
                            // it is restored for the next iteration.
                            let mut stop_future = Some(stream_stop_rx);
                            loop {
                                let next = signals.next();
                                let stop = stop_future
                                    .take()
                                    .expect("stop future kept between messages");
                                match future::select(Box::pin(next), stop).await {
                                    Either::Left((Some(message), returned_stop)) => {
                                        stop_future = Some(returned_stop);
                                        if let Ok((sleeping,)) =
                                            message.body().deserialize::<(bool,)>()
                                        {
                                            sink(if sleeping {
                                                PowerEvent::Suspend
                                            } else {
                                                PowerEvent::Resume
                                            });
                                        }
                                    }
                                    Either::Left((None, _)) | Either::Right((_, _)) => break,
                                }
                            }
                        }
                        // The caller stopped us (dropped sender or explicit send): the readiness
                        // answer would go nowhere, so exit silently.
                        Err(None) => {}
                        Err(Some(error)) => {
                            let _ = ready_tx.send(Err(error.to_string()));
                        }
                    }
                });
            })
            .map_err(|error| AlefError::new(ErrorCode::NotAvailable, error.to_string()))?;
        match ready_rx.recv_timeout(Duration::from_secs(3)) {
            Ok(Ok(())) => Ok(PowerWatch::new(move || {
                let _ = stream_stop_tx.send(());
                drop(setup_stop_tx);
                let _ = thread.join();
            })),
            Ok(Err(message)) => {
                let _ = thread.join();
                Err(AlefError::new(ErrorCode::NotAvailable, message))
            }
            Err(_) => {
                // The thread is detached: a still-running or hung D-Bus connect leaks at most
                // one thread, which exits as soon as the stop channels close and the race
                // resolves. Joining here would block the caller past the three second bound.
                drop(stream_stop_tx);
                drop(setup_stop_tx);
                drop(thread);
                Err(AlefError::new(
                    ErrorCode::NotAvailable,
                    "system bus/logind unavailable",
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::setup_raced_against_stop;
    use std::time::Duration;

    #[tokio::test]
    async fn a_never_completing_setup_is_interrupted_by_a_closed_stop_channel() {
        let (sender, receiver) = tokio::sync::oneshot::channel::<()>();
        let mut task = tokio::spawn(setup_raced_against_stop(
            std::future::pending::<Result<u32, &str>>(),
            receiver,
        ));
        // The setup never finishes, so the race must not resolve while the stop channel is
        // open (`timeout`'s only error is `Elapsed`).
        let not_finished = tokio::time::timeout(Duration::from_millis(100), &mut task).await;
        assert!(
            not_finished.is_err(),
            "the race resolved before the stop channel closed"
        );
        // Closing the stop channel must interrupt the pending setup within the bound.
        drop(sender);
        let resolved = tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("the closed stop channel resolves the race within the bound")
            .expect("the racing task joins");
        assert_eq!(resolved, Err(None));
    }
}
