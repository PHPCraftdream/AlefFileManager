// SPDX-License-Identifier: MIT OR Apache-2.0
//! Pure endpoint naming and (Unix) permission checks.
//!
//! The entry-point name derives from the session/store identity via SHA-256
//! and NEVER contains `ALEF_HOME`/`ALEF_APP_HOME` values — only their identity
//! is hashed, by the caller, into the `session_identity` string. All functions
//! here are pure and testable on every OS; the permission helpers are Unix-only.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// Hex digits of the SHA-256 used in endpoint names (64 bits).
pub const HASH_HEX_LEN: usize = 16;
/// Socket file name inside the private Unix directory.
pub const UNIX_SOCKET_FILE: &str = "broker.sock";
/// Windows pipe name prefix (after `\\.\pipe\`).
pub const WINDOWS_PIPE_PREFIX: &str = r"\\.\pipe\alef-broker-";

/// A fully named broker entry point for one session/store identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    /// `broker-<hash16hex>`.
    pub name: String,
    /// Unix: `<base>/<name>/` (mode 0700, owner = current uid).
    pub unix_dir: PathBuf,
    /// Unix: `<unix_dir>/broker.sock` (mode 0600).
    pub unix_socket: PathBuf,
    /// Windows: `\\.\pipe\alef-broker-<name>`.
    pub windows_pipe: String,
}

impl Endpoint {
    /// Derive the endpoint from a base directory and a session/store identity
    /// string. The identity MUST already exclude raw `ALEF_HOME` values (the
    /// caller hashes or omits them); nothing here reads the environment.
    pub fn for_session(base: &Path, session_identity: &str) -> Endpoint {
        let name = endpoint_name(session_identity);
        let dir = base.join(&name);
        Endpoint {
            unix_socket: dir.join(UNIX_SOCKET_FILE),
            windows_pipe: format!("{WINDOWS_PIPE_PREFIX}{name}"),
            unix_dir: dir,
            name,
        }
    }

    /// NUL-terminated wide pipe name for `CreateNamedPipeW`/`CreateFileW`.
    #[cfg(windows)]
    pub fn windows_pipe_wide(&self) -> Vec<u16> {
        self.windows_pipe.encode_utf16().chain([0]).collect()
    }
}

/// First 8 bytes of `SHA-256(session_identity)` as 16 lowercase hex chars.
pub fn endpoint_hash(session_identity: &str) -> String {
    let digest = Sha256::digest(session_identity.as_bytes());
    let mut out = String::with_capacity(HASH_HEX_LEN);
    for byte in &digest[..HASH_HEX_LEN / 2] {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// `broker-<hash16hex>`: filesystem- and pipe-safe, charset-checked.
pub fn endpoint_name(session_identity: &str) -> String {
    let name = format!("broker-{}", endpoint_hash(session_identity));
    debug_assert!(is_safe_name(&name));
    name
}

/// Defensive charset check: `broker-` + lowercase hex only.
pub fn is_safe_name(name: &str) -> bool {
    name.strip_prefix("broker-").is_some_and(|rest| {
        !rest.is_empty()
            && rest
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    })
}

/// Windows DACL string: protected descriptor, generic-all to the user only.
/// `user_sid` must look like `S-1-...`; the SDDL form is
/// `D:P(A;;GA;;;<user sid>)` — no inheritance, no other trustees.
pub fn sddl_dacl(user_sid: &str) -> String {
    format!("D:P(A;;GA;;;{user_sid})")
}

/// Sanity check for a string-form SID (`S-1-<digits>...`) before embedding it
/// into SDDL, so a hostile value cannot break out of the ACE field.
pub fn is_valid_sid_string(sid: &str) -> bool {
    let Some(rest) = sid.strip_prefix("S-1-") else {
        return false;
    };
    !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit() || b == b'-')
}

/// Unix permission problems, kept typed for tests.
#[derive(Debug)]
#[cfg(unix)]
pub enum EndpointError {
    /// Path does not exist.
    Missing,
    /// Path is a symlink (always rejected, fail-closed).
    Symlink,
    /// Not a directory (socket helpers: not the expected socket file).
    WrongType,
    /// Owned by another uid.
    WrongOwner {
        expected: u32,
        actual: u32,
    },
    /// Mode is wider than required (e.g. 0755 instead of 0700).
    TooPermissive {
        actual: u32,
    },
    Io(io::Error),
}

#[cfg(unix)]
impl fmt::Display for EndpointError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EndpointError::Missing => write!(f, "endpoint path is missing"),
            EndpointError::Symlink => write!(f, "endpoint path is a symlink"),
            EndpointError::WrongType => write!(f, "endpoint path has the wrong file type"),
            EndpointError::WrongOwner { expected, actual } => {
                write!(f, "endpoint owned by uid {actual}, expected {expected}")
            }
            EndpointError::TooPermissive { actual } => {
                write!(f, "endpoint permissions {actual:o} are too wide")
            }
            EndpointError::Io(e) => write!(f, "endpoint I/O error: {e}"),
        }
    }
}

#[cfg(unix)]
impl std::error::Error for EndpointError {}

#[cfg(unix)]
impl From<io::Error> for EndpointError {
    fn from(e: io::Error) -> Self {
        if e.kind() == io::ErrorKind::NotFound {
            EndpointError::Missing
        } else {
            EndpointError::Io(e)
        }
    }
}

/// Check a broker directory: no symlink, owned by `expected_uid`, exactly
/// 0700 (0755 or wider is rejected). Uses `lstat` semantics throughout.
#[cfg(unix)]
pub fn check_private_dir(path: &Path, expected_uid: u32) -> Result<(), EndpointError> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let md = std::fs::symlink_metadata(path)?;
    if md.file_type().is_symlink() {
        return Err(EndpointError::Symlink);
    }
    if !md.is_dir() {
        return Err(EndpointError::WrongType);
    }
    let uid = md.uid();
    if uid != expected_uid {
        return Err(EndpointError::WrongOwner {
            expected: expected_uid,
            actual: uid,
        });
    }
    let mode = md.permissions().mode() & 0o777;
    if mode != 0o700 {
        return Err(EndpointError::TooPermissive { actual: mode });
    }
    Ok(())
}

/// Check the socket file: no symlink, owned by `expected_uid`, exactly 0600.
#[cfg(unix)]
pub fn check_socket_file(path: &Path, expected_uid: u32) -> Result<(), EndpointError> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
    let md = std::fs::symlink_metadata(path)?;
    if md.file_type().is_symlink() {
        return Err(EndpointError::Symlink);
    }
    if !md.file_type().is_socket() {
        return Err(EndpointError::WrongType);
    }
    let uid = md.uid();
    if uid != expected_uid {
        return Err(EndpointError::WrongOwner {
            expected: expected_uid,
            actual: uid,
        });
    }
    let mode = md.permissions().mode() & 0o777;
    if mode != 0o600 {
        return Err(EndpointError::TooPermissive { actual: mode });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_is_deterministic_and_separating() {
        let a = endpoint_name("session-1");
        let b = endpoint_name("session-1");
        let c = endpoint_name("session-2");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert!(is_safe_name(&a));
        assert_eq!(a.len(), "broker-".len() + HASH_HEX_LEN);
        // 16 hex chars = first 8 digest bytes.
        assert_eq!(endpoint_hash("session-1").len(), HASH_HEX_LEN);
    }

    #[test]
    fn name_never_contains_identity_or_env() {
        let tricky = "ALEF_HOME=/secret S-1-5-21-.. password";
        let name = endpoint_name(tricky);
        assert!(!name.contains("ALEF"));
        assert!(!name.contains("secret"));
        assert!(!name.contains('/'));
        assert!(!name.contains('\\'));
    }

    #[test]
    fn endpoint_paths_match_platform_conventions() {
        let ep = Endpoint::for_session(Path::new("/base"), "sess");
        assert_eq!(ep.unix_dir, PathBuf::from("/base").join(&ep.name));
        assert_eq!(
            ep.unix_socket,
            PathBuf::from("/base").join(&ep.name).join(UNIX_SOCKET_FILE)
        );
        assert!(ep.windows_pipe.starts_with(WINDOWS_PIPE_PREFIX));
        assert!(ep.windows_pipe.ends_with(&ep.name));
    }

    #[test]
    fn sddl_shape_and_sid_validation() {
        let sid = "S-1-5-21-1004336348-1178098362-1801674531-1001";
        assert_eq!(sddl_dacl(sid), format!("D:P(A;;GA;;;{sid})"));
        assert!(is_valid_sid_string(sid));
        assert!(!is_valid_sid_string("S-1-x;GA;;;BA)")); // no injection
        assert!(!is_valid_sid_string("D:P(A;;FA;;;BA)"));
        assert!(!is_valid_sid_string(""));
    }

    #[test]
    fn unsafe_names_rejected() {
        assert!(!is_safe_name(""));
        assert!(!is_safe_name("broker-"));
        assert!(!is_safe_name("broker-GHIJ"));
        assert!(!is_safe_name("broker-abc/../etc"));
        assert!(is_safe_name("broker-0123456789abcdef"));
    }
}
