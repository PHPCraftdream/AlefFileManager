// SPDX-License-Identifier: MIT OR Apache-2.0
//! macOS system icons rendered by AppKit on the dedicated icon worker.
use objc2::rc::{autoreleasepool, Retained};
use objc2::{runtime::AnyObject, AllocAnyThread};
use objc2_app_kit::{
    NSBitmapImageFileType, NSBitmapImageRep, NSBitmapImageRepPropertyKey, NSDeviceRGBColorSpace,
    NSGraphicsContext, NSImage, NSWorkspace,
};
use objc2_foundation::{NSCopying, NSDictionary, NSPoint, NSRect, NSSize, NSString};
use objc2_uniform_type_identifiers::UTType;

use super::icon::{
    generic_fallback, mime_to_extension, run_worker_loop, IconBackend, IconImage, IconKind,
    IconTarget, JobReceiver,
};

struct MacBackend;

impl IconBackend for MacBackend {
    fn render(
        &mut self,
        target: &IconTarget,
        size: u32,
    ) -> Result<IconImage, alef_core::AlefError> {
        autoreleasepool(|_| match (target, icon_for(target, size)) {
            (IconTarget::Path(_), result) => result,
            (_, Ok(image)) => Ok(image),
            (_, Err(_)) => Ok(generic_fallback()),
        })
    }
}

pub(crate) fn worker_loop(receiver: JobReceiver) {
    run_worker_loop(receiver, MacBackend);
}

fn dimensions(requested: u32) -> (u32, u32) {
    (requested, requested)
}

fn kind_type(kind: IconKind) -> &'static str {
    match kind {
        IconKind::Folder => "public.folder",
        IconKind::File => "public.data",
        IconKind::Drive => "public.volume",
    }
}

fn icon_for(target: &IconTarget, requested: u32) -> Result<IconImage, alef_core::AlefError> {
    let (width, height) = dimensions(requested);
    let workspace = NSWorkspace::sharedWorkspace();
    let image = match target {
        IconTarget::Path(path) => {
            let path = NSString::from_str(&path.path);
            workspace.iconForFile(&path)
        }
        IconTarget::Extension(extension) => {
            let extension = super::icon::extension_key(&extension.extension);
            if extension.is_empty() || extension.len() > 64 {
                return Err(invalid("shell.fileIcon: invalid extension"));
            }
            let content_type = UTType::typeWithFilenameExtension(&NSString::from_str(&extension))
                .ok_or_else(|| internal("unknown filename extension"))?;
            workspace.iconForContentType(&content_type)
        }
        IconTarget::Mime(mime) => {
            let mime = mime.mime.split(';').next().unwrap_or("").trim();
            let content_type = if let Some(extension) = mime_to_extension(mime) {
                UTType::typeWithFilenameExtension(&NSString::from_str(extension))
            } else {
                UTType::typeWithMIMEType(&NSString::from_str(mime))
            }
            .ok_or_else(|| invalid("shell.fileIcon: unsupported mime type"))?;
            workspace.iconForContentType(&content_type)
        }
        IconTarget::Kind(kind) => {
            let content_type =
                UTType::typeWithIdentifier(&NSString::from_str(kind_type(kind.kind)))
                    .ok_or_else(|| internal("unknown system icon type"))?;
            workspace.iconForContentType(&content_type)
        }
    };
    // NSWorkspace's image is shared/cached. Copy it before rendering so all mutable
    // drawing state is owned and confined to this worker invocation.
    let image = image.copy();
    render_png(&image, width, height)
}

struct GraphicsContextRestore(Option<Retained<NSGraphicsContext>>);
impl Drop for GraphicsContextRestore {
    fn drop(&mut self) {
        NSGraphicsContext::setCurrentContext(self.0.as_deref());
    }
}

fn render_png(image: &NSImage, width: u32, height: u32) -> Result<IconImage, alef_core::AlefError> {
    let width = i32::try_from(width).map_err(|_| internal("icon width overflow"))?;
    let height = i32::try_from(height).map_err(|_| internal("icon height overflow"))?;
    if width <= 0 || height <= 0 || width > 256 || height > 256 {
        return Err(invalid("shell.fileIcon: invalid icon dimensions"));
    }
    // SAFETY: positive dimensions are validated and bounded above; null planes ask AppKit to allocate its own bitmap buffer. The bitmap is 8-bit, 4-sample, alpha, nonplanar RGB, with AppKit's default row/bit sizing. NSDeviceRGBColorSpace is a static color-space name whose lifetime is sufficient for this call.
    let bitmap = unsafe {
        NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(), std::ptr::null_mut(), width as isize, height as isize, 8, 4, true,
            false, &*NSDeviceRGBColorSpace, 0, 0,
        )
    }.ok_or_else(|| internal("cannot allocate bitmap representation"))?;
    let context = NSGraphicsContext::graphicsContextWithBitmapImageRep(&bitmap)
        .ok_or_else(|| internal("cannot create bitmap graphics context"))?;
    let previous = NSGraphicsContext::currentContext();
    NSGraphicsContext::setCurrentContext(Some(&context));
    let restore = GraphicsContextRestore(previous);
    let size = NSSize::new(width.into(), height.into());
    let rect = NSRect::new(NSPoint::new(0.0, 0.0), size);
    image.drawInRect_fromRect_operation_fraction(
        rect,
        NSRect::new(NSPoint::new(0.0, 0.0), image.size()),
        objc2_app_kit::NSCompositingOperation::Copy,
        1.0,
    );
    context.flushGraphics();
    drop(restore);
    // SAFETY: bitmap is live and owns the pixel buffer; the properties dictionary and PNG type are valid.
    let data = unsafe {
        bitmap.representationUsingType_properties(
            NSBitmapImageFileType::PNG,
            &NSDictionary::<NSBitmapImageRepPropertyKey, AnyObject>::new(),
        )
    }
    .ok_or_else(|| internal("AppKit could not encode PNG"))?;
    Ok(IconImage {
        width: width as u32,
        height: height as u32,
        png: data.to_vec(),
    })
}

fn invalid(message: impl Into<String>) -> alef_core::AlefError {
    alef_core::AlefError::new(alef_core::ErrorCode::InvalidArgument, message)
}
fn internal(message: impl std::fmt::Display) -> alef_core::AlefError {
    alef_core::AlefError::new(
        alef_core::ErrorCode::Internal,
        format!("shell.fileIcon: {message}"),
    )
}

#[cfg(test)]
mod tests {
    use super::super::icon::{png_dimensions, IconKind, KindTarget};
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn requested_size_is_rendered_without_resampling() {
        assert_eq!(dimensions(16), (16, 16));
        assert_eq!(dimensions(256), (256, 256));
    }

    #[test]
    fn rendered_icon_png_dimensions_match_each_requested_size() {
        let (send, receive) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let result = autoreleasepool(|_| {
                [16, 48, 256]
                    .into_iter()
                    .map(|size| {
                        icon_for(
                            &IconTarget::Kind(KindTarget {
                                kind: IconKind::Folder,
                            }),
                            size,
                        )
                        .map(|icon| (size, icon.png, icon.width, icon.height))
                    })
                    .collect::<Result<Vec<_>, _>>()
            });
            let _ = send.send(result);
        });
        let rendered = receive
            .recv_timeout(Duration::from_secs(5))
            .expect("macOS icon rendering worker exceeded five seconds")
            .expect("macOS icon rendering failed");
        worker.join().expect("macOS icon rendering worker panicked");
        for (requested, png, width, height) in rendered {
            assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
            assert_eq!(png_dimensions(&png), Some((requested, requested)));
            assert_eq!((width, height), (requested, requested));
        }
    }

    #[test]
    fn kind_targets_use_system_uniform_type_identifiers() {
        assert_eq!(kind_type(IconKind::Folder), "public.folder");
        assert_eq!(kind_type(IconKind::File), "public.data");
        assert_eq!(kind_type(IconKind::Drive), "public.volume");
    }
}
