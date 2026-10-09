// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native Windows child: preserve the exact primary thread and poll the owned process handle.
use super::{spawn::Pipes, tree::Killer};
use alef_core::AlefError;
use std::{
    collections::BTreeMap,
    ffi::OsStr,
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::Path,
    time::Duration,
};
use windows_sys::Win32::{
    Foundation::*,
    Storage::FileSystem::*,
    System::{Pipes::CreatePipe, Threading::*},
};

pub(crate) struct Child {
    process: OwnedHandle,
    pid: u32,
    pub stdin: Option<tokio::process::ChildStdin>,
    pub stdout: Option<tokio::process::ChildStdout>,
    pub stderr: Option<tokio::process::ChildStderr>,
    killer: Killer,
    status: Option<std::process::ExitStatus>,
}
impl Child {
    pub fn id(&self) -> Option<u32> {
        Some(self.pid)
    }
    pub async fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        use std::os::windows::process::ExitStatusExt;
        if let Some(status) = self.status {
            return Ok(status);
        }
        loop {
            // SAFETY: process remains owned; zero timeout never blocks.
            match unsafe { WaitForSingleObject(self.process.as_raw_handle(), 0) } {
                WAIT_OBJECT_0 => break,
                WAIT_TIMEOUT => tokio::time::sleep(Duration::from_millis(5)).await,
                _ => return Err(std::io::Error::last_os_error()),
            }
        }
        let mut code = 0;
        // SAFETY: owned exited process and valid output pointer.
        if unsafe { GetExitCodeProcess(self.process.as_raw_handle(), &mut code) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let status = std::process::ExitStatus::from_raw(code);
        self.status = Some(status);
        Ok(status)
    }
    pub async fn kill(&mut self) -> std::io::Result<()> {
        self.killer.kill();
        self.wait().await.map(|_| ())
    }
}
impl Drop for Child {
    fn drop(&mut self) {
        self.killer.kill();
    }
}
fn wide(s: &OsStr) -> std::io::Result<Vec<u16>> {
    let mut s: Vec<u16> = s.encode_wide().collect();
    if s.contains(&0) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "NUL in argument",
        ));
    }
    s.push(0);
    Ok(s)
}
fn quote(s: &OsStr) -> String {
    let mut out = String::from("\"");
    let mut slashes = 0;
    for c in s.to_string_lossy().chars() {
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
fn pipe(input: bool, piped: bool) -> std::io::Result<(OwnedHandle, Option<OwnedHandle>)> {
    // SAFETY: zeroed SECURITY_ATTRIBUTES is valid and size is set before use.
    let mut security: windows_sys::Win32::Security::SECURITY_ATTRIBUTES =
        unsafe { std::mem::zeroed() };
    security.nLength = std::mem::size_of_val(&security) as u32;
    security.bInheritHandle = 1;
    if !piped {
        let name = wide(OsStr::new("NUL"))?;
        // SAFETY: terminated device path, valid security pointer, synchronous open.
        let h = unsafe {
            CreateFileW(
                name.as_ptr(),
                if input { GENERIC_READ } else { GENERIC_WRITE },
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                &security,
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        if h == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: successful open transfers unique ownership.
        return Ok((unsafe { OwnedHandle::from_raw_handle(h) }, None));
    }
    let (mut r, mut w) = (std::ptr::null_mut(), std::ptr::null_mut());
    // SAFETY: valid output pointers and inheritable security attributes.
    if unsafe { CreatePipe(&mut r, &mut w, &security, 65536) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: distinct successful pipe handles transfer unique ownership.
    let (r, w) = unsafe {
        (
            OwnedHandle::from_raw_handle(r),
            OwnedHandle::from_raw_handle(w),
        )
    };
    let (child, parent) = if input { (r, w) } else { (w, r) };
    // SAFETY: owned parent endpoint; disable inheritance so child cannot hold its own EOF open.
    if unsafe { SetHandleInformation(parent.as_raw_handle(), HANDLE_FLAG_INHERIT, 0) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok((child, Some(parent)))
}
struct Attributes {
    initialized: bool,
    data: Vec<usize>,
}
impl Attributes {
    fn new(handles: &[HANDLE; 3]) -> std::io::Result<Self> {
        let mut size = 0;
        // SAFETY: size query only, valid output pointer.
        unsafe {
            InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut size);
        }
        let mut this = Self {
            initialized: false,
            data: vec![0; size.div_ceil(std::mem::size_of::<usize>())],
        };
        // SAFETY: aligned buffer covers queried byte size.
        if unsafe { InitializeProcThreadAttributeList(this.ptr(), 1, 0, &mut size) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        this.initialized = true;
        // SAFETY: initialized one-slot list; three live inheritable handles remain owned by caller.
        if unsafe {
            UpdateProcThreadAttribute(
                this.ptr(),
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                handles.as_ptr().cast(),
                std::mem::size_of_val(handles),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error());
        }
        Ok(this)
    }
    fn ptr(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.data.as_mut_ptr().cast()
    }
}
impl Drop for Attributes {
    fn drop(&mut self) {
        if self.initialized {
            // SAFETY: successfully initialized list remains allocated.
            unsafe {
                DeleteProcThreadAttributeList(self.ptr());
            }
        }
    }
}
pub(crate) fn spawn(
    program: &Path,
    args: &[String],
    cwd: &Path,
    env: &[(String, String)],
    pipes: Pipes,
) -> Result<(Child, Killer), AlefError> {
    let exe = wide(program.as_os_str())?;
    let mut line = quote(program.as_os_str());
    let is_cmd = program
        .file_name()
        .is_some_and(|s| s.eq_ignore_ascii_case("cmd.exe"));
    for arg in args {
        line.push(' ');
        if is_cmd {
            line.push_str(arg);
        } else {
            line.push_str(&quote(OsStr::new(arg)));
        }
    }
    let mut line = wide(OsStr::new(&line))?;
    let cwd = wide(cwd.as_os_str())?;
    let mut pairs: BTreeMap<String, (std::ffi::OsString, std::ffi::OsString)> = std::env::vars_os()
        .map(|(k, v)| (k.to_string_lossy().to_uppercase(), (k, v)))
        .collect();
    for (k, v) in env {
        pairs.insert(k.to_uppercase(), (k.into(), v.into()));
    }
    let mut environment = Vec::new();
    for (_, (k, v)) in pairs {
        let mut pair = k;
        pair.push("=");
        pair.push(v);
        environment.extend(wide(&pair)?);
    }
    environment.push(0);
    let (input, stdin) = pipe(true, pipes.stdin)?;
    let (output, stdout) = pipe(false, pipes.stdout)?;
    let (error, stderr) = pipe(false, pipes.stderr)?;
    let handles = [
        input.as_raw_handle(),
        output.as_raw_handle(),
        error.as_raw_handle(),
    ];
    let mut attributes = Attributes::new(&handles)?;
    // SAFETY: zeroed startup/info are valid initial representations.
    let (mut extended, mut info): (STARTUPINFOEXW, PROCESS_INFORMATION) =
        unsafe { (std::mem::zeroed(), std::mem::zeroed()) };
    extended.lpAttributeList = attributes.ptr();
    let startup = &mut extended.StartupInfo;
    startup.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    startup.dwFlags = STARTF_USESTDHANDLES;
    startup.hStdInput = input.as_raw_handle();
    startup.hStdOutput = output.as_raw_handle();
    startup.hStdError = error.as_raw_handle();
    // SAFETY: terminated buffers and live stdio handles; primary thread starts suspended.
    if unsafe {
        CreateProcessW(
            exe.as_ptr(),
            line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
            CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT | EXTENDED_STARTUPINFO_PRESENT,
            environment.as_ptr().cast(),
            cwd.as_ptr(),
            startup,
            &mut info,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: successful creation transfers two distinct handles.
    let (process, thread) = unsafe {
        (
            OwnedHandle::from_raw_handle(info.hProcess),
            OwnedHandle::from_raw_handle(info.hThread),
        )
    };
    let killer = match Killer::adopt_handle(process.as_raw_handle()) {
        Ok(k) => k,
        Err(e) => {
            // SAFETY: actual still-suspended owned process.
            unsafe {
                TerminateProcess(process.as_raw_handle(), 1);
            }
            return Err(e);
        }
    };
    let mut child = Child {
        process,
        pid: info.dwProcessId,
        stdin: None,
        stdout: None,
        stderr: None,
        killer: killer.clone(),
        status: None,
    };
    child.stdin = stdin
        .map(|h| tokio::process::ChildStdin::from_std(h.into()))
        .transpose()?;
    child.stdout = stdout
        .map(|h| tokio::process::ChildStdout::from_std(h.into()))
        .transpose()?;
    child.stderr = stderr
        .map(|h| tokio::process::ChildStderr::from_std(h.into()))
        .transpose()?;
    // SAFETY: exact primary thread from this CreateProcessW, job assignment completed.
    if unsafe { ResumeThread(thread.as_raw_handle()) } == u32::MAX {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok((child, killer))
}
