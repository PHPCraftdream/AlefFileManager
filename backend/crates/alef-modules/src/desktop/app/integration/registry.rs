// SPDX-License-Identifier: MIT OR Apache-2.0
//! HKCU registry handle with scoped ownership. Tests write only names of their own, removed after.
use super::unavailable;
use alef_core::AlefError;
use std::ptr;
use windows_sys::Win32::{
    Foundation::{ERROR_FILE_NOT_FOUND, ERROR_NO_MORE_ITEMS, ERROR_SUCCESS},
    System::Registry::*,
};

struct Key(HKEY);
impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: this handle was opened successfully, is owned solely here, and is closed once.
        unsafe {
            RegCloseKey(self.0);
        }
    }
}
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}
fn check(code: u32) -> Result<(), AlefError> {
    if code == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(unavailable(format!("registry error {code}")))
    }
}
impl Key {
    fn run(enable: Option<bool>) -> Result<Option<Self>, AlefError> {
        let path = wide("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
        let mut handle = ptr::null_mut();
        // SAFETY: predefined HKCU is valid; path is NUL terminated; output is writable; no pointers retained.
        let code = unsafe {
            if enable == Some(true) {
                RegCreateKeyExW(
                    HKEY_CURRENT_USER,
                    path.as_ptr(),
                    0,
                    ptr::null(),
                    REG_OPTION_NON_VOLATILE,
                    KEY_QUERY_VALUE | KEY_SET_VALUE,
                    ptr::null(),
                    &mut handle,
                    ptr::null_mut(),
                )
            } else {
                RegOpenKeyExW(
                    HKEY_CURRENT_USER,
                    path.as_ptr(),
                    0,
                    KEY_QUERY_VALUE | if enable.is_some() { KEY_SET_VALUE } else { 0 },
                    &mut handle,
                )
            }
        };
        if code == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        check(code)?;
        Ok(Some(Self(handle)))
    }
    fn read(&self, name: &[u16]) -> Result<Option<Vec<u8>>, AlefError> {
        let mut kind = 0;
        let mut size = 0;
        // SAFETY: owned live key, terminated name, valid output pointers; null buffer requests length only.
        let code = unsafe {
            RegQueryValueExW(
                self.0,
                name.as_ptr(),
                ptr::null(),
                &mut kind,
                ptr::null_mut(),
                &mut size,
            )
        };
        if code == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        check(code)?;
        if kind != REG_SZ || size > 65536 {
            return Ok(Some(Vec::new()));
        }
        let mut bytes = vec![0; size as usize];
        // SAFETY: buffer has the reported size; API bounds writes to size; all pointers live through call.
        check(unsafe {
            RegQueryValueExW(
                self.0,
                name.as_ptr(),
                ptr::null(),
                &mut kind,
                bytes.as_mut_ptr(),
                &mut size,
            )
        })?;
        bytes.truncate(size as usize);
        if kind != REG_SZ {
            return Ok(Some(Vec::new()));
        }
        Ok(Some(bytes))
    }
}

fn validate_command(command: &str) -> Result<(), AlefError> {
    // Run/RunOnce values are documented as command lines of at most 260 characters.
    if command.encode_utf16().count() > 260 {
        return Err(unavailable("Run command exceeds Windows Run limit (260)"));
    }
    Ok(())
}

pub(in crate::desktop::app) fn apply(
    name: &str,
    command: &str,
    enable: Option<bool>,
) -> Result<bool, AlefError> {
    validate_command(command)?;
    let Some(key) = Key::run(enable)? else {
        return Ok(false);
    };
    let name = wide(name);
    let expected: Vec<u8> = wide(command)
        .into_iter()
        .flat_map(u16::to_le_bytes)
        .collect();
    let current = key.read(&name)?;
    let owned = current.as_ref() == Some(&expected);
    let Some(enable) = enable else {
        return Ok(owned);
    };
    if current.is_some() && !owned {
        return Err(unavailable("Run entry belongs to another command"));
    }
    if enable && !owned {
        let size = u32::try_from(expected.len()).map_err(unavailable)?;
        // SAFETY: owned writable key; terminated name; byte buffer and exact checked length remain live.
        check(unsafe { RegSetValueExW(key.0, name.as_ptr(), 0, REG_SZ, expected.as_ptr(), size) })?;
    } else if !enable && owned {
        // SAFETY: owned writable key and terminated value name; no pointers retained.
        let code = unsafe { RegDeleteValueW(key.0, name.as_ptr()) };
        if code != ERROR_FILE_NOT_FOUND {
            check(code)?;
        }
    }
    Ok(enable)
}

fn deep_link_command(launch: &super::Launch) -> Result<String, AlefError> {
    Ok(format!("{} \"%1\"", launch.windows_command()?))
}
fn owned_marker(current: Option<Vec<u8>>, marker: &str) -> bool {
    current == Some(encoded(marker))
}

pub(in crate::desktop::app) fn deep_links(
    launch: &super::Launch,
    schemes: &[String],
    register: bool,
) -> Result<(), AlefError> {
    let command = deep_link_command(launch)?;
    let marker = format!("{}|{}", launch.name, super::path_text(&launch.folder)?);
    // Preflight the complete native subset before any mutation.
    for scheme in schemes {
        if let Some(key) = Key::at(&format!("Software\\Classes\\{scheme}"), false)? {
            if !owned_marker(key.read(&wide("AlefOwner"))?, &marker) {
                return Err(unavailable("scheme belongs to another application"));
            }
            if register
                && key
                    .read(&wide("URL Protocol"))?
                    .is_some_and(|bytes| bytes != encoded(""))
            {
                return Err(unavailable("scheme protocol value has changed"));
            }
            if let Some(key) = Key::at(
                &format!("Software\\Classes\\{scheme}\\shell\\open\\command"),
                false,
            )? {
                if register
                    && key
                        .read(&wide(""))?
                        .is_some_and(|bytes| bytes != encoded(&command))
                {
                    return Err(unavailable("scheme command has changed"));
                }
            }
        }
    }
    for scheme in schemes {
        let path = format!("Software\\Classes\\{scheme}");
        let Some(key) = Key::at(&path, register)? else {
            continue;
        };
        if register {
            key.set("AlefOwner", &marker)?;
            key.set("URL Protocol", "")?;
            Key::at(&format!("{path}\\shell\\open\\command"), true)?
                .ok_or_else(|| unavailable("missing command key"))?
                .set("", &command)?;
        } else {
            // Only exact owned values go; the keys go bottom-up while they are empty, never by a
            // recursive delete: whatever else is there, now or added meanwhile, stays.
            if let Some(command_key) = Key::at(&format!("{path}\\shell\\open\\command"), false)? {
                command_key.remove_if("", &command)?;
            }
            key.remove_if("URL Protocol", "")?;
            key.remove_if("AlefOwner", &marker)?;
            drop(key);
            for suffix in ["\\shell\\open\\command", "\\shell\\open", "\\shell", ""] {
                let child = format!("{path}{suffix}");
                if let Some(key) = Key::at(&child, false)? {
                    if !key.names(false)?.is_empty() || !key.names(true)?.is_empty() {
                        break;
                    }
                    drop(key);
                    let child = wide(&child);
                    // SAFETY: predefined HKCU and terminated path; no pointers retained.
                    let code = unsafe { RegDeleteKeyW(HKEY_CURRENT_USER, child.as_ptr()) };
                    if code != ERROR_FILE_NOT_FOUND {
                        check(code)?;
                    }
                }
            }
        }
    }
    Ok(())
}

fn encoded(value: &str) -> Vec<u8> {
    wide(value).into_iter().flat_map(u16::to_le_bytes).collect()
}
impl Key {
    fn names(&self, children: bool) -> Result<Vec<String>, AlefError> {
        let mut names = Vec::new();
        for index in 0..4096 {
            let mut buffer = vec![0_u16; 16384];
            let mut len = buffer.len() as u32;
            // SAFETY: live key and writable name buffer with its capacity; other outputs
            // are optional and null. The API retains no pointers.
            let code = unsafe {
                if children {
                    RegEnumKeyExW(
                        self.0,
                        index,
                        buffer.as_mut_ptr(),
                        &mut len,
                        ptr::null(),
                        ptr::null_mut(),
                        ptr::null_mut(),
                        ptr::null_mut(),
                    )
                } else {
                    RegEnumValueW(
                        self.0,
                        index,
                        buffer.as_mut_ptr(),
                        &mut len,
                        ptr::null(),
                        ptr::null_mut(),
                        ptr::null_mut(),
                        ptr::null_mut(),
                    )
                }
            };
            if code == ERROR_NO_MORE_ITEMS {
                return Ok(names);
            }
            check(code)?;
            names.push(String::from_utf16(&buffer[..len as usize]).map_err(unavailable)?);
        }
        Err(unavailable("scheme enumeration limit exceeded"))
    }
    fn at(path: &str, create: bool) -> Result<Option<Self>, AlefError> {
        let path = wide(path);
        let mut handle = ptr::null_mut();
        // SAFETY: HKCU is predefined; terminated path and writable output live through the call.
        // Returned handle is owned by Key and closed exactly once. No pointers are retained.
        let code = unsafe {
            if create {
                RegCreateKeyExW(
                    HKEY_CURRENT_USER,
                    path.as_ptr(),
                    0,
                    ptr::null(),
                    REG_OPTION_NON_VOLATILE,
                    KEY_QUERY_VALUE | KEY_SET_VALUE | KEY_ENUMERATE_SUB_KEYS,
                    ptr::null(),
                    &mut handle,
                    ptr::null_mut(),
                )
            } else {
                RegOpenKeyExW(
                    HKEY_CURRENT_USER,
                    path.as_ptr(),
                    0,
                    KEY_QUERY_VALUE | KEY_SET_VALUE | KEY_ENUMERATE_SUB_KEYS,
                    &mut handle,
                )
            }
        };
        if code == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        check(code)?;
        Ok(Some(Self(handle)))
    }
    fn set(&self, name: &str, value: &str) -> Result<(), AlefError> {
        let name = wide(name);
        let bytes = encoded(value);
        let size = u32::try_from(bytes.len()).map_err(unavailable)?;
        // SAFETY: owned live writable key, terminated name and exact byte buffer length.
        check(unsafe { RegSetValueExW(self.0, name.as_ptr(), 0, REG_SZ, bytes.as_ptr(), size) })
    }
    fn remove_if(&self, name: &str, expected: &str) -> Result<(), AlefError> {
        let name = wide(name);
        match self.read(&name)? {
            None => return Ok(()),
            Some(bytes) if bytes == encoded(expected) => (),
            _ => return Ok(()), // A changed value is foreign; preserve it.
        }
        // SAFETY: owned live writable key and terminated value name; no pointers retained.
        let code = unsafe { RegDeleteValueW(self.0, name.as_ptr()) };
        if code == ERROR_FILE_NOT_FOUND {
            Ok(())
        } else {
            check(code)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deep_link_formatting_and_ownership_are_tied_to_folder_and_identity() {
        let scratch = tempfile::tempdir().unwrap();
        let launch = super::super::Launch::resolve("org.example.deep", scratch.path()).unwrap();
        assert_eq!(
            deep_link_command(&launch).unwrap(),
            format!("{} \"%1\"", launch.windows_command().unwrap())
        );
        let marker = format!(
            "{}|{}",
            launch.name,
            super::super::path_text(&launch.folder).unwrap()
        );
        assert!(owned_marker(Some(encoded(&marker)), &marker));
        assert!(!owned_marker(None, &marker));
        assert!(!owned_marker(Some(encoded("foreign")), &marker));
        assert!(!owned_marker(
            Some(encoded(&format!("{marker}/other"))),
            &marker
        ));
    }

    fn unique() -> String {
        use std::sync::atomic::{AtomicU32, Ordering};
        static SERIAL: AtomicU32 = AtomicU32::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        format!(
            "alefunit{}x{}x{nanos}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        )
    }

    struct RunValue(String);
    impl Drop for RunValue {
        fn drop(&mut self) {
            if let Ok(Some(key)) = Key::run(Some(false)) {
                let name = wide(&self.0);
                // SAFETY: owned writable key and terminated value name; no pointers retained.
                unsafe { RegDeleteValueW(key.0, name.as_ptr()) };
            }
        }
    }

    struct Scheme(String);
    impl Drop for Scheme {
        fn drop(&mut self) {
            let path = wide(&format!("Software\\Classes\\{}", self.0));
            // SAFETY: predefined HKCU and terminated path of a name this test made; no pointers retained.
            unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, path.as_ptr()) };
        }
    }

    fn read(path: &str, name: &str) -> Option<Vec<u8>> {
        Key::at(path, false)
            .unwrap()
            .and_then(|key| key.read(&wide(name)).unwrap())
    }

    /// A launch of the application folder `scratch` and a scheme of its own, with the registry
    /// paths of the scheme and of its command.
    fn registered(
        scratch: &tempfile::TempDir,
    ) -> (super::super::Launch, String, Scheme, String, String) {
        let launch = super::super::Launch::resolve("org.example.unit", scratch.path()).unwrap();
        let scheme = unique();
        let path = format!("Software\\Classes\\{scheme}");
        let command_path = format!("{path}\\shell\\open\\command");
        (launch, scheme.clone(), Scheme(scheme), path, command_path)
    }

    #[test]
    fn a_run_value_is_written_read_and_removed_only_while_it_is_ours() {
        let name = unique();
        let _cleanup = RunValue(name.clone());
        let command = r#""C:\unit\alef.exe" "--app" "C:\unit""#;
        assert!(!apply(&name, command, None).unwrap());
        assert!(apply(&name, command, Some(true)).unwrap());
        assert!(apply(&name, command, Some(true)).unwrap());
        assert!(apply(&name, command, None).unwrap());
        assert!(apply(&name, "other", Some(true)).is_err());
        assert!(apply(&name, "other", Some(false)).is_err());
        assert!(!apply(&name, "other", None).unwrap());
        assert!(apply(&name, command, None).unwrap());
        assert!(!apply(&name, command, Some(false)).unwrap());
        assert!(!apply(&name, command, None).unwrap());
        assert!(!apply(&name, command, Some(false)).unwrap());
    }

    #[test]
    fn a_scheme_is_registered_and_removed_with_its_whole_skeleton() {
        let scratch = tempfile::tempdir().unwrap();
        let (launch, scheme, _cleanup, path, command_path) = registered(&scratch);
        let marker = format!(
            "{}|{}",
            launch.name,
            super::super::path_text(&launch.folder).unwrap()
        );
        let command = deep_link_command(&launch).unwrap();
        for _ in 0..2 {
            deep_links(&launch, std::slice::from_ref(&scheme), true).unwrap();
            assert_eq!(read(&path, "AlefOwner"), Some(encoded(&marker)));
            assert_eq!(read(&path, "URL Protocol"), Some(encoded("")));
            assert_eq!(read(&command_path, ""), Some(encoded(&command)));
        }
        for _ in 0..2 {
            deep_links(&launch, std::slice::from_ref(&scheme), false).unwrap();
            assert!(Key::at(&path, false).unwrap().is_none());
        }
    }

    #[test]
    fn a_scheme_of_another_application_is_neither_taken_nor_removed() {
        let scratch = tempfile::tempdir().unwrap();
        let (launch, scheme, _cleanup, path, _) = registered(&scratch);
        let (_, other, _other_cleanup, other_path, _) = registered(&scratch);
        let key = Key::at(&path, true).unwrap().unwrap();
        key.set("AlefOwner", "another|C:\\elsewhere").unwrap();
        Key::at(&other_path, true).unwrap().unwrap();
        for register in [true, false] {
            assert!(deep_links(&launch, std::slice::from_ref(&scheme), register).is_err());
            assert!(deep_links(&launch, std::slice::from_ref(&other), register).is_err());
        }
        assert_eq!(
            read(&path, "AlefOwner"),
            Some(encoded("another|C:\\elsewhere"))
        );
        assert!(Key::at(&other_path, false).unwrap().is_some());
    }

    #[test]
    fn what_was_changed_or_added_in_an_owned_scheme_is_kept() {
        let scratch = tempfile::tempdir().unwrap();
        let (launch, scheme, _cleanup, path, command_path) = registered(&scratch);
        let schemes = [scheme];
        deep_links(&launch, &schemes, true).unwrap();
        Key::at(&command_path, false)
            .unwrap()
            .unwrap()
            .set("", "changed")
            .unwrap();
        let key = Key::at(&path, false).unwrap().unwrap();
        key.set("Extra", "kept").unwrap();
        Key::at(&format!("{path}\\extra"), true).unwrap().unwrap();
        deep_links(&launch, &schemes, false).unwrap();
        assert_eq!(read(&command_path, ""), Some(encoded("changed")));
        assert_eq!(read(&path, "Extra"), Some(encoded("kept")));
        assert!(Key::at(&format!("{path}\\extra"), false).unwrap().is_some());
        assert_eq!(read(&path, "AlefOwner"), None);
        assert_eq!(read(&path, "URL Protocol"), None);
    }

    #[test]
    fn a_changed_protocol_or_command_stops_a_registration() {
        let scratch = tempfile::tempdir().unwrap();
        let (launch, scheme, _cleanup, path, command_path) = registered(&scratch);
        let schemes = [scheme];
        deep_links(&launch, &schemes, true).unwrap();
        let key = Key::at(&path, false).unwrap().unwrap();
        key.set("URL Protocol", "claimed").unwrap();
        assert!(deep_links(&launch, &schemes, true).is_err());
        assert_eq!(read(&path, "URL Protocol"), Some(encoded("claimed")));
        key.set("URL Protocol", "").unwrap();
        deep_links(&launch, &schemes, true).unwrap();
        let command = Key::at(&command_path, false).unwrap().unwrap();
        command.set("", "changed").unwrap();
        assert!(deep_links(&launch, &schemes, true).is_err());
        assert_eq!(read(&command_path, ""), Some(encoded("changed")));
    }

    #[test]
    fn run_limit_counts_utf16_units_without_touching_the_registry() {
        assert!(validate_command(&"a".repeat(260)).is_ok());
        assert!(validate_command(&"a".repeat(261)).is_err());
        assert!(validate_command(&"😀".repeat(130)).is_ok());
        assert!(validate_command(&"😀".repeat(131)).is_err());
    }
}
