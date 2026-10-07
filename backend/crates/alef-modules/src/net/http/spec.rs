// SPDX-License-Identifier: MIT OR Apache-2.0
//! The request as the client takes it: the parts the page names, held against what a request may be
//! before anything is sent. The scope of `net.http` is the first guard; these are the ones behind it.
use hyper::{
    header::{HeaderName, HeaderValue},
    HeaderMap, Method,
};
use url::Url;

use super::{client::Spec, invalid};
use alef_core::AlefError;

/// Headers the program does not set: the framing of a request is the client's business.
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

pub(super) fn spec(
    url: &str,
    method: Option<&str>,
    headers: &[(String, String)],
    follow: bool,
) -> Result<Spec, AlefError> {
    let url = Url::parse(url).map_err(|_| invalid("the address is not a URL"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(invalid("an address is http:// or https://"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(invalid(
            "an address has no user name or password: send an Authorization header",
        ));
    }
    let method = match method {
        None => Method::GET,
        Some(text) => Method::from_bytes(text.to_ascii_uppercase().as_bytes())
            .ok()
            .filter(|method| method != Method::CONNECT)
            .ok_or_else(|| invalid("the method is not one a request may have"))?,
    };
    let mut map = HeaderMap::new();
    for (name, value) in headers {
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| invalid("a header has a name that is not valid"))?;
        if FORBIDDEN.contains(&name.as_str()) || name.as_str().starts_with("proxy-") {
            return Err(invalid(
                "a header is the client's own: host, framing, connection",
            ));
        }
        let value = HeaderValue::from_str(value)
            .map_err(|_| invalid("a header has a value that is not valid"))?;
        map.append(name, value);
    }
    Ok(Spec {
        url,
        method,
        headers: map,
        follow,
    })
}

#[cfg(test)]
mod tests {
    use alef_core::ErrorCode;

    use super::*;

    fn refused(url: &str, method: Option<&str>, headers: &[(&str, &str)]) -> Option<ErrorCode> {
        let headers: Vec<(String, String)> = headers
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        spec(url, method, &headers, true).err().map(|e| e.code)
    }

    fn made(url: &str, method: Option<&str>, headers: &[(&str, &str)], follow: bool) -> Spec {
        let headers: Vec<(String, String)> = headers
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        spec(url, method, &headers, follow).expect("a request that is fit")
    }

    #[test]
    fn an_address_is_http_or_https_with_no_user_name_or_password() {
        for url in [
            "ftp://127.0.0.1/a",
            "ws://127.0.0.1/a",
            "file:///etc/passwd",
            "not a url",
            "",
            "http://user:secret@127.0.0.1/a",
            "http://user@127.0.0.1/a",
            "http://:secret@127.0.0.1/a",
        ] {
            assert_eq!(
                refused(url, None, &[]),
                Some(ErrorCode::InvalidArgument),
                "{url}"
            );
        }
        for url in ["http://127.0.0.1:8080/a?b=c#d", "https://example.com/"] {
            assert_eq!(refused(url, None, &[]), None, "{url}");
        }
    }

    #[test]
    fn a_method_is_upper_case_and_never_connect() {
        assert_eq!(made("http://h.test/", None, &[], true).method, Method::GET);
        for (given, expected) in [
            ("get", Method::GET),
            ("post", Method::POST),
            ("Put", Method::PUT),
            ("PATCH", Method::PATCH),
            ("delete", Method::DELETE),
            ("head", Method::HEAD),
        ] {
            assert_eq!(
                made("http://h.test/", Some(given), &[], true).method,
                expected,
                "{given}"
            );
        }
        for bad in ["CONNECT", "connect", "no way", "", "a/b"] {
            assert_eq!(
                refused("http://h.test/", Some(bad), &[]),
                Some(ErrorCode::InvalidArgument),
                "{bad:?}"
            );
        }
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
            "Proxy-Connection",
            "TE",
            "Trailer",
            "proxy-authorization",
            "Proxy-Anything",
        ] {
            assert_eq!(
                refused("http://h.test/", None, &[(name, "x")]),
                Some(ErrorCode::InvalidArgument),
                "{name}"
            );
        }
        for name in [
            "accept",
            "x-proxy",
            "authorization",
            "cookie",
            "content-type",
        ] {
            assert_eq!(
                refused("http://h.test/", None, &[(name, "x")]),
                None,
                "{name}"
            );
        }
        for (name, value) in [("bad name", "x"), ("x-ok", "line\nbreak"), ("", "x")] {
            assert_eq!(
                refused("http://h.test/", None, &[(name, value)]),
                Some(ErrorCode::InvalidArgument),
                "{name:?}"
            );
        }
    }

    #[test]
    fn every_value_of_a_header_stays_in_order_and_the_flag_of_redirects_goes_through() {
        let made = made(
            "http://h.test/",
            None,
            &[("x-a", "1"), ("X-A", "2"), ("x-b", "3")],
            false,
        );
        let values: Vec<&str> = made
            .headers
            .get_all("x-a")
            .iter()
            .map(|value| value.to_str().unwrap())
            .collect();
        assert_eq!(values, ["1", "2"]);
        assert_eq!(made.headers.len(), 3);
        assert!(!made.follow);
        assert!(
            super::spec("http://h.test/", None, &[], true)
                .unwrap()
                .follow
        );
    }
}
