use std::sync::Arc;

use rcgen::{CertificateParams, KeyPair as RcgenKeyPair};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName};
use rustls::{ClientConfig, ServerConfig};
use tokio::net::TcpStream;

use crate::identity::NodeIdentity;

/// A TLS-wrapped TCP stream (either client or server side).
pub type PeerStream = tokio_rustls::TlsStream<TcpStream>;

/// Generate a self-signed X.509 certificate from the node identity.
pub fn generate_self_signed_cert(
    identity: &NodeIdentity,
) -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
    let subject = format!(
        "rxrpl-node-{}",
        hex::encode(&identity.node_id.as_bytes()[..8])
    );
    let mut params = CertificateParams::new(vec![subject]).expect("valid cert params");
    params.distinguished_name = rcgen::DistinguishedName::new();

    let key_pair = RcgenKeyPair::generate().expect("keygen");
    let cert = params.self_signed(&key_pair).expect("self-sign");

    let cert_der = CertificateDer::from(cert.der().to_vec());
    let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_pair.serialize_der()));

    (cert_der, key_der)
}

/// Build a rustls `ServerConfig` that accepts any client certificate.
///
/// Peer authentication happens at the overlay handshake layer, not TLS.
pub fn build_server_config(identity: &NodeIdentity) -> Arc<ServerConfig> {
    let (cert_der, key_der) = generate_self_signed_cert(identity);

    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der)
        .expect("valid server TLS config");

    Arc::new(config)
}

/// Build a rustls `ClientConfig` that accepts any server certificate.
///
/// Peer authentication happens at the overlay handshake layer, not TLS.
pub fn build_client_config() -> Arc<ClientConfig> {
    let config = ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAnyCert))
        .with_no_client_auth();

    Arc::new(config)
}

/// Connect a TLS client stream over the given TCP stream.
pub async fn connect_tls(
    stream: TcpStream,
    client_config: &Arc<ClientConfig>,
) -> Result<PeerStream, std::io::Error> {
    let connector = tokio_rustls::TlsConnector::from(Arc::clone(client_config));
    let server_name = ServerName::try_from("rxrpl-peer").expect("valid DNS name");
    let tls = connector.connect(server_name, stream).await?;
    Ok(tokio_rustls::TlsStream::Client(tls))
}

/// Accept a TLS server stream over the given TCP stream.
pub async fn accept_tls(
    stream: TcpStream,
    server_config: &Arc<ServerConfig>,
) -> Result<PeerStream, std::io::Error> {
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::clone(server_config));
    let tls = acceptor.accept(stream).await?;
    Ok(tokio_rustls::TlsStream::Server(tls))
}

/// Certificate verifier that accepts any certificate.
///
/// This is safe because peer identity verification happens in the
/// overlay handshake protocol (public key + signature proof).
#[derive(Debug)]
struct AcceptAnyCert;

impl rustls::client::danger::ServerCertVerifier for AcceptAnyCert {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::aws_lc_rs::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_cert_roundtrip() {
        let identity = NodeIdentity::generate();
        let (cert, key) = generate_self_signed_cert(&identity);
        assert!(!cert.is_empty());
        assert!(!key.secret_der().is_empty());
    }

    #[test]
    fn build_configs() {
        let identity = NodeIdentity::generate();
        let _server = build_server_config(&identity);
        let _client = build_client_config();
    }
}
