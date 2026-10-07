// SPDX-License-Identifier: MIT OR Apache-2.0
//! What `fs` tells about a file or a folder.
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "modules.ts")]
pub enum FileKind {
    File,
    Dir,
    Symlink,
    Other,
}

/// `fs.stat` and `fs.lstat`.
#[derive(Debug, Clone, PartialEq, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "modules.ts")]
pub struct FileStat {
    pub kind: FileKind,
    /// Bytes; 0 for a folder.
    #[ts(type = "number")]
    pub size: u64,
    /// Milliseconds since the Unix epoch; absent where the file system keeps no such time.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub modified: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub accessed: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub created: Option<f64>,
    pub readonly: bool,
}

/// One entry of `fs.readDir`: the entry itself, a link is not followed.
#[derive(Debug, Clone, PartialEq, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "modules.ts")]
pub struct DirEntry {
    pub name: String,
    /// The path of the entry as the application names it.
    pub path: String,
    pub kind: FileKind,
    #[ts(type = "number")]
    pub size: u64,
}
