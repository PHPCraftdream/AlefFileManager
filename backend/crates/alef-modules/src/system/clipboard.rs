// SPDX-License-Identifier: MIT OR Apache-2.0
//! `clipboard`: text, HTML and PNG images. Reading needs the `clipboard.read` permission, writing
//! none. Text, HTML and images travel as bytes (UTF-8, UTF-8, PNG) because they outgrow a JSON body.
//! The system clipboard is one [`ClipboardBackend`]; another one, kept in memory, serves runs that
//! must not touch the clipboard of the user.
use std::{
    fmt::Debug,
    io::Cursor,
    sync::{Arc, Mutex},
};

use alef_core::{
    registry::{command::Reply, dispatch::Registry},
    security::permissions::Permission,
    AlefError, ErrorCode,
};
use bytes::Bytes;
use image::{ExtendedColorType, ImageEncoder, ImageFormat, Limits};

/// The largest image the clipboard takes: 8192 by 8192 pixels, 256 MiB decoded.
const MAX_SIDE: u32 = 8192;
const MAX_DECODED: u64 = 256 * 1024 * 1024;

/// A picture of 8-bit RGBA pixels, row after row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Where the clipboard lives. Calls may block briefly; the module runs them off the async threads.
pub trait ClipboardBackend: Send + Sync + Debug {
    /// The text on the clipboard; empty when there is none.
    fn read_text(&self) -> Result<String, AlefError>;
    fn write_text(&self, text: &str) -> Result<(), AlefError>;
    /// The HTML on the clipboard; empty when there is none.
    fn read_html(&self) -> Result<String, AlefError>;
    fn write_html(&self, html: &str) -> Result<(), AlefError>;
    fn read_image(&self) -> Result<Option<Image>, AlefError>;
    fn write_image(&self, image: Image) -> Result<(), AlefError>;
}

/// The clipboard of the desktop.
#[derive(Debug, Default)]
pub struct SystemClipboard;

fn failed(error: arboard::Error) -> AlefError {
    let code = match error {
        arboard::Error::ClipboardNotSupported => ErrorCode::NotAvailable,
        arboard::Error::ClipboardOccupied => ErrorCode::Busy,
        arboard::Error::ConversionFailure => ErrorCode::InvalidArgument,
        _ => ErrorCode::Internal,
    };
    AlefError::new(code, format!("clipboard: {error}"))
}

fn open() -> Result<arboard::Clipboard, AlefError> {
    arboard::Clipboard::new().map_err(failed)
}

/// A format the clipboard does not hold reads as nothing.
fn or_nothing<T: Default>(result: Result<T, arboard::Error>) -> Result<T, AlefError> {
    match result {
        Err(arboard::Error::ContentNotAvailable) => Ok(T::default()),
        other => other.map_err(failed),
    }
}

impl ClipboardBackend for SystemClipboard {
    fn read_text(&self) -> Result<String, AlefError> {
        or_nothing(open()?.get_text())
    }

    fn write_text(&self, text: &str) -> Result<(), AlefError> {
        open()?.set_text(text).map_err(failed)
    }

    fn read_html(&self) -> Result<String, AlefError> {
        or_nothing(open()?.get().html())
    }

    fn write_html(&self, html: &str) -> Result<(), AlefError> {
        open()?.set_html(html, None::<&str>).map_err(failed)
    }

    fn read_image(&self) -> Result<Option<Image>, AlefError> {
        match open()?.get_image() {
            Ok(data) => Ok(Some(Image {
                width: u32::try_from(data.width).map_err(|_| too_big())?,
                height: u32::try_from(data.height).map_err(|_| too_big())?,
                rgba: data.bytes.into_owned(),
            })),
            Err(arboard::Error::ContentNotAvailable) => Ok(None),
            Err(error) => Err(failed(error)),
        }
    }

    fn write_image(&self, image: Image) -> Result<(), AlefError> {
        let data = arboard::ImageData {
            width: image.width as usize,
            height: image.height as usize,
            bytes: image.rgba.into(),
        };
        open()?.set_image(data).map_err(failed)
    }
}

/// A clipboard that lives in this process only. Like the real one it holds one thing at a time:
/// writing a format replaces whatever was there.
#[derive(Debug, Default)]
pub struct MemoryClipboard {
    held: Mutex<Held>,
}

#[derive(Debug, Default)]
enum Held {
    #[default]
    Nothing,
    Text(String),
    Html(String),
    Picture(Image),
}

impl MemoryClipboard {
    fn held(&self) -> std::sync::MutexGuard<'_, Held> {
        self.held.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl ClipboardBackend for MemoryClipboard {
    fn read_text(&self) -> Result<String, AlefError> {
        Ok(match &*self.held() {
            Held::Text(text) => text.clone(),
            _ => String::new(),
        })
    }

    fn write_text(&self, text: &str) -> Result<(), AlefError> {
        *self.held() = Held::Text(text.to_owned());
        Ok(())
    }

    fn read_html(&self) -> Result<String, AlefError> {
        Ok(match &*self.held() {
            Held::Html(html) => html.clone(),
            _ => String::new(),
        })
    }

    fn write_html(&self, html: &str) -> Result<(), AlefError> {
        *self.held() = Held::Html(html.to_owned());
        Ok(())
    }

    fn read_image(&self) -> Result<Option<Image>, AlefError> {
        Ok(match &*self.held() {
            Held::Picture(image) => Some(image.clone()),
            _ => None,
        })
    }

    fn write_image(&self, image: Image) -> Result<(), AlefError> {
        *self.held() = Held::Picture(image);
        Ok(())
    }
}

fn refuse(message: impl Into<String>) -> AlefError {
    AlefError::new(ErrorCode::InvalidArgument, message)
}

fn too_big() -> AlefError {
    refuse("clipboard: the image is too large")
}

/// The PNG of a picture.
pub(crate) fn encode_png(image: &Image) -> Result<Vec<u8>, AlefError> {
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(
            &image.rgba,
            image.width,
            image.height,
            ExtendedColorType::Rgba8,
        )
        .map_err(|error| AlefError::new(ErrorCode::Internal, format!("clipboard: {error}")))?;
    Ok(png)
}

/// The picture of a PNG; a PNG that would decode to more than the limits is refused unread.
pub(crate) fn decode_png(png: &[u8]) -> Result<Image, AlefError> {
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(MAX_DECODED);
    let mut reader = image::ImageReader::with_format(Cursor::new(png), ImageFormat::Png);
    reader.limits(limits);
    let decoded = reader
        .decode()
        .map_err(|error| refuse(format!("clipboard.writeImage: not a usable PNG ({error})")))?
        .into_rgba8();
    Ok(Image {
        width: decoded.width(),
        height: decoded.height(),
        rgba: decoded.into_raw(),
    })
}

fn text_of(command: &str, body: Option<&Bytes>) -> Result<String, AlefError> {
    let body = body.ok_or_else(|| refuse(format!("{command} needs its content as the body")))?;
    String::from_utf8(body.to_vec())
        .map_err(|_| refuse(format!("{command}: the content is not UTF-8 text")))
}

/// Runs a blocking call of the backend off the async threads.
async fn blocking<T: Send + 'static>(
    backend: &Arc<dyn ClipboardBackend>,
    work: impl FnOnce(&dyn ClipboardBackend) -> Result<T, AlefError> + Send + 'static,
) -> Result<T, AlefError> {
    let backend = backend.clone();
    tokio::task::spawn_blocking(move || work(backend.as_ref()))
        .await
        .map_err(|error| AlefError::new(ErrorCode::Internal, format!("clipboard: {error}")))?
}

pub(crate) fn register(
    registry: &mut Registry,
    backend: Arc<dyn ClipboardBackend>,
) -> Result<(), AlefError> {
    let this = backend.clone();
    registry
        .command::<()>("clipboard.readText")?
        .permission(Permission::ClipboardRead, |_| None)
        .handler(move |_, ()| {
            let backend = this.clone();
            async move {
                let text = blocking(&backend, |b| b.read_text()).await?;
                Ok(Reply::Bytes(Bytes::from(text)))
            }
        })?;
    let this = backend.clone();
    registry
        .command::<()>("clipboard.writeText")?
        .handler(move |ctx, ()| {
            let backend = this.clone();
            async move {
                let text = text_of("clipboard.writeText", ctx.body())?;
                blocking(&backend, move |b| b.write_text(&text)).await?;
                Ok(Reply::Json(serde_json::Value::Null))
            }
        })?;
    let this = backend.clone();
    registry
        .command::<()>("clipboard.readHtml")?
        .permission(Permission::ClipboardRead, |_| None)
        .handler(move |_, ()| {
            let backend = this.clone();
            async move {
                let html = blocking(&backend, |b| b.read_html()).await?;
                Ok(Reply::Bytes(Bytes::from(html)))
            }
        })?;
    let this = backend.clone();
    registry
        .command::<()>("clipboard.writeHtml")?
        .handler(move |ctx, ()| {
            let backend = this.clone();
            async move {
                let html = text_of("clipboard.writeHtml", ctx.body())?;
                blocking(&backend, move |b| b.write_html(&html)).await?;
                Ok(Reply::Json(serde_json::Value::Null))
            }
        })?;
    let this = backend.clone();
    registry
        .command::<()>("clipboard.readImage")?
        .permission(Permission::ClipboardRead, |_| None)
        .handler(move |_, ()| {
            let backend = this.clone();
            async move {
                match blocking(&backend, |b| b.read_image()).await? {
                    Some(image) => Ok(Reply::Bytes(Bytes::from(encode_png(&image)?))),
                    None => Ok(Reply::Json(serde_json::Value::Null)),
                }
            }
        })?;
    registry
        .command::<()>("clipboard.writeImage")?
        .handler(move |ctx, ()| {
            let backend = backend.clone();
            async move {
                let png = ctx
                    .body()
                    .ok_or_else(|| refuse("clipboard.writeImage needs the PNG as the body"))?
                    .clone();
                let image = decode_png(&png)?;
                blocking(&backend, move |b| b.write_image(image)).await?;
                Ok(Reply::Json(serde_json::Value::Null))
            }
        })
}
