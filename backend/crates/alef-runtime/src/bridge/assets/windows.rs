// SPDX-License-Identifier: MIT OR Apache-2.0
//! Minimal ntdll bindings checked against Windows SDK/windows-sys 0.52 declarations.
use std::{
    ffi::c_void,
    fs::{File, OpenOptions},
    io,
    os::windows::{
        fs::{MetadataExt, OpenOptionsExt},
        io::{AsRawHandle, FromRawHandle},
    },
    path::Path,
    ptr,
};
type Handle = *mut c_void;
#[repr(C)]
struct Unicode {
    length: u16,
    maximum: u16,
    buffer: *mut u16,
}
#[repr(C)]
struct Attributes {
    length: u32,
    root: Handle,
    name: *mut Unicode,
    flags: u32,
    security: *mut c_void,
    quality: *mut c_void,
}
#[repr(C)]
struct Status {
    status: usize,
    information: usize,
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetFileType(handle: Handle) -> u32;
}
#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtCreateFile(
        handle: *mut Handle,
        access: u32,
        attributes: *const Attributes,
        status: *mut Status,
        allocation: *const i64,
        flags: u32,
        share: u32,
        disposition: u32,
        options: u32,
        ea: *const c_void,
        ea_length: u32,
    ) -> i32;
    fn NtQueryDirectoryFile(
        handle: Handle,
        event: Handle,
        apc: *const c_void,
        context: *const c_void,
        status: *mut Status,
        buffer: *mut c_void,
        length: u32,
        class: u32,
        single: u8,
        pattern: *const Unicode,
        restart: u8,
    ) -> i32;
    fn NtQueryInformationFile(
        handle: Handle,
        status: *mut Status,
        buffer: *mut c_void,
        length: u32,
        class: u32,
    ) -> i32;
    fn RtlNtStatusToDosError(status: i32) -> u32;
}
fn error(status: i32) -> io::Error {
    // SAFETY: pure conversion of an NTSTATUS; no pointers or owned resources.
    io::Error::from_raw_os_error(unsafe { RtlNtStatusToDosError(status) } as i32)
}
fn verify(file: File) -> io::Result<File> {
    // SAFETY: borrowed live handle; GetFileType does not take ownership.
    if unsafe { GetFileType(file.as_raw_handle()) } != 1 {
        return Err(super::denied());
    }
    let meta = file.metadata()?;
    if meta.file_attributes() & 0x400 != 0 || (!meta.is_dir() && !meta.is_file()) {
        return Err(super::denied());
    }
    Ok(file)
}
pub(super) fn root(path: &Path) -> io::Result<File> {
    // BACKUP_SEMANTICS | OPEN_REPARSE_POINT applies to the application itself, not ancestors.
    let file = verify(
        OpenOptions::new()
            .read(true)
            .custom_flags(0x02000000 | 0x00200000)
            .open(path)?,
    )?;
    if !file.metadata()?.is_dir() {
        return Err(super::denied());
    }
    Ok(file)
}
fn open(directory: &File, name: &str, kind: u32) -> io::Result<File> {
    // NT relative empty name reopens the directory itself; NT does not accept Win32 ".".
    let mut wide: Vec<u16> = if name == "." {
        Vec::new()
    } else {
        name.encode_utf16().collect()
    };
    let length = u16::try_from(wide.len() * 2).map_err(|_| super::invalid())?;
    let mut unicode = Unicode {
        length,
        maximum: length,
        buffer: wide.as_mut_ptr(),
    };
    let attributes = Attributes {
        length: size_of::<Attributes>() as u32,
        root: directory.as_raw_handle(),
        name: &mut unicode,
        flags: 0,
        security: ptr::null_mut(),
        quality: ptr::null_mut(),
    };
    let mut status = Status {
        status: 0,
        information: 0,
    };
    let mut handle = ptr::null_mut();
    // SAFETY: live directory handle; one validated component (or internal "."); UTF16 buffer
    // and repr(C) outputs live through synchronous call. FILE_OPEN never creates. No case folding,
    // OPEN_REPARSE_POINT never follows the leaf. Every intermediate directory is already opened.
    let result = unsafe {
        NtCreateFile(
            &mut handle,
            0x00100081,
            &attributes,
            &mut status,
            ptr::null(),
            0,
            7,
            1,
            0x00200000 | 0x20 | kind,
            ptr::null(),
            0,
        )
    };
    if result < 0 {
        return Err(error(result));
    }
    // SAFETY: successful NtCreateFile transferred a unique owned handle, closed by File.
    let file = verify(unsafe { File::from_raw_handle(handle) })?;
    if name != "." {
        verify_name(&file, name)?;
    }
    Ok(file)
}
// Enumeration-before-open rejects aliases. Verify the opened object's name too, so a rename
// between enumeration and open cannot change exact long-name selection into alias selection.
fn verify_name(file: &File, expected: &str) -> io::Result<()> {
    let mut buffer = vec![0u32; 16384];
    let mut status = Status {
        status: 0,
        information: 0,
    };
    // SAFETY: live handle, aligned writable buffer, synchronous query. FileNormalizedNameInformation
    // (48) gives the stored long name; FileNameInformation (9) echoes a short alias as opened.
    let code = unsafe {
        NtQueryInformationFile(
            file.as_raw_handle(),
            &mut status,
            buffer.as_mut_ptr().cast(),
            (buffer.len() * size_of::<u32>()) as u32,
            48,
        )
    };
    if code < 0 {
        return Err(error(code));
    }
    let length = buffer[0] as usize;
    if status.information < 4
        || !length.is_multiple_of(2)
        || length + 4 > status.information
        || status.information > buffer.len() * size_of::<u32>()
    {
        return Err(super::invalid());
    }
    // SAFETY: checked returned length and alignment; FileNameInformation starts UTF16 at byte 4.
    let wide =
        unsafe { std::slice::from_raw_parts(buffer.as_ptr().cast::<u16>().add(2), length / 2) };
    let path = String::from_utf16(wide).map_err(|_| super::invalid())?;
    if path.rsplit('\\').next() != Some(expected) {
        return Err(super::denied());
    }
    Ok(())
}
pub(super) fn child(directory: &File, name: &str, is_directory: bool) -> io::Result<File> {
    open(directory, name, if is_directory { 1 } else { 0x40 })
}
pub(super) fn any_child(directory: &File, name: &str) -> io::Result<File> {
    open(directory, name, 0)
}
pub(super) fn names(directory: &File) -> io::Result<Vec<String>> {
    // A separate open description prevents concurrent requests sharing an enumeration cursor.
    let scan = child(directory, ".", true)?;
    let mut result = Vec::new();
    let mut buffer = [0u32; 1024];
    let mut restart = 1;
    loop {
        let mut status = Status {
            status: 0,
            information: 0,
        };
        // SAFETY: synchronous handle, no callbacks; aligned writable buffer of stated size.
        // FileNamesInformation (12) returns the long name, never its DOS short alias.
        let code = unsafe {
            NtQueryDirectoryFile(
                scan.as_raw_handle(),
                ptr::null_mut(),
                ptr::null(),
                ptr::null(),
                &mut status,
                buffer.as_mut_ptr().cast(),
                size_of_val(&buffer) as u32,
                12,
                1,
                ptr::null(),
                restart,
            )
        };
        restart = 0;
        if code as u32 == 0x80000006 {
            break;
        } // STATUS_NO_MORE_FILES
        if code < 0 {
            return Err(error(code));
        }
        let length = buffer[2] as usize;
        if status.information < 12
            || !length.is_multiple_of(2)
            || length + 12 > status.information
            || status.information > size_of_val(&buffer)
        {
            return Err(super::invalid());
        }
        // SAFETY: checked bounds, buffer is u32-aligned and UTF16 starts at byte 12.
        let wide =
            unsafe { std::slice::from_raw_parts(buffer.as_ptr().cast::<u16>().add(6), length / 2) };
        let name = String::from_utf16(wide).map_err(|_| super::invalid())?;
        if name != "." && name != ".." {
            result.push(name);
        }
    }
    Ok(result)
}

#[cfg(test)]
mod abi_tests {
    use super::*;
    #[test]
    fn native_layout_matches_the_windows_sdk() {
        assert_eq!(size_of::<Status>(), 2 * size_of::<usize>());
        assert_eq!(
            std::mem::offset_of!(Unicode, buffer),
            if size_of::<usize>() == 8 { 8 } else { 4 }
        );
        assert_eq!(
            size_of::<Attributes>(),
            if size_of::<usize>() == 8 { 48 } else { 24 }
        );
        assert_eq!(
            std::mem::offset_of!(Attributes, root),
            if size_of::<usize>() == 8 { 8 } else { 4 }
        );
    }
}
