// SPDX-License-Identifier: MIT OR Apache-2.0
//! `clipboard` through the registry: the permission to read, what is written and read back, and
//! what a bad image or text does. The last test uses the real clipboard and runs only on request
//! (`ALEF_TEST_DESKTOP=1 cargo test -p alef-modules --test clipboard -- --ignored`; CI does on its
//! clean runners).

use std::io::Cursor;

use crate::common::{desktop_asked, Fixture};
use alef_core::{registry::command::Reply, security::permissions::Permission, ErrorCode};
use alef_modules::{ClipboardBackend, Image, SystemClipboard};
use bytes::Bytes;
use image::{ImageFormat, RgbaImage};
use serde_json::Value;

const MANIFEST: &str = include_str!("../fixtures/app.ktav");

fn reading() -> String {
    let allowed = MANIFEST
        .replace('\r', "")
        .replace("read: false", "read: true");
    assert!(allowed.contains("read: true"));
    allowed
}

fn png_of(width: u32, height: u32, pixel: impl Fn(u32, u32) -> [u8; 4]) -> Bytes {
    let mut picture = RgbaImage::new(width, height);
    for (x, y, rgba) in picture.enumerate_pixels_mut() {
        rgba.0 = pixel(x, y);
    }
    let mut png = Vec::new();
    picture
        .write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
        .expect("a PNG");
    Bytes::from(png)
}

async fn bytes_of(fixture: &Fixture, command: &str) -> Bytes {
    match fixture
        .call_reply(command, Value::Null, None)
        .await
        .expect(command)
    {
        Reply::Bytes(bytes) => bytes,
        other => panic!("{command}: expected bytes, got {other:?}"),
    }
}

async fn write(
    fixture: &Fixture,
    command: &str,
    body: &[u8],
) -> Result<Reply, alef_core::AlefError> {
    fixture
        .call_reply(command, Value::Null, Some(Bytes::copy_from_slice(body)))
        .await
}

#[tokio::test]
async fn text_goes_in_and_comes_out_whatever_its_size_or_script() {
    let fixture = Fixture::new(Some(&reading()), &[]).await;
    for text in [
        "plain".to_owned(),
        "Привет, мир — שלום 🙂".to_owned(),
        String::new(),
        "x".repeat(1024 * 1024),
    ] {
        write(&fixture, "clipboard.writeText", text.as_bytes())
            .await
            .expect("write");
        assert_eq!(fixture.clipboard.read_text().unwrap(), text);
        assert_eq!(
            bytes_of(&fixture, "clipboard.readText").await,
            text.as_bytes()
        );
    }
}

#[tokio::test]
async fn html_goes_in_and_comes_out_and_text_and_html_replace_each_other() {
    let fixture = Fixture::new(Some(&reading()), &[]).await;
    write(&fixture, "clipboard.writeHtml", b"<b>bold</b>")
        .await
        .unwrap();
    assert_eq!(
        bytes_of(&fixture, "clipboard.readHtml").await,
        "<b>bold</b>".as_bytes()
    );
    assert!(
        bytes_of(&fixture, "clipboard.readText").await.is_empty(),
        "the clipboard holds one thing: HTML, not text"
    );
    write(&fixture, "clipboard.writeText", b"text")
        .await
        .unwrap();
    assert!(bytes_of(&fixture, "clipboard.readHtml").await.is_empty());
    assert_eq!(
        bytes_of(&fixture, "clipboard.readText").await,
        "text".as_bytes()
    );
}

#[tokio::test]
async fn an_empty_clipboard_reads_as_nothing() {
    let fixture = Fixture::new(Some(&reading()), &[]).await;
    assert!(bytes_of(&fixture, "clipboard.readText").await.is_empty());
    assert!(bytes_of(&fixture, "clipboard.readHtml").await.is_empty());
    let image = fixture
        .call_reply("clipboard.readImage", Value::Null, None)
        .await
        .expect("read");
    assert!(matches!(image, Reply::Json(Value::Null)), "{image:?}");
}

#[tokio::test]
async fn reading_needs_the_permission_and_writing_does_not() {
    let fixture = Fixture::new(None, &[]).await;
    for command in [
        "clipboard.readText",
        "clipboard.readHtml",
        "clipboard.readImage",
    ] {
        let error = fixture
            .call_reply(command, Value::Null, None)
            .await
            .expect_err(command);
        assert_eq!(error.code, ErrorCode::PermissionDenied, "{command}");
        assert_eq!(
            error.details,
            Some(serde_json::json!({ "permission": Permission::ClipboardRead.name() }))
        );
    }
    write(&fixture, "clipboard.writeText", b"copied")
        .await
        .expect("write");
    assert_eq!(fixture.clipboard.read_text().unwrap(), "copied");
    let denied = fixture
        .call_reply("clipboard.readText", Value::Null, None)
        .await
        .unwrap_err();
    assert_eq!(
        denied.code,
        ErrorCode::PermissionDenied,
        "writing did not open reading"
    );
}

#[tokio::test]
async fn an_image_makes_the_round_trip_pixel_for_pixel() {
    let fixture = Fixture::new(Some(&reading()), &[]).await;
    let png = png_of(3, 2, |x, y| {
        [x as u8 * 80, y as u8 * 120, 7, 255 - x as u8 * 50]
    });
    write(&fixture, "clipboard.writeImage", &png)
        .await
        .expect("write");
    let held = fixture.clipboard.read_image().unwrap().expect("an image");
    assert_eq!((held.width, held.height), (3, 2));
    assert_eq!(&held.rgba[..4], &[0, 0, 7, 255]);
    assert_eq!(&held.rgba[4..8], &[80, 0, 7, 205]);
    let back = bytes_of(&fixture, "clipboard.readImage").await;
    let decoded = image::load_from_memory_with_format(&back, ImageFormat::Png)
        .expect("a PNG comes back")
        .into_rgba8();
    assert_eq!((decoded.width(), decoded.height()), (3, 2));
    assert_eq!(decoded.into_raw(), held.rgba);
}

#[tokio::test]
async fn an_image_replaces_the_text_and_the_text_replaces_the_image() {
    let fixture = Fixture::new(Some(&reading()), &[]).await;
    write(&fixture, "clipboard.writeText", b"text")
        .await
        .unwrap();
    write(
        &fixture,
        "clipboard.writeImage",
        &png_of(1, 1, |_, _| [1, 2, 3, 4]),
    )
    .await
    .unwrap();
    assert!(bytes_of(&fixture, "clipboard.readText").await.is_empty());
    write(&fixture, "clipboard.writeText", b"text")
        .await
        .unwrap();
    let gone = fixture
        .call_reply("clipboard.readImage", Value::Null, None)
        .await
        .unwrap();
    assert!(matches!(gone, Reply::Json(Value::Null)));
}

#[tokio::test]
async fn an_image_that_is_not_a_usable_png_is_refused_and_leaves_the_clipboard_alone() {
    let fixture = Fixture::new(Some(&reading()), &[]).await;
    write(&fixture, "clipboard.writeText", b"kept")
        .await
        .unwrap();
    let too_wide = png_of(8193, 1, |_, _| [0, 0, 0, 255]);
    let jpeg_like = [0xFF, 0xD8, 0xFF, 0xE0, 0, 0x10];
    for (name, body) in [
        ("garbage", Bytes::from_static(b"not an image at all")),
        ("empty", Bytes::new()),
        ("a JPEG signature", Bytes::copy_from_slice(&jpeg_like)),
        ("wider than the limit", too_wide),
        (
            "a truncated PNG",
            png_of(4, 4, |_, _| [9, 9, 9, 9]).slice(..20),
        ),
    ] {
        let error = write(&fixture, "clipboard.writeImage", &body)
            .await
            .expect_err(name);
        assert_eq!(error.code, ErrorCode::InvalidArgument, "{name}");
    }
    assert_eq!(fixture.clipboard.read_text().unwrap(), "kept");
    let widest = png_of(8192, 1, |x, _| [x as u8, 0, 0, 255]);
    write(&fixture, "clipboard.writeImage", &widest)
        .await
        .expect("the limit itself is allowed");
}

#[tokio::test]
async fn a_write_needs_its_content_as_the_body_and_text_must_be_utf8() {
    let fixture = Fixture::new(Some(&reading()), &[]).await;
    write(&fixture, "clipboard.writeText", b"kept")
        .await
        .unwrap();
    for command in [
        "clipboard.writeText",
        "clipboard.writeHtml",
        "clipboard.writeImage",
    ] {
        let error = fixture
            .call_reply(command, Value::Null, None)
            .await
            .expect_err(command);
        assert_eq!(
            error.code,
            ErrorCode::InvalidArgument,
            "{command} without a body"
        );
    }
    for command in ["clipboard.writeText", "clipboard.writeHtml"] {
        let error = write(&fixture, command, &[0x66, 0xFF, 0xFE, 0x6F])
            .await
            .expect_err(command);
        assert_eq!(
            error.code,
            ErrorCode::InvalidArgument,
            "{command} with bytes that are not UTF-8"
        );
    }
    assert_eq!(fixture.clipboard.read_text().unwrap(), "kept");
}

#[tokio::test]
async fn the_memory_clipboard_is_what_a_pretending_run_gives() {
    let backends = alef_modules::Backends::pretending(None, None);
    backends.clipboard.write_text("only here").unwrap();
    assert_eq!(backends.clipboard.read_text().unwrap(), "only here");
    backends
        .clipboard
        .write_image(Image {
            width: 1,
            height: 1,
            rgba: vec![1, 2, 3, 4],
        })
        .unwrap();
    assert_eq!(backends.clipboard.read_text().unwrap(), "");
}

#[test]
#[ignore = "changes the clipboard of the desktop"]
fn the_system_clipboard_keeps_text_html_and_pictures() {
    if !desktop_asked() {
        eprintln!("skipped: ALEF_TEST_DESKTOP=1 allows the test to use the clipboard");
        return;
    }
    let clipboard = SystemClipboard::default();
    if clipboard.read_image().ok().flatten().is_some() {
        eprintln!("skipped: the clipboard holds a picture, which this test would lose");
        return;
    }
    let before = clipboard.read_text().unwrap_or_default();

    let text = "Alef: тест 🙂 — שלום";
    clipboard.write_text(text).expect("write text");
    assert_eq!(clipboard.read_text().expect("read text"), text);

    clipboard
        .write_html("<b>Alef</b> bold")
        .expect("write html");
    let html = clipboard.read_html().expect("read html");
    assert!(html.contains("<b>Alef</b> bold"), "{html}");

    let picture = Image {
        width: 2,
        height: 2,
        rgba: vec![
            255, 0, 0, 255, 0, 255, 0, 255, //
            0, 0, 255, 255, 10, 20, 30, 255,
        ],
    };
    clipboard.write_image(picture.clone()).expect("write image");
    let back = clipboard
        .read_image()
        .expect("read image")
        .expect("a picture is there");
    assert_eq!(back, picture);

    if before.is_empty() {
        clipboard.write_text("").ok();
    } else {
        clipboard.write_text(&before).expect("restore the text");
    }
}
