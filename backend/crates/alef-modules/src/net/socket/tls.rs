// SPDX-License-Identifier: MIT OR Apache-2.0
//! TLS of a client socket: the roots of Mozilla, or only the authorities the page names (they replace
//! the roots, as in Node: an application that pins its authority trusts no other).
use std::sync::{Arc, OnceLock};

use alef_core::AlefError;
use rustls::{
    pki_types::{pem::PemObject, CertificateDer, ServerName},
    ClientConfig, RootCertStore,
};
use tokio_rustls::TlsConnector;

use super::invalid;

fn config(roots: RootCertStore) -> Arc<ClientConfig> {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    Arc::new(
        ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("the provider supports the versions of TLS in use")
            .with_root_certificates(roots)
            .with_no_client_auth(),
    )
}

fn mozilla() -> Arc<ClientConfig> {
    static CONFIG: OnceLock<Arc<ClientConfig>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            config(RootCertStore {
                roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
            })
        })
        .clone()
}

/// A connector that trusts the roots of Mozilla, or the certificates of `ca` (PEM) and nothing else.
pub(super) fn connector(ca: Option<&str>) -> Result<TlsConnector, AlefError> {
    let Some(pem) = ca else {
        return Ok(TlsConnector::from(mozilla()));
    };
    let mut roots = RootCertStore::empty();
    for certificate in CertificateDer::pem_slice_iter(pem.as_bytes()) {
        let certificate = certificate.map_err(|_| invalid("ca is not PEM certificates"))?;
        roots
            .add(certificate)
            .map_err(|_| invalid("ca has a certificate that cannot be an authority"))?;
    }
    if roots.is_empty() {
        return Err(invalid("ca has no certificate"));
    }
    Ok(TlsConnector::from(config(roots)))
}

/// The name the certificate of the server must have: the one the page gives, or the host it connects to.
pub(super) fn server_name(
    host: &str,
    given: Option<&str>,
) -> Result<ServerName<'static>, AlefError> {
    ServerName::try_from(given.unwrap_or(host).to_owned())
        .map_err(|_| invalid("the name of the server is not a name for TLS"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOT_A_CERTIFICATE: &str =
        "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n";

    #[test]
    fn a_name_is_the_one_given_or_the_host_and_an_address_is_a_name_too() {
        assert!(matches!(
            server_name("example.com", None).unwrap(),
            ServerName::DnsName(name) if name.as_ref() == "example.com"
        ));
        assert!(matches!(
            server_name("127.0.0.1", Some("localhost")).unwrap(),
            ServerName::DnsName(name) if name.as_ref() == "localhost"
        ));
        assert!(matches!(
            server_name("127.0.0.1", None).unwrap(),
            ServerName::IpAddress(_)
        ));
        assert!(server_name("not a name", None).is_err());
        assert!(server_name("", None).is_err());
    }

    fn refusal(ca: &str) -> String {
        match connector(Some(ca)) {
            Ok(_) => panic!("{ca:?} was let through"),
            Err(error) => error.message,
        }
    }

    #[test]
    fn the_authorities_the_page_names_are_pem_certificates() {
        assert!(connector(None).is_ok());
        assert_eq!(refusal(""), "ca has no certificate");
        assert_eq!(refusal("not pem at all"), "ca has no certificate");
        assert_eq!(
            refusal("-----BEGIN CERTIFICATE-----\n!!!!\n-----END CERTIFICATE-----\n"),
            "ca is not PEM certificates"
        );
        assert_eq!(
            refusal(NOT_A_CERTIFICATE),
            "ca has a certificate that cannot be an authority"
        );
    }
}
