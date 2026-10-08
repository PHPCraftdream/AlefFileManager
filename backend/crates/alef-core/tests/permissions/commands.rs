// SPDX-License-Identifier: MIT OR Apache-2.0
//! Declared commands and sidecar programs: parsing, template expansion, the rights they make.
use super::{allowed, build, closed, vars};
use alef_core::{
    security::{
        command::{ArgTemplate, DeclaredCommand, ExpandError, Program},
        consent::{Consent, Decision, Right},
        grants::Grants,
        manifest::{CliCommand, Manifest, Permissions},
        permissions::{Permission, PermissionSet},
    },
    AlefError, ErrorCode,
};
use std::collections::BTreeMap;

const MINIMAL: &str = include_str!("../fixtures/minimal.ktav");

fn command(name: &str, program: &str, args: &[&str]) -> CliCommand {
    CliCommand {
        name: name.into(),
        program: program.into(),
        args: args.iter().map(|arg| arg.to_string()).collect(),
        description: "Shows the state of the folder".into(),
    }
}

fn policy(commands: Vec<CliCommand>) -> Permissions {
    let mut policy = closed();
    policy.cli.commands = commands;
    policy
}

fn set_of(commands: Vec<CliCommand>) -> PermissionSet {
    build(&policy(commands))
}

fn status() -> CliCommand {
    command("status", "git", &["status", "--short", "{path}"])
}

fn declared(raw: CliCommand) -> DeclaredCommand {
    let name = raw.name.clone();
    set_of(vec![raw]).command(&name).unwrap().clone()
}

fn params(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

fn path_of(error: &AlefError) -> &str {
    assert_eq!(error.code, ErrorCode::ManifestInvalid);
    error.details.as_ref().unwrap()["path"].as_str().unwrap()
}

fn manifest_with_cli(cli: &str) -> Result<Manifest, AlefError> {
    let text = MINIMAL
        .replace("\r\n", "\n")
        .replace("exec: []", &format!("exec: []\n{cli}"));
    Manifest::from_ktav_str(&text)
}

#[test]
fn a_manifest_without_commands_declares_none_and_serializes_without_the_field() {
    let manifest = Manifest::from_ktav_str(MINIMAL).unwrap();
    assert!(manifest.permissions.cli.commands.is_empty());
    let json = serde_json::to_value(&manifest).unwrap();
    assert!(json["permissions"]["cli"].get("commands").is_none());
}

#[test]
fn declared_commands_are_read_from_the_ktav_manifest() {
    let manifest = manifest_with_cli(
        "        commands: [
            {
                name: status
                program: git
                args: [
                    status
                    --short
                    :: {path}
                    a{b}
                ]
                description: Shows the state of the folder
            }
            {
                name: tool-2
                program: sidecar:my-tool
                args: []
                description: Runs the tool
            }
        ]",
    )
    .unwrap();
    let commands = &manifest.permissions.cli.commands;
    assert_eq!(commands.len(), 2);
    assert_eq!(commands[0].args, ["status", "--short", "{path}", "a{b}"]);
    assert_eq!(commands[1].program, "sidecar:my-tool");
    let set = build(&manifest.permissions);
    let status = set.command("status").unwrap();
    assert_eq!(status.program, Program::Plain("git".into()));
    assert_eq!(
        status.args,
        [
            ArgTemplate::Literal("status".into()),
            ArgTemplate::Literal("--short".into()),
            ArgTemplate::Param("path".into()),
            ArgTemplate::Literal("a{b}".into()),
        ]
    );
    assert_eq!(
        set.command("tool-2").unwrap().program,
        Program::Sidecar("my-tool".into())
    );
}

#[test]
fn commands_only_manifests_default_exec_to_empty_without_granting_exec() {
    let cli = "commands: [
            {
                name: status
                program: git
                args: [
                    :: {bad_name}
                    :: {}
                    :: {Path}
                    :: {path}
                ]
                description: Shows the state of the folder
            }
        ]";
    let text = MINIMAL.replace("exec: []", cli);
    let manifest = Manifest::from_ktav_str(&text).unwrap();
    assert!(manifest.permissions.cli.exec.is_empty());
    let set = build(&manifest.permissions);
    assert!(allowed(&set, Permission::CliCommand, Some("status")));
    assert!(!allowed(&set, Permission::CliExec, Some("git")));
    assert_eq!(
        set.command("status")
            .unwrap()
            .expand(&params(&[("path", "src")]))
            .unwrap(),
        ["{bad_name}", "{}", "{Path}", "src"]
    );
    let mut json = serde_json::to_value(&manifest).unwrap();
    json["permissions"]["cli"]
        .as_object_mut()
        .unwrap()
        .remove("exec");
    let from_json: Manifest = serde_json::from_value(json).unwrap();
    assert_eq!(from_json, manifest);
    let round_trip: Manifest =
        serde_json::from_value(serde_json::to_value(&manifest).unwrap()).unwrap();
    assert_eq!(round_trip, manifest);
}

#[test]
fn an_invalid_declared_command_makes_the_manifest_invalid_with_the_path() {
    let error = manifest_with_cli(
        "        commands: [
            {
                name: ok
                program: git
                args: []
                description: Fine
            }
            {
                name: Bad
                program: git
                args: []
                description: Not fine
            }
        ]",
    )
    .unwrap_err();
    assert_eq!(path_of(&error), "permissions.cli.commands[1].name");
    assert!(error
        .message
        .starts_with("permissions.cli.commands[1].name:"));
}

#[test]
fn every_invalid_part_of_a_declared_command_is_named_by_its_path() {
    let with = |change: fn(&mut CliCommand)| {
        let mut raw = status();
        change(&mut raw);
        raw
    };
    let cases: Vec<(CliCommand, &str)> = vec![
        (with(|c| c.name = "".into()), "name"),
        (with(|c| c.name = "Status".into()), "name"),
        (with(|c| c.name = "sTatus".into()), "name"),
        (with(|c| c.name = "-x".into()), "name"),
        (with(|c| c.name = "a_b".into()), "name"),
        (with(|c| c.name = "a b".into()), "name"),
        (with(|c| c.program = "".into()), "program"),
        (with(|c| c.program = "*".into()), "program"),
        (with(|c| c.program = "bin/git".into()), "program"),
        (with(|c| c.program = "git\0".into()), "program"),
        (with(|c| c.program = "sidecar:".into()), "program"),
        (with(|c| c.program = "sidecar:..".into()), "program"),
        (with(|c| c.program = "sidecar:a/b".into()), "program"),
        (with(|c| c.program = "sidecar:a\\b".into()), "program"),
        (with(|c| c.program = "sidecar:.x".into()), "program"),
        (with(|c| c.program = "sidecar:-x".into()), "program"),
        (with(|c| c.args[0] = "a\0b".into()), "args[0]"),
        (with(|c| c.description = "".into()), "description"),
        (with(|c| c.description = "  ".into()), "description"),
        (with(|c| c.description = "a\nb".into()), "description"),
    ];
    for (raw, field) in cases {
        let error = PermissionSet::from_manifest(&policy(vec![raw.clone()]), &vars()).unwrap_err();
        assert_eq!(
            path_of(&error),
            format!("permissions.cli.commands[0].{field}"),
            "{raw:?}"
        );
    }
}

#[test]
fn only_exact_valid_parameter_elements_are_substituted() {
    let literals = [
        "{bad_name}",
        "{}",
        "{Path}",
        "{a b}",
        "{{x}}",
        "{0path}",
        "{é}",
        "{aé}",
        "{a-b}",
        "{a}{b}",
        "a{b}",
        "{b}c",
        "{",
        "}",
        "{b",
        "--x={y}",
        "",
    ];
    let literal_command = declared(command("x", "git", &literals));
    assert_eq!(
        literal_command.args,
        literals.map(|text| ArgTemplate::Literal(text.into()))
    );
    assert!(literal_command.params().is_empty());
    assert_eq!(literal_command.expand(&params(&[])).unwrap(), literals);
    assert_eq!(
        literal_command
            .expand(&params(&[("bad_name", "value")]))
            .unwrap_err(),
        ExpandError::UnknownParam("bad_name".into())
    );

    let command = declared(command("x", "git", &["{a}", "{pathA0}"]));
    assert_eq!(command.params(), ["a", "pathA0"]);
    assert_eq!(
        command
            .expand(&params(&[("a", "one"), ("pathA0", "two")]))
            .unwrap(),
        ["one", "two"]
    );
}

#[test]
fn a_command_name_must_be_unique_in_the_list() {
    let error = PermissionSet::from_manifest(
        &policy(vec![status(), command("other", "ls", &[]), status()]),
        &vars(),
    )
    .unwrap_err();
    assert_eq!(path_of(&error), "permissions.cli.commands[2].name");
}

#[test]
fn a_program_is_a_bare_name_or_an_absolute_path() {
    let absolute = if cfg!(windows) {
        "C:\\tools\\git.exe"
    } else {
        "/usr/bin/git"
    };
    assert_eq!(
        declared(command("a", absolute, &[])).program,
        Program::Plain(absolute.into())
    );
    assert_eq!(
        declared(command("a", "git", &[])).program.to_string(),
        "git"
    );
    assert_eq!(
        declared(command("a", "sidecar:tool", &[]))
            .program
            .to_string(),
        "sidecar:tool"
    );
}

#[test]
fn the_template_replaces_params_and_keeps_literals() {
    let command = declared(command(
        "c",
        "git",
        &["log", "--format={x}", "{count}", "{path}", "{count}"],
    ));
    assert_eq!(command.params(), ["count", "path"]);
    assert_eq!(
        command
            .expand(&params(&[("path", "/a b/c"), ("count", "3")]))
            .unwrap(),
        ["log", "--format={x}", "3", "/a b/c", "3"]
    );
    let value = "$(rm -rf *) ; `x` \"q\" {path}";
    assert_eq!(
        command
            .expand(&params(&[("path", value), ("count", "")]))
            .unwrap()[2..4],
        ["", value],
        "values are not interpreted, split or expanded again"
    );
}

#[test]
fn a_template_without_params_expands_to_its_literals() {
    let command = declared(command("c", "ls", &["-l"]));
    assert_eq!(command.expand(&params(&[])).unwrap(), ["-l"]);
    assert_eq!(
        command.expand(&params(&[("x", "1")])).unwrap_err(),
        ExpandError::UnknownParam("x".into())
    );
}

#[test]
fn a_missing_or_unknown_or_nul_param_is_an_invalid_argument() {
    let command = declared(status());
    let missing = command.expand(&params(&[])).unwrap_err();
    assert_eq!(missing, ExpandError::MissingParam("path".into()));
    let unknown = command
        .expand(&params(&[("path", "x"), ("extra", "y")]))
        .unwrap_err();
    assert_eq!(unknown, ExpandError::UnknownParam("extra".into()));
    let nul = command.expand(&params(&[("path", "a\0b")])).unwrap_err();
    assert_eq!(nul, ExpandError::NulInParam("path".into()));
    for error in [missing, unknown, nul] {
        let alef: AlefError = error.clone().into();
        assert_eq!(alef.code, ErrorCode::InvalidArgument);
        assert!(alef.message.contains(&error.to_string()));
        assert!(alef.details.is_some());
    }
}

#[test]
fn only_a_listed_command_may_run_and_the_denial_is_the_uniform_one() {
    let set = set_of(vec![status(), command("other", "ls", &[])]);
    let check = |name: Option<&str>| set.check(Permission::CliCommand, name, &Grants::new());
    assert_eq!(check(Some("status")).unwrap(), Decision::Allow);
    assert_eq!(check(Some("other")).unwrap(), Decision::Allow);
    for no in [
        Some("nope"),
        Some("Status"),
        Some(""),
        Some("status "),
        None,
    ] {
        let error = check(no).unwrap_err();
        assert_eq!(error.code, ErrorCode::PermissionDenied, "{no:?}");
        assert_eq!(error.details.unwrap()["permission"], "cli.command");
    }
}

#[test]
fn a_declared_command_is_a_right_of_its_own_with_the_three_decisions() {
    let right = Right::scoped("cli.command", "status");
    let set = set_of(vec![status(), command("other", "ls", &[])]);
    assert!(set.rights().contains(&right));
    assert!(set
        .rights()
        .contains(&Right::scoped("cli.command", "other")));
    for (decision, ok) in [
        (Decision::Allow, true),
        (Decision::Substitute, true),
        (Decision::Deny, false),
    ] {
        let mut consent = Consent::undecided();
        consent.set(right.clone(), decision);
        consent.set(Right::scoped("cli.command", "other"), Decision::Allow);
        let set = set.clone().with_consent(consent);
        let got = set.check(Permission::CliCommand, Some("status"), &Grants::new());
        assert_eq!(got.is_ok(), ok, "{decision:?}");
        if ok {
            assert_eq!(got.unwrap(), decision);
        }
        assert_eq!(
            set.check(Permission::CliCommand, Some("other"), &Grants::new())
                .unwrap(),
            Decision::Allow,
            "the other command is its own right"
        );
    }
    assert_eq!(right.to_string(), "cli.command:status");
    assert_eq!("cli.command:status".parse::<Right>().unwrap(), right);
    assert_eq!(right.risk(), None);
}

#[test]
fn an_undecided_command_is_denied_for_a_user_who_was_not_asked() {
    let set = set_of(vec![status()]).with_consent(Consent::undecided());
    assert!(!allowed(&set, Permission::CliCommand, Some("status")));
}

#[test]
fn exec_does_not_open_declared_commands_and_commands_do_not_open_exec() {
    let mut open = policy(vec![status()]);
    open.cli.exec = vec!["*".into()];
    let set = build(&open);
    assert!(allowed(&set, Permission::CliExec, Some("git")));
    assert!(!allowed(&set, Permission::CliCommand, Some("nope")));
    assert!(!allowed(&set, Permission::CliCommand, Some("git")));

    let only_commands = set_of(vec![status()]);
    assert!(allowed(
        &only_commands,
        Permission::CliCommand,
        Some("status")
    ));
    for target in ["git", "status", "*", "sidecar:tool"] {
        assert!(!allowed(&only_commands, Permission::CliExec, Some(target)));
    }
}

#[test]
fn sidecar_entries_of_exec_match_exactly_that_sidecar() {
    let mut p = closed();
    p.cli.exec = vec!["sidecar:tool".into(), "git".into()];
    let set = build(&p);
    assert!(allowed(&set, Permission::CliExec, Some("sidecar:tool")));
    assert!(allowed(&set, Permission::CliExec, Some("git")));
    for no in [
        "sidecar:other",
        "sidecar:",
        "sidecar:..",
        "sidecar:tool/x",
        "sidecar:tool\n",
        "tool",
        "Sidecar:tool",
        "sidecar:git",
        "sidecar:tool ",
    ] {
        assert!(!allowed(&set, Permission::CliExec, Some(no)), "{no:?}");
    }
    assert!(set
        .rights()
        .contains(&Right::scoped("cli.exec", "sidecar:tool")));
}

#[test]
fn a_bare_name_does_not_allow_the_sidecar_of_that_name_and_vice_versa() {
    let mut p = closed();
    p.cli.exec = vec!["foo".into()];
    let bare = build(&p);
    assert!(allowed(&bare, Permission::CliExec, Some("foo")));
    assert!(!allowed(&bare, Permission::CliExec, Some("sidecar:foo")));
    p.cli.exec = vec!["sidecar:foo".into()];
    let sidecar = build(&p);
    assert!(allowed(&sidecar, Permission::CliExec, Some("sidecar:foo")));
    assert!(!allowed(&sidecar, Permission::CliExec, Some("foo")));
}

#[test]
fn a_star_in_exec_allows_every_sidecar() {
    let mut p = closed();
    p.cli.exec = vec!["*".into()];
    let set = build(&p);
    assert!(allowed(&set, Permission::CliExec, Some("sidecar:anything")));
}

#[test]
fn a_bad_sidecar_name_in_exec_makes_the_manifest_invalid() {
    for bad in [
        "sidecar:",
        "sidecar:..",
        "sidecar:a/b",
        "sidecar:.x",
        "sidecar:a b",
    ] {
        let mut p = closed();
        p.cli.exec = vec!["git".into(), bad.into()];
        let error = PermissionSet::from_manifest(&p, &vars()).unwrap_err();
        assert_eq!(path_of(&error), "permissions.cli.exec[1]", "{bad}");
    }
    let text = MINIMAL.replace("\r\n", "\n").replace(
        "exec: []",
        "exec: [\n        git\n        sidecar:a/b\n    ]",
    );
    let error = Manifest::from_ktav_str(&text).unwrap_err();
    assert_eq!(path_of(&error), "permissions.cli.exec[1]");
}

#[test]
fn the_listing_shows_the_description_and_the_command_line() {
    let set = set_of(vec![
        status(),
        command("say", "sidecar:tool", &["", "a b", "{x}"]),
    ]);
    assert_eq!(
        set.describe(&Right::scoped("cli.command", "status"))
            .unwrap(),
        "status — Shows the state of the folder (git status --short {path})"
    );
    assert_eq!(
        set.describe(&Right::scoped("cli.command", "say")).unwrap(),
        "say — Shows the state of the folder (sidecar:tool \"\" \"a b\" {x})"
    );
    let escaped = set_of(vec![command("esc", "tool", &["a\\b c", "q\"x"])]);
    assert_eq!(
        escaped
            .describe(&Right::scoped("cli.command", "esc"))
            .unwrap(),
        "esc — Shows the state of the folder (tool \"a\\\\b c\" \"q\\\"x\")"
    );
    for right in [
        Right::scoped("cli.command", "nope"),
        Right::scoped("cli.exec", "status"),
        Right::plain("cli.command"),
        Right::plain("secrets"),
    ] {
        assert_eq!(set.describe(&right), None, "{right}");
    }
}
