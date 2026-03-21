use prost::Message;
use rxrpl_consensus::types::{NodeId, Proposal, Validation};
use rxrpl_p2p_proto::proto::{
    TmEndpoints, TmGetLedger, TmGetObjectByHash, TmHaveTransactionSet, TmHaveTransactions,
    TmHello, TmLedgerData, TmLedgerNode, TmManifest, TmManifests, TmPing, TmProposeSet,
    TmSquelch, TmStatusChange, TmTransaction, TmTransactions, TmValidation, TmValidatorList,
    TmValidatorListCollection,
};
use rxrpl_primitives::Hash256;

use crate::error::OverlayError;
use crate::identity::NodeIdentity;

// --- ProposeSet ---

pub fn encode_propose_set(proposal: &Proposal) -> Vec<u8> {
    let msg = TmProposeSet {
        propose_seq: Some(proposal.prop_seq),
        current_tx_hash: Some(proposal.tx_set_hash.as_bytes().to_vec()),
        node_pub_key: Some(proposal.public_key.clone()),
        close_time: Some(proposal.close_time),
        signature: Some(proposal.signature.clone().unwrap_or_default()),
        previousledger: Some(proposal.prev_ledger.as_bytes().to_vec()),
        added_transactions: Vec::new(),
        removed_transactions: Vec::new(),
    };
    msg.encode_to_vec()
}

pub fn decode_propose_set(data: &[u8]) -> Result<Proposal, OverlayError> {
    let msg = TmProposeSet::decode(data)
        .map_err(|e| OverlayError::Codec(format!("decode ProposeSet: {e}")))?;

    let pubkey_bytes = msg.node_pub_key.unwrap_or_default();
    let node_id = NodeId(rxrpl_crypto::sha512_half::sha512_half(&[&pubkey_bytes]));
    let tx_set_hash = hash256_from_bytes(&msg.current_tx_hash.unwrap_or_default())?;
    let prev_ledger = hash256_from_bytes(&msg.previousledger.unwrap_or_default())?;

    Ok(Proposal {
        node_id,
        public_key: pubkey_bytes,
        tx_set_hash,
        close_time: msg.close_time.unwrap_or(0),
        prop_seq: msg.propose_seq.unwrap_or(0),
        ledger_seq: 0,
        prev_ledger,
        signature: {
            let sig = msg.signature.unwrap_or_default();
            if sig.is_empty() {
                None
            } else {
                Some(sig)
            }
        },
    })
}

// --- Validation ---

/// Encode a validation as a rippled-compatible STObject inside TMValidation.
///
/// The STObject contains: sfFlags, sfLedgerHash, sfLedgerSequence,
/// sfSigningTime, sfSigningPubKey, sfSignature.
pub fn encode_validation(validation: &Validation, public_key: &[u8]) -> Vec<u8> {
    use crate::stobject;

    // Build the signing data (without signature) for hashing
    let mut stobj = Vec::with_capacity(256);

    // sfFlags (UINT32, field 2) -- 0x80000001 = vfFullValidation if full
    let flags: u32 = if validation.full { 0x80000001 } else { 0x00000000 };
    stobject::put_uint32(&mut stobj, 2, flags);

    // sfLedgerSequence (UINT32, field 6)
    stobject::put_uint32(&mut stobj, 6, validation.ledger_seq);

    // sfSigningTime (UINT32, field 9)
    stobject::put_uint32(&mut stobj, 9, validation.sign_time);

    // sfLedgerHash (UINT256, field 1)
    stobject::put_hash256(&mut stobj, 1, validation.ledger_hash.as_bytes());

    // sfSigningPubKey (VL, field 3)
    stobject::put_vl(&mut stobj, 3, public_key);

    // sfSignature (VL, field 6) -- must be last (notSigning field)
    if let Some(ref sig) = validation.signature {
        stobject::put_vl(&mut stobj, 6, sig);
    }

    let msg = TmValidation {
        validation: Some(stobj),
    };
    msg.encode_to_vec()
}

/// Decode a validation from rippled STObject format.
pub fn decode_validation(data: &[u8]) -> Result<Validation, OverlayError> {
    let msg = TmValidation::decode(data)
        .map_err(|e| OverlayError::Codec(format!("decode Validation: {e}")))?;

    let payload = msg.validation.unwrap_or_default();

    // Parse STObject fields
    let mut ledger_hash = Hash256::ZERO;
    let mut ledger_seq = 0u32;
    let mut sign_time = 0u32;
    let mut full = false;
    let mut signature: Option<Vec<u8>> = None;
    let mut signing_pub_key: Vec<u8> = Vec::new();

    let mut pos = 0;
    while pos < payload.len() {
        let (type_id, field_id, hdr_len) =
            crate::stobject::decode_field_id(&payload[pos..])
                .ok_or_else(|| OverlayError::Codec("invalid STObject field header".into()))?;
        pos += hdr_len;

        match (type_id, field_id) {
            // UINT32 fields
            (2, 2) => {
                // sfFlags
                if pos + 4 > payload.len() { break; }
                let v = u32::from_be_bytes(payload[pos..pos + 4].try_into().unwrap());
                full = (v & 0x80000001) == 0x80000001;
                pos += 4;
            }
            (2, fid) => {
                // Other UINT32
                if pos + 4 > payload.len() { break; }
                let v = u32::from_be_bytes(payload[pos..pos + 4].try_into().unwrap());
                match fid {
                    6 => ledger_seq = v,  // sfLedgerSequence
                    9 => sign_time = v,   // sfSigningTime
                    _ => {}
                }
                pos += 4;
            }
            // UINT64 fields
            (3, _) => {
                if pos + 8 > payload.len() { break; }
                pos += 8;
            }
            // UINT256 fields
            (5, fid) => {
                if pos + 32 > payload.len() { break; }
                if fid == 1 {
                    // sfLedgerHash
                    ledger_hash = hash256_from_bytes(&payload[pos..pos + 32])?;
                }
                pos += 32;
            }
            // VL fields
            (7, fid) => {
                let (vl_len, vl_hdr) =
                    crate::stobject::decode_vl_length(&payload[pos..])
                        .ok_or_else(|| OverlayError::Codec("invalid VL length".into()))?;
                pos += vl_hdr;
                if pos + vl_len > payload.len() { break; }
                match fid {
                    3 => signing_pub_key = payload[pos..pos + vl_len].to_vec(),
                    6 => signature = Some(payload[pos..pos + vl_len].to_vec()),
                    _ => {}
                }
                pos += vl_len;
            }
            // Skip unknown types
            _ => break,
        }
    }

    let node_id = if !signing_pub_key.is_empty() {
        NodeId(rxrpl_crypto::sha512_half::sha512_half(&[&signing_pub_key]))
    } else {
        NodeId(Hash256::ZERO)
    };

    Ok(Validation {
        node_id,
        ledger_hash,
        ledger_seq,
        full,
        close_time: sign_time,
        sign_time,
        signature,
    })
}

// --- Transaction ---

pub fn encode_transaction(_tx_hash: &Hash256, tx_data: &[u8]) -> Vec<u8> {
    // rippled-compatible: raw_transaction contains the serialized tx directly.
    let msg = TmTransaction {
        raw_transaction: Some(tx_data.to_vec()),
        status: Some(0),
        receive_timestamp: Some(0),
        deferred: Some(false),
    };
    msg.encode_to_vec()
}

pub fn decode_transaction(data: &[u8]) -> Result<(Hash256, Vec<u8>), OverlayError> {
    let msg = TmTransaction::decode(data)
        .map_err(|e| OverlayError::Codec(format!("decode Transaction: {e}")))?;

    let raw_transaction = msg.raw_transaction.unwrap_or_default();
    if raw_transaction.is_empty() {
        return Err(OverlayError::Codec("empty transaction payload".into()));
    }

    // rippled sends the serialized transaction directly in raw_transaction.
    // Compute the hash: SHA-512-Half(HashPrefix::TRANSACTION_ID || raw_tx)
    let prefix = rxrpl_crypto::hash_prefix::HashPrefix::TRANSACTION_ID.to_bytes();
    let mut hash_input = prefix.to_vec();
    hash_input.extend_from_slice(&raw_transaction);
    let tx_hash = rxrpl_crypto::sha512_half::sha512_half(&[&hash_input]);

    Ok((tx_hash, raw_transaction))
}

// --- StatusChange ---

pub fn encode_status_change(ledger_hash: &Hash256, ledger_seq: u32) -> Vec<u8> {
    let msg = TmStatusChange {
        new_status: Some(0),
        new_event: Some(0),
        ledger_seq: Some(ledger_seq),
        ledger_hash: Some(ledger_hash.as_bytes().to_vec()),
        ledger_hash_previous: None,
        network_time: Some(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        ),
        first_seq: None,
        last_seq: None,
    };
    msg.encode_to_vec()
}

pub fn decode_status_change(data: &[u8]) -> Result<(Hash256, u32), OverlayError> {
    let msg = TmStatusChange::decode(data)
        .map_err(|e| OverlayError::Codec(format!("decode StatusChange: {e}")))?;
    let ledger_hash = hash256_from_bytes(&msg.ledger_hash.unwrap_or_default())?;
    Ok((ledger_hash, msg.ledger_seq.unwrap_or(0)))
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
    proof_data.extend_from_slice(b"XRPL-HANDSHAKE");
    let proof_hash = rxrpl_crypto::sha512_half::sha512_half(&[&proof_data]);
    let node_proof = identity.sign(proof_hash.as_bytes());

    let network_time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let msg = TmHello {
        proto_version: 2,
        proto_version_min: 2,
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
        r#type: Some(if is_pong { 1 } else { 0 }),
        seq: Some(seq),
        ping_time: Some(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        ),
        net_time: Some(0),
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
    request_cookie: u64,
) -> Vec<u8> {
    encode_get_ledger_with_nodes(ledger_type, hash, seq, request_cookie, Vec::new())
}

/// Encode a GetLedger request with specific node hashes for delta sync.
///
/// When `node_ids` is non-empty, the peer should return the raw node data
/// for each requested hash instead of all leaf nodes.
pub fn encode_get_ledger_with_nodes(
    ledger_type: i32,
    hash: Option<&Hash256>,
    seq: u32,
    request_cookie: u64,
    node_ids: Vec<Vec<u8>>,
) -> Vec<u8> {
    let msg = TmGetLedger {
        itype: Some(ledger_type),
        ltype: None,
        ledger_hash: Some(hash.map(|h| h.as_bytes().to_vec()).unwrap_or_default()),
        ledger_seq: Some(seq),
        node_ids,
        request_cookie: Some(request_cookie),
        query_type: None,
        query_depth: Some(0),
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
        ledger_hash: Some(hash.as_bytes().to_vec()),
        ledger_seq: Some(seq),
        ledger_info_type: Some(ltype),
        nodes: nodes
            .into_iter()
            .map(|(id, data)| TmLedgerNode {
                nodeid: Some(id),
                nodedata: Some(data),
            })
            .collect(),
        request_cookie: Some(cookie),
        error: None,
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
                ip: Some(ip),
                port: Some(port as u32),
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
        .map(|p| (p.ip.unwrap_or_default(), p.port.unwrap_or(0) as u16))
        .collect())
}

// --- Manifest ---

/// Decoded manifest fields -- raw stobject bytes from TMManifest.
pub struct ManifestData {
    pub raw: Vec<u8>,
}

pub fn decode_manifest(data: &[u8]) -> Result<ManifestData, OverlayError> {
    let msg = TmManifest::decode(data)
        .map_err(|e| OverlayError::Codec(format!("decode Manifest: {e}")))?;

    Ok(ManifestData {
        raw: msg.stobject.unwrap_or_default(),
    })
}

// --- Manifests (batch, type 2) ---

pub fn decode_manifests(data: &[u8]) -> Result<Vec<TmManifest>, OverlayError> {
    let msg = TmManifests::decode(data)
        .map_err(|e| OverlayError::Codec(format!("decode Manifests: {e}")))?;
    Ok(msg.list)
}

pub fn encode_manifests(manifests: Vec<Vec<u8>>) -> Vec<u8> {
    let msg = TmManifests {
        list: manifests
            .into_iter()
            .map(|raw| TmManifest {
                stobject: Some(raw),
            })
            .collect(),
    };
    msg.encode_to_vec()
}

// --- Endpoints (type 15) ---

pub fn decode_endpoints(data: &[u8]) -> Result<Vec<(String, u32)>, OverlayError> {
    let msg = TmEndpoints::decode(data)
        .map_err(|e| OverlayError::Codec(format!("decode Endpoints: {e}")))?;
    Ok(msg
        .endpoints_v2
        .into_iter()
        .map(|ep| (ep.endpoint.unwrap_or_default(), ep.hops.unwrap_or(0)))
        .collect())
}

// --- HaveTransactionSet (type 35) ---

pub struct HaveSetData {
    pub status: u32,
    pub hash: Hash256,
}

pub fn decode_have_set(data: &[u8]) -> Result<HaveSetData, OverlayError> {
    let msg = TmHaveTransactionSet::decode(data)
        .map_err(|e| OverlayError::Codec(format!("decode HaveSet: {e}")))?;
    let hash = hash256_from_bytes(&msg.hash.unwrap_or_default())?;
    Ok(HaveSetData {
        status: msg.status.unwrap_or(0) as u32,
        hash,
    })
}

// --- GetObjectByHash (type 42) ---

pub fn decode_get_objects(data: &[u8]) -> Result<TmGetObjectByHash, OverlayError> {
    TmGetObjectByHash::decode(data)
        .map_err(|e| OverlayError::Codec(format!("decode GetObjects: {e}")))
}

// --- Squelch (type 55) ---

pub fn decode_squelch(data: &[u8]) -> Result<TmSquelch, OverlayError> {
    TmSquelch::decode(data).map_err(|e| OverlayError::Codec(format!("decode Squelch: {e}")))
}

// --- ValidatorList (type 54) ---

pub fn decode_validator_list(data: &[u8]) -> Result<TmValidatorList, OverlayError> {
    TmValidatorList::decode(data)
        .map_err(|e| OverlayError::Codec(format!("decode ValidatorList: {e}")))
}

// --- ValidatorListCollection (type 56) ---

pub fn decode_validator_list_collection(
    data: &[u8],
) -> Result<TmValidatorListCollection, OverlayError> {
    TmValidatorListCollection::decode(data)
        .map_err(|e| OverlayError::Codec(format!("decode ValidatorListCollection: {e}")))
}

// --- HaveTransactions (type 63) ---

pub fn decode_have_transactions(data: &[u8]) -> Result<Vec<Vec<u8>>, OverlayError> {
    let msg = TmHaveTransactions::decode(data)
        .map_err(|e| OverlayError::Codec(format!("decode HaveTransactions: {e}")))?;
    Ok(msg.hashes)
}

// --- Transactions (batch, type 64) ---

pub fn decode_transactions(data: &[u8]) -> Result<TmTransactions, OverlayError> {
    TmTransactions::decode(data)
        .map_err(|e| OverlayError::Codec(format!("decode Transactions: {e}")))
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
        let pubkey = vec![0x02; 33];
        let node_id = NodeId(rxrpl_crypto::sha512_half::sha512_half(&[&pubkey]));
        let proposal = Proposal {
            node_id,
            public_key: pubkey,
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
        assert_eq!(decoded.public_key, proposal.public_key);
        assert_eq!(decoded.tx_set_hash, proposal.tx_set_hash);
        assert_eq!(decoded.close_time, proposal.close_time);
        assert_eq!(decoded.prop_seq, proposal.prop_seq);
        assert_eq!(decoded.ledger_seq, 0); // ledger_seq removed from proto
        assert_eq!(decoded.prev_ledger, proposal.prev_ledger);
        assert_eq!(decoded.signature, proposal.signature);
    }

    #[test]
    fn transaction_roundtrip() {
        let data = vec![1, 2, 3, 4, 5];

        // Compute expected hash: SHA-512-Half(TRANSACTION_ID_PREFIX || data)
        let prefix = rxrpl_crypto::hash_prefix::HashPrefix::TRANSACTION_ID.to_bytes();
        let mut hash_input = prefix.to_vec();
        hash_input.extend_from_slice(&data);
        let expected_hash = rxrpl_crypto::sha512_half::sha512_half(&[&hash_input]);

        let encoded = encode_transaction(&expected_hash, &data);
        let (dec_hash, dec_data) = decode_transaction(&encoded).unwrap();

        assert_eq!(dec_hash, expected_hash);
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
        assert_eq!(decoded.seq.unwrap_or(0), 7);
        assert_eq!(decoded.r#type.unwrap_or(0), 0);

        let encoded_pong = encode_ping(8, true);
        let decoded_pong = decode_ping(&encoded_pong).unwrap();
        assert_eq!(decoded_pong.seq.unwrap_or(0), 8);
        assert_eq!(decoded_pong.r#type.unwrap_or(0), 1);
    }

    #[test]
    fn get_ledger_roundtrip() {
        let hash = Hash256::new([0x0C; 32]);
        let encoded = encode_get_ledger(3, Some(&hash), 42, 1);
        let decoded = decode_get_ledger(&encoded).unwrap();

        assert_eq!(decoded.itype.unwrap_or(0), 3);
        assert_eq!(decoded.ledger_seq.unwrap_or(0), 42);
        assert_eq!(decoded.request_cookie.unwrap_or(0), 1);
        assert_eq!(decoded.ledger_hash.unwrap_or_default(), hash.as_bytes());
    }

    #[test]
    fn get_ledger_no_hash_roundtrip() {
        let encoded = encode_get_ledger(1, None, 10, 0);
        let decoded = decode_get_ledger(&encoded).unwrap();

        assert_eq!(decoded.itype.unwrap_or(0), 1);
        assert_eq!(decoded.ledger_seq.unwrap_or(0), 10);
        assert_eq!(decoded.request_cookie.unwrap_or(0), 0);
        assert!(decoded.ledger_hash.as_ref().map_or(true, |v| v.is_empty()));
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

        assert_eq!(decoded.ledger_hash.unwrap_or_default(), hash.as_bytes());
        assert_eq!(decoded.ledger_seq.unwrap_or(0), 50);
        assert_eq!(decoded.ledger_info_type.unwrap_or(0), 2);
        assert_eq!(decoded.request_cookie.unwrap_or(0), 99);
        assert_eq!(decoded.nodes.len(), 2);
        assert_eq!(decoded.nodes[0].nodeid.as_deref().unwrap_or(&[]), &[1, 2, 3]);
        assert_eq!(decoded.nodes[0].nodedata.as_deref().unwrap_or(&[]), &[4, 5, 6]);
        assert_eq!(decoded.nodes[1].nodeid.as_deref().unwrap_or(&[]), &[7, 8]);
        assert_eq!(decoded.nodes[1].nodedata.as_deref().unwrap_or(&[]), &[9, 10, 11, 12]);
    }
}
