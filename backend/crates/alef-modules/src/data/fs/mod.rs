// SPDX-License-Identifier: MIT OR Apache-2.0
//! `fs`: files and folders. Every path is authorized in the handler, not in the registry: a path
//! is first resolved to the entry it names (a link is followed, or not, as the command means it)
//! and only then held against the scopes of `permissions.fs.read` and `.write`, the paths the user
//! picked in a dialog, and what the user decided about the scope. Where he chose a stand-in, the
//! same command works on the place of the path in the stand-in (see `space`).
use std::{fs, future::Future, path::PathBuf};

use alef_core::{
    registry::{command::Reply, dispatch::Registry},
    security::permissions::{Permission, Reach},
    AlefError, ErrorCode,
};
use bytes::Bytes;
use serde::Deserialize;

use crate::{json, ModuleContext};

mod dto;
mod fault;
mod ops;
mod space;

pub use dto::{DirEntry, FileKind, FileStat};
use fault::{fault as io_fault, invalid};
use space::Space;

/// The most `fs.readFile` returns and `fs.writeFile` takes: more goes through `fs.open`.
pub const MAX_WHOLE_FILE: u64 = 64 * 1024 * 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PathArgs {
    path: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteArgs {
    path: String,
    #[serde(default)]
    append: bool,
    #[serde(default = "yes")]
    create: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TreeArgs {
    path: String,
    #[serde(default)]
    recursive: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TwoPaths {
    from: String,
    to: String,
}

/// Runs a blocking operation of the disk off the async threads.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, AlefError> + Send + 'static,
) -> Result<T, AlefError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| AlefError::new(ErrorCode::Internal, format!("fs: {error}")))?
}

fn null() -> Result<Reply, AlefError> {
    Ok(Reply::Json(serde_json::Value::Null))
}

pub(crate) fn register(registry: &mut Registry, context: &ModuleContext) -> Result<(), AlefError> {
    let space = Space::new(context.shadow.clone());
    let temp = context.paths.app_cache.join("tmp");

    let at = space.clone();
    registry
        .command::<PathArgs>("fs.readFile")?
        .handler(move |ctx, args| {
            let at = at.clone();
            async move {
                let place = at.place(&ctx, Permission::FsRead, &args.path, Reach::Through)?;
                let data = blocking(move || ops::read_file(&place, MAX_WHOLE_FILE)).await?;
                Ok(Reply::Bytes(Bytes::from(data)))
            }
        })?;

    let at = space.clone();
    registry
        .command::<WriteArgs>("fs.writeFile")?
        .handler(move |ctx, args| {
            let at = at.clone();
            async move {
                let place = at.place(&ctx, Permission::FsWrite, &args.path, Reach::Through)?;
                let data = ctx.body().cloned().unwrap_or_default();
                if data.len() as u64 > MAX_WHOLE_FILE {
                    return Err(invalid(
                        "more than 64 MiB: open the file with fs.open and write it in pieces",
                    ));
                }
                blocking(move || ops::write_file(&place, &data, args.append, args.create)).await?;
                null()
            }
        })?;

    for (name, follow) in [("fs.stat", true), ("fs.lstat", false)] {
        let at = space.clone();
        registry
            .command::<PathArgs>(name)?
            .handler(move |ctx, args| {
                let at = at.clone();
                async move {
                    let reach = if follow { Reach::Through } else { Reach::Entry };
                    let place = at.place(&ctx, Permission::FsRead, &args.path, reach)?;
                    json(&blocking(move || ops::stat(&place, follow)).await?)
                }
            })?;
    }

    let at = space.clone();
    registry
        .command::<PathArgs>("fs.readDir")?
        .handler(move |ctx, args| {
            let at = at.clone();
            async move {
                let place = at.place(&ctx, Permission::FsRead, &args.path, Reach::Through)?;
                json(&blocking(move || ops::read_dir(&place)).await?)
            }
        })?;

    let at = space.clone();
    registry
        .command::<PathArgs>("fs.exists")?
        .handler(move |ctx, args| {
            let at = at.clone();
            async move {
                let place = at.place(&ctx, Permission::FsRead, &args.path, Reach::Through)?;
                json(&blocking(move || ops::exists(&place)).await?)
            }
        })?;

    let at = space.clone();
    registry
        .command::<TreeArgs>("fs.mkdir")?
        .handler(move |ctx, args| {
            let at = at.clone();
            async move {
                let place = at.place(&ctx, Permission::FsWrite, &args.path, Reach::Through)?;
                blocking(move || ops::mkdir(&place, args.recursive)).await?;
                null()
            }
        })?;

    let at = space.clone();
    registry
        .command::<TreeArgs>("fs.remove")?
        .handler(move |ctx, args| {
            let at = at.clone();
            async move {
                // A link is removed, not what it leads to.
                let place = at.place(&ctx, Permission::FsWrite, &args.path, Reach::Entry)?;
                blocking(move || ops::remove(&place, args.recursive)).await?;
                null()
            }
        })?;

    let at = space.clone();
    registry
        .command::<TwoPaths>("fs.rename")?
        .handler(move |ctx, args| {
            let at = at.clone();
            async move {
                let from = at.place(&ctx, Permission::FsWrite, &args.from, Reach::Entry)?;
                let to = at.place(&ctx, Permission::FsWrite, &args.to, Reach::Entry)?;
                blocking(move || ops::rename(&from, &to)).await?;
                null()
            }
        })?;

    let at = space.clone();
    registry
        .command::<TwoPaths>("fs.copy")?
        .handler(move |ctx, args| {
            let at = at.clone();
            async move {
                let from = at.place(&ctx, Permission::FsRead, &args.from, Reach::Through)?;
                let to = at.place(&ctx, Permission::FsWrite, &args.to, Reach::Through)?;
                blocking(move || ops::copy(&from, &to)).await?;
                null()
            }
        })?;

    // A scratch file or folder of the application: nothing of the manifest names it, so the path
    // is granted to this document for as long as it lives.
    let scratch = temp.clone();
    registry
        .command::<()>("fs.tempFile")?
        .handler(move |ctx, ()| {
            let folder = scratch.clone();
            async move { scratch_path(&ctx, folder, false).await }
        })?;
    registry
        .command::<()>("fs.tempDir")?
        .handler(move |ctx, ()| {
            let folder = temp.clone();
            async move { scratch_path(&ctx, folder, true).await }
        })
}

fn scratch_path(
    ctx: &alef_core::registry::context::CallContext,
    folder: PathBuf,
    directory: bool,
) -> impl Future<Output = Result<Reply, AlefError>> {
    let grants = ctx.grants();
    async move {
        let path = blocking(move || {
            fs::create_dir_all(&folder).map_err(io_fault)?;
            let mut builder = tempfile::Builder::new();
            builder.prefix("alef-");
            let path = if directory {
                builder.tempdir_in(&folder).map_err(io_fault)?.keep()
            } else {
                let (_, path) = builder
                    .tempfile_in(&folder)
                    .map_err(io_fault)?
                    .keep()
                    .map_err(|error| io_fault(error.error))?;
                path
            };
            Ok(path)
        })
        .await?;
        grants.grant_read(&path)?;
        grants.grant_write(&path)?;
        json(&path.to_string_lossy())
    }
}
