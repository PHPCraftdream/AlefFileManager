// SPDX-License-Identifier: MIT OR Apache-2.0
//! Unix: a private process group and forwarding of INT/TERM/HUP. Linux PDEATHSIG protects the
//! direct child (not arbitrary descendants that escape the group). macOS uses a forked watchdog
//! with EVFILT_PROC/NOTE_EXIT registrations for both parent and child. Registration happens
//! before exec, with a getppid race check; kernel process watches do not follow reused pids.
//! The watchdog remains in the group, pinning its identity, and kills it on either exit.
//! No inherited writer can leak into concurrent spawns. The watchdog needs to be scheduled:
//! externally killing/stopping it (or stopping the whole group) defeats this mechanism.
//! The watchdog does not return into the inherited Rust runtime; its Darwin kernel calls use
//! stack data only. This path still requires native macOS validation (not a POSIX portability claim).
use std::{
    ffi::OsString,
    io,
    os::unix::process::CommandExt,
    path::Path,
    process::{Child as StdChild, Command, ExitStatus},
    time::Duration,
};
use tokio::signal::unix::{signal, Signal, SignalKind};

pub(super) struct Child {
    child: StdChild,
    signals: [Signal; 3],
    done: bool,
}

fn spawn_command(
    program: &Path,
    args: &[OsString],
    extra: &[(String, String)],
) -> io::Result<Child> {
    let signals = [
        signal(SignalKind::interrupt())?,
        signal(SignalKind::terminate())?,
        signal(SignalKind::hangup())?,
    ];
    let mut command = Command::new(program);
    command.args(args).process_group(0);
    command.envs(
        extra
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str())),
    );
    #[cfg(target_os = "linux")]
    {
        // Linux binds PDEATHSIG to the creating thread, not the entire thread group. The Tokio
        // worker that spawns stays alive while window() is awaited; worker exit also kills child.
        // SAFETY: getpid has no pointer arguments. Capture before fork for the race check.
        let parent = unsafe { libc::getpid() };
        // SAFETY: pre_exec performs only async-signal-safe system calls, no locks/allocations.
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::getppid() != parent {
                    libc::_exit(137);
                }
                Ok(())
            });
        }
    }
    #[cfg(target_os = "macos")]
    {
        // SAFETY: capture launcher identity before fork; no pointers involved.
        let parent = unsafe { libc::getpid() };
        // SAFETY: macOS pre-exec/watchdog uses only raw syscall wrappers and stack data,
        // no Rust allocation, locks or destructors after fork. kqueue/kevent are Darwin
        // kernel calls; the watchdog does not return into the inherited Rust runtime.
        unsafe {
            command.pre_exec(move || {
                let group = libc::getpid();
                let mut ready = [0; 2];
                if libc::pipe(ready.as_mut_ptr()) < 0 {
                    return Err(io::Error::last_os_error());
                }
                let watcher = libc::fork();
                if watcher < 0 {
                    let error = io::Error::last_os_error();
                    libc::close(ready[0]);
                    libc::close(ready[1]);
                    return Err(error);
                }
                if watcher == 0 {
                    libc::close(ready[0]);
                    // kqueues are NOT inherited across fork; create/register in the watchdog.
                    let queue = libc::kqueue();
                    if queue < 0 {
                        libc::kill(-group, libc::SIGKILL);
                        libc::_exit(1);
                    }
                    let mut events: [libc::kevent; 2] = std::mem::zeroed();
                    for (event, pid) in events.iter_mut().zip([parent, group]) {
                        event.ident = pid as libc::uintptr_t;
                        event.filter = libc::EVFILT_PROC;
                        event.flags = libc::EV_ADD | libc::EV_ENABLE;
                        event.fflags = libc::NOTE_EXIT;
                    }
                    if libc::kevent(
                        queue,
                        events.as_ptr(),
                        2,
                        std::ptr::null_mut(),
                        0,
                        std::ptr::null(),
                    ) < 0
                    {
                        libc::kill(-group, libc::SIGKILL);
                        libc::_exit(1);
                    }
                    // Keep group identity alive; graceful forwarding must not kill the watchdog.
                    let mut ignored: libc::sigaction = std::mem::zeroed();
                    ignored.sa_sigaction = libc::SIG_IGN;
                    libc::sigemptyset(&mut ignored.sa_mask);
                    for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
                        if libc::sigaction(sig, &ignored, std::ptr::null_mut()) != 0 {
                            libc::kill(-group, libc::SIGKILL);
                            libc::_exit(1);
                        }
                    }
                    if libc::write(ready[1], [1u8].as_ptr().cast(), 1) != 1 {
                        libc::kill(-group, libc::SIGKILL);
                        libc::_exit(1);
                    }
                    libc::close(ready[1]);
                    if libc::dup2(queue, 0) < 0 {
                        libc::kill(-group, libc::SIGKILL);
                        libc::_exit(1);
                    }
                    libc::close(1);
                    libc::close(2);
                    let limit = libc::getdtablesize();
                    for fd in 3..limit {
                        libc::close(fd);
                    }
                    let mut event: libc::kevent = std::mem::zeroed();
                    loop {
                        let count =
                            libc::kevent(0, std::ptr::null(), 0, &mut event, 1, std::ptr::null());
                        if count > 0 {
                            libc::kill(-group, libc::SIGKILL);
                            libc::_exit(0);
                        }
                        if count < 0 && *libc::__error() != libc::EINTR {
                            libc::kill(-group, libc::SIGKILL);
                            libc::_exit(1);
                        }
                    }
                }
                libc::close(ready[1]);
                let mut byte = 0u8;
                loop {
                    let count = libc::read(ready[0], (&mut byte as *mut u8).cast(), 1);
                    if count == 1 {
                        break;
                    }
                    if count == 0 || *libc::__error() != libc::EINTR {
                        libc::_exit(137);
                    }
                }
                libc::close(ready[0]);
                if libc::getppid() != parent {
                    libc::_exit(137);
                }
                Ok(())
            });
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    return Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "parent-death protection is unavailable on this Unix",
    ));
    let child = command.spawn()?;
    Ok(Child {
        child,
        signals,
        done: false,
    })
}

pub(super) fn spawn(program: &Path, args: &[OsString]) -> io::Result<Child> {
    spawn_command(program, args, &[])
}

impl Child {
    fn send(&self, sig: i32) {
        // SAFETY: positive id is the unreaped child/private group. No signal after wait succeeds.
        unsafe {
            libc::kill(-(self.child.id() as i32), sig);
        }
    }
    pub(super) async fn wait(&mut self) -> io::Result<ExitStatus> {
        loop {
            if let Some(status) = self.child.try_wait()? {
                self.done = true;
                return Ok(status);
            }
            let [interrupt, terminate, hangup] = &mut self.signals;
            let sig = tokio::select! {
                _ = interrupt.recv() => Some(libc::SIGINT),
                _ = terminate.recv() => Some(libc::SIGTERM),
                _ = hangup.recv() => Some(libc::SIGHUP),
                _ = tokio::time::sleep(Duration::from_millis(10)) => None,
            };
            if let Some(sig) = sig {
                self.send(sig);
            }
        }
    }
}
impl Drop for Child {
    fn drop(&mut self) {
        if !self.done {
            self.send(libc::SIGKILL);
            let _ = self.child.wait();
        }
    }
}

/// The runtime relaunch child: the stored binary with an inherited environment plus the
/// relaunch marker, standard handles inherited.
pub(super) fn spawn_with(
    program: &Path,
    args: &[OsString],
    extra: &[(String, String)],
) -> io::Result<Child> {
    spawn_command(program, args, extra)
}
