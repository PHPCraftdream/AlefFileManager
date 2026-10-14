// SPDX-License-Identifier: MIT OR Apache-2.0
use super::Updater;
use crate::security::scope::net::UrlScope;
use crate::{AlefError, ErrorCode};
use serde::Deserialize;
use serde_json::json;
use std::net::IpAddr;

pub(super) fn deserialize_updater<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Updater>, D::Error> {
    struct NonNullUpdater;
    impl<'de> serde::de::Visitor<'de> for NonNullUpdater {
        type Value = Updater;
        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a non-null updater object")
        }
        fn visit_map<M: serde::de::MapAccess<'de>>(self, map: M) -> Result<Updater, M::Error> {
            Updater::deserialize(serde::de::value::MapAccessDeserializer::new(map))
        }
    }
    deserializer.deserialize_map(NonNullUpdater).map(Some)
}

pub(super) fn validate_updater(updater: &Updater, scopes: &[String]) -> Result<(), AlefError> {
    let invalid = |path: &str, reason: &str| {
        AlefError::new(ErrorCode::ManifestInvalid, format!("{path}: {reason}"))
            .with_details(json!({"path": path}))
    };
    let endpoint = &updater.endpoint;
    let Some((scheme, rest)) = endpoint.split_once("://") else {
        return Err(invalid(
            "updater.endpoint",
            "must be an absolute HTTP(S) URL",
        ));
    };
    if endpoint.contains(['@', '#', '\\', ' '])
        || !endpoint.is_ascii()
        || endpoint.chars().any(char::is_control)
    {
        return Err(invalid(
            "updater.endpoint",
            "credentials and fragments are forbidden",
        ));
    }
    if !matches!(scheme, "https" | "http") {
        return Err(invalid("updater.endpoint", "must use HTTP(S)"));
    }
    let authority_end = rest.find(['/', '?']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    if authority.is_empty() || authority.contains('@') {
        return Err(invalid("updater.endpoint", "invalid URL authority"));
    }
    let scope_matches = scopes
        .iter()
        .filter_map(|scope| UrlScope::parse(scope).ok())
        .any(|scope| scope.matches(endpoint));
    if !scope_matches {
        return Err(invalid(
            "updater.endpoint",
            "not allowed by permissions.net.http",
        ));
    }
    let host = authority
        .strip_prefix('[')
        .and_then(|rest| rest.split_once(']').map(|(host, _)| host))
        .or_else(|| {
            authority
                .split_once(':')
                .map_or(Some(authority), |(host, _)| Some(host))
        })
        .unwrap_or("");
    let loopback = host == "localhost" || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback());
    if scheme != "https" && !(scheme == "http" && loopback) {
        return Err(invalid(
            "updater.endpoint",
            "must use https except on loopback",
        ));
    }
    let key = updater.public_key.as_bytes();
    if key.len() != 44
        || key[43] != b'='
        || key[..43]
            .iter()
            .any(|b| !b.is_ascii_alphanumeric() && !b"+/".contains(b))
    {
        return Err(invalid(
            "updater.publicKey",
            "must be canonical base64 of a 32-byte Ed25519 public key",
        ));
    }
    // 32 bytes encode to 43 data characters and one '='. Canonical padding requires two low zero bits.
    if base64_last_value(key[42]).is_none_or(|value| value & 0x03 != 0) {
        return Err(invalid(
            "updater.publicKey",
            "must be canonical base64 of a 32-byte Ed25519 public key",
        ));
    }
    Ok(())
}

fn base64_last_value(byte: u8) -> Option<u8> {
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"
        .iter()
        .position(|candidate| *candidate == byte)
        .and_then(|value| u8::try_from(value).ok())
}
