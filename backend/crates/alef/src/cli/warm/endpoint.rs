// SPDX-License-Identifier: MIT OR Apache-2.0
//! The broker entry point of this session (docs/stages/m11-warm-start.md,
//! «Точка входа брокера и путь к ней»): a short per-user base directory, a
//! session identity from the build and the exact store-identity variables,
//! and the derived [`Endpoint`]. Pure functions; the permission checks are
//! the transport's (`ipc::Server::bind` / `ipc::Client`).

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

#[cfg(unix)]
use alef_broker::ipc::endpoint::is_safe_name;
use alef_broker::ipc::endpoint::Endpoint;
#[cfg(windows)]
use alef_broker::ipc::endpoint::WINDOWS_PIPE_PREFIX;

/// Practical Unix socket path limit (`sun_path`, with a safety margin).
pub(crate) const MAX_SOCKET_PATH: usize = 100;

/// The endpoint of this session, or `None` when warm start is unavailable
/// (the socket path does not fit the limit — the client goes cold, the
/// broker refuses to start; never a panic).
pub(crate) fn endpoint() -> Option<Endpoint> {
    let ep = Endpoint::for_session(&session_base(), &session_identity());
    #[cfg(unix)]
    if ep.unix_socket.as_os_str().as_encoded_bytes().len() > MAX_SOCKET_PATH {
        return None;
    }
    Some(ep)
}

/// Whether something may be listening at the endpoint (read-only check, so a
/// cold fallback never creates the token file as a side effect).
pub(crate) fn endpoint_present(ep: &Endpoint) -> bool {
    #[cfg(unix)]
    {
        ep.unix_socket.exists()
    }
    #[cfg(windows)]
    {
        alef_broker::ipc::token::windows_token_path(ep)
            .map(|path| path.exists())
            .unwrap_or(false)
    }
}

/// The endpoint argument of `warm-spare` back into an [`Endpoint`]: the Unix
/// socket path or the Windows pipe name the spawner appended to the argv.
pub(crate) fn endpoint_from_arg(arg: &OsStr) -> Option<Endpoint> {
    #[cfg(windows)]
    {
        let text = arg.to_str()?;
        let name = text.strip_prefix(WINDOWS_PIPE_PREFIX)?;
        Some(Endpoint {
            name: name.to_owned(),
            unix_dir: PathBuf::new(),
            unix_socket: PathBuf::new(),
            windows_pipe: text.to_owned(),
        })
    }
    #[cfg(unix)]
    {
        let socket = PathBuf::from(arg);
        let dir = socket.parent()?.to_path_buf();
        let name = dir.file_name()?.to_string_lossy().into_owned();
        if !is_safe_name(&name) {
            return None;
        }
        Some(Endpoint {
            name,
            unix_dir: dir,
            unix_socket: socket,
            windows_pipe: String::new(),
        })
    }
}

/// The short per-user base directory: Unix `$XDG_RUNTIME_DIR/alef` when it is
/// set and absolute, otherwise `/tmp/alef-<uid>`; the ownership and 0700 mode
/// are enforced by `ipc`. Windows only uses it to derive the token path.
pub(crate) fn session_base() -> PathBuf {
    #[cfg(unix)]
    {
        if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") {
            let path = PathBuf::from(dir);
            if path.is_absolute() {
                return path.join("alef");
            }
        }
        // Invariant: getuid has no preconditions and cannot fail.
        PathBuf::from(format!("/tmp/alef-{}", unsafe { libc::getuid() }))
    }
    #[cfg(windows)]
    {
        dirs::cache_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("alef")
    }
}

/// The session identity of this process: the exact values (or absence) of
/// `ALEF_HOME`/`ALEF_APP_HOME` plus the build version — what the `Hello` /
/// `ExpectedIdentity` handshake compares, hashed into the endpoint name.
pub(crate) fn session_identity() -> String {
    identity_for(
        env!("CARGO_PKG_VERSION"),
        std::env::var_os("ALEF_HOME").as_deref(),
        std::env::var_os("ALEF_APP_HOME").as_deref(),
    )
}

fn identity_for(build: &str, home: Option<&OsStr>, app_home: Option<&OsStr>) -> String {
    format!(
        "alef/1 build={build} ALEF_HOME={} ALEF_APP_HOME={}",
        identity_part(home),
        identity_part(app_home)
    )
}

/// Absence is a value; present values are hex-encoded platform bytes, so the
/// identity is exact on every OS and never contains the raw value.
fn identity_part(value: Option<&OsStr>) -> String {
    match value {
        None => "absent".to_owned(),
        Some(value) => hex(&os_bytes(value)),
    }
}

#[cfg(unix)]
fn os_bytes(value: &OsStr) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    value.as_bytes().to_vec()
}

#[cfg(windows)]
fn os_bytes(value: &OsStr) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;
    value
        .encode_wide()
        .flat_map(|unit| unit.to_le_bytes())
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_separates_absence_from_values_and_hides_them() {
        let absent = identity_for("1.2.3", None, None);
        let set = identity_for("1.2.3", Some(OsStr::new("/home/u/.alef")), None);
        assert_ne!(absent, set);
        assert!(absent.contains("ALEF_HOME=absent"));
        assert!(!set.contains("/home/u/.alef"));
        assert!(set.contains("ALEF_HOME=2f686f6d652f752f2e616c6566"));
        assert_ne!(
            identity_for("1.2.3", None, None),
            identity_for("1.2.4", None, None)
        );
    }

    #[cfg(unix)]
    #[test]
    fn endpoint_fits_or_refuses_and_round_trips_from_the_argv() {
        let ep = Endpoint::for_session(std::path::Path::new("/tmp/base"), "identity");
        assert!(ep.unix_socket.as_os_str().as_encoded_bytes().len() <= MAX_SOCKET_PATH);
        let from_arg = endpoint_from_arg(ep.unix_socket.as_os_str()).unwrap();
        assert_eq!(from_arg.unix_socket, ep.unix_socket);
        assert_eq!(from_arg.name, ep.name);
        assert!(endpoint_from_arg(OsStr::new("not-an-endpoint")).is_none());
    }

    #[cfg(windows)]
    #[test]
    fn endpoint_round_trips_from_the_pipe_name() {
        let ep = Endpoint::for_session(std::path::Path::new("base"), "identity");
        let from_arg = endpoint_from_arg(OsStr::new(&ep.windows_pipe)).unwrap();
        assert_eq!(from_arg.windows_pipe, ep.windows_pipe);
        assert_eq!(from_arg.name, ep.name);
        assert!(endpoint_from_arg(OsStr::new(r"\\.\pipe\other-broker")).is_none());
    }

    #[test]
    fn the_base_directory_is_short_and_per_user() {
        #[cfg(unix)]
        assert!(session_base().as_os_str().as_encoded_bytes().len() < 64);
        let _ = session_base();
    }
}
