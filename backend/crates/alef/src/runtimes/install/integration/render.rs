// SPDX-License-Identifier: MIT OR Apache-2.0
//! Pure serializers for desktop integration artifacts. No OS state is accessed here.
use std::{io, path::Path};

use super::Artifact;

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn path_text(path: &Path) -> io::Result<String> {
    let value = path
        .to_str()
        .ok_or_else(|| invalid("path is not valid UTF-8"))?;
    if value.contains('\0') {
        return Err(invalid("path contains a NUL character"));
    }
    Ok(value.to_owned())
}

fn desktop_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out
}

fn exec_token_inner(value: &str) -> String {
    let mut out = String::new();
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '$' => out.push_str("\\$"),
            '`' => out.push_str("\\`"),
            '%' => out.push_str("%%"),
            c => out.push(c),
        }
    }
    out
}

fn exec_token(value: &str) -> String {
    let quoted = format!("\"{}\"", exec_token_inner(value));
    desktop_value(&quoted)
}

/// Render a freedesktop desktop-entry file. Values are serialized as data, never shell text.
pub fn desktop_entry(artifact: &Artifact) -> io::Result<String> {
    let executable = path_text(&artifact.executable)?;
    let package = path_text(&artifact.package)?;
    let mut class = artifact.id.clone();
    class.retain(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-');
    let class = if class.is_empty() { "alef-app" } else { &class };
    let icon = artifact
        .icon
        .as_deref()
        .map(|icon| path_text(icon).map(|value| format!("Icon={}\n", desktop_value(&value))))
        .transpose()?
        .unwrap_or_default();
    Ok(format!(
        "[Desktop Entry]\nType=Application\nName={}\nExec={} run {}\n{}Terminal=false\nCategories=Utility;\nStartupWMClass={}\n",
        desktop_value(&artifact.name), exec_token(&executable), exec_token(&package), icon, desktop_value(class)
    ))
}

fn xml_text(input: &str, out: &mut String) -> io::Result<()> {
    for c in input.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c if c == '\t'
                || c == '\n'
                || c == '\r'
                || (' '..='\u{d7ff}').contains(&c)
                || ('\u{e000}'..='\u{fffd}').contains(&c)
                || ('\u{10000}'..='\u{10ffff}').contains(&c) =>
            {
                out.push(c)
            }
            _ => return Err(invalid("value contains an XML-invalid control character")),
        }
    }
    Ok(())
}

fn plist_string(key: &str, value: &str, out: &mut String) -> io::Result<()> {
    out.push_str("<key>");
    xml_text(key, out)?;
    out.push_str("</key><string>");
    xml_text(value, out)?;
    out.push_str("</string>");
    Ok(())
}

/// Render a minimal XML property list for a macOS application bundle.
pub fn plist(artifact: &Artifact) -> io::Result<String> {
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict>");
    plist_string("CFBundleIdentifier", &artifact.id, &mut out)?;
    plist_string("CFBundleName", &artifact.name, &mut out)?;
    plist_string("CFBundleExecutable", "launcher", &mut out)?;
    if artifact.icon.is_some() {
        plist_string("CFBundleIconFile", "icon.png", &mut out)?;
    }
    out.push_str("</dict></plist>\n");
    Ok(out)
}

fn shell_quote(value: &str, out: &mut String) {
    out.push('\'');
    for c in value.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
}

/// Render a POSIX launcher script whose executable and package paths are literal arguments.
pub fn mac_launcher(artifact: &Artifact) -> io::Result<String> {
    let executable = path_text(&artifact.executable)?;
    let package = path_text(&artifact.package)?;
    let mut out = String::from("#!/bin/sh\nexec ");
    shell_quote(&executable, &mut out);
    out.push_str(" run ");
    shell_quote(&package, &mut out);
    out.push_str(" \"$@\"\n");
    Ok(out)
}

/// Quote one argument according to the Windows C runtime command-line parsing algorithm.
pub fn windows_arguments(package: &Path) -> io::Result<String> {
    let value = path_text(package)?;
    let mut out = String::from("run \"");
    let mut slashes = 0usize;
    for c in value.chars() {
        match c {
            '\\' => slashes += 1,
            '"' => {
                out.extend(std::iter::repeat('\\').take(slashes * 2 + 1));
                out.push('"');
                slashes = 0;
            }
            c => {
                out.extend(std::iter::repeat('\\').take(slashes));
                slashes = 0;
                out.push(c);
            }
        }
    }
    out.extend(std::iter::repeat('\\').take(slashes * 2));
    out.push('"');
    Ok(out)
}

/// Produce a portable, deterministic path component with a hash suffix.
pub fn safe_display_component(name: &str, hash: &str) -> String {
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = matches!(
        stem.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    );
    let mut encoded = String::new();
    for b in name.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b' ' | b'-' | b'_') {
            encoded.push(char::from(b));
        } else {
            encoded.push_str(&format!("~{b:02X}"));
        }
    }
    let suffix = format!("~{hash}");
    let mut prefix = if reserved {
        encoded
            .chars()
            .map(|c| format!("~{:02X}", c as u32))
            .collect::<String>()
    } else {
        encoded
    };
    let limit = 160usize.saturating_sub(suffix.len());
    while prefix.len() > limit {
        prefix.pop();
    }
    format!("{prefix}{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn artifact() -> Artifact {
        Artifact {
            id: "org.alef.hostile".into(),
            name: "A \"quoted\" % $\nName".into(),
            hash: "0123456789abcdef0123456789abcdef".into(),
            executable: PathBuf::from("/apps/a $b%/run\"now"),
            package: PathBuf::from("/apps/p\n%$`\\\".alef"),
            icon: Some(PathBuf::from("/icons/a&b.png")),
        }
    }

    #[test]
    fn desktop_entry_escapes_hostile_values_without_field_substitution() {
        let rendered = desktop_entry(&artifact()).unwrap();
        assert!(rendered.contains("Name=A \"quoted\" % $\\nName\n"));
        assert!(rendered.contains("run\\n") == false);
        assert!(rendered.contains(concat!(
            r#"Exec="/apps/a \\$b%%/run\\"now" run "/apps/p\n%%\\$\\`\\\\\\".alef""#,
            "\n"
        )));
        assert!(rendered.contains("Icon=/icons/a&b.png\n"));
    }

    #[test]
    fn plist_uses_bundle_relative_launcher_and_icon_names() {
        let rendered = plist(&artifact()).unwrap();
        assert!(rendered.contains("<key>CFBundleExecutable</key><string>launcher</string>"));
        assert!(rendered.contains("<key>CFBundleIconFile</key><string>icon.png</string>"));
        assert!(!rendered.contains("/icons/"));
    }

    #[test]
    fn launcher_quotes_hostile_paths_and_includes_the_run_command() {
        assert_eq!(
            mac_launcher(&artifact()).unwrap(),
            "#!/bin/sh\nexec '/apps/a $b%/run\"now' run '/apps/p\n%$`\\\".alef' \"$@\"\n"
        );
    }

    #[test]
    fn windows_arguments_quote_hostile_path_and_append_run() {
        assert_eq!(
            windows_arguments(Path::new(r#"C:\Apps\A B\"x\\"#)).unwrap(),
            r#"run "C:\Apps\A B\\\"x\\\\""#
        );
    }

    #[test]
    fn safe_display_component_suffixes_all_names_and_bounds_utf8_output() {
        assert_eq!(
            safe_display_component("Normal Name", "abcdef0123456789"),
            "Normal Name~abcdef0123456789"
        );
        assert_eq!(
            safe_display_component("a/b", "abcdef0123456789"),
            "a~2Fb~abcdef0123456789"
        );
        assert_ne!(
            safe_display_component("Same", "aaaaaaaaaaaa1111"),
            safe_display_component("Same", "bbbbbbbbbbbb2222")
        );
        let long = safe_display_component(&"é".repeat(200), "abcdef0123456789");
        assert!(long.len() <= 160);
        assert!(long.is_char_boundary(long.len()));
    }

    #[test]
    fn plist_escapes_xml_names() {
        let mut value = artifact();
        value.name = "A & <name>".into();
        assert!(plist(&value)
            .unwrap()
            .contains("<key>CFBundleName</key><string>A &amp; &lt;name&gt;</string>"));
    }

    #[test]
    fn plist_rejects_xml_forbidden_controls() {
        let mut value = artifact();
        value.name = "bad\u{1}".into();
        assert_eq!(
            plist(&value).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }
}
