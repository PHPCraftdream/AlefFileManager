// SPDX-License-Identifier: MIT OR Apache-2.0
//! Stable std has no spawn_with_attributes (nightly-only). Native CreateProcessW is therefore
//! unavoidable: JOB_LIST assigns KILL_ON_JOB_CLOSE before the primary thread can run. There is
//! no suspended-thread discovery/resume. The non-inheritable job is owned solely by this parent.
//! Console/control events are inherited naturally; this parent waits, never targets an old view.
use std::{
    ffi::{OsStr, OsString},
    io,
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
        process::ExitStatusExt,
    },
    path::Path,
    process::ExitStatus,
    time::Duration,
};
use windows_sys::Win32::{
    Foundation::*,
    System::{Console::*, JobObjects::*, Threading::*},
};

pub(super) struct Child {
    process: OwnedHandle,
    _job: OwnedHandle,
}

fn wide(s: &OsStr) -> io::Result<Vec<u16>> {
    let mut out: Vec<u16> = s.encode_wide().collect();
    if out.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "NUL in process argument",
        ));
    }
    out.push(0);
    Ok(out)
}
fn environment(extra: &[(String, String)]) -> io::Result<Vec<u16>> {
    // Sorted `NAME=VALUE` pairs, NUL separated, double NUL terminated; overrides last.
    let mut pairs: std::collections::BTreeMap<OsString, OsString> = std::env::vars_os().collect();
    for (name, value) in extra {
        if name.contains(['=', '\0']) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "NUL or = in environment name",
            ));
        }
        pairs.insert(OsString::from(name), OsString::from(value));
    }
    let mut block = Vec::new();
    for (name, value) in pairs {
        block.extend(wide(&name)?);
        block.pop(); // wide() appends the terminator; the separator is `=`
        block.push(b'=' as u16);
        block.extend(wide(&value)?); // keeps its terminator: the entry separator
    }
    block.push(0);
    Ok(block)
}
fn quote(s: &OsStr, out: &mut Vec<u16>) -> io::Result<()> {
    out.push(b'"' as u16);
    let mut slashes = 0;
    for c in s.encode_wide() {
        if c == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "NUL in process argument",
            ));
        }
        if c == b'\\' as u16 {
            slashes += 1;
            continue;
        }
        out.extend(std::iter::repeat_n(
            b'\\' as u16,
            if c == b'"' as u16 {
                slashes * 2 + 1
            } else {
                slashes
            },
        ));
        slashes = 0;
        out.push(c);
    }
    out.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2));
    out.push(b'"' as u16);
    Ok(())
}
struct Attributes {
    data: Vec<usize>,
    initialized: bool,
}
impl Attributes {
    fn ptr(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.data.as_mut_ptr().cast()
    }
    fn new(job: &[HANDLE; 1], streams: &[HANDLE]) -> io::Result<Self> {
        let count = if streams.is_empty() { 1 } else { 2 };
        let mut size = 0;
        // SAFETY: size query only, valid writable size pointer.
        unsafe {
            InitializeProcThreadAttributeList(std::ptr::null_mut(), count, 0, &mut size);
        }
        let mut this = Self {
            data: vec![0; size.div_ceil(std::mem::size_of::<usize>())],
            initialized: false,
        };
        // SAFETY: word-aligned allocation covers queried byte size.
        if unsafe { InitializeProcThreadAttributeList(this.ptr(), count, 0, &mut size) } == 0 {
            return Err(io::Error::last_os_error());
        }
        this.initialized = true;
        for (kind, handles) in [
            (PROC_THREAD_ATTRIBUTE_JOB_LIST, job.as_slice()),
            (PROC_THREAD_ATTRIBUTE_HANDLE_LIST, streams),
        ] {
            if handles.is_empty() {
                continue;
            }
            // SAFETY: initialized list with enough slots; caller keeps arrays and live handles
            // allocated through CreateProcessW. Neither attribute transfers ownership.
            if unsafe {
                UpdateProcThreadAttribute(
                    this.ptr(),
                    0,
                    kind as usize,
                    handles.as_ptr().cast(),
                    std::mem::size_of_val(handles),
                    std::ptr::null_mut(),
                    std::ptr::null(),
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(this)
    }
}
impl Drop for Attributes {
    fn drop(&mut self) {
        if self.initialized {
            // SAFETY: initialized list still owns its backing allocation; deleted exactly once.
            unsafe {
                DeleteProcThreadAttributeList(self.ptr());
            }
        }
    }
}
fn streams() -> io::Result<[Option<OwnedHandle>; 3]> {
    let mut owned = [None, None, None];
    for (slot, kind) in
        owned
            .iter_mut()
            .zip([STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE])
    {
        // SAFETY: documented constant; pseudo process handle remains valid in this process.
        let (source, current) = unsafe { (GetStdHandle(kind), GetCurrentProcess()) };
        if source.is_null() {
            continue;
        }
        if source == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let mut copy = std::ptr::null_mut();
        // SAFETY: duplicate live standard handle to a uniquely owned inheritable handle, with
        // identical access. Only these copies appear in HANDLE_LIST; job is never inherited.
        if unsafe {
            DuplicateHandle(
                current,
                source,
                current,
                &mut copy,
                0,
                1,
                DUPLICATE_SAME_ACCESS,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: successful DuplicateHandle transfers a new unique handle.
        *slot = Some(unsafe { OwnedHandle::from_raw_handle(copy) });
    }
    Ok(owned)
}
pub(super) fn spawn_with(
    program: &Path,
    args: &[OsString],
    extra: &[(String, String)],
) -> io::Result<Child> {
    spawn_command(program, args, extra)
}
pub(super) fn spawn(program: &Path, args: &[OsString]) -> io::Result<Child> {
    spawn_command(program, args, &[])
}
fn spawn_command(
    program: &Path,
    args: &[OsString],
    extra: &[(String, String)],
) -> io::Result<Child> {
    let environment = if extra.is_empty() {
        None
    } else {
        Some(environment(extra)?)
    };
    let exe = wide(program.as_os_str())?;
    let mut line = Vec::new();
    quote(program.as_os_str(), &mut line)?;
    for arg in args {
        line.push(b' ' as u16);
        quote(arg, &mut line)?;
    }
    line.push(0);
    // SAFETY: null security/name create unnamed non-inheritable job.
    let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if raw.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: new job uniquely transferred; closes on every failure and after child wait.
    let job = unsafe { OwnedHandle::from_raw_handle(raw) };
    // SAFETY: zero is valid initial representation; flag is set before use.
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    // A program the child starts again (app.relaunch) breaks away: it is a restart, not a leftover.
    limits.BasicLimitInformation.LimitFlags =
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_BREAKAWAY_OK;
    // SAFETY: owned job, correctly sized initialized structure for this information class.
    if unsafe {
        SetInformationJobObject(
            raw,
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            std::mem::size_of_val(&limits) as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let streams = streams()?;
    let handles: Vec<HANDLE> = streams
        .iter()
        .flatten()
        .map(AsRawHandle::as_raw_handle)
        .collect();
    let jobs = [raw];
    let mut attributes = Attributes::new(&jobs, &handles)?;
    // SAFETY: these Win32 structures permit zero initialization; required fields filled below.
    let (mut startup, mut info): (STARTUPINFOEXW, PROCESS_INFORMATION) =
        unsafe { (std::mem::zeroed(), std::mem::zeroed()) };
    startup.lpAttributeList = attributes.ptr();
    startup.StartupInfo.cb = std::mem::size_of_val(&startup) as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    let raw_stream = |index: usize| {
        streams[index]
            .as_ref()
            .map_or(std::ptr::null_mut(), AsRawHandle::as_raw_handle)
    };
    startup.StartupInfo.hStdInput = raw_stream(0);
    startup.StartupInfo.hStdOutput = raw_stream(1);
    startup.StartupInfo.hStdError = raw_stream(2);
    // SAFETY: terminated executable/command buffers, live attribute arrays/handles, valid output
    // structures. Null cwd inherits exactly; a built environment block replaces the inherited one.
    // JOB_LIST assignment is atomic with creation; any failure returns without an unguarded
    // running child. No new console/group.
    if unsafe {
        CreateProcessW(
            exe.as_ptr(),
            line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            i32::from(!handles.is_empty()),
            CREATE_UNICODE_ENVIRONMENT | EXTENDED_STARTUPINFO_PRESENT,
            environment
                .as_ref()
                .map_or(std::ptr::null(), |block| block.as_ptr().cast()),
            std::ptr::null(),
            &startup.StartupInfo,
            &mut info,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful creation returns two distinct uniquely owned handles. The primary
    // thread is already running; close its handle, retain process and job through wait.
    let (process, thread) = unsafe {
        (
            OwnedHandle::from_raw_handle(info.hProcess),
            OwnedHandle::from_raw_handle(info.hThread),
        )
    };
    drop(thread);
    Ok(Child { process, _job: job })
}
impl Child {
    pub(super) async fn wait(&mut self) -> io::Result<ExitStatus> {
        loop {
            // SAFETY: process owned through wait, zero timeout never blocks runtime workers.
            match unsafe { WaitForSingleObject(self.process.as_raw_handle(), 0) } {
                WAIT_OBJECT_0 => break,
                WAIT_TIMEOUT => tokio::time::sleep(Duration::from_millis(10)).await,
                _ => return Err(io::Error::last_os_error()),
            }
        }
        let mut code = 0;
        // SAFETY: live exited process handle and valid writable output pointer.
        if unsafe { GetExitCodeProcess(self.process.as_raw_handle(), &mut code) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(ExitStatus::from_raw(code))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_environment_block_is_nul_separated_and_double_nul_terminated_with_the_marker() {
        let block = environment(&[("ALEF_TEST_MARKER".to_owned(), "1".to_owned())]).unwrap();
        assert_eq!(&block[block.len() - 2..], [0, 0]);
        let entries: Vec<String> = block[..block.len() - 1]
            .split(|unit| *unit == 0)
            .filter(|entry| !entry.is_empty())
            .map(String::from_utf16_lossy)
            .collect();
        assert!(entries.iter().any(|entry| entry == "ALEF_TEST_MARKER=1"));
    }

    #[test]
    fn an_environment_name_with_a_separator_is_refused() {
        for name in ["A=B", "A\0B"] {
            assert!(environment(&[(name.to_owned(), "x".to_owned())]).is_err());
        }
    }
}
