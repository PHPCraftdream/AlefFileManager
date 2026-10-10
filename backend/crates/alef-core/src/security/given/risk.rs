// SPDX-License-Identifier: MIT OR Apache-2.0
//! What makes a right risky, read from the words of its scope (`Right::risk`).

/// A scope that is the whole disk, the whole home folder, or a folder right under the root of a disk
/// (`/home/**`, `C:/Users/**`) that holds the homes of everyone. The scope is read as the words it
/// is written in, so `.` and doubled separators are dropped and `..` is risky whatever it leads to.
pub(super) fn reaches_everything(scope: &str) -> bool {
    let scope = scope.replace('\\', "/");
    let parts: Vec<&str> = scope
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect();
    if parts.contains(&"..") {
        return true;
    }
    let wildcard = parts.iter().position(|part| part.contains('*'));
    let literal = &parts[..wildcard.unwrap_or(parts.len())];
    let drive = literal.first().is_some_and(|first| {
        first.len() == 2 && first.as_bytes()[0].is_ascii_alphabetic() && first.ends_with(':')
    });
    let literal = if drive { &literal[1..] } else { literal };
    let rooted = drive || scope.starts_with('/');
    match literal {
        [] | ["$HOME"] => true,
        [_] => rooted && wildcard.is_some(),
        _ => false,
    }
}

/// A shell or an interpreter: allowing it allows whatever it is told to run.
pub(super) fn runs_other_programs(scope: &str) -> bool {
    const NAMES: &[&str] = &[
        "sh",
        "bash",
        "zsh",
        "fish",
        "dash",
        "ksh",
        "csh",
        "tcsh",
        "cmd",
        "powershell",
        "pwsh",
        "wsl",
        "python",
        "py",
        "pyw",
        "node",
        "nodejs",
        "deno",
        "bun",
        "perl",
        "ruby",
        "php",
        "lua",
        "java",
        "wscript",
        "cscript",
        "mshta",
        "osascript",
        "env",
        "xargs",
        "sudo",
        "start",
    ];
    let scope = scope.replace('\\', "/").to_ascii_lowercase();
    let name = scope.rsplit('/').next().unwrap_or_default();
    let name = name.strip_prefix("sidecar:").unwrap_or(name);
    let stem = ["exe", "cmd", "bat", "com"]
        .iter()
        .find_map(|extension| name.strip_suffix(&format!(".{extension}")))
        .unwrap_or(name);
    // `python3` and `python3.12` are `python`.
    NAMES.contains(&stem.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.'))
}

/// A URL scope whose host is a wildcard.
pub(super) fn any_address(scope: &str) -> bool {
    let Some((_, rest)) = scope.split_once("://") else {
        return false;
    };
    rest.split('/').next() == Some("*")
}
