// SPDX-License-Identifier: MIT OR Apache-2.0
//! Windows shell icons. Small and large system image lists supply native sizes above 32px.
use std::{
    mem,
    os::windows::{ffi::OsStrExt, io::AsRawHandle},
    path::Path,
    ptr,
};

use alef_core::{AlefError, ErrorCode};
use windows::Win32::{
    Foundation::SIZE,
    UI::{
        Controls::{IImageList, ILD_TRANSPARENT},
        Shell::{
            IShellItemImageFactory, SHCreateItemFromParsingName, SHGetImageList, SHIL_EXTRALARGE,
            SHIL_JUMBO, SIIGBF_ICONONLY,
        },
    },
};
use windows_sys::Win32::{
    Foundation::{S_FALSE, S_OK, WAIT_FAILED, WAIT_OBJECT_0},
    Graphics::Gdi::{
        CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GdiFlush, GetDIBits,
        SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
    },
    Storage::FileSystem::FILE_ATTRIBUTE_NORMAL,
    System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED},
    System::Threading::INFINITE,
    UI::{
        Shell::{
            SHGetFileInfoW, SHGetStockIconInfo, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON,
            SHGFI_SMALLICON, SHGFI_SYSICONINDEX, SHGFI_USEFILEATTRIBUTES, SHGSI_ICON,
            SHGSI_SMALLICON, SHGSI_SYSICONINDEX, SHSTOCKICONINFO, SIID_DRIVEFIXED, SIID_FOLDER,
        },
        WindowsAndMessaging::{
            DestroyIcon, DispatchMessageW, DrawIconEx, MsgWaitForMultipleObjectsEx, PeekMessageW,
            TranslateMessage, DI_NORMAL, HICON, MSG, MWMO_INPUTAVAILABLE, PM_REMOVE, QS_ALLINPUT,
        },
    },
};

#[cfg(test)]
use windows_sys::Win32::UI::WindowsAndMessaging::{PostThreadMessageW, WM_APP};

use super::icon::{
    generic_fallback, mime_to_extension, rgba_to_png, run_worker_loop_with_receive,
    ExtensionTarget, IconBackend, IconImage, IconKind, IconTarget, JobReceiver, MimeTarget,
    PathTarget,
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
            // SAFETY: shell/image-list APIs return an owned HICON, destroyed exactly once.
            unsafe { DestroyIcon(self.0) };
        }
    }
}

struct DcHandle(HDC);
impl Drop for DcHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: this DC is owned from CreateCompatibleDC; selected objects are restored first.
            unsafe { DeleteDC(self.0) };
        }
    }
}

struct BitmapHandle(HBITMAP);
impl Drop for BitmapHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: this bitmap is owned from CreateDIBSection or the shell factory.
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

struct WindowsBackend(Result<ComApartment, AlefError>);
impl IconBackend for WindowsBackend {
    fn render(&mut self, target: &IconTarget, size: u32) -> Result<IconImage, AlefError> {
        match &self.0 {
            Ok(_) => {
                pump_messages();
                icon_for(target, size).or_else(|error| match target {
                    IconTarget::Path(_) => Err(error),
                    _ => Ok(generic_fallback()),
                })
            }
            Err(_) => match target {
                IconTarget::Path(_) => Err(internal("COM STA initialization failed")),
                _ => Ok(generic_fallback()),
            },
        }
    }
}

pub(crate) fn worker_loop(
    mut receiver: JobReceiver,
    wake: std::sync::Arc<std::os::windows::io::OwnedHandle>,
) {
    let event = wake.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;
    run_worker_loop_with_receive(
        || receive_or_pump(&mut receiver, event),
        WindowsBackend(initialize_com()),
    );
}

fn receive_or_pump(
    receiver: &mut JobReceiver,
    event: windows_sys::Win32::Foundation::HANDLE,
) -> Option<super::icon::IconJob> {
    receive_or_pump_with(receiver, event, pump_messages)
}

fn receive_or_pump_with(
    receiver: &mut JobReceiver,
    event: windows_sys::Win32::Foundation::HANDLE,
    mut pump: impl FnMut(),
) -> Option<super::icon::IconJob> {
    loop {
        match receiver.try_recv() {
            Ok(job) => return Some(job),
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => return None,
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {}
        }
        pump();
        // SAFETY: event is a valid owned event handle and the handle array has one item.
        let result = unsafe {
            MsgWaitForMultipleObjectsEx(1, &event, INFINITE, QS_ALLINPUT, MWMO_INPUTAVAILABLE)
        };
        if result == WAIT_FAILED {
            return None;
        }
        if result == WAIT_OBJECT_0 {
            continue;
        }
        if result == WAIT_OBJECT_0 + 1 {
            pump();
            continue;
        }
        // WAIT_FAILED or an unexpected result must not turn into a hot loop.
        return None;
    }
}

fn pump_messages() {
    pump_messages_with(|_| {});
}

fn pump_messages_with(mut on_message: impl FnMut(&MSG)) {
    // SAFETY: zeroed MSG is writable output for PeekMessageW; null hwnd drains this STA's queue.
    let mut message: MSG = unsafe { mem::zeroed() };
    // SAFETY: message is a valid output buffer; PM_REMOVE removes messages owned by this thread.
    while unsafe { PeekMessageW(&mut message, ptr::null_mut(), 0, 0, PM_REMOVE) } != 0 {
        on_message(&message);
        // SAFETY: retrieved message is valid for translation and dispatch on this thread.
        unsafe {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

fn icon_for(target: &IconTarget, requested: u32) -> Result<IconImage, AlefError> {
    let size = native_size(requested);
    let icon = match target {
        IconTarget::Path(PathTarget { path }) => {
            let wide: Vec<u16> = Path::new(path)
                .as_os_str()
                .encode_wide()
                .chain(Some(0))
                .collect();
            if requested > LARGE {
                // SAFETY: wide is NUL-terminated and remains alive for the shell call.
                let factory: IShellItemImageFactory = unsafe {
                    SHCreateItemFromParsingName(windows::core::PCWSTR(wide.as_ptr()), None)
                }
                .map_err(internal)?;
                // SAFETY: valid positive dimensions and the COM interface is live on this worker's STA.
                let bitmap = unsafe {
                    factory.GetImage(
                        SIZE {
                            cx: requested as i32,
                            cy: requested as i32,
                        },
                        SIIGBF_ICONONLY,
                    )
                }
                .map_err(internal)?;
                return bitmap_to_image(bitmap);
            }
            file_icon(&wide, size, false)?
        }
        IconTarget::Extension(ExtensionTarget { extension }) => {
            let fake = fake_filename(extension);
            if requested > LARGE {
                return image_list_file_icon(&fake, true, requested);
            }
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
            if requested > LARGE {
                return image_list_file_icon(&fake, true, requested);
            }
            file_icon(&fake, size, true)?
        }
        IconTarget::Kind(kind) => {
            let id = match kind.kind {
                IconKind::Folder => SIID_FOLDER,
                IconKind::File => windows_sys::Win32::UI::Shell::SIID_DOCNOASSOC,
                IconKind::Drive => SIID_DRIVEFIXED,
            };
            if requested > LARGE {
                return image_list_stock_icon(id, requested);
            }
            stock_icon(id, size)?
        }
    };
    icon_to_image(icon, size)
}

fn bitmap_to_image(source: windows::Win32::Graphics::Gdi::HBITMAP) -> Result<IconImage, AlefError> {
    let bitmap = BitmapHandle(source.0 as HBITMAP);
    // SAFETY: zeroed BITMAP is an output buffer for GetObjectW; bitmap is live.
    let mut dimensions: windows_sys::Win32::Graphics::Gdi::BITMAP = unsafe { mem::zeroed() };
    // SAFETY: dimensions is writable and correctly sized; shell bitmap remains live.
    let got = unsafe {
        windows_sys::Win32::Graphics::Gdi::GetObjectW(
            bitmap.0 as HGDIOBJ,
            mem::size_of_val(&dimensions) as i32,
            (&mut dimensions as *mut _).cast(),
        )
    };
    if got != mem::size_of_val(&dimensions) as i32 {
        return Err(internal("GetObjectW failed for shell bitmap"));
    }
    let width = dimensions.bmWidth;
    let height = dimensions.bmHeight.unsigned_abs();
    if width <= 0 || height == 0 || width > 4096 || height > 4096 {
        return Err(internal("shell bitmap dimensions invalid or exceed 4096px"));
    }
    let width_u = width as u32;
    let byte_len = (width_u as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(4))
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(|| internal("icon dimensions overflow"))?;
    // SAFETY: null requests a memory DC compatible with the interactive desktop.
    let dc = DcHandle(unsafe { CreateCompatibleDC(ptr::null_mut()) });
    if dc.0.is_null() {
        return Err(internal("CreateCompatibleDC failed"));
    }
    // SAFETY: zeroed BITMAPINFO initializes its color table storage before setting header fields.
    let mut info: BITMAPINFO = unsafe { mem::zeroed() };
    info.bmiHeader = BITMAPINFOHEADER {
        biSize: mem::size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: width,
        biHeight: -(height as i32),
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB,
        biSizeImage: byte_len,
        ..unsafe { mem::zeroed() }
    };
    let mut bgra = vec![0u8; byte_len as usize];
    // SAFETY: live bitmap/DC, correctly sized buffer, and a top-down 32bpp header.
    let lines = unsafe {
        GetDIBits(
            dc.0,
            bitmap.0,
            0,
            height,
            bgra.as_mut_ptr().cast(),
            &mut info,
            DIB_RGB_COLORS,
        )
    };
    if lines != height as i32 {
        return Err(internal("GetDIBits failed for shell image"));
    }
    super::icon::unpremultiply_bgra_to_rgba(&mut bgra);
    let png = rgba_to_png(width_u, height, &bgra)?;
    Ok(IconImage {
        width: width_u,
        height,
        png,
    })
}

fn image_list_size(requested: u32) -> u32 {
    if requested >= 128 {
        256
    } else {
        48
    }
}

fn get_image_list(requested: u32) -> Result<IImageList, AlefError> {
    let kind = if image_list_size(requested) == 256 {
        SHIL_JUMBO as i32
    } else {
        SHIL_EXTRALARGE as i32
    };
    // SAFETY: the system image-list kind is supported and IImageList is the COM interface requested.
    unsafe { SHGetImageList::<IImageList>(kind) }.map_err(internal)
}

fn image_list_icon(index: i32, requested: u32) -> Result<IconImage, AlefError> {
    let list = get_image_list(requested)?;
    let mut width = 0;
    let mut height = 0;
    // SAFETY: output dimensions are writable and COM interface is live.
    unsafe { list.GetIconSize(&mut width, &mut height) }.map_err(internal)?;
    // SAFETY: index comes from a successful shell icon-index query; requests transparent rendering.
    let handle = unsafe { list.GetIcon(index, ILD_TRANSPARENT.0) }.map_err(internal)?;
    icon_to_image(IconHandle(handle.0), width as u32)
}

fn image_list_file_icon(path: &[u16], fake: bool, requested: u32) -> Result<IconImage, AlefError> {
    // SAFETY: zeroed SHFILEINFO is writable output; path is NUL-terminated UTF-16.
    let mut info: SHFILEINFOW = unsafe { mem::zeroed() };
    let flags = SHGFI_SYSICONINDEX | if fake { SHGFI_USEFILEATTRIBUTES } else { 0 };
    let attrs = if fake { FILE_ATTRIBUTE_NORMAL } else { 0 };
    // SAFETY: path/structure are valid; USEFILEATTRIBUTES prevents fake extension paths from being accessed.
    let result = unsafe {
        SHGetFileInfoW(
            path.as_ptr(),
            attrs,
            &mut info,
            mem::size_of::<SHFILEINFOW>() as u32,
            flags,
        )
    };
    if result == 0 {
        return Err(internal("SHGetFileInfoW returned no system image index"));
    }
    image_list_icon(info.iIcon, requested)
}

fn image_list_stock_icon(id: i32, requested: u32) -> Result<IconImage, AlefError> {
    // SAFETY: zeroed stock info is writable output; cbSize is set before the call.
    let mut info: SHSTOCKICONINFO = unsafe { mem::zeroed() };
    info.cbSize = mem::size_of::<SHSTOCKICONINFO>() as u32;
    // SAFETY: correctly sized output structure; SYSICONINDEX does not return an owned HICON.
    let hr = unsafe { SHGetStockIconInfo(id, SHGSI_SYSICONINDEX, &mut info) };
    if hr < 0 {
        return Err(internal(format!(
            "SHGetStockIconInfo failed: HRESULT 0x{:08x}",
            hr as u32
        )));
    }
    image_list_icon(info.iSysImageIndex, requested)
}

fn file_icon(path: &[u16], size: u32, fake: bool) -> Result<IconHandle, AlefError> {
    // SAFETY: zero is a valid initial representation for this plain C output structure.
    let mut info: SHFILEINFOW = unsafe { mem::zeroed() };
    let flags = file_icon_flags(size) | if fake { SHGFI_USEFILEATTRIBUTES } else { 0 };
    let attributes = if fake { FILE_ATTRIBUTE_NORMAL } else { 0 };
    // SAFETY: path is NUL-terminated UTF-16 and info correctly sized; fake paths use USEFILEATTRIBUTES.
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
    // SAFETY: info is writable and correctly sized; the API returns an owned HICON.
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
    // SAFETY: null requests a memory DC compatible with the interactive desktop.
    let dc = DcHandle(unsafe { CreateCompatibleDC(ptr::null_mut()) });
    if dc.0.is_null() {
        return Err(internal("CreateCompatibleDC failed"));
    }
    // SAFETY: zeroed color field initializes BITMAPINFO before use.
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
    let bitmap = BitmapHandle(unsafe {
        CreateDIBSection(dc.0, &info, DIB_RGB_COLORS, &mut bits, ptr::null_mut(), 0)
    });
    if bitmap.0.is_null() || bits.is_null() {
        return Err(internal("CreateDIBSection failed"));
    }
    // SAFETY: bits belongs to this DIB and byte_len is its checked writable extent.
    unsafe { ptr::write_bytes(bits.cast::<u8>(), 0, byte_len as usize) };
    // SAFETY: bitmap is live and the returned handle is the previously selected object.
    let previous = unsafe { SelectObject(dc.0, bitmap.0 as HGDIOBJ) };
    if previous.is_null() || previous as isize == -1 {
        return Err(internal("SelectObject failed"));
    }
    let selected = SelectedBitmap { dc: dc.0, previous };
    // SAFETY: DC, icon and DIB are live; DI_NORMAL draws the image and mask.
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
    // SAFETY: flushes completed GDI operations before reading DIB pixels.
    if unsafe { GdiFlush() } == 0 {
        return Err(internal("GdiFlush failed"));
    }
    // SAFETY: selected DIB remains live and bits points to byte_len initialized bytes.
    let bgra = unsafe { std::slice::from_raw_parts_mut(bits.cast::<u8>(), byte_len as usize) };
    let black = bgra.to_vec();
    for pixel in bgra.chunks_exact_mut(4) {
        pixel.copy_from_slice(&[255, 255, 255, 0]);
    }
    // SAFETY: DC, DIB and icon remain live; white background permits mask alpha recovery.
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
    // SAFETY: completes the second GDI draw before reading DIB memory.
    if unsafe { GdiFlush() } == 0 {
        return Err(internal("GdiFlush failed"));
    }
    let rgba = recover_alpha(&black, bgra)?;
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
    use std::{sync::mpsc, thread, time::Duration};

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
        assert_eq!(image_list_size(48), 48);
        assert_eq!(image_list_size(64), 48);
        assert_eq!(image_list_size(128), 256);
        assert_eq!(image_list_size(256), 256);
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

    #[test]
    fn idle_sta_pumps_posted_thread_message_and_exits_on_sender_drop() {
        let (sender, receiver, wake) =
            super::super::icon::WindowsJobSender::channel().expect("create bounded wake event");
        let (thread_tx, thread_rx) = mpsc::channel();
        let (message_tx, message_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            let mut receiver = receiver;
            let event = wake.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;
            let _sta = initialize_com().expect("initialize test COM STA");
            let mut queue_probe: MSG = unsafe { mem::zeroed() };
            // SAFETY: queue_probe is writable; null HWND initializes this thread's queue.
            unsafe { PeekMessageW(&mut queue_probe, ptr::null_mut(), 0, 0, PM_REMOVE) };
            // SAFETY: returns the calling thread's identifier for PostThreadMessageW.
            let thread_id = unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() };
            thread_tx.send(thread_id).unwrap();
            let job = receive_or_pump_with(&mut receiver, event, || {
                pump_messages_with(|message| {
                    if message.message == WM_APP + 7 {
                        let _ = message_tx.send(());
                    }
                });
            });
            done_tx.send(job.is_some()).unwrap();
            let closed = receive_or_pump_with(&mut receiver, event, pump_messages);
            done_tx.send(closed.is_none()).unwrap();
        });
        let thread_id = thread_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("STA thread start");
        // SAFETY: PostThreadMessageW posts a plain application message to the live worker thread.
        let posted = unsafe { PostThreadMessageW(thread_id, WM_APP + 7, 0, 0) };
        assert_ne!(posted, 0, "post idle STA message");
        message_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("idle message dispatched");
        let (_reply, reply_rx) = tokio::sync::oneshot::channel();
        sender
            .try_send(super::super::icon::IconJob {
                target: IconTarget::Kind(super::super::icon::KindTarget {
                    kind: IconKind::Folder,
                }),
                size: 32,
                reply: _reply,
            })
            .expect("enqueue test job signals wake event");
        assert!(done_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("queued job received"));
        drop(reply_rx);
        drop(sender);
        assert!(done_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("worker exits on close"));
        worker.join().expect("worker joined");
    }

    fn assert_png_has_opaque_non_background_pixel(png: &[u8]) {
        let decoder = png::Decoder::new(std::io::Cursor::new(png));
        let mut reader = decoder.read_info().expect("decode PNG header");
        let mut buffer = vec![0; reader.output_buffer_size().expect("PNG output buffer size")];
        let info = reader.next_frame(&mut buffer).expect("decode PNG pixels");
        let pixels = &buffer[..info.buffer_size()];
        let (channels, has_alpha) = match info.color_type {
            png::ColorType::Rgba => (4, true),
            png::ColorType::Rgb => (3, false),
            _ => panic!("unexpected decoded PNG color type: {:?}", info.color_type),
        };
        assert!(
            pixels
                .chunks_exact(channels)
                .any(|p| p[0..3] != [0, 0, 0] && (!has_alpha || p[3] == 255)),
            "all decoded pixels are transparent or black"
        );
    }

    #[test]
    fn native_path_and_generic_large_icons_have_real_png_dimensions() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let target = IconTarget::Path(PathTarget {
            path: temp.path().to_string_lossy().into_owned(),
        });
        let (tx, rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            let result = initialize_com().and_then(|_sta| {
                let paths = [48, 64, 128, 256].map(|size| icon_for(&target, size));
                let generic = icon_for(
                    &IconTarget::Extension(ExtensionTarget {
                        extension: "txt".into(),
                    }),
                    48,
                );
                Ok((paths, generic))
            });
            let _ = tx.send(result);
        });
        let (paths, generic) = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("STA icon call timed out")
            .expect("COM STA and shell image calls succeed");
        worker.join().expect("STA worker joined");
        for (requested, image) in [48, 64, 128, 256].into_iter().zip(paths) {
            let image = image.expect("native path icon");
            assert!(
                image.width > 32 && image.height > 32,
                "{requested}: {image:?}"
            );
            assert_png_has_opaque_non_background_pixel(&image.png);
            assert_eq!(
                super::super::icon::png_dimensions(&image.png),
                Some((image.width, image.height))
            );
        }
        let generic = generic.expect("generic extension image-list icon");
        assert!(generic.width > 32 && generic.height > 32);
        assert_png_has_opaque_non_background_pixel(&generic.png);
        assert_eq!(
            super::super::icon::png_dimensions(&generic.png),
            Some((generic.width, generic.height))
        );
    }
}
