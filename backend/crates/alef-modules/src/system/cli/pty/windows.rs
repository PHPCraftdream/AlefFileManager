// SPDX-License-Identifier: MIT OR Apache-2.0
//! Suspended ConPTY creation and nonblocking pipe polling.
use super::super::tree::Killer;
use alef_core::{AlefError, ErrorCode};
use bytes::Bytes;
use std::{
    collections::BTreeMap,
    ffi::OsStr,
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    time::Duration,
};
use windows_sys::Win32::{
    Foundation::{ERROR_BROKEN_PIPE, ERROR_NO_DATA},
    Storage::FileSystem::{ReadFile, WriteFile},
    System::{Console::*, Pipes::*, Threading::*},
};

pub(super) struct Terminal {
    console: Mutex<Option<HPCON>>,
    read: Mutex<Option<OwnedHandle>>,
    write: Mutex<Option<OwnedHandle>>,
    done: AtomicBool,
}
pub(super) struct Child {
    process: OwnedHandle,
    killer: Killer,
}

fn wide(s: &OsStr) -> Result<Vec<u16>, AlefError> {
    let mut value: Vec<u16> = s.encode_wide().collect();
    if value.contains(&0) {
        return Err(super::super::invalid("NUL in process argument"));
    }
    value.push(0);
    Ok(value)
}
fn pipe() -> Result<(OwnedHandle, OwnedHandle), AlefError> {
    let (mut read, mut write) = (std::ptr::null_mut(), std::ptr::null_mut());
    // SAFETY: output pointers are valid; null security attributes make both handles non-inheritable.
    if unsafe { CreatePipe(&mut read, &mut write, std::ptr::null(), 65536) } == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: successful CreatePipe returned two distinct owned handles.
    Ok(unsafe {
        (
            OwnedHandle::from_raw_handle(read),
            OwnedHandle::from_raw_handle(write),
        )
    })
}
fn quote(arg: &str) -> String {
    let mut out = String::from("\"");
    let mut slashes = 0;
    for c in arg.chars() {
        if c == '\\' {
            slashes += 1;
            continue;
        }
        out.extend(std::iter::repeat_n(
            '\\',
            if c == '"' { slashes * 2 + 1 } else { slashes },
        ));
        slashes = 0;
        out.push(c);
    }
    out.extend(std::iter::repeat_n('\\', slashes * 2));
    out.push('"');
    out
}
struct Attributes {
    storage: Vec<usize>,
    initialized: bool,
}
impl Attributes {
    fn pointer(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.storage.as_mut_ptr().cast()
    }
    fn new(console: HPCON) -> Result<Self, AlefError> {
        let mut size = 0;
        // SAFETY: the first call queries the required byte count, without dereferencing a list.
        unsafe {
            InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut size);
        }
        let mut this = Self {
            storage: vec![0; size.div_ceil(std::mem::size_of::<usize>())],
            initialized: false,
        };
        // SAFETY: storage has native pointer alignment and at least the queried size.
        if unsafe { InitializeProcThreadAttributeList(this.pointer(), 1, 0, &mut size) } == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        this.initialized = true;
        // SAFETY: initialized list has one slot; HPCON is passed by value as required for this attribute.
        if unsafe {
            UpdateProcThreadAttribute(
                this.pointer(),
                0,
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                console as *const _,
                std::mem::size_of::<HPCON>(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(this)
    }
}
impl Drop for Attributes {
    fn drop(&mut self) {
        if self.initialized {
            // SAFETY: initialized list remains allocated until after deletion.
            unsafe {
                DeleteProcThreadAttributeList(self.pointer());
            }
        }
    }
}

pub(super) fn open(
    program: PathBuf,
    args: Vec<String>,
    cwd: PathBuf,
    env: Vec<(String, String)>,
    cols: u16,
    rows: u16,
) -> Result<(Terminal, Child, Killer, u32), AlefError> {
    if matches!(
        program
            .extension()
            .and_then(|s| s.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("cmd" | "bat")
    ) {
        return Err(super::super::invalid(
            "terminal requires a native executable",
        ));
    }
    let exe = wide(program.as_os_str())?;
    let mut line = quote(&program.to_string_lossy());
    for arg in args {
        line.push(' ');
        line.push_str(&quote(&arg));
    }
    let mut line = wide(OsStr::new(&line))?;
    let cwd = wide(cwd.as_os_str())?;
    let mut pairs: BTreeMap<String, (String, String)> = std::env::vars_os()
        .map(|(k, v)| {
            let k = k.to_string_lossy().into_owned();
            (k.to_uppercase(), (k, v.to_string_lossy().into_owned()))
        })
        .collect();
    for (k, v) in env {
        pairs.insert(k.to_uppercase(), (k, v));
    }
    let mut environment = Vec::new();
    for (_, (k, v)) in pairs {
        environment.extend(wide(OsStr::new(&format!("{k}={v}")))?);
    }
    environment.push(0);
    let (input_read, input_write) = pipe()?;
    let (output_read, output_write) = pipe()?;
    let mut console = 0;
    // SAFETY: dimensions were bounded; handles remain live throughout creation; output is initialized.
    let hr = unsafe {
        CreatePseudoConsole(
            COORD {
                X: cols as i16,
                Y: rows as i16,
            },
            input_read.as_raw_handle(),
            output_write.as_raw_handle(),
            0,
            &mut console,
        )
    };
    if hr < 0 {
        return Err(AlefError::new(
            ErrorCode::Internal,
            format!("CreatePseudoConsole: {hr:#x}"),
        ));
    }
    let terminal = Terminal {
        console: Mutex::new(Some(console)),
        read: Mutex::new(Some(output_read)),
        write: Mutex::new(Some(input_write)),
        done: AtomicBool::new(false),
    };
    let mut attributes = Attributes::new(console)?;
    // SAFETY: all-zero startup and process information structures are valid initial representations.
    let (mut startup, mut info): (STARTUPINFOEXW, PROCESS_INFORMATION) =
        unsafe { (std::mem::zeroed(), std::mem::zeroed()) };
    startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    startup.StartupInfo.hStdOutput = windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    startup.StartupInfo.hStdError = windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    startup.lpAttributeList = attributes.pointer();
    // SAFETY: all UTF-16 buffers are terminated and live, command line is mutable, attribute list
    // is initialized and console remains live. The primary thread is suspended before user code.
    if unsafe {
        CreateProcessW(
            exe.as_ptr(),
            line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT,
            environment.as_ptr().cast(),
            cwd.as_ptr(),
            &startup.StartupInfo,
            &mut info,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: successful creation transfers the two unique handles to these owners.
    let (process, thread) = unsafe {
        (
            OwnedHandle::from_raw_handle(info.hProcess),
            OwnedHandle::from_raw_handle(info.hThread),
        )
    };
    let killer = match Killer::adopt_handle(process.as_raw_handle()) {
        Ok(killer) => killer,
        Err(error) => {
            // SAFETY: process is the still-suspended owned child, not a PID lookup.
            unsafe {
                TerminateProcess(process.as_raw_handle(), 1);
            }
            return Err(error);
        }
    };
    // SAFETY: this is the actual primary thread returned by CreateProcessW, with a live owner;
    // Job assignment has completed, so descendants cannot escape the spawn-to-assignment window.
    if unsafe { ResumeThread(thread.as_raw_handle()) } == u32::MAX {
        let error = std::io::Error::last_os_error();
        killer.kill();
        return Err(error.into());
    }
    drop(input_read);
    drop(output_write);
    Ok((
        terminal,
        Child {
            process,
            killer: killer.clone(),
        },
        killer,
        info.dwProcessId,
    ))
}
impl Terminal {
    pub(super) fn resize(&self, cols: u16, rows: u16) -> Result<(), AlefError> {
        let guard = self.console.lock().unwrap_or_else(|e| e.into_inner());
        let console = guard.ok_or_else(|| AlefError::new(ErrorCode::Closed, "terminal closed"))?;
        // SAFETY: the mutex excludes closing or concurrent resizing; dimensions are validated.
        let hr = unsafe {
            ResizePseudoConsole(
                console,
                COORD {
                    X: cols as i16,
                    Y: rows as i16,
                },
            )
        };
        if hr < 0 {
            return Err(AlefError::new(
                ErrorCode::Closed,
                format!("ResizePseudoConsole: {hr:#x}"),
            ));
        }
        Ok(())
    }
    pub(super) fn read(&self, buffer: &mut [u8]) -> Result<Option<usize>, AlefError> {
        let guard = self.read.lock().unwrap_or_else(|e| e.into_inner());
        let Some(handle) = guard.as_ref() else {
            return Ok(Some(0));
        };
        let mut available = 0;
        // SAFETY: the read handle is owned and live; only one pump reads it; only available is written.
        if unsafe {
            PeekNamedPipe(
                handle.as_raw_handle(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                &mut available,
                std::ptr::null_mut(),
            )
        } == 0
        {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(ERROR_BROKEN_PIPE as i32) {
                return Ok(Some(0));
            }
            return Err(error.into());
        }
        if available == 0 {
            return Ok(if self.done.load(Ordering::Acquire) {
                Some(0)
            } else {
                None
            });
        }
        let count = usize::min(buffer.len(), available as usize) as u32;
        let mut read = 0;
        // SAFETY: sole-reader Peek establishes at least count bytes, preventing blocking;
        // buffer covers count bytes and the synchronous handle requires null OVERLAPPED.
        if unsafe {
            ReadFile(
                handle.as_raw_handle(),
                buffer.as_mut_ptr(),
                count,
                &mut read,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(Some(read as usize))
    }
    pub(super) async fn write(&self, bytes: Bytes) -> Result<(), AlefError> {
        let mut offset = 0;
        while offset < bytes.len() {
            let written = {
                let guard = self.write.lock().unwrap_or_else(|e| e.into_inner());
                let handle = guard
                    .as_ref()
                    .ok_or_else(|| AlefError::new(ErrorCode::Closed, "terminal input closed"))?;
                let mode = PIPE_NOWAIT;
                // SAFETY: owned pipe is live under the mutex; mode is a valid pipe mode.
                if unsafe {
                    SetNamedPipeHandleState(
                        handle.as_raw_handle(),
                        &mode,
                        std::ptr::null(),
                        std::ptr::null(),
                    )
                } == 0
                {
                    return Err(std::io::Error::last_os_error().into());
                }
                let mut written = 0;
                let count = (bytes.len() - offset).min(4096) as u32;
                // SAFETY: slice covers count bytes; NOWAIT prevents an unread input from blocking.
                if unsafe {
                    WriteFile(
                        handle.as_raw_handle(),
                        bytes[offset..].as_ptr(),
                        count,
                        &mut written,
                        std::ptr::null_mut(),
                    )
                } == 0
                {
                    let error = std::io::Error::last_os_error();
                    if error.raw_os_error() != Some(ERROR_NO_DATA as i32) {
                        return Err(error.into());
                    }
                }
                written as usize
            };
            offset += written;
            if written == 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }
        Ok(())
    }
    pub(super) fn end_input(&self) {
        self.write.lock().unwrap_or_else(|e| e.into_inner()).take();
    }
    pub(super) async fn finished(&self) {
        self.end_input();
        let console = self
            .console
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(console) = console {
            let task = tokio::task::spawn_blocking(move || {
                // SAFETY: unique console ownership transferred here; output pump remains active
                // to drain ClosePseudoConsole's final output, excluding resize via the taken slot.
                unsafe {
                    ClosePseudoConsole(console);
                }
            });
            let _ = task.await;
        }
        self.done.store(true, Ordering::Release);
    }
    pub(super) fn abandon_output(&self) {
        self.read.lock().unwrap_or_else(|e| e.into_inner()).take();
    }
    pub(super) fn stop(&self) {
        self.end_input();
    }
}
impl Drop for Terminal {
    fn drop(&mut self) {
        self.write
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        // Break the output pipe before synchronous close: no pump exists on early errors.
        self.read
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(console) = self
            .console
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            // SAFETY: exclusive ownership; no reads/resizes remain and input has been closed.
            unsafe {
                ClosePseudoConsole(console);
            }
        }
    }
}
impl Child {
    pub(super) async fn wait(self) -> Result<std::process::ExitStatus, AlefError> {
        use std::os::windows::process::ExitStatusExt;
        loop {
            // SAFETY: process handle remains owned; zero timeout is nonblocking.
            match unsafe { WaitForSingleObject(self.process.as_raw_handle(), 0) } {
                0 => break,
                windows_sys::Win32::Foundation::WAIT_FAILED => {
                    return Err(std::io::Error::last_os_error().into());
                }
                _ => {}
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let mut code = 0;
        // SAFETY: process exited and the owned handle remains valid; code is writable.
        if unsafe { GetExitCodeProcess(self.process.as_raw_handle(), &mut code) } == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        self.killer.disarm();
        Ok(std::process::ExitStatus::from_raw(code))
    }
}
impl Drop for Child {
    fn drop(&mut self) {
        self.killer.kill();
    }
}
