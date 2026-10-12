// SPDX-License-Identifier: MIT OR Apache-2.0
//! Authority identity compatibility and persistence isolation tests.
use super::*;

fn identity(id: &str, signer: Option<&str>) -> Identity {
    Identity {
        app_id: id.to_owned(),
        signer: signer.map(str::to_owned),
    }
}

#[test]
fn legacy_directory_keys_are_unchanged() {
    assert_eq!(
        identity("org.example.app", None).key(),
        "org.example.app@folder"
    );
    assert_eq!(
        identity("org.example.app", Some("dir:/example/Example App")).key(),
        "org.example.app@dir__example_Example_App"
    );
    let long = format!("dir:/example/{}", "long-folder/".repeat(8));
    assert_eq!(
        identity("org.example.app", Some(&long)).key(),
        "org.example.app@dir__example_long-folder-3df7b65b03c3e758"
    );
    assert_eq!(
        identity("org.example.app", Some("abcd")).key(),
        "org.example.app@abcd"
    );
}

#[test]
fn package_keys_match_authority_tokens_and_fixed_vectors() {
    use crate::security::authority::Authority;
    let digest = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    for (prefix, authority, expected) in [
        (
            "pkg-signed:",
            Authority::SignedPackage {
                fingerprint: digest.into(),
            },
            "20fd524915d71e1a1bda8a99c6af7715ab7f3931655bfa918d35d7b56a5f8343",
        ),
        (
            "pkg-unsigned:",
            Authority::UnsignedPackage {
                archive_digest: digest.into(),
            },
            "9d7e2473837df35661dc156ca7948bc68d20cd613e43d11be365318dcd321c5a",
        ),
    ] {
        let mine = identity("org.example.app", Some(&format!("{prefix}{digest}")));
        assert_eq!(mine.key(), expected);
        assert_eq!(Some(mine.key()), authority.package_token(&mine.app_id));
        assert_eq!(
            serde_json::from_str::<Identity>(&serde_json::to_string(&mine).unwrap()).unwrap(),
            mine
        );
    }
    assert_ne!(
        identity("a", Some("pkg-unsigned:bc")).key(),
        identity("ab", Some("pkg-unsigned:c")).key()
    );
    assert_ne!(
        identity("a", Some("pkg-signed:x:y")).key(),
        identity("a", Some("pkg-signed:x_y")).key()
    );
}

#[test]
fn package_decisions_are_isolated_on_disk_without_changing_json_identity() {
    let folder = tempfile::tempdir().unwrap();
    let store = FileConsentStore::new(folder.path());
    let identities = [
        identity("org.example.app", Some("dir:/example/app")),
        identity("org.example.app", Some("pkg-signed:key-a")),
        identity("org.example.app", Some("pkg-signed:key-b")),
        identity("org.example.app", Some("pkg-unsigned:key-a")),
        identity("org.example.app", Some("pkg-unsigned:changed-content")),
        identity("org.example.other", Some("pkg-signed:key-a")),
    ];
    let mut consent = Consent::undecided();
    consent.set(Right::plain("secrets"), Decision::Allow);
    for (index, mine) in identities.iter().enumerate() {
        assert_eq!(store.load(mine).unwrap(), None);
        store.save(mine, &consent).unwrap();
        assert_eq!(store.load(mine).unwrap(), Some(consent.clone()));
        let saved: serde_json::Value =
            serde_json::from_slice(&fs::read(store.path(mine)).unwrap()).unwrap();
        assert_eq!(saved["version"], 1);
        assert_eq!(saved["app_id"], mine.app_id);
        assert_eq!(saved["signer"].as_str(), mine.signer.as_deref());
        assert_eq!(store.identities().unwrap().len(), index + 1);
    }
    store.forget(&identities[1]).unwrap();
    assert_eq!(store.load(&identities[1]).unwrap(), None);
    for mine in identities.iter().filter(|mine| *mine != &identities[1]) {
        assert_eq!(store.load(mine).unwrap(), Some(consent.clone()));
    }
}
