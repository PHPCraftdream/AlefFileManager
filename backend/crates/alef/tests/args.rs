// SPDX-License-Identifier: MIT OR Apache-2.0
//! Integration tests of the launcher command line, including the DevTools flag.
use std::{ffi::OsString, path::PathBuf};

use alef_core::security::consent::{Decision, Right};
use alef_launch::args::{
    parse_args, AiCommand, Command, Launch, PermissionsCommand, SpareKind, WarmCommand, USAGE,
};
use url::Url;

fn parse(arguments: &[&str]) -> Result<Command, String> {
    parse_args(arguments.iter().map(OsString::from))
}

#[test]
fn an_absolute_windows_package_path_is_the_target_and_not_a_link() {
    let Command::Run(launch) =
        parse(&["run", r"C:\Temp\app.alef", "--trust-key", &"a".repeat(64)]).unwrap()
    else {
        panic!("run")
    };
    assert_eq!(launch.target, PathBuf::from(r"C:\Temp\app.alef"));
    assert!(launch.app_args.is_empty());
    assert!(launch.trust_key.is_some());
}

#[test]
fn the_hidden_window_mode_is_strict_and_only_belongs_to_the_runtime_before_the_separator() {
    let Command::Run(run) = parse(&["--launch-mode", "window", "--app", "site"]).unwrap() else {
        panic!("run")
    };
    assert!(run.launch_window);
    assert!(!USAGE.contains("--launch-mode"));
    for bad in [
        "cli",
        "foreground",
        "undecided",
        "Window",
        "",
        "window ",
        "--",
    ] {
        assert!(
            parse(&["--app", "site", "--launch-mode", bad]).is_err(),
            "{bad:?}"
        );
    }
    assert!(parse(&["--app", "site", "--launch-mode"])
        .unwrap_err()
        .contains("requires a value"));
    let Command::Run(run) = parse(&["--app", "site", "--", "--launch-mode", "window"]).unwrap()
    else {
        panic!("run")
    };
    assert!(!run.launch_window);
    assert_eq!(
        run.app_args,
        [OsString::from("--launch-mode"), OsString::from("window")]
    );
}

#[test]
fn trailing_urls_belong_to_the_application_but_unknown_options_and_strays_do_not() {
    let Command::Run(launch) = parse(&["--app", "site", "alef://open", "--", "alef:next"]).unwrap()
    else {
        panic!("run");
    };
    assert_eq!(
        launch.app_args,
        [OsString::from("alef://open"), OsString::from("alef:next")]
    );
    for bad in ["stray", "--unknown", "alef://[bad", "alef:bad\n"] {
        assert!(parse(&["--app", "site", bad]).is_err(), "{bad:?}");
    }
}

#[test]
fn no_option_of_the_runtime_follows_a_url_or_comes_twice() {
    for after in [
        ["--app", "other"].as_slice(),
        ["--grant", "allow"].as_slice(),
        ["--dev-url", "http://127.0.0.1:3000/"].as_slice(),
        ["--devtools"].as_slice(),
        ["--devtools=7000"].as_slice(),
        ["--no-prompt"].as_slice(),
        ["--launch-mode", "window"].as_slice(),
        ["--trust-key", "a"].as_slice(),
        ["--allow-unsigned"].as_slice(),
    ] {
        let mut line = vec!["--app", "site", "alef://open"];
        line.extend(after);
        assert!(parse(&line).is_err(), "{line:?}");
    }
    for twice in [
        ["--app", "a", "--app", "b"].as_slice(),
        ["--app", "a", "--grant", "allow", "--grant", "deny"].as_slice(),
        ["--app", "a", "--devtools", "--devtools"].as_slice(),
        ["--app", "a", "--devtools=7000", "--devtools"].as_slice(),
        [
            "--app",
            "a",
            "--launch-mode",
            "window",
            "--launch-mode",
            "window",
        ]
        .as_slice(),
        [
            "--app",
            "a",
            "--dev-url",
            "http://127.0.0.1:1/",
            "--dev-url",
            "http://127.0.0.1:2/",
        ]
        .as_slice(),
    ] {
        assert!(parse(twice).is_err(), "{twice:?}");
    }
    // What the registration of a scheme passes: everything after the double dash is the
    // application's.
    let Command::Run(launch) =
        parse(&["--app", "site", "--", "alef:x", "--grant", "allow"]).unwrap()
    else {
        panic!("run");
    };
    assert!(launch.grant.is_none());
    assert_eq!(launch.app_args.len(), 3);
    assert!(launch.devtools_port.is_none());
}

#[test]
fn devtools_port_is_strict_ascii_and_optional_equals_form() {
    for bad in ["", "0", "65536", "+1", " 1", "1 ", "１", "١", "1x"] {
        assert!(
            parse(&["--app", "site", &format!("--devtools={bad}")]).is_err(),
            "{bad:?}"
        );
    }
    assert!(parse(&["--app", "site", "--devtools=7000", "--devtools"])
        .unwrap_err()
        .contains("given twice"));
    let Command::Run(default) = parse(&["--app", "site", "--devtools"]).unwrap() else {
        panic!("run");
    };
    assert_eq!(default.devtools_port, Some(7000));
    for (text, port) in [("1", 1), ("65535", 65535), ("0070", 70)] {
        let Command::Run(run) = parse(&["--app", "site", &format!("--devtools={text}")]).unwrap()
        else {
            panic!("run");
        };
        assert_eq!(run.devtools_port, Some(port));
    }
}

#[test]
fn app_and_dev_url_are_read() {
    let command = parse(&["--app", "site", "--dev-url", "http://127.0.0.1:3000/"]).unwrap();
    assert_eq!(
        command,
        Command::Run(Launch {
            target: PathBuf::from("site"),
            dev_url: Some(Url::parse("http://127.0.0.1:3000/").unwrap()),
            devtools_port: None,
            app_args: Vec::new(),
            grant: None,
            trust_key: None,
            allow_unsigned: false,
            no_prompt: false,
            launch_window: false,
            positional: false,
        })
    );
    assert_eq!(
        parse(&["--app", "site"]).unwrap(),
        Command::Run(Launch {
            target: PathBuf::from("site"),
            dev_url: None,
            devtools_port: None,
            app_args: Vec::new(),
            grant: None,
            trust_key: None,
            allow_unsigned: false,
            no_prompt: false,
            launch_window: false,
            positional: false,
        })
    );
}

#[test]
fn everything_after_the_double_dash_belongs_to_the_application() {
    let command = parse(&["--app", "site", "--", "--help", "--app", "x", "-h", "--"]).unwrap();
    let Command::Run(launch) = command else {
        panic!("a run, not the launcher help");
    };
    assert_eq!(launch.target, PathBuf::from("site"));
    let expected: Vec<OsString> = ["--help", "--app", "x", "-h", "--"]
        .iter()
        .map(OsString::from)
        .collect();
    assert_eq!(launch.app_args, expected);
    assert!(parse(&["--", "--app", "x"]).unwrap_err().contains("--app"));
}

#[test]
fn help_wins_and_everything_else_is_strict() {
    assert_eq!(parse(&["--help"]).unwrap(), Command::Help);
    assert_eq!(parse(&["--app", "x", "-h"]).unwrap(), Command::Help);
    assert!(parse(&[]).unwrap_err().contains("--app"));
    assert!(parse(&["--app"]).unwrap_err().contains("requires a value"));
    assert!(parse(&["--dev-url"])
        .unwrap_err()
        .contains("requires a value"));
    assert!(parse(&["--app", "x", "--frontend-dir", "y"])
        .unwrap_err()
        .contains("unknown argument"));
    assert!(parse(&["stray"]).unwrap_err().contains("unknown argument"));
}

#[test]
fn install_management_commands_are_strict_and_preserve_paths() {
    assert_eq!(
        parse(&[
            "install",
            "packages/app.alef",
            "--trust-key",
            "fingerprint",
            "--allow-unsigned"
        ])
        .unwrap(),
        Command::Install {
            package: PathBuf::from("packages/app.alef"),
            trust_key: Some("fingerprint".to_owned()),
            allow_unsigned: true,
        }
    );
    assert_eq!(
        parse(&["uninstall", "app-id", "--purge-data"]).unwrap(),
        Command::Uninstall {
            id: "app-id".to_owned(),
            purge_data: true
        }
    );
    assert_eq!(
        parse(&["apps", "--json"]).unwrap(),
        Command::Apps { json: true }
    );

    for bad in [
        &["install"][..],
        &["install", "--allow-unsigned"][..],
        &["install", "pkg", "--allow-unsigned", "--allow-unsigned"][..],
        &["install", "pkg", "--trust-key", "a", "--trust-key", "b"][..],
        &["install", "pkg", "extra"][..],
        &["apps", "--json", "extra"][..],
        &["apps", "--unknown"][..],
        &["uninstall"][..],
        &["uninstall", "id", "--purge-data", "--purge-data"][..],
        &["uninstall", "id", "extra"][..],
    ] {
        assert!(
            parse(bad).unwrap_err().contains("INVALID_ARGUMENT"),
            "{bad:?}"
        );
    }
}

#[test]
fn apps_default_and_install_unsigned_parse() {
    assert_eq!(
        parse(&["install", "folder/app.alef", "--allow-unsigned"]).unwrap(),
        Command::Install {
            package: PathBuf::from("folder/app.alef"),
            trust_key: None,
            allow_unsigned: true
        }
    );
    assert_eq!(parse(&["apps"]).unwrap(), Command::Apps { json: false });
    assert!(parse(&["install", "--trust-key"])
        .unwrap_err()
        .contains("INVALID_ARGUMENT"));
}

#[test]
fn a_multibyte_target_is_kept_as_an_os_string() {
    let Command::Run(launch) = parse(&["run", "приложение"]).unwrap() else {
        panic!("a run");
    };
    assert_eq!(launch.target, PathBuf::from("приложение"));
}

#[test]
fn run_and_app_are_the_same_launch_and_a_positional_after_run_is_the_target() {
    for (line, expected) in [
        (&["--app", "site"][..], "site"),
        (&["run", "site"][..], "site"),
        (&["run", "site.alef"][..], "site.alef"),
    ] {
        let Command::Run(launch) = parse(line).unwrap() else {
            panic!("a run for {line:?}");
        };
        assert_eq!(launch.target, PathBuf::from(expected), "{line:?}");
    }
    assert!(parse(&["stray"]).unwrap_err().contains("unknown argument"));
    assert!(parse(&["run", "a", "b"])
        .unwrap_err()
        .contains("the application is given twice"));
    assert!(parse(&["run", "a", "--app", "b"])
        .unwrap_err()
        .contains("is given twice"));
}

#[test]
fn trust_flags_are_captured_and_refused_after_a_url() {
    let Command::Run(launch) = parse(&[
        "run",
        "site.alef",
        "--trust-key",
        "aabb",
        "--allow-unsigned",
    ])
    .unwrap() else {
        panic!("a run");
    };
    assert_eq!(launch.trust_key.as_deref(), Some("aabb"));
    assert!(launch.allow_unsigned);
    assert!(
        parse(&["run", "site.alef", "--trust-key", "a", "--trust-key", "b"])
            .unwrap_err()
            .contains("--trust-key is given twice")
    );
    assert!(
        parse(&["run", "site.alef", "--allow-unsigned", "--allow-unsigned"])
            .unwrap_err()
            .contains("--allow-unsigned is given twice")
    );
    for after in [&["--trust-key", "a"][..], &["--allow-unsigned"][..]] {
        let mut line = vec!["run", "site.alef", "alef://open"];
        line.extend(after);
        assert!(parse(&line)
            .unwrap_err()
            .contains("is not accepted after a URL"));
    }
}

#[test]
fn options_of_the_other_source_are_refused_by_check() {
    let mut launch = Launch {
        target: PathBuf::from("site.alef"),
        dev_url: None,
        devtools_port: Some(7000),
        app_args: Vec::new(),
        grant: None,
        trust_key: Some("aabb".to_owned()),
        allow_unsigned: true,
        no_prompt: false,
        launch_window: false,
        positional: false,
    };
    assert!(launch
        .check(true)
        .unwrap_err()
        .contains("--devtools is not accepted for a package"));
    launch.dev_url = Some(Url::parse("http://127.0.0.1:3000/").unwrap());
    // With both dev_url and devtools_port present, --dev-url wins because
    // Launch::check validates dev_url before devtools_port.
    assert!(launch
        .check(true)
        .unwrap_err()
        .contains("--dev-url is not accepted for a package"));
    launch.devtools_port = None;
    launch.dev_url = None;
    assert_eq!(launch.check(true), Ok(()));
    assert!(launch
        .check(false)
        .unwrap_err()
        .contains("--trust-key is only accepted for a package"));
    launch.trust_key = None;
    assert!(launch
        .check(false)
        .unwrap_err()
        .contains("--allow-unsigned is only accepted for a package"));
    launch.allow_unsigned = false;
    assert_eq!(launch.check(false), Ok(()));
}

#[test]
fn the_trust_subcommand_takes_exactly_a_request_and_an_answer() {
    assert_eq!(
        parse(&["trust", "in.json", "out.json"]).unwrap(),
        Command::Trust {
            request: PathBuf::from("in.json"),
            answer: PathBuf::from("out.json"),
        }
    );
    for bad in [
        &["trust"][..],
        &["trust", "in.json"][..],
        &["trust", "a", "b", "c"][..],
    ] {
        assert!(parse(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn grant_is_one_of_the_three_decisions() {
    for (word, decision) in [
        ("allow", Decision::Allow),
        ("substitute", Decision::Substitute),
        ("deny", Decision::Deny),
    ] {
        let Command::Run(launch) = parse(&["--app", "x", "--grant", word]).unwrap() else {
            panic!("a run");
        };
        assert_eq!(launch.grant, Some(decision));
    }
    assert!(parse(&["--app", "x", "--grant", "maybe"])
        .unwrap_err()
        .contains("not a decision"));
    assert!(parse(&["--app", "x", "--grant"])
        .unwrap_err()
        .contains("requires a value"));
    let Command::Run(launch) = parse(&["--app", "x", "--", "--grant", "deny"]).unwrap() else {
        panic!("a run");
    };
    assert_eq!(launch.grant, None, "after -- it is the application's own");
}

#[test]
fn no_prompt_belongs_to_the_launcher_and_the_consent_window_takes_two_files() {
    let Command::Run(launch) = parse(&["--app", "x", "--no-prompt"]).unwrap() else {
        panic!("a run");
    };
    assert!(launch.no_prompt);
    let Command::Run(after) = parse(&["--app", "x", "--", "--no-prompt"]).unwrap() else {
        panic!("a run");
    };
    assert!(!after.no_prompt, "after -- it is the application's own");
    assert_eq!(
        parse(&["consent", "in.json", "out.json"]).unwrap(),
        Command::Consent {
            request: PathBuf::from("in.json"),
            answer: PathBuf::from("out.json"),
        }
    );
    for bad in [
        &["consent"][..],
        &["consent", "in.json"][..],
        &["consent", "a", "b", "c"][..],
    ] {
        assert!(parse(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn permissions_accepts_application_ids_for_each_verb() {
    let id = "org.example.app";
    for verb in ["list", "reset"] {
        let command = parse(&["permissions", verb, id]).unwrap();
        match (verb, command) {
            ("list", Command::Permissions(PermissionsCommand::List { app: Some(app) }))
            | ("reset", Command::Permissions(PermissionsCommand::Reset { app })) => {
                assert_eq!(app, PathBuf::from(id));
            }
            _ => panic!("unexpected permissions command for {verb}"),
        }
    }
    assert_eq!(
        parse(&[
            "permissions",
            "set",
            id,
            "clipboard.read",
            "allow",
            "--confirm-red"
        ])
        .unwrap(),
        Command::Permissions(PermissionsCommand::Set {
            app: PathBuf::from(id),
            right: "clipboard.read".parse().unwrap(),
            decision: Decision::Allow,
            confirm_red: true,
        })
    );
}

#[test]
fn permissions_has_three_verbs_with_strict_arguments() {
    assert_eq!(
        parse(&["permissions", "list"]).unwrap(),
        Command::Permissions(PermissionsCommand::List { app: None })
    );
    assert_eq!(
        parse(&["permissions", "list", "site"]).unwrap(),
        Command::Permissions(PermissionsCommand::List {
            app: Some(PathBuf::from("site"))
        })
    );
    assert_eq!(
        parse(&["permissions", "set", "site", "app.env:HOME", "substitute"]).unwrap(),
        Command::Permissions(PermissionsCommand::Set {
            app: PathBuf::from("site"),
            right: Right::scoped("app.env", "HOME"),
            decision: Decision::Substitute,
            confirm_red: false,
        })
    );
    assert_eq!(
        parse(&["permissions", "reset", "site"]).unwrap(),
        Command::Permissions(PermissionsCommand::Reset {
            app: PathBuf::from("site")
        })
    );
    assert_eq!(
        parse(&[
            "permissions",
            "set",
            "site",
            "app.env:HOME",
            "substitute",
            "--confirm-red"
        ])
        .unwrap(),
        Command::Permissions(PermissionsCommand::Set {
            app: PathBuf::from("site"),
            right: Right::scoped("app.env", "HOME"),
            decision: Decision::Substitute,
            confirm_red: true,
        })
    );
    assert!(USAGE.contains("--confirm-red"));
    for bad in [
        &["permissions"][..],
        &["permissions", "grant"][..],
        &["permissions", "set", "site", "app.env:HOME"][..],
        &[
            "permissions",
            "set",
            "site",
            "app.env:HOME",
            "substitute",
            "--confirm-red",
            "--confirm-red",
        ][..],
        &["permissions", "set", "site", "not a right", "allow"][..],
        &["permissions", "set", "site", "secrets", "maybe"][..],
        &["permissions", "set"][..],
        &["permissions", "reset"][..],
        &["permissions", "reset", "site", "extra"][..],
        &["permissions", "list", "site", "extra"][..],
    ] {
        assert!(parse(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn ai_limits_and_log_parse() {
    assert_eq!(
        parse(&[
            "ai",
            "limits",
            "somewhere",
            "--requests",
            "5",
            "--tokens",
            "0"
        ])
        .unwrap(),
        Command::Ai(AiCommand::Limits {
            app: PathBuf::from("somewhere"),
            requests: Some(5),
            tokens: Some(0),
        })
    );
    assert_eq!(
        parse(&["ai", "log", "somewhere"]).unwrap(),
        Command::Ai(AiCommand::Log {
            app: PathBuf::from("somewhere"),
        })
    );
    for bad in [
        &["ai", "limits", "site", "--requests", "5", "--requests", "6"][..],
        &["ai", "limits", "site", "--wat", "1"][..],
        &["ai", "history", "site"][..],
        &["ai", "log"][..],
        &["ai", "log", "site", "extra"][..],
        &["ai", "limits", "site", "--requests"][..],
    ] {
        assert!(parse(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn the_development_server_must_be_loopback_http_without_credentials() {
    for bad in [
        "https://127.0.0.1:3000/",
        "http://localhost:3000/",
        "http://192.168.0.1:3000/",
        "http://user:pass@127.0.0.1:3000/",
        "http://user@127.0.0.1:3000/",
        "ftp://127.0.0.1/",
        "not a url",
    ] {
        assert!(
            parse(&["--app", "x", "--dev-url", bad]).is_err(),
            "accepted {bad}"
        );
    }
}

#[test]
fn a_target_shaped_like_an_installed_id_parses_as_a_path_and_the_usage_names_the_id_form() {
    let Command::Run(run) = parse(&["run", "org.example.app"]).unwrap() else {
        panic!("run")
    };
    assert_eq!(run.target, PathBuf::from("org.example.app"));
    assert!(USAGE.contains("<PATH|ID>"));
    assert!(USAGE.contains("`alef install`"));
}

#[test]
fn only_the_positional_run_form_carries_the_installed_id_target() {
    let Command::Run(app) = parse(&["--app", "org.example.app"]).unwrap() else {
        panic!("run")
    };
    assert!(!app.positional);
    let Command::Run(run) = parse(&["run", "org.example.app"]).unwrap() else {
        panic!("run")
    };
    assert!(run.positional);
}

#[test]
fn after_run_the_internal_command_names_are_targets_not_subcommands() {
    for name in ["trust", "consent"] {
        let Command::Run(run) = parse(&["run", name]).unwrap() else {
            panic!("run {name}")
        };
        assert_eq!(run.target, PathBuf::from(name));
        assert!(run.positional, "{name}");
    }
    assert!(matches!(
        parse(&["trust", "a", "b"]),
        Ok(Command::Trust {
            request: _,
            answer: _
        })
    ));
    assert!(matches!(
        parse(&["consent", "a", "b"]),
        Ok(Command::Consent {
            request: _,
            answer: _
        })
    ));
}

#[test]
fn broker_takes_only_the_no_tray_flag() {
    assert!(matches!(
        parse(&["broker"]),
        Ok(Command::Broker { no_tray: false })
    ));
    assert!(matches!(
        parse(&["broker", "--no-tray"]),
        Ok(Command::Broker { no_tray: true })
    ));
    for bad in [
        vec!["broker", "--json"],
        vec!["broker", "extra"],
        vec!["broker", "--no-tray", "--no-tray"],
    ] {
        assert!(parse(&bad).is_err(), "{bad:?}");
    }
}

#[test]
fn warm_spare_is_strict_but_not_advertised() {
    assert!(matches!(
        parse(&[
            "warm-spare",
            "--kind",
            "windowed",
            "--spare-id",
            "7",
            r"\.\pipe\alef-broker-broker-0123456789abcdef"
        ]),
        Ok(Command::WarmSpare {
            kind: SpareKind::Windowed,
            spare_id: 7,
            endpoint: _
        })
    ));
    assert!(matches!(
        parse(&[
            "warm-spare",
            "--kind",
            "windowless",
            "--spare-id",
            "0",
            "/tmp/alef-1000/broker-x/broker.sock"
        ]),
        Ok(Command::WarmSpare {
            kind: SpareKind::Windowless,
            spare_id: 0,
            endpoint: _
        })
    ));
    for bad in [
        vec!["warm-spare"],
        vec!["warm-spare", "--kind", "windowed"],
        vec!["warm-spare", "--kind", "windowed", "--spare-id", "1"],
        vec!["warm-spare", "--kind", "quiet", "--spare-id", "1", "ep"],
        vec!["warm-spare", "--kind", "windowed", "--spare-id", "x", "ep"],
        vec![
            "warm-spare",
            "--kind",
            "windowed",
            "--spare-id",
            "1",
            "ep",
            "x",
        ],
        vec![
            "warm-spare",
            "--kind",
            "windowed",
            "--kind",
            "windowless",
            "ep",
        ],
        vec!["warm-spare", "--spare-id", "1", "--spare-id", "2", "ep"],
        vec!["warm-spare", "--quiet", "ep"],
    ] {
        assert!(parse(&bad).is_err(), "{bad:?}");
    }
    assert!(!USAGE.contains("warm-spare"));
}

#[test]
fn warm_takes_one_verb_and_the_json_flag() {
    for (word, expected) in [
        ("status", WarmCommand::Status),
        ("start", WarmCommand::Start),
        ("stop", WarmCommand::Stop),
        ("enable", WarmCommand::Enable),
        ("disable", WarmCommand::Disable),
    ] {
        assert!(matches!(
            parse(&["warm", word]),
            Ok(Command::Warm {
                command,
                json: false
            }) if command == expected
        ));
        assert!(matches!(
            parse(&["warm", word, "--json"]),
            Ok(Command::Warm {
                command,
                json: true
            }) if command == expected
        ));
    }
    for bad in [
        vec!["warm"],
        vec!["warm", "restart"],
        vec!["warm", "status", "--json", "--json"],
        vec!["warm", "status", "extra"],
        vec!["warm", "status", "--unknown"],
    ] {
        assert!(parse(&bad).is_err(), "{bad:?}");
    }
}
