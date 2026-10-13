// SPDX-License-Identifier: MIT OR Apache-2.0
//! Content security policy declarations and the application origin host.
pub mod origin;

use std::net::Ipv4Addr;

use crate::security::manifest::External;
use crate::{AlefError, ErrorCode};

/// Build the Content-Security-Policy value for an app document from its manifest `external` section.
///
/// Schemes and hosts are normalized to lowercase. `app_origin` is either an HTTPS origin, which
/// `'self'` already covers (validated, not emitted), or `native://<host>`: the origin of such a
/// document is opaque for CSP matching, so `'self'` matches nothing and the source is emitted
/// explicitly in every load directive.
pub fn build_csp(external: &External, app_origin: &str) -> Result<String, AlefError> {
    let own = app_source(app_origin)?;
    let mut directives = vec!["default-src 'none'".to_owned()];
    for (directive, entries) in [
        ("script-src", &external.load.scripts),
        ("style-src", &external.load.styles),
        ("img-src", &external.load.images),
        ("font-src", &external.load.fonts),
        ("media-src", &external.load.media),
        ("frame-src", &external.load.frames),
    ] {
        directives.push(format!(
            "{} {}",
            directive,
            sources(entries, directive, false, own.as_deref())?
        ));
    }
    directives.push(format!(
        "connect-src {}",
        sources(&external.connect, "connect-src", true, None)?
    ));
    directives.extend([
        "base-uri 'none'".into(),
        "object-src 'none'".into(),
        "form-action 'none'".into(),
    ]);
    Ok(directives.join("; "))
}

/// The explicit source of the app's own documents, when `'self'` cannot express it.
fn app_source(app_origin: &str) -> Result<Option<String>, AlefError> {
    let Some(host) = app_origin.strip_prefix("native://") else {
        parse_origin(app_origin, "app_origin", true)?;
        return Ok(None);
    };
    let plain = !host.is_empty()
        && host
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if plain {
        Ok(Some(format!("native://{host}")))
    } else {
        Err(AlefError::new(
            ErrorCode::ManifestInvalid,
            format!("Invalid entry '{app_origin}' in app_origin"),
        ))
    }
}

fn sources(
    entries: &[String],
    directive: &str,
    connect: bool,
    own: Option<&str>,
) -> Result<String, AlefError> {
    let mut values = vec!["'self'".to_owned()];
    values.extend(own.map(str::to_owned));
    if connect {
        values.push("native:".into());
    }
    for entry in entries {
        let source = parse_origin(entry, directive, false)?;
        if !values.contains(&source) {
            values.push(source);
        }
    }
    Ok(values.join(" "))
}

fn parse_origin(entry: &str, directive: &str, https_only: bool) -> Result<String, AlefError> {
    let invalid = || {
        AlefError::new(
            ErrorCode::ManifestInvalid,
            format!("Invalid entry '{entry}' in {directive}"),
        )
    };
    if entry.is_empty()
        || entry.bytes().any(|b| {
            b.is_ascii_control()
                || b.is_ascii_whitespace()
                || matches!(b, b'\'' | b'"' | b';' | b',')
        })
    {
        return Err(invalid());
    }
    let lower = entry.to_ascii_lowercase();
    if !https_only && matches!(lower.as_str(), "data:" | "blob:") {
        return Ok(lower);
    }
    let (scheme, authority) = lower.split_once("://").ok_or_else(invalid)?;
    if https_only && scheme != "https" {
        return Err(invalid());
    }
    if !matches!(scheme, "https" | "http" | "wss")
        || authority.contains('/')
        || authority.is_empty()
    {
        return Err(invalid());
    }
    let (host, port) = if authority.starts_with('[') {
        let end = authority.find(']').ok_or_else(invalid)?;
        let host = &authority[..=end];
        let tail = &authority[end + 1..];
        if !host.eq_ignore_ascii_case("[::1]") || (!tail.is_empty() && !tail.starts_with(':')) {
            return Err(invalid());
        }
        (host, tail.strip_prefix(':'))
    } else {
        let (host, port) = authority
            .rsplit_once(':')
            .filter(|(h, p)| !h.contains(':') && !p.is_empty())
            .map_or((authority, None), |(h, p)| (h, Some(p)));
        (host, port)
    };
    let wildcard = host.starts_with("*.");
    let domain = if wildcard { &host[2..] } else { host };
    if domain.is_empty()
        || domain == "*"
        || (domain != "[::1]"
            && (domain.split('.').any(str::is_empty)
                || !domain
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')))
    {
        return Err(invalid());
    }
    if wildcard && domain.split('.').count() < 2 {
        return Err(invalid());
    }
    if scheme == "http"
        && !(domain == "localhost"
            || domain.ends_with(".localhost")
            || domain.parse::<Ipv4Addr>().is_ok_and(|ip| ip.is_loopback())
            || domain == "[::1]")
    {
        return Err(invalid());
    }
    if let Some(port) = port {
        let value = port.parse::<u32>().map_err(|_| invalid())?;
        if !(1..=65535).contains(&value) {
            return Err(invalid());
        }
    }
    Ok(format!(
        "{scheme}://{host}{}",
        port.map_or(String::new(), |p| format!(":{p}"))
    ))
}

/// Normalized manifest source, or `None` when the entry is not a valid source.
pub fn normalize_source(entry: &str) -> Option<String> {
    parse_origin(entry, "source", false).ok()
}

/// The default port of a special scheme (`:80` http, `:443` https/wss).
fn default_port(scheme: &str) -> Option<u16> {
    match scheme {
        "http" => Some(80),
        "https" | "wss" => Some(443),
        _ => None,
    }
}

/// The port an origin comparison sees: the written port, or the scheme default.
fn effective_port(scheme: &str, port: Option<&str>) -> Option<u16> {
    match port {
        Some(port) => port.parse::<u16>().ok(),
        None => default_port(scheme),
    }
}

/// True when the serialized request origin (`scheme://host[:port]`) is covered by `source`.
/// Wildcard hosts (`*.domain`) cover any subdomain of `domain` (never `domain` itself) with an
/// exact scheme and port; a default port (`:443` https/wss, `:80` http) equals the omitted
/// form; `data:`/`blob:` sources never cover an origin.
pub fn origin_matches(source: &str, origin: &str) -> bool {
    let Some(normalized) = normalize_source(source) else {
        return false;
    };
    if normalized == origin {
        return true;
    }
    let Some((scheme, authority)) = normalized.split_once("://") else {
        return false;
    };
    let Some((request_scheme, request_authority)) = origin.split_once("://") else {
        return false;
    };
    if request_scheme != scheme {
        return false;
    }
    let (request_host, request_port) = split_authority(request_authority);
    let Some(wild) = authority.strip_prefix("*.") else {
        // An exact source: same host, and the same port once defaults are filled in.
        let (host, port) = split_authority(authority);
        return host == request_host
            && effective_port(scheme, port) == effective_port(request_scheme, request_port);
    };
    let (wild_host, wild_port) = split_authority(wild);
    effective_port(scheme, wild_port) == effective_port(request_scheme, request_port)
        && request_host.len() > wild_host.len() + 1
        && request_host.ends_with(wild_host)
        && request_host.as_bytes()[request_host.len() - wild_host.len() - 1] == b'.'
}

/// `host[:port]` for the non-IPv6 authorities `parse_origin` produces.
fn split_authority(authority: &str) -> (&str, Option<&str>) {
    match authority.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') && !port.is_empty() => (host, Some(port)),
        _ => (authority, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_exact_source_matches_only_its_own_origin() {
        assert!(origin_matches("https://example.com", "https://example.com"));
        // A default port equals the omitted form on both sides.
        assert!(origin_matches(
            "https://example.com:443",
            "https://example.com"
        ));
        assert!(!origin_matches(
            "https://example.com",
            "https://example.com:8443"
        ));
        assert!(!origin_matches("https://example.com", "http://example.com"));
    }

    #[test]
    fn a_wildcard_source_covers_subdomains_but_not_the_domain_itself() {
        let source = "https://*.example.com";
        assert!(origin_matches(source, "https://a.example.com"));
        assert!(origin_matches(source, "https://a.b.example.com"));
        // A default port on the wildcard side equals the omitted form.
        assert!(origin_matches(
            "https://*.example.com:443",
            "https://a.example.com"
        ));
        assert!(origin_matches(
            "http://example.localhost:80",
            "http://example.localhost"
        ));
        assert!(!origin_matches(source, "https://example.com"));
        assert!(!origin_matches(source, "https://a.example.com:8443"));
        assert!(!origin_matches(source, "http://a.example.com"));
    }

    #[test]
    fn non_origin_sources_and_invalid_entries_match_nothing() {
        // `data:`/`blob:` never cover a `scheme://host` origin; the bare-string equality
        // arm cannot fire because an origin always carries `://`.
        assert!(!origin_matches("data:", "https://example.com"));
        assert!(!origin_matches("blob:", "https://example.com"));
        assert!(!origin_matches("not a source", "https://example.com"));
        assert!(!origin_matches("", "https://example.com"));
    }
}
