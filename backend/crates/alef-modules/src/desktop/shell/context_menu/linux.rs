// SPDX-License-Identifier: MIT OR Apache-2.0
//! The `Open With` catalog of Linux and the BSDs, backed by GIO's desktop app database.
use std::{collections::BTreeMap, path::Path, thread};

use alef_core::{AlefError, ErrorCode};
use gio::prelude::*;

const MAX_APPS: usize = 128;

fn native<T: Send + 'static>(operation: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    thread::Builder::new()
        .name("alef-open-with-gio".into())
        .spawn(operation)
        .ok()?
        .join()
        .ok()
}

/// The applications the system offers for the file, as (identifier, display name).
pub(super) fn open_with_apps(path: &Path) -> Vec<(String, String)> {
    let path = path.to_owned();
    native(move || {
        let file = gio::File::for_path(path);
        let info = file
            .query_info(
                "standard::content-type",
                gio::FileQueryInfoFlags::NONE,
                gio::Cancellable::NONE,
            )
            .ok();
        let Some(content_type) = info.and_then(|info| info.content_type()) else {
            return Vec::new();
        };
        let mut apps = BTreeMap::new();
        for app in gio::AppInfo::all_for_type(&content_type)
            .into_iter()
            .take(MAX_APPS * 2)
        {
            let Some(id) = app.id().filter(|id| !id.is_empty()) else {
                continue;
            };
            let name = app.display_name();
            if !name.is_empty() {
                apps.entry(id.to_string())
                    .or_insert_with(|| name.to_string());
            }
            if apps.len() == MAX_APPS {
                break;
            }
        }
        apps.into_iter().collect()
    })
    .unwrap_or_default()
}

/// Hands the file to one application of the catalog.
pub(super) fn launch_with(path: &Path, app_id: &str) -> Result<(), AlefError> {
    let path = path.to_owned();
    let app_id = app_id.to_owned();
    native(move || {
        let file = gio::File::for_path(path);
        let info = file
            .query_info(
                "standard::content-type",
                gio::FileQueryInfoFlags::NONE,
                gio::Cancellable::NONE,
            )
            .map_err(|_| {
                AlefError::new(ErrorCode::NotFound, "file has no Open With applications")
            })?;
        let content_type = info.content_type().ok_or_else(|| {
            AlefError::new(ErrorCode::NotFound, "file has no Open With applications")
        })?;
        let app = gio::AppInfo::all_for_type(&content_type)
            .into_iter()
            .find(|app| app.id().as_deref() == Some(app_id.as_str()))
            .ok_or_else(|| {
                AlefError::new(ErrorCode::NotFound, "Open With application not found")
            })?;
        app.launch(&[file], None::<&gio::AppLaunchContext>)
            .map_err(|error| {
                AlefError::new(
                    ErrorCode::Internal,
                    format!("could not launch application: {error}"),
                )
            })
    })
    .ok_or_else(|| AlefError::new(ErrorCode::Internal, "GIO worker failed"))?
}
