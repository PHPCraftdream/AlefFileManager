// SPDX-License-Identifier: MIT OR Apache-2.0
//! Two things every process needs: a handle that kills the whole tree of it, and the search along
//! `PATH` that turns a bare name into a program. The handle is the safety net: whatever a child
//! starts goes with it, even when a call is cancelled in mid-flight.
use std::{
    ffi::OsStr,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

use alef_core::AlefError;
#[cfg(any(unix, test))]
use tokio::process::Child;

/// An owning handle that kills a whole process tree, and disarms itself once the direct child has
/// been waited for, so a recycled process id is never signalled. Cloning shares one handle.
#[derive(Clone)]
pub(crate) struct Killer(Arc<Inner>);

struct Inner {
    handle: Mutex<Option<Handle>>,
    disarmed: AtomicBool,
}

enum Handle {
    /// A Windows job object; closing it kills the tree that was put into it.
    #[cfg(windows)]
    Job(std::os::windows::io::RawHandle),
    /// A Unix process group; the child was spawned into its own.
    #[cfg(unix)]
    Group(i32),
}

// SAFETY: Windows job handles have no thread affinity. All access/close is serialized by
// Inner's mutex, and the unique owning Inner closes each handle exactly once.
unsafe impl Send for Handle {}
// SAFETY: shared access never dereferences the opaque handle; it uses thread-safe OS APIs
// under the same mutex, excluding concurrent close.
unsafe impl Sync for Handle {}

impl Killer {
    /// Takes a newly created child under the tree-killer. Windows callers must keep its
    /// primary thread suspended until assignment completes; Unix callers set process_group(0).
    #[cfg(any(unix, test))]
    pub(crate) fn adopt(child: &mut Child) -> Result<Self, AlefError> {
        #[cfg(windows)]
        {
            Self::adopt_handle(child.raw_handle().ok_or_else(|| {
                AlefError::new(
                    alef_core::ErrorCode::Internal,
                    "child has no process handle",
                )
            })?)
        }
        #[cfg(unix)]
        {
            Self::adopt_group(child.id().ok_or_else(|| {
                AlefError::new(alef_core::ErrorCode::Internal, "child has no process id")
            })?)
        }
    }

    #[cfg(windows)]
    pub(crate) fn adopt_handle(
        process: std::os::windows::io::RawHandle,
    ) -> Result<Self, AlefError> {
        let mut inner = Inner {
            handle: Mutex::new(None),
            disarmed: AtomicBool::new(false),
        };
        #[cfg(windows)]
        {
            use windows_sys::Win32::{
                Foundation::{CloseHandle, HANDLE},
                System::JobObjects::{
                    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
                    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
                    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                },
            };
            // SAFETY: null security/name pointers request default security and an unnamed job.
            // The zeroed limits structure is valid, and its size/layout matches the information
            // class. child owns a live process handle throughout assignment. job is closed on
            // failure or transferred into the unique owning Inner on success.
            unsafe {
                let job: HANDLE = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if job.is_null() {
                    return Err(AlefError::new(
                        alef_core::ErrorCode::Internal,
                        "cannot create a job object",
                    ));
                }
                let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let ok = SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    &limits as *const _ as *const core::ffi::c_void,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                ) != 0
                    && AssignProcessToJobObject(job, process as HANDLE) != 0;
                if !ok {
                    CloseHandle(job);
                    return Err(AlefError::new(
                        alef_core::ErrorCode::Internal,
                        "cannot put the process under a job object",
                    ));
                }
                inner.handle = Mutex::new(Some(Handle::Job(job)));
            }
        }
        Ok(Self(Arc::new(inner)))
    }

    #[cfg(unix)]
    pub(crate) fn adopt_group(pid: u32) -> Result<Self, AlefError> {
        let pid = i32::try_from(pid)
            .ok()
            .filter(|pid| *pid > 0)
            .ok_or_else(|| {
                AlefError::new(alef_core::ErrorCode::Internal, "invalid process group")
            })?;
        Ok(Self(Arc::new(Inner {
            handle: Mutex::new(Some(Handle::Group(pid))),
            disarmed: AtomicBool::new(false),
        })))
    }

    fn armed(&self) -> bool {
        !self.0.disarmed.load(Ordering::SeqCst)
    }

    /// Kills the whole tree the way `SIGKILL` does, unless it was disarmed.
    pub(crate) fn kill(&self) {
        let handle = self.0.handle.lock().unwrap_or_else(|e| e.into_inner());
        // Check while holding the same lock as disarm: a pre-lock check can become stale.
        if !self.armed() {
            return;
        }
        match handle.as_ref() {
            #[cfg(windows)]
            // SAFETY: the mutex protects the owned live job from concurrent close.
            Some(Handle::Job(job)) => unsafe {
                use windows_sys::Win32::System::JobObjects::TerminateJobObject;
                TerminateJobObject(*job, 1);
            },
            #[cfg(unix)]
            // SAFETY: pid is a positive process-group id created for this child. The mutex
            // serializes signalling and disarm; no Rust memory is accessed by kill.
            Some(Handle::Group(pid)) => unsafe {
                libc::kill(-*pid, libc::SIGKILL);
            },
            None => {}
        }
    }

    /// Signals the whole tree with the given signal (Unix only).
    #[cfg(unix)]
    pub(crate) fn signal(&self, sig: i32) {
        let handle = self.0.handle.lock().unwrap_or_else(|e| e.into_inner());
        // Check while holding the same lock as disarm: a pre-lock check can become stale.
        if !self.armed() {
            return;
        }
        if let Some(Handle::Group(pid)) = handle.as_ref() {
            // SAFETY: the locked positive group id belongs to this child's process group.
            unsafe {
                libc::kill(-*pid, sig);
            }
        }
    }

    /// Says the direct child is done: from now on nothing is signalled, for a recycled group of
    /// the same number is no longer ours.
    pub(crate) fn disarm(&self) {
        // Descendants may outlive the direct child; terminate them before releasing ownership.
        #[cfg(unix)]
        let mut handle = self.0.handle.lock().unwrap_or_else(|e| e.into_inner());
        #[cfg(windows)]
        let handle = self.0.handle.lock().unwrap_or_else(|e| e.into_inner());
        #[cfg(unix)]
        if let Some(Handle::Group(pid)) = handle.take() {
            // SAFETY: the owned group id is positive and signaling is serialized with kill.
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
        }
        #[cfg(windows)]
        if let Some(Handle::Job(job)) = handle.as_ref() {
            // SAFETY: the owned job is live under the mutex and TerminateJobObject is thread-safe.
            unsafe {
                windows_sys::Win32::System::JobObjects::TerminateJobObject(*job, 1);
            }
        }
        self.0.disarmed.store(true, Ordering::SeqCst);
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        if !self.disarmed.load(Ordering::SeqCst) {
            // The safety net: a tree nobody waited for dies with the handle.
            match self
                .handle
                .get_mut()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
            {
                #[cfg(windows)]
                // SAFETY: the mutex protects the owned live job from concurrent close.
                Some(Handle::Job(job)) => unsafe {
                    use windows_sys::Win32::System::JobObjects::TerminateJobObject;
                    TerminateJobObject(*job, 1);
                },
                #[cfg(unix)]
                // SAFETY: pid is a positive process-group id created for this child. The mutex
                // serializes signalling and disarm; no Rust memory is accessed by kill.
                Some(Handle::Group(pid)) => unsafe {
                    libc::kill(-*pid, libc::SIGKILL);
                },
                None => {}
            }
        }
        #[cfg(windows)]
        // SAFETY: exclusive access to Inner on drop; each owned job is closed exactly once.
        unsafe {
            use windows_sys::Win32::Foundation::CloseHandle;
            if let Some(Handle::Job(job)) = self
                .handle
                .get_mut()
                .unwrap_or_else(|e| e.into_inner())
                .take()
            {
                CloseHandle(job);
            }
        }
    }
}

/// Whether a text opens with an absolute form: a root, a share, or a drive with a separator.
fn absolute(text: &str) -> bool {
    text.starts_with('/')
        || (cfg!(windows)
            && (text.starts_with('\\')
                || (text.len() >= 3
                    && text.as_bytes()[0].is_ascii_alphabetic()
                    && text.as_bytes()[1] == b':'
                    && matches!(text.as_bytes()[2], b'/' | b'\\'))))
}

/// The separators that split a path on this platform.
const SEPARATORS: &[char] = if cfg!(windows) { &['/', '\\'] } else { &['/'] };

/// Turns a program text into a program: an absolute path is used as it is, a text with a separator
/// is not a program at all, and a bare name is looked up along `path_var` — with the extensions of
/// `pathext` tried after the plain name, on Windows only.
pub(in crate::system) fn resolve(
    program: &str,
    path_var: Option<&OsStr>,
    pathext: Option<&OsStr>,
) -> Option<PathBuf> {
    if absolute(program) {
        return Some(PathBuf::from(program));
    }
    if program.contains(SEPARATORS) {
        return None;
    }
    let path_var = path_var?;
    let runtime_cwd = std::env::current_dir().ok()?;
    for dir in std::env::split_paths(&path_var) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let dir = if dir.is_absolute() {
            dir
        } else {
            runtime_cwd.join(dir)
        };
        // The extensions come first: next to `npm.cmd` lies `npm`, a script no Windows can start.
        if cfg!(windows) {
            if let Some(pathext) = pathext {
                for ext in pathext
                    .to_string_lossy()
                    .split(';')
                    .filter(|e| !e.is_empty())
                {
                    let candidate = dir.join(format!("{program}{ext}"));
                    if candidate.is_file() {
                        return Some(candidate);
                    }
                }
            }
        }
        let candidate = dir.join(program);
        if executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn executable(path: &std::path::Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(windows)]
    {
        true
    }
}

/// Spawn suspended on Windows: the primary thread cannot execute any user code until the job
/// assignment succeeds. Tokio/std expose creation_flags but not the primary thread handle, so
/// ToolHelp locates that sole suspended thread. Adoption/resume failures always kill and reap.
#[cfg(any(unix, test))]
pub(crate) async fn spawn(
    command: &mut tokio::process::Command,
) -> Result<(Child, Killer), AlefError> {
    command.kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    #[cfg(windows)]
    command.creation_flags(windows_sys::Win32::System::Threading::CREATE_SUSPENDED);
    let mut child = command.spawn()?;
    let adopted = Killer::adopt(&mut child);
    let killer = match adopted {
        Ok(killer) => killer,
        Err(error) => {
            let _ = child.kill().await;
            return Err(error);
        }
    };
    #[cfg(windows)]
    if let Err(error) = resume(&child) {
        killer.kill();
        let _ = child.kill().await;
        return Err(error);
    }
    Ok((child, killer))
}

#[cfg(all(windows, test))]
fn resume(child: &Child) -> Result<(), AlefError> {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, INVALID_HANDLE_VALUE},
        System::{
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD,
                THREADENTRY32,
            },
            Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME},
        },
    };
    let pid = child
        .id()
        .ok_or_else(|| AlefError::new(alef_core::ErrorCode::Internal, "suspended child exited"))?;
    // SAFETY: snapshot is uniquely owned and closed on all paths. THREADENTRY32 is initialized
    // with its required size; iteration writes only that structure. The child handle keeps the
    // suspended process alive, so its threads cannot be reused before OpenThread. Each thread
    // handle is used only for ResumeThread and closed once. No child code runs before assignment.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error().into());
        }
        let mut entry: THREADENTRY32 = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
        let mut found = Thread32First(snapshot, &mut entry) != 0;
        let mut result = Err(AlefError::new(
            alef_core::ErrorCode::Internal,
            "suspended thread not found",
        ));
        // Every thread of the child, not the first one: another program (an antivirus) may have
        // put a thread of its own into the suspended process, and the primary one must not stay asleep.
        while found {
            if entry.th32OwnerProcessID == pid {
                let thread = OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID);
                if thread.is_null() {
                    if result.is_err() {
                        result = Err(std::io::Error::last_os_error().into());
                    }
                } else {
                    match ResumeThread(thread) {
                        u32::MAX if result.is_err() => {
                            result = Err(std::io::Error::last_os_error().into());
                        }
                        u32::MAX | 0 => {}
                        _ => result = Ok(()),
                    }
                    CloseHandle(thread);
                }
            }
            found = Thread32Next(snapshot, &mut entry) != 0;
        }
        CloseHandle(snapshot);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn path_of(dir: &Path) -> Option<&OsStr> {
        Some(dir.as_os_str())
    }

    #[test]
    fn an_absolute_path_is_taken_as_it_is() {
        let resolved = resolve(
            if cfg!(windows) {
                "C:/x/prog.exe"
            } else {
                "/x/prog"
            },
            Some(OsStr::new("nowhere")),
            None,
        );
        assert_eq!(
            resolved,
            Some(PathBuf::from(if cfg!(windows) {
                "C:/x/prog.exe"
            } else {
                "/x/prog"
            }))
        );
    }

    #[test]
    fn a_relative_path_with_a_separator_is_no_program() {
        let root = tempfile::tempdir().unwrap();
        let name = if cfg!(windows) { "prog.exe" } else { "prog" };
        std::fs::create_dir(root.path().join("dir")).unwrap();
        std::fs::write(root.path().join("dir").join(name), b"").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let file = root.path().join("dir").join(name);
            std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        // Even where the search path would find it.
        let path = path_of(root.path());
        assert_eq!(resolve(&format!("dir/{name}"), path, None), None);
        assert_eq!(resolve(&format!("./dir/{name}"), path, None), None);
    }

    #[test]
    fn a_bare_name_is_found_along_the_search_path_or_not_at_all() {
        let dir = tempfile::tempdir().unwrap();
        let name = if cfg!(windows) { "prog.exe" } else { "prog" };
        std::fs::write(dir.path().join(name), b"").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                dir.path().join(name),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
        assert_eq!(
            resolve(name, path_of(dir.path()), None),
            Some(dir.path().join(name))
        );
        assert_eq!(resolve("absent", path_of(dir.path()), None), None);
        assert_eq!(resolve("prog", Some(OsStr::new("")), None), None);
    }

    #[tokio::test]
    async fn relative_runtime_path_stays_absolute_when_child_cwd_differs() {
        let runtime = std::env::current_dir().unwrap();
        let dir = tempfile::tempdir_in(&runtime).unwrap();
        let other = tempfile::tempdir_in(&runtime).unwrap();
        let name = if cfg!(windows) { "node.exe" } else { "node" };
        let source = resolve(name, std::env::var_os("PATH").as_deref(), None).unwrap();
        std::fs::copy(&source, dir.path().join(name)).unwrap();
        let relative = dir.path().strip_prefix(&runtime).unwrap();
        let found = resolve(name, Some(relative.as_os_str()), None).unwrap();
        assert!(found.is_absolute());
        assert_eq!(found, dir.path().join(name));
        let mut command = tokio::process::Command::new(found);
        command
            .arg("-v")
            .current_dir(other.path())
            .stdout(std::process::Stdio::piped());
        let (child, killer) = spawn(&mut command).await.unwrap();
        let output = child.wait_with_output().await.unwrap();
        killer.disarm();
        assert!(output.status.success());
        assert!(output.stdout.starts_with(b"v"));
    }

    #[test]
    fn an_empty_entry_of_the_path_is_not_the_working_folder() {
        let name = format!("alef-probe-{}.exe", std::process::id());
        let file = std::env::current_dir().unwrap().join(&name);
        std::fs::write(&file, b"").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let separator = if cfg!(windows) { ";" } else { ":" };
        let path = format!("{separator}{separator}");
        let found = resolve(&name, Some(OsStr::new(&path)), None);
        std::fs::remove_file(&file).unwrap();
        assert_eq!(found, None);
    }

    #[cfg(unix)]
    #[test]
    fn non_executable_path_entry_is_skipped() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("tool");
        std::fs::write(&file, b"not executable").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(resolve("tool", path_of(dir.path()), None), None);
    }

    #[cfg(windows)]
    #[test]
    fn pathext_extensions_are_tried_after_the_plain_name_case_insensitively() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("tool.bat"), b"").unwrap();
        std::fs::write(dir.path().join("plain"), b"").unwrap();
        let pathext = OsStr::new(".COM;.BAT");
        let found = resolve("tool", path_of(dir.path()), Some(pathext)).unwrap();
        // The disk answers with its own spelling of the name: same file, any case.
        assert_eq!(found.parent(), Some(dir.path()));
        assert_eq!(
            found.file_name().unwrap().to_string_lossy().to_lowercase(),
            "tool.bat"
        );
        assert_eq!(
            resolve("plain", path_of(dir.path()), Some(pathext)),
            Some(dir.path().join("plain"))
        );
        assert_eq!(resolve("missing", path_of(dir.path()), Some(pathext)), None);
    }

    #[cfg(windows)]
    #[test]
    fn a_script_without_an_extension_does_not_hide_the_one_with_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("npm"), b"#!/bin/sh").unwrap();
        std::fs::write(dir.path().join("npm.cmd"), b"").unwrap();
        let found = resolve("npm", path_of(dir.path()), Some(OsStr::new(".EXE;.CMD"))).unwrap();
        assert_eq!(
            found.file_name().unwrap().to_string_lossy().to_lowercase(),
            "npm.cmd"
        );
    }
}
