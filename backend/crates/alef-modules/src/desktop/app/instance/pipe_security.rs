// SPDX-License-Identifier: MIT OR Apache-2.0
//! The security of the instance pipe on Windows: who may open it and whose it is. A pipe made
//! with the default security can be read by every user of the machine, and its name can be taken
//! by another user before the application starts.
use std::{ffi::c_void, io, mem::size_of, os::windows::io::RawHandle, ptr};

use windows_sys::Win32::{
    Foundation::{CloseHandle, LocalFree, HANDLE},
    Security::{
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            GetSecurityInfo, SDDL_REVISION_1, SE_KERNEL_OBJECT,
        },
        GetTokenInformation, TokenOwner, TokenUser, OWNER_SECURITY_INFORMATION,
        PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, TOKEN_INFORMATION_CLASS, TOKEN_QUERY,
    },
    System::Threading::{GetCurrentProcess, OpenProcessToken},
};

/// A protected DACL with one entry: the owner of the pipe (`OW`, owner rights), nobody else.
pub(super) const OWNER_ONLY: &str = "D:P(A;;GA;;;OW)";

/// The security attributes of a pipe that only its owner can open.
pub(super) struct OwnerOnly {
    descriptor: PSECURITY_DESCRIPTOR,
    attributes: SECURITY_ATTRIBUTES,
}

impl OwnerOnly {
    pub(super) fn new() -> io::Result<Self> {
        let text: Vec<u16> = OWNER_ONLY.encode_utf16().chain(Some(0)).collect();
        let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
        // SAFETY: `text` ends with a zero and outlives the call; the descriptor is freed in `Drop`.
        let made = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                text.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                ptr::null_mut(),
            )
        };
        if made == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            descriptor,
            attributes: SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: descriptor,
                bInheritHandle: 0,
            },
        })
    }

    /// For `create_with_security_attributes_raw`; valid while `self` lives.
    pub(super) fn as_raw(&mut self) -> *mut c_void {
        ptr::from_mut(&mut self.attributes).cast()
    }
}

impl Drop for OwnerOnly {
    fn drop(&mut self) {
        // SAFETY: made by `ConvertStringSecurityDescriptorToSecurityDescriptorW`, freed once.
        unsafe { LocalFree(self.descriptor) };
    }
}

fn sid_text(sid: PSID) -> io::Result<String> {
    let mut text: *mut u16 = ptr::null_mut();
    // SAFETY: `sid` is a valid SID; the text it makes is freed below.
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the text is zero-terminated and lives until the `LocalFree`.
    let value = unsafe {
        let mut length = 0;
        while *text.add(length) != 0 {
            length += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(text, length))
    };
    // SAFETY: made by `ConvertSidToStringSidW`.
    unsafe { LocalFree(text.cast()) };
    Ok(value)
}

/// The SID that a structure of the token (`TOKEN_USER`, `TOKEN_OWNER`: both start with it) holds.
fn token_sid(token: HANDLE, class: TOKEN_INFORMATION_CLASS) -> io::Result<String> {
    let mut size = 0_u32;
    // SAFETY: asks only for the size; the call fails with a short buffer by design.
    unsafe { GetTokenInformation(token, class, ptr::null_mut(), 0, &mut size) };
    if size == 0 {
        return Err(io::Error::last_os_error());
    }
    // Words, not bytes: the structure starts with a pointer.
    let mut buffer = vec![0_usize; (size as usize).div_ceil(size_of::<usize>())];
    // SAFETY: the buffer holds `size` bytes.
    if unsafe { GetTokenInformation(token, class, buffer.as_mut_ptr().cast(), size, &mut size) }
        == 0
    {
        return Err(io::Error::last_os_error());
    }
    sid_text(buffer[0] as PSID)
}

/// The SIDs that objects made by this process belong to: the user, and the default owner of the
/// token (the group of administrators when the process is elevated).
pub(super) fn own_sids() -> io::Result<Vec<String>> {
    let mut token: HANDLE = ptr::null_mut();
    // SAFETY: the token is closed below.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let sids =
        token_sid(token, TokenUser).and_then(|user| Ok(vec![user, token_sid(token, TokenOwner)?]));
    // SAFETY: opened above.
    unsafe { CloseHandle(token) };
    sids
}

/// The owner of the object behind `handle`, as the text of a SID.
pub(super) fn owner_of(handle: RawHandle) -> io::Result<String> {
    let mut owner: PSID = ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: `handle` is a live handle with the right to read its security; the descriptor holds
    // the owner and is freed below.
    let status = unsafe {
        GetSecurityInfo(
            handle as HANDLE,
            SE_KERNEL_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            &mut descriptor,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    let text = sid_text(owner);
    // SAFETY: made by `GetSecurityInfo`.
    unsafe { LocalFree(descriptor) };
    text
}

/// Whether `owner` is one of `mine`.
pub(super) fn is_mine(owner: &str, mine: &[String]) -> bool {
    mine.iter().any(|sid| sid == owner)
}

#[cfg(test)]
mod tests {
    use std::{os::windows::io::AsRawHandle, sync::atomic::AtomicU32, time::Duration};

    use tokio::net::windows::named_pipe::{ClientOptions, ServerOptions};
    use windows_sys::Win32::Security::{
        Authorization::ConvertSecurityDescriptorToStringSecurityDescriptorW, ACL,
        DACL_SECURITY_INFORMATION,
    };

    use super::super::{platform, Claim, Endpoint};
    use super::*;

    fn unique(what: &str) -> String {
        static COUNT: AtomicU32 = AtomicU32::new(0);
        format!(
            "test-{what}-{}-{}",
            std::process::id(),
            COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        )
    }

    fn endpoint(what: &str) -> Endpoint {
        Endpoint {
            folder: std::path::PathBuf::new(),
            key: unique(what),
        }
    }

    /// The DACL of the object behind `handle` in the text form of the security descriptors.
    fn dacl_text(handle: RawHandle) -> String {
        let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
        let mut dacl: *mut ACL = ptr::null_mut();
        // SAFETY: a live handle, the descriptor is freed below.
        let status = unsafe {
            GetSecurityInfo(
                handle as HANDLE,
                SE_KERNEL_OBJECT,
                DACL_SECURITY_INFORMATION,
                ptr::null_mut(),
                ptr::null_mut(),
                &mut dacl,
                ptr::null_mut(),
                &mut descriptor,
            )
        };
        assert_eq!(status, 0, "the DACL is read");
        let mut text: *mut u16 = ptr::null_mut();
        // SAFETY: the descriptor is valid, the text is freed below.
        let made = unsafe {
            ConvertSecurityDescriptorToStringSecurityDescriptorW(
                descriptor,
                SDDL_REVISION_1,
                DACL_SECURITY_INFORMATION,
                &mut text,
                ptr::null_mut(),
            )
        };
        assert_ne!(made, 0, "the DACL is written as text");
        // SAFETY: zero-terminated text made by the call above.
        let value = unsafe {
            let mut length = 0;
            while *text.add(length) != 0 {
                length += 1;
            }
            String::from_utf16_lossy(std::slice::from_raw_parts(text, length))
        };
        // SAFETY: both were made by the calls above.
        unsafe {
            LocalFree(text.cast());
            LocalFree(descriptor);
        }
        value
    }

    fn entries(text: &str) -> usize {
        text.matches('(').count()
    }

    #[tokio::test]
    async fn a_pipe_of_ours_lets_in_its_owner_alone_and_the_default_one_does_not() {
        let mut security = OwnerOnly::new().unwrap();
        let ours =
            platform::serve(&format!(r"\\.\pipe\{}", unique("dacl")), &mut security).unwrap();
        let text = dacl_text(ours.as_raw_handle());
        assert!(text.starts_with("D:P"), "nothing is inherited: {text}");
        assert_eq!(entries(&text), 1, "one entry: {text}");
        assert!(!text.contains("WD") && !text.contains("AN"), "{text}");
        let plain = ServerOptions::new()
            .create(format!(r"\\.\pipe\{}", unique("default")))
            .unwrap();
        assert!(
            entries(&dacl_text(plain.as_raw_handle())) > 1,
            "the control: the default security names more than the owner"
        );
    }

    #[tokio::test]
    async fn the_next_instance_of_the_pipe_is_as_closed_as_the_first() {
        let endpoint = endpoint("next");
        let Claim::First(mut listener) = platform::claim(&endpoint).await.unwrap() else {
            panic!("the pipe was free");
        };
        let name = format!(r"\\.\pipe\alef-instance-{}", endpoint.key);
        let client = ClientOptions::new().open(&name).unwrap();
        let accepted = tokio::time::timeout(Duration::from_secs(5), listener.accept())
            .await
            .expect("accept ends in time")
            .unwrap();
        drop((client, accepted));
        let text = dacl_text(listener.server.as_raw_handle());
        assert!(text.starts_with("D:P") && entries(&text) == 1, "{text}");
    }

    #[tokio::test]
    async fn what_this_user_made_is_mine_and_a_file_of_the_system_is_not() {
        let mut security = OwnerOnly::new().unwrap();
        let server =
            platform::serve(&format!(r"\\.\pipe\{}", unique("mine")), &mut security).unwrap();
        let mine = own_sids().unwrap();
        assert!(!mine.is_empty());
        assert!(is_mine(&owner_of(server.as_raw_handle()).unwrap(), &mine));
        let system = std::path::Path::new(&std::env::var_os("SystemRoot").unwrap())
            .join("System32")
            .join("kernel32.dll");
        let file = std::fs::File::open(system).unwrap();
        assert!(!is_mine(&owner_of(file.as_raw_handle()).unwrap(), &mine));
        assert!(is_mine(
            "S-1-5-21-1",
            &["S-1-5-18".into(), "S-1-5-21-1".into()]
        ));
        assert!(!is_mine("S-1-5-21-1", &["S-1-5-21-10".into()]));
        assert!(!is_mine("S-1-5-21-1", &[]));
    }

    #[test]
    fn the_user_and_the_default_owner_of_the_token_are_both_mine() {
        let mut token: HANDLE = ptr::null_mut();
        // SAFETY: closed below.
        assert_ne!(
            unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) },
            0
        );
        let expected = [
            token_sid(token, TokenUser).unwrap(),
            token_sid(token, TokenOwner).unwrap(),
        ];
        // SAFETY: opened above.
        unsafe { CloseHandle(token) };
        assert_eq!(own_sids().unwrap(), expected);
    }

    #[tokio::test]
    async fn a_pipe_that_belongs_to_somebody_else_is_not_talked_to() {
        let endpoint = endpoint("foreign");
        let Claim::First(_listener) = platform::claim(&endpoint).await.unwrap() else {
            panic!("the pipe was free");
        };
        let error = platform::connect_as(&endpoint, &["S-1-5-18".to_owned()])
            .await
            .map(|_| ())
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    }
}
