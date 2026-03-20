use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use rxrpl_p2p_proto::MessageType;
use rxrpl_p2p_proto::codec::{PeerCodec, PeerMessage};
use rxrpl_primitives::Hash256;
use tokio::time::timeout;
use tokio_util::codec::Framed;

use crate::error::OverlayError;
use crate::identity::NodeIdentity;
use crate::proto_convert;
use crate::tls::PeerStream;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

/// Perform outbound handshake: send our Hello, then receive theirs.
pub async fn handshake_outbound(
    framed: &mut Framed<PeerStream, PeerCodec>,
    identity: &NodeIdentity,
    network_id: u32,
    ledger_seq: u32,
    ledger_hash: &Hash256,
) -> Result<Hash256, OverlayError> {
    let hello_payload = proto_convert::encode_hello(identity, network_id, ledger_seq, ledger_hash);

    // Send our hello
    let msg = PeerMessage {
        msg_type: MessageType::Hello,
        payload: hello_payload,
    };
    timeout(HANDSHAKE_TIMEOUT, framed.send(msg))
        .await
        .map_err(|_| OverlayError::Handshake("send hello timeout".into()))?
        .map_err(|e| OverlayError::Handshake(format!("send hello: {e}")))?;

    // Receive their hello
    let peer_msg = timeout(HANDSHAKE_TIMEOUT, framed.next())
        .await
        .map_err(|_| OverlayError::Handshake("receive hello timeout".into()))?
        .ok_or_else(|| OverlayError::Handshake("connection closed before hello".into()))?
        .map_err(|e| OverlayError::Handshake(format!("receive hello: {e}")))?;

    if peer_msg.msg_type != MessageType::Hello {
        return Err(OverlayError::Handshake(format!(
            "expected Hello, got {:?}",
            peer_msg.msg_type
        )));
    }

    validate_hello(&peer_msg.payload, network_id, identity)
}

/// Perform inbound handshake: receive their Hello, then send ours.
pub async fn handshake_inbound(
    framed: &mut Framed<PeerStream, PeerCodec>,
    identity: &NodeIdentity,
    network_id: u32,
    ledger_seq: u32,
    ledger_hash: &Hash256,
) -> Result<Hash256, OverlayError> {
    // Receive their hello first
    let peer_msg = timeout(HANDSHAKE_TIMEOUT, framed.next())
        .await
        .map_err(|_| OverlayError::Handshake("receive hello timeout".into()))?
        .ok_or_else(|| OverlayError::Handshake("connection closed before hello".into()))?
        .map_err(|e| OverlayError::Handshake(format!("receive hello: {e}")))?;

    if peer_msg.msg_type != MessageType::Hello {
        return Err(OverlayError::Handshake(format!(
            "expected Hello, got {:?}",
            peer_msg.msg_type
        )));
    }

    let peer_node_id = validate_hello(&peer_msg.payload, network_id, identity)?;

    // Send our hello
    let hello_payload = proto_convert::encode_hello(identity, network_id, ledger_seq, ledger_hash);
    let msg = PeerMessage {
        msg_type: MessageType::Hello,
        payload: hello_payload,
    };
    timeout(HANDSHAKE_TIMEOUT, framed.send(msg))
        .await
        .map_err(|_| OverlayError::Handshake("send hello timeout".into()))?
        .map_err(|e| OverlayError::Handshake(format!("send hello: {e}")))?;

    Ok(peer_node_id)
}

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

    if !rxrpl_crypto::ed25519::verify(proof_hash.as_bytes(), &hello.node_public, &hello.node_proof)
    {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tls;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn handshake_success() {
        let network_id = 42;
        let ledger_hash = Hash256::new([0xAA; 32]);

        let id_b = NodeIdentity::from_seed(&rxrpl_crypto::Seed::from_passphrase("node-b"));
        let server_config = tls::build_server_config(&id_b);

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let handle = tokio::spawn({
            async move {
                let (tcp, _) = listener.accept().await.unwrap();
                let stream = tls::accept_tls(tcp, &server_config).await.unwrap();
                let mut framed = Framed::new(stream, PeerCodec);
                handshake_inbound(&mut framed, &id_b, network_id, 1, &ledger_hash).await
            }
        });

        let id_a = NodeIdentity::from_seed(&rxrpl_crypto::Seed::from_passphrase("node-a"));
        let client_config = tls::build_client_config();

        let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
        let stream = tls::connect_tls(tcp, &client_config).await.unwrap();
        let mut framed = Framed::new(stream, PeerCodec);

        let result = handshake_outbound(&mut framed, &id_a, network_id, 1, &ledger_hash).await;
        assert!(result.is_ok());

        let inbound_result = handle.await.unwrap();
        assert!(inbound_result.is_ok());

        assert_eq!(
            result.unwrap(),
            NodeIdentity::from_seed(&rxrpl_crypto::Seed::from_passphrase("node-b")).node_id
        );
        assert_eq!(inbound_result.unwrap(), id_a.node_id);
    }

    #[tokio::test]
    async fn handshake_over_tls() {
        let network_id = 100;
        let ledger_hash = Hash256::new([0xCC; 32]);

        let id_server = NodeIdentity::from_seed(&rxrpl_crypto::Seed::from_passphrase("tls-server"));
        let id_client = NodeIdentity::from_seed(&rxrpl_crypto::Seed::from_passphrase("tls-client"));

        let server_tls = tls::build_server_config(&id_server);
        let client_tls = tls::build_client_config();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server_handle = tokio::spawn({
            let id = NodeIdentity::from_seed(&rxrpl_crypto::Seed::from_passphrase("tls-server"));
            async move {
                let (tcp, _) = listener.accept().await.unwrap();
                let tls_stream = tls::accept_tls(tcp, &server_tls).await.unwrap();
                let mut framed = Framed::new(tls_stream, PeerCodec);
                handshake_inbound(&mut framed, &id, network_id, 5, &ledger_hash).await
            }
        });

        let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
        let tls_stream = tls::connect_tls(tcp, &client_tls).await.unwrap();
        let mut framed = Framed::new(tls_stream, PeerCodec);

        let client_result =
            handshake_outbound(&mut framed, &id_client, network_id, 5, &ledger_hash).await;
        assert!(
            client_result.is_ok(),
            "client handshake failed: {:?}",
            client_result.err()
        );

        let server_result = server_handle.await.unwrap();
        assert!(
            server_result.is_ok(),
            "server handshake failed: {:?}",
            server_result.err()
        );

        assert_eq!(client_result.unwrap(), id_server.node_id);
        assert_eq!(server_result.unwrap(), id_client.node_id);
    }

    #[tokio::test]
    async fn handshake_network_id_mismatch() {
        let ledger_hash = Hash256::new([0xBB; 32]);

        let id_server = NodeIdentity::from_seed(&rxrpl_crypto::Seed::from_passphrase("server"));
        let server_tls = tls::build_server_config(&id_server);
        let client_tls = tls::build_client_config();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let handle = tokio::spawn(async move {
            let id = NodeIdentity::from_seed(&rxrpl_crypto::Seed::from_passphrase("server"));
            let (tcp, _) = listener.accept().await.unwrap();
            let stream = tls::accept_tls(tcp, &server_tls).await.unwrap();
            let mut framed = Framed::new(stream, PeerCodec);
            handshake_inbound(&mut framed, &id, 99, 1, &ledger_hash).await
        });

        let id = NodeIdentity::from_seed(&rxrpl_crypto::Seed::from_passphrase("client"));
        let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
        let stream = tls::connect_tls(tcp, &client_tls).await.unwrap();
        let mut framed = Framed::new(stream, PeerCodec);
        // Client uses network_id=1, server expects 99
        let _result = handshake_outbound(&mut framed, &id, 1, 1, &ledger_hash).await;

        let server_result = handle.await.unwrap();
        assert!(server_result.is_err());
        let err = server_result.unwrap_err().to_string();
        assert!(err.contains("network_id mismatch"), "got: {err}");
    }
}
