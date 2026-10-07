// SPDX-License-Identifier: MIT OR Apache-2.0
//! `store`: the values an application keeps between runs. Every application has a database of its
//! own in its data folder, so what one stores another never sees and no right is asked for; the
//! areas an application opens are keyspaces of that database. A value is JSON.
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use alef_core::{
    registry::{command::Reply, dispatch::Registry},
    AlefError, ErrorCode,
};
use fjall::{Database, Keyspace, KeyspaceCreateOptions, PersistMode};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{json, ModuleContext};

/// The area the commands use when they name none.
const DEFAULT_AREA: &str = "default";
const MAX_AREA: usize = 64;
const MAX_KEY: usize = 1024;
/// The most one value takes once written as JSON: what one call carries; a bigger one is for `fs`
/// or `sqlite`.
pub const MAX_VALUE: usize = 256 * 1024;
/// The most `store.keys` names: more is asked for with a prefix.
pub const MAX_KEYS: usize = 100_000;

struct Opened {
    database: Database,
    areas: HashMap<String, Keyspace>,
}

/// The database of the application, opened when it is first needed.
#[derive(Clone)]
struct Shelf {
    path: PathBuf,
    opened: Arc<Mutex<Option<Opened>>>,
}

impl Shelf {
    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Opened>> {
        self.opened.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The keyspace of an area; the area, and the database with it, are made the first time.
    fn area(&self, name: &str) -> Result<Keyspace, AlefError> {
        let mut guard = self.lock();
        if guard.is_none() {
            std::fs::create_dir_all(&self.path).map_err(|_| unavailable())?;
            let database = Database::builder(&self.path).open().map_err(failed)?;
            *guard = Some(Opened {
                database,
                areas: HashMap::new(),
            });
        }
        let opened = guard.as_mut().expect("the database was opened above");
        if let Some(area) = opened.areas.get(name) {
            return Ok(area.clone());
        }
        let area = opened
            .database
            .keyspace(name, KeyspaceCreateOptions::default)
            .map_err(failed)?;
        opened.areas.insert(name.to_owned(), area.clone());
        Ok(area)
    }

    /// Everything written so far is on the disk, not only in the cache of the system.
    fn flush(&self) -> Result<(), AlefError> {
        match self.lock().as_ref() {
            Some(opened) => opened
                .database
                .persist(PersistMode::SyncAll)
                .map_err(failed),
            None => Ok(()),
        }
    }
}

fn unavailable() -> AlefError {
    AlefError::new(ErrorCode::NotAvailable, "the store cannot be opened")
}

/// What went wrong in the database, in words that tell nothing of the machine.
fn failed(error: fjall::Error) -> AlefError {
    match error {
        fjall::Error::Locked => AlefError::new(
            ErrorCode::Busy,
            "the store is in use by another run of the application",
        ),
        _ => AlefError::new(ErrorCode::Internal, "the store failed"),
    }
}

fn invalid(message: &str) -> AlefError {
    AlefError::new(ErrorCode::InvalidArgument, message)
}

fn area_name(area: Option<&str>) -> Result<&str, AlefError> {
    let name = area.unwrap_or(DEFAULT_AREA);
    let fit = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'-';
    if name.is_empty() || name.len() > MAX_AREA || !name.bytes().all(fit) {
        return Err(invalid(
            "an area is named with letters, digits, _ and - and has up to 64 of them",
        ));
    }
    Ok(name)
}

fn key_bytes(key: &str) -> Result<&[u8], AlefError> {
    if key.is_empty() || key.len() > MAX_KEY {
        return Err(invalid("a key has from 1 to 1024 bytes"));
    }
    Ok(key.as_bytes())
}

/// The keys of an area that start with `prefix`, in order; more than `limit` of them is an error.
fn list_keys(area: &Keyspace, prefix: &[u8], limit: usize) -> Result<Vec<String>, AlefError> {
    let mut keys = Vec::new();
    for item in area.prefix(prefix) {
        if keys.len() == limit {
            return Err(invalid("too many keys: ask with a prefix"));
        }
        let key = item.key().map_err(failed)?;
        keys.push(String::from_utf8_lossy(&key).into_owned());
    }
    Ok(keys)
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, AlefError> + Send + 'static,
) -> Result<T, AlefError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| AlefError::new(ErrorCode::Internal, error.to_string()))?
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AreaArgs {
    #[serde(default)]
    area: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyArgs {
    #[serde(default)]
    area: Option<String>,
    key: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SetArgs {
    #[serde(default)]
    area: Option<String>,
    key: String,
    value: Value,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeysArgs {
    #[serde(default)]
    area: Option<String>,
    #[serde(default)]
    prefix: Option<String>,
}

pub(crate) fn register(registry: &mut Registry, context: &ModuleContext) -> Result<(), AlefError> {
    let shelf = Shelf {
        path: context.paths.app_data.join("store"),
        opened: Arc::default(),
    };

    let at = shelf.clone();
    registry
        .command::<AreaArgs>("store.open")?
        .handler(move |_ctx, args| {
            let at = at.clone();
            async move {
                let name = area_name(args.area.as_deref())?.to_owned();
                blocking(move || at.area(&name).map(drop)).await?;
                Ok(Reply::Json(Value::Null))
            }
        })?;

    let at = shelf.clone();
    registry
        .command::<KeyArgs>("store.get")?
        .handler(move |_ctx, args| {
            let at = at.clone();
            async move {
                let name = area_name(args.area.as_deref())?.to_owned();
                key_bytes(&args.key)?;
                let found =
                    blocking(move || at.area(&name)?.get(args.key.as_bytes()).map_err(failed))
                        .await?;
                // A stored `null` is a value; nothing stored is an answer without `value`.
                match found {
                    None => json(&json!({})),
                    Some(bytes) => {
                        let value: Value = serde_json::from_slice(&bytes).map_err(|_| {
                            AlefError::new(ErrorCode::Internal, "the stored value is damaged")
                        })?;
                        json(&json!({ "value": value }))
                    }
                }
            }
        })?;

    let at = shelf.clone();
    registry
        .command::<SetArgs>("store.set")?
        .handler(move |_ctx, args| {
            let at = at.clone();
            async move {
                let name = area_name(args.area.as_deref())?.to_owned();
                key_bytes(&args.key)?;
                let bytes = serde_json::to_vec(&args.value)
                    .map_err(|_| invalid("the value cannot be written as JSON"))?;
                if bytes.len() > MAX_VALUE {
                    return Err(invalid("a value is up to 256 KiB"));
                }
                blocking(move || {
                    at.area(&name)?
                        .insert(args.key.as_bytes(), bytes)
                        .map_err(failed)
                })
                .await?;
                Ok(Reply::Json(Value::Null))
            }
        })?;

    let at = shelf.clone();
    registry
        .command::<KeyArgs>("store.delete")?
        .handler(move |_ctx, args| {
            let at = at.clone();
            async move {
                let name = area_name(args.area.as_deref())?.to_owned();
                key_bytes(&args.key)?;
                blocking(move || at.area(&name)?.remove(args.key.as_bytes()).map_err(failed))
                    .await?;
                Ok(Reply::Json(Value::Null))
            }
        })?;

    let at = shelf.clone();
    registry
        .command::<KeysArgs>("store.keys")?
        .handler(move |_ctx, args| {
            let at = at.clone();
            async move {
                let name = area_name(args.area.as_deref())?.to_owned();
                let prefix = args.prefix.unwrap_or_default();
                if prefix.len() > MAX_KEY {
                    return Err(invalid("a prefix has up to 1024 bytes"));
                }
                let keys =
                    blocking(move || list_keys(&at.area(&name)?, prefix.as_bytes(), MAX_KEYS))
                        .await?;
                json(&keys)
            }
        })?;

    let at = shelf;
    registry
        .command::<()>("store.flush")?
        .handler(move |_ctx, ()| {
            let at = at.clone();
            async move {
                blocking(move || at.flush()).await?;
                Ok(Reply::Json(Value::Null))
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_area_and_a_key_are_named_within_limits() {
        assert_eq!(area_name(None).unwrap(), "default");
        assert_eq!(area_name(Some("a-b_9")).unwrap(), "a-b_9");
        for bad in ["", "a b", "a/b", "..", "é", &"a".repeat(65)] {
            assert_eq!(
                area_name(Some(bad)).unwrap_err().code,
                ErrorCode::InvalidArgument,
                "{bad:?}"
            );
        }
        assert!(key_bytes("k").is_ok());
        assert!(key_bytes(&"k".repeat(1024)).is_ok());
        for bad in ["", &"k".repeat(1025)] {
            assert_eq!(key_bytes(bad).unwrap_err().code, ErrorCode::InvalidArgument);
        }
    }

    #[test]
    fn more_keys_than_the_limit_are_an_error_and_a_prefix_is_the_way_out() {
        let folder = tempfile::tempdir().unwrap();
        let database = Database::builder(folder.path()).open().unwrap();
        let area = database
            .keyspace("keys", KeyspaceCreateOptions::default)
            .unwrap();
        for index in 0..12 {
            let prefix = if index < 4 { "a" } else { "b" };
            area.insert(format!("{prefix}{index:02}"), "1").unwrap();
        }
        assert_eq!(list_keys(&area, b"", 12).unwrap().len(), 12);
        let error = list_keys(&area, b"", 11).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument);
        assert_eq!(list_keys(&area, b"a", 11).unwrap().len(), 4);
        assert_eq!(list_keys(&area, b"b", 8).unwrap().len(), 8);
        assert!(list_keys(&area, b"c", 0).unwrap().is_empty());
    }
}
