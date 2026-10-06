// SPDX-License-Identifier: MIT OR Apache-2.0
use std::io;
use std::path::Path;

use fjall::{Database, Keyspace, KeyspaceCreateOptions, PersistMode};

/// Fjall handles never escape the asynchronous facade.
#[derive(Clone)]
pub struct Store {
    keyspace: Keyspace,
    database: Database,
}

impl Store {
    pub async fn open(path: impl AsRef<Path>, keyspace: impl Into<String>) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        let name = keyspace.into();
        tokio::task::spawn_blocking(move || {
            let database = Database::builder(path).open().map_err(io::Error::other)?;
            let keyspace = database
                .keyspace(&name, KeyspaceCreateOptions::default)
                .map_err(io::Error::other)?;
            Ok(Self { keyspace, database })
        })
        .await
        .map_err(io::Error::other)?
    }

    pub async fn get(&self, key: impl Into<Vec<u8>>) -> io::Result<Option<Vec<u8>>> {
        let keyspace = self.keyspace.clone();
        let key = key.into();
        tokio::task::spawn_blocking(move || {
            keyspace
                .get(key)
                .map(|value| value.map(|bytes| bytes.to_vec()))
                .map_err(io::Error::other)
        })
        .await
        .map_err(io::Error::other)?
    }

    /// Once scheduled, a write can finish after cancellation. Await flush for durability.
    pub async fn insert(
        &self,
        key: impl Into<Vec<u8>>,
        value: impl Into<Vec<u8>>,
    ) -> io::Result<()> {
        let keyspace = self.keyspace.clone();
        let key = key.into();
        let value = value.into();
        tokio::task::spawn_blocking(move || keyspace.insert(key, value).map_err(io::Error::other))
            .await
            .map_err(io::Error::other)?
    }

    /// Once scheduled, removal can finish after cancellation. Await flush for durability.
    pub async fn remove(&self, key: impl Into<Vec<u8>>) -> io::Result<()> {
        let keyspace = self.keyspace.clone();
        let key = key.into();
        tokio::task::spawn_blocking(move || keyspace.remove(key).map_err(io::Error::other))
            .await
            .map_err(io::Error::other)?
    }

    pub async fn flush(&self) -> io::Result<()> {
        let database = self.database.clone();
        tokio::task::spawn_blocking(move || {
            database
                .persist(PersistMode::SyncAll)
                .map_err(io::Error::other)
        })
        .await
        .map_err(io::Error::other)?
    }

    /// Release this owner's handles off the caller's executor.
    pub async fn close(self) -> io::Result<()> {
        tokio::task::spawn_blocking(move || {
            self.database
                .persist(PersistMode::SyncAll)
                .map_err(io::Error::other)?;
            drop(self);
            Ok(())
        })
        .await
        .map_err(io::Error::other)?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn writes_removals_and_keyspace_isolation_survive_reopen() {
        let directory = tempfile::tempdir().expect("database directory");
        let store = Store::open(directory.path(), "settings")
            .await
            .expect("open");
        assert_eq!(store.get(b"language").await.expect("missing key"), None);
        store.insert(b"language", b"he").await.expect("insert");
        store
            .insert(b"removed", b"value")
            .await
            .expect("insert disposable key");
        store.remove(b"removed").await.expect("remove");
        store.flush().await.expect("flush");
        store.close().await.expect("close");
        let store = Store::open(directory.path(), "settings")
            .await
            .expect("reopen");
        assert_eq!(
            store.get(b"language").await.expect("language"),
            Some(b"he".to_vec())
        );
        assert_eq!(store.get(b"removed").await.expect("removed key"), None);
        store.close().await.expect("close");
        let isolated = Store::open(directory.path(), "other")
            .await
            .expect("other keyspace");
        assert_eq!(isolated.get(b"language").await.expect("isolated key"), None);
        isolated.close().await.expect("close");
    }
}
