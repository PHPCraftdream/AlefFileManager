// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
#[test]
fn authority_endpoint_uses_full_package_tokens_and_pins_directory_names() {
    use crate::{AppInfo, Backends, LaunchState, Termination};
    use alef_core::security::manifest::Mode;
    use alef_core::security::{authority::Authority, permissions::PathVars};
    let at = |name: &str| PathBuf::from(name);
    let mut context = ModuleContext {
        authority: Authority::Directory,
        app: AppInfo {
            id: "org.example.app".into(),
            name: "App".into(),
            version: "1".into(),
            runtime_version: "1".into(),
        },
        paths: PathVars {
            app_data: at("data"),
            app_config: at("config"),
            app_cache: at("cache"),
            home: at("home"),
            documents: at("documents"),
            downloads: at("downloads"),
            desktop: at("desktop"),
            temp: at("temp"),
            app: Some(at("app")),
        },
        args: ParsedArgs::default(),
        deep_link_schemes: Vec::new(),
        startup_urls: Vec::new(),
        process_args: Vec::new(),
        backends: Backends::pretending(None, None),
        shadow: at("shadow"),
        ai_home: at("ai-home"),
        console: None,
        launch: LaunchState::new(vec![Mode::Window], Some(Mode::Window), crate::LAUNCH_LIMIT)
            .unwrap(),
        termination: Termination::default(),
    };
    let directory = Endpoint::of(&context);
    assert_eq!(directory.key, "a0956347e3845b59");
    let mut keys = vec![directory.key.clone()];
    for authority in [
        Authority::SignedPackage {
            fingerprint: "keyA".into(),
        },
        Authority::SignedPackage {
            fingerprint: "keyB".into(),
        },
        Authority::UnsignedPackage {
            archive_digest: "keyA".into(),
        },
    ] {
        context.authority = authority;
        let endpoint = Endpoint::of(&context);
        assert_eq!(
            endpoint.key,
            context.authority.package_token(&context.app.id).unwrap()
        );
        assert_eq!(endpoint.key.len(), 64);
        assert!(!keys.contains(&endpoint.key));
        keys.push(endpoint.key.clone());
        #[cfg(windows)]
        assert_eq!(
            platform::name(&endpoint),
            format!(r"\\.\pipe\alef-instance-{}", endpoint.key)
        );
        #[cfg(unix)]
        assert_eq!(
            platform::socket(&endpoint),
            endpoint.folder.join(format!("{}.sock", endpoint.key))
        );
        context.paths.home = at("another-home");
        assert_eq!(
            Endpoint::of(&context).key,
            endpoint.key,
            "package identity is not home-dependent"
        );
    }
    #[cfg(windows)]
    assert_eq!(
        platform::name(&directory),
        r"\\.\pipe\alef-instance-a0956347e3845b59"
    );
    #[cfg(unix)]
    assert_eq!(
        platform::socket(&directory),
        directory.folder.join("a0956347e3845b59.sock")
    );
}

#[cfg(unix)]
#[test]
fn authority_full_token_socket_fits_even_with_a_long_cache() {
    let key = "a".repeat(64);
    let folder = Endpoint::folder_for(&key, &PathBuf::from(format!("/{}", "x".repeat(200))));
    let socket = folder.join(format!("{key}.sock"));
    assert!(socket.as_os_str().len() < SOCKET_PATH_LIMIT);
    assert_eq!(socket.file_name().unwrap(), format!("{key}.sock").as_str());
}
