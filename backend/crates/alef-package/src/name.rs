// SPDX-License-Identifier: MIT OR Apache-2.0
use crate::PackageError;

pub(crate) const MAX_NAME_BYTES: usize = 1024;
pub(crate) const MAX_SEGMENT_BYTES: usize = 255;

pub(crate) fn escaped_raw_name(raw: &[u8]) -> String {
    let escaped: String = raw
        .iter()
        .flat_map(|&byte| std::ascii::escape_default(byte))
        .map(char::from)
        .collect();
    format!("\"{escaped}\"")
}

pub(crate) fn validate_name(raw: &[u8]) -> Result<&str, PackageError> {
    let name =
        std::str::from_utf8(raw).map_err(|_| PackageError::InvalidUtf8(escaped_raw_name(raw)))?;
    let invalid = |reason| PackageError::InvalidName(name.to_owned(), reason);
    if name.is_empty() {
        return Err(invalid("empty"));
    }
    if name.len() > MAX_NAME_BYTES {
        return Err(invalid("over 1024 bytes"));
    }
    if name.starts_with('/') {
        return Err(invalid("absolute path"));
    }
    if !raw
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || b"._-@+/".contains(b))
    {
        return Err(invalid("outside the format-1 ASCII alphabet"));
    }
    for segment in name.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(invalid("empty or dot segment"));
        }
        if segment.len() > MAX_SEGMENT_BYTES {
            return Err(invalid("segment over 255 bytes"));
        }
        if segment.ends_with('.') {
            return Err(invalid("segment ends in dot"));
        }
        let stem = segment
            .split('.')
            .next()
            .ok_or_else(|| invalid("empty stem"))?;
        let upper = stem.to_ascii_uppercase();
        let numbered = upper.as_bytes();
        if matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || (numbered.len() == 4
                && (upper.starts_with("COM") || upper.starts_with("LPT"))
                && matches!(numbered[3], b'1'..=b'9'))
        {
            return Err(invalid("Windows device segment"));
        }
    }
    for reserved in ["alef.ktav", "META-INF/hashes", "META-INF/signature"] {
        if name.eq_ignore_ascii_case(reserved) && name != reserved {
            return Err(invalid("reserved name requires exact spelling"));
        }
    }
    if comparison_key(name).starts_with("meta-inf/")
        && !matches!(name, "META-INF/hashes" | "META-INF/signature")
    {
        return Err(invalid("foreign META-INF entry"));
    }
    Ok(name)
}

// Names have already been restricted to ASCII.
pub(crate) fn comparison_key(name: &str) -> String {
    name.to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rejects(name: &str) {
        let error = validate_name(name.as_bytes()).expect_err("forbidden name");
        assert!(matches!(&error, PackageError::InvalidName(entry, _) if entry == name));
        assert!(error.to_string().contains(&format!("{name:?}")));
    }

    #[test]
    fn forbidden_characters_directories_and_reserved_names_are_rejected() {
        for name in [
            "a/",
            "a b",
            "a*b",
            "a?b",
            "a\"b",
            "a<b",
            "a>b",
            "a|b",
            "a%b",
            "a~b",
            "Alef.ktav",
            "meta-inf/hashes",
            "META-INF/Signature",
            "META-INF/other",
            "mEtA-InF/sub/file",
        ] {
            rejects(name);
        }
    }

    #[test]
    fn empty_names_are_rejected() {
        rejects("");
    }

    #[test]
    fn absolute_paths_are_rejected() {
        rejects("/a");
        rejects("/");
    }

    #[test]
    fn backslashes_are_rejected() {
        rejects("a\\b");
    }

    #[test]
    fn colons_are_rejected() {
        rejects("a:b");
    }

    #[test]
    fn control_characters_are_rejected() {
        for name in ["a\0b", "a\nb", "a\u{7f}", "a\u{85}"] {
            rejects(name);
        }
    }

    #[test]
    fn invalid_utf8_names_are_rejected_and_safely_escaped() {
        let error = validate_name(&[b'\n', b'"', b'\\', 0xff]).expect_err("invalid UTF-8");
        assert!(matches!(&error, PackageError::InvalidUtf8(_)));
        assert_eq!(
            error.to_string(),
            "entry name \"\\n\\\"\\\\\\xff\" is not UTF-8"
        );
    }

    #[test]
    fn empty_path_segments_are_rejected() {
        rejects("a//b");
        rejects("a//");
    }

    #[test]
    fn current_directory_segments_are_rejected() {
        rejects(".");
        rejects("a/./b");
    }

    #[test]
    fn parent_directory_segments_are_rejected() {
        rejects("..");
        rejects("a/../b");
    }

    #[test]
    fn segments_ending_in_spaces_are_rejected() {
        rejects("a /b");
        rejects("a ");
    }

    #[test]
    fn segments_ending_in_dots_are_rejected() {
        rejects("a./b");
        rejects("a.");
    }

    fn rejects_device(device: &str) {
        for name in [
            device.to_owned(),
            device.to_lowercase(),
            format!("dir/{device}.txt"),
            format!("{device}.x.y/"),
        ] {
            rejects(&name);
        }
    }

    #[test]
    fn con_device_segments_are_rejected() {
        rejects_device("CON");
    }

    #[test]
    fn prn_device_segments_are_rejected() {
        rejects_device("PRN");
    }

    #[test]
    fn aux_device_segments_are_rejected() {
        rejects_device("AUX");
    }

    #[test]
    fn nul_device_segments_are_rejected() {
        rejects_device("NUL");
    }

    #[test]
    fn numbered_com_device_segments_are_rejected() {
        for n in 1..=9 {
            rejects_device(&format!("COM{n}"));
        }
    }

    #[test]
    fn numbered_lpt_device_segments_are_rejected() {
        for n in 1..=9 {
            rejects_device(&format!("LPT{n}"));
        }
    }

    #[test]
    fn segments_over_255_bytes_are_rejected() {
        assert!(validate_name("a".repeat(255).as_bytes()).is_ok());
        rejects(&"a".repeat(256));
    }

    #[test]
    fn non_ascii_names_are_rejected() {
        rejects("é");
        rejects("שלום.js");
    }

    #[test]
    fn names_over_1024_bytes_are_rejected() {
        let exact = [
            "a".repeat(255),
            "b".repeat(255),
            "c".repeat(255),
            "d".repeat(254),
            "e".to_owned(),
        ]
        .join("/");
        assert_eq!(exact.len(), 1024);
        assert!(validate_name(exact.as_bytes()).is_ok());
        rejects(&(exact + "x"));
    }

    #[test]
    fn valid_file_names_are_accepted() {
        for name in [
            "alef.ktav",
            "META-INF/hashes",
            "assets/ascii.js",
            "a/b",
            ".hidden",
            "COM0",
            "COM10",
            "LPT0",
            "CONSOLE",
            "a@b+c_d-1",
        ] {
            assert!(validate_name(name.as_bytes()).is_ok(), "{name}");
        }
    }
}
