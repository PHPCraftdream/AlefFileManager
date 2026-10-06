// SPDX-License-Identifier: MIT OR Apache-2.0
//! `os`: platform facts and the desktop theme.
use std::{process::Command, sync::Arc, sync::OnceLock};

use alef_core::{
    registry::{dispatch::Registry, host::Host},
    AlefError, ErrorCode,
};
use serde::{Deserialize, Serialize};

use crate::json;

/// Facts about the machine (`os.info`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "modules.ts")]
pub struct OsInfo {
    /// `windows`, `macos`, `linux` or another Rust `target_os` name.
    pub platform: String,
    /// `x86_64`, `aarch64` or another Rust `target_arch` name.
    pub arch: String,
    /// OS release: `10.0.19045.6456`, `14.5`, `24.04`; `unknown` if the system does not tell.
    pub version: String,
    /// BCP 47 tag of the user's language, `und` if unknown.
    pub locale: String,
    pub hostname: String,
}

fn output_of(program: &str, arguments: &[&str]) -> Option<String> {
    let output = Command::new(program).args(arguments).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The first `N.N.N[.N]` token of `text` (`Microsoft Windows [Version 10.0.19045.6456]`).
fn first_version(text: &str) -> Option<String> {
    text.split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .find(|token| {
            token.matches('.').count() >= 2
                && token
                    .split('.')
                    .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
        })
        .map(str::to_owned)
}

/// `VERSION_ID` of an `os-release` file, without quotes.
fn os_release_version(text: &str) -> Option<String> {
    text.lines()
        .find_map(|line| line.strip_prefix("VERSION_ID="))
        .map(|value| {
            value
                .trim()
                .trim_matches(|c| c == '"' || c == '\'')
                .to_owned()
        })
        .filter(|value| !value.is_empty())
}

fn read_version() -> String {
    let found = if cfg!(windows) {
        output_of("cmd", &["/C", "ver"]).and_then(|text| first_version(&text))
    } else if cfg!(target_os = "macos") {
        output_of("sw_vers", &["-productVersion"]).map(|text| text.trim().to_owned())
    } else {
        std::fs::read_to_string("/etc/os-release")
            .ok()
            .and_then(|text| os_release_version(&text))
            .or_else(|| {
                std::fs::read_to_string("/proc/sys/kernel/osrelease")
                    .ok()
                    .map(|text| text.trim().to_owned())
            })
    };
    found
        .filter(|version| !version.is_empty())
        .unwrap_or_else(|| "unknown".to_owned())
}

/// Reading the version may start a process, so it is done once.
fn version() -> &'static str {
    static VERSION: OnceLock<String> = OnceLock::new();
    VERSION.get_or_init(read_version)
}

/// Gathers the facts (blocking: may start a process the first time).
pub fn info() -> OsInfo {
    OsInfo {
        platform: std::env::consts::OS.to_owned(),
        arch: std::env::consts::ARCH.to_owned(),
        version: version().to_owned(),
        locale: sys_locale::get_locale()
            .filter(|locale| !locale.is_empty())
            .unwrap_or_else(|| "und".to_owned()),
        hostname: gethostname::gethostname().to_string_lossy().into_owned(),
    }
}

pub(crate) fn register(registry: &mut Registry, host: Arc<dyn Host>) -> Result<(), AlefError> {
    registry
        .command::<()>("os.info")?
        .handler(|_ctx, ()| async move {
            let info = tokio::task::spawn_blocking(info)
                .await
                .map_err(|error| AlefError::new(ErrorCode::Internal, error.to_string()))?;
            json(&info)
        })?;
    registry
        .command::<()>("os.theme")?
        .handler(move |_ctx, ()| {
            let host = host.clone();
            async move { json(&host.theme()) }
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_dotted_version_is_picked_out_of_the_ver_output() {
        assert_eq!(
            first_version("\r\nMicrosoft Windows [Version 10.0.19045.6456]\r\n").as_deref(),
            Some("10.0.19045.6456")
        );
        assert_eq!(
            first_version("Microsoft Windows [Версия 10.0.22631.4037]").as_deref(),
            Some("10.0.22631.4037")
        );
        assert_eq!(
            first_version("14.5").as_deref(),
            None,
            "two parts are not enough"
        );
        assert_eq!(first_version("a..b 1..2").as_deref(), None);
        assert_eq!(first_version("").as_deref(), None);
    }

    #[test]
    fn os_release_version_id_is_read_with_or_without_quotes() {
        assert_eq!(
            os_release_version("NAME=Ubuntu\nVERSION_ID=\"24.04\"\n").as_deref(),
            Some("24.04")
        );
        assert_eq!(os_release_version("VERSION_ID=12\n").as_deref(), Some("12"));
        assert_eq!(os_release_version("VERSION_ID=''\n").as_deref(), None);
        assert_eq!(os_release_version("NAME=Arch\n").as_deref(), None);
    }

    #[test]
    fn this_machine_reports_complete_facts() {
        let info = info();
        assert_eq!(info.platform, std::env::consts::OS);
        assert_eq!(info.arch, std::env::consts::ARCH);
        for (name, text) in [
            ("version", &info.version),
            ("locale", &info.locale),
            ("hostname", &info.hostname),
        ] {
            assert!(!text.trim().is_empty(), "{name} is empty");
        }
    }
}
