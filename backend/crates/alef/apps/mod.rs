// SPDX-License-Identifier: MIT OR Apache-2.0
use alef_core::security::{
    consent::{Consent, ConsentStore, Decision, Identity, Right, RiskLevel},
    permissions::PermissionSet,
};
use serde::{Deserialize, Serialize};
use sha2::Digest;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppEntry {
    pub identity: Identity,
    pub id: String,
    pub name: Option<String>,
    pub version: Option<String>,
    pub source: Option<String>,
    pub signer_fingerprint: Option<String>,
    pub availability: Availability,
    pub summary: Summary,
    pub rights: Vec<RightEntry>,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Availability {
    Available,
    DecisionsOnly,
    Damaged,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Summary {
    pub allow: usize,
    pub substitute: usize,
    pub deny: usize,
    pub undecided: usize,
    pub red: usize,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RightEntry {
    pub right: Right,
    pub decision: Option<Decision>,
    pub red: bool,
    pub description: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppDescription {
    pub identity: Identity,
    pub name: Option<String>,
    pub version: Option<String>,
    pub rights: Vec<RightEntry>,
}
pub struct AppsIndex<'a> {
    home: &'a Path,
    store: &'a dyn ConsentStore,
}

type Loaded = (
    Identity,
    String,
    String,
    Vec<Right>,
    std::sync::Arc<PermissionSet>,
);

impl<'a> AppsIndex<'a> {
    pub fn new(home: &'a Path, store: &'a dyn ConsentStore) -> Self {
        Self { home, store }
    }
    pub fn list(&self) -> Result<Vec<AppEntry>, String> {
        use sha2::Digest;
        let mut entries = Vec::new();
        for identity in self.store.identities().map_err(io_error)? {
            if !self
                .store
                .load(&identity)
                .map_err(io_error)?
                .is_some_and(|consent| consent.decisions().next().is_some())
            {
                continue;
            }
            if entries.iter().any(|e: &AppEntry| e.identity == identity) {
                continue;
            }
            if let Some((loaded_identity, name, version, wanted, permissions)) =
                self.resolve_identity(&identity)?
            {
                if loaded_identity == identity {
                    if identity
                        .signer
                        .as_deref()
                        .is_some_and(|s| s.starts_with("dir:"))
                    {
                        entries.push(Self::entry_loaded(
                            identity,
                            (name, version),
                            wanted,
                            &permissions,
                            (Some("dev"), Availability::Available),
                            self.store,
                        )?);
                    } else {
                        entries.push(Self::entry(
                            identity,
                            Some(name),
                            Some(version),
                            Some("installed"),
                            Availability::DecisionsOnly,
                            self.store,
                        )?);
                    }
                    continue;
                }
            }
            entries.push(Self::entry(
                identity,
                None,
                None,
                Some("decisions-only"),
                Availability::DecisionsOnly,
                self.store,
            )?);
        }
        let rows = crate::install::list_records(&self.home.join("apps"))
            .map_err(|e| io_error(e.to_string()))?;
        for row in rows {
            match row {
                crate::install::AppListing::Damaged { hash } => {
                    let identity = Identity {
                        app_id: hash.clone(),
                        signer: None,
                    };
                    entries.push(Self::entry(
                        identity,
                        None,
                        None,
                        Some("installed"),
                        Availability::Damaged,
                        self.store,
                    )?);
                }
                crate::install::AppListing::Installed(record) => {
                    let path = self
                        .home
                        .join("apps")
                        .join(format!("{:x}", sha2::Sha256::digest(record.id.as_bytes())))
                        .join(&record.package);
                    match crate::application::source::load_metadata(&path)
                        .map_err(|e| e.to_string())
                        .and_then(asked_source)
                    {
                        Ok((identity, name, version, wanted, permissions))
                            if identity.app_id == record.id =>
                        {
                            entries.retain(|e| e.identity != identity);
                            entries.push(Self::entry_loaded(
                                identity,
                                (name, version),
                                wanted,
                                &permissions,
                                (Some("installed"), Availability::Available),
                                self.store,
                            )?);
                        }
                        _ => {
                            let identity = Identity {
                                app_id: record.id.clone(),
                                signer: None,
                            };
                            entries.push(Self::entry(
                                identity,
                                Some(record.name),
                                Some(record.version),
                                Some("installed"),
                                Availability::Damaged,
                                self.store,
                            )?);
                        }
                    }
                }
            }
        }
        entries.sort_by(|a, b| {
            (&a.identity.app_id, &a.identity.signer).cmp(&(&b.identity.app_id, &b.identity.signer))
        });
        Ok(entries)
    }
    pub fn describe(&self, target: &Path) -> Result<AppDescription, String> {
        let (identity, name, version, rights, permissions) = self.load_target(target)?;
        let consent = self.store.load(&identity).map_err(io_error)?;
        Ok(AppDescription {
            identity,
            name: Some(name),
            version: Some(version),
            rights: merge_rights(rights, consent.as_ref(), Some(&permissions)),
        })
    }
    pub fn describe_identity(&self, identity: &Identity) -> Result<AppDescription, String> {
        if let Some((_, name, version, wanted, permissions)) = self.resolve_identity(identity)? {
            let consent = self.store.load(identity).map_err(io_error)?;
            return Ok(AppDescription {
                identity: identity.clone(),
                name: Some(name),
                version: Some(version),
                rights: merge_rights(wanted, consent.as_ref(), Some(&permissions)),
            });
        }
        let consent = self.store.load(identity).map_err(io_error)?;
        let rights = consent
            .as_ref()
            .map(|c| c.decisions().map(|(r, _)| r.clone()).collect())
            .unwrap_or_default();
        Ok(AppDescription {
            identity: identity.clone(),
            name: None,
            version: None,
            rights: merge_rights(rights, consent.as_ref(), None),
        })
    }
    pub fn set_identity(
        &self,
        identity: &Identity,
        right: &Right,
        decision: Decision,
        confirm_red: bool,
    ) -> Result<Identity, String> {
        let Some((_, _, _, wanted, _)) = self.resolve_identity(identity)? else {
            return Err("application source is unavailable; cannot set a right".into());
        };
        self.set_known(identity, &wanted, right, decision, confirm_red)
    }
    pub fn reset_identity(&self, identity: &Identity) -> Result<Identity, String> {
        self.store
            .save(identity, &Consent::undecided())
            .map_err(io_error)?;
        Ok(identity.clone())
    }
    pub fn set(
        &self,
        target: &Path,
        right: &Right,
        decision: Decision,
        confirm_red: bool,
    ) -> Result<Identity, String> {
        let (identity, _, _, wanted, _) = self.load_target(target)?;
        self.set_known(&identity, &wanted, right, decision, confirm_red)
    }
    fn set_known(
        &self,
        identity: &Identity,
        wanted: &[Right],
        right: &Right,
        decision: Decision,
        confirm_red: bool,
    ) -> Result<Identity, String> {
        if !wanted.contains(right) {
            return Err(format!(
                "{} does not ask for {right}; it asks for: {}",
                identity.app_id,
                wanted
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        let mut consent = self
            .store
            .load(identity)
            .map_err(io_error)?
            .unwrap_or_else(Consent::undecided);
        let existing = consent.decided(right).then(|| consent.decision(right));
        let expands = match (existing, decision) {
            (Some(Decision::Allow), Decision::Allow | Decision::Substitute | Decision::Deny) => {
                false
            }
            (
                Some(Decision::Substitute),
                Decision::Allow | Decision::Substitute | Decision::Deny,
            ) => decision == Decision::Allow,
            (Some(Decision::Deny) | None, Decision::Allow | Decision::Substitute) => true,
            (Some(Decision::Deny) | None, Decision::Deny) => false,
        };
        if right.level() == RiskLevel::Red && expands && !confirm_red {
            return Err(format!("red right {right} requires --confirm-red"));
        }
        consent.set(right.clone(), decision);
        self.store.save(identity, &consent).map_err(io_error)?;
        Ok(identity.clone())
    }
    pub fn reset(&self, target: &Path) -> Result<Identity, String> {
        let identity = self.describe(target)?.identity;
        self.reset_identity(&identity)
    }
    fn resolve_identity(&self, identity: &Identity) -> Result<Option<Loaded>, String> {
        if let Some(dir) = identity
            .signer
            .as_deref()
            .and_then(|s| s.strip_prefix("dir:"))
        {
            if let Ok(loaded) = crate::application::source::load_metadata(Path::new(dir))
                .map_err(|e| e.to_string())
                .and_then(asked_source)
            {
                if loaded.0 == *identity {
                    return Ok(Some(loaded));
                }
            }
        }
        for row in crate::install::list_records(&self.home.join("apps"))
            .map_err(|e| io_error(e.to_string()))?
        {
            if let crate::install::AppListing::Installed(r) = row {
                let path = self
                    .home
                    .join("apps")
                    .join(format!("{:x}", sha2::Sha256::digest(r.id.as_bytes())))
                    .join(r.package);
                if let Ok(loaded) = crate::application::source::load_metadata(&path)
                    .map_err(|e| e.to_string())
                    .and_then(asked_source)
                {
                    if loaded.0 == *identity {
                        return Ok(Some(loaded));
                    }
                }
            }
        }
        Ok(None)
    }
    fn load_target(&self, target: &Path) -> Result<Loaded, String> {
        match crate::install::run_target_in(self.home, target, true) {
            crate::install::RunTarget::Path(path) => {
                crate::application::source::load_metadata(&path)
                    .map_err(|e| e.to_string())
                    .and_then(asked_source)
            }
            crate::install::RunTarget::Id(id) => {
                let mut result = None;
                let mut loader = |path: &Path| match crate::application::source::load_metadata(path)
                {
                    Ok(s) => {
                        result = Some(s);
                        Ok(())
                    }
                    Err(e) => Err(e),
                };
                crate::install::run_id_in(self.home, &id, &mut loader).map_err(|e| match e {
                    crate::install::RunLaunchFailure::Resolve(e) => e.to_string(),
                    crate::install::RunLaunchFailure::Load(e) => e.to_string(),
                })?;
                asked_source(
                    result
                        .ok_or_else(|| "installed application source is unavailable".to_owned())?,
                )
            }
        }
    }
    fn entry(
        identity: Identity,
        name: Option<String>,
        version: Option<String>,
        source: Option<&str>,
        availability: Availability,
        store: &dyn ConsentStore,
    ) -> Result<AppEntry, String> {
        let consent = store.load(&identity).map_err(io_error)?;
        let rights: Vec<RightEntry> = consent
            .as_ref()
            .map(|c| {
                c.decisions()
                    .map(|(right, decision)| RightEntry {
                        right: right.clone(),
                        decision: Some(decision),
                        red: right.level() == RiskLevel::Red,
                        description: None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        let summary = summarize(&rights);
        let fingerprint = identity
            .signer
            .as_deref()
            .and_then(|s| s.strip_prefix("pkg-signed:"))
            .map(str::to_owned);
        Ok(AppEntry {
            id: identity.app_id.clone(),
            identity,
            name,
            version,
            source: source.map(str::to_owned),
            signer_fingerprint: fingerprint,
            availability,
            summary,
            rights,
        })
    }
    fn entry_loaded(
        identity: Identity,
        metadata: (String, String),
        wanted: Vec<Right>,
        permissions: &PermissionSet,
        context: (Option<&str>, Availability),
        store: &dyn ConsentStore,
    ) -> Result<AppEntry, String> {
        let (name, version) = metadata;
        let (source, availability) = context;
        let consent = store.load(&identity).map_err(io_error)?;
        let rights = merge_rights(wanted, consent.as_ref(), Some(permissions));
        let summary = summarize(&rights);
        let fingerprint = identity
            .signer
            .as_deref()
            .and_then(|s| s.strip_prefix("pkg-signed:"))
            .map(str::to_owned);
        Ok(AppEntry {
            id: identity.app_id.clone(),
            identity,
            name: Some(name),
            version: Some(version),
            source: source.map(str::to_owned),
            signer_fingerprint: fingerprint,
            availability,
            summary,
            rights,
        })
    }
}
fn io_error(error: impl std::fmt::Display) -> String {
    format!("the decisions cannot be read or written: {error}")
}
fn summarize(rights: &[RightEntry]) -> Summary {
    let mut s = Summary::default();
    for r in rights {
        match r.decision {
            Some(Decision::Allow) => s.allow += 1,
            Some(Decision::Substitute) => s.substitute += 1,
            Some(Decision::Deny) => s.deny += 1,
            None => s.undecided += 1,
        }
        if r.red {
            s.red += 1;
        }
    }
    s
}
fn merge_rights(
    mut rights: Vec<Right>,
    consent: Option<&Consent>,
    permissions: Option<&PermissionSet>,
) -> Vec<RightEntry> {
    if permissions.is_none() {
        if let Some(c) = consent {
            for (r, _) in c.decisions() {
                if !rights.contains(r) {
                    rights.push(r.clone());
                }
            }
        }
    }
    rights.sort();
    rights
        .into_iter()
        .map(|right| RightEntry {
            red: right.level() == RiskLevel::Red,
            decision: consent
                .filter(|c| c.decided(&right))
                .map(|c| c.decision(&right)),
            description: permissions.and_then(|p| p.describe(&right)),
            right,
        })
        .collect()
}

fn asked_source(source: crate::application::source::ApplicationSource) -> Result<Loaded, String> {
    match source {
        crate::application::source::ApplicationSource::Directory { dir } => {
            let manifest = crate::application::plan::load_manifest(&dir).map_err(|e| e.message)?;
            let vars =
                crate::application::plan::path_vars(&dir, &manifest.id).map_err(|e| e.message)?;
            let name = manifest.name.clone();
            let version = manifest.version.clone();
            let id = manifest.id.clone();
            let plan = crate::application::plan::make_plan(&dir, manifest, &vars)
                .map_err(|e| e.message)?;
            let identity = crate::consent::identity_of(&dir, &id).map_err(|e| e.message)?;
            Ok((
                identity,
                name,
                version,
                plan.permissions.rights(),
                plan.permissions,
            ))
        }
        crate::application::source::ApplicationSource::Package(prepared) => {
            let identity = prepared.identity();
            let name = prepared.manifest.name.clone();
            let version = prepared.manifest.version.clone();
            let vars = crate::application::plan::package_path_vars(&prepared.manifest.id)
                .map_err(|e| e.message)?;
            let plan = crate::application::plan::make_package_plan(
                prepared.package,
                &prepared.selected_root,
                prepared.manifest.clone(),
                &vars,
            )
            .map_err(|e| e.message)?;
            Ok((
                identity,
                name,
                version,
                plan.permissions.rights(),
                plan.permissions,
            ))
        }
    }
}

#[cfg(test)]
mod tests;
