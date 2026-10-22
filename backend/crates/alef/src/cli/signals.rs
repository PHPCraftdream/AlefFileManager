// SPDX-License-Identifier: MIT OR Apache-2.0
//! The console signals that end the process the way `app.quit` would.
use alef_modules::Termination;

/// Waits for the signals that ask a process to end (Ctrl+C, SIGTERM, Ctrl+Break, the closing of the
/// console) and asks the application to quit the way `app.quit` does; the code is 128 and the number of the
/// signal where there is one. When the documents veto, or when another signal comes, the process ends.
pub(crate) async fn end_on_signals(termination: Termination, handle: alef_runtime::RuntimeHandle) {
    let Ok(mut signals) = signals() else {
        eprintln!("alef: the signals cannot be watched");
        return;
    };
    let mut asked = false;
    while let Some(code) = signals.recv().await {
        if asked {
            alef_core::registry::host::Host::quit(&handle, code);
            return;
        }
        if termination.request(code).await {
            return;
        }
        asked = true;
    }
}

/// The signals as exit codes, one by one.
fn signals() -> std::io::Result<tokio::sync::mpsc::Receiver<i32>> {
    use tokio::sync::mpsc;
    let (sender, receiver) = mpsc::channel(4);
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        for (kind, code) in [
            (SignalKind::interrupt(), 130),
            (SignalKind::terminate(), 143),
            (SignalKind::hangup(), 129),
        ] {
            let mut stream = signal(kind)?;
            let sender = sender.clone();
            tokio::spawn(async move {
                while stream.recv().await.is_some() {
                    if sender.send(code).await.is_err() {
                        break;
                    }
                }
            });
        }
    }
    #[cfg(windows)]
    {
        use tokio::signal::windows::{ctrl_break, ctrl_c, ctrl_close, ctrl_shutdown};
        macro_rules! watch {
            ($stream:expr, $code:expr) => {{
                let mut stream = $stream?;
                let sender = sender.clone();
                tokio::spawn(async move {
                    while stream.recv().await.is_some() {
                        if sender.send($code).await.is_err() {
                            break;
                        }
                    }
                });
            }};
        }
        watch!(ctrl_c(), 130);
        watch!(ctrl_break(), 143);
        watch!(ctrl_close(), 143);
        watch!(ctrl_shutdown(), 143);
    }
    Ok(receiver)
}
