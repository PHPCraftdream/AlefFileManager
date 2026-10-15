// SPDX-License-Identifier: MIT OR Apache-2.0
//! macOS system icons rendered by AppKit on the dedicated icon worker.
use objc2::rc::{autoreleasepool, Retained};
use objc2::{runtime::AnyObject, AllocAnyThread};
use objc2_app_kit::{
    NSBitmapImageFileType, NSBitmapImageRep, NSBitmapImageRepPropertyKey, NSDeviceRGBColorSpace,
    NSGraphicsContext, NSImage, NSWorkspace,
};
use objc2_foundation::{NSDictionary, NSPoint, NSRect, NSSize, NSString};
use objc2_uniform_type_identifiers::UTType;

use super::icon::{
    generic_fallback, mime_to_extension, IconImage, IconKind, IconTarget, JobReceiver,
};

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

pub(crate) fn worker_loop(mut receiver: JobReceiver) {
    while let Some(job) = receiver.blocking_recv() {
        if job.reply.is_closed() {
            continue;
        }
        let result = autoreleasepool(|_| {
            let result = icon_for(&job.target, job.size);
            match (&job.target, result) {
                (IconTarget::Path(_), result) => result,
                (_, Ok(image)) => Ok(image),
                (_, Err(_)) => Ok(generic_fallback()),
            }
        });
        let _ = job.reply.send(result);
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
    // SAFETY: AppKit only declares nullable planes; null asks AppKit to allocate storage. The dimensions and format are valid.
    let bitmap = unsafe {
        NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(),
            std::ptr::null_mut(),
            width.into(),
            height.into(),
            8,
            4,
            true,
            false,
            // SAFETY: NSDeviceRGBColorSpace is a static AppKit color-space name with process lifetime.
            &*NSDeviceRGBColorSpace,
            0,
            0,
        )
    }
    .ok_or_else(|| internal("cannot allocate bitmap representation"))?;
    let context = NSGraphicsContext::graphicsContextWithBitmapImageRep(&bitmap)
        .ok_or_else(|| internal("cannot create bitmap graphics context"))?;
    let previous = NSGraphicsContext::currentContext();
    NSGraphicsContext::setCurrentContext(Some(&context));
    let _restore = GraphicsContextRestore(previous);
    let size = NSSize::new(width.into(), height.into());
    let rect = NSRect::new(NSPoint::new(0.0, 0.0), size);
    image.drawInRect_fromRect_operation_fraction(
        rect,
        NSRect::new(NSPoint::new(0.0, 0.0), image.size()),
        objc2_app_kit::NSCompositingOperation::Copy,
        1.0,
    );
    context.flushGraphics();
    drop(_restore);
    let data = unsafe {
        // SAFETY: empty properties are valid for PNG output; the selected file type fully determines encoding.
        bitmap.representationUsingType_properties(
            NSBitmapImageFileType::PNG,
            &NSDictionary::<NSBitmapImageRepPropertyKey, AnyObject>::new(),
        )
    }
    .ok_or_else(|| internal("AppKit could not encode PNG"))?;
    let png = data.to_vec();
    Ok(IconImage {
        width: width as u32,
        height: height as u32,
        png,
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
    use super::*;

    #[test]
    fn requested_size_is_rendered_without_resampling() {
        assert_eq!(dimensions(16), (16, 16));
        assert_eq!(dimensions(256), (256, 256));
    }

    #[test]
    fn kind_targets_use_system_uniform_type_identifiers() {
        assert_eq!(kind_type(IconKind::Folder), "public.folder");
        assert_eq!(kind_type(IconKind::File), "public.data");
        assert_eq!(kind_type(IconKind::Drive), "public.volume");
    }
}
