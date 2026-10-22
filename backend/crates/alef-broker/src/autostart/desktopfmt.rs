// SPDX-License-Identifier: MIT OR Apache-2.0
//! Linux XDG autostart `.desktop` record: render and a strict parser of
//! exactly our own shape, with the ownership marker comment.

use std::ffi::OsString;
use std::path::PathBuf;

use super::entry::Entry;
use super::AutostartError;
use super::Parsed;

pub const DESKTOP_NAME: &str = "Alef Runtime";

/// Render the `.desktop` file. The first line is the ownership marker
/// comment; its absence on parse means the file is not ours.
pub fn render_desktop(marker: &str, entry: &Entry) -> Result<String, AutostartError> {
    entry.validate()?;
    if marker.is_empty() || marker.contains('\n') || marker.contains('\r') {
        return Err(AutostartError::InvalidEntry(
            "owner marker must be a single non-empty line",
        ));
    }
    let mut out = String::with_capacity(256);
    out.push_str(marker);
    out.push('\n');
    out.push_str("[Desktop Entry]\n");
    out.push_str("Type=Application\n");
    out.push_str("Name=");
    out.push_str(DESKTOP_NAME);
    out.push('\n');
    out.push_str("Exec=");
    out.push_str(&render_exec(entry));
    out.push('\n');
    out.push_str("NoDisplay=true\n");
    out.push_str("Terminal=false\n");
    out.push_str("X-GNOME-Autostart-enabled=true\n");
    Ok(out)
}

/// Parse a `.desktop` file. A structurally valid file without our marker is
/// `Foreign`; anything unparseable (including a missing header or Exec) is
/// `Damaged`.
pub fn parse_desktop(marker: &str, raw: &str) -> Parsed {
    let Some(tokens) = parse_desktop_shape(raw) else {
        return Parsed::Damaged;
    };
    if !raw.lines().any(|l| l == marker) {
        return Parsed::Foreign;
    }
    let (exe, args) = tokens.split_first().expect("non-empty checked");
    Parsed::Entry(Entry::new(
        PathBuf::from(exe),
        args.iter().map(OsString::from).collect(),
    ))
}

/// Header and Exec line present, Exec tokenizes; the rest of the keys is
/// tolerated so that a foreign (but valid) desktop file stays `Foreign`.
fn parse_desktop_shape(raw: &str) -> Option<Vec<String>> {
    let mut has_header = false;
    let mut exec: Option<String> = None;
    for line in raw.lines() {
        let line = line.trim_end_matches('\r');
        if line.trim() == "[Desktop Entry]" {
            has_header = true;
        } else if let Some(value) = line.strip_prefix("Exec=") {
            if exec.is_none() {
                exec = Some(value.to_string());
            }
        }
    }
    if !has_header {
        return None;
    }
    parse_exec(&exec?)
}

fn render_exec(entry: &Entry) -> String {
    let mut out = String::new();
    for (i, arg) in std::iter::once(entry.exe.as_os_str())
        .chain(entry.args.iter().map(|a| a.as_os_str()))
        .enumerate()
    {
        if i > 0 {
            out.push(' ');
        }
        out.push('"');
        for c in arg.to_string_lossy().chars() {
            match c {
                '%' => out.push_str("%%"),
                '\\' => out.push_str("\\\\"),
                '"' => out.push_str("\\\""),
                '$' => out.push_str("\\$"),
                '`' => out.push_str("\\`"),
                _ => out.push(c),
            }
        }
        out.push('"');
    }
    out
}

/// Tokenize an Exec value: quotes group, backslash escapes the next
/// character (including inside quotes), `%%` folds to `%`.
fn parse_exec(s: &str) -> Option<Vec<String>> {
    let mut tokens = Vec::new();
    let mut cur = String::new();
    let mut has_token = false;
    let mut in_quotes = false;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' | '\n' if !in_quotes => {
                if has_token {
                    tokens.push(std::mem::take(&mut cur));
                    has_token = false;
                }
            }
            '\\' => {
                let next = chars.next()?;
                cur.push(next);
                has_token = true;
            }
            '"' => {
                in_quotes = !in_quotes;
                has_token = true;
            }
            '%' => match chars.next() {
                Some('%') => {
                    cur.push('%');
                    has_token = true;
                }
                Some(other) => {
                    // We never emit field codes; keep the character as-is.
                    cur.push('%');
                    cur.push(other);
                    has_token = true;
                }
                None => {
                    cur.push('%');
                    has_token = true;
                }
            },
            _ => {
                cur.push(c);
                has_token = true;
            }
        }
    }
    if has_token {
        tokens.push(cur);
    }
    // An unterminated quote makes the whole Exec value unparseable.
    if in_quotes || tokens.is_empty() {
        return None;
    }
    Some(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MARKER: &str = "# Owner: alef-runtime-broker (alef-broker autostart)";

    fn entry(exe: impl AsRef<std::path::Path>, args: &[&str]) -> Entry {
        Entry::new(
            PathBuf::from(exe.as_ref()),
            args.iter().map(OsString::from).collect(),
        )
    }

    fn abs(name: &str) -> PathBuf {
        // Absolute on every platform; render validates it.
        std::env::temp_dir().join(name)
    }

    fn round_trip(parts: &[&str]) {
        let entry = Entry::new(abs(&parts[0].replace(['/', '\\'], "_")), {
            parts[1..].iter().map(OsString::from).collect()
        });
        let rendered = render_desktop(MARKER, &entry).expect("render");
        assert_eq!(parse_desktop(MARKER, &rendered), Parsed::Entry(entry));
    }

    #[test]
    fn round_trip_plain() {
        round_trip(&["/usr/bin/alef", "--broker"]);
    }

    #[test]
    fn round_trip_percent_dollar_quotes_cyrillic_empty() {
        round_trip(&["/usr/bin/alef run", "100%", "$HOME", "he said \"hi\""]);
        round_trip(&["/opt/Алеф/алеф", "кириллица", ""]);
    }

    #[test]
    fn percent_is_doubled_in_exec() {
        let rendered = render_desktop(MARKER, &entry(abs("a"), &["50%"])).expect("render");
        // The Exec field doubles a backslash of a Windows path.
        let exe = abs("a").display().to_string().replace('\\', "\\\\");
        assert!(rendered.contains(&format!("Exec=\"{exe}\" \"50%%\"")));
        assert_eq!(
            parse_desktop(MARKER, &rendered),
            Parsed::Entry(entry(abs("a"), &["50%"]))
        );
    }

    #[test]
    fn all_escapes_survive_the_round_trip() {
        round_trip(&["/x/a", "\\", "`cmd`", "\"q\"", "$v"]);
    }

    #[test]
    fn file_without_marker_is_foreign() {
        let rendered = render_desktop(MARKER, &entry(abs("a"), &[])).expect("render");
        let foreign = rendered.replace(&format!("{MARKER}\n"), "");
        assert_eq!(parse_desktop(MARKER, &foreign), Parsed::Foreign);
    }

    #[test]
    fn garbage_is_damaged() {
        assert_eq!(parse_desktop(MARKER, ""), Parsed::Damaged);
        assert_eq!(parse_desktop(MARKER, "random text\n"), Parsed::Damaged);
        assert_eq!(
            parse_desktop(MARKER, "[Desktop Entry]\nExec=\"unterminated\n"),
            Parsed::Damaged
        );
        assert_eq!(
            parse_desktop(MARKER, "[Desktop Entry]\nType=Application\n"),
            Parsed::Damaged
        );
    }

    #[test]
    fn marker_line_must_be_single() {
        assert!(matches!(
            render_desktop("two\nlines", &entry(abs("a"), &[])),
            Err(AutostartError::InvalidEntry(_))
        ));
        assert!(matches!(
            render_desktop("", &entry(abs("a"), &[])),
            Err(AutostartError::InvalidEntry(_))
        ));
    }

    #[test]
    fn invalid_entry_is_rejected_on_render() {
        assert!(matches!(
            render_desktop(MARKER, &entry(PathBuf::from("relative"), &[])),
            Err(AutostartError::InvalidEntry(_))
        ));
    }
}
