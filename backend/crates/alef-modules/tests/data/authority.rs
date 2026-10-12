// SPDX-License-Identifier: MIT OR Apache-2.0
//! Authority persistence using synthetic contexts, not launcher-selected OS paths.
use crate::common::{path_vars, Fixture};
use alef_core::{
    registry::{command::Reply, dispatch::Registry},
    security::{
        authority::Authority,
        consent::{Consent, Decision, Right},
    },
    ErrorCode,
};
use alef_modules::{namespace_paths, register_all, MemorySecrets};
use bytes::Bytes;
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

fn authorities() -> [Authority; 4] {
    [
        Authority::Directory,
        Authority::SignedPackage {
            fingerprint: "keyA".into(),
        },
        Authority::SignedPackage {
            fingerprint: "keyB".into(),
        },
        Authority::UnsignedPackage {
            archive_digest: "keyA".into(),
        },
    ]
}

async fn app(root: &Path, authority: &Authority, secrets: &Arc<MemorySecrets>) -> Fixture {
    let manifest = include_str!("../fixtures/app.ktav").replace("secrets: false", "secrets: true");
    let mut app =
        Fixture::new_sharing_authority(root, Some(&manifest), secrets.clone(), authority.clone())
            .await;
    if authority.package_token(&app.context.app.id).is_none() {
        app.context.paths.app_cache = root.join("cache");
    }
    let mut registry = Registry::default();
    register_all(&mut registry, app.host.clone(), &app.context).unwrap();
    app.registry = registry;
    let mut consent = Consent::undecided();
    consent.set(Right::plain("secrets"), Decision::Allow);
    app.with_consent(consent)
}

#[test]
fn authority_explicit_bases_preserve_directory_and_scope_packages_exactly() {
    let root = tempfile::tempdir().unwrap();
    let original = path_vars(root.path());
    let bases = [
        root.path().join("os-data"),
        root.path().join("os-config"),
        root.path().join("os-cache"),
    ];
    let mut owned = Vec::new();
    for authority in authorities() {
        let mut paths = original.clone();
        namespace_paths(
            &mut paths,
            &bases[0],
            &bases[1],
            &bases[2],
            "org.example.app",
            &authority,
        );
        let expected = match authority.package_token("org.example.app") {
            Some(token) => {
                assert!(paths.app.is_none());
                bases
                    .each_ref()
                    .map(|base| base.join("alef-packages").join(&token))
            }
            None => {
                assert_eq!(paths.app, original.app);
                [
                    original.app_data.clone(),
                    original.app_config.clone(),
                    original.app_cache.clone(),
                ]
            }
        };
        assert_eq!(
            [
                paths.app_data.clone(),
                paths.app_config.clone(),
                paths.app_cache.clone()
            ],
            expected
        );
        assert_eq!(paths.home, original.home);
        assert_eq!(paths.documents, original.documents);
        assert_eq!(paths.downloads, original.downloads);
        assert_eq!(paths.desktop, original.desktop);
        assert_eq!(paths.temp, original.temp);
        assert!(!owned.contains(&paths.app_data));
        owned.push(paths.app_data);
    }
    let authority = Authority::SignedPackage {
        fingerprint: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
    };
    let mut paths = original;
    namespace_paths(
        &mut paths,
        &bases[0],
        &bases[1],
        &bases[2],
        "org.example.app",
        &authority,
    );
    assert_eq!(
        paths.app_data,
        bases[0]
            .join("alef-packages/20fd524915d71e1a1bda8a99c6af7715ab7f3931655bfa918d35d7b56a5f8343")
    );
}

#[tokio::test]
async fn authority_store_files_reopen_secrets_and_temp_are_isolated_for_same_id() {
    let root = tempfile::tempdir().unwrap();
    let secrets = Arc::new(MemorySecrets::default());
    let authorities = authorities();
    let mut services = Vec::new();
    let mut stores = Vec::new();
    for (index, authority) in authorities.iter().enumerate() {
        let app = app(root.path(), authority, &secrets).await;
        let paths = app.context.effective_paths().unwrap();
        let expected_data = authority
            .package_token(&app.context.app.id)
            .map(|token| root.path().join("alef-packages").join(token))
            .unwrap_or_else(|| root.path().join("data"));
        assert_eq!(paths.app_data, expected_data);
        assert_eq!(
            app.context.effective_paths().unwrap().app_data,
            paths.app_data
        );
        let mut scoped = app.context.clone();
        scoped.paths = paths.clone();
        assert_eq!(
            scoped.effective_paths().unwrap().app_cache,
            paths.app_cache,
            "idempotent"
        );
        for (command, path) in [
            ("path.appData", &paths.app_data),
            ("path.appConfig", &paths.app_config),
            ("path.appCache", &paths.app_cache),
        ] {
            assert_eq!(
                app.call(command, Value::Null).await.unwrap(),
                json!(path.to_string_lossy())
            );
        }
        assert_eq!(
            app.call("store.get", json!({"key":"same"})).await.unwrap(),
            json!({})
        );
        app.call("store.set", json!({"key":"same", "value":index}))
            .await
            .unwrap();
        app.call("store.flush", Value::Null).await.unwrap();
        let store = expected_data.join("store");
        assert!(store.is_dir());
        assert!(
            std::fs::read_dir(&store).unwrap().next().is_some(),
            "real database files"
        );
        assert!(!stores.contains(&store));
        stores.push(store);
        for command in ["fs.tempFile", "fs.tempDir"] {
            let temp = PathBuf::from(
                app.call(command, Value::Null)
                    .await
                    .unwrap()
                    .as_str()
                    .unwrap(),
            );
            assert_eq!(temp.parent().unwrap(), paths.app_cache.join("tmp"));
            assert!(temp.exists());
        }
        let names = json!({"service":"mail", "account":"me"});
        assert!(matches!(
            app.call_reply("secrets.get", names.clone(), None)
                .await
                .unwrap(),
            Reply::Json(Value::Null)
        ));
        app.call_reply(
            "secrets.set",
            names,
            Some(Bytes::from(vec![index as u8 + 1])),
        )
        .await
        .unwrap();
        services.push((
            match authority.package_token(&app.context.app.id) {
                Some(token) => format!("alef/packages/{token}/mail"),
                None => format!("alef/{}/mail", app.context.app.id),
            },
            "me".to_owned(),
        ));
        app.session().close().await;
        // Drop registry/database before reopening on Windows as well as Unix.
        drop(app);
    }
    services.sort();
    assert_eq!(secrets.names(), services);
    for (index, authority) in authorities.iter().enumerate() {
        let app = app(root.path(), authority, &secrets).await;
        assert_eq!(
            app.call("store.get", json!({"key":"same"})).await.unwrap(),
            json!({"value":index})
        );
        let reply = app
            .call_reply(
                "secrets.get",
                json!({"service":"mail", "account":"me"}),
                None,
            )
            .await
            .unwrap();
        assert!(matches!(reply, Reply::Bytes(bytes) if bytes.as_ref() == [index as u8 + 1]));
    }
}

#[tokio::test]
async fn authority_effective_paths_reject_mismatched_reserved_tokens_and_relative_bases() {
    let root = tempfile::tempdir().unwrap();
    let mut app = app(root.path(), &authorities()[1], &Arc::default()).await;
    let valid = app.context.paths.clone();
    for legacy in [
        root.path().join("data"),
        root.path().join("arbitrary/nested/owned"),
    ] {
        app.context.paths.app_data = legacy;
        assert_eq!(
            app.context.effective_paths().unwrap_err().code,
            ErrorCode::InvalidArgument
        );
    }
    app.context.paths = valid;
    app.context.paths.app_data = root.path().join("alef-packages/wrong-token");
    assert_eq!(
        app.context.effective_paths().unwrap_err().code,
        ErrorCode::InvalidArgument
    );
    app.context.paths.app_data = PathBuf::from("relative/owned");
    assert_eq!(
        app.context.effective_paths().unwrap_err().code,
        ErrorCode::InvalidArgument
    );
}
