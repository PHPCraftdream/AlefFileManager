// SPDX-License-Identifier: MIT OR Apache-2.0
//! Linux icons from GIO content types and installed Freedesktop icon theme assets.
use std::{
    fs,
    path::{Path, PathBuf},
};

use gdk_pixbuf::Pixbuf;
use gio::prelude::*;

use super::icon::{
    generic_fallback, mime_to_extension, rgba_to_png, IconImage, IconKind, IconTarget, JobReceiver,
};
use super::icon::{run_worker_loop, IconBackend};

const THEME_ROOTS: &[&str] = &[
    "/usr/local/share/icons",
    "/usr/share/icons",
    "/usr/local/share/pixmaps",
    "/usr/share/pixmaps",
];
struct LinuxBackend;

impl IconBackend for LinuxBackend {
    fn render(
        &mut self,
        target: &IconTarget,
        size: u32,
    ) -> Result<IconImage, alef_core::AlefError> {
        icon_for(target, size).or_else(|error| match target {
            IconTarget::Path(_) => Err(error),
            _ => Ok(generic_fallback()),
        })
    }
}

pub(crate) fn worker_loop(receiver: JobReceiver) {
    run_worker_loop(receiver, LinuxBackend);
}

#[derive(Clone, Debug)]
struct ThemeConfig {
    roots: Vec<PathBuf>,
    preferred: Option<String>,
}

impl ThemeConfig {
    fn production() -> Self {
        let home = std::env::var_os("HOME");
        let data_dirs = std::env::var_os("XDG_DATA_DIRS");
        Self {
            roots: icon_roots_with_system(home.as_deref(), data_dirs.as_deref(), THEME_ROOTS),
            preferred: preferred_theme(),
        }
    }
}

fn icon_for(target: &IconTarget, size: u32) -> Result<IconImage, alef_core::AlefError> {
    icon_for_with_config(target, size, &ThemeConfig::production())
}

fn icon_for_with_config(
    target: &IconTarget,
    size: u32,
    config: &ThemeConfig,
) -> Result<IconImage, alef_core::AlefError> {
    let icon = match target {
        IconTarget::Path(path) => {
            let file = gio::File::for_path(&path.path);
            let info = file
                .query_info(
                    "standard::icon",
                    gio::FileQueryInfoFlags::NONE,
                    gio::Cancellable::NONE,
                )
                .map_err(internal)?;
            info.icon()
                .ok_or_else(|| internal("GIO path has no icon"))?
        }
        IconTarget::Extension(extension) => {
            let filename = format!(
                "alef-icon.{}",
                super::icon::extension_key(&extension.extension)
            );
            let (content_type, _) = gio::content_type_guess(Some(filename), &[]);
            gio::content_type_get_icon(content_type.as_str())
        }
        IconTarget::Mime(mime) => {
            let mime = mime.mime.split(';').next().unwrap_or("").trim();
            let content_type = if let Some(ext) = mime_to_extension(mime) {
                gio::content_type_guess(Some(format!("alef-icon.{ext}")), &[]).0
            } else {
                gio::content_type_from_mime_type(mime)
                    .ok_or_else(|| invalid("shell.fileIcon: unsupported mime type"))?
            };
            gio::content_type_get_icon(content_type.as_str())
        }
        IconTarget::Kind(kind) => {
            let name = match kind.kind {
                IconKind::Folder => "folder",
                IconKind::File => "text-x-generic",
                IconKind::Drive => "drive-harddisk",
            };
            gio::ThemedIcon::with_default_fallbacks(name).upcast()
        }
    };
    let requested = i32::try_from(size).map_err(|_| internal("icon size overflow"))?;
    let path = match resolve_icon(&icon, size, config) {
        Some(path) => path,
        None => return Ok(generic_fallback()),
    };
    let pixbuf = match Pixbuf::from_file_at_scale(path, requested, requested, true) {
        Ok(pixbuf) => pixbuf,
        Err(_) => return Ok(generic_fallback()),
    };
    let width = u32::try_from(pixbuf.width()).map_err(|_| internal("invalid pixbuf width"))?;
    let height = u32::try_from(pixbuf.height()).map_err(|_| internal("invalid pixbuf height"))?;
    let rgba = pixbuf_to_rgba(&pixbuf)?;
    let png = rgba_to_png(width, height, &rgba)?;
    Ok(IconImage { width, height, png })
}

fn resolve_icon(icon: &gio::Icon, requested: u32, config: &ThemeConfig) -> Option<PathBuf> {
    if let Some(file_icon) = icon.downcast_ref::<gio::FileIcon>() {
        let path = file_icon.file().path()?;
        return path.is_file().then_some(path);
    }
    let themed = icon.downcast_ref::<gio::ThemedIcon>()?;
    let names = themed.names();
    for root in &config.roots {
        for name in &names {
            if let Some(path) =
                find_named_icon(root, name.as_str(), requested, config.preferred.as_deref())
            {
                return Some(path);
            }
        }
    }
    None
}

fn icon_roots_with_system(
    home: Option<&std::ffi::OsStr>,
    data_dirs: Option<&std::ffi::OsStr>,
    system_roots: &[&str],
) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(home) = home {
        let home = PathBuf::from(home);
        roots.push(home.join(".local/share/icons"));
        roots.push(home.join(".icons"));
    }
    if let Some(data_dirs) = data_dirs {
        roots.extend(std::env::split_paths(data_dirs).map(|dir| dir.join("icons")));
    }
    roots.extend(system_roots.iter().map(PathBuf::from));
    roots
}

fn find_named_icon(
    root: &Path,
    name: &str,
    requested: u32,
    preferred: Option<&str>,
) -> Option<PathBuf> {
    if !root.is_dir() {
        return None;
    }
    for theme in theme_order(root, preferred.map(str::to_owned)) {
        if let Some(path) = find_theme_icon(&root.join(theme), name, requested) {
            return Some(path);
        }
    }
    for extension in ["svg", "png", "xpm"] {
        let candidate = root.join(format!("{name}.{extension}"));
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn preferred_theme() -> Option<String> {
    let source = gio::SettingsSchemaSource::default()?;
    let schema = source.lookup("org.gnome.desktop.interface", true)?;
    if !schema.has_key("icon-theme") {
        return None;
    }
    let settings = gio::Settings::new_full(&schema, None, None);
    let theme = settings.string("icon-theme").to_string();
    (!theme.is_empty()).then_some(theme)
}

fn theme_name_is_safe(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
}

fn theme_order(root: &Path, preferred: Option<String>) -> Vec<String> {
    const MAX_THEMES: usize = 32;
    const FALLBACKS: [&str; 2] = ["Adwaita", "hicolor"];
    let mut order = Vec::new();
    if let Some(theme) = preferred.filter(|theme| theme_name_is_safe(theme)) {
        if !FALLBACKS.contains(&theme.as_str()) {
            order.push(theme);
        }
    }
    let mut pending = order.clone();
    while let Some(theme) = pending.pop() {
        let index = root.join(&theme).join("index.theme");
        let Ok(text) = fs::read_to_string(index) else {
            continue;
        };
        let mut in_icon_theme = false;
        for line in text.lines().map(str::trim) {
            if line.starts_with('[') {
                in_icon_theme = line == "[Icon Theme]";
                continue;
            }
            if in_icon_theme {
                if let Some(value) = line.strip_prefix("Inherits=") {
                    for parent in value
                        .split(',')
                        .map(str::trim)
                        .filter(|parent| theme_name_is_safe(parent) && !FALLBACKS.contains(parent))
                    {
                        if order.len() >= MAX_THEMES - FALLBACKS.len() {
                            break;
                        }
                        if !order.iter().any(|item| item == parent) {
                            order.push(parent.to_owned());
                            pending.push(parent.to_owned());
                        }
                    }
                }
            }
        }
    }
    for fallback in FALLBACKS {
        if !order.iter().any(|item| item == fallback) {
            order.push(fallback.to_owned());
        }
    }
    order.truncate(MAX_THEMES);
    order
}

fn find_theme_icon(theme: &Path, name: &str, requested: u32) -> Option<PathBuf> {
    let sizes = [16u32, 24, 32, 48, 64, 128, 256];
    let ordered_sizes = sizes
        .iter()
        .copied()
        .filter(|size| *size == requested)
        .chain(sizes.iter().copied().filter(|size| *size > requested))
        .chain(sizes.iter().copied().rev().filter(|size| *size < requested));
    const CONTEXTS: &[&str] = &[
        "mimetypes",
        "apps",
        "places",
        "devices",
        "status",
        "actions",
    ];
    for size in ordered_sizes {
        for context in CONTEXTS {
            for extension in ["png", "svg", "xpm"] {
                let candidate = theme
                    .join(format!("{size}x{size}"))
                    .join(context)
                    .join(format!("{name}.{extension}"));
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    for context in CONTEXTS {
        for extension in ["svg", "png", "xpm"] {
            let candidate = theme
                .join("scalable")
                .join(context)
                .join(format!("{name}.{extension}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixbuf_rgb_accepts_omitted_final_row_padding() {
        assert_eq!(
            pixbuf_bytes_to_rgba(&[10, 20, 30], 1, 1, 3, 4).unwrap(),
            [10, 20, 30, 255]
        );
        assert_eq!(
            pixbuf_bytes_to_rgba(&[1, 2, 3, 99, 5, 6, 7], 1, 2, 3, 4).unwrap(),
            [1, 2, 3, 255, 5, 6, 7, 255]
        );
    }

    #[test]
    fn pixbuf_conversion_rejects_short_data_and_empty_dimensions() {
        assert!(pixbuf_bytes_to_rgba(&[1, 2], 1, 1, 3, 4).is_err());
        assert!(pixbuf_bytes_to_rgba(&[], 0, 1, 3, 0).is_err());
    }

    #[test]
    fn theme_search_reaches_standard_sized_mimetype_paths_and_inheritance() {
        let root = tempfile::tempdir().expect("tempdir");
        let theme = root.path().join("Custom");
        fs::create_dir_all(theme.join("32x32/mimetypes")).expect("theme directory");
        fs::write(
            theme.join("index.theme"),
            "[Icon Theme]\nInherits=hicolor,Adwaita\n[32x32/status]\nSize=32\n",
        )
        .expect("index");
        let inherited = root.path().join("hicolor/32x32/mimetypes");
        fs::create_dir_all(&inherited).expect("inherited directory");
        fs::write(inherited.join("application-x-test.png"), b"test").expect("inherited icon");
        assert_eq!(
            find_named_icon(root.path(), "application-x-test", 24, Some("Custom")),
            Some(inherited.join("application-x-test.png"))
        );
    }

    #[test]
    fn unsafe_preferred_theme_name_is_ignored() {
        let root = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            theme_order(root.path(), Some("../outside".to_owned())),
            ["Adwaita", "hicolor"]
        );
    }

    #[test]
    fn theme_inheritance_cycles_terminate_and_theme_count_is_bounded() {
        let root = tempfile::tempdir().expect("tempdir");
        for (theme, parent) in [("A", "B"), ("B", "A,../outside")] {
            let directory = root.path().join(theme);
            fs::create_dir_all(&directory).expect("theme directory");
            fs::write(
                directory.join("index.theme"),
                format!("[Icon Theme]\nInherits={parent}\n"),
            )
            .expect("index");
        }
        assert_eq!(
            theme_order(root.path(), Some("A".to_owned())),
            ["A", "B", "Adwaita", "hicolor"]
        );

        let parents = (0..40)
            .map(|index| format!("Theme{index}"))
            .collect::<Vec<_>>()
            .join(",");
        fs::write(
            root.path().join("A/index.theme"),
            format!("[Icon Theme]\nInherits={parents}\n"),
        )
        .expect("index");
        let order = theme_order(root.path(), Some("A".to_owned()));
        assert_eq!(order.len(), 32);
        assert_eq!(order.iter().filter(|name| *name == "Adwaita").count(), 1);
        assert_eq!(order.iter().filter(|name| *name == "hicolor").count(), 1);
    }

    #[test]
    fn production_lookup_uses_injected_empty_data_root_for_generic_fallback() {
        let scratch = tempfile::tempdir().expect("scratch data root");
        let data_dirs = std::env::join_paths([scratch.path()]).expect("single data path");
        let config = ThemeConfig {
            roots: icon_roots_with_system(None, Some(data_dirs.as_os_str()), &[]),
            preferred: None,
        };
        let fallback = generic_fallback();
        let result = icon_for_with_config(
            &IconTarget::Extension(super::super::icon::ExtensionTarget {
                extension: "txt".into(),
            }),
            32,
            &config,
        )
        .expect("GIO content type resolution returns fallback");
        assert_eq!(result.png, fallback.png);
    }

    #[test]
    fn requested_size_prefers_nearest_larger_asset_then_exact_and_scalable() {
        let root = tempfile::tempdir().expect("theme root");
        for size in [16, 32, 48, 64] {
            let directory = root.path().join(format!("{size}x{size}/mimetypes"));
            fs::create_dir_all(&directory).expect("size context");
            fs::write(directory.join("sample.png"), [size as u8]).expect("asset");
        }
        assert!(find_theme_icon(root.path(), "sample", 24)
            .unwrap()
            .ends_with("32x32/mimetypes/sample.png"));
        assert_eq!(
            find_theme_icon(root.path(), "sample", 32),
            Some(root.path().join("32x32/mimetypes/sample.png"))
        );
        assert!(find_theme_icon(root.path(), "sample", 64)
            .unwrap()
            .ends_with("64x64/mimetypes/sample.png"));
        assert!(find_theme_icon(root.path(), "sample", 48)
            .unwrap()
            .ends_with("48x48/mimetypes/sample.png"));
        let scalable = root.path().join("scalable/mimetypes");
        fs::create_dir_all(&scalable).expect("scalable context");
        fs::write(scalable.join("vector.svg"), b"fixture").expect("scalable asset");
        assert_eq!(
            find_theme_icon(root.path(), "vector", 24),
            Some(scalable.join("vector.svg"))
        );
    }

    #[test]
    fn no_theme_asset_is_a_generic_fallback_for_non_paths() {
        let root = tempfile::tempdir().expect("tempdir");
        assert!(find_named_icon(root.path(), "not-installed", 32, None).is_none());
        assert!(!generic_fallback().png.is_empty());
    }
}

fn pixbuf_to_rgba(pixbuf: &Pixbuf) -> Result<Vec<u8>, alef_core::AlefError> {
    let channels =
        usize::try_from(pixbuf.n_channels()).map_err(|_| internal("invalid pixbuf channels"))?;
    if channels != 3 && channels != 4 {
        return Err(internal("unexpected pixbuf channel count"));
    }
    let width = usize::try_from(pixbuf.width()).map_err(|_| internal("invalid pixbuf width"))?;
    let height = usize::try_from(pixbuf.height()).map_err(|_| internal("invalid pixbuf height"))?;
    let stride =
        usize::try_from(pixbuf.rowstride()).map_err(|_| internal("invalid pixbuf rowstride"))?;
    let bytes = pixbuf.read_pixel_bytes();
    let raw = bytes.as_ref();
    pixbuf_bytes_to_rgba(raw, width, height, channels, stride)
}

fn pixbuf_bytes_to_rgba(
    raw: &[u8],
    width: usize,
    height: usize,
    channels: usize,
    stride: usize,
) -> Result<Vec<u8>, alef_core::AlefError> {
    if width == 0 || height == 0 {
        return Err(internal("empty pixbuf dimensions"));
    }
    if channels != 3 && channels != 4 {
        return Err(internal("unexpected pixbuf channel count"));
    }
    let minimum_stride = width
        .checked_mul(channels)
        .ok_or_else(|| internal("pixbuf row overflow"))?;
    if stride < minimum_stride {
        return Err(internal("truncated pixbuf pixels"));
    }
    let required_len = (height - 1)
        .checked_mul(stride)
        .and_then(|offset| offset.checked_add(minimum_stride))
        .ok_or_else(|| internal("pixbuf dimensions overflow"))?;
    if raw.len() < required_len {
        return Err(internal("truncated pixbuf pixels"));
    }
    let pixel_len = width
        .checked_mul(height)
        .and_then(|n| n.checked_mul(4))
        .ok_or_else(|| internal("pixbuf dimensions overflow"))?;
    let mut rgba = Vec::with_capacity(pixel_len);
    for y in 0..height {
        let start = y * stride;
        let row_len = width
            .checked_mul(channels)
            .ok_or_else(|| internal("pixbuf row overflow"))?;
        let row_end = start
            .checked_add(row_len)
            .ok_or_else(|| internal("pixbuf row overflow"))?;
        let row = raw
            .get(start..row_end)
            .ok_or_else(|| internal("truncated pixbuf pixels"))?;
        for pixel in row.chunks_exact(channels) {
            rgba.extend_from_slice(&pixel[..3]);
            rgba.push(if channels == 4 { pixel[3] } else { 255 });
        }
    }
    Ok(rgba)
}

fn invalid(message: impl Into<String>) -> alef_core::AlefError {
    alef_core::AlefError::new(alef_core::ErrorCode::InvalidArgument, message)
}

fn internal(error: impl std::fmt::Display) -> alef_core::AlefError {
    alef_core::AlefError::new(
        alef_core::ErrorCode::Internal,
        format!("shell.fileIcon: {error}"),
    )
}
