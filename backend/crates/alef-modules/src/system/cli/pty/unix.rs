// SPDX-License-Identifier: MIT OR Apache-2.0
//! portable-pty calls setsid before exec, so the owned group id is the child's pid.
//! As with spawn, descendants that deliberately create another session escape Unix group killing.
use super::super::tree::Killer;
use alef_core::{AlefError, ErrorCode};
use bytes::Bytes;
use portable_pty::{CommandBuilder, MasterPty, PtySize};
use std::{
    io::{Read, Write},
    os::fd::RawFd,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    time::Duration,
};

pub(super) struct Terminal {
    master: Mutex<Box<dyn MasterPty + Send>>,
    reader: Mutex<Box<dyn Read + Send>>,
    writer: Mutex<Option<Box<dyn Write + Send>>>,
    done: AtomicBool,
}
pub(super) struct Child {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    killer: Killer,
}
fn error(error: impl std::fmt::Display) -> AlefError {
    AlefError::new(ErrorCode::Internal, error.to_string())
}
fn size(cols: u16, rows: u16) -> PtySize {
    PtySize {
        cols,
        rows,
        pixel_width: 0,
        pixel_height: 0,
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
    let pair = portable_pty::native_pty_system()
        .openpty(size(cols, rows))
        .map_err(error)?;
    let mut command = CommandBuilder::new(program);
    command.args(args);
    command.cwd(cwd);
    for (k, v) in env {
        command.env(k, v);
    }
    let child = pair.slave.spawn_command(command).map_err(error)?;
    drop(pair.slave);
    let pid = child
        .process_id()
        .ok_or_else(|| error("terminal has no pid"))?;
    let killer = Killer::adopt_group(pid)?;
    let mut child = Child {
        child,
        killer: killer.clone(),
    };
    let fd: RawFd = pair
        .master
        .as_raw_fd()
        .ok_or_else(|| error("terminal has no descriptor"))?;
    // SAFETY: fd is live and owned by master; F_GETFL/F_SETFL do not alter ownership.
    // O_NONBLOCK applies to the shared open description used by cloned reader and writer.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    // SAFETY: valid descriptor and flags from F_GETFL; no pointer arguments are involved.
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        let error = std::io::Error::last_os_error();
        killer.kill();
        let _ = child.child.wait();
        return Err(error.into());
    }
    let reader = pair.master.try_clone_reader().map_err(error)?;
    let writer = pair.master.take_writer().map_err(error)?;
    Ok((
        Terminal {
            master: Mutex::new(pair.master),
            reader: Mutex::new(reader),
            writer: Mutex::new(Some(writer)),
            done: AtomicBool::new(false),
        },
        child,
        killer,
        pid,
    ))
}
impl Terminal {
    pub(super) fn resize(&self, cols: u16, rows: u16) -> Result<(), AlefError> {
        self.master
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .resize(size(cols, rows))
            .map_err(error)
    }
    pub(super) fn read(&self, buffer: &mut [u8]) -> Result<Option<usize>, AlefError> {
        match self
            .reader
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .read(buffer)
        {
            Ok(n) => Ok(Some(n)),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                Ok(if self.done.load(Ordering::Acquire) {
                    Some(0)
                } else {
                    None
                })
            }
            Err(e) if e.raw_os_error() == Some(libc::EIO) => Ok(Some(0)),
            Err(e) => Err(e.into()),
        }
    }
    pub(super) async fn write(&self, bytes: Bytes) -> Result<(), AlefError> {
        let mut offset = 0;
        while offset < bytes.len() {
            let count = {
                let mut guard = self.writer.lock().unwrap_or_else(|e| e.into_inner());
                let writer = guard
                    .as_mut()
                    .ok_or_else(|| error("terminal input closed"))?;
                match writer.write(&bytes[offset..]) {
                    Ok(n) => n,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => 0,
                    Err(e) => return Err(e.into()),
                }
            };
            offset += count;
            if count == 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }
        Ok(())
    }
    pub(super) fn end_input(&self) {
        self.writer.lock().unwrap_or_else(|e| e.into_inner()).take();
    }
    pub(super) async fn finished(&self) {
        self.done.store(true, Ordering::Release);
    }
    pub(super) fn abandon_output(&self) {}
    pub(super) fn stop(&self) {
        self.end_input();
    }
}
impl Child {
    pub(super) async fn wait(mut self) -> Result<std::process::ExitStatus, AlefError> {
        let child = self
            .child
            .downcast_mut::<std::process::Child>()
            .ok_or_else(|| error("portable-pty did not return a native Unix child"))?;
        loop {
            if let Some(status) = child.try_wait()? {
                self.killer.disarm();
                return Ok(status);
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
}
impl Drop for Child {
    fn drop(&mut self) {
        self.killer.kill();
        let _ = self.child.wait();
    }
}
