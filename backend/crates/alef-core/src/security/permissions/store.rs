// SPDX-License-Identifier: MIT OR Apache-2.0
//! The one Ktav codec for the state files of the runtime: the settings of the AI usage and the
//! decisions of the user. Every state file goes through [`to_bytes`] / [`from_bytes`], so the
//! format of the files on the disk is decided here and nowhere else.

/// Serializes `value` as a Ktav document; a serialization failure is `InvalidData`.
pub fn to_bytes<T: serde::Serialize>(value: &T) -> std::io::Result<Vec<u8>> {
    ktav::to_string(value)
        .map(String::into_bytes)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string()))
}

/// Deserializes `T` from a Ktav document; a text that is not UTF-8 or does not parse is
/// `InvalidData`.
pub fn from_bytes<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> std::io::Result<T> {
    let text = std::str::from_utf8(bytes)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    ktav::from_str(text)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string()))
}

/// Re-exports for the tests that inspect a document without a concrete type.
pub use ktav::{parse, Value};

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[derive(Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
    struct Sample {
        version: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    }

    #[test]
    fn a_struct_survives_a_round_trip_through_the_codec() {
        let sample = Sample {
            version: 1,
            name: Some("app".to_owned()),
        };
        let bytes = to_bytes(&sample).unwrap();
        assert_eq!(from_bytes::<Sample>(&bytes).unwrap(), sample);
        // A field left out of the document comes back as `None`.
        let bare = Sample {
            version: 1,
            name: None,
        };
        let bare_bytes = to_bytes(&bare).unwrap();
        assert_eq!(from_bytes::<Sample>(&bare_bytes).unwrap(), bare);
    }

    #[test]
    fn bytes_that_are_not_utf8_are_invalid_data() {
        let error = from_bytes::<Sample>(&[0xff, 0xfe, 0x00]).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn a_document_that_does_not_parse_is_invalid_data() {
        // No separator space between the key and the value: a parse error.
        let error = from_bytes::<Sample>(b"version:1\n").unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn a_duplicate_key_is_an_error() {
        let error = from_bytes::<Sample>(b"version: 1\nversion: 2\n").unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn a_map_key_with_a_dot_and_a_space_survives_a_round_trip() {
        let mut map = BTreeMap::new();
        map.insert("runtime.fs.read".to_owned(), "documents".to_owned());
        map.insert("my app id".to_owned(), "kept".to_owned());
        let bytes = to_bytes(&map).unwrap();
        let back: BTreeMap<String, String> = from_bytes(&bytes).unwrap();
        assert_eq!(back, map);
    }

    #[test]
    fn a_value_that_looks_like_a_keyword_or_an_object_stays_a_string() {
        #[derive(Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
        struct Texts {
            flag: String,
            body: String,
        }
        let texts = Texts {
            flag: "true".to_owned(),
            body: "{ not an object".to_owned(),
        };
        let bytes = to_bytes(&texts).unwrap();
        assert_eq!(from_bytes::<Texts>(&bytes).unwrap(), texts);
    }

    #[test]
    fn none_is_written_as_null_and_read_back_as_none() {
        #[derive(Debug, PartialEq, serde::Deserialize, serde::Serialize)]
        struct Maybe {
            value: Option<u64>,
        }
        let empty = Maybe { value: None };
        let bytes = to_bytes(&empty).unwrap();
        assert_eq!(from_bytes::<Maybe>(&bytes).unwrap(), empty);
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("null"), "{text}");
    }

    #[test]
    fn parse_and_value_are_available_for_document_inspection() {
        let document = parse("version: 1\n").unwrap();
        let saved = document.as_object().unwrap();
        assert_eq!(saved.get("version").and_then(Value::as_integer), Some("1"));
    }
}
