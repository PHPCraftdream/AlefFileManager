// SPDX-License-Identifier: MIT OR Apache-2.0
//! A build with the end-to-end hooks says so on stderr, once, and only when ALEF_E2E=1 asks for
//! the hooks; a plain build never says it.
use std::process::Command;

const NOTICE: &str = "this build carries the end-to-end hooks";

fn stderr_when(mode: Option<&str>) -> String {
    let mut command = Command::new(env!("CARGO_BIN_EXE_alef"));
    command.arg("--help").env_remove("ALEF_E2E");
    if let Some(mode) = mode {
        command.env("ALEF_E2E", mode);
    }
    let output = command.output().expect("the launcher runs");
    assert!(output.status.success(), "{output:?}");
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn the_launcher_names_the_hooks_once_and_only_when_a_build_with_them_is_asked() {
    let hooks = usize::from(cfg!(feature = "e2e"));
    assert_eq!(stderr_when(Some("1")).matches(NOTICE).count(), hooks);
    assert_eq!(stderr_when(Some("0")).matches(NOTICE).count(), 0);
    assert_eq!(stderr_when(None).matches(NOTICE).count(), 0);
}
