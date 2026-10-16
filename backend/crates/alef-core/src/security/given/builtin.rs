// SPDX-License-Identifier: MIT OR Apache-2.0
//! Authorization for capabilities delegated to AI builtins.
use super::{
    super::manifest::{AiBuiltin, AiPermissions},
    *,
};

#[derive(Debug, Clone)]
pub(super) struct BuiltinScope {
    pub(super) name: String,
    pub(super) raw: String,
    pub(super) pattern: PathPattern,
}

pub(super) fn parse(
    ai: Option<&AiPermissions>,
    vars: &PathVars,
) -> Result<Vec<BuiltinScope>, AlefError> {
    ai.into_iter()
        .flat_map(|ai| &ai.builtin)
        .flat_map(|builtin| {
            builtin
                .scopes
                .iter()
                .map(move |scope| (&builtin.name, scope))
        })
        .map(|(name, raw)| {
            Ok(BuiltinScope {
                name: name.clone(),
                raw: raw.clone(),
                pattern: super::path_pattern(raw, vars)?,
            })
        })
        .collect()
}

pub(super) fn rights<'a>(
    ai: Option<&'a AiPermissions>,
    scopes: &'a [BuiltinScope],
) -> impl Iterator<Item = Right> + 'a {
    let scoped = scopes
        .iter()
        .map(|scope| Right::scoped(&format!("ai.builtin.{}", scope.name), &scope.raw));
    let plain = ai
        .into_iter()
        .flat_map(|ai| &ai.builtin)
        .filter(|builtin| execution_name(&builtin.name))
        .map(|builtin| Right::plain(&format!("ai.builtin.{}", builtin.name)));
    scoped.chain(plain)
}

fn execution_name(name: &str) -> bool {
    matches!(name, "exec" | "bash")
}

pub(super) fn check(
    ai: Option<&AiPermissions>,
    scopes: &[BuiltinScope],
    consent: &Consent,
    name: &str,
    scope: Option<&str>,
) -> Result<Decision, AlefError> {
    if execution_name(name) {
        if scope.is_some()
            || !ai.is_some_and(|ai| ai.builtin.iter().any(|builtin| builtin.name == name))
        {
            return Err(refusal_builtin(name));
        }
        let decision = consent.decision(&Right::plain(&format!("ai.builtin.{name}")));
        return (decision != Decision::Deny)
            .then_some(decision)
            .ok_or_else(|| refusal_builtin(name));
    }
    let Some(scope) = scope else {
        return Err(refusal_builtin(name));
    };
    let entry = scopes
        .iter()
        .find(|entry| entry.name == name && entry.raw == scope)
        .ok_or_else(|| refusal_builtin(name))?;
    let decision = consent.decision(&Right::scoped(&format!("ai.builtin.{name}"), &entry.raw));
    (decision != Decision::Deny)
        .then_some(decision)
        .ok_or_else(|| refusal_builtin(name))
}

fn refusal_builtin(name: &str) -> AlefError {
    AlefError::new(ErrorCode::PermissionDenied, "permission denied")
        .with_details(json!({"permission": format!("ai.builtin.{name}")}))
}

pub(super) fn authorize_path(
    set: &PermissionSet,
    name: &str,
    target: &str,
) -> Result<Authorized, AlefError> {
    let permission = format!("ai.builtin.{name}");
    let resolved = canonical(Path::new(target)).ok_or_else(|| refusal_builtin(name))?;
    let components = parts(&resolved);
    if set.is_protected(&components) {
        return Err(refusal_builtin(name));
    }
    let mut matches = set
        .builtin
        .iter()
        .filter(|entry| entry.name == name && entry.pattern.matches_canonical(&components))
        .map(|entry| {
            (
                set.decided_by_user()
                    .decision(&Right::scoped(&permission, &entry.raw)),
                entry,
            )
        });
    let Some((decision, _)) = matches.next().map(|first| {
        matches.fold(first, |best, next| {
            if best.0.stricter(next.0) == best.0 {
                best
            } else {
                next
            }
        })
    }) else {
        return Err(refusal_builtin(name));
    };
    if decision == Decision::Deny {
        return Err(refusal_builtin(name));
    }
    Ok(Authorized {
        path: resolved,
        decision,
        shadow: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(root: &Path) -> PermissionSet {
        let policy: Permissions = serde_json::from_value(json!({
            "fs": {"read": [], "write": []}, "cli": {"exec": []},
            "net": {"http": [], "socket": []}, "shell": {"openExternal": []},
            "clipboard": {"read": false}, "shortcut": {"global": false},
            "secrets": false, "app": {"env": []},
            "ai": {"roles": [], "tools": [], "builtin": [
                {"name":"fs.read","scopes":[format!("{}/allowed/**", root.display()), format!("{}/allowed/private/**", root.display())]},
                {"name":"exec","scopes":[]}
            ]}
        })).expect("policy");
        let vars = PathVars {
            app_data: root.into(),
            app_config: root.into(),
            app_cache: root.into(),
            home: root.into(),
            documents: root.into(),
            downloads: root.into(),
            desktop: root.into(),
            temp: root.into(),
            app: Some(root.into()),
        };
        PermissionSet::from_manifest(&policy, &vars).expect("permissions")
    }

    #[test]
    fn scoped_rights_allow_inside_and_deny_outside() {
        let root = tempfile::tempdir().expect("tempdir");
        let set = set(root.path());
        assert_eq!(
            set.check_ai_builtin(
                "fs.read",
                Some(&format!("{}/allowed/**", root.path().display()))
            )
            .expect("scoped right"),
            Decision::Allow
        );
        assert!(set
            .authorize_ai_builtin_path(
                "fs.read",
                root.path().join("allowed/file").to_str().expect("path")
            )
            .is_ok());
        assert!(set
            .authorize_ai_builtin_path(
                "fs.read",
                root.path().join("other/file").to_str().expect("path")
            )
            .is_err());
    }

    #[test]
    fn strictest_overlap_and_protected_paths_win() {
        let root = tempfile::tempdir().expect("tempdir");
        let path = root.path().join("allowed/private/file");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("directory");
        std::fs::write(&path, b"").expect("file");
        let allowed = set(root.path());
        assert!(allowed
            .authorize_ai_builtin_path("fs.read", path.to_str().expect("path"))
            .is_ok());
        let denied = allowed.clone().with_consent({
            let mut consent = Consent::allow_all();
            consent.set(
                Right::scoped(
                    "ai.builtin.fs.read",
                    &format!("{}/allowed/private/**", root.path().display()),
                ),
                Decision::Deny,
            );
            consent
        });
        assert_eq!(
            denied
                .authorize_ai_builtin_path("fs.read", path.to_str().expect("path"))
                .expect_err("consent deny")
                .code,
            ErrorCode::PermissionDenied
        );
        let protected = allowed.with_protected(root.path());
        assert_eq!(
            protected
                .authorize_ai_builtin_path("fs.read", path.to_str().expect("path"))
                .expect_err("protected path")
                .code,
            ErrorCode::PermissionDenied
        );
    }

    #[test]
    fn execution_requires_an_explicit_plain_right() {
        let root = tempfile::tempdir().expect("tempdir");
        let set = set(root.path());
        assert_eq!(
            set.check_ai_builtin("exec", None).expect("exec right"),
            Decision::Allow
        );
        assert!(set.check_ai_builtin("bash", None).is_err());
    }
}
