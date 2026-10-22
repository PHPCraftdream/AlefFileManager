// SPDX-License-Identifier: MIT OR Apache-2.0
//! macOS LaunchAgent plist: render and a minimal strict parser of exactly our
//! own record shape. Anything else is `Foreign` or `Damaged`; a foreign plist
//! is never reinterpreted as ours.

use std::ffi::OsString;
use std::path::PathBuf;

use super::entry::Entry;
use super::AutostartError;
use super::Parsed;

/// Render the LaunchAgent plist for `label`.
pub fn render_plist(label: &str, entry: &Entry) -> Result<String, AutostartError> {
    entry.validate()?;
    validate_label(label)?;
    let mut out = String::with_capacity(512);
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str("<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" ");
    out.push_str("\"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n");
    out.push_str("<plist version=\"1.0\">\n<dict>\n");
    out.push_str("\t<key>Label</key>\n\t<string>");
    out.push_str(&xml_escape(label));
    out.push_str("</string>\n");
    out.push_str("\t<key>ProgramArguments</key>\n\t<array>\n");
    for arg in
        std::iter::once(entry.exe.as_os_str()).chain(entry.args.iter().map(|a| a.as_os_str()))
    {
        out.push_str("\t\t<string>");
        out.push_str(&xml_escape(&arg.to_string_lossy()));
        out.push_str("</string>\n");
    }
    out.push_str("\t</array>\n");
    out.push_str("\t<key>RunAtLoad</key>\n\t<true/>\n");
    out.push_str("\t<key>KeepAlive</key>\n\t<false/>\n");
    out.push_str("</dict>\n</plist>\n");
    Ok(out)
}

/// Parse a plist file. `Parsed::Entry` only for our exact record with our
/// label; a structurally valid plist with another label is `Foreign`.
pub fn parse_plist(raw: &str, our_label: &str) -> Parsed {
    match parse_dict(raw, our_label) {
        Outcome::Entry(entry) => Parsed::Entry(entry),
        Outcome::Foreign => Parsed::Foreign,
        Outcome::Damaged => Parsed::Damaged,
    }
}

enum Outcome {
    Entry(Entry),
    Foreign,
    Damaged,
}

fn validate_label(label: &str) -> Result<(), AutostartError> {
    if label.is_empty() || label.bytes().any(|b| b == 0 || b == b'\n' || b == b'\r') {
        return Err(AutostartError::InvalidEntry(
            "launch agent label must be a single non-empty line",
        ));
    }
    Ok(())
}

fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

fn xml_unescape(s: &str) -> Option<String> {
    if !s.contains('&') {
        return Some(s.to_string());
    }
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '&' {
            out.push(c);
            continue;
        }
        let mut entity = String::new();
        loop {
            match chars.next() {
                Some(';') => break,
                Some(e) => entity.push(e),
                None => return None,
            }
        }
        match entity.as_str() {
            "lt" => out.push('<'),
            "gt" => out.push('>'),
            "amp" => out.push('&'),
            "quot" => out.push('"'),
            "apos" => out.push('\''),
            _ => return None,
        }
    }
    Some(out)
}

/// Keys of our record, in the exact order we render them. A subset that is a
/// prefix of this sequence (with `Label` first) is accepted.
const OUR_KEYS: [&str; 4] = ["Label", "ProgramArguments", "RunAtLoad", "KeepAlive"];

fn parse_dict(raw: &str, our_label: &str) -> Outcome {
    let mut c = Cursor::new(raw);
    if preamble_tags(&mut c).is_none() {
        return Outcome::Damaged;
    }
    c.ws();
    if c.tag().as_deref() != Some("dict") {
        return Outcome::Damaged;
    }
    let mut label: Option<String> = None;
    let mut args: Vec<String> = Vec::new();
    let mut key_index = 0usize;
    loop {
        c.ws();
        match c.tag().as_deref() {
            Some("key") => {
                let Some(key) = c.text() else {
                    return Outcome::Damaged;
                };
                c.ws();
                if c.tag().as_deref() != Some("/key") {
                    return Outcome::Damaged;
                }
                if key_index >= OUR_KEYS.len() || OUR_KEYS[key_index] != key {
                    // Unknown or reordered key: not our record shape.
                    return Outcome::Damaged;
                }
                key_index += 1;
                c.ws();
                let next = c.tag();
                match key.as_str() {
                    "Label" => {
                        if next.as_deref() != Some("string") {
                            return Outcome::Damaged;
                        }
                        let Some(text) = c.text() else {
                            return Outcome::Damaged;
                        };
                        label = Some(text);
                        c.ws();
                        if c.tag().as_deref() != Some("/string") {
                            return Outcome::Damaged;
                        }
                    }
                    "ProgramArguments" => {
                        if next.as_deref() != Some("array") {
                            return Outcome::Damaged;
                        }
                        loop {
                            c.ws();
                            match c.tag().as_deref() {
                                Some("string") => {
                                    let Some(text) = c.text() else {
                                        return Outcome::Damaged;
                                    };
                                    args.push(text);
                                    c.ws();
                                    if c.tag().as_deref() != Some("/string") {
                                        return Outcome::Damaged;
                                    }
                                }
                                Some("/array") => break,
                                _ => return Outcome::Damaged,
                            }
                        }
                    }
                    "RunAtLoad" => {
                        if next.as_deref() != Some("true/") {
                            return Outcome::Damaged;
                        }
                    }
                    "KeepAlive" => {
                        if next.as_deref() != Some("false/") {
                            return Outcome::Damaged;
                        }
                    }
                    _ => return Outcome::Damaged,
                }
            }
            Some("/dict") => break,
            _ => return Outcome::Damaged,
        }
    }
    c.ws();
    if c.tag().as_deref() != Some("/plist") {
        return Outcome::Damaged;
    }
    c.ws();
    if !c.at_end() {
        return Outcome::Damaged;
    }
    if label.as_deref() != Some(our_label) {
        // Structurally sound, but someone else's record.
        return Outcome::Foreign;
    }
    if args.is_empty() {
        return Outcome::Damaged;
    }
    let exe = PathBuf::from(args.remove(0));
    Outcome::Entry(Entry::new(
        exe,
        args.into_iter().map(OsString::from).collect(),
    ))
}

fn preamble_tags(c: &mut Cursor<'_>) -> Option<()> {
    // XML declaration, DOCTYPE and the plist wrapper must all be present.
    c.tag()?.starts_with("?xml").then_some(())?;
    c.ws();
    c.tag()?.starts_with("!DOCTYPE").then_some(())?;
    c.ws();
    c.tag()?.starts_with("plist").then_some(())?;
    Some(())
}

struct Cursor<'a> {
    s: &'a str,
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(s: &'a str) -> Self {
        Self { s, pos: 0 }
    }

    fn ws(&mut self) {
        let rest = &self.s[self.pos..];
        let trimmed = rest.trim_start();
        self.pos = self.s.len() - trimmed.len();
    }

    fn at_end(&self) -> bool {
        self.pos >= self.s.len()
    }

    /// Read the next `<...>` element and return its inner text (without the
    /// angle brackets), or `None` if there is none.
    fn tag(&mut self) -> Option<String> {
        if self.s.as_bytes().get(self.pos) != Some(&b'<') {
            return None;
        }
        let rest = &self.s[self.pos + 1..];
        let end = rest.find('>')?;
        let inner = &rest[..end];
        self.pos += 1 + end + 1;
        Some(inner.to_string())
    }

    /// Read character data up to the next `<` (raw `<` cannot appear: our
    /// writer escapes it) and unescape the entities we produce.
    fn text(&mut self) -> Option<String> {
        let rest = &self.s[self.pos..];
        let end = rest.find('<')?;
        self.pos += end;
        xml_unescape(&rest[..end])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LABEL: &str = "org.alef.runtime.broker";

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
        let rendered = render_plist(LABEL, &entry).expect("render");
        assert_eq!(parse_plist(&rendered, LABEL), Parsed::Entry(entry));
    }

    #[test]
    fn round_trip_plain() {
        round_trip(&["/Applications/Alef.app/alef", "--broker"]);
    }

    #[test]
    fn round_trip_spaces_quotes_unicode_entities() {
        round_trip(&["/opt/Alef Runtime/alef exe", "a b", "he said \"hi\""]);
        round_trip(&["/opt/alef", "&amp;<raw>", "кириллица 日本語", "'single'"]);
    }

    #[test]
    fn all_five_entities_are_escaped() {
        let rendered = render_plist(LABEL, &entry(abs("a"), &["&<>\"'"])).expect("render");
        assert!(rendered.contains("&amp;&lt;&gt;&quot;&apos;"));
    }

    #[test]
    fn other_label_is_foreign() {
        let rendered = render_plist("com.other.agent", &entry(abs("a"), &[])).expect("render");
        assert_eq!(parse_plist(&rendered, LABEL), Parsed::Foreign);
    }

    #[test]
    fn broken_xml_is_damaged() {
        assert_eq!(parse_plist("<plist><dict>", LABEL), Parsed::Damaged);
        assert_eq!(parse_plist("not xml at all", LABEL), Parsed::Damaged);
        assert_eq!(parse_plist("", LABEL), Parsed::Damaged);
    }

    #[test]
    fn foreign_structure_is_damaged_not_reinterpreted() {
        let foreign = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\">\n<dict>\n\t<key>Program</key>\n\t<string>/bin/sh</string>\n\
\t<key>MachServices</key>\n\t<dict/>\n</dict>\n</plist>\n";
        assert_eq!(parse_plist(foreign, LABEL), Parsed::Damaged);
        // Script tag / stray elements are equally refused.
        assert_eq!(
            parse_plist("<script>alert(1)</script>", LABEL),
            Parsed::Damaged
        );
    }

    #[test]
    fn missing_program_arguments_is_damaged_for_our_label() {
        let minimal = render_plist(LABEL, &entry(abs("a"), &[])).expect("render");
        let stripped = minimal.replace("<key>ProgramArguments</key>", "");
        assert_eq!(parse_plist(&stripped, LABEL), Parsed::Damaged);
    }

    #[test]
    fn invalid_label_is_rejected_on_render() {
        let e = entry(abs("a"), &[]);
        assert!(matches!(
            render_plist("", &e),
            Err(AutostartError::InvalidEntry(_))
        ));
        assert!(matches!(
            render_plist("a\nb", &e),
            Err(AutostartError::InvalidEntry(_))
        ));
    }
}
