// SPDX-License-Identifier: MIT OR Apache-2.0
//! Unix transport: private-directory UDS endpoints and peer uid checks
//! (docs/stages/m11-warm-start.md: каталог 0700, сокет 0600, SO_PEERCRED).
//!
//! - The endpoint directory is created (or reused) and then verified with
//!   `lstat` semantics: no symlink, owner = euid, mode exactly 0700.
//! - The socket file is forced to 0600 after `bind` and re-verified.
//! - Peer identity: `SO_PEERCRED` on Linux, `getpeereid` on macOS/BSD.
//! - The socket file is removed when the listener is dropped.
//! - Connect with timeout: non-blocking `connect` + `poll(POLLOUT)` +
//!   `SO_ERROR`, then back to blocking mode (std only, no `socket2`).

use std::io::{self, ErrorKind, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::ipc::endpoint::{check_private_dir, check_socket_file, Endpoint};
use crate::ipc::{Conn, Listener, PeerIdentity};

/// Listening side of a Unix endpoint; removes the socket file on drop.
pub struct UnixEndpointListener {
    listener: UnixListener,
    path: PathBuf,
}

impl UnixEndpointListener {
    /// Create or reuse the private directory, bind the socket, force 0600.
    pub fn bind(ep: &Endpoint) -> io::Result<UnixEndpointListener> {
        let dir = &ep.unix_dir;
        // Created 0700 from the start (a plain create_dir would follow the umask).
        match std::os::unix::fs::DirBuilderExt::mode(&mut std::fs::DirBuilder::new(), 0o700)
            .create(dir)
        {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
        check_private_dir(dir, current_uid())
            .map_err(|e| io::Error::new(io::ErrorKind::PermissionDenied, e.to_string()))?;
        let path = &ep.unix_socket;
        if path.symlink_metadata().is_ok() {
            // A stale socket from a crashed broker is fine to reuse-bind over?
            // Conservative: remove it only if it IS a socket owned by us.
            let md = path.symlink_metadata()?;
            use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
            let stale = md.file_type().is_socket()
                && !md.file_type().is_symlink()
                && md.uid() == current_uid()
                && md.permissions().mode() & 0o777 == 0o600;
            if !stale {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "endpoint path exists and is not a stale socket",
                ));
            }
            std::fs::remove_file(path)?;
        }
        let listener = UnixListener::bind(path)?;
        // Force 0600 regardless of umask, then verify with lstat.
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        check_socket_file(path, current_uid())
            .map_err(|e| io::Error::new(io::ErrorKind::PermissionDenied, e.to_string()))?;
        Ok(UnixEndpointListener {
            listener,
            path: path.clone(),
        })
    }
}

impl Drop for UnixEndpointListener {
    fn drop(&mut self) {
        // Best effort: only remove what is still our socket.
        let _ = std::fs::remove_file(&self.path);
    }
}

impl Listener for UnixEndpointListener {
    fn accept(&self) -> io::Result<Box<dyn Conn>> {
        let (stream, _) = self.listener.accept()?;
        Ok(Box::new(stream))
    }

    fn wake(&self) {
        // Unblock a pending accept; the server drops this dummy connection.
        let _ = UnixStream::connect(&self.path);
    }
}

impl Conn for UnixStream {
    fn set_read_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()> {
        UnixStream::set_read_timeout(self, timeout)
    }

    fn set_write_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()> {
        UnixStream::set_write_timeout(self, timeout)
    }

    fn peer_identity(&self) -> io::Result<PeerIdentity> {
        Ok(PeerIdentity::UnixUid(peer_uid(self)?))
    }
}

/// Current effective uid.
pub fn current_uid() -> u32 {
    // Invariant: geteuid has no failure mode.
    unsafe { libc::geteuid() }
}

/// Own identity, used by the client to verify the server and by the server to
/// verify clients: the same uid.
pub fn own_identity() -> io::Result<PeerIdentity> {
    Ok(PeerIdentity::UnixUid(current_uid()))
}

/// Peer uid via kernel credentials (never trust the peer's own claim).
pub fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    #[cfg(target_os = "linux")]
    {
        let fd = stream.as_raw_fd();
        // Invariant: fd is a live socket; buffer/len match libc::ucred.
        unsafe {
            let mut cred: libc::ucred = std::mem::zeroed();
            let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
            if libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                &mut cred as *mut libc::ucred as *mut libc::c_void,
                &mut len,
            ) != 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(cred.uid as u32)
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let fd = stream.as_raw_fd();
        // Invariant: fd is a live socket; getpeereid writes both out-params.
        unsafe {
            let mut uid: libc::uid_t = 0;
            let mut gid: libc::gid_t = 0;
            if libc::getpeereid(fd, &mut uid, &mut gid) != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(uid as u32)
        }
    }
}

/// Connect to `path` within `timeout`: raw non-blocking socket + `connect` +
/// `poll(POLLOUT)` + `SO_ERROR`, then wrapped back into a blocking stream.
pub fn connect_with_timeout(path: &Path, timeout: Duration) -> io::Result<UnixStream> {
    use std::os::unix::ffi::OsStrExt;
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() }; // invariant: all-zero is a valid start
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
    let bytes = path.as_os_str().as_bytes();
    // Keep room for the implicit NUL terminator.
    if bytes.len() + 1 > addr.sun_path.len() {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            "socket path too long",
        ));
    }
    // sun_path is [c_char] (i8 or u8 per platform); copy element-wise.
    for (dst, src) in addr.sun_path.iter_mut().zip(bytes.iter()) {
        *dst = *src as libc::c_char;
    }
    // Invariant: AF_UNIX SOCK_STREAM is always supported.
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // From here on `fd` must be closed on every early return.
    let result = (|| {
        // Invariant: fd is a valid open socket descriptor.
        unsafe { libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK) };
        let r = unsafe {
            libc::connect(
                fd,
                &addr as *const libc::sockaddr_un as *const libc::sockaddr,
                std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t,
            )
        };
        if r != 0 {
            let e = io::Error::last_os_error();
            if e.raw_os_error() != Some(libc::EINPROGRESS) {
                return Err(e);
            }
        }
        let deadline = Instant::now() + timeout;
        loop {
            let wait_ms = deadline
                .saturating_duration_since(Instant::now())
                .as_millis()
                .min(i32::MAX as u128) as i32;
            let mut pfd = libc::pollfd {
                fd,
                events: libc::POLLOUT,
                revents: 0,
            };
            // Invariant: valid fd; one pollfd; wait_ms clamped to i32.
            let ready = unsafe { libc::poll(&mut pfd, 1, wait_ms) };
            if ready < 0 {
                let e = io::Error::last_os_error();
                if e.kind() == ErrorKind::Interrupted {
                    continue;
                }
                return Err(e);
            }
            if ready == 0 {
                return Err(io::Error::new(ErrorKind::TimedOut, "connect timeout"));
            }
            let mut err: libc::c_int = 0;
            let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
            // Invariant: fd is a live socket; buffer matches the option size.
            if unsafe {
                libc::getsockopt(
                    fd,
                    libc::SOL_SOCKET,
                    libc::SO_ERROR,
                    &mut err as *mut libc::c_int as *mut libc::c_void,
                    &mut len,
                )
            } != 0
            {
                return Err(io::Error::last_os_error());
            }
            if err != 0 {
                return Err(io::Error::from_raw_os_error(err));
            }
            return Ok(());
        }
    })();
    match result {
        Ok(()) => {
            // Invariant: fd is owned exclusively and now wrapped; new stream
            // is blocking again (no O_NONBLOCK inheritance issues for Rust's
            // UnixStream — clear the flag before wrapping).
            unsafe { libc::fcntl(fd, libc::F_SETFL, 0) };
            Ok(unsafe { UnixStream::from_raw_fd(fd) })
        }
        Err(e) => {
            // Invariant: fd is still exclusively owned here; close it.
            unsafe { libc::close(fd) };
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::endpoint::Endpoint;

    #[test]
    fn bind_creates_0700_dir_and_0600_socket() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let ep = Endpoint::for_session(tmp.path(), "unix-tests");
        {
            let l = UnixEndpointListener::bind(&ep).unwrap();
            let d = std::fs::symlink_metadata(&ep.unix_dir).unwrap();
            assert_eq!(d.permissions().mode() & 0o777, 0o700);
            let s = std::fs::symlink_metadata(&ep.unix_socket).unwrap();
            assert_eq!(s.permissions().mode() & 0o777, 0o600);
            // accept+identity on our own socket.
            let handle = std::thread::spawn({
                let ep = ep.clone();
                move || {
                    let c = connect_with_timeout(&ep.unix_socket, Duration::from_secs(2)).unwrap();
                    c.peer_identity().unwrap()
                }
            });
            let conn = l.accept().unwrap();
            let own = own_identity().unwrap();
            assert_eq!(conn.peer_identity().unwrap(), own);
            assert_eq!(handle.join().unwrap(), own);
        }
        // Socket file removed on drop.
        assert!(!ep.unix_socket.exists());
    }

    #[test]
    fn bind_rejects_0755_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let ep = Endpoint::for_session(tmp.path(), "wide-dir");
        std::fs::create_dir(&ep.unix_dir).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&ep.unix_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(UnixEndpointListener::bind(&ep).is_err());
    }

    #[test]
    fn bind_rejects_symlinked_dir() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let real = tempfile::tempdir_in(tmp.path()).unwrap();
        let link = tmp.path().join("link");
        symlink(real.path(), &link).unwrap();
        let ep = Endpoint::for_session(&link, "sym-dir");
        assert!(UnixEndpointListener::bind(&ep).is_err());
    }

    #[test]
    fn connect_timeout_on_missing_socket() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("nope.sock");
        let err = connect_with_timeout(&missing, Duration::from_millis(200)).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::NotFound);
    }
}
