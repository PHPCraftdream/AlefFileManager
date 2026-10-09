// SPDX-License-Identifier: MIT OR Apache-2.0
//! Permission-gated autostart. Native blocking work (including path resolution) is awaited off-thread.
use super::integration::{self, unavailable, Launch};
use crate::{json, ModuleContext};
use alef_core::{
    registry::dispatch::Registry,
    security::{consent::Decision, permissions::Permission},
    AlefError, ErrorCode,
};
use serde_json::Value;
use std::{
    collections::{HashMap, VecDeque},
    fmt::Debug,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutostartOperation {
    Enable,
    Disable,
    IsEnabled,
}

/// Object-safe synchronous backend: the module owns off-thread execution.
pub trait AutostartBackend: Send + Sync + Debug {
    fn apply(
        &self,
        id: &str,
        folder: &Path,
        operation: AutostartOperation,
    ) -> Result<bool, AlefError>;
}

#[derive(Debug, Default)]
pub struct MemoryAutostart {
    state: Mutex<MemoryState>,
}
#[derive(Debug, Default)]
struct MemoryState {
    enabled: HashMap<(String, PathBuf), bool>,
    calls: VecDeque<AutostartOperation>,
    failure: Option<String>,
}
impl MemoryAutostart {
    /// Returns the last 256 operations, oldest first.
    pub fn calls(&self) -> Vec<AutostartOperation> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .calls
            .iter()
            .copied()
            .collect()
    }
    pub fn fail_with(&self, message: Option<String>) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).failure = message;
    }
}
impl AutostartBackend for MemoryAutostart {
    fn apply(
        &self,
        id: &str,
        folder: &Path,
        operation: AutostartOperation,
    ) -> Result<bool, AlefError> {
        integration::safe_id(id)?;
        integration::path_text(folder)?;
        if !folder.is_absolute() {
            return Err(unavailable("application folder must be absolute"));
        }
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.calls.len() == 256 {
            state.calls.pop_front();
        }
        state.calls.push_back(operation);
        if let Some(message) = &state.failure {
            return Err(unavailable(message));
        }
        let key = (id.to_owned(), folder.to_owned());
        match operation {
            AutostartOperation::Enable => {
                state.enabled.insert(key, true);
                Ok(true)
            }
            AutostartOperation::Disable => {
                state.enabled.remove(&key);
                Ok(false)
            }
            AutostartOperation::IsEnabled => Ok(state.enabled.get(&key).copied().unwrap_or(false)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_calls_keep_only_the_last_256_without_losing_enabled_state() {
        use AutostartOperation::{Disable, Enable, IsEnabled};

        let backend = MemoryAutostart::default();
        let folder = std::env::current_dir().unwrap();
        let apply = |operation| backend.apply("org.example.bounded", &folder, operation);
        assert!(apply(Enable).unwrap());
        for _ in 0..300 {
            assert!(apply(IsEnabled).unwrap());
            assert!(backend.calls().len() <= 256);
        }
        assert_eq!(backend.calls(), vec![IsEnabled; 256]);
        assert!(!apply(Disable).unwrap());
        assert!(!apply(IsEnabled).unwrap());
        assert!(apply(Enable).unwrap());
        assert!(apply(IsEnabled).unwrap());
        let mut expected = vec![IsEnabled; 252];
        expected.extend([Disable, IsEnabled, Enable, IsEnabled]);
        assert_eq!(backend.calls(), expected);
    }
}

#[derive(Debug, Default)]
pub struct SystemAutostart {
    serial: Mutex<()>,
}
impl AutostartBackend for SystemAutostart {
    fn apply(
        &self,
        id: &str,
        folder: &Path,
        operation: AutostartOperation,
    ) -> Result<bool, AlefError> {
        let _guard = self
            .serial
            .lock()
            .map_err(|_| unavailable("autostart lock poisoned"))?;
        let launch = Launch::resolve(id, folder)?;
        let enable = match operation {
            AutostartOperation::Enable => Some(true),
            AutostartOperation::Disable => Some(false),
            AutostartOperation::IsEnabled => None,
        };
        #[cfg(windows)]
        {
            let command = launch.windows_command()?;
            integration::registry::apply(&launch.name, &command, enable)
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let (path, expected) = entry(&launch)?;
            match enable {
                Some(enable) => {
                    integration::files::change(&path, &expected, enable)?;
                    Ok(enable)
                }
                None => integration::files::enabled(&path, &expected),
            }
        }
        #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
        {
            let _ = (launch, enable);
            Err(unavailable("autostart is unsupported on this platform"))
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn entry(launch: &Launch) -> Result<(PathBuf, String), AlefError> {
    let home = || -> Result<PathBuf, AlefError> {
        let path =
            PathBuf::from(std::env::var_os("HOME").ok_or_else(|| unavailable("HOME is not set"))?);
        integration::path_text(&path)?;
        if !path.is_absolute() {
            return Err(unavailable("HOME must be absolute"));
        }
        Ok(path)
    };
    #[cfg(target_os = "macos")]
    {
        Ok((
            home()?
                .join("Library/LaunchAgents")
                .join(format!("{}.plist", launch.name)),
            integration::files::plist(launch)?,
        ))
    }
    #[cfg(target_os = "linux")]
    {
        let root = match std::env::var_os("XDG_CONFIG_HOME") {
            Some(value) if !value.is_empty() => {
                let path = PathBuf::from(value);
                integration::path_text(&path)?;
                if !path.is_absolute() {
                    return Err(unavailable("XDG_CONFIG_HOME must be absolute"));
                }
                path
            }
            _ => home()?.join(".config"),
        };
        Ok((
            root.join("autostart")
                .join(format!("{}.desktop", launch.name)),
            integration::files::desktop(launch)?,
        ))
    }
}

pub(super) fn register(registry: &mut Registry, context: &ModuleContext) -> Result<(), AlefError> {
    // One isolated stand-in per registry/application, never shares the real backend's state.
    let substitute: Arc<dyn AutostartBackend> = Arc::new(MemoryAutostart::default());
    for (command, operation) in [
        ("app.autostart.enable", AutostartOperation::Enable),
        ("app.autostart.disable", AutostartOperation::Disable),
        ("app.autostart.isEnabled", AutostartOperation::IsEnabled),
    ] {
        let real = context.backends.autostart.clone();
        let substitute = substitute.clone();
        let id = context.app.id.clone();
        let folder = context.paths.app.clone();
        registry
            .command::<Value>(command)?
            .permission(Permission::AppAutostart, |_| None)
            .substitutes()
            .handler(move |ctx, args| {
                let backend = if ctx.decision() == Decision::Substitute {
                    substitute.clone()
                } else {
                    real.clone()
                };
                let id = id.clone();
                let folder = folder.clone();
                async move {
                    if !args.is_null()
                        && !(args.is_object() && args.as_object().is_some_and(|map| map.is_empty()))
                    {
                        return Err(AlefError::new(
                            ErrorCode::InvalidArgument,
                            "autostart accepts no arguments",
                        ));
                    }
                    let enabled =
                        tokio::task::spawn_blocking(move || backend.apply(&id, &folder, operation))
                            .await
                            .map_err(|e| {
                                AlefError::new(ErrorCode::Internal, format!("autostart task: {e}"))
                            })??;
                    if operation == AutostartOperation::IsEnabled {
                        json(&enabled)
                    } else {
                        json(&())
                    }
                }
            })?;
    }
    Ok(())
}
