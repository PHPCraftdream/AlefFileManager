// SPDX-License-Identifier: MIT OR Apache-2.0
//! `secrets`: passwords, tokens and keys an application keeps in the credential store of the system
//! (Windows Credential Manager, macOS Keychain, the Secret Service of Linux) instead of in a file
//! of its own. Every application has a namespace, `alef/<id of the application>`, put in front of the
//! service it names, so what one keeps another does not find. The manifest asks for the right
//! `secrets`; a document the user gave a stand-in keeps its secrets in the memory of the process
//! and they are gone when the run ends. A secret is bytes and travels as the body of a call, never
//! in its arguments.
use std::{
    collections::BTreeMap,
    fmt::Debug,
    sync::{Arc, Mutex},
};

use alef_core::{
    registry::{command::Reply, context::CallContext, dispatch::Registry},
    security::{consent::Decision, permissions::Permission},
    AlefError, ErrorCode,
};
use bytes::Bytes;
use serde::Deserialize;
use serde_json::json;

use crate::{json, ModuleContext};

/// The most bytes a service or an account has.
pub const MAX_NAME: usize = 128;
/// The most bytes a secret has: what Windows Credential Manager takes with a margin.
pub const MAX_SECRET: usize = 1024;

/// Where the secrets live. Calls may block; the module runs them off the async threads. The service
/// is already the one of the namespace of the application.
pub trait SecretsBackend: Send + Sync + Debug {
    /// The secret kept under `service` and `account`; `None` when there is none.
    fn get(&self, service: &str, account: &str) -> Result<Option<Vec<u8>>, AlefError>;
    fn set(&self, service: &str, account: &str, secret: &[u8]) -> Result<(), AlefError>;
    /// Forgets the secret; whether there was one.
    fn delete(&self, service: &str, account: &str) -> Result<bool, AlefError>;
}

/// Secrets in the memory of this process only: the stand-in of a document that was given one, and
/// the store of runs that must not touch the one of the user.
#[derive(Default)]
pub struct MemorySecrets {
    kept: Mutex<BTreeMap<(String, String), Vec<u8>>>,
}

impl Debug for MemorySecrets {
    /// Never shows what is kept.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MemorySecrets")
            .finish_non_exhaustive()
    }
}

impl MemorySecrets {
    fn kept(&self) -> std::sync::MutexGuard<'_, BTreeMap<(String, String), Vec<u8>>> {
        self.kept.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The services and accounts kept, in order; what a test asks to see where the secrets went.
    pub fn names(&self) -> Vec<(String, String)> {
        self.kept().keys().cloned().collect()
    }
}

impl SecretsBackend for MemorySecrets {
    fn get(&self, service: &str, account: &str) -> Result<Option<Vec<u8>>, AlefError> {
        Ok(self
            .kept()
            .get(&(service.to_owned(), account.to_owned()))
            .cloned())
    }

    fn set(&self, service: &str, account: &str, secret: &[u8]) -> Result<(), AlefError> {
        self.kept()
            .insert((service.to_owned(), account.to_owned()), secret.to_vec());
        Ok(())
    }

    fn delete(&self, service: &str, account: &str) -> Result<bool, AlefError> {
        Ok(self
            .kept()
            .remove(&(service.to_owned(), account.to_owned()))
            .is_some())
    }
}

/// The credential store of the system, opened when it is first needed and again after a failure to
/// open it (the service of a Linux desktop may start late).
#[derive(Default)]
pub struct SystemSecrets {
    store: Mutex<Option<Arc<keyring_core::CredentialStore>>>,
}

impl Debug for SystemSecrets {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SystemSecrets")
            .finish_non_exhaustive()
    }
}

#[cfg(windows)]
fn open_store() -> keyring_core::Result<Arc<keyring_core::CredentialStore>> {
    // Windows joins service and account into one name: a divider no name has (control characters
    // are refused) keeps ("a.b", "c") and ("a", "b.c") apart.
    let config =
        std::collections::HashMap::from([("divider", "\u{1f}"), ("service_no_divider", "true")]);
    Ok(windows_native_keyring_store::Store::new_with_configuration(
        &config,
    )?)
}

#[cfg(target_os = "macos")]
fn open_store() -> keyring_core::Result<Arc<keyring_core::CredentialStore>> {
    Ok(apple_native_keyring_store::keychain::Store::new()?)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn open_store() -> keyring_core::Result<Arc<keyring_core::CredentialStore>> {
    Ok(zbus_secret_service_keyring_store::Store::new()?)
}

fn unreachable_store() -> AlefError {
    AlefError::new(
        ErrorCode::NotAvailable,
        "the secret store of the system cannot be reached or is locked",
    )
}

/// What went wrong in the store, in words that tell nothing of the machine.
fn failed(error: keyring_core::Error) -> AlefError {
    use keyring_core::Error as E;
    match error {
        E::NoStorageAccess(_) | E::NoDefaultStore => unreachable_store(),
        E::TooLong(..) | E::Invalid(..) => invalid("the secret store does not take this name"),
        _ => AlefError::new(ErrorCode::Internal, "the secret store failed"),
    }
}

impl SystemSecrets {
    fn entry(&self, service: &str, account: &str) -> Result<keyring_core::Entry, AlefError> {
        let store = {
            let mut guard = self.store.lock().unwrap_or_else(|e| e.into_inner());
            match guard.as_ref() {
                Some(store) => store.clone(),
                None => {
                    let store = open_store().map_err(|_| unreachable_store())?;
                    *guard = Some(store.clone());
                    store
                }
            }
        };
        store.build(service, account, None).map_err(failed)
    }
}

impl SecretsBackend for SystemSecrets {
    fn get(&self, service: &str, account: &str) -> Result<Option<Vec<u8>>, AlefError> {
        match self.entry(service, account)?.get_secret() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring_core::Error::NoEntry) => Ok(None),
            Err(error) => Err(failed(error)),
        }
    }

    fn set(&self, service: &str, account: &str, secret: &[u8]) -> Result<(), AlefError> {
        self.entry(service, account)?
            .set_secret(secret)
            .map_err(failed)
    }

    fn delete(&self, service: &str, account: &str) -> Result<bool, AlefError> {
        match self.entry(service, account)?.delete_credential() {
            Ok(()) => Ok(true),
            Err(keyring_core::Error::NoEntry) => Ok(false),
            Err(error) => Err(failed(error)),
        }
    }
}

fn invalid(message: &str) -> AlefError {
    AlefError::new(ErrorCode::InvalidArgument, message)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Name {
    service: String,
    account: String,
}

fn check_name(what: &str, text: &str) -> Result<(), AlefError> {
    if text.is_empty() || text.len() > MAX_NAME || text.chars().any(char::is_control) {
        return Err(invalid(&format!(
            "{what} has from 1 to 128 bytes and no control characters"
        )));
    }
    Ok(())
}

/// Runs a blocking call of the backend off the async threads.
async fn blocking<T: Send + 'static>(
    backend: Arc<dyn SecretsBackend>,
    work: impl FnOnce(&dyn SecretsBackend) -> Result<T, AlefError> + Send + 'static,
) -> Result<T, AlefError> {
    tokio::task::spawn_blocking(move || work(backend.as_ref()))
        .await
        .map_err(|error| AlefError::new(ErrorCode::Internal, error.to_string()))?
}

/// The store of the user and the stand-in for a document that was given one.
struct Shelf {
    real: Arc<dyn SecretsBackend>,
    stand_in: Arc<dyn SecretsBackend>,
    /// `alef/<id of the application>`: what the services of this application begin with.
    namespace: String,
}

impl Shelf {
    /// The decision on `secrets` decides for all of them: with a stand-in, the document finds its own
    /// writes there and writes nothing to the store of the user.
    fn pick(&self, ctx: &CallContext) -> Arc<dyn SecretsBackend> {
        let decision = ctx
            .permissions
            .check(Permission::Secrets, None, &ctx.grants())
            .unwrap_or(Decision::Allow);
        if decision == Decision::Substitute {
            self.stand_in.clone()
        } else {
            self.real.clone()
        }
    }

    /// The service as the backend knows it, once the names are known to be good.
    fn names(&self, name: &Name) -> Result<(String, String), AlefError> {
        check_name("a service", &name.service)?;
        check_name("an account", &name.account)?;
        Ok((
            format!("{}/{}", self.namespace, name.service),
            name.account.clone(),
        ))
    }
}

pub(crate) fn register(registry: &mut Registry, context: &ModuleContext) -> Result<(), AlefError> {
    if context.app.id.is_empty() || context.app.id.contains('/') {
        return Err(invalid("the id of an application has no / for its secrets"));
    }
    let shelf = Arc::new(Shelf {
        real: context.backends.secrets.clone(),
        stand_in: Arc::new(MemorySecrets::default()),
        namespace: format!("alef/{}", context.app.id),
    });

    let this = shelf.clone();
    registry
        .command::<Name>("secrets.get")?
        .permission(Permission::Secrets, |_| None)
        .substitutes()
        .handler(move |ctx, name| {
            let backend = this.pick(&ctx);
            let names = this.names(&name);
            async move {
                let (service, account) = names?;
                let secret = blocking(backend, move |b| b.get(&service, &account)).await?;
                Ok(match secret {
                    Some(secret) => Reply::Bytes(Bytes::from(secret)),
                    None => Reply::Json(serde_json::Value::Null),
                })
            }
        })?;

    let this = shelf.clone();
    registry
        .command::<Name>("secrets.set")?
        .permission(Permission::Secrets, |_| None)
        .substitutes()
        .handler(move |ctx, name| {
            let backend = this.pick(&ctx);
            let names = this.names(&name);
            async move {
                let (service, account) = names?;
                let secret = ctx
                    .body()
                    .ok_or_else(|| invalid("secrets.set needs the secret as the body"))?
                    .clone();
                if secret.is_empty() || secret.len() > MAX_SECRET {
                    return Err(invalid("a secret has from 1 to 1024 bytes"));
                }
                blocking(backend, move |b| b.set(&service, &account, &secret)).await?;
                Ok(Reply::Json(serde_json::Value::Null))
            }
        })?;

    let this = shelf;
    registry
        .command::<Name>("secrets.delete")?
        .permission(Permission::Secrets, |_| None)
        .substitutes()
        .handler(move |ctx, name| {
            let backend = this.pick(&ctx);
            let names = this.names(&name);
            async move {
                let (service, account) = names?;
                let deleted = blocking(backend, move |b| b.delete(&service, &account)).await?;
                json(&json!({ "deleted": deleted }))
            }
        })
}
