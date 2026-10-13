// SPDX-License-Identifier: MIT OR Apache-2.0
//! Disk and archive publication through one portable pathname boundary.
mod archive;
#[cfg(test)]
mod archive_tests;
#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;
#[cfg(unix)]
use unix as os;
#[cfg(windows)]
use windows as os;
#[cfg(test)]
mod tests;

use std::{
    collections::HashSet,
    fs::File,
    io::{self, Read},
    path::Path,
};

/// Kept separate from protocol routing so a future archive implements the same lookup contract.
pub(crate) enum Source {
    Disk(File),
    Archive(std::sync::Arc<archive::Archive>),
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, "Invalid asset pathname")
}
fn denied() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "Asset link, alias or case collision",
    )
}

fn component(name: &str) -> io::Result<()> {
    if name.is_empty()
        || name.len() > 255
        || matches!(name, "." | "..")
        || name.ends_with(['.', ' '])
        || name
            .chars()
            .any(|c| c.is_control() || "/\\:*?\"<>|".contains(c))
    {
        return Err(invalid());
    }
    let stem = name.split('.').next().unwrap().to_uppercase();
    if ["CON", "PRN", "AUX", "NUL"].contains(&stem.as_str())
        || ["COM", "LPT"].iter().any(|prefix| {
            stem.strip_prefix(prefix).is_some_and(|suffix| {
                matches!(
                    suffix,
                    "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                )
            })
        })
    {
        return Err(invalid());
    }
    Ok(())
}

/// Input is the parsed URL's pathname, NOT a URL, query, fragment or decoded path.
fn pathname(path: &str) -> io::Result<Vec<String>> {
    if path.len() > 3072 {
        return Err(invalid());
    }
    let path = path.strip_prefix('/').ok_or_else(invalid)?;
    if path.is_empty() {
        return Ok(vec!["index.html".into()]);
    }
    let mut parts = Vec::new();
    for part in path.split('/') {
        let bytes = part.as_bytes();
        let mut decoded = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%' {
                let hex = |b: u8| (b as char).to_digit(16).map(|n| n as u8);
                let a = bytes
                    .get(i + 1)
                    .copied()
                    .and_then(hex)
                    .ok_or_else(invalid)?;
                let b = bytes
                    .get(i + 2)
                    .copied()
                    .and_then(hex)
                    .ok_or_else(invalid)?;
                decoded.push(a * 16 + b);
                i += 3;
            } else {
                decoded.push(bytes[i]);
                i += 1;
            }
        }
        let name = String::from_utf8(decoded).map_err(|_| invalid())?;
        component(&name)?;
        parts.push(name);
    }
    if parts.iter().map(String::len).sum::<usize>() + parts.len() - 1 > 1024 {
        return Err(invalid());
    }
    Ok(parts)
}

/// Neither filesystem case folding nor short names select files: the stored long name must equal
/// `name` and be the only sibling that folds to it. Unrelated siblings are not this lookup's business.
fn select(stored: &[String], name: &str) -> io::Result<()> {
    let wanted = name.to_lowercase();
    let mut same = stored.iter().filter(|s| s.to_lowercase() == wanted);
    match (same.next(), same.next()) {
        (Some(only), None) if only == name => Ok(()),
        (Some(_), Some(_)) => Err(denied()),
        _ => Err(io::Error::new(
            io::ErrorKind::NotFound,
            "No exact asset long name",
        )),
    }
}
/// The public tree holds no two entries that fold to one name.
fn reject_collisions(names: &[String]) -> io::Result<()> {
    let mut folded = HashSet::new();
    match names.iter().all(|name| folded.insert(name.to_lowercase())) {
        true => Ok(()),
        false => Err(denied()),
    }
}
fn unique_names(directory: &File) -> io::Result<Vec<String>> {
    let names = os::names(directory)?;
    reject_collisions(&names)?;
    Ok(names)
}
fn child(directory: &File, name: &str, is_directory: bool) -> io::Result<File> {
    select(&os::names(directory)?, name)?;
    os::child(directory, name, is_directory)
}

#[derive(Debug)]
pub(super) struct AssetBytes {
    pub(super) bytes: Vec<u8>,
    pub(super) mime: String,
    // Retained until bridge response-body handoff, not merely worker completion.
    pub(super) permit: Option<tokio::sync::OwnedSemaphorePermit>,
}

impl Source {
    pub(super) fn prepare(input: super::AssetInput) -> io::Result<Self> {
        match input {
            super::AssetInput::Disk {
                application_dir,
                selected_root,
            } => Self::open(&application_dir, &selected_root),
            super::AssetInput::Archive {
                package,
                selected_root,
            } => archive::Archive::new(package, selected_root)
                .map(|archive| Self::Archive(std::sync::Arc::new(archive))),
        }
    }
    pub(super) fn open(application: &Path, selected: &Path) -> io::Result<Self> {
        let relative = selected.strip_prefix(application).map_err(|_| invalid())?;
        if relative.to_str().ok_or_else(invalid)?.len() > 1024 {
            return Err(invalid());
        }
        let mut directory = os::root(application)?;
        let mut count = 0;
        for part in relative.components() {
            let std::path::Component::Normal(part) = part else {
                return Err(invalid());
            };
            let name = part.to_str().ok_or_else(invalid)?;
            component(name)?;
            if count == 0 && name.eq_ignore_ascii_case("META-INF") {
                return Err(invalid());
            }
            directory = child(&directory, name, true)?;
            count += 1;
        }
        if count == 0 {
            return Err(invalid());
        }
        // Check the entire public tree before startup, including unused collision/link entries.
        audit(&directory, 0)?;
        Ok(Self::Disk(directory))
    }

    pub(super) fn read(&self, path: &str) -> io::Result<(Vec<u8>, String)> {
        let parts = pathname(path)?;
        let bytes = match self {
            Self::Archive(archive) => archive.read(&parts)?,
            Self::Disk(root) => {
                let mut directory = root.try_clone()?;
                for part in &parts[..parts.len() - 1] {
                    directory = child(&directory, part, true)?;
                }
                let mut file = child(&directory, parts.last().unwrap(), false)?;
                read_bounded(&mut file, MAX_FILE)?
            }
        };
        // pathname always yields at least one validated component, including for "/".
        let name = parts.last().unwrap();
        Ok((
            bytes,
            mime_guess::from_path(name)
                .first_or_octet_stream()
                .to_string(),
        ))
    }
}

/// Format 1 per-file ceiling; it also bounds mutable files that grow while being read.
const MAX_FILE: u64 = 512 * 1024 * 1024;

fn read_bounded(file: &mut impl Read, max: u64) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    file.by_ref().take(max + 1).read_to_end(&mut bytes)?;
    match bytes.len() as u64 > max {
        true => Err(invalid()),
        false => Ok(bytes),
    }
}

fn audit(directory: &File, depth: usize) -> io::Result<()> {
    if depth > 512 {
        return Err(invalid());
    }
    for name in unique_names(directory)? {
        component(&name)?;
        // Open without following links, then decide type from that same handle.
        let file = os::any_child(directory, &name)?;
        let metadata = file.metadata()?;
        if metadata.is_dir() {
            audit(&file, depth + 1)?;
        } else if !metadata.is_file() {
            return Err(denied());
        }
    }
    Ok(())
}

pub(super) async fn read_asset(
    source: Option<std::sync::Arc<Source>>,
    path: String,
) -> io::Result<AssetBytes> {
    let source =
        source.ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Assets disabled"))?;
    // Admission waits in a bounded queue and happens before submitting a blocking job. The
    // worker owns the permit even if its async caller is cancelled; its result keeps owning it.
    let permit = match source.as_ref() {
        Source::Archive(archive) => Some(archive.admit().await?),
        Source::Disk(_) => None,
    };
    tokio::task::spawn_blocking(move || {
        let (bytes, mime) = source.read(&path)?;
        Ok(AssetBytes {
            bytes,
            mime,
            permit,
        })
    })
    .await
    .map_err(io::Error::other)?
}
