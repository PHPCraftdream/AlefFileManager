// SPDX-License-Identifier: MIT OR Apache-2.0
//! Static files for `http.serve({ files })`: a folder the application may read (`permissions.fs.read`,
//! asked again for every file), and no way out of it. A folder the user gave a stand-in for has no files.
use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

use alef_core::security::permissions::{Grants, Permission, PermissionSet, Reach};
use bytes::Bytes;
use http_body_util::{channel::Channel, BodyExt, Empty};
use hyper::{
    header::{HeaderName, HeaderValue, CONTENT_LENGTH, CONTENT_TYPE},
    Method, Response, StatusCode,
};
use tokio::io::AsyncReadExt;

use crate::net::http::body::RequestBody;

/// How much of a file one piece of its body carries.
const PIECE: usize = 64 * 1024;

pub(super) struct Files {
    /// The folder as the application knows it, canonical; `None` for a folder that a stand-in replaced.
    root: Option<PathBuf>,
    permissions: Arc<PermissionSet>,
    grants: Arc<Grants>,
}

/// The media type of a file by its extension.
fn media_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("html" | "htm") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("json" | "map") => "application/json",
        Some("txt") => "text/plain; charset=utf-8",
        Some("xml") => "application/xml",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("ico") => "image/x-icon",
        Some("wasm") => "application/wasm",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("pdf") => "application/pdf",
        _ => "application/octet-stream",
    }
}

/// A path without the prefix of Windows for paths of any length (`\\?\C:\...`), which no scope knows.
pub(super) fn plain(path: PathBuf) -> PathBuf {
    match path.to_str().and_then(|text| text.strip_prefix(r"\\?\")) {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => path,
    }
}

/// A byte of a percent-encoded part of a path.
fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// The segments of the path of a URL below the folder, decoded; `None` for a path that tries to leave it
/// (a segment of dots, a separator or a drive in one) or is not text.
pub(super) fn segments(url_path: &str) -> Option<Vec<String>> {
    let mut found = Vec::new();
    for raw in url_path.split('/') {
        let bytes = raw.as_bytes();
        let mut decoded = Vec::with_capacity(bytes.len());
        let mut at = 0;
        while at < bytes.len() {
            if bytes[at] == b'%' {
                let high = hex(*bytes.get(at + 1)?)?;
                let low = hex(*bytes.get(at + 2)?)?;
                decoded.push(high << 4 | low);
                at += 3;
            } else {
                decoded.push(bytes[at]);
                at += 1;
            }
        }
        let segment = String::from_utf8(decoded).ok()?;
        if segment.is_empty() {
            continue;
        }
        if segment.contains(['/', '\\', ':', '\0']) || segment.ends_with(['.', ' ']) {
            return None;
        }
        found.push(segment);
    }
    Some(found)
}

impl Files {
    pub(super) fn new(
        root: Option<PathBuf>,
        permissions: Arc<PermissionSet>,
        grants: Arc<Grants>,
    ) -> Self {
        Self {
            root,
            permissions,
            grants,
        }
    }

    /// The file of the path of a request, if there is one the application may read.
    fn find(&self, url_path: &str) -> Option<PathBuf> {
        let root = self.root.as_ref()?;
        let mut path = root.clone();
        path.extend(segments(url_path)?);
        if path.is_dir() {
            path.push("index.html");
        }
        let canonical = plain(path.canonicalize().ok()?);
        if !canonical.starts_with(root) || !canonical.is_file() {
            return None;
        }
        let text = canonical.to_str()?;
        let allowed = self
            .permissions
            .authorize_at(Permission::FsRead, Some(text), &self.grants, Reach::Through)
            .ok()?;
        allowed.shadow.is_none().then_some(canonical)
    }

    /// The answer for a request of a file, or `None` when there is no such file here.
    pub(super) async fn answer(
        &self,
        method: &Method,
        url_path: &str,
    ) -> Option<Response<RequestBody>> {
        if method != Method::GET && method != Method::HEAD {
            return None;
        }
        let path = self.find(url_path)?;
        let mut file = tokio::fs::File::open(&path).await.ok()?;
        let length = file.metadata().await.ok()?.len();
        let builder = Response::builder()
            .status(StatusCode::OK)
            .header(CONTENT_TYPE, media_type(&path))
            .header(CONTENT_LENGTH, length)
            .header(
                HeaderName::from_static("x-content-type-options"),
                HeaderValue::from_static("nosniff"),
            );
        let body: RequestBody = if method == Method::HEAD {
            Empty::new().map_err(|never| match never {}).boxed()
        } else {
            let (mut sender, channel) = Channel::<Bytes, io::Error>::new(2);
            tokio::spawn(async move {
                let mut buffer = vec![0_u8; PIECE];
                loop {
                    match file.read(&mut buffer).await {
                        Ok(0) => return,
                        Ok(count) => {
                            if sender
                                .send_data(Bytes::copy_from_slice(&buffer[..count]))
                                .await
                                .is_err()
                            {
                                return;
                            }
                        }
                        Err(error) => {
                            sender.abort(error);
                            return;
                        }
                    }
                }
            });
            channel.boxed()
        };
        builder.body(body).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts(path: &str) -> Option<Vec<String>> {
        segments(path)
    }

    #[test]
    fn the_segments_of_a_path_are_decoded_and_empty_ones_go() {
        assert_eq!(parts("/"), Some(vec![]));
        assert_eq!(parts(""), Some(vec![]));
        assert_eq!(parts("/a/b.txt"), Some(vec!["a".into(), "b.txt".into()]));
        assert_eq!(parts("//a///b/"), Some(vec!["a".into(), "b".into()]));
        assert_eq!(parts("/a%20b/%D0%BC"), Some(vec!["a b".into(), "м".into()]));
        assert_eq!(parts("/%41"), Some(vec!["A".into()]));
        assert_eq!(parts("/%61bc"), Some(vec!["abc".into()]));
    }

    #[test]
    fn a_path_that_tries_to_leave_the_folder_is_refused_however_it_is_written() {
        for path in [
            "/..",
            "/a/../b",
            "/./a",
            "/%2e%2e/x",
            "/%2E%2E",
            "/a/%2e%2e/b",
            "/a%2fb",
            "/a%5cb",
            "/a\\b",
            "/c:/x",
            "/a%3Ab",
            "/a%00b",
            "/%zz",
            "/%4",
            "/%",
            "/%C3%28",
            "/a.",
            "/a ",
            "/%2e",
        ] {
            assert_eq!(parts(path), None, "{path}");
        }
    }

    #[test]
    fn the_prefix_of_a_long_path_goes_for_a_drive_and_for_nothing_else() {
        let same = |path: &str, expected: &str| {
            assert_eq!(
                plain(PathBuf::from(path)),
                PathBuf::from(expected),
                "{path}"
            );
        };
        same(r"\\?\C:\a\b", r"C:\a\b");
        same(r"\\?\UNC\srv\share", r"\\?\UNC\srv\share");
        same(r"C:\a", r"C:\a");
        same("/a/b", "/a/b");
    }

    #[test]
    fn the_media_type_follows_the_extension() {
        for (name, expected) in [
            ("a.html", "text/html; charset=utf-8"),
            ("a.HTM", "text/html; charset=utf-8"),
            ("a.css", "text/css; charset=utf-8"),
            ("a.js", "text/javascript; charset=utf-8"),
            ("a.mjs", "text/javascript; charset=utf-8"),
            ("a.json", "application/json"),
            ("a.txt", "text/plain; charset=utf-8"),
            ("a.svg", "image/svg+xml"),
            ("a.png", "image/png"),
            ("a.jpeg", "image/jpeg"),
            ("a.wasm", "application/wasm"),
            ("a.woff2", "font/woff2"),
            ("a.unknown", "application/octet-stream"),
            ("a", "application/octet-stream"),
        ] {
            assert_eq!(media_type(Path::new(name)), expected, "{name}");
        }
    }
}
