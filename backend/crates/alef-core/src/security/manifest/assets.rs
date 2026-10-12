// SPDX-License-Identifier: MIT OR Apache-2.0
//! R-006 asset-root syntax, shared by all manifest deserialization paths.

// docs/stages/m7-distribution.md, Format 1 names: ASCII only,
// 255 bytes per component and 1024 bytes for the selected relative path.
pub(super) fn deserialize_assets<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<String>, D::Error> {
    struct Assets;
    impl<'de> serde::de::Visitor<'de> for Assets {
        type Value = String;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a proper relative subdirectory String")
        }
        fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<String, E> {
            validate_assets(value).map_err(E::custom)?;
            Ok(value.to_owned())
        }
    }
    // deserialize_any avoids Ktav's numeric-to-String coercion and rejects null.
    d.deserialize_any(Assets)
        .map(Some)
        .map_err(|e| serde::de::Error::custom(format!("assets: {e}")))
}

fn validate_assets(value: &str) -> Result<(), &'static str> {
    if value.len() > 1024 {
        return Err("must contain at most 1024 bytes");
    }
    for (index, part) in value.split('/').enumerate() {
        // A trailing dot also rejects "." and "..".
        if part.is_empty()
            || part.len() > 255
            || part.ends_with('.')
            || !part
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-@+".contains(&b))
        {
            return Err("must use ordinary Format 1 relative path components of 1–255 ASCII bytes");
        }
        // The alphabet check above proves ASCII before slicing the device stem.
        let stem = part.split('.').next().unwrap_or_default();
        if ["CON", "PRN", "AUX", "NUL"]
            .iter()
            .any(|s| stem.eq_ignore_ascii_case(s))
            || (stem.len() == 4
                && (stem[..3].eq_ignore_ascii_case("COM") || stem[..3].eq_ignore_ascii_case("LPT"))
                && matches!(stem.as_bytes()[3], b'1'..=b'9'))
        {
            return Err("must not contain a Windows device name");
        }
        if index == 0 && part.eq_ignore_ascii_case("META-INF") {
            return Err("must not select the reserved META-INF control root");
        }
    }
    // The ASCII alphabet also excludes superscript-digit COM/LPT aliases.
    Ok(())
}

pub(super) fn assets_line(src: &str) -> usize {
    let mut depth = 0;
    let mut line = None;
    let _ = ktav::parse_events(src, |event| match event {
        ktav::ParseEvent::BeginObject | ktav::ParseEvent::BeginArray => depth += 1,
        ktav::ParseEvent::EndObject | ktav::ParseEvent::EndArray => depth -= 1,
        ktav::ParseEvent::Key(key) if depth == 1 && key == "assets" => {
            // Plain and unescaped quoted keys borrow their original source slice.
            let offset = (key.as_ptr() as usize).checked_sub(src.as_ptr() as usize);
            if let Some(offset) = offset.filter(|&offset| offset < src.len()) {
                line = Some(
                    src.as_bytes()[..offset]
                        .iter()
                        .filter(|&&b| b == b'\n')
                        .count()
                        + 1,
                );
            }
        }
        _ => {} // Other events neither change nesting nor identify this field.
    });
    // Escaped keys are arena-owned; let Ktav decode them rather than matching text.
    line.unwrap_or_else(|| {
        src.lines()
            .enumerate()
            .find_map(|(index, text)| {
                let mut found = false;
                let _ = ktav::parse_events(text, |event| {
                    if matches!(event, ktav::ParseEvent::Key("assets")) {
                        found = true;
                    }
                });
                found.then_some(index + 1)
            })
            .unwrap_or(1)
    })
}

#[cfg(test)]
mod tests;
