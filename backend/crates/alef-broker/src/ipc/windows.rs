// SPDX-License-Identifier: MIT OR Apache-2.0
//! Windows transport: named pipes via `windows-sys` (OVERLAPPED I/O).
//!
//! Choices (also documented in docs/stages/m11-warm-start.md):
//! - DACL built from SDDL `D:P(A;;GA;;;<user sid>)` (no inheritance, user
//!   only), `PIPE_REJECT_REMOTE_CLIENTS`, and `FILE_FLAG_FIRST_PIPE_INSTANCE`
//!   on the FIRST instance only: a second broker on the same name gets
//!   `ERROR_ACCESS_DENIED`.
//! - I/O model: the pipe handle is opened with `FILE_FLAG_OVERLAPPED` on BOTH
//!   sides, and every read/write carries its own manual-reset event. A
//!   blocking read on one thread therefore never serializes a concurrent
//!   write from another thread (the earlier reader/writer-threads + mpsc
//!   design deadlocked for exactly that reason: I/O on one synchronous
//!   handle is serialized). Timeouts: `WaitForSingleObject(event, ms)`;
//!   on timeout `CancelIoEx` + a blocking `GetOverlappedResult` to reap the
//!   cancelled operation; read timeout = `WouldBlock`, write timeout =
//!   `TimedOut`.
//! - Accept: `ConnectNamedPipe` overlapped on a per-accept event; the
//!   listener's manual-reset stop event (set by `wake`, called from
//!   `Server::stop`) unblocks the wait via `WaitForMultipleObjects`.
//! - Client verifies the server conservatively: `GetNamedPipeServerProcessId`
//!   -> `OpenProcess` -> `OpenProcessToken` -> `TokenUser` SID must equal the
//!   client's own user SID (same-user broker). Combined with the unique name
//!   + `FIRST_PIPE_INSTANCE` + user-only DACL this authenticates the server
//!   as a process of the same user; the server checks the client's SID AND
//!   session id symmetrically.

use std::ffi::c_void;
use std::io::{self, ErrorKind, Read, Write};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use windows_sys::core::PWSTR;
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, LocalFree, ERROR_ACCESS_DENIED, ERROR_BROKEN_PIPE,
    ERROR_FILE_NOT_FOUND, ERROR_IO_PENDING, ERROR_NO_DATA, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED,
    ERROR_PIPE_NOT_CONNECTED, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE, PSID,
    WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, TokenUser, SECURITY_ATTRIBUTES, TOKEN_QUERY,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED,
    FILE_SHARE_NONE, OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeClientProcessId,
    GetNamedPipeServerProcessId, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
    PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows_sys::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, OpenProcess, OpenProcessToken, ResetEvent, SetEvent,
    WaitForMultipleObjects, WaitForSingleObject, INFINITE, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

use crate::ipc::endpoint::Endpoint;
use crate::ipc::{Conn, Listener, PeerIdentity};

/// Pipe buffer size.
const PIPE_BUF_SIZE: u32 = 64 * 1024;
/// Upper bound for a single wait conversion (ms).
const MAX_WAIT_MS: u32 = u32::MAX - 1;

fn win_err() -> io::Error {
    io::Error::last_os_error()
}

fn is_disconnect(code: u32) -> bool {
    code == ERROR_BROKEN_PIPE || code == ERROR_NO_DATA || code == ERROR_PIPE_NOT_CONNECTED
}

/// Create a manual-reset, initially-unset, unnamed event.
fn new_event() -> io::Result<HANDLE> {
    // Invariant: null name/attrs; out = event handle or INVALID.
    let h = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
    if h == 0 as HANDLE {
        Err(win_err())
    } else {
        Ok(h)
    }
}

/// OVERLAPPED with the operation's completion event.
fn new_overlapped(event: HANDLE) -> OVERLAPPED {
    // Invariant: OVERLAPPED must be zeroed and kept alive during the op.
    let mut o: OVERLAPPED = unsafe { std::mem::zeroed() };
    o.hEvent = event;
    o
}

fn wait_ms(timeout: Option<Duration>) -> u32 {
    match timeout {
        None => INFINITE,
        Some(t) => t.as_millis().min(MAX_WAIT_MS as u128) as u32,
    }
}

/// Listening side of the broker pipe. `FIRST_PIPE_INSTANCE` is used exactly
/// once, in [`PipeListener::bind`], so the name is owned or we fail.
pub struct PipeListener {
    name: Vec<u16>, // NUL-terminated wide pipe name
    handle: Mutex<HANDLE>,
    stop_event: HANDLE,
}

impl std::fmt::Debug for PipeListener {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The handles are not printed.
        f.debug_struct("PipeListener").finish_non_exhaustive()
    }
}

impl PipeListener {
    /// Create the first pipe instance with a user-only DACL.
    pub fn bind(ep: &Endpoint, user_sid: &str) -> io::Result<PipeListener> {
        let handle = create_instance(&ep.windows_pipe_wide(), user_sid, true)?;
        let stop_event = new_event()?;
        Ok(PipeListener {
            name: ep.windows_pipe_wide(),
            handle: Mutex::new(handle),
            stop_event,
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
        // Invariant: stop_event was created by new_event and is ours.
        unsafe {
            CloseHandle(self.stop_event);
        }
    }
}

impl Listener for PipeListener {
    fn accept(&self) -> io::Result<Box<dyn Conn>> {
        let mut guard = self.handle.lock().expect("pipe listener mutex");
        let listening = *guard;
        let conn_event = new_event()?;
        let result = accept_overlapped(listening, conn_event, self.stop_event);
        // Invariant: conn_event is ours; the overlapped op has been reaped
        // (or never started) before we close it.
        unsafe {
            CloseHandle(conn_event);
        }
        match result {
            Ok(()) => {
                // Immediately open the next listening instance for clients.
                *guard = self.next_instance()?;
                drop(guard);
                Ok(Box::new(PipeConn::new(listening, true)?))
            }
            Err(e) => Err(e),
        }
    }

    fn wake(&self) {
        // Unblock a pending accept; `Server::stop` sets the stopping flag
        // before calling this, so the loop exits on the returned error.
        unsafe {
            SetEvent(self.stop_event);
        }
    }
}

/// Wait for a client (or stop) on `listening`; reaps the overlapped op.
fn accept_overlapped(listening: HANDLE, conn_event: HANDLE, stop_event: HANDLE) -> io::Result<()> {
    unsafe {
        ResetEvent(conn_event);
    }
    let mut ovl = new_overlapped(conn_event);
    // Invariant: listening is a valid pipe handle we own; no other op is
    // in flight on it; ovl outlives the async operation (reaped below).
    let ok = unsafe { ConnectNamedPipe(listening, &mut ovl) };
    if ok == 0 {
        let err = win_err();
        let raw = err.raw_os_error().unwrap_or(0) as u32;
        if raw == ERROR_PIPE_CONNECTED {
            return Ok(()); // client connected before the call
        }
        if raw != ERROR_IO_PENDING {
            return Err(err);
        }
    }
    let handles = [conn_event, stop_event];
    // Invariant: handles are valid event handles; wait-any.
    let w = unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, INFINITE) };
    if w == WAIT_OBJECT_0 + 1 {
        // Stopped: cancel the pending connect and reap it.
        unsafe {
            CancelIoEx(listening, std::ptr::null_mut());
            let mut got: u32 = 0;
            GetOverlappedResult(listening, &mut ovl, &mut got, 1);
        }
        return Err(io::Error::new(
            ErrorKind::ConnectionAborted,
            "broker listener stopped",
        ));
    }
    if w != WAIT_OBJECT_0 {
        return Err(win_err());
    }
    let mut got: u32 = 0;
    // Invariant: the event fired, so the result is available immediately.
    if unsafe { GetOverlappedResult(listening, &mut ovl, &mut got, 0) } == 0 {
        let err = win_err();
        if is_disconnect(err.raw_os_error().unwrap_or(0) as u32) {
            // Client connected and vanished; caller may retry.
            return Err(io::Error::new(
                ErrorKind::ConnectionReset,
                "client disconnected during accept",
            ));
        }
        return Err(err);
    }
    Ok(())
}

/// Create one listening pipe instance (always overlapped).
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
        return Err(win_err());
    }
    let sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd,
        bInheritHandle: 0,
    };
    let mut open_mode = PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED;
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
        let err = win_err();
        if err.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) && first {
            // Keep the raw OS code: tests match on ERROR_ACCESS_DENIED.
            return Err(io::Error::from_raw_os_error(ERROR_ACCESS_DENIED as i32));
        }
        return Err(err);
    }
    Ok(handle)
}

/// One connection; every operation carries its own manual-reset event so
/// concurrent reads and writes never serialize (see module docs).
pub struct PipeConn {
    handle: HANDLE,
    server_side: bool, // Drop: server disconnects the pipe, client just closes
    read_event: HANDLE,
    write_event: HANDLE,
    read_timeout: Option<Duration>,
    write_timeout: Option<Duration>,
}

impl PipeConn {
    /// Wrap a connected pipe handle. `server_side` selects Drop semantics
    /// (server: `DisconnectNamedPipe` + `CloseHandle`; client: `CloseHandle`).
    pub fn new(handle: HANDLE, server_side: bool) -> io::Result<PipeConn> {
        let read_event = new_event()?;
        let write_event = match new_event() {
            Ok(e) => e,
            Err(e) => {
                unsafe {
                    CloseHandle(read_event);
                }
                return Err(e);
            }
        };
        Ok(PipeConn {
            handle,
            server_side,
            read_event,
            write_event,
            read_timeout: None,
            write_timeout: None,
        })
    }

    fn close(&mut self) {
        if self.handle != INVALID_HANDLE_VALUE {
            if self.server_side {
                // Invariant: connected server-side instance we own; unblocks
                // the peer's pending reads with a broken pipe.
                unsafe {
                    DisconnectNamedPipe(self.handle);
                    CloseHandle(self.handle);
                }
            } else {
                // Invariant: client handle we own.
                unsafe {
                    CloseHandle(self.handle);
                }
            }
            self.handle = INVALID_HANDLE_VALUE;
        }
    }
}

/// Run one overlapped op to completion under `timeout`; on timeout cancels
/// and reaps it. `op` invokes ReadFile/WriteFile (BOOL = i32). Returns the
/// transferred byte count or the failure/timout marker.
fn overlapped_io(
    handle: HANDLE,
    event: HANDLE,
    timeout: Option<Duration>,
    op: impl FnOnce(HANDLE, *mut u8, u32, *mut OVERLAPPED) -> i32,
    buf: *mut u8,
    len: u32,
) -> Result<u32, OverlappedOutcome> {
    unsafe {
        ResetEvent(event);
    }
    let mut ovl = new_overlapped(event);
    // Invariant: handle valid and opened with FILE_FLAG_OVERLAPPED; ovl
    // outlives the operation (reaped on every path below).
    let ok = unsafe { op(handle, buf, len, &mut ovl) };
    if ok == 0 {
        let raw = win_err().raw_os_error().unwrap_or(0) as u32;
        if raw != ERROR_IO_PENDING {
            return Err(OverlappedOutcome::Failed(raw));
        }
    }
    // Invariant: wait on our own event only.
    let w = unsafe { WaitForSingleObject(event, wait_ms(timeout)) };
    if w == WAIT_TIMEOUT {
        unsafe {
            CancelIoEx(handle, std::ptr::null_mut());
            let mut got: u32 = 0;
            // Blocking wait: reap the cancelled operation before dropping
            // the OVERLAPPED (FALSE would leave it potentially pending).
            GetOverlappedResult(handle, &mut ovl, &mut got, 1);
        }
        return Err(OverlappedOutcome::TimedOut);
    }
    if w != WAIT_OBJECT_0 {
        return Err(OverlappedOutcome::Failed(
            win_err().raw_os_error().unwrap_or(0) as u32,
        ));
    }
    let mut got: u32 = 0;
    // Invariant: the event fired, so the result is available immediately.
    if unsafe { GetOverlappedResult(handle, &mut ovl, &mut got, 0) } == 0 {
        return Err(OverlappedOutcome::Failed(
            win_err().raw_os_error().unwrap_or(0) as u32,
        ));
    }
    Ok(got)
}

enum OverlappedOutcome {
    /// Completed with the given raw Win32 error code.
    Failed(u32),
    /// Timed out and was cancelled/reaped; report WouldBlock/TimedOut.
    TimedOut,
}

impl PipeConn {
    /// One overlapped read; `&self` on purpose: reads and writes use separate
    /// events on the same overlapped handle and are safe in parallel.
    pub fn read_some(&self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        match overlapped_io(
            self.handle,
            self.read_event,
            self.read_timeout,
            |h, p, n, o| unsafe { ReadFile(h, p, n, std::ptr::null_mut(), o) },
            buf.as_mut_ptr(),
            buf.len() as u32,
        ) {
            Ok(n) => Ok(n as usize),
            Err(OverlappedOutcome::TimedOut) => {
                // Same convention as Unix socket read timeouts.
                Err(io::Error::new(ErrorKind::WouldBlock, "read timeout"))
            }
            Err(OverlappedOutcome::Failed(code)) => {
                if is_disconnect(code) {
                    Ok(0) // peer closed: EOF
                } else {
                    Err(io::Error::from_raw_os_error(code as i32))
                }
            }
        }
    }

    /// One overlapped write of up to `buf.len()` bytes; `&self` like
    /// [`PipeConn::read_some`]. Returns the bytes actually written.
    pub fn write_some(&self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        match overlapped_io(
            self.handle,
            self.write_event,
            self.write_timeout,
            |h, p, n, o| unsafe { WriteFile(h, p, n, std::ptr::null_mut(), o) },
            buf.as_ptr() as *mut u8,
            buf.len() as u32,
        ) {
            Ok(n) => Ok(n as usize),
            Err(OverlappedOutcome::TimedOut) => {
                // The stream is unsynchronized after a partial write — same
                // fail-closed stance as the framing layer after an error.
                Err(io::Error::new(ErrorKind::TimedOut, "write timeout"))
            }
            Err(OverlappedOutcome::Failed(code)) => {
                if is_disconnect(code) {
                    Err(io::Error::new(
                        ErrorKind::BrokenPipe,
                        "peer closed the pipe",
                    ))
                } else {
                    Err(io::Error::from_raw_os_error(code as i32))
                }
            }
        }
    }
}

impl Drop for PipeConn {
    fn drop(&mut self) {
        self.close();
        // Invariant: both events were created by new_event and are ours; the
        // handle is closed above, so no overlapped op can still use them.
        unsafe {
            CloseHandle(self.read_event);
            CloseHandle(self.write_event);
        }
    }
}

impl Read for PipeConn {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.read_some(buf)
    }
}

impl Write for PipeConn {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut total = 0usize;
        while total < buf.len() {
            match self.write_some(&buf[total..]) {
                Ok(0) => {
                    return Err(io::Error::new(
                        ErrorKind::BrokenPipe,
                        "pipe wrote zero bytes",
                    ));
                }
                Ok(n) => total += n,
                Err(e) => return Err(e),
            }
        }
        Ok(total)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(()) // byte-mode pipe writes are handed to the OS immediately
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
            return Err(win_err());
        }
        let mut session_id: u32 = 0;
        // Invariant: pid is a live process id or the call fails cleanly.
        if unsafe { ProcessIdToSessionId(pid, &mut session_id) } == 0 {
            return Err(win_err());
        }
        Ok(PeerIdentity::WindowsSid {
            sid: process_user_sid(pid)?,
            session_id,
        })
    }
}

/// Open the broker pipe as a client (overlapped handle), retrying
/// `ERROR_PIPE_BUSY` until the deadline; verifies the server's SID when
/// `expected_sid` is given.
pub fn open_client(
    name_wide: &[u16],
    timeout: Duration,
    expected_sid: Option<&str>,
) -> io::Result<HANDLE> {
    let deadline = std::time::Instant::now() + timeout;
    let handle = loop {
        // Invariant: name_wide is NUL-terminated; no security attrs (DACL
        // on the pipe governs); overlapped, like the server side.
        let h = unsafe {
            CreateFileW(
                name_wide.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_NONE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                0 as HANDLE,
            )
        };
        if h != INVALID_HANDLE_VALUE {
            break h;
        }
        let err = win_err();
        match err.raw_os_error() {
            Some(code) if code == ERROR_PIPE_BUSY as i32 => {}
            Some(code) if code == ERROR_FILE_NOT_FOUND as i32 => {
                return Err(io::Error::new(ErrorKind::NotFound, "no broker pipe"));
            }
            _ => return Err(err),
        }
        if std::time::Instant::now() >= deadline {
            return Err(io::Error::new(ErrorKind::TimedOut, "pipe busy"));
        }
        std::thread::yield_now();
    };
    if let Some(expected) = expected_sid {
        let mut pid: u32 = 0;
        // Invariant: handle is a valid connected pipe instance.
        if unsafe { GetNamedPipeServerProcessId(handle, &mut pid) } == 0 {
            let err = win_err();
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
        return Err(win_err());
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
        return Err(win_err());
    }
    let res = (|| {
        let mut needed: u32 = 0;
        // Invariant: token is an open token handle; null buffer = size query.
        if unsafe { GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut needed) }
            == 0
            && unsafe { GetLastError() } != 122
        /* ERROR_INSUFFICIENT_BUFFER */
        {
            return Err(win_err());
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
            return Err(win_err());
        }
        // Layout: TOKEN_USER { SID_AND_ATTRIBUTES { Sid, Attributes } };
        // PSID is *mut c_void in windows-sys 0.52 (Win32::Foundation).
        // Invariant: the buffer holds a TOKEN_USER the API filled; its first word is the SID pointer.
        let sid: PSID = unsafe { (buffer.as_ptr() as *const usize).read_unaligned() } as PSID;
        let mut sid_str: PWSTR = std::ptr::null_mut();
        // Invariant: sid points into the buffer just returned by the API.
        if unsafe { ConvertSidToStringSidW(sid, &mut sid_str) } == 0 {
            return Err(win_err());
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
        return Err(win_err());
    }
    Ok(PeerIdentity::WindowsSid { sid, session_id })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::endpoint::{sddl_dacl, Endpoint};
    use std::sync::mpsc::channel;
    use std::thread::{self, JoinHandle};
    use std::time::Instant;

    /// Join `handle` with a 30 s deadline; panic with `what` on overrun.
    fn join_bounded<T: Send + 'static>(what: &'static str, handle: JoinHandle<T>) -> T {
        let (tx, rx) = channel();
        thread::spawn(move || {
            let _ = tx.send(handle.join().expect("worker thread panicked"));
        });
        match rx.recv_timeout(Duration::from_secs(30)) {
            Ok(v) => v,
            Err(_) => panic!("test deadline exceeded: {what}"),
        }
    }

    /// Run `f` on another thread; panic with `what` if it exceeds 30 s.
    fn bounded<T: Send + 'static>(what: &'static str, f: impl FnOnce() -> T + Send + 'static) -> T {
        let (tx, rx) = channel();
        thread::spawn(move || {
            let _ = tx.send(f());
        });
        match rx.recv_timeout(Duration::from_secs(30)) {
            Ok(v) => v,
            Err(_) => panic!("test deadline exceeded: {what}"),
        }
    }

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

    #[test]
    fn stop_unblocks_pending_accept() {
        let ep = Endpoint::for_session(std::path::Path::new("unused"), "win-stop-test");
        let sid = current_user_sid().unwrap();
        let listener = PipeListener::bind(&ep, &sid).unwrap();
        thread::scope(|s| {
            let l = &listener;
            let (tx, rx) = channel();
            s.spawn(move || {
                let _ = tx.send(l.accept());
            });
            // The stop event is manual-reset: SetEvent works whether or not
            // the accept has entered its wait yet. No sleeps anywhere.
            listener.wake();
            let res = rx
                .recv_timeout(Duration::from_secs(30))
                .expect("accept deadline after wake");
            // On stop the accept must fail, not return a connection.
            assert!(res.is_err(), "accept must fail after wake, not succeed");
        });
    }

    #[test]
    fn concurrent_read_and_write_do_not_block() {
        let ep = Endpoint::for_session(std::path::Path::new("unused"), "win-echo-test");
        let sid = current_user_sid().unwrap();
        let listener = PipeListener::bind(&ep, &sid).unwrap();
        // Echo server: read (with timeout) and write back.
        let server = thread::spawn(move || {
            let mut conn = listener.accept().expect("accept");
            conn.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut buf = [0u8; 64];
            let n = conn.read(&mut buf).expect("server read");
            conn.write_all(&buf[..n]).expect("server write");
        });
        let handle = open_client(&ep.windows_pipe_wide(), Duration::from_secs(5), None)
            .expect("client open");
        let mut client = PipeConn::new(handle, false).expect("client conn");
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        client
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        // Read and write IN PARALLEL on the SAME connection from two threads
        // (safe: `read_some`/`write_some` take &self and use separate events
        // on the overlapped handle). This is the regression test for the
        // old design where one synchronous handle serialized the I/O.
        let (tx, rx) = channel();
        let start = Instant::now();
        thread::scope(|s| {
            let c = &client;
            s.spawn(move || {
                let mut b = [0u8; 4];
                let mut got = 0;
                while got < 4 {
                    let n = c.read_some(&mut b[got..]).expect("client read");
                    assert!(n > 0);
                    got += n;
                }
                assert_eq!(&b, b"PING");
                let _ = tx.send(());
            });
            s.spawn(move || {
                let mut sent = 0;
                while sent < 4 {
                    sent += c.write_some(b"PING").expect("client write");
                }
            });
        });
        rx.recv_timeout(Duration::from_secs(10))
            .expect("echo round-trip deadline");
        assert!(start.elapsed() < Duration::from_secs(10));
        join_bounded("echo server", server);
        drop(client);
    }
}
