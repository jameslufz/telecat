use anyhow::{Context, Result};
use rustls::{
    ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    pki_types::{CertificateDer, ServerName, UnixTime},
};
use std::{io::BufReader, sync::Arc};
use telecat_core::types::{ConnectionConfig, SslMode};
#[derive(Debug)]
struct Verify {
    roots: RootCertStore,
    verify_ca: bool,
    verify_name: bool,
    provider: Arc<rustls::crypto::CryptoProvider>,
}
impl ServerCertVerifier for Verify {
    fn verify_server_cert(
        &self,
        end: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        name: &ServerName<'_>,
        _ocsp: &[u8],
        now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        if self.verify_ca {
            let cert = rustls::server::ParsedCertificate::try_from(end)?;
            rustls::client::verify_server_cert_signed_by_trust_anchor(
                &cert,
                &self.roots,
                intermediates,
                now,
                self.provider.signature_verification_algorithms.all,
            )?;
            if self.verify_name {
                rustls::client::verify_server_name(&cert, name)?;
            }
        }
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
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
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
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
pub fn pg_tls(c: &ConnectionConfig) -> Result<ClientConfig> {
    // Static Mozilla roots: never enumerate the user's Keychain.
    let mut roots = RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    if let Some(path) = c.ssl_ca.as_ref().filter(|s| !s.is_empty()) {
        roots = RootCertStore::empty();
        for cert in rustls_pemfile::certs(&mut BufReader::new(std::fs::File::open(path)?)) {
            roots.add(cert?)?;
        }
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = Verify {
        roots,
        verify_ca: matches!(c.ssl_mode, SslMode::VerifyCa | SslMode::VerifyFull),
        verify_name: c.ssl_mode == SslMode::VerifyFull,
        provider: provider.clone(),
    };
    let builder = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier));
    let mut cfg = match (&c.ssl_cert, &c.ssl_key) {
        (Some(cert), Some(key)) => {
            let certs = rustls_pemfile::certs(&mut BufReader::new(std::fs::File::open(cert)?))
                .collect::<std::io::Result<Vec<_>>>()?;
            let key = rustls_pemfile::private_key(&mut BufReader::new(std::fs::File::open(key)?))?
                .context("No TLS private key in PEM")?;
            builder.with_client_auth_cert(certs, key)?
        }
        _ => builder.with_no_client_auth(),
    };
    cfg.alpn_protocols = vec![b"postgresql".to_vec()];
    Ok(cfg)
}
