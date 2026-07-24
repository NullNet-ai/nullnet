//! Certificate verifier for the gRPC control channel: standard WebPKI chain
//! validation against a single pinned CA root — the server's own private CA
//! (see `nullnet-server`'s `grpc_tls.rs`). Only a leaf actually signed by
//! that CA, for the right host, passes.
use std::sync::Arc;
use tokio_rustls::rustls::client::WebPkiServerVerifier;
use tokio_rustls::rustls::client::danger::{
    HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier,
};
use tokio_rustls::rustls::crypto::{
    CryptoProvider, verify_tls12_signature, verify_tls13_signature,
};
use tokio_rustls::rustls::pki_types::pem::PemObject;
use tokio_rustls::rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use tokio_rustls::rustls::{DigitallySignedStruct, Error, RootCertStore, SignatureScheme};

/// The process's default crypto provider, falling back to aws-lc-rs (the
/// backend `tokio-rustls`'s default features pull in) if nothing has
/// installed one yet.
fn crypto_provider() -> Arc<CryptoProvider> {
    CryptoProvider::get_default()
        .cloned()
        .unwrap_or_else(|| Arc::new(tokio_rustls::rustls::crypto::aws_lc_rs::default_provider()))
}

#[derive(Debug)]
pub(crate) struct PinnedCa {
    webpki: Arc<WebPkiServerVerifier>,
    provider: Arc<CryptoProvider>,
}

impl PinnedCa {
    /// `ca_cert_pem`: the server's private CA root, PEM-encoded (its
    /// `grpc-tls/ca-cert.pem`) — the only cert this verifier trusts.
    pub(crate) fn verifier(ca_cert_pem: &[u8]) -> Result<Arc<dyn ServerCertVerifier>, String> {
        let provider = crypto_provider();

        let ca_cert = CertificateDer::from_pem_slice(ca_cert_pem).map_err(|e| e.to_string())?;
        let mut roots = RootCertStore::empty();
        roots.add(ca_cert).map_err(|e| e.to_string())?;

        let webpki = WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider.clone())
            .build()
            .map_err(|e| e.to_string())?;

        Ok(Arc::new(Self { webpki, provider }))
    }
}

impl ServerCertVerifier for PinnedCa {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        self.webpki
            .verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now)
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}
