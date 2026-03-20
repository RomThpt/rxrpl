use std::sync::Arc;

use openssl::hash::MessageDigest;
use openssl::pkey::PKey;
use openssl::rsa::Rsa;
use openssl::ssl::{SslAcceptor, SslConnector, SslMethod, SslVerifyMode};
use openssl::x509::X509NameBuilder;
use tokio::net::TcpStream;

use crate::error::OverlayError;
use crate::identity::NodeIdentity;

/// A TLS-wrapped TCP stream (openssl).
pub type PeerStream = tokio_openssl::SslStream<TcpStream>;

/// Generate a self-signed X.509 certificate.
pub fn generate_self_signed_cert(
    identity: &NodeIdentity,
) -> (openssl::x509::X509, PKey<openssl::pkey::Private>) {
    let rsa = Rsa::generate(2048).expect("RSA keygen");
    let pkey = PKey::from_rsa(rsa).expect("PKey");

    let mut name = X509NameBuilder::new().expect("name builder");
    name.append_entry_by_text(
        "CN",
        &format!(
            "rxrpl-node-{}",
            hex::encode(&identity.node_id.as_bytes()[..8])
        ),
    )
    .expect("CN entry");
    let name = name.build();

    let mut cert = openssl::x509::X509Builder::new().expect("cert builder");
    cert.set_version(2).expect("version");
    cert.set_subject_name(&name).expect("subject");
    cert.set_issuer_name(&name).expect("issuer");
    cert.set_pubkey(&pkey).expect("pubkey");

    let not_before = openssl::asn1::Asn1Time::days_from_now(0).expect("not_before");
    let not_after = openssl::asn1::Asn1Time::days_from_now(365).expect("not_after");
    cert.set_not_before(&not_before).expect("set not_before");
    cert.set_not_after(&not_after).expect("set not_after");

    cert.sign(&pkey, MessageDigest::sha256()).expect("sign");
    (cert.build(), pkey)
}

/// Build an SslAcceptor (server side) that accepts any client certificate.
///
/// Peer authentication happens at the overlay handshake layer, not TLS.
pub fn build_server_config(identity: &NodeIdentity) -> Arc<SslAcceptor> {
    let (cert, pkey) = generate_self_signed_cert(identity);

    let mut builder = SslAcceptor::mozilla_intermediate(SslMethod::tls()).expect("acceptor");
    builder.set_certificate(&cert).expect("set cert");
    builder.set_private_key(&pkey).expect("set key");
    builder.check_private_key().expect("check key");
    builder.set_verify(SslVerifyMode::NONE);

    Arc::new(builder.build())
}

/// Build an SslConnector (client side) that accepts any server certificate.
///
/// Peer authentication happens at the overlay handshake layer, not TLS.
pub fn build_client_config() -> Arc<SslConnector> {
    let mut builder = SslConnector::builder(SslMethod::tls()).expect("connector");
    builder.set_verify(SslVerifyMode::NONE);
    Arc::new(builder.build())
}

/// Connect a TLS client stream.
pub async fn connect_tls(
    stream: TcpStream,
    client_config: &Arc<SslConnector>,
) -> Result<PeerStream, OverlayError> {
    let ssl = client_config
        .configure()
        .map_err(|e| OverlayError::Connection(format!("SSL configure: {e}")))?
        .verify_hostname(false)
        .into_ssl("rxrpl-peer")
        .map_err(|e| OverlayError::Connection(format!("SSL create: {e}")))?;

    let mut tls = tokio_openssl::SslStream::new(ssl, stream)
        .map_err(|e| OverlayError::Connection(format!("SSL stream: {e}")))?;

    std::pin::Pin::new(&mut tls)
        .connect()
        .await
        .map_err(|e| OverlayError::Connection(format!("TLS connect: {e}")))?;

    Ok(tls)
}

/// Accept a TLS server stream.
pub async fn accept_tls(
    stream: TcpStream,
    server_config: &Arc<SslAcceptor>,
) -> Result<PeerStream, OverlayError> {
    let ssl = openssl::ssl::Ssl::new(server_config.context())
        .map_err(|e| OverlayError::Connection(format!("SSL create: {e}")))?;

    let mut tls = tokio_openssl::SslStream::new(ssl, stream)
        .map_err(|e| OverlayError::Connection(format!("SSL stream: {e}")))?;

    std::pin::Pin::new(&mut tls)
        .accept()
        .await
        .map_err(|e| OverlayError::Connection(format!("TLS accept: {e}")))?;

    Ok(tls)
}

/// Extract the session cookie from a TLS stream for the XRPL peer handshake.
///
/// Matches rippled's `makeSharedValue()`:
/// 1. cookie1 = SHA-512(local finished message)
/// 2. cookie2 = SHA-512(peer finished message)
/// 3. shared = cookie1 XOR cookie2
/// 4. cookie = SHA-512-Half(shared)
pub fn extract_session_cookie(
    stream: &PeerStream,
) -> Result<rxrpl_primitives::Hash256, OverlayError> {
    let ssl = stream.ssl();

    let mut local_buf = [0u8; 256];
    let local_len = ssl.finished(&mut local_buf);
    if local_len < 12 {
        return Err(OverlayError::Handshake(
            "local finished message too short".into(),
        ));
    }

    let mut peer_buf = [0u8; 256];
    let peer_len = ssl.peer_finished(&mut peer_buf);
    if peer_len < 12 {
        return Err(OverlayError::Handshake(
            "peer finished message too short".into(),
        ));
    }

    use sha2::{Digest, Sha512};
    let cookie1 = Sha512::digest(&local_buf[..local_len]);
    let cookie2 = Sha512::digest(&peer_buf[..peer_len]);

    let mut shared = [0u8; 64];
    for i in 0..64 {
        shared[i] = cookie1[i] ^ cookie2[i];
    }

    if shared.iter().all(|&b| b == 0) {
        return Err(OverlayError::Handshake(
            "identical finished messages".into(),
        ));
    }

    Ok(rxrpl_crypto::sha512_half::sha512_half(&[&shared]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_cert_roundtrip() {
        let identity = NodeIdentity::generate();
        let (cert, pkey) = generate_self_signed_cert(&identity);
        assert!(!cert.to_der().unwrap().is_empty());
        assert!(!pkey.private_key_to_der().unwrap().is_empty());
    }

    #[test]
    fn build_configs() {
        let identity = NodeIdentity::generate();
        let _server = build_server_config(&identity);
        let _client = build_client_config();
    }
}
