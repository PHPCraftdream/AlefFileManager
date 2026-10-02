// SPDX-License-Identifier: GPL-3.0-or-later
use std::io;
use std::path::Path;

use alef_runtime::Store;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    Ru,
    En,
    He,
}

#[derive(Clone)]
pub struct Storage(Store);

impl Storage {
    pub async fn open(path: &Path) -> io::Result<Self> {
        Store::open(path, "settings").await.map(Self)
    }

    pub async fn language(&self) -> io::Result<Language> {
        match self.0.get(b"language").await? {
            Some(value) => serde_json::from_slice(&value)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error)),
            None => Ok(Language::Ru),
        }
    }

    pub async fn set_language(&self, language: Language) -> io::Result<()> {
        let value = serde_json::to_vec(&language).map_err(io::Error::other)?;
        self.0.insert(b"language", value).await?;
        self.0.flush().await
    }
}
