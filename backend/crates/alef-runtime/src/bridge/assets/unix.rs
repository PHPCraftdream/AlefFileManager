// SPDX-License-Identifier: MIT OR Apache-2.0
//! POSIX handle-relative opens. Constants/layouts checked against libc's Linux/macOS headers.
use std::{
    ffi::{c_char, c_int, c_void, CStr, CString},
    fs::{File, OpenOptions},
    io,
    os::unix::{
        fs::OpenOptionsExt,
        io::{AsRawFd, FromRawFd, IntoRawFd},
    },
    path::Path,
};
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const FLAGS: c_int = 0x20000 | 0x80000 | 0x800; // NOFOLLOW | CLOEXEC | NONBLOCK
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const DIRECTORY: c_int = 0x10000;
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
const FLAGS: c_int = 0x8000 | 0x80000 | 0x800;
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
const DIRECTORY: c_int = 0x4000;
#[cfg(target_os = "macos")]
const FLAGS: c_int = 0x100 | 0x01000000 | 4;
#[cfg(target_os = "macos")]
const DIRECTORY: c_int = 0x00100000;
#[cfg(not(all(
    any(target_os = "linux", target_os = "macos"),
    any(target_arch = "x86_64", target_arch = "aarch64")
)))]
compile_error!("asset handle traversal supports 64-bit Linux/macOS x86_64 and aarch64 targets");
#[repr(C)]
struct Dirent {
    ino: u64,
    offset: i64,
    reclen: u16,
    #[cfg(target_os = "macos")]
    namlen: u16,
    kind: u8,
    #[cfg(target_os = "linux")]
    name: [c_char; 256],
    #[cfg(target_os = "macos")]
    name: [c_char; 1024],
}
unsafe extern "C" {
    fn openat(fd: c_int, path: *const c_char, flags: c_int, ...) -> c_int;
    fn fdopendir(fd: c_int) -> *mut c_void;
    fn readdir(dir: *mut c_void) -> *mut Dirent;
    #[cfg(target_os = "macos")]
    fn fcntl(fd: c_int, command: c_int, ...) -> c_int;
    fn closedir(dir: *mut c_void) -> c_int;
    #[cfg(target_os = "linux")]
    fn __errno_location() -> *mut c_int;
    #[cfg(target_os = "macos")]
    fn __error() -> *mut c_int;
}
fn verify(file: File) -> io::Result<File> {
    let meta = file.metadata()?;
    if !meta.is_dir() && !meta.is_file() {
        return Err(super::denied());
    }
    Ok(file)
}
pub(super) fn root(path: &Path) -> io::Result<File> {
    verify(
        OpenOptions::new()
            .read(true)
            .custom_flags(FLAGS | DIRECTORY)
            .open(path)?,
    )
}
fn open(directory: &File, name: &str, flags: c_int) -> io::Result<File> {
    let name = CString::new(name).map_err(|_| super::invalid())?;
    // SAFETY: live borrowed fd, NUL-terminated single component; no O_CREAT, so no mode argument.
    // NOFOLLOW rejects symlinks; NONBLOCK prevents a swapped FIFO from blocking before fstat.
    let fd = unsafe { openat(directory.as_raw_fd(), name.as_ptr(), FLAGS | flags) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: openat returned a fresh owned fd; File closes it, including on verification errors.
    let file = verify(unsafe { File::from_raw_fd(fd) })?;
    #[cfg(target_os = "macos")]
    if name.as_bytes() != b"." {
        let mut path = [0u8; 1024];
        // SAFETY: live fd and writable PATH_MAX buffer; fcntl F_GETPATH (Darwin 50)
        // queries this already-open object, never reopens it. No callbacks/thread affinity.
        if unsafe { fcntl(file.as_raw_fd(), 50, path.as_mut_ptr()) } < 0 {
            return Err(io::Error::last_os_error());
        }
        let end = path
            .iter()
            .position(|b| *b == 0)
            .ok_or_else(super::invalid)?;
        if path[..end].rsplit(|b| *b == b'/').next() != Some(name.as_bytes()) {
            return Err(super::denied());
        }
    }
    Ok(file)
}
pub(super) fn child(directory: &File, name: &str, is_directory: bool) -> io::Result<File> {
    let file = open(directory, name, if is_directory { DIRECTORY } else { 0 })?;
    if file.metadata()?.is_dir() != is_directory {
        return Err(super::denied());
    }
    Ok(file)
}
pub(super) fn any_child(directory: &File, name: &str) -> io::Result<File> {
    open(directory, name, 0)
}
// Non-generic uniquely owned DIR, !Send/!Sync by raw-pointer auto traits. No borrowed
// lifetime or element variance is exposed; Drop closes on the same thread as enumeration.
struct Scan(*mut c_void);
impl Drop for Scan {
    fn drop(&mut self) {
        // SAFETY: uniquely owned stream returned by fdopendir, closed exactly once.
        unsafe {
            closedir(self.0);
        }
    }
}
pub(super) fn names(directory: &File) -> io::Result<Vec<String>> {
    let fd = child(directory, ".", true)?.into_raw_fd();
    // SAFETY: fd is a fresh directory description. Success transfers ownership to DIR.
    let stream = unsafe { fdopendir(fd) };
    if stream.is_null() {
        let error = io::Error::last_os_error();
        // SAFETY: failed fdopendir leaves fd owned by caller.
        drop(unsafe { File::from_raw_fd(fd) });
        return Err(error);
    }
    // POSIX permits short variable-length dirent records; read only the fixed name offset,
    // never construct a reference to the full Rust Dirent allocation.
    let scan = Scan(stream);
    let mut names = Vec::new();
    loop {
        // SAFETY: errno pointer is thread-local; clear it to distinguish EOF from failure.
        #[cfg(target_os = "linux")]
        let errno = unsafe { __errno_location() };
        #[cfg(target_os = "macos")]
        let errno = unsafe { __error() };
        // SAFETY: live unique DIR, no other consumer; returned entry valid until next readdir.
        let entry = unsafe {
            *errno = 0;
            readdir(scan.0)
        };
        if entry.is_null() {
            // SAFETY: same thread-local errno pointer.
            if unsafe { *errno } != 0 {
                return Err(io::Error::last_os_error());
            }
            break;
        }
        // SAFETY: POSIX readdir returns a NUL-terminated d_name in the documented platform layout.
        let name = unsafe { CStr::from_ptr(std::ptr::addr_of!((*entry).name).cast::<c_char>()) }
            .to_str()
            .map_err(|_| super::invalid())?
            .to_owned();
        if name != "." && name != ".." {
            names.push(name);
        }
    }
    Ok(names)
}
