// SPDX-License-Identifier: MIT OR Apache-2.0
//! Window identity for transport requests: which window did a `native://` request come from?
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};

use servo::WebViewId;

/// Id the transport uses when a request cannot be tied to a window; no session ever has it,
/// so every authenticated route denies the request.
pub(crate) const UNKNOWN_WINDOW: u64 = 0;

/// Maps webviews to stable window ids (1, 2, ...); shared between the UI thread and the transport.
#[derive(Clone, Default)]
pub(crate) struct WindowRegistry {
    views: Arc<Mutex<HashMap<WebViewId, u64>>>,
    next: Arc<AtomicU64>,
}

impl WindowRegistry {
    /// Reserves an id for a window that is about to create its webview.
    pub(crate) fn allocate(&self) -> u64 {
        self.next.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Ties a created webview to its window id.
    pub(crate) fn bind(&self, view: WebViewId, window: u64) {
        self.views
            .lock()
            .expect("window registry")
            .insert(view, window);
    }

    /// Forgets a closed window.
    pub(crate) fn unbind(&self, window: u64) {
        self.views
            .lock()
            .expect("window registry")
            .retain(|_, id| *id != window);
    }

    /// Window of a request's target webview. A request without a webview belongs to the only
    /// window when there is exactly one; otherwise it belongs to none.
    pub(crate) fn resolve(&self, view: Option<WebViewId>) -> u64 {
        let views = self.views.lock().expect("window registry");
        match view {
            Some(view) => views.get(&view).copied().unwrap_or(UNKNOWN_WINDOW),
            None if views.len() == 1 => views.values().copied().next().unwrap_or(UNKNOWN_WINDOW),
            None => UNKNOWN_WINDOW,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use servo_base::id::{PainterId, PipelineNamespace, PipelineNamespaceId, PIPELINE_NAMESPACE};

    /// Servo ids need a per-thread namespace; every test thread installs its own once.
    fn view() -> WebViewId {
        if PIPELINE_NAMESPACE.with(|namespace| namespace.get().is_none()) {
            PipelineNamespace::install(PipelineNamespaceId(7));
        }
        WebViewId::new(PainterId::next())
    }

    #[test]
    fn allocates_increasing_ids_starting_at_one() {
        let windows = WindowRegistry::default();
        assert_eq!((windows.allocate(), windows.allocate()), (1, 2));
    }

    #[test]
    fn resolves_bound_views_and_forgets_closed_windows() {
        let windows = WindowRegistry::default();
        let (a, b) = (view(), view());
        let (first, second) = (windows.allocate(), windows.allocate());
        windows.bind(a, first);
        windows.bind(b, second);
        assert_eq!(windows.resolve(Some(a)), first);
        assert_eq!(windows.resolve(Some(b)), second);
        assert_eq!(
            windows.resolve(None),
            UNKNOWN_WINDOW,
            "ambiguous without a webview"
        );
        windows.unbind(first);
        assert_eq!(windows.resolve(Some(a)), UNKNOWN_WINDOW);
        assert_eq!(windows.resolve(None), second, "the only window left");
    }

    #[test]
    fn an_unknown_webview_belongs_to_no_window() {
        let windows = WindowRegistry::default();
        windows.bind(view(), windows.allocate());
        assert_eq!(windows.resolve(Some(view())), UNKNOWN_WINDOW);
    }
}
