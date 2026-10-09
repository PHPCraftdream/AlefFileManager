// SPDX-License-Identifier: MIT OR Apache-2.0
//! Deep-link validation, delivery and permission-gated native registration.
use super::integration::deeplink_platform as platform;

use alef_core::{
    registry::dispatch::Registry,
    security::{consent::Decision, permissions::Permission},
};
use std::{
    fmt::Debug,
    path::{Path, PathBuf},
};

/// Synchronous object-safe backend; commands await its work on a blocking worker.
pub trait DeepLinkBackend: Send + Sync + Debug {
    fn apply(
        &self,
        id: &str,
        folder: &Path,
        schemes: &[String],
        register: bool,
    ) -> Result<(), AlefError>;
}

/// Recorded registration call: application id, folder, schemes and register flag.
pub type DeepLinkCall = (String, PathBuf, Vec<String>, bool);

#[derive(Debug, Default)]
pub struct MemoryDeepLinks {
    state: Mutex<VecDeque<DeepLinkCall>>,
}
impl MemoryDeepLinks {
    /// Bounded spy and substitution state, oldest first. Never accesses native integration.
    pub fn calls(&self) -> Vec<DeepLinkCall> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .cloned()
            .collect()
    }
}
impl DeepLinkBackend for MemoryDeepLinks {
    fn apply(
        &self,
        id: &str,
        folder: &Path,
        schemes: &[String],
        register: bool,
    ) -> Result<(), AlefError> {
        super::integration::safe_id(id)?;
        super::integration::path_text(folder)?;
        if !folder.is_absolute() {
            return Err(invalid());
        }
        validate_schemes(schemes)?;
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.len() == 256 {
            state.pop_front();
        }
        state.push_back((id.into(), folder.into(), schemes.to_vec(), register));
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct SystemDeepLinks {
    serial: Mutex<()>,
}
impl DeepLinkBackend for SystemDeepLinks {
    fn apply(
        &self,
        id: &str,
        folder: &Path,
        schemes: &[String],
        register: bool,
    ) -> Result<(), AlefError> {
        validate_schemes(schemes)?;
        let _guard = self
            .serial
            .lock()
            .map_err(|_| super::integration::unavailable("deep-link lock poisoned"))?;
        let launch = super::integration::Launch::resolve(id, folder)?;
        platform::apply(&launch, schemes, register)
    }
}

pub(super) fn register(registry: &mut Registry, context: &ModuleContext) -> Result<(), AlefError> {
    let substitute: Arc<dyn DeepLinkBackend> = Arc::new(MemoryDeepLinks::default());
    for (command, register) in [
        ("app.registerDeepLinks", true),
        ("app.unregisterDeepLinks", false),
    ] {
        let real = context.backends.deep_links.clone();
        let substitute = substitute.clone();
        let id = context.app.id.clone();
        let folder = context.paths.app.clone();
        let schemes = context.deep_link_schemes.clone();
        registry
            .command::<serde_json::Value>(command)?
            .handler(move |ctx, args| {
                let (real, substitute, id, folder, schemes) = (
                    real.clone(),
                    substitute.clone(),
                    id.clone(),
                    folder.clone(),
                    schemes.clone(),
                );
                async move {
                    // Check the complete batch before dispatching either backend. A narrowed/denied
                    // scope aborts the batch; substituted scopes never reach the native backend.
                    if schemes.is_empty() {
                        return Err(AlefError::new(
                            ErrorCode::PermissionDenied,
                            "no declared deep-link schemes",
                        ));
                    }
                    let mut allowed = Vec::new();
                    let mut diverted = Vec::new();
                    for scheme in schemes {
                        match ctx.permissions.check(
                            Permission::AppDeepLinks,
                            Some(&scheme),
                            &ctx.grants(),
                        )? {
                            Decision::Allow => allowed.push(scheme),
                            // `check` refuses a denied scheme itself, so only these two come back.
                            Decision::Substitute | Decision::Deny => diverted.push(scheme),
                        }
                    }
                    if !args.is_null() && !args.as_object().is_some_and(|map| map.is_empty()) {
                        return Err(invalid());
                    }
                    tokio::task::spawn_blocking(move || {
                        if !diverted.is_empty() {
                            substitute.apply(&id, &folder, &diverted, register)?;
                        }
                        if !allowed.is_empty() {
                            real.apply(&id, &folder, &allowed, register)?;
                        }
                        Ok::<_, AlefError>(())
                    })
                    .await
                    .map_err(|e| {
                        AlefError::new(ErrorCode::Internal, format!("deep-link task: {e}"))
                    })??;
                    crate::json(&())
                }
            })?;
    }
    Ok(())
}
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use alef_core::{registry::host::Host, session::Session, AlefError, ErrorCode};
use serde_json::json;
use url::Url;

use super::Interceptor;
use crate::ModuleContext;

pub(crate) const MAX_URLS: usize = 32;
pub(crate) const MAX_URL_BYTES: usize = 8192;

fn invalid() -> AlefError {
    AlefError::new(
        ErrorCode::InvalidArgument,
        "invalid or undeclared deep-link URL",
    )
}

pub(super) fn validate(schemes: &[String], text: &str) -> Result<(), AlefError> {
    if text.len() > MAX_URL_BYTES || text.chars().any(char::is_control) || text.trim() != text {
        return Err(invalid());
    }
    let url = Url::parse(text).map_err(|_| invalid())?;
    if !schemes.iter().any(|scheme| scheme == url.scheme()) {
        return Err(invalid());
    }
    Ok(())
}

fn validate_schemes(schemes: &[String]) -> Result<(), AlefError> {
    if schemes.len() > 8 {
        return Err(invalid());
    }
    for (i, scheme) in schemes.iter().enumerate() {
        if scheme.len() > 64
            || !scheme
                .bytes()
                .next()
                .is_some_and(|b| b.is_ascii_lowercase())
            || !scheme
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"+-.".contains(&b))
            || [
                "http",
                "https",
                "file",
                "ftp",
                "ws",
                "wss",
                "data",
                "blob",
                "javascript",
                "about",
                "mailto",
                "tel",
            ]
            .contains(&scheme.as_str())
            || schemes[..i].contains(scheme)
        {
            return Err(invalid());
        }
    }
    Ok(())
}

#[derive(Default)]
struct Delivery {
    windows: Vec<Interceptor>,
    pending: VecDeque<String>,
    draining: bool,
}

pub(crate) struct DeepLinks {
    schemes: Vec<String>,
    delivery: Mutex<Delivery>,
}

impl DeepLinks {
    pub(crate) fn new(context: &ModuleContext) -> Result<Self, AlefError> {
        // ModuleContext may be constructed without a manifest: enforce the declaration contract
        // here too (lowercase, nonreserved, unique, at most eight, at most 64 bytes).
        let schemes = &context.deep_link_schemes;
        validate_schemes(schemes)?;
        let state = Self::empty(context.deep_link_schemes.clone());
        state.validate_all(&context.startup_urls)?;
        {
            let mut delivery = state.delivery.lock().unwrap_or_else(|e| e.into_inner());
            for url in &context.startup_urls {
                Self::enqueue(&mut delivery, url.clone());
            }
        }
        Ok(state)
    }

    pub(crate) fn empty(schemes: Vec<String>) -> Self {
        Self {
            schemes,
            delivery: Mutex::new(Delivery::default()),
        }
    }

    pub(crate) fn validate_all(&self, urls: &[String]) -> Result<(), AlefError> {
        if urls.len() > MAX_URLS {
            return Err(invalid());
        }
        for url in urls {
            validate(&self.schemes, url)?;
        }
        Ok(())
    }

    fn enqueue(delivery: &mut Delivery, url: String) {
        // Bounded FIFO: overflow explicitly drops the oldest pending URL.
        if delivery.pending.len() == MAX_URLS {
            delivery.pending.pop_front();
        }
        delivery.pending.push_back(url);
    }

    pub(crate) fn receive(&self, urls: &[String], host: &dyn Host) -> Result<(), AlefError> {
        self.validate_all(urls)?; // Atomic validation: no partial emit or enqueue on rejection.
        {
            let mut delivery = self.delivery.lock().unwrap_or_else(|e| e.into_inner());
            for url in urls {
                Self::enqueue(&mut delivery, url.clone());
            }
        }
        self.drain(host);
        Ok(())
    }

    pub(crate) fn intercept(&self, session: &Arc<Session>, enabled: bool, host: &dyn Host) {
        {
            let mut delivery = self.delivery.lock().unwrap_or_else(|e| e.into_inner());
            delivery
                .windows
                .retain(|known| known.alive() && !known.session.ptr_eq(&Arc::downgrade(session)));
            if enabled && session.is_open() {
                delivery
                    .windows
                    .retain(|known| known.window != session.window());
                delivery.windows.push(Interceptor {
                    window: session.window(),
                    session: Arc::downgrade(session),
                });
            }
        }
        self.drain(host);
    }

    fn drain(&self, host: &dyn Host) {
        {
            let mut delivery = self.delivery.lock().unwrap_or_else(|e| e.into_inner());
            if delivery.draining {
                return;
            }
            delivery.draining = true;
        }
        loop {
            let next = {
                // One synchronous coordinator preserves FIFO; never call Host under the state lock.
                // Poison recovery follows the surrounding app state: all fields remain safe owned values.
                let mut delivery = self.delivery.lock().unwrap_or_else(|e| e.into_inner());
                delivery.windows.retain(Interceptor::alive);
                if delivery.windows.is_empty() || delivery.pending.is_empty() {
                    delivery.draining = false;
                    return;
                }
                let windows: Vec<_> = delivery
                    .windows
                    .iter()
                    .filter_map(|known| known.session.upgrade())
                    .collect();
                (
                    delivery.pending.pop_front().expect("nonempty FIFO"),
                    windows,
                )
            };
            for session in next.1 {
                host.emit(
                    Some(session.window()),
                    "app.open-url",
                    json!({ "url": next.0 }),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schemes(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    #[test]
    fn a_url_is_taken_only_whole_clean_and_of_a_declared_scheme() {
        let declared = schemes(&["alef"]);
        for good in ["alef:open", "alef://host/path?q=1"] {
            validate(&declared, good).unwrap();
        }
        // The parser itself drops tabs and line ends inside a URL and the spaces around it.
        for bad in [
            "alef:a\tb",
            "alef:a\nb",
            "alef:a\rb",
            "alef:a\u{7f}b",
            " alef:x",
            "alef:x ",
            "other:x",
            "alef",
            "",
        ] {
            assert!(validate(&declared, bad).is_err(), "{bad:?}");
        }
        let longest = format!("alef:{}", "x".repeat(MAX_URL_BYTES - 5));
        validate(&declared, &longest).unwrap();
        assert!(validate(&declared, &format!("{longest}x")).is_err());
    }

    #[test]
    fn declared_schemes_are_lowercase_unreserved_unique_few_and_short() {
        validate_schemes(&schemes(&["alef", "my-app+v2.x", "a1"])).unwrap();
        validate_schemes(&schemes(&["a", "b", "c", "d", "e", "f", "g", "h"])).unwrap();
        validate_schemes(&[format!("a{}", "b".repeat(63))]).unwrap();
        assert!(
            validate_schemes(&schemes(&["a", "b", "c", "d", "e", "f", "g", "h", "i"])).is_err()
        );
        assert!(validate_schemes(&[format!("a{}", "b".repeat(64))]).is_err());
        for bad in [
            "",
            "1app",
            "+app",
            "-app",
            ".app",
            "aLef",
            "Alef",
            "a b",
            "a_b",
            "é",
            "http",
            "https",
            "file",
            "ftp",
            "ws",
            "wss",
            "data",
            "blob",
            "javascript",
            "about",
            "mailto",
            "tel",
        ] {
            assert!(validate_schemes(&schemes(&[bad])).is_err(), "{bad:?}");
        }
        assert!(validate_schemes(&schemes(&["alef", "other", "alef"])).is_err());
    }
}
