// SPDX-License-Identifier: MIT OR Apache-2.0
//! Application manager command line parsing.
use std::ffi::OsString;

use alef_launch::args::{parse_args, Command, USAGE};

fn parse(args: &[&str]) -> Result<Command, String> {
    parse_args(args.iter().map(OsString::from))
}

#[test]
fn apps_ui_is_a_distinct_mode_exclusive_of_json() {
    assert_eq!(parse(&["apps", "--ui"]).unwrap(), Command::AppsUi);
    assert_eq!(
        parse(&["apps", "--json"]).unwrap(),
        Command::Apps { json: true }
    );
    assert_eq!(parse(&["apps"]).unwrap(), Command::Apps { json: false });
    for args in [
        &["apps", "--ui", "--json"][..],
        &["apps", "--json", "--ui"][..],
        &["apps", "--ui", "x"][..],
    ] {
        assert!(parse(args).is_err());
    }
    assert!(USAGE.contains("alef apps [--json|--ui]"));
}
