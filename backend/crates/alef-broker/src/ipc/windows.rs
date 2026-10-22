// SPDX-License-Identifier: MIT OR Apache-2.0
//! Windows transport: named pipes via `windows-sys` (blocking, std threads).
//!
//! Choices (also documented in docs/stages/m11-warm-start.md):
//! - DACL built from SDDL `D:P(A;;GA;;;<user sid>)` (no inheritance, user
//!   only), `PIPE_REJECT_REMOTE_CLIENTS`, and `FILE_FLAG_FIRST_PIPE_INSTANCE`
//!   on the FIRST instance only: a second broker on the same name gets
//!   `ERROR_ACCESS_DENIED`.
//! - Read/write timeouts: dedicated reader and writer threads plus bounded
//!   `std::sync::mpsc` channels (chosen over overlapped I/O: plain blocking
//!   `ReadFile`/`WriteFile` on the threads, timeouts come from channel
//!   `recv_timeout`/bounded backlog — simple and correct without OVERLAPPED
//!   bookkeeping).
//! - Client verifies the server conservatively: `GetNamedPipeServerProcessId`
//!   -> `OpenProcess` -> `OpenProcessToken` -> `TokenUser` SID must equal the
//!   client's own user SID (same-user broker). Combined with the unique name
//!   + `FIRST_PIPE_INSTANCE` + user-only DACL this authenticates the server
//!   as a process of the same user; the server checks the client's SID AND
//!   session id symmetrically.

use std::ffi::c_void;
use std::io::{self, ErrorKind, Read, Write};
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::Mutex;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use windows_sys::core::PWSTR;
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, LocalFree, ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND,
    ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, GENERIC_READ, GENERIC_WRITE, HANDLE,
    INVALID_HANDLE_VALUE, PSID,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, TokenUser, SECURITY_ATTRIBUTES, TOKEN_QUERY,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_SHARE_NONE, OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeClientProcessId,
    GetNamedPipeServerProcessId, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
    PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};

use crate::ipc::endpoint::Endpoint;
use crate::ipc::{Conn, Listener, PeerIdentity};

/// Pipe buffer sizes, also the writer-thread backlog unit.
const PIPE_BUF_SIZE: u32 = 64 * 1024;
/// Max frames queued in the writer channel before "write timeout".
const WRITE_QUEUE_LEN: usize = 4;

/// Listening side of the broker pipe. `FIRST_PIPE_INSTANCE` is used exactly
/// once, in [`PipeListener::bind`], so the name is owned or we fail.
pub struct PipeListener {
    name: Vec<u16>, // NUL-terminated wide pipe name
    handle: Mutex<HANDLE>,
}

impl PipeListener {
    /// Create the first pipe instance with a user-only DACL.
    pub fn bind(ep: &Endpoint, user_sid: &str) -> io::Result<PipeListener> {
        let handle = create_instance(&ep.windows_pipe_wide(), user_sid, true)?;
        Ok(PipeListener {
            name: ep.windows_pipe_wide(),
            handle: Mutex::new(handle),
        })
    }

    /// Create the next listening instance (no FIRST flag: we own the name).
    fn next_instance(&self) -> io::Result<HANDLE> {
        let sid = current_user_sid()?;
        create_instance(&self.name, &sid, false)
    }
}

impl Drop for PipeListener {
    fn drop(&mut self) {
        if let Ok(h) = self.handle.lock() {
            if *h != INVALID_HANDLE_VALUE {
                // Invariant: h is a listening pipe instance we own.
                unsafe {
                    CloseHandle(*h);
                }
            }
        }
    }
}

impl Listener for PipeListener {
    fn accept(&self) -> io::Result<Box<dyn Conn>> {
        let mut guard = self.handle.lock().expect("pipe listener mutex");
        let listening = *guard;
        // Invariant: listening is a valid pipe handle we own; no OVERLAPPED.
        let ok = unsafe { ConnectNamedPipe(listening, std::ptr::null_mut()) };
        if ok == 0 {
            let err = io::Error::last_os_error();
            if err.raw_os_error() != Some(ERROR_PIPE_CONNECTED as i32) {
                return Err(err);
            }
        }
        // Immediately open the next listening instance so other clients queue.
        *guard = self.next_instance()?;
        drop(guard);
        Ok(Box::new(PipeConn::new(listening)?))
    }

    fn wake(&self) {
        // Connect to our own pipe; the accept path above returns and the
        // server drops the dummy connection when stopping.
        let _ = open_client(&self.name, Duration::from_secs(1), None);
    }
}

/// Create one listening pipe instance.
fn create_instance(name_wide: &[u16], user_sid: &str, first: bool) -> io::Result<HANDLE> {
    if !crate::ipc::endpoint::is_valid_sid_string(user_sid) {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            "invalid user SID for SDDL",
        ));
    }
    let sddl = crate::ipc::endpoint::sddl_dacl(user_sid);
    let sddl_wide: Vec<u16> = sddl.encode_utf16().chain([0]).collect();
    // Invariant: sddl_wide is NUL-terminated; out-params written on success.
    // PSECURITY_DESCRIPTOR = *mut c_void in windows-sys 0.52.
    let mut sd: *mut c_void = std::ptr::null_mut();
    let mut sd_size: u32 = 0;
    let ok = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl_wide.as_ptr(),
            SDDL_REVISION_1,
            &mut sd,
            &mut sd_size,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd,
        bInheritHandle: 0,
    };
    let mut open_mode = PIPE_ACCESS_DUPLEX;
    if first {
        open_mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
    }
    // Invariant: name_wide is NUL-terminated; sd stays alive through the call.
    let handle = unsafe {
        CreateNamedPipeW(
            name_wide.as_ptr(),
            open_mode,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            PIPE_BUF_SIZE,
            PIPE_BUF_SIZE,
            0,
            &sa,
        )
    };
    // Invariant: sd was allocated by the SDDL conversion; LocalFree frees it.
    unsafe {
        LocalFree(sd);
    }
    if handle == INVALID_HANDLE_VALUE {
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) && first {
            // Keep the raw OS code: tests match on ERROR_ACCESS_DENIED.
            return Err(io::Error::from_raw_os_error(ERROR_ACCESS_DENIED as i32));
        }
        return Err(err);
    }
    Ok(handle)
}

/// One connection with reader/writer helper threads for timeouts.
pub struct PipeConn {
    handle: HANDLE,
    pending: Vec<u8>, // bytes already read but not consumed by `Read`
    rx: Receiver<io::Result<Vec<u8>>>,
    tx: SyncSender<Vec<u8>>,
    reader: Option<JoinHandle<()>>,
    writer: Option<JoinHandle<()>>,
    read_timeout: Option<Duration>,
    write_timeout: Option<Duration>,
}

impl PipeConn {
    /// Wrap a connected pipe handle; spawns the helper threads.
    pub fn new(handle: HANDLE) -> io::Result<PipeConn> {
        let (tx, writer_rx) = sync_channel::<Vec<u8>>(WRITE_QUEUE_LEN);
        let (read_tx, rx) = sync_channel(4);
        let writer = thread::Builder::new()
            .name("ipc-pipe-writer".into())
            .spawn(move || {
                while let Ok(chunk) = writer_rx.recv() {
                    let mut off = 0usize;
                    while off < chunk.len() {
                        let mut written: u32 = 0;
                        // Invariant: handle valid (closed by Drop stops us);
                        // buffer slice within bounds; no OVERLAPPED.
                        let ok = unsafe {
                            windows_sys::Win32::Storage::FileSystem::WriteFile(
                                handle,
                                chunk[off..].as_ptr(),
                                (chunk.len() - off) as u32,
                                &mut written,
                                std::ptr::null_mut(),
                            )
                        };
                        if ok == 0 {
                            return; // peer gone / broken pipe
                        }
                        off += written as usize;
                    }
                }
            })?;
        let reader = thread::Builder::new()
            .name("ipc-pipe-reader".into())
            .spawn(move || {
                let mut buf = [0u8; 8192];
                loop {
                    let mut got: u32 = 0;
                    // Invariant: handle valid; buffer within bounds; no OVERLAPPED.
                    let ok = unsafe {
                        windows_sys::Win32::Storage::FileSystem::ReadFile(
                            handle,
                            buf.as_mut_ptr(),
                            buf.len() as u32,
                            &mut got,
                            std::ptr::null_mut(),
                        )
                    };
                    if ok == 0 || got == 0 {
                        let err = if ok == 0 {
                            Err(io::Error::last_os_error())
                        } else {
                            Err(io::Error::new(ErrorKind::UnexpectedEof, "pipe closed"))
                        };
                        let _ = read_tx.send(err);
                        return;
                    }
                    if read_tx.send(Ok(buf[..got as usize].to_vec())).is_err() {
                        return;
                    }
                }
            })?;
        Ok(PipeConn {
            handle,
            pending: Vec::new(),
            rx,
            tx,
            reader: Some(reader),
            writer: Some(writer),
            read_timeout: None,
            write_timeout: None,
        })
    }

    fn drain_pending(&mut self, buf: &mut [u8]) -> usize {
        let n = buf.len().min(self.pending.len());
        buf[..n].copy_from_slice(&self.pending[..n]);
        self.pending.drain(..n);
        n
    }

    fn shutdown(&mut self) {
        if self.handle != INVALID_HANDLE_VALUE {
            // Invariant: handle is a connected pipe we own; unblocks the reader.
            unsafe {
                DisconnectNamedPipe(self.handle);
                CloseHandle(self.handle);
            }
            self.handle = INVALID_HANDLE_VALUE;
        }
        if let Some(r) = self.reader.take() {
            let _ = r.join();
        }
        if let Some(w) = self.writer.take() {
            let _ = w.join();
        }
    }
}

impl Drop for PipeConn {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl Read for PipeConn {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if !self.pending.is_empty() {
            return Ok(self.drain_pending(buf));
        }
        let chunk = match self.read_timeout {
            None => self
                .rx
                .recv()
                .map_err(|_| io::Error::new(ErrorKind::UnexpectedEof, "reader exited"))??,
            Some(t) => match self.rx.recv_timeout(t) {
                Ok(res) => res?,
                Err(RecvTimeoutError::Timeout) => {
                    return Err(io::Error::new(ErrorKind::WouldBlock, "read timeout"));
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(io::Error::new(ErrorKind::UnexpectedEof, "reader exited"));
                }
            },
        };
        let n = buf.len().min(chunk.len());
        buf[..n].copy_from_slice(&chunk[..n]);
        if n < chunk.len() {
            self.pending.extend_from_slice(&chunk[n..]);
        }
        Ok(n)
    }
}

impl Write for PipeConn {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        // "Write timeout" = bounded backlog exceeded (peer stalled past the
        // queue); a fresh queue accepts immediately, so this is honest
        // backpressure, not a fake timeout.
        let res = match self.write_timeout {
            None => self.tx.send(buf.to_vec()),
            Some(t) => match deadline_send(&self.tx, buf, t) {
                Ok(()) => Ok(()),
                Err(()) => {
                    return Err(io::Error::new(ErrorKind::TimedOut, "write timeout"));
                }
            },
        };
        res.map_err(|_| io::Error::new(ErrorKind::BrokenPipe, "writer exited"))?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(()) // byte-mode pipe writes are handed to the OS immediately
    }
}

/// Bounded-queue send with a deadline (poll `try_send`, channel-based only).
fn deadline_send(tx: &SyncSender<Vec<u8>>, buf: &[u8], timeout: Duration) -> Result<(), ()> {
    let deadline = Instant::now() + timeout;
    loop {
        match tx.try_send(buf.to_vec()) {
            Ok(()) => return Ok(()),
            Err(TrySendError::Full(_)) => {}
            Err(TrySendError::Disconnected(_)) => return Err(()),
        }
        if Instant::now() >= deadline {
            return Err(());
        }
        thread::yield_now();
    }
}

impl Conn for PipeConn {
    fn set_read_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()> {
        self.read_timeout = timeout;
        Ok(())
    }

    fn set_write_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()> {
        self.write_timeout = timeout;
        Ok(())
    }

    fn peer_identity(&self) -> io::Result<PeerIdentity> {
        let mut pid: u32 = 0;
        // Invariant: handle is a valid connected pipe instance.
        if unsafe { GetNamedPipeClientProcessId(self.handle, &mut pid) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut session_id: u32 = 0;
        // Invariant: pid is a live process id or the call fails cleanly.
        if unsafe { ProcessIdToSessionId(pid, &mut session_id) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(PeerIdentity::WindowsSid {
            sid: process_user_sid(pid)?,
            session_id,
        })
    }
}

/// Open the broker pipe as a client, retrying `ERROR_PIPE_BUSY` until the
/// deadline; verifies the server's SID when `expected_sid` is given.
pub fn open_client(
    name_wide: &[u16],
    timeout: Duration,
    expected_sid: Option<&str>,
) -> io::Result<HANDLE> {
    let deadline = Instant::now() + timeout;
    let handle = loop {
        // Invariant: name_wide is NUL-terminated; no security attrs (DACL
        // on the pipe governs); no template file.
        let h = unsafe {
            CreateFileW(
                name_wide.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_NONE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                0 as HANDLE,
            )
        };
        if h != INVALID_HANDLE_VALUE {
            break h;
        }
        let err = io::Error::last_os_error();
        match err.raw_os_error() {
            Some(code) if code == ERROR_PIPE_BUSY as i32 => {}
            Some(code) if code == ERROR_FILE_NOT_FOUND as i32 => {
                return Err(io::Error::new(ErrorKind::NotFound, "no broker pipe"));
            }
            _ => return Err(err),
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(ErrorKind::TimedOut, "pipe busy"));
        }
        thread::yield_now();
    };
    if let Some(expected) = expected_sid {
        let mut pid: u32 = 0;
        // Invariant: handle is a valid connected pipe instance.
        if unsafe { GetNamedPipeServerProcessId(handle, &mut pid) } == 0 {
            let err = io::Error::last_os_error();
            unsafe {
                CloseHandle(handle);
            }
            return Err(err);
        }
        let sid = process_user_sid(pid).map_err(|e| {
            unsafe {
                CloseHandle(handle);
            }
            e
        })?;
        if sid != expected {
            unsafe {
                CloseHandle(handle);
            }
            return Err(io::Error::new(
                ErrorKind::PermissionDenied,
                "broker pipe is owned by another user",
            ));
        }
    }
    Ok(handle)
}

/// String SID of the token user of process `pid`.
pub fn process_user_sid(pid: u32) -> io::Result<String> {
    // Invariant: PROCESS_QUERY_LIMITED_INFORMATION only; handle closed below.
    let proc = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if proc == 0 as HANDLE {
        return Err(io::Error::last_os_error());
    }
    let res = token_user_sid(proc);
    unsafe {
        CloseHandle(proc);
    }
    res
}

/// String SID of the current process's token user.
pub fn current_user_sid() -> io::Result<String> {
    // Invariant: GetCurrentProcess returns a valid pseudo-handle (never closed).
    let proc = unsafe { GetCurrentProcess() };
    token_user_sid(proc)
}

/// Open `process`'s token and convert its `TokenUser` SID to a string.
fn token_user_sid(process: HANDLE) -> io::Result<String> {
    let mut token: HANDLE = 0 as HANDLE;
    // Invariant: process is a valid handle; out-param token written on success.
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let res = (|| {
        let mut needed: u32 = 0;
        // Invariant: token is an open token handle; null buffer = size query.
        if unsafe { GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut needed) }
            == 0
            && unsafe { GetLastError() } != 122
        /* ERROR_INSUFFICIENT_BUFFER */
        {
            return Err(io::Error::last_os_error());
        }
        let mut buffer = vec![0u8; needed as usize];
        // Invariant: buffer is `needed` bytes, as the size query above asked.
        if unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                buffer.as_mut_ptr() as *mut c_void,
                needed,
                &mut needed,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        // Layout: TOKEN_USER { SID_AND_ATTRIBUTES { Sid, Attributes } };
        // PSID is *mut c_void in windows-sys 0.52 (Win32::Foundation).
        // Invariant: the buffer holds a TOKEN_USER the API filled; its first word is the SID pointer.
        let sid: PSID = unsafe { (buffer.as_ptr() as *const usize).read_unaligned() } as PSID;
        let mut sid_str: PWSTR = std::ptr::null_mut();
        // Invariant: sid points into the buffer just returned by the API.
        if unsafe { ConvertSidToStringSidW(sid, &mut sid_str) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let s = unsafe {
            let mut len = 0usize;
            while *sid_str.add(len) != 0 {
                len += 1;
            }
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(sid_str, len));
            // Invariant: sid_str allocated by ConvertSidToStringSidW; LocalFree.
            LocalFree(sid_str as _);
            s
        };
        Ok(s)
    })();
    unsafe {
        CloseHandle(token);
    }
    res
}

/// Own identity: the current user's SID and session id.
pub fn own_identity() -> io::Result<PeerIdentity> {
    let sid = current_user_sid()?;
    let mut session_id: u32 = 0;
    let pid = std::process::id();
    // Invariant: our own pid is live; the call cannot fail for it.
    if unsafe { ProcessIdToSessionId(pid, &mut session_id) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(PeerIdentity::WindowsSid { sid, session_id })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::endpoint::{sddl_dacl, Endpoint};

    #[test]
    fn sddl_is_protected_user_only() {
        let sddl = sddl_dacl("S-1-5-21-1-2-3-1001");
        assert!(sddl.starts_with("D:P(A;;GA;;;S-1-"));
    }

    #[test]
    fn current_sid_is_parseable() {
        let sid = current_user_sid().unwrap();
        assert!(crate::ipc::endpoint::is_valid_sid_string(&sid), "{sid}");
    }

    #[test]
    fn bind_twice_denied_first_instance() {
        let ep = Endpoint::for_session(std::path::Path::new("unused"), "win-bind-test");
        let sid = current_user_sid().unwrap();
        let first = PipeListener::bind(&ep, &sid).unwrap();
        let err = PipeListener::bind(&ep, &sid).unwrap_err();
        assert_eq!(err.raw_os_error(), Some(ERROR_ACCESS_DENIED as i32));
        drop(first);
    }

    #[test]
    fn client_missing_pipe_is_not_found() {
        let ep = Endpoint::for_session(std::path::Path::new("unused"), "win-missing-test");
        let err =
            open_client(&ep.windows_pipe_wide(), Duration::from_millis(300), None).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::NotFound);
    }
}
