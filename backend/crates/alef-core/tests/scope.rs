// SPDX-License-Identifier: MIT OR Apache-2.0
//! Network (HTTP/WebSocket, socket, openExternal) and executable scopes.
use alef_core::{
    security::{
        grants::Grants,
        manifest::{Manifest, Permissions},
        permissions::{PathVars, Permission, PermissionSet},
    },
    ErrorCode,
};
use std::path::PathBuf;

const CASE_INSENSITIVE: bool = cfg!(any(windows, target_os = "macos"));

fn closed() -> Permissions {
    Manifest::from_ktav_str(include_str!("fixtures/minimal.ktav"))
        .expect("fixture")
        .permissions
}

fn vars() -> PathVars {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    PathVars {
        app_data: root.clone(),
        app_config: root.clone(),
        app_cache: root.clone(),
        home: root.clone(),
        documents: root.clone(),
        downloads: root.clone(),
        desktop: root.clone(),
        temp: root.clone(),
        app: root,
    }
}

fn build(policy: &Permissions) -> Result<PermissionSet, alef_core::AlefError> {
    PermissionSet::from_manifest(policy, &vars())
}

fn http(patterns: &[&str]) -> PermissionSet {
    let mut policy = closed();
    policy.net.http = patterns.iter().map(|s| s.to_string()).collect();
    build(&policy).expect("valid http scopes")
}

fn sockets(patterns: &[&str]) -> PermissionSet {
    let mut policy = closed();
    policy.net.socket = patterns.iter().map(|s| s.to_string()).collect();
    build(&policy).expect("valid socket scopes")
}

fn exec(patterns: &[&str]) -> PermissionSet {
    let mut policy = closed();
    policy.cli.exec = patterns.iter().map(|s| s.to_string()).collect();
    build(&policy).expect("valid exec scopes")
}

fn ok(set: &PermissionSet, permission: Permission, target: &str) -> bool {
    set.check(permission, Some(target), &Grants::new()).is_ok()
}

#[test]
fn origin_wide_url_scope_covers_every_path_but_only_that_origin() {
    let set = http(&["https://api.example.com"]);
    for yes in [
        "https://api.example.com",
        "https://api.example.com/",
        "https://api.example.com/x/y?q=1#frag",
        "https://API.Example.COM:443/x",
    ] {
        assert!(ok(&set, Permission::NetHttp, yes), "{yes}");
    }
    for no in [
        "http://api.example.com/",
        "https://api.example.com:8443/",
        "https://other.example.com/",
        "https://api.example.com.evil.com/",
        "wss://api.example.com/",
    ] {
        assert!(!ok(&set, Permission::NetHttp, no), "{no}");
    }
}

#[test]
fn url_path_scopes_are_segment_aware() {
    let prefix = http(&["https://h.example.com/api/*"]);
    for yes in [
        "https://h.example.com/api",
        "https://h.example.com/api/",
        "https://h.example.com/api/v1/x",
    ] {
        assert!(ok(&prefix, Permission::NetHttp, yes), "{yes}");
    }
    for no in [
        "https://h.example.com/apiv2",
        "https://h.example.com/",
        "https://h.example.com/other/api",
    ] {
        assert!(!ok(&prefix, Permission::NetHttp, no), "{no}");
    }
    let exact = http(&["https://h.example.com/api/v1"]);
    assert!(ok(
        &exact,
        Permission::NetHttp,
        "https://h.example.com/api/v1?x=1"
    ));
    for no in [
        "https://h.example.com/api/v1/",
        "https://h.example.com/api/v1/x",
        "https://h.example.com/api",
    ] {
        assert!(!ok(&exact, Permission::NetHttp, no), "{no}");
    }
}

#[test]
fn wildcard_host_matches_subdomains_only() {
    let set = http(&["https://*.example.com/*"]);
    for yes in ["https://a.example.com/", "https://a.b.example.com/x"] {
        assert!(ok(&set, Permission::NetHttp, yes), "{yes}");
    }
    for no in [
        "https://example.com/",
        "https://evilexample.com/",
        "https://a.evilexample.com/",
        "https://a.example.com.evil.com/",
        "https://.example.com/",
        "https://a..example.com/",
        "https://a.example.com./",
    ] {
        assert!(!ok(&set, Permission::NetHttp, no), "{no}");
    }
}

#[test]
fn hostile_candidate_urls_are_denied() {
    let set = http(&["https://api.example.com/api/*"]);
    for no in [
        "https://user@api.example.com/api",
        "https://api.example.com@evil.com/api",
        "https://evil.com\\@api.example.com/api",
        "https://api.example.com\\api",
        "https://api.example.com/api/../admin",
        "https://api.example.com/api/./x",
        "https://api.example.com/api/%2e%2e/admin",
        "https://api.example.com/api/%2E%2E/admin",
        "https://api.example.com/api%2fx",
        "https://api.example.com/api%5cx",
        "https://api.example.com/api x",
        "https://api.example.com/api\n",
        "https://api.example.com/api\0",
        "https://api.exаmple.com/api",
        "https://api.example.com:0/api",
        "https://api.example.com:99999/api",
        "https://api.example.com:/api",
        "https://api.example.com:44a/api",
        "https://%61pi.example.com/api",
        "HTTPS://api.example.com/api",
        "//api.example.com/api",
        "api.example.com/api",
        "",
    ] {
        assert!(!ok(&set, Permission::NetHttp, no), "{no:?}");
    }
    assert!(set
        .check(Permission::NetHttp, None, &Grants::new())
        .is_err());
}

#[test]
fn websocket_schemes_and_ipv6_literals() {
    let ws = http(&["wss://ws.example.com/*"]);
    assert!(ok(&ws, Permission::NetHttp, "wss://ws.example.com/x"));
    assert!(!ok(&ws, Permission::NetHttp, "ws://ws.example.com/x"));
    assert!(!ok(&ws, Permission::NetHttp, "https://ws.example.com/x"));
    let local = http(&["http://[::1]:8080/*"]);
    assert!(ok(&local, Permission::NetHttp, "http://[::1]:8080/x"));
    assert!(!ok(&local, Permission::NetHttp, "http://[::2]:8080/x"));
    assert!(!ok(&local, Permission::NetHttp, "http://[::1]:8081/x"));
    assert!(!ok(&local, Permission::NetHttp, "http://::1:8080/x"));
}

#[test]
fn open_external_uses_the_same_url_grammar() {
    let mut policy = closed();
    policy.shell.open_external = vec!["https://docs.example.com".into()];
    let set = build(&policy).expect("valid");
    assert!(ok(
        &set,
        Permission::ShellOpenExternal,
        "https://docs.example.com/page"
    ));
    assert!(!ok(
        &set,
        Permission::ShellOpenExternal,
        "https://docs.example.com.evil/page"
    ));
    assert!(
        !ok(&set, Permission::NetHttp, "https://docs.example.com/page"),
        "different permission"
    );
    policy.shell.open_external = vec!["javascript:alert(1)".into()];
    assert_eq!(build(&policy).unwrap_err().code, ErrorCode::ManifestInvalid);
}

#[test]
fn malformed_url_and_socket_scopes_are_manifest_errors() {
    let bad_http = [
        "",
        "https://",
        "https://*",
        "https://*.com",
        "ftp://host",
        "javascript:x",
        "https://user@host/",
        "https://host:0",
        "https://host:99999",
        "https://host:/",
        "https://host/a/*/b",
        "https://host/../x",
        "https://host/a*",
        "https://ho st/",
        "https://-bad.com",
        "https://a_b.com",
        "https://host/%2e",
        "https://host/?q",
        "https://host\\x",
        "https://[::1",
        "https://[zz]/",
    ];
    for pattern in bad_http {
        let mut policy = closed();
        policy.net.http = vec![pattern.into()];
        assert_eq!(
            build(&policy).unwrap_err().code,
            ErrorCode::ManifestInvalid,
            "http {pattern:?}"
        );
    }
    let bad_socket = [
        "",
        "icmp:host:80",
        "tcp:host",
        "tcp:host:",
        "tcp:host:0",
        "tcp:host:99999",
        "tcp::80",
        "tcp:*.com:1",
        "tcp:a*b.example.com:1",
        "tcp:host:80:81",
        "tcp:[::1:80",
        "tcp:ho st:80",
    ];
    for pattern in bad_socket {
        let mut policy = closed();
        policy.net.socket = vec![pattern.into()];
        assert_eq!(
            build(&policy).unwrap_err().code,
            ErrorCode::ManifestInvalid,
            "socket {pattern:?}"
        );
    }
}

#[test]
fn socket_protocols_hosts_and_ports() {
    let set = sockets(&[
        "tcp:*.example.com:443",
        "udp:0.0.0.0:5353",
        "listen:127.0.0.1:*",
        "tcp:[::1]:9000",
        "tcp:*:22",
    ]);
    let s = Permission::NetSocket;
    for yes in [
        "tcp:a.example.com:443",
        "tcp:A.B.Example.com:443",
        "udp:0.0.0.0:5353",
        "listen:127.0.0.1:9001",
        "listen:127.0.0.1:1",
        "tcp:[::1]:9000",
        "tcp:anything.test:22",
    ] {
        assert!(ok(&set, s, yes), "{yes}");
    }
    for no in [
        "udp:a.example.com:443",
        "tcp:example.com:443",
        "tcp:a.example.com:444",
        "udp:127.0.0.1:5353",
        "tcp:127.0.0.1:9001",
        "listen:127.0.0.1:0",
        "listen:127.0.0.1:",
        "listen:127.0.0.1:65536",
        "listen:127.0.0.2:9001",
        "tcp:*:22",
        "tcp:*.example.com:443",
        "tcp:::1:9000",
        "tcp:[::2]:9000",
        "tcp:a.example.com.:443",
        "tcp:a b:22",
        "tcp:host:22\n",
        "",
    ] {
        assert!(!ok(&set, s, no), "{no:?}");
    }
}

#[test]
fn exec_list_empty_denies_and_star_is_the_only_wildcard() {
    let none = exec(&[]);
    assert!(!ok(&none, Permission::CliExec, "git"));
    let any = exec(&["*"]);
    assert!(ok(&any, Permission::CliExec, "anything"));
    assert!(
        !ok(&any, Permission::CliExec, ""),
        "even `*` needs a real target"
    );
    assert!(!ok(&any, Permission::CliExec, "a\nb"));
    assert!(
        any.check(Permission::ClipboardRead, None, &Grants::new())
            .is_err(),
        "exec `*` grants no other capability"
    );
    let partial = exec(&["gi*"]);
    assert!(
        !ok(&partial, Permission::CliExec, "git"),
        "only a lone `*` is a wildcard"
    );
}

#[test]
fn exec_bare_name_never_matches_paths_and_follows_case_rules() {
    let set = exec(&["git"]);
    let c = Permission::CliExec;
    assert!(ok(&set, c, "git"));
    assert_eq!(ok(&set, c, "GIT"), CASE_INSENSITIVE);
    for no in [
        "git.exe",
        "/usr/bin/git",
        "./git",
        "bin/git",
        "git ",
        "gitx",
        "",
        "C:\\git",
    ] {
        assert!(!ok(&set, c, no), "{no:?}");
    }
}

fn abs_exec() -> (&'static str, &'static str) {
    if cfg!(windows) {
        (r"C:\tools\node.exe", r"c:/TOOLS/node.EXE")
    } else {
        ("/usr/bin/node", "/usr/bin/node")
    }
}

#[test]
fn exec_absolute_path_matches_only_the_same_normalized_path() {
    let (pattern, same) = abs_exec();
    let set = exec(&[pattern]);
    let c = Permission::CliExec;
    assert!(ok(&set, c, pattern));
    assert!(
        ok(&set, c, same),
        "{same:?}: the same path (separator style and case on Windows)"
    );
    let root = if cfg!(windows) {
        r"C:\tools"
    } else {
        "/usr/bin"
    };
    for no in [
        "node".to_string(),
        "node.exe".to_string(),
        format!(
            "{root}/../{}",
            if cfg!(windows) {
                "tools/node.exe"
            } else {
                "bin/node"
            }
        ),
        format!(
            "{root}/./{}",
            if cfg!(windows) { "node.exe" } else { "node" }
        ),
        format!("{pattern}/"),
        format!("{pattern}x"),
        format!("{root}/other"),
        String::new(),
    ] {
        assert!(!ok(&set, c, &no), "{no:?}");
    }
    if cfg!(windows) {
        for no in [
            r"C:\tools\node.exe.",
            r"C:\tools\node.exe:ads",
            r"\tools\node.exe",
            r"\\tools\node.exe",
        ] {
            assert!(!ok(&set, c, no), "{no:?}");
        }
    } else {
        assert!(
            !ok(&set, c, "/usr/bin/NODE"),
            "Unix is case-sensitive (macOS excepted)"
        );
    }
}

#[test]
fn malformed_exec_scopes_are_manifest_errors() {
    for pattern in [
        "",
        "bin/node",
        "./node",
        "..",
        ".",
        "/usr/bin/../node",
        "/usr/./node",
        "node\0",
        "a\nb",
        "/usr/bin/node/",
    ] {
        let mut policy = closed();
        policy.cli.exec = vec![pattern.into()];
        assert_eq!(
            build(&policy).unwrap_err().code,
            ErrorCode::ManifestInvalid,
            "{pattern:?}"
        );
    }
}
