// SPDX-License-Identifier: MIT OR Apache-2.0
//! Windows shell icons. SHGetFileInfo supplies 16px small and 32px large native sizes.
use std::{mem, os::windows::ffi::OsStrExt, path::Path, ptr};

use alef_core::{AlefError, ErrorCode};
use windows_sys::Win32::{
    Foundation::{S_FALSE, S_OK},
    Graphics::Gdi::{
        CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GdiFlush, SelectObject,
        BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
    },
    Storage::FileSystem::FILE_ATTRIBUTE_NORMAL,
    System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED},
    UI::{
        Shell::{
            SHGetFileInfoW, SHGetStockIconInfo, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON,
            SHGFI_SMALLICON, SHGFI_USEFILEATTRIBUTES, SHGSI_ICON, SHGSI_SMALLICON, SHSTOCKICONINFO,
            SIID_DRIVEFIXED, SIID_FOLDER,
        },
        WindowsAndMessaging::{DestroyIcon, DrawIconEx, DI_NORMAL, HICON},
    },
};

use super::icon::{
    generic_fallback, mime_to_extension, rgba_to_png, ExtensionTarget, IconImage, IconKind,
    IconTarget, JobReceiver, MimeTarget, PathTarget,
};

const SMALL: u32 = 16;
const LARGE: u32 = 32;

fn native_size(requested: u32) -> u32 {
    if requested.abs_diff(SMALL) <= requested.abs_diff(LARGE) {
        SMALL
    } else {
        LARGE
    }
}

fn file_icon_flags(size: u32) -> u32 {
    SHGFI_ICON
        | if size == SMALL {
            SHGFI_SMALLICON
        } else {
            SHGFI_LARGEICON
        }
}

fn fake_filename(extension: &str) -> Vec<u16> {
    let mut name: Vec<u16> = "alef-icon.".encode_utf16().collect();
    name.extend(extension.encode_utf16());
    name.push(0);
    name
}

fn internal(message: impl std::fmt::Display) -> AlefError {
    AlefError::new(ErrorCode::Internal, format!("shell.fileIcon: {message}"))
}

struct IconHandle(HICON);
impl Drop for IconHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the shell APIs return an owned icon handle; it is destroyed exactly once here.
            unsafe { DestroyIcon(self.0) };
        }
    }
}

struct DcHandle(HDC);
impl Drop for DcHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: this DC is owned from CreateCompatibleDC and all selected objects are restored first.
            unsafe { DeleteDC(self.0) };
        }
    }
}

struct BitmapHandle(HBITMAP);
impl Drop for BitmapHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: this DIB is owned from CreateDIBSection and is deselected before deletion.
            unsafe { DeleteObject(self.0 as HGDIOBJ) };
        }
    }
}

struct SelectedBitmap {
    dc: HDC,
    previous: HGDIOBJ,
}
impl Drop for SelectedBitmap {
    fn drop(&mut self) {
        if !self.previous.is_null() {
            // SAFETY: previous is the GDI object displaced by SelectObject on this live DC.
            unsafe { SelectObject(self.dc, self.previous) };
        }
    }
}

struct ComApartment(bool);
impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: balances this thread's successful CoInitializeEx call.
            unsafe { CoUninitialize() };
        }
    }
}

fn initialize_com() -> Result<ComApartment, AlefError> {
    // SAFETY: worker thread initialization supplies the required null reserved pointer.
    let hr = unsafe { CoInitializeEx(ptr::null(), COINIT_APARTMENTTHREADED as u32) };
    if hr == S_OK || hr == S_FALSE {
        Ok(ComApartment(true))
    } else {
        Err(internal(format!(
            "COM STA initialization failed: HRESULT 0x{:08x}",
            hr as u32
        )))
    }
}

pub(crate) fn worker_loop(mut receiver: JobReceiver) {
    let apartment = initialize_com();
    while let Some(job) = receiver.blocking_recv() {
        if job.reply.is_closed() {
            continue;
        }
        let result = match &apartment {
            Ok(_) => icon_for(&job.target, job.size).or_else(|error| match &job.target {
                IconTarget::Path(_) => Err(error),
                _ => Ok(generic_fallback()),
            }),
            Err(_) => match &job.target {
                IconTarget::Path(_) => Err(internal("COM STA initialization failed")),
                _ => Ok(generic_fallback()),
            },
        };
        let _ = job.reply.send(result);
    }
}

fn icon_for(target: &IconTarget, requested: u32) -> Result<IconImage, AlefError> {
    let size = native_size(requested);
    let icon = match target {
        IconTarget::Path(PathTarget { path }) => {
            let wide = Path::new(path)
                .as_os_str()
                .encode_wide()
                .chain(Some(0))
                .collect::<Vec<_>>();
            file_icon(&wide, size, false)?
        }
        IconTarget::Extension(ExtensionTarget { extension }) => {
            let fake = fake_filename(extension);
            file_icon(&fake, size, true)?
        }
        IconTarget::Mime(MimeTarget { mime }) => {
            let extension = mime_to_extension(mime).ok_or_else(|| {
                AlefError::new(
                    ErrorCode::InvalidArgument,
                    "shell.fileIcon: unsupported mime type",
                )
            })?;
            let fake = fake_filename(extension);
            file_icon(&fake, size, true)?
        }
        IconTarget::Kind(kind) => stock_icon(
            match kind.kind {
                IconKind::Folder => SIID_FOLDER,
                IconKind::File => windows_sys::Win32::UI::Shell::SIID_DOCNOASSOC,
                IconKind::Drive => SIID_DRIVEFIXED,
            },
            size,
        )?,
    };
    icon_to_image(icon, size)
}

fn file_icon(path: &[u16], size: u32, fake: bool) -> Result<IconHandle, AlefError> {
    // SAFETY: zero is a valid initial representation for this plain C output structure.
    let mut info: SHFILEINFOW = unsafe { mem::zeroed() };
    let flags = file_icon_flags(size) | if fake { SHGFI_USEFILEATTRIBUTES } else { 0 };
    let attributes = if fake { FILE_ATTRIBUTE_NORMAL } else { 0 };
    // SAFETY: path is NUL-terminated UTF-16 and info is correctly sized. USEFILEATTRIBUTES
    // explicitly avoids examining fake extension filenames; real paths were authorized upstream.
    let result = unsafe {
        SHGetFileInfoW(
            path.as_ptr(),
            attributes,
            &mut info,
            mem::size_of::<SHFILEINFOW>() as u32,
            flags,
        )
    };
    if result == 0 || info.hIcon.is_null() {
        return Err(internal("SHGetFileInfoW returned no icon"));
    }
    Ok(IconHandle(info.hIcon))
}

fn stock_icon(id: i32, size: u32) -> Result<IconHandle, AlefError> {
    // SAFETY: zero is valid initialization for this C output structure.
    let mut info: SHSTOCKICONINFO = unsafe { mem::zeroed() };
    info.cbSize = mem::size_of::<SHSTOCKICONINFO>() as u32;
    let flags = SHGSI_ICON | if size == SMALL { SHGSI_SMALLICON } else { 0 };
    // SAFETY: info is writable, sized by cbSize, and the API returns an owned HICON.
    let hr = unsafe { SHGetStockIconInfo(id, flags, &mut info) };
    if hr < 0 || info.hIcon.is_null() {
        return Err(internal(format!(
            "SHGetStockIconInfo failed: HRESULT 0x{:08x}",
            hr as u32
        )));
    }
    Ok(IconHandle(info.hIcon))
}

fn icon_to_image(icon: IconHandle, size: u32) -> Result<IconImage, AlefError> {
    let side = i32::try_from(size).map_err(internal)?;
    let byte_len = (size as usize)
        .checked_mul(size as usize)
        .and_then(|n| n.checked_mul(4))
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(|| internal("icon dimensions overflow"))?;
    // SAFETY: null requests a memory DC compatible with the current display; worker is on an interactive desktop.
    let dc = unsafe { CreateCompatibleDC(ptr::null_mut()) };
    let dc = DcHandle(dc);
    if dc.0.is_null() {
        return Err(internal("CreateCompatibleDC failed"));
    }
    // SAFETY: zero is a valid initial representation for this plain C BITMAPINFO color field.
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: side,
            biHeight: -side,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            biSizeImage: byte_len,
            biXPelsPerMeter: 0,
            biYPelsPerMeter: 0,
            biClrUsed: 0,
            biClrImportant: 0,
        },
        bmiColors: [unsafe { mem::zeroed() }],
    };
    let mut bits = ptr::null_mut();
    // SAFETY: info describes a bounded top-down 32bpp DIB; bits is its writable output pointer.
    let bitmap =
        unsafe { CreateDIBSection(dc.0, &info, DIB_RGB_COLORS, &mut bits, ptr::null_mut(), 0) };
    let bitmap = BitmapHandle(bitmap);
    if bitmap.0.is_null() || bits.is_null() {
        return Err(internal("CreateDIBSection failed"));
    }
    // SAFETY: bits belongs to this DIB and byte_len is its checked writable extent.
    unsafe { ptr::write_bytes(bits.cast::<u8>(), 0, byte_len as usize) };
    // SAFETY: bitmap is a live DIB and the returned handle is the prior object selected into this DC.
    let previous = unsafe { SelectObject(dc.0, bitmap.0 as HGDIOBJ) };
    if previous.is_null() || previous as isize == -1 {
        return Err(internal("SelectObject failed"));
    }
    let selected = SelectedBitmap { dc: dc.0, previous };
    // SAFETY: DC, icon and DIB remain live; native side is 16 or 32 and DI_NORMAL draws mask and image.
    if unsafe {
        DrawIconEx(
            dc.0,
            0,
            0,
            icon.0,
            side,
            side,
            0,
            ptr::null_mut(),
            DI_NORMAL,
        )
    } == 0
    {
        return Err(internal("DrawIconEx failed"));
    }
    // SAFETY: GDI operations above are complete before reading the rendered black-background pixels.
    if unsafe { GdiFlush() } == 0 {
        return Err(internal("GdiFlush failed"));
    }
    // SAFETY: the DIB remains selected and bits points to byte_len initialized writable bytes.
    let bgra = unsafe { std::slice::from_raw_parts_mut(bits.cast::<u8>(), byte_len as usize) };
    let black_render = bgra.to_vec();
    for pixel in bgra.chunks_exact_mut(4) {
        pixel.copy_from_slice(&[255, 255, 255, 0]);
    }
    // SAFETY: the same live DIB, DC, and icon are retained; white background permits mask alpha recovery.
    if unsafe {
        DrawIconEx(
            dc.0,
            0,
            0,
            icon.0,
            side,
            side,
            0,
            ptr::null_mut(),
            DI_NORMAL,
        )
    } == 0
    {
        return Err(internal("DrawIconEx failed"));
    }
    // SAFETY: completes the white-background draw before reading DIB memory.
    if unsafe { GdiFlush() } == 0 {
        return Err(internal("GdiFlush failed"));
    }
    let rgba = recover_alpha(&black_render, bgra)?;
    let png = rgba_to_png(size, size, &rgba)?;
    drop(selected);
    drop(bitmap);
    drop(dc);
    Ok(IconImage {
        width: size,
        height: size,
        png,
    })
}

fn recover_alpha(black: &[u8], white: &[u8]) -> Result<Vec<u8>, AlefError> {
    if black.len() != white.len() || black.len() % 4 != 0 {
        return Err(internal("invalid icon color buffers"));
    }
    let mut rgba = Vec::with_capacity(black.len());
    for (black, white) in black.chunks_exact(4).zip(white.chunks_exact(4)) {
        let alpha = 255u32
            - (0..3)
                .map(|channel| white[channel].saturating_sub(black[channel]) as u32)
                .sum::<u32>()
                / 3;
        for channel in [2, 1, 0] {
            let value = if alpha == 0 {
                0
            } else {
                (black[channel] as u32 * 255 / alpha).min(255) as u8
            };
            rgba.push(value);
        }
        rgba.push(alpha as u8);
    }
    Ok(rgba)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_recovery_preserves_black_opaque_and_transparent_pixels() {
        let black = [0, 0, 0, 0, 0, 0, 0, 0];
        let white = [0, 0, 0, 0, 255, 255, 255, 0];
        assert_eq!(
            recover_alpha(&black, &white).unwrap(),
            [0, 0, 0, 255, 0, 0, 0, 0]
        );
    }

    #[test]
    fn native_size_chooses_nearest_supported_shell_size() {
        assert_eq!(native_size(16), 16);
        assert_eq!(native_size(24), 16);
        assert_eq!(native_size(25), 32);
        assert_eq!(native_size(256), 32);
    }

    #[test]
    fn file_flags_select_small_or_large_shell_icon() {
        assert_eq!(file_icon_flags(SMALL), SHGFI_ICON | SHGFI_SMALLICON);
        assert_eq!(file_icon_flags(LARGE), SHGFI_ICON | SHGFI_LARGEICON);
    }

    #[test]
    fn fake_name_is_a_synthetic_extension_filename() {
        assert_eq!(
            fake_filename("pdf"),
            "alef-icon.pdf\0".encode_utf16().collect::<Vec<_>>()
        );
    }
}
