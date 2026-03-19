use prost::Message;
use rxrpl_consensus::types::{NodeId, Proposal, Validation};
use rxrpl_p2p_proto::proto::{
    TmGetLedger, TmHello, TmLedgerData, TmPing, TmProposeSet, TmStatusChange, TmTransaction,
    TmValidation, tm_ledger_data::TmLedgerNode,
};
use rxrpl_primitives::Hash256;

use crate::error::OverlayError;
use crate::identity::NodeIdentity;

// --- ProposeSet ---

pub fn encode_propose_set(proposal: &Proposal) -> Vec<u8> {
    let msg = TmProposeSet {
        propose_seq: proposal.prop_seq,
        current_tx_hash: proposal.tx_set_hash.as_bytes().to_vec(),
        node_pub_key: proposal.node_id.0.as_bytes().to_vec(),
        close_time: proposal.close_time,
        signature: proposal.signature.clone().unwrap_or_default(),
        previous_ledger: proposal.prev_ledger.as_bytes().to_vec(),
        ledger_seq: proposal.ledger_seq,
    };
    msg.encode_to_vec()
}

pub fn decode_propose_set(data: &[u8]) -> Result<Proposal, OverlayError> {
    let msg = TmProposeSet::decode(data)
        .map_err(|e| OverlayError::Codec(format!("decode ProposeSet: {e}")))?;

    let node_id = NodeId(hash256_from_bytes(&msg.node_pub_key)?);
    let tx_set_hash = hash256_from_bytes(&msg.current_tx_hash)?;
    let prev_ledger = hash256_from_bytes(&msg.previous_ledger)?;

    Ok(Proposal {
        node_id,
        tx_set_hash,
        close_time: msg.close_time,
        prop_seq: msg.propose_seq,
        ledger_seq: msg.ledger_seq,
        prev_ledger,
        signature: if msg.signature.is_empty() {
            None
        } else {
            Some(msg.signature)
        },
    })
}

// --- Validation ---

pub fn encode_validation(validation: &Validation) -> Vec<u8> {
    // Pack validation data: signing_data + signature
    let mut payload = validation.signing_data();
    if let Some(ref sig) = validation.signature {
        payload.extend_from_slice(sig);
    }
    let msg = TmValidation {
        validation: payload,
        ledger_seq: validation.ledger_seq,
    };
    msg.encode_to_vec()
}

pub fn decode_validation(data: &[u8]) -> Result<Validation, OverlayError> {
    let msg = TmValidation::decode(data)
        .map_err(|e| OverlayError::Codec(format!("decode Validation: {e}")))?;

    // Validation payload: signing_data(45 bytes) + signature(64 bytes)
    let payload = &msg.validation;
    if payload.len() < 45 {
        return Err(OverlayError::Codec("validation payload too short".into()));
    }

    let ledger_hash = hash256_from_bytes(&payload[0..32])?;
    let ledger_seq = u32::from_be_bytes(payload[32..36].try_into().unwrap());
    let close_time = u32::from_be_bytes(payload[36..40].try_into().unwrap());
    let sign_time = u32::from_be_bytes(payload[40..44].try_into().unwrap());
    let full = payload[44] != 0;

    let signature = if payload.len() > 45 {
        Some(payload[45..].to_vec())
    } else {
        None
    };

    Ok(Validation {
        node_id: NodeId(Hash256::ZERO), // caller must set from context
        ledger_hash,
        ledger_seq,
        full,
        close_time,
        sign_time,
        signature,
    })
}

// --- Transaction ---

pub fn encode_transaction(tx_hash: &Hash256, tx_data: &[u8]) -> Vec<u8> {
    // Pack: hash(32) + raw_transaction
    let mut raw = Vec::with_capacity(32 + tx_data.len());
    raw.extend_from_slice(tx_hash.as_bytes());
    raw.extend_from_slice(tx_data);

    let msg = TmTransaction {
        raw_transaction: raw,
        status: 0,
        receive_timestamp: 0,
        deferred: 0,
    };
    msg.encode_to_vec()
}

pub fn decode_transaction(data: &[u8]) -> Result<(Hash256, Vec<u8>), OverlayError> {
    let msg = TmTransaction::decode(data)
        .map_err(|e| OverlayError::Codec(format!("decode Transaction: {e}")))?;

    if msg.raw_transaction.len() < 32 {
        return Err(OverlayError::Codec("transaction payload too short".into()));
    }

    let tx_hash = hash256_from_bytes(&msg.raw_transaction[..32])?;
    let tx_data = msg.raw_transaction[32..].to_vec();
    Ok((tx_hash, tx_data))
}

// --- StatusChange ---

pub fn encode_status_change(ledger_hash: &Hash256, ledger_seq: u32) -> Vec<u8> {
    let msg = TmStatusChange {
        new_status: 0,
        new_event: 0,
        ledger_seq,
        ledger_hash: ledger_hash.as_bytes().to_vec(),
        validated_hash: Vec::new(),
        validated_seq: 0,
    };
    msg.encode_to_vec()
}

pub fn decode_status_change(data: &[u8]) -> Result<(Hash256, u32), OverlayError> {
    let msg = TmStatusChange::decode(data)
        .map_err(|e| OverlayError::Codec(format!("decode StatusChange: {e}")))?;
    let ledger_hash = hash256_from_bytes(&msg.ledger_hash)?;
    Ok((ledger_hash, msg.ledger_seq))
}

// --- Hello ---

pub fn encode_hello(
    identity: &NodeIdentity,
    network_id: u32,
    ledger_seq: u32,
    ledger_hash: &Hash256,
) -> Vec<u8> {
    // node_proof = sign(SHA-512-Half(pubkey || "RXRPL-HANDSHAKE"))
    let mut proof_data = Vec::new();
    proof_data.extend_from_slice(identity.public_key_bytes());
    proof_data.extend_from_slice(b"RXRPL-HANDSHAKE");
    let proof_hash = rxrpl_crypto::sha512_half::sha512_half(&[&proof_data]);
    let node_proof = identity.sign(proof_hash.as_bytes());

    let network_time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let msg = TmHello {
        proto_version: 1,
        proto_version_min: 1,
        node_public: identity.public_key_bytes().to_vec(),
        node_proof,
        network_id,
        ledger_seq,
        ledger_hash: ledger_hash.as_bytes().to_vec(),
        network_time,
    };
    msg.encode_to_vec()
}

pub fn decode_hello(data: &[u8]) -> Result<TmHello, OverlayError> {
    TmHello::decode(data).map_err(|e| OverlayError::Codec(format!("decode Hello: {e}")))
}

// --- Ping ---

pub fn encode_ping(seq: u32, is_pong: bool) -> Vec<u8> {
    let msg = TmPing {
        r#type: if is_pong { 1 } else { 0 },
        seq,
        ping_time: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        net_time: 0,
    };
    msg.encode_to_vec()
}

pub fn decode_ping(data: &[u8]) -> Result<TmPing, OverlayError> {
    TmPing::decode(data).map_err(|e| OverlayError::Codec(format!("decode Ping: {e}")))
}

// --- GetLedger ---

pub fn encode_get_ledger(
    ledger_type: i32,
    hash: Option<&Hash256>,
    seq: u32,
    request_cookie: bool,
) -> Vec<u8> {
    let msg = TmGetLedger {
        ledger_type,
        ledger_hash: hash.map(|h| h.as_bytes().to_vec()).unwrap_or_default(),
        ledger_seq: seq,
        node_ids: Vec::new(),
        request_cookie,
        query_depth: 0,
    };
    msg.encode_to_vec()
}

pub fn decode_get_ledger(data: &[u8]) -> Result<TmGetLedger, OverlayError> {
    TmGetLedger::decode(data).map_err(|e| OverlayError::Codec(format!("decode GetLedger: {e}")))
}

// --- LedgerData ---

pub fn encode_ledger_data(
    hash: &Hash256,
    seq: u32,
    ltype: i32,
    nodes: Vec<(Vec<u8>, Vec<u8>)>,
    cookie: u32,
) -> Vec<u8> {
    let msg = TmLedgerData {
        ledger_hash: hash.as_bytes().to_vec(),
        ledger_seq: seq,
        ledger_type: ltype,
        nodes: nodes
            .into_iter()
            .map(|(node_id, node_data)| TmLedgerNode { node_id, node_data })
            .collect(),
        request_cookie: cookie,
    };
    msg.encode_to_vec()
}

pub fn decode_ledger_data(data: &[u8]) -> Result<TmLedgerData, OverlayError> {
    TmLedgerData::decode(data).map_err(|e| OverlayError::Codec(format!("decode LedgerData: {e}")))
}

// --- Peers ---

pub fn encode_peers(peers: Vec<(String, u16)>) -> Vec<u8> {
    use rxrpl_p2p_proto::proto::{TmPeers, tm_peers::TmPeer};
    let msg = TmPeers {
        peers: peers
            .into_iter()
            .map(|(ip, port)| TmPeer {
                ip,
                port: port as u32,
            })
            .collect(),
    };
    msg.encode_to_vec()
}

pub fn decode_peers(data: &[u8]) -> Result<Vec<(String, u16)>, OverlayError> {
    use rxrpl_p2p_proto::proto::TmPeers;
    let msg =
        TmPeers::decode(data).map_err(|e| OverlayError::Codec(format!("decode Peers: {e}")))?;
    Ok(msg
        .peers
        .into_iter()
        .map(|p| (p.ip, p.port as u16))
        .collect())
}

// --- Helpers ---

fn hash256_from_bytes(bytes: &[u8]) -> Result<Hash256, OverlayError> {
    if bytes.len() < 32 {
        // Pad with zeros if shorter (e.g. empty field)
        let mut buf = [0u8; 32];
        buf[..bytes.len()].copy_from_slice(bytes);
        return Ok(Hash256::new(buf));
    }
    let arr: [u8; 32] = bytes[..32]
        .try_into()
        .map_err(|_| OverlayError::Codec("invalid hash256 length".into()))?;
    Ok(Hash256::new(arr))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rxrpl_consensus::types::NodeId;

    #[test]
    fn propose_set_roundtrip() {
        let proposal = Proposal {
            node_id: NodeId(Hash256::new([0x01; 32])),
            tx_set_hash: Hash256::new([0x02; 32]),
            close_time: 100,
            prop_seq: 1,
            ledger_seq: 5,
            prev_ledger: Hash256::new([0x03; 32]),
            signature: Some(vec![0xAA; 64]),
        };

        let encoded = encode_propose_set(&proposal);
        let decoded = decode_propose_set(&encoded).unwrap();

        assert_eq!(decoded.node_id, proposal.node_id);
        assert_eq!(decoded.tx_set_hash, proposal.tx_set_hash);
        assert_eq!(decoded.close_time, proposal.close_time);
        assert_eq!(decoded.prop_seq, proposal.prop_seq);
        assert_eq!(decoded.ledger_seq, proposal.ledger_seq);
        assert_eq!(decoded.prev_ledger, proposal.prev_ledger);
        assert_eq!(decoded.signature, proposal.signature);
    }

    #[test]
    fn transaction_roundtrip() {
        let hash = Hash256::new([0x05; 32]);
        let data = vec![1, 2, 3, 4, 5];

        let encoded = encode_transaction(&hash, &data);
        let (dec_hash, dec_data) = decode_transaction(&encoded).unwrap();

        assert_eq!(dec_hash, hash);
        assert_eq!(dec_data, data);
    }

    #[test]
    fn status_change_roundtrip() {
        let hash = Hash256::new([0x0A; 32]);
        let seq = 42;

        let encoded = encode_status_change(&hash, seq);
        let (dec_hash, dec_seq) = decode_status_change(&encoded).unwrap();

        assert_eq!(dec_hash, hash);
        assert_eq!(dec_seq, seq);
    }

    #[test]
    fn hello_encode_decode() {
        let id = NodeIdentity::generate();
        let hash = Hash256::new([0xBB; 32]);
        let encoded = encode_hello(&id, 1, 10, &hash);
        let decoded = decode_hello(&encoded).unwrap();

        assert_eq!(decoded.network_id, 1);
        assert_eq!(decoded.ledger_seq, 10);
        assert_eq!(decoded.node_public, id.public_key_bytes());
    }

    #[test]
    fn ping_roundtrip() {
        let encoded = encode_ping(7, false);
        let decoded = decode_ping(&encoded).unwrap();
        assert_eq!(decoded.seq, 7);
        assert_eq!(decoded.r#type, 0);

        let encoded_pong = encode_ping(8, true);
        let decoded_pong = decode_ping(&encoded_pong).unwrap();
        assert_eq!(decoded_pong.seq, 8);
        assert_eq!(decoded_pong.r#type, 1);
    }

    #[test]
    fn get_ledger_roundtrip() {
        let hash = Hash256::new([0x0C; 32]);
        let encoded = encode_get_ledger(3, Some(&hash), 42, true);
        let decoded = decode_get_ledger(&encoded).unwrap();

        assert_eq!(decoded.ledger_type, 3);
        assert_eq!(decoded.ledger_seq, 42);
        assert!(decoded.request_cookie);
        assert_eq!(decoded.ledger_hash, hash.as_bytes());
    }

    #[test]
    fn get_ledger_no_hash_roundtrip() {
        let encoded = encode_get_ledger(1, None, 10, false);
        let decoded = decode_get_ledger(&encoded).unwrap();

        assert_eq!(decoded.ledger_type, 1);
        assert_eq!(decoded.ledger_seq, 10);
        assert!(!decoded.request_cookie);
        assert!(decoded.ledger_hash.is_empty());
    }

    #[test]
    fn peers_roundtrip() {
        let peers = vec![
            ("10.0.0.1".to_string(), 51235u16),
            ("192.168.1.100".to_string(), 6561),
        ];

        let encoded = encode_peers(peers.clone());
        let decoded = decode_peers(&encoded).unwrap();

        assert_eq!(decoded.len(), 2);
        assert_eq!(decoded[0], ("10.0.0.1".to_string(), 51235));
        assert_eq!(decoded[1], ("192.168.1.100".to_string(), 6561));
    }

    #[test]
    fn peers_empty_roundtrip() {
        let encoded = encode_peers(vec![]);
        let decoded = decode_peers(&encoded).unwrap();
        assert!(decoded.is_empty());
    }

    #[test]
    fn ledger_data_roundtrip() {
        let hash = Hash256::new([0x0D; 32]);
        let nodes = vec![
            (vec![1, 2, 3], vec![4, 5, 6]),
            (vec![7, 8], vec![9, 10, 11, 12]),
        ];

        let encoded = encode_ledger_data(&hash, 50, 2, nodes.clone(), 99);
        let decoded = decode_ledger_data(&encoded).unwrap();

        assert_eq!(&decoded.ledger_hash[..], hash.as_bytes());
        assert_eq!(decoded.ledger_seq, 50);
        assert_eq!(decoded.ledger_type, 2);
        assert_eq!(decoded.request_cookie, 99);
        assert_eq!(decoded.nodes.len(), 2);
        assert_eq!(decoded.nodes[0].node_id, vec![1, 2, 3]);
        assert_eq!(decoded.nodes[0].node_data, vec![4, 5, 6]);
        assert_eq!(decoded.nodes[1].node_id, vec![7, 8]);
        assert_eq!(decoded.nodes[1].node_data, vec![9, 10, 11, 12]);
    }
}
