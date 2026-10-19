// SPDX-License-Identifier: MIT OR Apache-2.0
//! Framework modules: desktop (`app`, `window`, `dialog`, `shell`), system (`path`, `os`, `screen`,
//! `clipboard`), data (`fs`); more arrive with M3 and later.
//!
//! A module registers its commands in the registry (`<module>.<command>`) with the permission and
//! scope each one needs; the registry checks them before the handler runs. Everything the modules
//! need from the process comes through [`ModuleContext`] and the [`Host`](alef_core::registry::host::Host).
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    sync::Arc,
};

use alef_core::{
    registry::{command::Reply, dispatch::Registry, host::Host},
    security::{authority::Authority, permissions::PathVars},
    AlefError, ErrorCode,
};
use serde::{Deserialize, Serialize};

pub mod ai;
pub mod data;
pub mod desktop;
pub mod media;
pub mod net;
pub mod system;

pub use ai::dto::{ChatMessage, ChatOptions, ChatPart, Role};
pub use ai::provider::{
    AiProvider, ChatControl, ChatEvent, ChatRequest, ChatUsage, FakeProvider, MissingProvider,
    ToolDef,
};
pub use data::secrets::{MemorySecrets, SecretsBackend, SystemSecrets};
pub use desktop::{
    app::{
        autostart::{AutostartBackend, AutostartOperation, MemoryAutostart, SystemAutostart},
        console::Console,
        deeplink::{DeepLinkBackend, MemoryDeepLinks, SystemDeepLinks},
        LaunchState, Termination, LAUNCH_LIMIT,
    },
    args::{ArgValue, ParsedArgs},
    shell::{PretendShell, ShellBackend, ShellRequest, SystemShell},
};
pub use media::{
    AudioBackend, AudioSink, CameraBackend, CameraInfo, MicrophoneBackend, MicrophoneInfo,
    MissingAudio, MissingCamera, MissingMicrophone, VirtualAudio, VirtualCamera, VirtualMicrophone,
};
pub use system::{
    clipboard::{ClipboardBackend, Image, MemoryClipboard, SystemClipboard},
    notification::{Notification, NotificationBackend, PretendNotifications, SystemNotifications},
};

/// What the clipboard and the desktop shell really do. The system ones act on the desktop of the
/// user; the pretending ones answer as if they did and leave it alone.
#[derive(Debug, Clone)]
pub struct Backends {
    pub deep_links: Arc<dyn DeepLinkBackend>,
    pub autostart: Arc<dyn AutostartBackend>,
    pub clipboard: Arc<dyn ClipboardBackend>,
    pub shell: Arc<dyn ShellBackend>,
    pub notification: Arc<dyn NotificationBackend>,
    pub secrets: Arc<dyn SecretsBackend>,
    /// Where the chats of the `ai` module go; none in a build without a bridge to Rush.
    pub ai: Arc<dyn AiProvider>,
    /// Where camera frames, microphone chunks and played audio come from and go; the system build
    /// has none of them.
    pub camera: Arc<dyn CameraBackend>,
    pub microphone: Arc<dyn MicrophoneBackend>,
    pub audio: Arc<dyn AudioBackend>,
}

impl Backends {
    /// The clipboard, the shell and the notifications of the desktop; the notifications are shown
    /// under the name `application`.
    pub fn system(application: &str) -> Self {
        Self {
            deep_links: Arc::new(SystemDeepLinks::default()),
            autostart: Arc::new(SystemAutostart::default()),
            clipboard: Arc::new(SystemClipboard::default()),
            shell: Arc::new(SystemShell),
            notification: Arc::new(SystemNotifications::new(application)),
            secrets: Arc::new(SystemSecrets::default()),
            ai: Arc::new(MissingProvider),
            #[cfg(feature = "media-system")]
            camera: Arc::new(media::system::SystemCamera),
            #[cfg(feature = "media-system")]
            microphone: Arc::new(media::system::SystemMicrophone),
            #[cfg(feature = "media-system")]
            audio: Arc::new(media::system::SystemAudio),
            #[cfg(not(feature = "media-system"))]
            camera: Arc::new(MissingCamera),
            #[cfg(not(feature = "media-system"))]
            microphone: Arc::new(MissingMicrophone),
            #[cfg(not(feature = "media-system"))]
            audio: Arc::new(MissingAudio),
        }
    }

    /// A clipboard in memory, a shell and notifications that do nothing and, given a file, log
    /// their requests there.
    pub fn pretending(shell_log: Option<PathBuf>, notification_log: Option<PathBuf>) -> Self {
        Self {
            deep_links: Arc::new(MemoryDeepLinks::default()),
            autostart: Arc::new(MemoryAutostart::default()),
            clipboard: Arc::new(MemoryClipboard::default()),
            shell: Arc::new(match shell_log {
                Some(path) => PretendShell::logging_to(path),
                None => PretendShell::default(),
            }),
            notification: Arc::new(match notification_log {
                Some(path) => PretendNotifications::logging_to(path),
                None => PretendNotifications::default(),
            }),
            secrets: Arc::new(MemorySecrets::default()),
            ai: Arc::new(MissingProvider),
            camera: Arc::new(MissingCamera),
            microphone: Arc::new(MissingMicrophone),
            audio: Arc::new(MissingAudio),
        }
    }

    /// An end-to-end run (`ALEF_E2E=1`) pretends, its requests of the shell and of the
    /// notifications go to the files `ALEF_E2E_SHELL_LOG` and `ALEF_E2E_NOTIFICATION_LOG` name; every
    /// other run uses the desktop. The run of the checks of autostart and deep links
    /// (`ALEF_E2E_APP_INTEGRATION=1`) takes the real ones of those two and nothing else.
    #[cfg(feature = "e2e")]
    pub fn from_environment(application: &str) -> Self {
        let flag = |name: &str| std::env::var(name).is_ok_and(|value| value == "1");
        Self::chosen(
            application,
            flag("ALEF_E2E"),
            flag("ALEF_E2E_APP_INTEGRATION"),
            std::env::var_os("ALEF_E2E_SHELL_LOG").map(PathBuf::from),
            std::env::var_os("ALEF_E2E_NOTIFICATION_LOG").map(PathBuf::from),
        )
    }

    /// Without the `e2e` feature, always uses the desktop and ignores E2E environment variables.
    #[cfg(not(feature = "e2e"))]
    pub fn from_environment(application: &str) -> Self {
        Self::system(application)
    }

    #[cfg(any(test, feature = "e2e"))]
    fn chosen(
        application: &str,
        e2e: bool,
        app_integration: bool,
        shell_log: Option<PathBuf>,
        notification_log: Option<PathBuf>,
    ) -> Self {
        if !e2e {
            return Self::system(application);
        }
        let mut backends = Self::pretending(shell_log, notification_log);
        if app_integration {
            backends.autostart = Arc::new(SystemAutostart::default());
            backends.deep_links = Arc::new(SystemDeepLinks::default());
        }
        // An end-to-end run chats with the deterministic fake; nothing reaches a real model.
        backends.ai = Arc::new(FakeProvider::default());
        // An end-to-end run uses the deterministic virtual devices; nothing touches real hardware.
        backends.camera = Arc::new(VirtualCamera::default());
        backends.microphone = Arc::new(VirtualMicrophone::default());
        backends.audio = Arc::new(VirtualAudio);
        backends
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `media-system` build has the real devices behind `Backends::system`; the others keep
    /// the Missing stand-ins.
    #[cfg(feature = "media-system")]
    const SYSTEM_MEDIA: [&str; 3] = ["SystemCamera", "SystemMicrophone", "SystemAudio"];
    #[cfg(not(feature = "media-system"))]
    const SYSTEM_MEDIA: [&str; 3] = ["MissingCamera", "MissingMicrophone", "MissingAudio"];

    fn kinds(backends: &Backends) -> [String; 9] {
        [
            format!("{:?}", backends.clipboard),
            format!("{:?}", backends.shell),
            format!("{:?}", backends.notification),
            format!("{:?}", backends.secrets),
            format!("{:?}", backends.autostart),
            format!("{:?}", backends.deep_links),
            format!("{:?}", backends.camera),
            format!("{:?}", backends.microphone),
            format!("{:?}", backends.audio),
        ]
    }

    #[cfg(not(feature = "e2e"))]
    #[test]
    fn from_environment_without_the_e2e_feature_uses_system_backends() {
        let kinds = kinds(&Backends::from_environment("app"));
        let mut expected = [
            "SystemClipboard",
            "SystemShell",
            "SystemNotifications",
            "SystemSecrets",
            "SystemAutostart",
            "SystemDeepLinks",
        ]
        .to_vec();
        expected.extend_from_slice(&SYSTEM_MEDIA);
        for (kind, expected) in kinds.iter().zip(expected) {
            assert!(kind.contains(expected), "{kinds:?}");
        }
    }

    #[test]
    fn a_run_that_is_not_an_e2e_one_uses_the_desktop_whatever_else_is_set() {
        for app_integration in [false, true] {
            let backends = Backends::chosen("app", false, app_integration, None, None);
            let kinds = kinds(&backends);
            assert!(kinds[0].contains("SystemClipboard"), "{kinds:?}");
            assert!(kinds[1].contains("SystemShell"), "{kinds:?}");
            assert!(kinds[2].contains("SystemNotifications"), "{kinds:?}");
            assert!(kinds[3].contains("SystemSecrets"), "{kinds:?}");
            assert!(kinds[4].contains("SystemAutostart"), "{kinds:?}");
            assert!(kinds[5].contains("SystemDeepLinks"), "{kinds:?}");
            for (kind, expected) in kinds[6..].iter().zip(SYSTEM_MEDIA) {
                assert!(kind.contains(expected), "{kinds:?}");
            }
        }
    }

    #[cfg(feature = "media-system")]
    #[test]
    fn the_system_backends_pick_the_system_media_devices() {
        let kinds = kinds(&Backends::system("app"));
        assert!(kinds[6].contains("SystemCamera"), "{kinds:?}");
        assert!(kinds[7].contains("SystemMicrophone"), "{kinds:?}");
        assert!(kinds[8].contains("SystemAudio"), "{kinds:?}");
    }

    #[test]
    fn an_e2e_run_pretends_and_only_the_app_integration_opt_in_is_real_for_two_backends() {
        let pretending = kinds(&Backends::chosen("app", true, false, None, None));
        assert!(pretending[0].contains("MemoryClipboard"), "{pretending:?}");
        assert!(pretending[1].contains("PretendShell"), "{pretending:?}");
        assert!(
            pretending[2].contains("PretendNotifications"),
            "{pretending:?}"
        );
        assert!(pretending[3].contains("MemorySecrets"), "{pretending:?}");
        assert!(pretending[4].contains("MemoryAutostart"), "{pretending:?}");
        assert!(pretending[5].contains("MemoryDeepLinks"), "{pretending:?}");
        assert!(pretending[6].contains("VirtualCamera"), "{pretending:?}");
        assert!(
            pretending[7].contains("VirtualMicrophone"),
            "{pretending:?}"
        );
        assert!(pretending[8].contains("VirtualAudio"), "{pretending:?}");
        let opted = kinds(&Backends::chosen("app", true, true, None, None));
        assert!(opted[4].contains("SystemAutostart"), "{opted:?}");
        assert!(opted[5].contains("SystemDeepLinks"), "{opted:?}");
        assert!(opted[6].contains("VirtualCamera"), "{opted:?}");
        assert!(opted[7].contains("VirtualMicrophone"), "{opted:?}");
        assert!(opted[8].contains("VirtualAudio"), "{opted:?}");
        assert_eq!(opted[..4], pretending[..4]);
    }
}

/// Identity of the running application (`app.info`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "modules.ts")]
pub struct AppInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    /// Version of the Alef runtime executing the application.
    pub runtime_version: String,
}

/// The facts about the process that the modules serve.
#[derive(Debug, Clone)]
pub struct ModuleContext {
    /// Host-selected authority, independent of the application path and manifest id.
    pub authority: Authority,
    pub app: AppInfo,
    pub paths: PathVars,
    /// The command line of the application, already parsed by its schema.
    pub args: ParsedArgs,
    /// Validated manifest declarations, separate from registration permissions.
    pub deep_link_schemes: Vec<String>,
    /// Validated launch URLs, removed before application argument parsing.
    pub startup_urls: Vec<String>,
    /// Arguments of this process after the program name; `app.relaunch` starts it again with them.
    pub process_args: Vec<OsString>,
    /// The clipboard and the shell the modules act through.
    pub backends: Backends,
    /// Where the stand-ins of this application keep their content: a folder of the runtime, outside
    /// every scope of every application.
    pub shadow: PathBuf,
    /// The folder of the runtime where the user keeps the AI usage of this application: limits,
    /// counters, journal.
    pub ai_home: PathBuf,
    /// The standard streams of a console utility (`console: true`); `None` for every other application.
    pub console: Option<Console>,
    /// Process-wide launch selection, shared by every document and registration.
    pub launch: LaunchState,
    /// How the process is asked to end from outside, by a signal.
    pub termination: Termination,
}

/// Applies host-selected authority to caller-built paths before constructing permissions.
/// Bases are the OS data/config/cache directories, not application-owned directories.
/// Directory authority leaves *every* field literally unchanged; packages use
/// `<base>/alef-packages/<full authority token>` and have no filesystem `$APP`.
/// Other fields are untouched. This performs no IO or authority verification.
pub fn namespace_paths(
    paths: &mut PathVars,
    data_base: &Path,
    config_base: &Path,
    cache_base: &Path,
    id: &str,
    authority: &Authority,
) {
    if let Some(token) = authority.package_token(id) {
        let owned = |base: &Path| base.join("alef-packages").join(&token);
        paths.app_data = owned(data_base);
        paths.app_config = owned(config_base);
        paths.app_cache = owned(cache_base);
        paths.app = None;
    }
}

impl ModuleContext {
    /// Validates already namespaced package paths before module registration.
    /// Each owned path must be absolute and exactly `<base>/alef-packages/<this token>`.
    /// Callers must use explicit bases with `namespace_paths` before building permissions;
    /// registration never infers bases or repairs legacy paths or permissions.
    pub fn effective_paths(&self) -> Result<PathVars, AlefError> {
        let mut paths = self.paths.clone();
        let Some(token) = self.authority.package_token(&self.app.id) else {
            return Ok(paths);
        };
        for owned in [&paths.app_data, &paths.app_config, &paths.app_cache] {
            if !owned.is_absolute()
                || owned.file_name().is_none_or(|name| name != token.as_str())
                || owned
                    .parent()
                    .and_then(Path::file_name)
                    .is_none_or(|name| name != "alef-packages")
                || owned.components().any(|part| {
                    matches!(
                        part,
                        std::path::Component::ParentDir | std::path::Component::CurDir
                    )
                })
            {
                return Err(AlefError::new(
                    ErrorCode::InvalidArgument,
                    "package paths require absolute base/alef-packages/authority-token",
                ));
            }
        }
        paths.app = None;
        Ok(paths)
    }
}

/// Registers every module of this crate with the same authority-scoped paths.
pub fn register_all(
    registry: &mut Registry,
    host: Arc<dyn Host>,
    context: &ModuleContext,
) -> Result<(), AlefError> {
    let mut effective = context.clone();
    effective.paths = context.effective_paths()?;
    let context = &effective;
    desktop::register(registry, host.clone(), context)?;
    media::register(registry, context)?;
    data::register(registry, context)?;
    net::register(registry, context)?;
    ai::register(registry, host.clone(), context)?;
    system::register(registry, host, context)
}

pub(crate) fn json<T: Serialize>(value: &T) -> Result<Reply, AlefError> {
    serde_json::to_value(value)
        .map(Reply::Json)
        .map_err(|error| AlefError::new(ErrorCode::Internal, error.to_string()))
}
