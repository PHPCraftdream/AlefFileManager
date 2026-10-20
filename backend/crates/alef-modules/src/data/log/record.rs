use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const MAX_MESSAGE_BYTES: usize = 8 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Debug,
    Info,
    Warn,
    Error,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LogRecord {
    pub time: String,
    pub level: Level,
    pub message: String,
    pub fields: Map<String, Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dropped: Option<u64>,
}

pub(crate) fn truncate_utf8(input: &str, limit: usize, marker: &str) -> String {
    if input.len() <= limit {
        return input.to_owned();
    }
    let room = limit.saturating_sub(marker.len());
    let mut end = room.min(input.len());
    while !input.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{}", &input[..end], marker)
}

pub(crate) fn redact_fields(value: Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .into_iter()
                .map(|(key, value)| {
                    let lower = key.to_ascii_lowercase();
                    let sensitive = [
                        "password",
                        "passwd",
                        "secret",
                        "token",
                        "authorization",
                        "cookie",
                    ]
                    .iter()
                    .any(|needle| lower.contains(needle))
                        || api_key(&lower);
                    (
                        key,
                        if sensitive {
                            Value::String("[redacted]".into())
                        } else {
                            redact_fields(value)
                        },
                    )
                })
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.into_iter().map(redact_fields).collect()),
        other => other,
    }
}

fn api_key(key: &str) -> bool {
    let bytes = key.as_bytes();
    (0..bytes.len().saturating_sub(2)).any(|i| {
        if bytes.get(i..i + 3) != Some(b"api") {
            return false;
        }
        let mut j = i + 3;
        if matches!(bytes.get(j), Some(b'_') | Some(b'-')) {
            j += 1;
        }
        bytes.get(j..j.saturating_add(3)) == Some(b"key")
    })
}

pub(crate) fn now_rfc3339() -> String {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64;
    let stamp = alef_core::session::usage::timestamp(ms);
    let (date, time) = stamp.split_once(' ').unwrap_or((&stamp, "00:00:00Z"));
    let (clock, _) = time.split_once('Z').unwrap_or((time, ""));
    format!("{date}T{clock}.{:03}Z", ms.rem_euclid(1000))
}
