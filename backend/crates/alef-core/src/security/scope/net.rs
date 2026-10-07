// SPDX-License-Identifier: MIT OR Apache-2.0
//! Network scopes: `scheme://host[:port][/path]` URL patterns and `proto:host:port` socket patterns.
//! Patterns are parsed once at load; candidate targets are parsed strictly and any doubt denies.
use super::{clean, invalid};
use crate::AlefError;

/// Host part of a scope: exact name, `*.suffix` (subdomains only) or `*` (sockets only).
#[derive(Debug, Clone, PartialEq, Eq)]
enum HostScope {
    Any,
    Suffix(String),
    Exact(String),
}

impl HostScope {
    fn parse(text: &str, allow_any: bool) -> Result<Self, AlefError> {
        if text == "*" && allow_any {
            return Ok(Self::Any);
        }
        if let Some(suffix) = text.strip_prefix("*.") {
            let suffix = host(suffix).ok_or_else(|| invalid("invalid wildcard host"))?;
            if !suffix.contains('.') {
                return Err(invalid("wildcard host needs a registrable domain"));
            }
            return Ok(Self::Suffix(suffix));
        }
        host(text)
            .map(Self::Exact)
            .ok_or_else(|| invalid("invalid host"))
    }

    fn matches(&self, candidate: &str) -> bool {
        match self {
            Self::Any => true,
            Self::Exact(exact) => exact == candidate,
            Self::Suffix(suffix) => {
                candidate.len() > suffix.len() + 1
                    && candidate.ends_with(suffix.as_str())
                    && candidate[..candidate.len() - suffix.len()].ends_with('.')
            }
        }
    }
}

/// Validates and lowercases a DNS name, IPv4 address or bracketed IPv6 literal.
fn host(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    if let Some(inner) = lower.strip_prefix('[').and_then(|t| t.strip_suffix(']')) {
        let ok = inner.contains(':')
            && inner
                .chars()
                .all(|c| c.is_ascii_hexdigit() || c == ':' || c == '.');
        return ok.then_some(lower);
    }
    let labels_ok = !lower.is_empty()
        && lower.len() <= 253
        && lower.split('.').all(|label| {
            (1..=63).contains(&label.len())
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        });
    labels_ok.then_some(lower)
}

/// Splits `host[:port]` (bracketed IPv6 aware); the port must be 1..=65535 when present.
fn host_port(authority: &str) -> Option<(&str, Option<u16>)> {
    let (host, port_text) = if authority.starts_with('[') {
        let end = authority.find(']')?;
        let tail = &authority[end + 1..];
        let port = if tail.is_empty() {
            None
        } else {
            Some(tail.strip_prefix(':')?)
        };
        (&authority[..=end], port)
    } else {
        match authority.rsplit_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        }
    };
    let port = match port_text {
        Some(text) => Some(port_number(text)?),
        None => None,
    };
    Some((host, port))
}

fn port_number(text: &str) -> Option<u16> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse::<u16>().ok().filter(|port| *port > 0)
}

fn default_port(scheme: &str) -> Option<u16> {
    match scheme {
        "http" | "ws" => Some(80),
        "https" | "wss" => Some(443),
        _ => None,
    }
}

/// Which request paths a URL scope covers.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PathScope {
    Any,
    Exact(Vec<String>),
    Prefix(Vec<String>),
}

/// `scheme://host[:port][/path]` scope; no path or `/*` covers the whole origin.
#[derive(Debug, Clone)]
pub(crate) struct UrlScope {
    scheme: String,
    host: HostScope,
    port: u16,
    path: PathScope,
}

impl UrlScope {
    pub(crate) fn parse(pattern: &str) -> Result<Self, AlefError> {
        if !clean(pattern) || pattern.contains(['\\', ' ', '@', '%', '?', '#']) {
            return Err(invalid("invalid URL scope"));
        }
        let (scheme, rest) = pattern
            .split_once("://")
            .ok_or_else(|| invalid("URL scope needs scheme://"))?;
        let default = default_port(scheme)
            .ok_or_else(|| invalid("URL scope scheme must be http, https, ws or wss"))?;
        let (authority, path) = match rest.find('/') {
            Some(index) => (&rest[..index], &rest[index..]),
            None => (rest, ""),
        };
        let (host_text, port) =
            host_port(authority).ok_or_else(|| invalid("invalid URL scope authority"))?;
        let mut segments: Vec<String> = path.split('/').skip(1).map(str::to_owned).collect();
        let prefix = segments.last().is_some_and(|last| last == "*");
        if prefix {
            segments.pop();
        }
        if segments
            .iter()
            .any(|s| s.contains('*') || s == "." || s == "..")
        {
            return Err(invalid("`*` is only allowed as the last path segment"));
        }
        let scope = match (prefix, segments.as_slice()) {
            (_, []) | (false, [_]) if segments.iter().all(String::is_empty) => PathScope::Any,
            (true, _) => PathScope::Prefix(segments),
            (false, _) => PathScope::Exact(segments),
        };
        Ok(Self {
            scheme: scheme.to_owned(),
            host: HostScope::parse(host_text, false)?,
            port: port.unwrap_or(default),
            path: scope,
        })
    }

    /// Tests a candidate URL; anything that is not a plain well-formed URL is denied.
    pub(crate) fn matches(&self, target: &str) -> bool {
        let Some(url) = Url::parse(target) else {
            return false;
        };
        url.scheme == self.scheme
            && url.port == self.port
            && self.host.matches(&url.host)
            && match &self.path {
                PathScope::Any => true,
                PathScope::Exact(exact) => &url.segments == exact,
                PathScope::Prefix(prefix) => {
                    url.segments.len() >= prefix.len() && url.segments[..prefix.len()] == prefix[..]
                }
            }
    }
}

struct Url {
    scheme: String,
    host: String,
    port: u16,
    segments: Vec<String>,
}

impl Url {
    /// Strict candidate parser: ASCII only, no userinfo, no dot segments, no encoded separators.
    fn parse(text: &str) -> Option<Self> {
        if !clean(text) || !text.is_ascii() || text.contains(['\\', ' ']) {
            return None;
        }
        let (scheme, rest) = text.split_once("://")?;
        let default = default_port(scheme)?;
        let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let (authority, tail) = rest.split_at(end);
        if authority.contains(['@', '%']) {
            return None;
        }
        let (host_text, port) = host_port(authority)?;
        let path = tail.split(['?', '#']).next().unwrap_or("");
        let lowered = path.to_ascii_lowercase();
        if ["%2e", "%2f", "%5c"]
            .iter()
            .any(|bad| lowered.contains(bad))
        {
            return None;
        }
        let segments: Vec<String> = path.split('/').skip(1).map(str::to_owned).collect();
        if segments.iter().any(|s| s == "." || s == "..") {
            return None;
        }
        Some(Self {
            scheme: scheme.to_owned(),
            host: host(host_text)?,
            port: port.unwrap_or(default),
            segments: strip_root(segments),
        })
    }
}

/// `/` alone has one empty segment; treat it as the empty path.
fn strip_root(segments: Vec<String>) -> Vec<String> {
    if segments.len() == 1 && segments[0].is_empty() {
        Vec::new()
    } else {
        segments
    }
}

/// Transport of a socket scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Proto {
    Tcp,
    Udp,
    Listen,
}

/// `tcp|udp|listen:host:port` scope; `*` host and `*` port are explicit wildcards.
#[derive(Debug, Clone)]
pub(crate) struct SocketScope {
    proto: Proto,
    host: HostScope,
    port: Option<u16>,
}

fn proto(text: &str) -> Option<Proto> {
    match text {
        "tcp" => Some(Proto::Tcp),
        "udp" => Some(Proto::Udp),
        "listen" => Some(Proto::Listen),
        _ => None,
    }
}

/// Splits `proto:host:port` where host may be a bracketed IPv6 literal; the port stays text.
fn socket_parts(text: &str) -> Option<(Proto, &str, &str)> {
    if !clean(text) {
        return None;
    }
    let (proto_text, rest) = text.split_once(':')?;
    let (host, port) = if rest.starts_with('[') {
        let end = rest.find(']')?;
        (&rest[..=end], rest[end + 1..].strip_prefix(':')?)
    } else {
        rest.rsplit_once(':')?
    };
    Some((proto(proto_text)?, host, port))
}

impl SocketScope {
    pub(crate) fn parse(pattern: &str) -> Result<Self, AlefError> {
        let (proto, host_text, port) =
            socket_parts(pattern).ok_or_else(|| invalid("invalid socket scope"))?;
        let port = if port == "*" {
            None
        } else {
            Some(port_number(port).ok_or_else(|| invalid("invalid socket scope port"))?)
        };
        Ok(Self {
            proto,
            host: HostScope::parse(host_text, true)?,
            port,
        })
    }

    /// Tests a concrete `proto:host:port` target (no wildcards, port 1..=65535; `listen` may also
    /// ask for port 0, any free port, which only a `*` port in the scope covers).
    pub(crate) fn matches(&self, target: &str) -> bool {
        let Some((proto, host_text, port_text)) = socket_parts(target) else {
            return false;
        };
        let port = if proto == Proto::Listen && port_text == "0" {
            Some(0)
        } else {
            port_number(port_text)
        };
        let (Some(host_name), Some(port)) = (host(host_text), port) else {
            return false;
        };
        proto == self.proto && self.host.matches(&host_name) && self.port.is_none_or(|p| p == port)
    }
}
