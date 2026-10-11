// SPDX-License-Identifier: MIT OR Apache-2.0
//! The operations of `fs` on the disk. They run off the async threads and know nothing of rights:
//! a [`Place`] says where the bytes are, whether in the real folder or in a stand-in.
//! Byte IO checks the opened handle before use; copy, rename, remove, directory listing and
//! path-based metadata still have authorization-to-use races. This is not inode identity checking.
use std::{
    fs::{self, File, Metadata, OpenOptions},
    io::{ErrorKind, Read, Write},
    path::{Path, PathBuf},
    time::SystemTime,
};

use alef_core::{AlefError, ErrorCode};

use super::{
    dto::{DirEntry, FileKind, FileStat},
    fault::{coded, fault, invalid},
    space::Place,
};

/// Opens without truncation; the open-step seam also drives deterministic race tests.
/// `authorized` is the frozen authorization result, never canonicalized again here.
/// Creating a missing file can still leave an empty file outside the scope on a race.
pub(super) fn checked_open(
    authorized: &Path,
    options: &OpenOptions,
    open: impl FnOnce(&OpenOptions, &Path) -> std::io::Result<File>,
) -> Result<File, AlefError> {
    checked_open_by(authorized, options, open, opened_path)
}

/// `checked_open` with the reading of the path of the opened handle as a seam, for the case that
/// the system cannot tell it: the file is refused, not trusted.
fn checked_open_by(
    authorized: &Path,
    options: &OpenOptions,
    open: impl FnOnce(&OpenOptions, &Path) -> std::io::Result<File>,
    opened_path: impl FnOnce(&File) -> std::io::Result<PathBuf>,
) -> Result<File, AlefError> {
    let mut options = options.clone();
    options.truncate(false);
    let file = open(&options, authorized).map_err(fault)?;
    let opened = opened_path(&file).map_err(|_| coded(ErrorCode::PermissionDenied))?;
    if comparable_path(&opened) != comparable_path(authorized) {
        return Err(coded(ErrorCode::PermissionDenied));
    }
    Ok(file)
}

/// Pure spelling normalization only: never resolve the authorized path against the disk again.
/// Stand-in roots may use ordinary rather than verbatim prefixes and contain `.`/`..`.
fn comparable_path(path: &Path) -> PathBuf {
    #[cfg(windows)]
    let spelling = {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
        let prefix: Vec<u16> = r"\\?\".encode_utf16().collect();
        let unc: Vec<u16> = r"\\?\UNC\".encode_utf16().collect();
        if wide.starts_with(&unc) {
            wide.splice(..unc.len(), [b'\\' as u16, b'\\' as u16]);
        } else if wide.starts_with(&prefix) {
            wide.drain(..prefix.len());
        }
        for unit in &mut wide {
            if *unit >= u16::from(b'A') && *unit <= u16::from(b'Z') {
                *unit += u16::from(b'a' - b'A');
            }
        }
        PathBuf::from(std::ffi::OsString::from_wide(&wide))
    };
    #[cfg(target_os = "macos")]
    let spelling = {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};
        PathBuf::from(std::ffi::OsString::from_vec(
            path.as_os_str().as_bytes().to_ascii_lowercase(),
        ))
    };
    #[cfg(not(any(windows, target_os = "macos")))]
    let spelling = path.to_path_buf();
    let mut normalized = PathBuf::new();
    for component in spelling.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

#[cfg(windows)]
fn opened_path(file: &File) -> std::io::Result<PathBuf> {
    use std::os::windows::{ffi::OsStringExt, io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::GetFinalPathNameByHandleW;
    let mut buffer = vec![0u16; 512];
    loop {
        // SAFETY: file owns a live handle throughout the call; buffer is writable for its
        // advertised length. Flags 0 request the normalized DOS path of this handle.
        let length = unsafe {
            GetFinalPathNameByHandleW(
                file.as_raw_handle(),
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                0,
            )
        } as usize;
        if length == 0 {
            return Err(std::io::Error::last_os_error());
        }
        if length < buffer.len() {
            return Ok(PathBuf::from(std::ffi::OsString::from_wide(
                &buffer[..length],
            )));
        }
        if length > 32768 {
            return Err(std::io::Error::other("opened path exceeds limit"));
        }
        buffer.resize(length + 1, 0);
    }
}

#[cfg(target_os = "linux")]
fn opened_path(file: &File) -> std::io::Result<PathBuf> {
    use std::os::fd::AsRawFd;
    fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))
}

#[cfg(target_os = "macos")]
fn opened_path(file: &File) -> std::io::Result<PathBuf> {
    use std::os::{fd::AsRawFd, unix::ffi::OsStringExt};
    let mut buffer = vec![0u8; libc::PATH_MAX as usize];
    // SAFETY: file holds a live fd; F_GETPATH writes at most PATH_MAX bytes into this
    // initialized, writable buffer, which remains allocated throughout the call.
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETPATH, buffer.as_mut_ptr()) } == -1 {
        return Err(std::io::Error::last_os_error());
    }
    let end = buffer
        .iter()
        .position(|&byte| byte == 0)
        .ok_or_else(|| std::io::Error::other("unterminated opened path"))?;
    buffer.truncate(end);
    Ok(PathBuf::from(std::ffi::OsString::from_vec(buffer)))
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn opened_path(_: &File) -> std::io::Result<PathBuf> {
    Err(std::io::Error::new(
        ErrorKind::Unsupported,
        "opened path verification unavailable",
    ))
}

/// What `fs.readDir` lists at most: a bigger folder is for `fs.readDirStream`.
pub(super) const MAX_ENTRIES: usize = 100_000;

fn kind_of(meta: &Metadata) -> FileKind {
    let kind = meta.file_type();
    if kind.is_dir() {
        FileKind::Dir
    } else if kind.is_symlink() {
        FileKind::Symlink
    } else if kind.is_file() {
        FileKind::File
    } else {
        FileKind::Other
    }
}

fn millis(time: std::io::Result<SystemTime>) -> Option<f64> {
    let since = time.ok()?.duration_since(SystemTime::UNIX_EPOCH).ok()?;
    Some(since.as_secs_f64() * 1000.0)
}

fn size_of(meta: &Metadata) -> u64 {
    if meta.is_dir() {
        0
    } else {
        meta.len()
    }
}

fn is_folder(place: &Place) -> bool {
    fs::metadata(&place.real).is_ok_and(|meta| meta.is_dir())
}

/// A file opened for a handle: checked like every other open, and made empty only after the check.
pub(super) fn open_for_handle(
    place: &Place,
    options: &OpenOptions,
    truncate: bool,
) -> Result<File, AlefError> {
    place.prepare().map_err(fault)?;
    if is_folder(place) {
        return Err(coded(ErrorCode::IsADirectory));
    }
    let file = checked_open(&place.real, options, |options, path| options.open(path))?;
    if truncate {
        file.set_len(0).map_err(fault)?;
    }
    Ok(file)
}

pub(super) fn read_file(place: &Place, limit: u64) -> Result<Vec<u8>, AlefError> {
    place.prepare().map_err(fault)?;
    if is_folder(place) {
        return Err(coded(ErrorCode::IsADirectory));
    }
    let file = checked_open(
        &place.real,
        OpenOptions::new().read(true),
        |options, path| options.open(path),
    )?;
    let size = file.metadata().map_err(fault)?.len();
    if size > limit {
        return Err(too_big(limit));
    }
    let mut data = Vec::with_capacity(usize::try_from(size).unwrap_or(0));
    file.take(limit + 1).read_to_end(&mut data).map_err(fault)?;
    if data.len() as u64 > limit {
        return Err(too_big(limit));
    }
    Ok(data)
}

fn too_big(limit: u64) -> AlefError {
    invalid(&format!(
        "more than {} MiB: open the file with fs.open and read it in pieces",
        limit / (1024 * 1024)
    ))
}

pub(super) fn write_file(
    place: &Place,
    data: &[u8],
    append: bool,
    create: bool,
) -> Result<(), AlefError> {
    place.prepare().map_err(fault)?;
    if is_folder(place) {
        return Err(coded(ErrorCode::IsADirectory));
    }
    let mut options = OpenOptions::new();
    if append {
        options.append(true);
    } else {
        options.write(true);
    }
    options.create(create);
    let mut file = checked_open(&place.real, &options, |options, path| options.open(path))?;
    if !append {
        file.set_len(0).map_err(fault)?;
    }
    file.write_all(data).map_err(fault)
}

/// A file made empty and open for writing, for a stream that arrives from elsewhere.
pub(super) fn create_file(place: &Place) -> Result<fs::File, AlefError> {
    place.prepare().map_err(fault)?;
    if is_folder(place) {
        return Err(coded(ErrorCode::IsADirectory));
    }
    let file = checked_open(
        &place.real,
        OpenOptions::new().write(true).create(true),
        |options, path| options.open(path),
    )?;
    file.set_len(0).map_err(fault)?;
    Ok(file)
}

pub(super) fn file_stat(meta: &Metadata) -> FileStat {
    FileStat {
        kind: kind_of(meta),
        size: size_of(meta),
        modified: millis(meta.modified()),
        accessed: millis(meta.accessed()),
        created: millis(meta.created()),
        readonly: meta.permissions().readonly(),
    }
}

/// `follow`: a link is looked through (`stat`), or looked at (`lstat`).
pub(super) fn stat(place: &Place, follow: bool) -> Result<FileStat, AlefError> {
    place.prepare().map_err(fault)?;
    let meta = if follow {
        fs::metadata(&place.real)
    } else {
        fs::symlink_metadata(&place.real)
    }
    .map_err(fault)?;
    Ok(file_stat(&meta))
}

fn entry_of(place: &Place, entry: std::io::Result<fs::DirEntry>) -> Result<DirEntry, AlefError> {
    let entry = entry.map_err(fault)?;
    let meta = entry.metadata().map_err(fault)?;
    let name = entry.file_name().to_string_lossy().into_owned();
    Ok(DirEntry {
        path: place.shown_entry(&name).to_string_lossy().into_owned(),
        name,
        kind: kind_of(&meta),
        size: size_of(&meta),
    })
}

pub(super) fn read_dir(place: &Place) -> Result<Vec<DirEntry>, AlefError> {
    let mut entries = Vec::new();
    for entry in open_dir(place)? {
        if entries.len() >= MAX_ENTRIES {
            return Err(invalid(&format!(
                "more than {MAX_ENTRIES} entries: read the folder with fs.readDirStream"
            )));
        }
        entries.push(entry_of(place, entry)?);
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(entries)
}

/// How many entries of a big folder travel in one frame.
const BATCH: usize = 500;

/// Opens a folder for reading; the mistakes of the first look come now.
pub(super) fn open_dir(place: &Place) -> Result<fs::ReadDir, AlefError> {
    place.prepare().map_err(fault)?;
    fs::read_dir(&place.real).map_err(fault)
}

/// Hands the entries of an opened folder to `send` in batches, in the order of the disk, until the
/// folder ends or `send` says no (the one who reads has gone).
pub(super) fn send_batches(
    place: &Place,
    entries: fs::ReadDir,
    mut send: impl FnMut(Vec<DirEntry>) -> bool,
) -> Result<(), AlefError> {
    let mut batch = Vec::with_capacity(BATCH);
    for entry in entries {
        batch.push(entry_of(place, entry)?);
        if batch.len() == BATCH && !send(std::mem::take(&mut batch)) {
            return Ok(());
        }
    }
    if !batch.is_empty() {
        send(batch);
    }
    Ok(())
}

pub(super) fn mkdir(place: &Place, recursive: bool) -> Result<(), AlefError> {
    place.prepare().map_err(fault)?;
    if recursive {
        fs::create_dir_all(&place.real)
    } else {
        fs::create_dir(&place.real)
    }
    .map_err(fault)
}

pub(super) fn remove(place: &Place, recursive: bool) -> Result<(), AlefError> {
    place.prepare().map_err(fault)?;
    let meta = fs::symlink_metadata(&place.real).map_err(fault)?;
    if meta.is_dir() {
        if recursive {
            fs::remove_dir_all(&place.real)
        } else {
            fs::remove_dir(&place.real)
        }
    } else if meta.file_type().is_symlink() {
        // A link to a folder is removed like a folder on Windows.
        fs::remove_file(&place.real).or_else(|_| fs::remove_dir(&place.real))
    } else {
        fs::remove_file(&place.real)
    }
    .map_err(fault)
}

pub(super) fn exists(place: &Place) -> Result<bool, AlefError> {
    place.prepare().map_err(fault)?;
    fs::exists(&place.real).map_err(fault)
}

pub(super) fn rename(from: &Place, to: &Place) -> Result<(), AlefError> {
    from.prepare().map_err(fault)?;
    to.prepare().map_err(fault)?;
    match fs::rename(&from.real, &to.real) {
        Ok(()) => Ok(()),
        // Another disk, or a stand-in on another disk: copy, then remove.
        Err(error) if error.kind() == ErrorKind::CrossesDevices => {
            copy_tree(from, to)?;
            remove(from, true)
        }
        Err(error) => Err(fault(error)),
    }
}

pub(super) fn copy(from: &Place, to: &Place) -> Result<(), AlefError> {
    from.prepare().map_err(fault)?;
    to.prepare().map_err(fault)?;
    copy_tree(from, to)
}

fn copy_tree(from: &Place, to: &Place) -> Result<(), AlefError> {
    let meta = fs::metadata(&from.real).map_err(fault)?;
    if meta.is_dir() {
        if to.real.starts_with(&from.real) {
            return Err(invalid("a folder cannot be copied into itself"));
        }
        copy_folder(&from.real, &to.real)
    } else {
        if is_folder(to) {
            return Err(coded(ErrorCode::IsADirectory));
        }
        fs::copy(&from.real, &to.real).map(drop).map_err(fault)
    }
}

fn copy_folder(from: &std::path::Path, to: &std::path::Path) -> Result<(), AlefError> {
    fs::create_dir(to).map_err(fault)?;
    for entry in fs::read_dir(from).map_err(fault)? {
        let entry = entry.map_err(fault)?;
        let kind = entry.file_type().map_err(fault)?;
        let target = to.join(entry.file_name());
        if kind.is_symlink() {
            return Err(invalid("a link inside the folder: links are not copied"));
        } else if kind.is_dir() {
            copy_folder(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target).map_err(fault)?;
        }
    }
    Ok(())
}

#[cfg(all(test, any(windows, unix)))]
mod tests;
