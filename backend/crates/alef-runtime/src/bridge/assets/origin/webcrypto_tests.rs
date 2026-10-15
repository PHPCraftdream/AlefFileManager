use super::{substitutes, SUBTITUTE_SCRIPT};
use alef_core::security::consent::{Consent, Decision, Right};
use alef_core::security::manifest::{
    AppPermissions, CliPermissions, ClipboardPermissions, FsPermissions, NetPermissions,
    Permissions, ShellPermissions, ShortcutPermissions,
};
use alef_core::security::permissions::{PathVars, PermissionSet};

fn policy(webcrypto: bool) -> PermissionSet {
    let manifest = Permissions {
        fs: FsPermissions {
            read: Vec::new(),
            write: Vec::new(),
        },
        cli: CliPermissions {
            exec: Vec::new(),
            commands: Vec::new(),
        },
        net: NetPermissions {
            http: Vec::new(),
            socket: Vec::new(),
        },
        shell: ShellPermissions {
            open_external: Vec::new(),
        },
        clipboard: ClipboardPermissions { read: false },
        shortcut: ShortcutPermissions { global: false },
        tray: false,
        secrets: false,
        webcrypto,
        app: AppPermissions {
            env: Vec::new(),
            autostart: false,
        },
        window: None,
    };
    let root = std::env::temp_dir();
    let vars = PathVars {
        app_data: root.clone(),
        app_config: root.clone(),
        app_cache: root.clone(),
        home: root.clone(),
        documents: root.clone(),
        downloads: root.clone(),
        desktop: root.clone(),
        temp: root.clone(),
        app: Some(root),
    };
    PermissionSet::from_manifest(&manifest, &vars).expect("manifest")
}

#[test]
fn the_script_seals_every_prototype_method_with_a_rejecting_stub() {
    // Methods come from the prototype, not the instance: WebIDL methods are inherited.
    assert!(SUBTITUTE_SCRIPT.contains("SubtleCrypto.prototype"));
    assert!(SUBTITUTE_SCRIPT.contains("Object.getOwnPropertyNames(proto)"));
    assert!(SUBTITUTE_SCRIPT.contains("Object.defineProperty(proto, name"));
    assert!(SUBTITUTE_SCRIPT.contains("writable: false"));
    assert!(SUBTITUTE_SCRIPT.contains("configurable: false"));
    assert!(SUBTITUTE_SCRIPT
        .contains("Promise.reject(new DOMException(MESSAGE, \"NotSupportedError\"))"));
    assert!(SUBTITUTE_SCRIPT.contains("substituted by Alef"));
    assert!(SUBTITUTE_SCRIPT.contains("console.error("));
    assert!(!SUBTITUTE_SCRIPT.contains("[\"encrypt\", \"decrypt\""));
}

#[test]
fn an_allowed_webcrypto_right_keeps_the_engine_subtle() {
    let allowed = policy(true).with_consent({
        let mut consent = Consent::undecided();
        consent.set(Right::plain("webcrypto"), Decision::Allow);
        consent
    });
    assert!(!substitutes(&allowed));
}

#[test]
fn a_substituted_webcrypto_right_stands_in() {
    let substituted = policy(true).with_consent({
        let mut consent = Consent::undecided();
        consent.set(Right::plain("webcrypto"), Decision::Substitute);
        consent
    });
    assert!(substitutes(&substituted));
}

#[test]
fn a_denied_webcrypto_right_stands_in() {
    let denied = policy(true).with_consent({
        let mut consent = Consent::undecided();
        consent.set(Right::plain("webcrypto"), Decision::Deny);
        consent
    });
    assert!(substitutes(&denied));
}

#[test]
fn an_undeclared_webcrypto_right_stands_in_even_with_allow_all() {
    assert!(substitutes(
        &policy(false).with_consent(Consent::allow_all())
    ));
}

#[test]
fn the_closed_policy_stands_in() {
    assert!(substitutes(&policy(false)));
}
