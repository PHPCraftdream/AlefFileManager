// SPDX-License-Identifier: MIT OR Apache-2.0
//! A TLS server of the tests, with a certificate of the authority of the tests (the files in
//! `fixtures/tls` are made for these tests alone and protect nothing).
use std::sync::Arc;

use rustls::{
    pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer},
    ServerConfig,
};
use tokio_rustls::TlsAcceptor;

/// The authority, to give to the runtime as the one to trust.
pub const AUTHORITY: &str = include_str!("../../fixtures/tls/ca.pem");
const CERTIFICATE: &[u8] = include_bytes!("../../fixtures/tls/server.pem");
const KEY: &[u8] = include_bytes!("../../fixtures/tls/server.key");

pub fn acceptor(tls12_only: bool) -> TlsAcceptor {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let builder = ServerConfig::builder_with_provider(provider);
    let builder = if tls12_only {
        builder.with_protocol_versions(&[&rustls::version::TLS12])
    } else {
        builder.with_safe_default_protocol_versions()
    }
    .unwrap();
    let chain: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(CERTIFICATE)
        .collect::<Result<_, _>>()
        .unwrap();
    let key = PrivateKeyDer::from_pem_slice(KEY).unwrap();
    TlsAcceptor::from(Arc::new(
        builder
            .with_no_client_auth()
            .with_single_cert(chain, key)
            .unwrap(),
    ))
}

/// The certificate of the server, as a text (a test that wants a wrong authority gives this one).
pub fn certificate() -> String {
    String::from_utf8(CERTIFICATE.to_vec()).unwrap()
}
