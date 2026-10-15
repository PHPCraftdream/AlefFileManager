// SPDX-License-Identifier: MIT OR Apache-2.0
use super::super::meta::dto::Volume;
use alef_core::AlefError;
use windows_sys::Win32::Storage::FileSystem::{
    GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
    FILE_READ_ONLY_VOLUME,
};

pub(super) fn list() -> Result<Vec<Volume>, AlefError> {
    // SAFETY: GetLogicalDrives has no pointer arguments and returns a drive bitmask.
    let mask = unsafe { GetLogicalDrives() };
    if mask == 0 {
        return Err(super::super::meta::fault::fault(
            std::io::Error::last_os_error(),
        ));
    }
    let mut volumes = Vec::new();
    for index in 0..26u32 {
        if mask & (1 << index) == 0 {
            continue;
        }
        let letter = char::from_u32(u32::from(b'A') + index).unwrap_or('A');
        let root = format!("{letter}:\\");
        let wide: Vec<u16> = root.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: `wide` is a valid NUL-terminated UTF-16 root string.
        let drive = unsafe { GetDriveTypeW(wide.as_ptr()) };
        if drive == 1 {
            continue;
        }
        let mut name = [0u16; 261];
        let mut fs = [0u16; 261];
        let mut serial = 0u32;
        let mut max_component = 0u32;
        let mut flags = 0u32;
        // SAFETY: input is NUL-terminated and output buffers are writable with their lengths supplied.
        let info_ok = unsafe {
            GetVolumeInformationW(
                wide.as_ptr(),
                name.as_mut_ptr(),
                name.len() as u32,
                &mut serial,
                &mut max_component,
                &mut flags,
                fs.as_mut_ptr(),
                fs.len() as u32,
            )
        } != 0;
        let mut available = 0u64;
        let mut total = 0u64;
        let mut total_free = 0u64;
        // SAFETY: input is NUL-terminated and all output pointers refer to initialized writable u64 values.
        let space_ok = unsafe {
            GetDiskFreeSpaceExW(wide.as_ptr(), &mut available, &mut total, &mut total_free)
        } != 0;
        let label = info_ok
            .then(|| {
                String::from_utf16_lossy(
                    &name[..name.iter().position(|v| *v == 0).unwrap_or(name.len())],
                )
            })
            .filter(|s| !s.is_empty());
        let file_system = info_ok
            .then(|| {
                String::from_utf16_lossy(&fs[..fs.iter().position(|v| *v == 0).unwrap_or(fs.len())])
            })
            .filter(|s| !s.is_empty());
        volumes.push(Volume {
            root,
            label,
            kind: super::drive_type(drive),
            file_system,
            total: space_ok.then_some(total),
            free: space_ok.then_some(available),
            read_only: info_ok && flags & FILE_READ_ONLY_VOLUME != 0,
        });
    }
    volumes.sort_by(|a, b| a.root.cmp(&b.root));
    Ok(volumes)
}
