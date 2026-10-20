// SPDX-License-Identifier: MIT OR Apache-2.0
//! Validation and transport helpers for explicit HTTP proxies.
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tls_tests;
mod transport;
pub(in crate::net::http) use transport::{request, ResponseBody};

use url::Url;

use crate::net::http::invalid;

#[derive(Clone, Debug)]
pub(super) struct Proxy {
    pub url: Url,
    pub address: String,
    pub port: u16,
}

pub(super) fn parse(text: Option<&str>) -> Result<Option<Proxy>, alef_core::AlefError> {
    let Some(text) = text else { return Ok(None) };
    let authority = text
        .split_once("://")
        .map(|(_, rest)| rest.split(['/', '?', '#']).next().unwrap_or(""))
        .unwrap_or("");
    let explicit_port = authority
        .rsplit_once(':')
        .is_some_and(|(_, p)| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
    if authority.is_empty()
        || authority.contains(['@', '\\'])
        || authority.bytes().any(|b| b.is_ascii_whitespace())
        || authority.ends_with(':')
        || !explicit_port
    {
        return Err(invalid(
            "proxy must be http://host:port with explicit port and no credentials",
        ));
    }
    let url = Url::parse(text).map_err(|_| invalid("proxy must be http://host:port"))?;
    if url.scheme() != "http"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
        || url.host_str().is_none()
        || url.port_or_known_default().is_none()
        || url.port() == Some(0)
    {
        return Err(invalid(
            "proxy must be http://host:port with no credentials or suffix",
        ));
    }
    let host = url.host_str().unwrap();
    Ok(Some(Proxy {
        address: host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_owned(),
        port: url.port_or_known_default().unwrap(),
        url,
    }))
}

pub(super) fn authority(host: &str, port: u16) -> String {
    let host = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_owned()
    };
    format!("{host}:{port}")
}
