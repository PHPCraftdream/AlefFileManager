// SPDX-License-Identifier: MIT OR Apache-2.0
//! What a server of the page lets in: the `Host` of a request must be a name of this server, and the
//! `Origin` of one, if it has any, the origin of this server or one the page lists. A page of any site can
//! make the browser of the user send a request to a server on the loopback (a rebinding of DNS, a form
//! posted from elsewhere); these two checks are what stops it.
use std::net::{IpAddr, SocketAddr};

use hyper::{
    header::{HOST, ORIGIN},
    HeaderMap, StatusCode,
};

/// The names of a server and the origins it takes.
#[derive(Debug, Clone)]
pub(super) struct Guard {
    bound: SocketAddr,
    hosts: Vec<String>,
    origins: Vec<String>,
}

/// The host of a `Host` header (lower case, without brackets) and its port, if it has one.
fn split_host(value: &str) -> (String, Option<u16>) {
    let value = value.trim();
    if let Some(rest) = value.strip_prefix('[') {
        if let Some((host, after)) = rest.split_once(']') {
            let port = after.strip_prefix(':').and_then(|port| port.parse().ok());
            return (host.to_ascii_lowercase(), port);
        }
    }
    match value.rsplit_once(':') {
        Some((host, port))
            if !host.contains(':')
                && !port.is_empty()
                && port.bytes().all(|byte| byte.is_ascii_digit()) =>
        {
            (host.to_ascii_lowercase(), port.parse().ok())
        }
        _ => (value.to_ascii_lowercase(), None),
    }
}

impl Guard {
    pub(super) fn new(bound: SocketAddr, hosts: &[String], origins: &[String]) -> Self {
        Self {
            bound,
            hosts: hosts.iter().map(|host| host.to_ascii_lowercase()).collect(),
            origins: origins
                .iter()
                .map(|origin| origin.to_ascii_lowercase())
                .collect(),
        }
    }

    /// Whether `name` is one of the names this server answers to. A server bound to every address answers
    /// to any address, but never to a name the page did not list: a name is what a rebinding of DNS uses.
    fn name_allowed(&self, name: &str) -> bool {
        if self.hosts.iter().any(|host| host == name) {
            return true;
        }
        let ip = self.bound.ip();
        if ip.is_unspecified() {
            return name.parse::<IpAddr>().is_ok();
        }
        if ip.is_loopback()
            && (name == "localhost" || name.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback()))
        {
            return true;
        }
        name.parse::<IpAddr>().is_ok_and(|named| named == ip)
    }

    fn host_allowed(&self, value: &str) -> bool {
        let (name, port) = split_host(value);
        port.is_none_or(|port| port == self.bound.port()) && self.name_allowed(&name)
    }

    fn origin_allowed(&self, origin: &str) -> bool {
        let origin = origin.trim().to_ascii_lowercase();
        if self.origins.contains(&origin) {
            return true;
        }
        let (authority, default_port) = if let Some(rest) = origin.strip_prefix("http://") {
            (rest, 80)
        } else if let Some(rest) = origin.strip_prefix("https://") {
            (rest, 443)
        } else {
            return false;
        };
        let (name, port) = split_host(authority);
        port.unwrap_or(default_port) == self.bound.port() && self.name_allowed(&name)
    }

    /// Why a request is not let in (the status and the reason), or `None` when it is.
    pub(super) fn check(&self, headers: &HeaderMap) -> Option<(StatusCode, &'static str)> {
        let Some(host) = headers.get(HOST).and_then(|value| value.to_str().ok()) else {
            return Some((StatusCode::BAD_REQUEST, "a request has a Host"));
        };
        if !self.host_allowed(host) {
            return Some((
                StatusCode::MISDIRECTED_REQUEST,
                "this server is not that host",
            ));
        }
        let mut origins = headers.get_all(ORIGIN).iter();
        match (origins.next(), origins.next()) {
            (None, _) => None,
            (Some(origin), None) => match origin.to_str() {
                Ok(origin) if self.origin_allowed(origin) => None,
                _ => Some((StatusCode::FORBIDDEN, "this origin is not let in")),
            },
            _ => Some((StatusCode::FORBIDDEN, "a request has one Origin")),
        }
    }
}

#[cfg(test)]
mod tests {
    use hyper::header::HeaderValue;

    use super::*;

    fn guard(bound: &str, hosts: &[&str], origins: &[&str]) -> Guard {
        let hosts: Vec<String> = hosts.iter().map(|host| (*host).to_owned()).collect();
        let origins: Vec<String> = origins.iter().map(|origin| (*origin).to_owned()).collect();
        Guard::new(bound.parse().unwrap(), &hosts, &origins)
    }

    fn headers(host: Option<&str>, origin: &[&str]) -> HeaderMap {
        let mut map = HeaderMap::new();
        if let Some(host) = host {
            map.insert(HOST, HeaderValue::from_str(host).unwrap());
        }
        for origin in origin {
            map.append(ORIGIN, HeaderValue::from_str(origin).unwrap());
        }
        map
    }

    #[test]
    fn a_host_is_split_into_its_name_and_its_port() {
        assert_eq!(
            split_host("LocalHost:8080"),
            ("localhost".to_owned(), Some(8080))
        );
        assert_eq!(split_host("example.com"), ("example.com".to_owned(), None));
        assert_eq!(split_host("[::1]:9"), ("::1".to_owned(), Some(9)));
        assert_eq!(split_host("[::1]"), ("::1".to_owned(), None));
        assert_eq!(
            split_host("::1"),
            ("::1".to_owned(), None),
            "an address of IPv6 without brackets has no port"
        );
        assert_eq!(split_host("a:b"), ("a:b".to_owned(), None));
        assert_eq!(
            split_host(" 127.0.0.1:80 "),
            ("127.0.0.1".to_owned(), Some(80))
        );
    }

    #[test]
    fn a_server_on_the_loopback_answers_to_the_loopback_and_to_its_port_only() {
        let guard = guard("127.0.0.1:8080", &[], &[]);
        for host in [
            "127.0.0.1:8080",
            "127.0.0.1",
            "localhost:8080",
            "LOCALHOST",
            "[::1]:8080",
            "127.0.0.2:8080",
        ] {
            assert!(guard.host_allowed(host), "{host}");
        }
        for host in [
            "127.0.0.1:8081",
            "evil.test",
            "evil.test:8080",
            "10.0.0.1:8080",
            "",
            "localhost.evil.test",
        ] {
            assert!(!guard.host_allowed(host), "{host:?}");
        }
    }

    #[test]
    fn a_server_on_one_address_answers_to_that_address_and_to_the_names_listed() {
        let guard = guard("10.0.0.5:80", &["App.Local"], &[]);
        assert!(guard.host_allowed("10.0.0.5"));
        assert!(guard.host_allowed("app.local:80"));
        assert!(
            !guard.host_allowed("localhost"),
            "it is not on the loopback"
        );
        assert!(!guard.host_allowed("10.0.0.6"));
        assert!(!guard.host_allowed("other.local"));
    }

    #[test]
    fn a_server_on_every_address_answers_to_any_address_but_to_no_name_unlisted() {
        let guard = guard("0.0.0.0:80", &["app.local"], &[]);
        assert!(guard.host_allowed("192.168.1.9"));
        assert!(guard.host_allowed("[fe80::1]"));
        assert!(guard.host_allowed("app.local"));
        assert!(
            !guard.host_allowed("evil.test"),
            "a name is what a rebinding of DNS uses"
        );
        assert!(!guard.host_allowed("localhost"));
    }

    #[test]
    fn an_origin_is_this_server_or_one_the_page_lists() {
        let guard = guard("127.0.0.1:8080", &[], &["https://app.test"]);
        for origin in [
            "http://127.0.0.1:8080",
            "http://localhost:8080",
            "https://localhost:8080",
            "https://app.test",
            "HTTPS://APP.TEST",
        ] {
            assert!(guard.origin_allowed(origin), "{origin}");
        }
        for origin in [
            "null",
            "http://evil.test",
            "http://localhost",
            "http://localhost:9",
            "https://app.test:8443",
            "ftp://localhost:8080",
            "http://localhost:8080/path",
            "http://u@localhost:8080",
            "",
        ] {
            assert!(!guard.origin_allowed(origin), "{origin:?}");
        }
    }

    #[test]
    fn an_origin_without_a_port_has_the_port_of_its_scheme_and_has_a_scheme() {
        let plain = guard("127.0.0.1:80", &[], &[]);
        assert!(plain.origin_allowed("http://127.0.0.1"));
        assert!(!plain.origin_allowed("https://127.0.0.1"));
        let secure = guard("127.0.0.1:443", &[], &[]);
        assert!(secure.origin_allowed("https://localhost"));
        assert!(!secure.origin_allowed("http://localhost"));
        let local = guard("127.0.0.1:8080", &[], &[]);
        for origin in [
            "127.0.0.1:8080",
            "localhost:8080",
            "//localhost:8080",
            "http://",
        ] {
            assert!(!local.origin_allowed(origin), "{origin:?}");
        }
        assert!(
            !local.origin_allowed("http://evil.test:8080"),
            "the port is the server's, the name is not"
        );
    }

    #[test]
    fn a_request_is_let_in_when_its_host_and_its_origin_are() {
        let guard = guard("127.0.0.1:8080", &[], &["https://app.test"]);
        assert_eq!(guard.check(&headers(Some("127.0.0.1:8080"), &[])), None);
        assert_eq!(
            guard.check(&headers(Some("localhost:8080"), &["https://app.test"])),
            None
        );
        assert_eq!(
            guard.check(&headers(None, &[])).map(|refusal| refusal.0),
            Some(StatusCode::BAD_REQUEST)
        );
        assert_eq!(
            guard
                .check(&headers(Some("evil.test"), &[]))
                .map(|refusal| refusal.0),
            Some(StatusCode::MISDIRECTED_REQUEST)
        );
        assert_eq!(
            guard
                .check(&headers(Some("localhost:8080"), &["http://evil.test"]))
                .map(|refusal| refusal.0),
            Some(StatusCode::FORBIDDEN)
        );
        assert_eq!(
            guard
                .check(&headers(
                    Some("localhost:8080"),
                    &["https://app.test", "https://app.test"]
                ))
                .map(|refusal| refusal.0),
            Some(StatusCode::FORBIDDEN),
            "two origins are one too many"
        );
    }
}
