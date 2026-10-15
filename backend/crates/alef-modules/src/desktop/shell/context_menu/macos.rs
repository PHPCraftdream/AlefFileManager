// SPDX-License-Identifier: MIT OR Apache-2.0
//! The `Open With` catalog of macOS, backed by Launch Services.
//!
//! Launch Services is used instead of NSWorkspace because its synchronous URL APIs are available
//! through the system C frameworks and avoid depending on unverified objc2 selector bindings.
use std::{
    ffi::{c_char, c_void, CStr, CString},
    path::Path,
    ptr, thread,
};

use alef_core::{AlefError, ErrorCode};

type CFTypeRef = *const c_void;
type CFArrayRef = *const c_void;
type CFStringRef = *const c_void;
type CFURLRef = *const c_void;
type CFIndex = isize;
const MAX_APPS: CFIndex = 128;
const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

#[repr(C)]
struct CFArrayCallBacks {
    version: CFIndex,
    retain: *const c_void,
    release: *const c_void,
    copy_description: *const c_void,
    equal: *const c_void,
}

#[repr(C)]
struct LSLaunchURLSpec {
    app_url: CFURLRef,
    item_urls: CFArrayRef,
    pass_through: CFArrayRef,
    launch_flags: u32,
    async_ref_con: *mut c_void,
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(value: CFTypeRef);
    fn CFArrayCreate(
        allocator: CFTypeRef,
        values: *const *const c_void,
        count: CFIndex,
        callbacks: *const CFArrayCallBacks,
    ) -> CFArrayRef;
    fn CFArrayGetCount(array: CFArrayRef) -> CFIndex;
    fn CFArrayGetValueAtIndex(array: CFArrayRef, index: CFIndex) -> *const c_void;
    fn CFURLCreateWithFileSystemPath(
        allocator: CFTypeRef,
        path: CFStringRef,
        style: CFIndex,
        is_directory: u8,
    ) -> CFURLRef;
    fn CFURLCreateWithString(allocator: CFTypeRef, value: CFStringRef, base: CFURLRef) -> CFURLRef;
    fn CFURLCopyLastPathComponent(url: CFURLRef) -> CFStringRef;
    fn CFURLGetString(url: CFURLRef) -> CFStringRef;
    fn CFStringCreateWithCString(
        allocator: CFTypeRef,
        value: *const c_char,
        encoding: u32,
    ) -> CFStringRef;
    fn CFStringGetLength(value: CFStringRef) -> CFIndex;
    fn CFStringGetMaximumSizeForEncoding(length: CFIndex, encoding: u32) -> CFIndex;
    fn CFStringGetCString(
        value: CFStringRef,
        buffer: *mut c_char,
        capacity: CFIndex,
        encoding: u32,
    ) -> u8;
    fn CFStringGetCStringPtr(value: CFStringRef, encoding: u32) -> *const c_char;
    static kCFTypeArrayCallBacks: CFArrayCallBacks;
}

#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    fn LSCopyApplicationURLsForURL(url: CFURLRef, role_mask: u32) -> CFArrayRef;
    fn LSOpenFromURLSpec(spec: *const LSLaunchURLSpec, launched_url: *mut CFURLRef) -> i32;
}

struct Owned(CFTypeRef);
impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: Owned contains only non-null references returned with Create/Copy ownership.
        unsafe { CFRelease(self.0) }
    }
}

fn cf_string(text: &str) -> Option<Owned> {
    let text = CString::new(text).ok()?;
    // SAFETY: CString is terminated and remains alive for the call; UTF-8 encoding is valid.
    let value =
        unsafe { CFStringCreateWithCString(ptr::null(), text.as_ptr(), K_CF_STRING_ENCODING_UTF8) };
    if value.is_null() {
        None
    } else {
        Some(Owned(value))
    }
}

fn string(value: CFStringRef) -> Option<String> {
    if value.is_null() {
        return None;
    }
    // SAFETY: value is a valid CFString obtained from a framework-owned result.
    let direct = unsafe { CFStringGetCStringPtr(value, K_CF_STRING_ENCODING_UTF8) };
    if !direct.is_null() {
        // SAFETY: CoreFoundation returns a NUL-terminated pointer valid while the string is alive.
        return unsafe { CStr::from_ptr(direct) }
            .to_str()
            .ok()
            .map(str::to_owned);
    }
    // SAFETY: value is a valid CFString; negative lengths signal conversion failure.
    let length = unsafe { CFStringGetLength(value) };
    if length < 0 {
        return None;
    }
    // SAFETY: value and encoding are valid; checked capacity follows below.
    let max = unsafe { CFStringGetMaximumSizeForEncoding(length, K_CF_STRING_ENCODING_UTF8) };
    let capacity = max.checked_add(1)?;
    if capacity <= 0 || capacity > 16 * 1024 * 1024 {
        return None;
    }
    let mut buffer = vec![0u8; usize::try_from(capacity).ok()?];
    // SAFETY: buffer has the checked capacity required by CoreFoundation and is writable.
    if unsafe {
        CFStringGetCString(
            value,
            buffer.as_mut_ptr().cast(),
            capacity,
            K_CF_STRING_ENCODING_UTF8,
        )
    } == 0
    {
        return None;
    }
    // SAFETY: successful CFStringGetCString writes a terminated UTF-8 string into the buffer.
    unsafe { CStr::from_ptr(buffer.as_ptr().cast()) }
        .to_str()
        .ok()
        .map(str::to_owned)
}

fn path_url(path: &Path) -> Option<Owned> {
    let path = cf_string(path.to_str()?)?;
    // SAFETY: path is a valid CFString; style 0 is kCFURLPOSIXPathStyle.
    let value = unsafe { CFURLCreateWithFileSystemPath(ptr::null(), path.0, 0, 0) };
    if value.is_null() {
        None
    } else {
        Some(Owned(value))
    }
}

fn catalog(path: &Path) -> Vec<(String, String)> {
    let Some(file_url) = path_url(path) else {
        return Vec::new();
    };
    // SAFETY: input is a valid CFURL; Copy result is owned and released on this same worker.
    let array = unsafe { LSCopyApplicationURLsForURL(file_url.0, u32::MAX) };
    if array.is_null() {
        return Vec::new();
    }
    let array = Owned(array);
    // SAFETY: array is a valid copied CFArray.
    let count = unsafe { CFArrayGetCount(array.0) }.clamp(0, MAX_APPS);
    let mut result = Vec::new();
    for index in 0..count {
        // SAFETY: index is within the bounded array count; result is borrowed from array.
        let app_url = unsafe { CFArrayGetValueAtIndex(array.0, index) };
        if app_url.is_null() {
            continue;
        }
        // SAFETY: Launch Services returns CFURL values; Copy result is owned and released here.
        let component = unsafe { CFURLCopyLastPathComponent(app_url) };
        let Some(component) = (if component.is_null() {
            None
        } else {
            Some(Owned(component))
        }) else {
            continue;
        };
        let Some(name) = string(component.0) else {
            continue;
        };
        let name = name.trim_end_matches(".app").to_owned();
        if !name.is_empty() {
            // Preserve the URL identifier for subsequent Launch Services lookup.
            let raw = unsafe { CFURLGetString(app_url) };
            let Some(id) = string(raw) else { continue };
            result.push((id, name));
        }
    }
    result.sort_by(|a, b| a.0.cmp(&b.0));
    result.dedup_by(|a, b| a.0 == b.0);
    result
}

fn native<T: Send + 'static>(operation: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    thread::Builder::new()
        .name("alef-open-with-launch-services".into())
        .spawn(operation)
        .ok()?
        .join()
        .ok()
}

/// The applications the system offers for the file, as (identifier, display name).
pub(super) fn open_with_apps(path: &Path) -> Vec<(String, String)> {
    let path = path.to_owned();
    native(move || catalog(&path)).unwrap_or_default()
}

/// Hands the file to one application of the catalog.
pub(super) fn launch_with(path: &Path, app_id: &str) -> Result<(), AlefError> {
    let path = path.to_owned();
    let app_id = app_id.to_owned();
    native(move || {
        if !catalog(&path).iter().any(|(id, _)| id == &app_id) {
            return Err(AlefError::new(
                ErrorCode::NotFound,
                "Open With application not found",
            ));
        }
        let app_text = cf_string(&app_id)
            .ok_or_else(|| AlefError::new(ErrorCode::InvalidArgument, "invalid application URL"))?;
        // SAFETY: app_text is a valid CFString containing an absolute Launch Services URL.
        let app_url = unsafe { CFURLCreateWithString(ptr::null(), app_text.0, ptr::null()) };
        if app_url.is_null() {
            return Err(AlefError::new(
                ErrorCode::InvalidArgument,
                "invalid application URL",
            ));
        }
        let app_url = Owned(app_url);
        let file_url = path_url(&path).ok_or_else(|| {
            AlefError::new(ErrorCode::InvalidArgument, "file path is not valid UTF-8")
        })?;
        let values = [file_url.0];
        // SAFETY: callback table is CoreFoundation's exported CFType array callbacks; array retains
        // the URL according to that table and owns its own reference until its release.
        let item_urls =
            unsafe { CFArrayCreate(ptr::null(), values.as_ptr(), 1, &kCFTypeArrayCallBacks) };
        if item_urls.is_null() {
            return Err(AlefError::new(
                ErrorCode::Internal,
                "could not create launch item list",
            ));
        }
        let item_urls = Owned(item_urls);
        let spec = LSLaunchURLSpec {
            app_url: app_url.0,
            item_urls: item_urls.0,
            pass_through: ptr::null(),
            launch_flags: 0,
            async_ref_con: ptr::null_mut(),
        };
        // SAFETY: spec pointers are valid for the synchronous call; output is intentionally unused.
        let status = unsafe { LSOpenFromURLSpec(&spec, ptr::null_mut()) };
        if status == 0 {
            Ok(())
        } else {
            Err(AlefError::new(
                ErrorCode::Internal,
                format!("Launch Services failed ({status})"),
            ))
        }
    })
    .ok_or_else(|| AlefError::new(ErrorCode::Internal, "Launch Services worker failed"))?
}
