// SPDX-License-Identifier: MIT OR Apache-2.0
//! Per-application Windows Explorer registration.
#![cfg(all(windows, not(any(test, feature = "e2e"))))]
use super::Artifact;
use std::io;
use windows_sys::Win32::{
    Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS},
    System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegQueryValueExW, RegSetValueExW, HKEY,
        HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_CREATED_NEW_KEY,
        REG_OPTION_NON_VOLATILE, REG_SZ,
    },
};

const ROOT: &str = "Software\\Classes\\AppUserModelId";
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
fn error(code: i32) -> io::Error {
    io::Error::from_raw_os_error(code)
}
struct Key(HKEY);
impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: this key was returned by RegCreateKeyExW and is closed once.
        unsafe {
            RegCloseKey(self.0);
        }
    }
}
fn key(id: &str) -> io::Result<(Key, bool)> {
    let path = wide(&format!("{ROOT}\\{id}"));
    let mut handle = std::ptr::null_mut();
    let mut disposition = 0; // SAFETY: path is NUL-terminated; output pointers are valid for the call.
    let status = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            path.as_ptr(),
            0,
            std::ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_QUERY_VALUE | KEY_SET_VALUE,
            std::ptr::null(),
            &mut handle,
            &mut disposition,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(error(status as i32));
    }
    Ok((Key(handle), disposition == REG_CREATED_NEW_KEY))
}
fn set(k: HKEY, name: &str, value: &str) -> io::Result<()> {
    let n = wide(name);
    let v = wide(value);
    let bytes =
        u32::try_from(v.len().checked_mul(2).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "registry value too large")
        })?)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "registry value too large"))?; // SAFETY: UTF-16 buffers and their byte length are valid through the call.
    let status = unsafe { RegSetValueExW(k, n.as_ptr(), 0, REG_SZ, v.as_ptr().cast(), bytes) };
    if status == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(error(status as i32))
    }
}
fn query(k: HKEY, name: &str) -> io::Result<Option<(u32, Vec<u8>)>> {
    let n = wide(name);
    let mut kind = 0;
    let mut len = 0; // SAFETY: the initial query requests only the byte count.
    let status = unsafe {
        RegQueryValueExW(
            k,
            n.as_ptr(),
            std::ptr::null(),
            &mut kind,
            std::ptr::null_mut(),
            &mut len,
        )
    };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    if status != ERROR_SUCCESS {
        return Err(error(status as i32));
    }
    if len as usize > 16 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "registry value exceeds size limit",
        ));
    }
    let mut bytes = vec![0u8; len as usize]; // SAFETY: bytes has the size returned by the immediately preceding query.
    let status = unsafe {
        RegQueryValueExW(
            k,
            n.as_ptr(),
            std::ptr::null(),
            &mut kind,
            bytes.as_mut_ptr(),
            &mut len,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(error(status as i32));
    }
    if len as usize > bytes.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "registry value exceeds queried size",
        ));
    }
    bytes.truncate(len as usize);
    Ok(Some((kind, bytes)))
}
fn restore(k: HKEY, name: &str, value: Option<&(u32, Vec<u8>)>) -> io::Result<()> {
    let n = wide(name);
    let status = match value {
        Some((kind, v)) => {
            let len = u32::try_from(v.len()).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "registry value too large")
            })?; // SAFETY: the snapshot buffer remains alive for the call.
            unsafe { RegSetValueExW(k, n.as_ptr(), 0, *kind, v.as_ptr(), len) }
        }
        None => {
            // SAFETY: n is NUL-terminated and k is an open registry key.
            unsafe { windows_sys::Win32::System::Registry::RegDeleteValueW(k, n.as_ptr()) }
        }
    };
    if status == ERROR_SUCCESS || status == ERROR_FILE_NOT_FOUND {
        Ok(())
    } else {
        Err(error(status as i32))
    }
}
pub(super) fn registry_write(a: &Artifact) -> io::Result<()> {
    let (k, created) = key(&a.id)?;
    let old_name = match query(k.0, "DisplayName") {
        Ok(v) => v,
        Err(e) => {
            drop(k);
            if created {
                let _ = registry_remove(&a.id);
            }
            return Err(e);
        }
    };
    let old_icon = match query(k.0, "IconUri") {
        Ok(v) => v,
        Err(e) => {
            drop(k);
            if created {
                let _ = registry_remove(&a.id);
            }
            return Err(e);
        }
    };
    let outcome = set(k.0, "DisplayName", &a.name).and_then(|_| match a.icon.as_deref() {
        Some(icon) => set(k.0, "IconUri", &icon.to_string_lossy()),
        None => restore(k.0, "IconUri", None),
    });
    if let Err(e) = outcome {
        if created {
            drop(k);
            let _ = registry_remove(&a.id);
        } else {
            let _ = restore(k.0, "DisplayName", old_name.as_ref());
            let _ = restore(k.0, "IconUri", old_icon.as_ref());
        }
        return Err(e);
    }
    Ok(())
}
pub(super) fn registry_remove(id: &str) -> io::Result<bool> {
    let path = wide(&format!("{ROOT}\\{id}")); // SAFETY: path is a valid NUL-terminated HKCU subkey.
    let status = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, path.as_ptr()) };
    if status == ERROR_SUCCESS {
        Ok(true)
    } else if status == ERROR_FILE_NOT_FOUND {
        Ok(false)
    } else {
        Err(error(status as i32))
    }
}
