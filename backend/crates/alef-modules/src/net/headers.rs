// SPDX-License-Identifier: MIT OR Apache-2.0
//! The headers a page sets on a request of the network. The framing of a request (the host, the length
//! of the body, the connection, the upgrade) is the client's business, not the page's.
use alef_core::{AlefError, ErrorCode};
use hyper::{
    header::{HeaderName, HeaderValue},
    HeaderMap,
};

/// Headers the program does not set.
const FORBIDDEN: [&str; 8] = [
    "host",
    "content-length",
    "transfer-encoding",
    "connection",
    "upgrade",
    "keep-alive",
    "te",
    "trailer",
];

fn invalid(message: &str) -> AlefError {
    AlefError::new(ErrorCode::InvalidArgument, message)
}

/// The headers of the page as a map that keeps every value in the order given. A name that starts with
/// one of `reserved` (`proxy-`, and for a WebSocket `sec-websocket-`) is the client's too.
pub(super) fn parse(
    headers: &[(String, String)],
    reserved: &[&str],
) -> Result<HeaderMap, AlefError> {
    let mut map = HeaderMap::new();
    for (name, value) in headers {
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| invalid("a header has a name that is not valid"))?;
        if FORBIDDEN.contains(&name.as_str())
            || reserved
                .iter()
                .any(|prefix| name.as_str().starts_with(prefix))
        {
            return Err(invalid(
                "a header is the client's own: host, framing, connection",
            ));
        }
        let value = HeaderValue::from_str(value)
            .map_err(|_| invalid("a header has a value that is not valid"))?;
        map.append(name, value);
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(headers: &[(&str, &str)]) -> Vec<(String, String)> {
        headers
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
    }

    fn refusal(headers: &[(&str, &str)], reserved: &[&str]) -> Option<String> {
        parse(&pairs(headers), reserved).err().map(|e| e.message)
    }

    #[test]
    fn the_headers_of_the_client_are_refused_whatever_the_case() {
        for name in [
            "Host",
            "CONTENT-LENGTH",
            "Transfer-Encoding",
            "Connection",
            "Upgrade",
            "Keep-Alive",
            "TE",
            "Trailer",
        ] {
            assert_eq!(
                refusal(&[(name, "x")], &[]).as_deref(),
                Some("a header is the client's own: host, framing, connection"),
                "{name}"
            );
        }
        for name in [
            "accept",
            "x-proxy",
            "authorization",
            "cookie",
            "content-type",
            "origin",
        ] {
            assert_eq!(refusal(&[(name, "x")], &[]), None, "{name}");
        }
    }

    #[test]
    fn the_names_that_start_with_a_reserved_prefix_are_the_clients_too() {
        let reserved = ["proxy-", "sec-websocket-"];
        for name in [
            "Proxy-Authorization",
            "proxy-x",
            "Sec-WebSocket-Key",
            "sec-websocket-protocol",
        ] {
            assert!(refusal(&[(name, "x")], &reserved).is_some(), "{name}");
        }
        assert_eq!(refusal(&[("proxy-x", "x")], &["sec-websocket-"]), None);
        assert_eq!(refusal(&[("sec-websocket-key", "x")], &["proxy-"]), None);
        assert_eq!(
            refusal(&[("x-sec-websocket-y", "x")], &reserved),
            None,
            "only a prefix"
        );
    }

    #[test]
    fn a_name_or_a_value_that_is_not_valid_is_refused() {
        assert_eq!(
            refusal(&[("bad name", "x")], &[]).as_deref(),
            Some("a header has a name that is not valid")
        );
        assert_eq!(
            refusal(&[("", "x")], &[]).as_deref(),
            Some("a header has a name that is not valid")
        );
        assert_eq!(
            refusal(&[("x-ok", "line\nbreak")], &[]).as_deref(),
            Some("a header has a value that is not valid")
        );
    }

    #[test]
    fn every_value_of_a_header_stays_in_the_order_given() {
        let map = parse(&pairs(&[("x-a", "1"), ("X-A", "2"), ("x-b", "3")]), &[]).unwrap();
        let values: Vec<&str> = map
            .get_all("x-a")
            .iter()
            .map(|value| value.to_str().unwrap())
            .collect();
        assert_eq!(values, ["1", "2"]);
        assert_eq!(map.len(), 3);
    }
}
