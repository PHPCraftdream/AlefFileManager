// SPDX-License-Identifier: MIT OR Apache-2.0
//! The offer of a WebSocket in a request of HTTP, and the answer that takes it (RFC 6455, 4.2): a `GET` of
//! HTTP/1.1 that asks to upgrade the connection to `websocket` with the version 13 and a key. Anything else
//! is a request like the others, and the page may answer it as it pleases (a 426, say).
use async_tungstenite::tungstenite::handshake::derive_accept_key;
use http_body_util::{BodyExt, Empty};
use hyper::{
    header::{
        HeaderName, HeaderValue, CONNECTION, SEC_WEBSOCKET_ACCEPT, SEC_WEBSOCKET_KEY,
        SEC_WEBSOCKET_PROTOCOL, SEC_WEBSOCKET_VERSION, UPGRADE,
    },
    HeaderMap, Method, Response, StatusCode, Version,
};

use crate::net::http::body::RequestBody;
use crate::net::websocket::protocol_is_token;

/// What a client asks for when it offers a WebSocket: its key, and the subprotocols it can speak, in the
/// order it prefers them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Offer {
    key: String,
    pub(super) protocols: Vec<String>,
}

/// Whether the values of a header, as a list, hold the token (no matter the case).
fn lists(headers: &HeaderMap, name: HeaderName, token: &str) -> bool {
    headers
        .get_all(name)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .any(|part| part.trim().eq_ignore_ascii_case(token))
}

/// A key of the handshake is 16 bytes in base64: 24 characters, the last two of them padding.
fn key_is_valid(key: &str) -> bool {
    key.len() == 24
        && key.ends_with("==")
        && key[..22]
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'+' || byte == b'/')
}

/// The offer a request makes, if it makes one.
pub(super) fn offer(method: &Method, version: Version, headers: &HeaderMap) -> Option<Offer> {
    if method != Method::GET || version != Version::HTTP_11 {
        return None;
    }
    if !lists(headers, CONNECTION, "upgrade") || !lists(headers, UPGRADE, "websocket") {
        return None;
    }
    let mut versions = headers.get_all(SEC_WEBSOCKET_VERSION).iter();
    match (versions.next(), versions.next()) {
        (Some(version), None) if version.as_bytes() == b"13" => {}
        _ => return None,
    }
    let mut keys = headers.get_all(SEC_WEBSOCKET_KEY).iter();
    let key = match (keys.next(), keys.next()) {
        (Some(key), None) => key.to_str().ok()?,
        _ => return None,
    };
    if !key_is_valid(key) {
        return None;
    }
    let mut protocols: Vec<String> = Vec::new();
    for value in headers.get_all(SEC_WEBSOCKET_PROTOCOL) {
        for protocol in value.to_str().ok()?.split(',') {
            let protocol = protocol.trim();
            if protocol_is_token(protocol) && !protocols.iter().any(|seen| seen == protocol) {
                protocols.push(protocol.to_owned());
            }
        }
    }
    Some(Offer {
        key: key.to_owned(),
        protocols,
    })
}

/// The answer that takes the offer: a 101 with the proof of the key and the subprotocol chosen, if any.
/// Extensions are never taken, so none is named.
pub(super) fn accepted(offer: &Offer, protocol: Option<&str>) -> Response<RequestBody> {
    let mut response = Response::new(Empty::new().map_err(|never| match never {}).boxed());
    *response.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
    let headers = response.headers_mut();
    headers.insert(UPGRADE, HeaderValue::from_static("websocket"));
    headers.insert(CONNECTION, HeaderValue::from_static("Upgrade"));
    if let Ok(proof) = HeaderValue::from_str(&derive_accept_key(offer.key.as_bytes())) {
        headers.insert(SEC_WEBSOCKET_ACCEPT, proof);
    }
    if let Some(protocol) = protocol.and_then(|protocol| HeaderValue::from_str(protocol).ok()) {
        headers.insert(SEC_WEBSOCKET_PROTOCOL, protocol);
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "dGhlIHNhbXBsZSBub25jZQ==";

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.append(
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        map
    }

    fn good() -> Vec<(&'static str, &'static str)> {
        vec![
            ("connection", "Upgrade"),
            ("upgrade", "websocket"),
            ("sec-websocket-version", "13"),
            ("sec-websocket-key", KEY),
        ]
    }

    fn made(pairs: &[(&str, &str)]) -> Option<Offer> {
        offer(&Method::GET, Version::HTTP_11, &headers(pairs))
    }

    #[test]
    fn a_get_that_asks_for_a_websocket_with_a_key_and_the_version_13_is_an_offer() {
        let found = made(&good()).unwrap();
        assert_eq!(found.key, KEY);
        assert!(found.protocols.is_empty());
    }

    #[test]
    fn the_tokens_of_the_headers_are_found_in_a_list_whatever_their_case() {
        let mut pairs = good();
        pairs[0] = ("connection", "keep-alive, UPGRADE");
        pairs[1] = ("upgrade", "h2c, WebSocket");
        assert!(made(&pairs).is_some());
    }

    #[test]
    fn anything_that_is_short_of_the_handshake_is_no_offer() {
        assert!(offer(&Method::POST, Version::HTTP_11, &headers(&good())).is_none());
        assert!(offer(&Method::GET, Version::HTTP_10, &headers(&good())).is_none());
        for (name, value) in [
            ("connection", "keep-alive"),
            ("upgrade", "h2c"),
            ("sec-websocket-version", "12"),
            ("sec-websocket-version", "13, 8"),
            ("sec-websocket-key", "short"),
            ("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZ!=="),
            ("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ=a"),
            ("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQAA"),
            ("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQAA=="),
            ("sec-websocket-key", "ab=="),
            ("sec-websocket-key", "dGhlIHNhbXBsZSBub25j-Q=="),
        ] {
            let mut pairs = good();
            let at = pairs.iter().position(|(key, _)| *key == name).unwrap();
            pairs[at] = (name, value);
            assert!(made(&pairs).is_none(), "{name}: {value}");
        }
        for missing in [
            "connection",
            "upgrade",
            "sec-websocket-version",
            "sec-websocket-key",
        ] {
            let pairs: Vec<_> = good()
                .into_iter()
                .filter(|(name, _)| *name != missing)
                .collect();
            assert!(made(&pairs).is_none(), "without {missing}");
        }
    }

    #[test]
    fn a_key_may_hold_plus_and_slash() {
        for key in ["dGhlIHNhbXBsZSBub25j+Q==", "dGhlIHNhbXBsZSBub25j/Q=="] {
            let mut pairs = good();
            let at = pairs
                .iter()
                .position(|(name, _)| *name == "sec-websocket-key")
                .unwrap();
            pairs[at] = ("sec-websocket-key", key);
            assert_eq!(made(&pairs).unwrap().key, key);
        }
    }

    #[test]
    fn a_header_that_repeats_where_one_is_asked_is_no_offer() {
        let mut keys = good();
        keys.push(("sec-websocket-key", KEY));
        assert!(made(&keys).is_none());
        let mut versions = good();
        versions.push(("sec-websocket-version", "13"));
        assert!(made(&versions).is_none());
    }

    #[test]
    fn the_subprotocols_are_the_tokens_of_every_header_in_the_order_they_came_without_repeats() {
        let mut pairs = good();
        pairs.push(("sec-websocket-protocol", "chat, superchat"));
        pairs.push(("sec-websocket-protocol", "Chat,v1.json, chat ,,a b"));
        assert_eq!(
            made(&pairs).unwrap().protocols,
            ["chat", "superchat", "Chat", "v1.json"]
        );
    }

    #[test]
    fn the_answer_is_a_101_with_the_proof_of_the_key_and_the_subprotocol_chosen() {
        let taken = made(&good()).unwrap();
        let plain = accepted(&taken, None);
        assert_eq!(plain.status(), StatusCode::SWITCHING_PROTOCOLS);
        assert_eq!(plain.headers()[UPGRADE], "websocket");
        assert_eq!(plain.headers()[CONNECTION], "Upgrade");
        assert_eq!(
            plain.headers()[SEC_WEBSOCKET_ACCEPT],
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=",
            "the example of RFC 6455"
        );
        assert!(plain.headers().get(SEC_WEBSOCKET_PROTOCOL).is_none());
        let chosen = accepted(&taken, Some("superchat"));
        assert_eq!(chosen.headers()[SEC_WEBSOCKET_PROTOCOL], "superchat");
    }
}
