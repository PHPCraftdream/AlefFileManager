// SPDX-License-Identifier: MIT OR Apache-2.0
// Throwaway M0.5 spike (docs/stages/m0-spikes.md): the application runs in a hidden WebView on a
// software rendering context, with no window and no winit event loop. Started with
// `ALEF_SPIKE_HEADLESS=1 alef --app <folder>`; everything it learns is printed as `HEADLESS ...`.
use std::{
    cell::Cell,
    error::Error,
    io,
    rc::Rc,
    sync::{Arc, Condvar, Mutex},
    time::{Duration, Instant},
};

use servo::{
    ConsoleLogLevel, EventLoopWaker, LoadStatus, Opts, RenderingContext, ServoBuilder,
    SoftwareRenderingContext, WebResourceLoad, WebView, WebViewBuilder, WebViewDelegate,
};
use winit::dpi::PhysicalSize;

use crate::{ui::UiRequest, Bridge};

/// How long the loop sleeps when nobody woke it (`ALEF_SPIKE_IDLE_MS`): `quit` and the requests of the
/// application are looked at then at the latest.
const IDLE_DEFAULT_MS: u64 = 250;

#[derive(Clone, Default)]
struct Waker(Arc<(Mutex<bool>, Condvar)>);

impl Waker {
    /// Sleeps until Servo asks for attention or `limit` passes; says whether it was woken.
    fn sleep(&self, limit: Duration) -> bool {
        let (flag, signal) = &*self.0;
        let mut woken = flag.lock().unwrap_or_else(|e| e.into_inner());
        if !*woken {
            woken = signal
                .wait_timeout(woken, limit)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        std::mem::replace(&mut *woken, false)
    }
}

impl EventLoopWaker for Waker {
    fn clone_box(&self) -> Box<dyn EventLoopWaker> {
        Box::new(self.clone())
    }

    fn wake(&self) {
        let (flag, signal) = &*self.0;
        *flag.lock().unwrap_or_else(|e| e.into_inner()) = true;
        signal.notify_one();
    }
}

#[derive(Default)]
struct Delegate {
    frames: Cell<u64>,
    animating: Cell<bool>,
}

impl WebViewDelegate for Delegate {
    fn notify_new_frame_ready(&self, _: WebView) {
        self.frames.set(self.frames.get() + 1);
    }

    fn notify_animating_changed(&self, _: WebView, animating: bool) {
        self.animating.set(animating);
        eprintln!("HEADLESS animating={animating}");
    }

    fn notify_load_status_changed(&self, _: WebView, status: LoadStatus) {
        eprintln!("HEADLESS load-status {status:?}");
    }

    fn show_console_message(&self, _: WebView, level: ConsoleLogLevel, message: String) {
        eprintln!("Servo {level:?}: {message}");
    }

    fn load_web_resource(&self, _: WebView, load: WebResourceLoad) {
        crate::spikes::origin::web_resource(load);
    }

    fn notify_crashed(&self, _: WebView, reason: String, _: Option<String>) {
        eprintln!("HEADLESS content crashed: {reason}");
    }
}

fn millis() -> u64 {
    std::env::var("ALEF_SPIKE_IDLE_MS")
        .ok()
        .and_then(|text| text.parse().ok())
        .unwrap_or(IDLE_DEFAULT_MS)
}

/// Runs the entry document of `bridge` in a hidden WebView until the application quits. Requests
/// that need a window (`window.*`, dialogs) are refused: there is nobody to show them to.
pub fn run_headless(bridge: &mut Bridge) -> Result<(), Box<dyn Error>> {
    let started = Instant::now();
    let rendering = Rc::new(
        SoftwareRenderingContext::new(PhysicalSize::new(1, 1))
            .map_err(|error| io::Error::other(format!("software context: {error:?}")))?,
    );
    rendering
        .make_current()
        .map_err(|error| io::Error::other(format!("make_current: {error:?}")))?;
    eprintln!(
        "HEADLESS software-context ok after {} ms",
        started.elapsed().as_millis()
    );
    let waker = Waker::default();
    let servo = ServoBuilder::default()
        .opts(Opts {
            multiprocess: false,
            temporary_storage: true,
            ..Opts::default()
        })
        .preferences(crate::spikes::origin::preferences())
        .protocol_registry(bridge.take_registry()?)
        .event_loop_waker(Box::new(waker.clone()))
        .build();
    servo.setup_logging();
    let delegate = Rc::new(Delegate::default());
    let webview = WebViewBuilder::new(&servo, rendering.clone())
        .url(bridge.entry_url.clone())
        .delegate(delegate.clone())
        .build();
    // The transport tells documents apart by the webview: this is window 1 of a process without a window.
    let ids = bridge.windows();
    ids.bind(webview.id(), ids.allocate());
    webview.hide();
    if std::env::var("ALEF_SPIKE_THROTTLE").is_ok_and(|value| value == "1") {
        webview.set_throttled(true);
        eprintln!("HEADLESS webview throttled");
    }
    eprintln!(
        "HEADLESS webview ok after {} ms",
        started.elapsed().as_millis()
    );
    let handle = bridge.handle();
    let mut requests = bridge.take_requests()?;
    let idle = Duration::from_millis(millis());
    let (mut spins, mut wakes) = (0_u64, 0_u64);
    loop {
        servo.spin_event_loop();
        spins += 1;
        while let Ok(request) = requests.try_recv() {
            let refuse = |reply: crate::ui::UiReply| {
                reply.finish(Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "There is no window in the headless mode",
                )))
            };
            match request {
                UiRequest::Ui { reply, .. } | UiRequest::Drop { reply, .. } => refuse(reply),
            }
        }
        if handle.quit_requested() {
            break;
        }
        if waker.sleep(idle) {
            wakes += 1;
        }
    }
    eprintln!(
        "HEADLESS done spins={spins} wakes={wakes} frames={} total_ms={}",
        delegate.frames.get(),
        started.elapsed().as_millis()
    );
    drop(webview);
    drop(servo);
    Ok(())
}
