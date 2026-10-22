// SPDX-License-Identifier: MIT OR Apache-2.0
//! Windows Run-value command line: render and parse per the documented
//! `CommandLineToArgvW` rules. Pure string work, compiled on every OS so the
//! round trip is testable everywhere.

use std::ffi::OsString;
use std::path::PathBuf;

use super::entry::Entry;
use super::AutostartError;

/// Render `entry` as a single Run registry value (arguments separated by
/// spaces, each argument quoted per `CommandLineToArgvW`).
pub fn render_run_value(entry: &Entry) -> Result<String, AutostartError> {
    entry.validate()?;
    let mut out = String::new();
    for (i, part) in std::iter::once(entry.exe.as_os_str())
        .chain(entry.args.iter().map(|a| a.as_os_str()))
        .enumerate()
    {
        if i > 0 {
            out.push(' ');
        }
        // Validation above guarantees valid UTF-8.
        out.push_str(&quote_run(part.to_str().expect("validated UTF-8")));
    }
    Ok(out)
}

/// Parse a Run value back into an entry. Any non-empty parse is accepted:
/// ownership (the marker argument) is decided by the caller.
pub fn parse_run_value(raw: &str) -> Result<Entry, AutostartError> {
    let tokens = tokenize_run(raw).ok_or(AutostartError::Damaged)?;
    let (exe, args) = tokens.split_first().ok_or(AutostartError::Damaged)?;
    Ok(Entry::new(
        PathBuf::from(exe),
        args.iter().map(OsString::from).collect(),
    ))
}

fn quote_run(arg: &str) -> String {
    let needs_quotes = arg.is_empty() || arg.bytes().any(|b| b == b' ' || b == b'\t' || b == b'"');
    if !needs_quotes {
        return arg.to_string();
    }
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    let mut backslashes = 0usize;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                // 2n+1 backslashes make the quote literal inside quotes.
                for _ in 0..=backslashes * 2 {
                    out.push('\\');
                }
                backslashes = 0;
                out.push('"');
            }
            _ => {
                for _ in 0..backslashes {
                    out.push('\\');
                }
                backslashes = 0;
                out.push(c);
            }
        }
    }
    // Backslashes right before the closing quote are doubled.
    for _ in 0..backslashes * 2 {
        out.push('\\');
    }
    out.push('"');
    out
}

/// Exact inverse of `quote_run` plus plain tokenization; `None` only for an
/// empty token list. An unterminated quote stays open to the end of the
/// value, matching `CommandLineToArgvW`.
fn tokenize_run(raw: &str) -> Option<Vec<String>> {
    let mut tokens = Vec::new();
    let mut cur = String::new();
    let mut has_token = false;
    let mut in_quotes = false;
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' | '\n' | '\r' if !in_quotes => {
                if has_token {
                    tokens.push(std::mem::take(&mut cur));
                    has_token = false;
                }
            }
            '\\' => {
                let mut run = 1usize;
                while chars.peek() == Some(&'\\') {
                    chars.next();
                    run += 1;
                }
                if chars.peek() == Some(&'"') {
                    for _ in 0..run / 2 {
                        cur.push('\\');
                    }
                    if run % 2 == 1 {
                        // Odd run: the quote is literal and does not toggle.
                        chars.next();
                        cur.push('"');
                    } else {
                        in_quotes = !in_quotes;
                    }
                } else {
                    for _ in 0..run {
                        cur.push('\\');
                    }
                }
                has_token = true;
            }
            '"' => {
                in_quotes = !in_quotes;
                has_token = true;
            }
            _ => {
                cur.push(c);
                has_token = true;
            }
        }
    }
    if has_token {
        tokens.push(cur);
    }
    if tokens.is_empty() {
        None
    } else {
        Some(tokens)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(parts: &[&str]) {
        // An exe that is absolute on every platform (render validates it).
        let exe = std::env::temp_dir().join(parts[0].replace(['/', '\\'], "_"));
        let entry = Entry::new(exe, parts[1..].iter().map(OsString::from).collect());
        let rendered = render_run_value(&entry).expect("render");
        let parsed = parse_run_value(&rendered).expect("parse");
        assert_eq!(parsed, entry, "round trip failed for {rendered:?}");
    }

    #[test]
    fn round_trip_plain_arguments() {
        round_trip(&["/x/alef", "--broker", "run"]);
    }

    #[test]
    fn round_trip_spaces_and_tabs() {
        round_trip(&["/x/alef exe", "a b", "\tc"]);
        round_trip(&["/x/alef", " lead", "trail "]);
    }

    #[test]
    fn round_trip_quotes() {
        round_trip(&["/x/alef", "he said \"hi\""]);
        round_trip(&["/x/alef", "\""]);
        round_trip(&["/x/alef", "ends with \""]);
    }

    #[test]
    fn round_trip_backslashes() {
        round_trip(&["C:\\Program Files\\alef.exe"]);
        round_trip(&["/x/alef", "C:\\path\\"]);
        round_trip(&["/x/alef", "\\\\server\\share\\"]);
        round_trip(&["/x/alef", "\\\"quoted\\\""]);
    }

    #[test]
    fn round_trip_special_and_unicode() {
        round_trip(&["/x/alef", "&", "<", ">", "|"]);
        round_trip(&["/x/alef", "кириллица", "日本語", "emoji 😀"]);
        round_trip(&["/x/alef", ""]);
    }

    #[test]
    fn empty_value_is_damaged() {
        assert_eq!(parse_run_value(""), Err(AutostartError::Damaged));
        assert_eq!(parse_run_value("   "), Err(AutostartError::Damaged));
    }

    #[test]
    fn invalid_entries_are_rejected_on_render() {
        let relative = Entry::new(PathBuf::from("alef"), vec![]);
        assert!(matches!(
            render_run_value(&relative),
            Err(AutostartError::InvalidEntry(_))
        ));
        let nul = Entry::new(PathBuf::from("/x/a"), vec![OsString::from("b\0")]);
        assert!(matches!(
            render_run_value(&nul),
            Err(AutostartError::InvalidEntry(_))
        ));
    }

    #[test]
    fn unterminated_quote_consumes_rest_like_commandline_to_argv() {
        let parsed = parse_run_value("/x/alef \"open arg").expect("parse");
        assert_eq!(parsed.exe, PathBuf::from("/x/alef"));
        assert_eq!(parsed.args, vec![OsString::from("open arg")]);
    }

    #[test]
    fn quoted_token_continues_after_closing_quote() {
        // CommandLineToArgvW: a"b c"d is one token `ab cd`.
        let parsed = parse_run_value("/x/alef a\"b c\"d").expect("parse");
        assert_eq!(parsed.args, vec![OsString::from("ab cd")]);
    }
}
