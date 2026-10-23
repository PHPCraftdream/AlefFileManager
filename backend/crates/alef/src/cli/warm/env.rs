// SPDX-License-Identifier: MIT OR Apache-2.0
//! The exact environment of a warm launch (docs/stages/m11-warm-start.md,
//! «IPC, полномочия и передача управления»): a full snapshot of the client's
//! variables, applied by the spare so that variables the client does not
//! have are removed and the rest are set exactly. The traits keep the
//! application testable without touching the real process environment.

use std::ffi::{OsStr, OsString};

use alef_broker::protocol::message::{EnvChange, OsBytes};

/// Reads the current environment; the seam the tests replace.
pub(crate) trait EnvSource {
    fn vars(&self) -> Vec<(OsString, OsString)>;
}

/// The real process environment.
pub(crate) struct ProcessEnv;

impl EnvSource for ProcessEnv {
    fn vars(&self) -> Vec<(OsString, OsString)> {
        std::env::vars_os().collect()
    }
}

/// Writes environment changes; the seam the tests replace.
pub(crate) trait EnvTarget {
    fn set(&mut self, key: OsString, value: OsString);
    fn remove(&mut self, key: &OsStr);
}

/// The real process environment.
pub(crate) struct ProcessEnvTarget;

impl EnvTarget for ProcessEnvTarget {
    fn set(&mut self, key: OsString, value: OsString) {
        std::env::set_var(key, value);
    }

    fn remove(&mut self, key: &OsStr) {
        std::env::remove_var(key);
    }
}

/// A full snapshot: every variable the client has, exactly.
pub(crate) fn snapshot(source: &dyn EnvSource) -> Vec<EnvChange> {
    source
        .vars()
        .into_iter()
        .map(|(key, value)| EnvChange {
            key: OsBytes::from_os_string(&key),
            value: Some(OsBytes::from_os_string(&value)),
        })
        .collect()
}

/// Applies the client's environment to `target` seen through `current`:
/// remove what the client does not have, set the rest exactly. A value that
/// crossed platforms cannot be applied and fails the handoff.
pub(crate) fn apply(
    current: &[(OsString, OsString)],
    changes: &[EnvChange],
    target: &mut dyn EnvTarget,
) -> Result<(), String> {
    for (key, _) in current {
        if !changes.iter().any(|change| keys_equal(&change.key, key)) {
            target.remove(key);
        }
    }
    for change in changes {
        let key = change
            .key
            .clone()
            .into_os_string()
            .map_err(|_| "an environment variable name from another platform".to_owned())?;
        match &change.value {
            Some(value) => {
                let value = value.clone().into_os_string().map_err(|_| {
                    "an environment variable value from another platform".to_owned()
                })?;
                target.set(key, value);
            }
            None => target.remove(&key),
        }
    }
    Ok(())
}

/// The client's working directory, exactly.
pub(crate) fn set_cwd(cwd: Option<&OsBytes>) -> Result<(), String> {
    let Some(cwd) = cwd else {
        return Ok(());
    };
    let path = cwd
        .clone()
        .into_os_string()
        .map_err(|_| "a working directory from another platform".to_owned())?;
    std::env::set_current_dir(&path)
        .map_err(|error| format!("the working directory of the client: {error}"))
}

fn keys_equal(change: &OsBytes, current: &OsStr) -> bool {
    OsBytes::from_os_string(current) == *change
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct FakeTarget(BTreeMap<OsString, Option<OsString>>);

    impl EnvTarget for FakeTarget {
        fn set(&mut self, key: OsString, value: OsString) {
            self.0.insert(key, Some(value));
        }
        fn remove(&mut self, key: &OsStr) {
            self.0.insert(key.to_owned(), None);
        }
    }

    fn change(key: &str, value: Option<&str>) -> EnvChange {
        EnvChange {
            key: OsBytes::from_os_string(OsStr::new(key)),
            value: value.map(|v| OsBytes::from_os_string(OsStr::new(v))),
        }
    }

    fn source(vars: &[(&str, &str)]) -> impl EnvSource + '_ {
        struct S(Vec<(OsString, OsString)>);
        impl EnvSource for S {
            fn vars(&self) -> Vec<(OsString, OsString)> {
                self.0.clone()
            }
        }
        S(vars
            .iter()
            .map(|(k, v)| (OsString::from(*k), OsString::from(*v)))
            .collect())
    }

    #[test]
    fn the_snapshot_is_exact_and_typed_per_platform() {
        let changes = snapshot(&source(&[("A", "1"), ("B", "")]));
        assert_eq!(changes.len(), 2);
        assert_eq!(changes[0].key, OsBytes::from_os_string(OsStr::new("A")));
        assert_eq!(
            changes[0].value,
            Some(OsBytes::from_os_string(OsStr::new("1")))
        );
        assert_eq!(
            changes[1].value,
            Some(OsBytes::from_os_string(OsStr::new("")))
        );
    }

    #[test]
    fn apply_removes_missing_sets_others_and_honours_removals() {
        let current = vec![
            (OsString::from("KEEP"), OsString::from("k")),
            (OsString::from("DROP"), OsString::from("d")),
            (OsString::from("SET"), OsString::from("old")),
        ];
        let changes = vec![
            change("KEEP", Some("k")),
            change("SET", Some("new")),
            change("ADDED", Some("a")),
            change("GONE", None),
        ];
        let mut target = FakeTarget::default();
        apply(&current, &changes, &mut target).unwrap();
        let get = |key: &str| target.0.get(OsStr::new(key)).cloned().flatten();
        assert_eq!(get("KEEP").as_deref(), Some(OsStr::new("k")));
        assert_eq!(get("SET").as_deref(), Some(OsStr::new("new")));
        assert_eq!(get("ADDED").as_deref(), Some(OsStr::new("a")));
        assert_eq!(get("DROP"), None, "the client does not have it");
        assert_eq!(target.0.get(OsStr::new("DROP")), Some(&None));
        assert_eq!(target.0.get(OsStr::new("GONE")), Some(&None));
    }

    #[test]
    fn a_cross_platform_value_fails_instead_of_degrading() {
        let change = EnvChange {
            #[cfg(unix)]
            key: OsBytes::Windows(vec![0x61]),
            #[cfg(windows)]
            key: OsBytes::Unix(vec![0x61]),
            value: Some(OsBytes::from_os_string(OsStr::new("x"))),
        };
        let mut target = FakeTarget::default();
        assert!(apply(&[], &[change], &mut target).is_err());
    }

    #[test]
    fn a_missing_cwd_is_not_an_error() {
        assert!(set_cwd(None).is_ok());
    }
}
