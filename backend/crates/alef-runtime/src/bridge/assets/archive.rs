// SPDX-License-Identifier: MIT OR Apache-2.0
//! Exact publication index over one owned, verified archive snapshot.
use super::{component, denied, invalid};
use alef_package::Package;
use std::{
    collections::{HashMap, HashSet},
    io::{self, Cursor},
    sync::{Arc, Mutex},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Reads that run at once; each may hold up to `max_bytes` until its response is handed over.
pub(super) const RUNNING: usize = 4;
/// Requests that wait for a running read; a page loads dozens of modules in one burst.
const WAITING: usize = 256;

pub(crate) struct Archive {
    package: Mutex<Package<Cursor<Vec<u8>>>>,
    root: String,
    public_names: HashSet<String>,
    admissions: Arc<Semaphore>,
    places: Arc<Semaphore>,
    max_bytes: u64,
    #[cfg(test)]
    admitted: tokio::sync::Notify,
}

impl Archive {
    pub(super) fn new(package: Package<Cursor<Vec<u8>>>, root: String) -> io::Result<Self> {
        // The manifest validator is private to core. Apply the same Format 1 root alphabet
        // here, plus the existing portable component validator; no URL decoding of roots.
        if root.len() > 1024 {
            return Err(invalid());
        }
        for (index, part) in root.split('/').enumerate() {
            component(part)?;
            if !part
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-@+".contains(&b))
                || (index == 0 && part.eq_ignore_ascii_case("META-INF"))
            {
                return Err(invalid());
            }
        }
        let prefix = format!("{root}/");
        let mut public_names = HashSet::new();
        let mut siblings = HashMap::<(String, String), String>::new();
        for name in package.names().filter(|name| name.starts_with(&prefix)) {
            let relative = name.strip_prefix(&prefix).expect("exact prefix");
            let mut parent = String::new();
            for part in relative.split('/') {
                component(part)?;
                let key = (parent.clone(), part.to_lowercase());
                if let Some(previous) = siblings.insert(key, part.to_owned()) {
                    if previous != part {
                        return Err(denied());
                    }
                }
                parent.push_str(part);
                parent.push('/');
            }
            public_names.insert(name.to_owned());
        }
        if public_names.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "Missing archive asset root",
            ));
        }
        Ok(Self {
            package: Mutex::new(package),
            root,
            public_names,
            admissions: Arc::new(Semaphore::new(RUNNING)),
            places: Arc::new(Semaphore::new(WAITING)),
            max_bytes: 64 * 1024 * 1024,
            #[cfg(test)]
            admitted: tokio::sync::Notify::new(),
        })
    }

    pub(super) async fn admit(&self) -> io::Result<OwnedSemaphorePermit> {
        let permit = match self.admissions.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                let _place = self.places.clone().try_acquire_owned().map_err(|_| {
                    io::Error::new(io::ErrorKind::WouldBlock, "Archive asset admission limit")
                })?;
                self.admissions
                    .clone()
                    .acquire_owned()
                    .await
                    .map_err(io::Error::other)?
            }
        };
        #[cfg(test)]
        self.admitted.notify_one();
        Ok(permit)
    }

    pub(super) fn read(&self, parts: &[String]) -> io::Result<Vec<u8>> {
        let name = format!("{}/{}", self.root, parts.join("/"));
        if !self.public_names.contains(&name) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "No exact public archive entry",
            ));
        }
        self.package
            .lock()
            .map_err(|_| io::Error::other("Archive asset mutex poisoned"))?
            .read_limited(&name, self.max_bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }
    #[cfg(test)]
    pub(super) fn set_limit(&mut self, max_bytes: u64) {
        self.max_bytes = max_bytes;
    }
    #[cfg(test)]
    pub(super) fn hold_package(
        self: &std::sync::Arc<Self>,
    ) -> (std::sync::mpsc::Receiver<()>, std::sync::mpsc::Sender<()>) {
        let archive = self.clone();
        let (locked_tx, locked_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        // The guard lives on a blocking worker, never across an await of the test.
        tokio::task::spawn_blocking(move || {
            let _guard = archive.package.lock().unwrap();
            locked_tx.send(()).unwrap();
            let _ = release_rx.recv();
        });
        (locked_rx, release_tx)
    }
    #[cfg(test)]
    pub(super) async fn admitted(&self) {
        self.admitted.notified().await;
    }
    #[cfg(test)]
    pub(super) async fn drained(&self) {
        let _all = self
            .admissions
            .clone()
            .acquire_many_owned(RUNNING as u32)
            .await
            .unwrap();
    }
    #[cfg(test)]
    pub(super) fn queued(&self) -> usize {
        WAITING - self.places.available_permits()
    }
    #[cfg(test)]
    pub(super) fn free_admissions(&self) -> usize {
        self.admissions.available_permits()
    }
}
