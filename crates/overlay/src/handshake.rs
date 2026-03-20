use std::time::Duration;

use rxrpl_p2p_proto::codec::PeerCodec;
use rxrpl_primitives::Hash256;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::timeout;
use tokio_util::codec::Framed;

use crate::error::OverlayError;
use crate::http;
use crate::identity::NodeIdentity;
use crate::proto_convert;
use crate::tls::{self, PeerStream};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

/// Validate a received TMHello message and return the peer's node ID.
fn validate_hello(
    payload: &[u8],
    expected_network_id: u32,
    our_identity: &NodeIdentity,
) -> Result<Hash256, OverlayError> {
    let hello = proto_convert::decode_hello(payload)?;

    // Check network ID
    if hello.network_id != expected_network_id {
        return Err(OverlayError::Handshake(format!(
            "network_id mismatch: expected {}, got {}",
            expected_network_id, hello.network_id
        )));
    }

    // Check protocol version compatibility
    const OUR_PROTO_VERSION_MIN: u32 = 2;
    if hello.proto_version < OUR_PROTO_VERSION_MIN {
        return Err(OverlayError::Handshake(format!(
            "peer protocol version {} too old (min {})",
            hello.proto_version, OUR_PROTO_VERSION_MIN
        )));
    }

    // Verify node_proof: signature of SHA-512-Half(peer_pubkey || "XRPL-HANDSHAKE")
    let mut proof_data = Vec::new();
    proof_data.extend_from_slice(&hello.node_public);
    proof_data.extend_from_slice(b"XRPL-HANDSHAKE");
    let proof_hash = rxrpl_crypto::sha512_half::sha512_half(&[&proof_data]);

    let proof_valid = if hello.node_public.first() == Some(&0xED) {
        rxrpl_crypto::ed25519::verify(proof_hash.as_bytes(), &hello.node_public, &hello.node_proof)
    } else {
        rxrpl_crypto::secp256k1::verify(proof_hash.as_bytes(), &hello.node_public, &hello.node_proof)
    };
    if !proof_valid {
        return Err(OverlayError::Handshake("invalid node_proof".into()));
    }

    // Derive peer node ID
    let peer_node_id = rxrpl_crypto::sha512_half::sha512_half(&[&hello.node_public]);

    // Self-connection check
    if peer_node_id == our_identity.node_id {
        return Err(OverlayError::Handshake("self-connection detected".into()));
    }

    Ok(peer_node_id)
}

/// Maximum size of an HTTP upgrade request/response (16 KB should be more than enough).
const MAX_HTTP_HEADER_SIZE: usize = 16 * 1024;

/// Node public key base58 prefix (produces "n..." addresses).
const NODE_PUBLIC_KEY_PREFIX: &[u8] = &[0x1C];

/// Encode a node public key to base58 (nXXXX format used by rippled).
fn encode_node_public_key(pubkey_bytes: &[u8]) -> String {
    rxrpl_codec::address::base58::base58check_encode(pubkey_bytes, NODE_PUBLIC_KEY_PREFIX)
}

/// Decode a base58-encoded node public key back to raw bytes.
fn decode_node_public_key(encoded: &str) -> Result<Vec<u8>, OverlayError> {
    let decoded = rxrpl_codec::address::base58::base58check_decode(encoded)
        .map_err(|e| OverlayError::Handshake(format!("invalid node public key: {e}")))?;
    // Strip the 2-byte prefix (0x1C)
    if decoded.len() < 2 {
        return Err(OverlayError::Handshake("node public key too short".into()));
    }
    Ok(decoded[1..].to_vec())
}

/// Build the common XRPL HTTP upgrade headers for the handshake.
///
/// The cookie is the 32-byte SHA-512-Half of the TLS exported keying material.
/// For secp256k1 keys, we sign the cookie using our `sign()` method which
/// internally does SHA-512-Half before ECDSA -- matching rippled's `signDigest`.
fn build_upgrade_headers(
    identity: &NodeIdentity,
    cookie: &Hash256,
    network_id: u32,
    ledger_hash: &Hash256,
) -> Vec<(String, String)> {
    use base64::Engine;
    let signature = identity.sign_digest(cookie.as_bytes());
    let pubkey_b58 = encode_node_public_key(identity.public_key_bytes());
    let sig_b64 = base64::engine::general_purpose::STANDARD.encode(&signature);
    let mut headers = vec![
        ("Upgrade".into(), "XRPL/2.2".into()),
        ("Connection".into(), "Upgrade".into()),
        ("Connect-As".into(), "Peer".into()),
        ("Public-Key".into(), pubkey_b58),
        ("Session-Signature".into(), sig_b64),
        ("Network-ID".into(), network_id.to_string()),
        ("Crawl".into(), "public".into()),
    ];
    // Only send Closed-Ledger if we have a real ledger hash (rippled expects hex uint256)
    if *ledger_hash != Hash256::ZERO {
        headers.push(("Closed-Ledger".into(), ledger_hash.to_string()));
    }
    headers
}

/// Read from the stream until we find the `\r\n\r\n` HTTP header terminator.
async fn read_until_header_end(stream: &mut PeerStream) -> Result<Vec<u8>, OverlayError> {
    let mut buf = Vec::with_capacity(1024);
    let mut single = [0u8; 1];

    loop {
        if buf.len() >= MAX_HTTP_HEADER_SIZE {
            return Err(OverlayError::Handshake(
                "HTTP header too large".into(),
            ));
        }

        let n = stream
            .read(&mut single)
            .await
            .map_err(|e| OverlayError::Handshake(format!("read HTTP header: {e}")))?;
        if n == 0 {
            return Err(OverlayError::Handshake(
                "connection closed during HTTP header read".into(),
            ));
        }
        buf.push(single[0]);

        if buf.len() >= 4 && buf[buf.len() - 4..] == *b"\r\n\r\n" {
            return Ok(buf);
        }
    }
}

/// Verify a peer's identity from HTTP upgrade headers against the TLS session cookie.
///
/// Returns the peer's node ID (SHA-512-Half of their public key).
fn verify_peer_http_headers(
    headers: &[(String, String)],
    cookie: &Hash256,
    our_identity: &NodeIdentity,
    expected_network_id: u32,
) -> Result<Hash256, OverlayError> {
    // Extract required headers
    let peer_pubkey_hex = http::get_header(headers, "Public-Key")
        .ok_or_else(|| OverlayError::Handshake("missing Public-Key header".into()))?;
    let peer_sig_hex = http::get_header(headers, "Session-Signature")
        .ok_or_else(|| OverlayError::Handshake("missing Session-Signature header".into()))?;
    let network_id_str = http::get_header(headers, "Network-ID")
        .ok_or_else(|| OverlayError::Handshake("missing Network-ID header".into()))?;

    // Verify network ID
    let peer_network_id: u32 = network_id_str
        .parse()
        .map_err(|_| OverlayError::Handshake(format!("invalid Network-ID: {network_id_str}")))?;
    if peer_network_id != expected_network_id {
        return Err(OverlayError::Handshake(format!(
            "network_id mismatch: expected {expected_network_id}, got {peer_network_id}"
        )));
    }

    // Decode public key (base58 nXXXX format) and signature (base64)
    use base64::Engine;
    let peer_pubkey = decode_node_public_key(peer_pubkey_hex)?;
    let peer_sig = base64::engine::general_purpose::STANDARD
        .decode(peer_sig_hex)
        .map_err(|e| OverlayError::Handshake(format!("invalid Session-Signature base64: {e}")))?;

    // Verify the session signature against the TLS session cookie (pre-hashed digest)
    // Determine key type from first byte: 0xED = Ed25519, 0x02/0x03 = secp256k1
    let sig_valid = if peer_pubkey.first() == Some(&0xED) {
        rxrpl_crypto::ed25519::verify(cookie.as_bytes(), &peer_pubkey, &peer_sig)
    } else {
        rxrpl_crypto::secp256k1::verify_digest(cookie.as_bytes(), &peer_pubkey, &peer_sig)
    };
    if !sig_valid {
        return Err(OverlayError::Handshake(
            "invalid Session-Signature".into(),
        ));
    }

    // Derive peer node ID
    let peer_node_id = rxrpl_crypto::sha512_half::sha512_half(&[&peer_pubkey]);

    // Self-connection check
    if peer_node_id == our_identity.node_id {
        return Err(OverlayError::Handshake("self-connection detected".into()));
    }

    Ok(peer_node_id)
}

/// Perform outbound HTTP upgrade handshake (rippled-compatible).
///
/// Sends an HTTP upgrade request over the raw TLS stream, reads the 101 response,
/// verifies the peer's identity, then wraps the stream in a Framed codec for
/// subsequent protobuf messaging.
pub async fn handshake_outbound_http(
    mut stream: PeerStream,
    identity: &NodeIdentity,
    network_id: u32,
    _ledger_seq: u32,
    ledger_hash: &Hash256,
) -> Result<(Hash256, Framed<PeerStream, PeerCodec>), OverlayError> {
    let cookie = tls::extract_session_cookie(&stream)?;
    let headers = build_upgrade_headers(identity, &cookie, network_id, ledger_hash);
    let request = http::format_http_request(&headers);

    // Send HTTP upgrade request
    timeout(HANDSHAKE_TIMEOUT, stream.write_all(&request))
        .await
        .map_err(|_| OverlayError::Handshake("send HTTP upgrade timeout".into()))?
        .map_err(|e| OverlayError::Handshake(format!("send HTTP upgrade: {e}")))?;

    timeout(HANDSHAKE_TIMEOUT, stream.flush())
        .await
        .map_err(|_| OverlayError::Handshake("flush HTTP upgrade timeout".into()))?
        .map_err(|e| OverlayError::Handshake(format!("flush HTTP upgrade: {e}")))?;

    // Read HTTP response
    let response_buf = timeout(HANDSHAKE_TIMEOUT, read_until_header_end(&mut stream))
        .await
        .map_err(|_| OverlayError::Handshake("receive HTTP response timeout".into()))?
        ?;

    let (status, resp_headers) = http::parse_http_response(&response_buf)
        .map_err(|e| OverlayError::Handshake(format!("parse HTTP response: {e}")))?;

    if status != 101 {
        let req_text = String::from_utf8_lossy(&request);
        let resp_text = String::from_utf8_lossy(&response_buf);
        tracing::error!("HTTP upgrade rejected by peer.\n--- REQUEST ---\n{req_text}--- RESPONSE ---\n{resp_text}");
        return Err(OverlayError::Handshake(format!(
            "expected HTTP 101, got {status}"
        )));
    }

    // Verify peer identity from response headers
    let peer_node_id =
        verify_peer_http_headers(&resp_headers, &cookie, identity, network_id)?;

    let framed = Framed::new(stream, PeerCodec);
    Ok((peer_node_id, framed))
}

/// Perform inbound HTTP upgrade handshake (rippled-compatible).
///
/// Reads an HTTP upgrade request from the raw TLS stream, verifies the peer's
/// identity, sends a 101 response with our identity, then wraps the stream in a
/// Framed codec for subsequent protobuf messaging.
pub async fn handshake_inbound_http(
    mut stream: PeerStream,
    identity: &NodeIdentity,
    network_id: u32,
    _ledger_seq: u32,
    ledger_hash: &Hash256,
) -> Result<(Hash256, Framed<PeerStream, PeerCodec>), OverlayError> {
    let cookie = tls::extract_session_cookie(&stream)?;

    // Read HTTP upgrade request
    let request_buf = timeout(HANDSHAKE_TIMEOUT, read_until_header_end(&mut stream))
        .await
        .map_err(|_| OverlayError::Handshake("receive HTTP request timeout".into()))?
        ?;

    let req_headers = http::parse_http_request(&request_buf)
        .map_err(|e| OverlayError::Handshake(format!("parse HTTP request: {e}")))?;

    // Verify peer identity from request headers
    let peer_node_id =
        verify_peer_http_headers(&req_headers, &cookie, identity, network_id)?;

    // Send HTTP 101 response with our identity
    let headers = build_upgrade_headers(identity, &cookie, network_id, ledger_hash);
    let response = http::format_http_response(101, "Switching Protocols", &headers);

    timeout(HANDSHAKE_TIMEOUT, stream.write_all(&response))
        .await
        .map_err(|_| OverlayError::Handshake("send HTTP response timeout".into()))?
        .map_err(|e| OverlayError::Handshake(format!("send HTTP response: {e}")))?;

    timeout(HANDSHAKE_TIMEOUT, stream.flush())
        .await
        .map_err(|_| OverlayError::Handshake("flush HTTP response timeout".into()))?
        .map_err(|e| OverlayError::Handshake(format!("flush HTTP response: {e}")))?;

    let framed = Framed::new(stream, PeerCodec);
    Ok((peer_node_id, framed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tls;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn http_handshake_success() {
        let network_id = 42;
        let ledger_hash = Hash256::new([0xAA; 32]);

        let id_b = NodeIdentity::from_seed(&rxrpl_crypto::Seed::from_passphrase("http-node-b"));
        let server_config = tls::build_server_config(&id_b);

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let handle = tokio::spawn({
            async move {
                let (tcp, _) = listener.accept().await.unwrap();
                let stream = tls::accept_tls(tcp, &server_config).await.unwrap();
                handshake_inbound_http(stream, &id_b, network_id, 1, &ledger_hash).await
            }
        });

        let id_a = NodeIdentity::from_seed(&rxrpl_crypto::Seed::from_passphrase("http-node-a"));
        let client_config = tls::build_client_config();

        let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
        let stream = tls::connect_tls(tcp, &client_config).await.unwrap();

        let result =
            handshake_outbound_http(stream, &id_a, network_id, 1, &ledger_hash).await;
        assert!(result.is_ok(), "outbound failed: {:?}", result.err());

        let (peer_id_from_client, _framed_client) = result.unwrap();

        let inbound_result = handle.await.unwrap();
        assert!(
            inbound_result.is_ok(),
            "inbound failed: {:?}",
            inbound_result.err()
        );
        let (peer_id_from_server, _framed_server) = inbound_result.unwrap();

        assert_eq!(
            peer_id_from_client,
            NodeIdentity::from_seed(&rxrpl_crypto::Seed::from_passphrase("http-node-b")).node_id
        );
        assert_eq!(peer_id_from_server, id_a.node_id);
    }

    #[tokio::test]
    async fn http_handshake_network_id_mismatch() {
        let ledger_hash = Hash256::new([0xBB; 32]);

        let id_server =
            NodeIdentity::from_seed(&rxrpl_crypto::Seed::from_passphrase("http-server"));
        let server_tls = tls::build_server_config(&id_server);
        let client_tls = tls::build_client_config();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let handle = tokio::spawn(async move {
            let id =
                NodeIdentity::from_seed(&rxrpl_crypto::Seed::from_passphrase("http-server"));
            let (tcp, _) = listener.accept().await.unwrap();
            let stream = tls::accept_tls(tcp, &server_tls).await.unwrap();
            handshake_inbound_http(stream, &id, 99, 1, &ledger_hash).await
        });

        let id =
            NodeIdentity::from_seed(&rxrpl_crypto::Seed::from_passphrase("http-client"));
        let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
        let stream = tls::connect_tls(tcp, &client_tls).await.unwrap();
        // Client uses network_id=1, server expects 99
        let _result = handshake_outbound_http(stream, &id, 1, 1, &ledger_hash).await;

        let server_result = handle.await.unwrap();
        assert!(server_result.is_err());
        let err = server_result.err().unwrap().to_string();
        assert!(err.contains("network_id mismatch"), "got: {err}");
    }

    #[tokio::test]
    async fn http_handshake_self_connection() {
        let network_id = 42;
        let ledger_hash = Hash256::new([0xCC; 32]);

        let id = NodeIdentity::from_seed(&rxrpl_crypto::Seed::from_passphrase("self-node"));
        let id_clone =
            NodeIdentity::from_seed(&rxrpl_crypto::Seed::from_passphrase("self-node"));
        let server_config = tls::build_server_config(&id);
        let client_config = tls::build_client_config();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let handle = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let stream = tls::accept_tls(tcp, &server_config).await.unwrap();
            handshake_inbound_http(stream, &id, network_id, 1, &ledger_hash).await
        });

        let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
        let stream = tls::connect_tls(tcp, &client_config).await.unwrap();
        let result =
            handshake_outbound_http(stream, &id_clone, network_id, 1, &ledger_hash).await;

        // Either side should detect self-connection
        let client_is_err = result.is_err();
        let server_result = handle.await.unwrap();
        let server_is_err = server_result.is_err();

        assert!(
            client_is_err || server_is_err,
            "at least one side should detect self-connection"
        );
    }
}
