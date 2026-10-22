// SPDX-License-Identifier: MIT OR Apache-2.0
//! Challenge-token storage (docs/stages/m11-warm-start.md, «IPC, полномочия и
//! передача управления»): a 32-byte random token in a protected file, never
//! passed via CLI, environment, or logs (and [`Token`] has no `Debug`/`Display`).
//!
//! Creation is atomic: exclusive `create_new` first; if the file already
//! exists it is read back and its permissions are re-verified. Unix stores the
//! token in the broker's private 0700 directory as a 0600 file; Windows uses
//! the user's data directory with the user-profile's default ACLs.

use std::fs;
use std::io::{self, ErrorKind};
use std::path::PathBuf;

use crate::ipc::endpoint::Endpoint;
use crate::protocol::auth::Token;

/// Token length in bytes.
pub const TOKEN_LEN: usize = 32;

/// Unix: `<broker private dir>/broker.token`.
#[cfg(unix)]
pub fn unix_token_path(ep: &Endpoint) -> PathBuf {
    ep.unix_dir.join("broker.token")
}

/// Windows: `%LOCALAPPDATA%\alef\broker\<endpoint name>\broker.token`,
/// created with the user profile's default ACLs.
#[cfg(windows)]
pub fn windows_token_path(ep: &Endpoint) -> io::Result<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("APPDATA"))
        .ok_or_else(|| io::Error::other("no user data directory (LOCALAPPDATA/APPDATA)"))?;
    let base_path = PathBuf::from(base);
    if !base_path.is_absolute() {
        return Err(io::Error::other("user data directory must be absolute"));
    }
    let dir = base_path.join("alef").join("broker").join(&ep.name);
    Ok(dir.join("broker.token"))
}

/// Load the token for `ep`, creating it (32 fresh random bytes) if absent.
/// `Ok((token, created))`; re-verification of permissions happens on read.
pub fn load_or_create_token(ep: &Endpoint) -> io::Result<(Token, bool)> {
    #[cfg(unix)]
    {
        let path = unix_token_path(ep);
        unix_prepare_dir(&ep.unix_dir)?;
        match atomic_create(&path, TOKEN_LEN)? {
            Some(bytes) => Ok((token_from_bytes(bytes)?, true)),
            None => Ok((read_existing(&path)?, false)),
        }
    }
    #[cfg(windows)]
    {
        let path = windows_token_path(ep)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        match atomic_create(&path, TOKEN_LEN)? {
            Some(bytes) => Ok((token_from_bytes(bytes)?, true)),
            None => Ok((read_existing(&path)?, false)),
        }
    }
}

/// Wrap raw bytes, mapping the token error to an I/O error.
fn token_from_bytes(bytes: Vec<u8>) -> io::Result<Token> {
    Token::new(bytes).map_err(|_| io::Error::new(ErrorKind::InvalidData, "empty token"))
}

/// Create the file exclusively with `len` random bytes; `None` if it existed.
fn atomic_create(path: &std::path::Path, len: usize) -> io::Result<Option<Vec<u8>>> {
    // Fresh entropy; ownership of the bytes passes to `Token` (zeroized there).
    let mut bytes = vec![0u8; len];
    getrandom::fill(&mut bytes).map_err(|e| io::Error::other(format!("entropy failure: {e}")))?;
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(mut f) => {
            use std::io::Write;
            f.write_all(&bytes)?;
            #[cfg(unix)]
            {
                // File is 0600 only if the umask is tight; force it.
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
            }
            Ok(Some(bytes))
        }
        Err(e) if e.kind() == ErrorKind::AlreadyExists => {
            bytes.iter_mut().for_each(|b| *b = 0);
            Ok(None)
        }
        Err(e) => {
            bytes.iter_mut().for_each(|b| *b = 0);
            Err(e)
        }
    }
}

/// Read and re-verify an existing token file; the bytes are never logged.
fn read_existing(path: &std::path::Path) -> io::Result<Token> {
    #[cfg(unix)]
    verify_unix_token_file(path)?;
    let mut bytes = fs::read(path)?;
    if bytes.len() != TOKEN_LEN {
        bytes.zeroize();
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            "token file has the wrong length",
        ));
    }
    Token::new(bytes).map_err(|_| io::Error::new(ErrorKind::InvalidData, "empty token"))
}

/// Unix: ensure the broker directory exists and passes the 0700/uid checks.
#[cfg(unix)]
fn unix_prepare_dir(dir: &std::path::Path) -> io::Result<()> {
    // Created 0700 from the start (a plain create_dir would follow the umask).
    match std::os::unix::fs::DirBuilderExt::mode(&mut fs::DirBuilder::new(), 0o700).create(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    crate::ipc::endpoint::check_private_dir(dir, current_uid()).map_err(|e| match e {
        crate::ipc::endpoint::EndpointError::Io(io) => io,
        other => io::Error::new(ErrorKind::PermissionDenied, other.to_string()),
    })
}

/// Unix: token file must not be a symlink and must be 0600 owned by us.
#[cfg(unix)]
fn verify_unix_token_file(path: &std::path::Path) -> io::Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let md = fs::symlink_metadata(path)?;
    if md.file_type().is_symlink() {
        return Err(io::Error::new(
            ErrorKind::PermissionDenied,
            "token file is a symlink",
        ));
    }
    if md.permissions().mode() & 0o777 != 0o600 || md.uid() != current_uid() {
        return Err(io::Error::new(
            ErrorKind::PermissionDenied,
            "token file permissions are too wide or foreign-owned",
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn current_uid() -> u32 {
    // Invariant: geteuid has no failure mode.
    unsafe { libc::geteuid() }
}

// A tiny zeroizer for the scratch buffer (Token itself zeroizes on drop).
trait Zeroize {
    fn zeroize(&mut self);
}
impl Zeroize for [u8] {
    fn zeroize(&mut self) {
        for b in self.iter_mut() {
            *b = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_endpoint(dir: &std::path::Path) -> Endpoint {
        Endpoint::for_session(dir, "token-tests")
    }

    #[test]
    fn token_round_trip_atomic() {
        let tmp = tempfile::tempdir().unwrap();
        let ep = temp_endpoint(tmp.path());
        let (t1, created1) = load_or_create_token(&ep).unwrap();
        assert!(created1);
        assert_eq!(t1.expose().len(), TOKEN_LEN);
        let (t2, created2) = load_or_create_token(&ep).unwrap();
        assert!(!created2);
        assert_eq!(t1.expose(), t2.expose());
    }

    #[test]
    fn token_file_permitted_tightly_unix() {
        let tmp = tempfile::tempdir().unwrap();
        let ep = temp_endpoint(tmp.path());
        load_or_create_token(&ep).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let md = fs::symlink_metadata(unix_token_path(&ep)).unwrap();
            assert_eq!(md.permissions().mode() & 0o777, 0o600);
            // Directory itself must be 0700.
            let dmd = fs::symlink_metadata(&ep.unix_dir).unwrap();
            assert_eq!(dmd.permissions().mode() & 0o777, 0o700);
        }
    }

    #[cfg(unix)]
    #[test]
    fn token_rejected_on_symlinked_dir() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let real = tempfile::tempdir_in(tmp.path()).unwrap();
        let ep = Endpoint::for_session(tmp.path(), "symlink");
        // The directory of the endpoint itself is a symlink.
        symlink(real.path(), &ep.unix_dir).unwrap();
        assert!(load_or_create_token(&ep).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn token_rejected_in_shared_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let ep = Endpoint::for_session(tmp.path(), "shared");
        // The directory of the endpoint itself is wider than 0700.
        fs::create_dir(&ep.unix_dir).unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&ep.unix_dir, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(load_or_create_token(&ep).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn token_rejected_if_symlinked_file() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let ep = temp_endpoint(tmp.path());
        load_or_create_token(&ep).unwrap();
        let path = unix_token_path(&ep);
        fs::remove_file(&path).unwrap();
        symlink(tmp.path().join("elsewhere"), &path).unwrap();
        assert!(load_or_create_token(&ep).is_err());
    }
}
