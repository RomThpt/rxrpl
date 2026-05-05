use prost::Message;
use rxrpl_consensus::types::{NodeId, Proposal, Validation};
use rxrpl_p2p_proto::proto::{
    TmCluster, TmClusterNode, TmEndpoints, TmGetLedger, TmGetObjectByHash, TmHaveTransactionSet,
    TmHaveTransactions, TmHello, TmLedgerData, TmLedgerNode, TmManifest, TmManifests, TmPing,
    TmProposeSet, TmSquelch, TmStatusChange, TmTransaction, TmTransactions, TmValidation,
    TmValidatorList, TmValidatorListCollection,
};
use rxrpl_primitives::Hash256;

use crate::error::OverlayError;
use crate::identity::NodeIdentity;

/// Maximum permitted size of an STValidation payload (the inner STObject
/// carried in `TMValidation.validation`).
///
/// rippled's `STValidation` is a small, fixed-shape STObject — every
/// field defined in the SOTemplate (flags, ledger seq, hashes, fees,
/// optional `sfAmendments` vector, signatures, etc.) fits comfortably
/// within a few hundred bytes; the upper bound on the wire is dominated
/// by `sfAmendments` (a Vector256 of amendment hashes) plus the two
/// signatures. Even with every optional field populated the encoded size
/// stays well under 32 KiB, so we use that as a hard ceiling. This bound
/// matches the conservative cap rippled itself enforces on validation
/// payloads in `OverlayImpl::onMessage(TMValidation)` and exists to
/// prevent peer-controlled memory amplification: without a cap, the
/// `Vec::with_capacity(payload.len())` allocation in `decode_validation`
/// would honour any `payload.len()` a peer claims, allowing a single
/// crafted message to coerce the node into a multi-MiB allocation per
/// peer per validation. See audit pass 1 finding H9.
pub const MAX_STVALIDATION_BYTES: usize = 32 * 1024;

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
            if sig.is_empty() { None } else { Some(sig) }
        },
    })
}

// --- Validation ---

/// Encode a validation as a rippled-compatible STObject inside TMValidation.
///
/// The strip-result produced by [`crate::identity::NodeIdentity::sign_validation`]
/// is laid out in canonical `(type_code, field_code)` ascending order — the
/// only fields it omits are `sfSignature (7,6)` and `sfMasterSignature (7,18)`.
/// To produce the on-wire STObject we have to splice `sfSignature` back into
/// its canonical position. With the rxrpl-supported SOTemplate that means
/// inserting it BEFORE `sfAmendments (19,3)` when the latter is present, and
/// appending it at the end otherwise.
///
/// This matches goXRPL's `SerializeSTValidation` (the goXRPLd reference
/// implementation that interoperates with rippled) and rippled's own
/// `STObject` serializer, which sorts fields by `fieldCode` ascending.
/// Emitting `sfSignature` after `sfAmendments` produces a non-canonical byte
/// image — rippled's deduplication, signature verification, and trace logs
/// still see "valid bytes" but the suppression hash diverges from what the
/// network expects, so the validation is treated as a stray packet and never
/// fed into the trusted-validator aggregator.
///
/// When `signing_payload` is `None` (legacy / locally-built validations that
/// haven't been re-signed via T09), we fall back to the pre-T09 5-field
/// encoding followed by `sfSignature`. That path has no `sfAmendments` so the
/// canonical-order question is moot.
pub fn encode_validation(validation: &Validation, public_key: &[u8]) -> Vec<u8> {
    use crate::stobject;

    let mut stobj = Vec::with_capacity(256);

    if let Some(stripped) = validation.signing_payload.as_ref() {
        // The strip-result is already canonically sorted. Insert sfSignature
        // at its canonical position by splitting at the first field whose
        // (type<<16)|field key is greater than sfSignature's key (0x70006).
        // For the STValidation SOTemplate the only such field is sfAmendments
        // (key 0x130003), but we walk generically so any future field added
        // to the SOTemplate behind sfSignature is handled correctly without
        // an additional code change.
        let split = canonical_signature_insert_offset(stripped);
        stobj.extend_from_slice(&stripped[..split]);
        if let Some(ref sig) = validation.signature {
            stobject::put_vl(&mut stobj, 6, sig);
        }
        stobj.extend_from_slice(&stripped[split..]);
    } else {
        // Legacy 5-field fallback. Matches the pre-T09 byte image.
        let flags: u32 = if validation.full {
            0x80000001
        } else {
            0x00000000
        };
        stobject::put_uint32(&mut stobj, 2, flags);
        stobject::put_uint32(&mut stobj, 6, validation.ledger_seq);
        stobject::put_uint32(&mut stobj, 9, validation.sign_time);
        stobject::put_hash256(&mut stobj, 1, validation.ledger_hash.as_bytes());
        stobject::put_vl(&mut stobj, 3, public_key);
        if let Some(ref sig) = validation.signature {
            stobject::put_vl(&mut stobj, 6, sig);
        }
    }

    let msg = TmValidation {
        validation: Some(stobj),
    };
    msg.encode_to_vec()
}

/// Walk the canonical strip-result and return the byte offset at which
/// `sfSignature (7,6)` should be inserted so the resulting STObject stays in
/// `(type_code, field_code)` ascending order.
///
/// Returns `stripped.len()` if every field already encoded sorts before
/// `sfSignature` (the common case when no `sfAmendments` are voted on).
///
/// Robust against mid-buffer parse failures: if a field header looks
/// malformed we conservatively return the current offset, matching the
/// pre-fix "append at end" behaviour for that field.
fn canonical_signature_insert_offset(stripped: &[u8]) -> usize {
    use crate::stobject;

    // sfSignature canonical sort key.
    const SIG_KEY: u32 = (7u32 << 16) | 6;

    let mut pos = 0usize;
    while pos < stripped.len() {
        let field_start = pos;
        let Some((type_id, field_id, hdr_len)) = stobject::decode_field_id(&stripped[pos..]) else {
            return field_start;
        };
        let key = ((type_id as u32) << 16) | field_id as u32;
        if key > SIG_KEY {
            return field_start;
        }
        pos += hdr_len;

        // Skip the value. We only need to recognise the type IDs that
        // sign_validation can emit before sfAmendments, which are exactly
        // UINT32(2), UINT64(3), UINT256(5), AMOUNT(6) and Blob/VL(7).
        let consumed = match type_id {
            2 => stobject::decode_uint32(&stripped[pos..]).map(|(_, n)| n),
            3 => stobject::decode_uint64(&stripped[pos..]).map(|(_, n)| n),
            5 => stobject::decode_hash256(&stripped[pos..]).map(|(_, n)| n),
            6 => stobject::decode_amount_xrp(&stripped[pos..]).map(|(_, n)| n),
            7 => stobject::decode_vl(&stripped[pos..]).map(|(_, n)| n),
            // Vector256 is the only other type the encoder produces; if we
            // see one its key (>= 0x130000) is already > SIG_KEY so the
            // early `return field_start` above caught it.
            _ => return field_start,
        };
        let Some(consumed) = consumed else {
            return field_start;
        };
        pos += consumed;
    }
    stripped.len()
}

/// Decode a validation from rippled STObject format.
///
/// Walks the STObject byte stream, extracting every recognised SOTemplate
/// field defined for STValidation, and reconstructs the canonical signing
/// payload (the strip-result that excludes only `sfSignature` (7,6) and
/// `sfMasterSignature` (7,18)). The strip-result is stashed in
/// `Validation::signing_payload` so that
/// [`crate::identity::verify_validation_signature`] can replay the exact
/// byte sequence rippled signed over — without it, verification of any
/// validation that carries an optional field unknown to the local schema
/// would fail.
///
/// Field map (canonical, as encoded by rippled and by T09's encoder):
/// - `(2,2)` sfFlags          → derives `full = (flags & 0x80000001) == 0x80000001`
/// - `(2,6)` sfLedgerSequence → `ledger_seq`
/// - `(2,7)` sfCloseTime      → `close_time`
/// - `(2,9)` sfSigningTime    → `sign_time`
/// - `(2,24)` sfLoadFee       → `load_fee`
/// - `(2,31)` sfReserveBase   → `reserve_base`
/// - `(2,32)` sfReserveIncrement → `reserve_increment`
/// - `(3,5)` sfBaseFee        → `base_fee`
/// - `(3,10)` sfCookie        → `cookie`
/// - `(3,11)` sfServerVersion → `server_version`
/// - `(5,1)` sfLedgerHash     → `ledger_hash`
/// - `(5,23)` sfConsensusHash → `consensus_hash`
/// - `(5,25)` sfValidatedHash → `validated_hash`
/// - `(6,22)` sfBaseFeeDrops  → `base_fee_drops`
/// - `(6,23)` sfReserveBaseDrops → `reserve_base_drops`
/// - `(6,24)` sfReserveIncrementDrops → `reserve_increment_drops`
/// - `(7,3)` sfSigningPubKey  → `public_key`
/// - `(7,6)` sfSignature      → `signature` (excluded from signing_payload)
/// - `(7,18)` sfMasterSignature → ignored, but still excluded from signing_payload
/// - `(19,3)` sfAmendments    → `amendments`
pub fn decode_validation(data: &[u8]) -> Result<Validation, OverlayError> {
    use crate::stobject;

    let msg = TmValidation::decode(data)
        .map_err(|e| OverlayError::Codec(format!("decode Validation: {e}")))?;

    let payload = msg.validation.unwrap_or_default();

    // H9: reject grossly oversized payloads up front (peer-controlled).
    // We use a factor-of-2 leniency over MAX_STVALIDATION_BYTES so that
    // any borderline-but-legitimate payload still parses; anything beyond
    // that is malformed by definition and we refuse to read further.
    if payload.len() > MAX_STVALIDATION_BYTES * 2 {
        return Err(OverlayError::Codec(format!(
            "STValidation payload too large: {} bytes (max {})",
            payload.len(),
            MAX_STVALIDATION_BYTES * 2
        )));
    }

    let mut validation = Validation::default();
    let mut signing_pub_key: Vec<u8> = Vec::new();
    // H9: cap the initial allocation at MAX_STVALIDATION_BYTES even if
    // `payload.len()` is large, so a peer cannot coerce a multi-MiB
    // allocation through a crafted (but not yet rejected above) payload.
    // The Vec will grow on demand if a legitimate payload happens to
    // exceed the soft cap — this only bounds the *initial* reservation.
    let mut signing_payload = Vec::with_capacity(payload.len().min(MAX_STVALIDATION_BYTES));
    // Track whether sfCloseTime (2,7) was actually present on the wire,
    // so we can distinguish "field absent" (legacy fallback to sign_time)
    // from "field present and explicitly zero" (the rippled
    // "no opinion on close time" sentinel that engine::eff_close_time
    // pattern-matches on). See H13.
    let mut seen_close_time = false;
    // Track the canonical sort key of the previous field so we can enforce
    // strict `(type_code << 16) | field_code` ascending order. This mirrors
    // rippled's `STObject::checkSorting` (src/libxrpl/protocol/STObject.cpp),
    // which rejects both duplicate fields and out-of-order fields. Without
    // this check a peer could craft a payload where, e.g., sfLedgerHash
    // appears twice with different values: the local decoder would accept
    // the latter while the suppression-hash logic on rippled would see the
    // first, splitting the network's view of the validation. See audit
    // pass-2 H12.
    let mut last_key: Option<u32> = None;

    let mut pos = 0;
    while pos < payload.len() {
        let field_start = pos;
        let (type_id, field_id, hdr_len) = stobject::decode_field_id(&payload[pos..])
            .ok_or_else(|| OverlayError::Codec("invalid STObject field header".into()))?;
        let key = ((type_id as u32) << 16) | field_id as u32;
        if let Some(prev) = last_key {
            if key <= prev {
                return Err(OverlayError::Codec(
                    "non-canonical STObject ordering".into(),
                ));
            }
        }
        last_key = Some(key);
        pos += hdr_len;

        // Decode the value and advance `pos` past it. We dispatch on
        // type_id and use the matching stobject helper. The total
        // bytes consumed for this field (header + value, including any
        // VL length prefix) is `value_end - field_start` — that's the
        // span we copy verbatim into the signing buffer for
        // non-signature fields.
        let value_end: usize = match type_id {
            // UINT32
            2 => {
                let (v, consumed) = stobject::decode_uint32(&payload[pos..])
                    .ok_or_else(|| OverlayError::Codec("truncated UINT32".into()))?;
                match field_id {
                    2 => validation.full = (v & 0x80000001) == 0x80000001,
                    6 => validation.ledger_seq = v,
                    7 => {
                        validation.close_time = v;
                        seen_close_time = true;
                    }
                    9 => validation.sign_time = v,
                    24 => validation.load_fee = Some(v),
                    31 => validation.reserve_base = Some(v),
                    32 => validation.reserve_increment = Some(v),
                    _ => {}
                }
                pos + consumed
            }
            // UINT64
            3 => {
                let (v, consumed) = stobject::decode_uint64(&payload[pos..])
                    .ok_or_else(|| OverlayError::Codec("truncated UINT64".into()))?;
                match field_id {
                    5 => validation.base_fee = Some(v),
                    10 => validation.cookie = Some(v),
                    11 => validation.server_version = Some(v),
                    _ => {}
                }
                pos + consumed
            }
            // UINT256
            5 => {
                let (h, consumed) = stobject::decode_hash256(&payload[pos..])
                    .ok_or_else(|| OverlayError::Codec("truncated UINT256".into()))?;
                let hash = Hash256::new(h);
                match field_id {
                    1 => validation.ledger_hash = hash,
                    23 => validation.consensus_hash = Some(hash),
                    25 => validation.validated_hash = Some(hash),
                    _ => {}
                }
                pos + consumed
            }
            // AMOUNT — only XRP-native amounts are valid in STValidation.
            6 => {
                let (drops, consumed) =
                    stobject::decode_amount_xrp(&payload[pos..]).ok_or_else(|| {
                        OverlayError::Codec("invalid Amount (non-XRP in STValidation)".into())
                    })?;
                match field_id {
                    22 => validation.base_fee_drops = Some(drops),
                    23 => validation.reserve_base_drops = Some(drops),
                    24 => validation.reserve_increment_drops = Some(drops),
                    _ => {}
                }
                pos + consumed
            }
            // VL (Blob): sfSigningPubKey, sfSignature, sfMasterSignature.
            7 => {
                let (bytes, consumed) = stobject::decode_vl(&payload[pos..])
                    .ok_or_else(|| OverlayError::Codec("invalid VL Blob".into()))?;
                match field_id {
                    3 => signing_pub_key = bytes,
                    6 => validation.signature = Some(bytes),
                    // 18 (sfMasterSignature) is intentionally not stored
                    // by `Validation`; we still consume it and exclude
                    // it from the signing payload below.
                    _ => {}
                }
                pos + consumed
            }
            // VECTOR256 (Amendments).
            19 => {
                let (entries, consumed) = stobject::decode_vector256(&payload[pos..])
                    .ok_or_else(|| OverlayError::Codec("invalid Vector256".into()))?;
                if field_id == 3 {
                    validation.amendments = entries.into_iter().map(Hash256::new).collect();
                }
                pos + consumed
            }
            // Unknown type — we cannot determine its length, so we cannot
            // continue parsing safely. Stop and keep what we already have.
            _ => {
                tracing::debug!(
                    "decode_validation: unknown STObject type_id={} field_id={}, stopping parse",
                    type_id,
                    field_id
                );
                break;
            }
        };

        // Append this field's full bytes (header + VL prefix + value) to
        // the signing payload UNLESS it's sfSignature (7,6) or
        // sfMasterSignature (7,18). This must match T09's
        // `sign_validation` strip-rule byte-for-byte so that the verifier
        // can replay the signing buffer.
        let is_signature_field = matches!((type_id, field_id), (7, 6) | (7, 18));
        if !is_signature_field {
            signing_payload.extend_from_slice(&payload[field_start..value_end]);
        }

        pos = value_end;
    }

    let node_id = if !signing_pub_key.is_empty() {
        NodeId(rxrpl_crypto::sha512_half::sha512_half(&[&signing_pub_key]))
    } else {
        NodeId(Hash256::ZERO)
    };
    validation.node_id = node_id;
    validation.public_key = signing_pub_key;
    validation.signing_payload = Some(signing_payload);
    // Fall back to `sign_time` ONLY when sfCloseTime was absent on the
    // wire. A field that was present-but-zero is the rippled "no
    // opinion on close time" sentinel: it MUST be preserved verbatim so
    // that downstream consensus code (engine::eff_close_time, peer
    // bucketing) can pattern-match on the zero value rather than seeing
    // a manufactured close_time copied from sign_time. See H13.
    if !seen_close_time {
        validation.close_time = validation.sign_time;
    }

    Ok(validation)
}

// --- Transaction ---

pub fn encode_transaction(_tx_hash: &Hash256, tx_data: &[u8]) -> Vec<u8> {
    // Rippled's `proto2` schema declares `status` as REQUIRED (TransactionStatus
    // enum). prost's `optional int32` with `Some(0)` is silently dropped on the
    // wire because 0 is the default for `int32`, so rippled rejects the
    // message with "missing required fields: status" and disconnects.
    // tsNEW = 1 (rippled's TransactionStatus enum) is the correct value for a
    // freshly-broadcast tx and is non-zero, so it gets serialized.
    const TS_NEW: i32 = 1;
    let msg = TmTransaction {
        raw_transaction: Some(tx_data.to_vec()),
        status: Some(TS_NEW),
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
    let has_nodes = !node_ids.is_empty();
    let msg = TmGetLedger {
        itype: ledger_type,
        ltype: None,
        ledger_hash: hash.map(|h| h.as_bytes().to_vec()),
        ledger_seq: if seq > 0 { Some(seq) } else { None },
        node_ids,
        request_cookie: if request_cookie > 0 {
            Some(request_cookie)
        } else {
            None
        },
        query_type: None,
        query_depth: if has_nodes { Some(2) } else { None },
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
    cookie: u64,
) -> Vec<u8> {
    let msg = TmLedgerData {
        ledger_hash: hash.as_bytes().to_vec(),
        ledger_seq: seq,
        ledger_info_type: ltype,
        nodes: nodes
            .into_iter()
            .map(|(id, data)| TmLedgerNode {
                nodeid: Some(id),
                nodedata: Some(data),
            })
            .collect(),
        request_cookie: Some(cookie as u32),
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

pub fn encode_have_set(hash: &Hash256, status: u32) -> Vec<u8> {
    let msg = TmHaveTransactionSet {
        status: Some(status as i32),
        hash: Some(hash.as_bytes().to_vec()),
    };
    msg.encode_to_vec()
}

// --- GetObjectByHash (type 42) ---

/// rippled ObjectType enum values for TMGetObjectByHash.type.
/// otLEDGER_NODE = 3: request SHAMap tree nodes by content hash.
const OT_LEDGER_NODE: i32 = 3;

/// Encode a TMGetObjectByHash request to fetch SHAMap nodes by content hash.
///
/// This is used as a fallback when tree-based incremental sync gets stuck:
/// instead of requesting nodes by their SHAMapNodeID position in the tree,
/// we request them directly by their content hash (SHA-512-Half of the
/// serialized node data).
pub fn encode_get_objects_by_hash(
    ledger_hash: &Hash256,
    ledger_seq: u32,
    content_hashes: &[Hash256],
    fat: bool,
) -> Vec<u8> {
    use rxrpl_p2p_proto::proto::TmIndexedObject;

    let objects: Vec<TmIndexedObject> = content_hashes
        .iter()
        .map(|h| TmIndexedObject {
            hash: Some(h.as_bytes().to_vec()),
            node_id: None,
            index: None,
            data: None,
            ledger_seq: Some(ledger_seq),
        })
        .collect();

    let msg = TmGetObjectByHash {
        r#type: Some(OT_LEDGER_NODE),
        query: Some(true),
        seq: Some(ledger_seq),
        ledger_hash: Some(ledger_hash.as_bytes().to_vec()),
        fat: Some(fat),
        objects,
    };
    msg.encode_to_vec()
}

/// Encode a TMGetObjectByHash response containing found objects.
///
/// This builds a response message (query=false) with the requested objects
/// populated with their data from the local node store.
pub fn encode_get_objects_response(
    object_type: i32,
    ledger_seq: u32,
    ledger_hash: Option<&Hash256>,
    objects: Vec<(Hash256, Vec<u8>)>,
) -> Vec<u8> {
    use rxrpl_p2p_proto::proto::TmIndexedObject;

    let indexed: Vec<TmIndexedObject> = objects
        .into_iter()
        .map(|(hash, data)| TmIndexedObject {
            hash: Some(hash.as_bytes().to_vec()),
            node_id: None,
            index: None,
            data: Some(data),
            ledger_seq: Some(ledger_seq),
        })
        .collect();

    let msg = TmGetObjectByHash {
        r#type: Some(object_type),
        query: Some(false),
        seq: Some(ledger_seq),
        ledger_hash: ledger_hash.map(|h| h.as_bytes().to_vec()),
        fat: Some(false),
        objects: indexed,
    };
    msg.encode_to_vec()
}

pub fn decode_get_objects(data: &[u8]) -> Result<TmGetObjectByHash, OverlayError> {
    TmGetObjectByHash::decode(data)
        .map_err(|e| OverlayError::Codec(format!("decode GetObjects: {e}")))
}

// --- Squelch (type 55) ---

pub fn decode_squelch(data: &[u8]) -> Result<TmSquelch, OverlayError> {
    TmSquelch::decode(data).map_err(|e| OverlayError::Codec(format!("decode Squelch: {e}")))
}

pub fn encode_squelch(validator_pub_key: &[u8], squelch: bool, duration_secs: u32) -> Vec<u8> {
    let msg = TmSquelch {
        squelch: Some(squelch),
        validator_pub_key: Some(validator_pub_key.to_vec()),
        squelch_duration: Some(duration_secs),
    };
    msg.encode_to_vec()
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

// --- Cluster (type 5) ---

/// Decoded cluster node entry.
pub struct ClusterNodeData {
    pub public_key: String,
    pub report_time: u32,
    pub node_load: u32,
    pub node_name: String,
    pub address: String,
}

pub fn encode_cluster(nodes: &[ClusterNodeData]) -> Vec<u8> {
    let msg = TmCluster {
        cluster_nodes: nodes
            .iter()
            .map(|n| TmClusterNode {
                public_key: Some(n.public_key.clone()),
                report_time: Some(n.report_time),
                node_load: Some(n.node_load),
                node_name: Some(n.node_name.clone()),
                address: Some(n.address.clone()),
            })
            .collect(),
    };
    msg.encode_to_vec()
}

pub fn decode_cluster(data: &[u8]) -> Result<Vec<ClusterNodeData>, OverlayError> {
    let msg =
        TmCluster::decode(data).map_err(|e| OverlayError::Codec(format!("decode Cluster: {e}")))?;
    Ok(msg
        .cluster_nodes
        .into_iter()
        .map(|n| ClusterNodeData {
            public_key: n.public_key.unwrap_or_default(),
            report_time: n.report_time.unwrap_or(0),
            node_load: n.node_load.unwrap_or(0),
            node_name: n.node_name.unwrap_or_default(),
            address: n.address.unwrap_or_default(),
        })
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

        assert_eq!(decoded.itype, 3);
        assert_eq!(decoded.ledger_seq.unwrap_or(0), 42);
        assert_eq!(decoded.request_cookie.unwrap_or(0), 1);
        assert_eq!(decoded.ledger_hash.unwrap_or_default(), hash.as_bytes());
    }

    #[test]
    fn get_ledger_no_hash_roundtrip() {
        let encoded = encode_get_ledger(1, None, 10, 0);
        let decoded = decode_get_ledger(&encoded).unwrap();

        assert_eq!(decoded.itype, 1);
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

        assert_eq!(decoded.ledger_hash, hash.as_bytes());
        assert_eq!(decoded.ledger_seq, 50);
        assert_eq!(decoded.ledger_info_type, 2);
        assert_eq!(decoded.request_cookie.unwrap_or(0), 99);
        assert_eq!(decoded.nodes.len(), 2);
        assert_eq!(
            decoded.nodes[0].nodeid.as_deref().unwrap_or(&[]),
            &[1, 2, 3]
        );
        assert_eq!(
            decoded.nodes[0].nodedata.as_deref().unwrap_or(&[]),
            &[4, 5, 6]
        );
        assert_eq!(decoded.nodes[1].nodeid.as_deref().unwrap_or(&[]), &[7, 8]);
        assert_eq!(
            decoded.nodes[1].nodedata.as_deref().unwrap_or(&[]),
            &[9, 10, 11, 12]
        );
    }

    #[test]
    fn have_set_roundtrip() {
        let hash = Hash256::new([0x0E; 32]);
        let encoded = encode_have_set(&hash, 1);
        let decoded = decode_have_set(&encoded).unwrap();

        assert_eq!(decoded.hash, hash);
        assert_eq!(decoded.status, 1);
    }

    #[test]
    fn have_set_zero_status() {
        let hash = Hash256::new([0xFF; 32]);
        let encoded = encode_have_set(&hash, 0);
        let decoded = decode_have_set(&encoded).unwrap();

        assert_eq!(decoded.hash, hash);
        assert_eq!(decoded.status, 0);
    }

    #[test]
    fn get_objects_response_roundtrip() {
        let ledger_hash = Hash256::new([0xAB; 32]);
        let h1 = Hash256::new([0x01; 32]);
        let h2 = Hash256::new([0x02; 32]);
        let d1 = vec![10, 20, 30];
        let d2 = vec![40, 50];

        let encoded = encode_get_objects_response(
            OT_LEDGER_NODE,
            100,
            Some(&ledger_hash),
            vec![(h1, d1.clone()), (h2, d2.clone())],
        );
        let decoded = decode_get_objects(&encoded).unwrap();

        assert_eq!(decoded.query, Some(false));
        assert_eq!(decoded.r#type, Some(OT_LEDGER_NODE));
        assert_eq!(decoded.seq, Some(100));
        assert_eq!(
            decoded.ledger_hash.as_deref().unwrap_or(&[]),
            ledger_hash.as_bytes()
        );
        assert_eq!(decoded.objects.len(), 2);
        assert_eq!(
            decoded.objects[0].hash.as_deref().unwrap_or(&[]),
            h1.as_bytes()
        );
        assert_eq!(decoded.objects[0].data.as_deref().unwrap_or(&[]), &d1[..]);
        assert_eq!(
            decoded.objects[1].hash.as_deref().unwrap_or(&[]),
            h2.as_bytes()
        );
        assert_eq!(decoded.objects[1].data.as_deref().unwrap_or(&[]), &d2[..]);
    }

    #[test]
    fn get_objects_response_empty() {
        let encoded = encode_get_objects_response(OT_LEDGER_NODE, 50, None, vec![]);
        let decoded = decode_get_objects(&encoded).unwrap();

        assert_eq!(decoded.query, Some(false));
        assert_eq!(decoded.seq, Some(50));
        assert!(decoded.objects.is_empty());
    }

    #[test]
    fn get_objects_request_response_interop() {
        // Encode a request
        let ledger_hash = Hash256::new([0xCC; 32]);
        let hashes = vec![Hash256::new([0x11; 32]), Hash256::new([0x22; 32])];
        let request = encode_get_objects_by_hash(&ledger_hash, 42, &hashes, false);
        let decoded_req = decode_get_objects(&request).unwrap();

        assert_eq!(decoded_req.query, Some(true));
        assert_eq!(decoded_req.seq, Some(42));
        assert_eq!(decoded_req.objects.len(), 2);

        // Encode a response with found data
        let response = encode_get_objects_response(
            decoded_req.r#type.unwrap_or(0),
            decoded_req.seq.unwrap_or(0),
            Some(&ledger_hash),
            vec![(Hash256::new([0x11; 32]), vec![1, 2, 3])],
        );
        let decoded_resp = decode_get_objects(&response).unwrap();

        assert_eq!(decoded_resp.query, Some(false));
        assert_eq!(decoded_resp.objects.len(), 1);
        assert!(decoded_resp.objects[0].data.is_some());
    }

    #[test]
    fn cluster_roundtrip() {
        let nodes = vec![
            ClusterNodeData {
                public_key: "pubkey_abc".to_string(),
                report_time: 12345,
                node_load: 256,
                node_name: "node-alpha".to_string(),
                address: "10.0.0.1:51235".to_string(),
            },
            ClusterNodeData {
                public_key: "pubkey_def".to_string(),
                report_time: 12346,
                node_load: 512,
                node_name: "node-beta".to_string(),
                address: "10.0.0.2:51235".to_string(),
            },
        ];

        let encoded = encode_cluster(&nodes);
        let decoded = decode_cluster(&encoded).unwrap();

        assert_eq!(decoded.len(), 2);
        assert_eq!(decoded[0].public_key, "pubkey_abc");
        assert_eq!(decoded[0].report_time, 12345);
        assert_eq!(decoded[0].node_load, 256);
        assert_eq!(decoded[0].node_name, "node-alpha");
        assert_eq!(decoded[0].address, "10.0.0.1:51235");
        assert_eq!(decoded[1].public_key, "pubkey_def");
        assert_eq!(decoded[1].node_load, 512);
    }

    #[test]
    fn cluster_empty_roundtrip() {
        let encoded = encode_cluster(&[]);
        let decoded = decode_cluster(&encoded).unwrap();
        assert!(decoded.is_empty());
    }

    /// T10 round-trip: build a `Validation` populated with every optional
    /// SOTemplate field, sign it via T09 `sign_validation`, encode it
    /// through `encode_validation`, and decode it back. Every field
    /// (including the strip-result `signing_payload`) must survive the
    /// trip unchanged.
    #[test]
    fn validation_full_sotemplate_roundtrip() {
        use crate::identity::{NodeIdentity, verify_validation_signature};
        use rxrpl_consensus::types::Validation;

        let id = NodeIdentity::generate();
        let mut original = Validation {
            node_id: NodeId(id.node_id),
            public_key: id.public_key_bytes().to_vec(),
            ledger_hash: Hash256::new([0xAB; 32]),
            ledger_seq: 12_345_678,
            full: true,
            close_time: 0, // sfCloseTime is not emitted by sign_validation
            sign_time: 770_000_001,
            signature: None,
            amendments: vec![Hash256::new([0x11; 32]), Hash256::new([0x22; 32])],
            signing_payload: None,
            load_fee: Some(256),
            base_fee: Some(10),
            reserve_base: Some(10_000_000),
            reserve_increment: Some(2_000_000),
            cookie: Some(0xDEAD_BEEF_CAFE_F00D),
            consensus_hash: Some(Hash256::new([0x33; 32])),
            validated_hash: Some(Hash256::new([0x44; 32])),
            server_version: Some(0x0102_0003_0000_0000),
            base_fee_drops: Some(10),
            reserve_base_drops: Some(10_000_000),
            reserve_increment_drops: Some(2_000_000),
        };

        // Sign: stashes the canonical strip-result into `signing_payload`.
        id.sign_validation(&mut original);
        assert!(
            original.signature.is_some(),
            "signing must produce a signature"
        );
        assert!(
            original.signing_payload.is_some(),
            "signing must stash the strip-result"
        );

        // Encode → decode.
        let wire = encode_validation(&original, id.public_key_bytes());
        let decoded = decode_validation(&wire).expect("decode must succeed");

        // Every SOTemplate field round-trips. `close_time` is special:
        // `sign_validation` does not emit sfCloseTime, so on the wire
        // there's no close_time field and the decoder falls back to
        // `sign_time`. We assert that semantics explicitly.
        assert_eq!(decoded.public_key, original.public_key);
        assert_eq!(decoded.node_id, original.node_id);
        assert_eq!(decoded.ledger_hash, original.ledger_hash);
        assert_eq!(decoded.ledger_seq, original.ledger_seq);
        assert_eq!(decoded.full, original.full);
        assert_eq!(decoded.sign_time, original.sign_time);
        assert_eq!(decoded.close_time, original.sign_time);
        assert_eq!(decoded.signature, original.signature);
        assert_eq!(decoded.amendments, original.amendments);
        assert_eq!(decoded.load_fee, original.load_fee);
        assert_eq!(decoded.base_fee, original.base_fee);
        assert_eq!(decoded.reserve_base, original.reserve_base);
        assert_eq!(decoded.reserve_increment, original.reserve_increment);
        assert_eq!(decoded.cookie, original.cookie);
        assert_eq!(decoded.consensus_hash, original.consensus_hash);
        assert_eq!(decoded.validated_hash, original.validated_hash);
        assert_eq!(decoded.server_version, original.server_version);
        assert_eq!(decoded.base_fee_drops, original.base_fee_drops);
        assert_eq!(decoded.reserve_base_drops, original.reserve_base_drops);
        assert_eq!(
            decoded.reserve_increment_drops,
            original.reserve_increment_drops
        );
        // The strip-result must be byte-identical: any divergence here
        // would break signature verification on the receiving side.
        assert_eq!(
            decoded.signing_payload, original.signing_payload,
            "strip-result must round-trip byte-for-byte"
        );

        // And the signature must verify against the decoded validation.
        assert!(
            verify_validation_signature(&decoded),
            "decoded validation signature must verify"
        );
    }

    /// T10 round-trip — minimal Validation (no optional fields). Verifies
    /// that the legacy 5-field byte image still round-trips after the
    /// decoder rewrite.
    #[test]
    fn validation_minimal_roundtrip() {
        use crate::identity::{NodeIdentity, verify_validation_signature};
        use rxrpl_consensus::types::Validation;

        let id = NodeIdentity::generate();
        let mut original = Validation {
            node_id: NodeId(id.node_id),
            public_key: id.public_key_bytes().to_vec(),
            ledger_hash: Hash256::new([0xCC; 32]),
            ledger_seq: 42,
            full: true,
            close_time: 1000,
            sign_time: 1000,
            signature: None,
            amendments: vec![],
            signing_payload: None,
            ..Default::default()
        };
        id.sign_validation(&mut original);

        let wire = encode_validation(&original, id.public_key_bytes());
        let decoded = decode_validation(&wire).expect("decode must succeed");

        assert_eq!(decoded.public_key, original.public_key);
        assert_eq!(decoded.ledger_hash, original.ledger_hash);
        assert_eq!(decoded.ledger_seq, original.ledger_seq);
        assert_eq!(decoded.full, original.full);
        assert_eq!(decoded.sign_time, original.sign_time);
        assert_eq!(decoded.signature, original.signature);
        assert!(decoded.amendments.is_empty());
        assert!(decoded.load_fee.is_none());
        assert!(decoded.base_fee.is_none());
        assert!(decoded.cookie.is_none());
        assert!(decoded.consensus_hash.is_none());
        assert_eq!(decoded.signing_payload, original.signing_payload);

        assert!(verify_validation_signature(&decoded));
    }

    /// H13 regression: a validation that explicitly carries `sfCloseTime
    /// = 0` on the wire (the rippled "no opinion on close_time"
    /// sentinel) MUST decode with `close_time == 0`. The pre-fix
    /// decoder rewrote any zero close_time to `sign_time`, destroying
    /// the sentinel and feeding fabricated close_times into the
    /// consensus bucket logic.
    #[test]
    fn h13_explicit_zero_close_time_is_preserved() {
        use crate::stobject;

        // Build a hand-crafted STObject containing every field the
        // decoder needs for a well-formed validation, with sfCloseTime
        // (2,7) explicitly present and set to 0.
        let mut stobj: Vec<u8> = Vec::new();
        stobject::put_uint32(&mut stobj, 2, 0x80000001); // sfFlags (full=true)
        stobject::put_uint32(&mut stobj, 6, 42); // sfLedgerSequence
        stobject::put_uint32(&mut stobj, 7, 0); // sfCloseTime — explicit 0
        stobject::put_uint32(&mut stobj, 9, 770_000_001); // sfSigningTime
        stobject::put_hash256(&mut stobj, 1, &[0xCD; 32]); // sfLedgerHash
        stobject::put_vl(&mut stobj, 3, &[0x02u8; 33]); // sfSigningPubKey

        let msg = TmValidation {
            validation: Some(stobj),
        };
        let wire = msg.encode_to_vec();

        let decoded = decode_validation(&wire).expect("decode must succeed");

        // The wire payload had sfCloseTime explicitly set to 0; the
        // decoder MUST preserve that value rather than substituting
        // sign_time.
        assert_eq!(
            decoded.close_time, 0,
            "explicit sfCloseTime=0 sentinel was rewritten to {} \
             (sign_time={}) — H13 regression",
            decoded.close_time, decoded.sign_time
        );
        // Sanity: sign_time still decodes correctly.
        assert_eq!(decoded.sign_time, 770_000_001);
    }

    /// Companion to the H13 regression: when sfCloseTime is *absent*
    /// from the wire, the decoder MUST still fall back to `sign_time`
    /// for backward compatibility with the existing encoder, which
    /// never emits sfCloseTime.
    #[test]
    fn h13_absent_close_time_falls_back_to_sign_time() {
        use crate::stobject;

        let mut stobj: Vec<u8> = Vec::new();
        stobject::put_uint32(&mut stobj, 2, 0x80000001); // sfFlags
        stobject::put_uint32(&mut stobj, 6, 42); // sfLedgerSequence
        // sfCloseTime intentionally omitted.
        stobject::put_uint32(&mut stobj, 9, 770_000_001); // sfSigningTime
        stobject::put_hash256(&mut stobj, 1, &[0xCD; 32]); // sfLedgerHash
        stobject::put_vl(&mut stobj, 3, &[0x02u8; 33]); // sfSigningPubKey

        let msg = TmValidation {
            validation: Some(stobj),
        };
        let wire = msg.encode_to_vec();
        let decoded = decode_validation(&wire).expect("decode must succeed");

        assert_eq!(
            decoded.close_time, 770_000_001,
            "absent sfCloseTime should fall back to sign_time"
        );
    }

    /// H9 regression: a peer-supplied TMValidation whose inner
    /// `validation` STObject claims to be larger than 2 *
    /// MAX_STVALIDATION_BYTES MUST be rejected as malformed *before*
    /// the decoder allocates its `signing_payload` buffer. This bounds
    /// the worst-case allocation per decoded message at 32 KiB even
    /// when a peer ships a 16 MiB blob, preventing the
    /// `Vec::with_capacity(payload.len())` memory amplification
    /// described in audit pass 1 H9.
    #[test]
    fn decode_validation_caps_oversize_signing_payload_alloc() {
        // 16 MiB of zeros — well above 2 * MAX_STVALIDATION_BYTES (64
        // KiB). The bytes themselves don't have to parse: the size
        // check fires before the STObject walker runs.
        let oversize_payload = vec![0u8; 16 * 1024 * 1024];

        let msg = TmValidation {
            validation: Some(oversize_payload),
        };
        let wire = msg.encode_to_vec();

        let result = decode_validation(&wire);
        assert!(
            result.is_err(),
            "oversize STValidation payload must be rejected before allocation"
        );
        let err = result.unwrap_err();
        let err_msg = format!("{err}");
        assert!(
            err_msg.contains("STValidation payload too large"),
            "error must identify the cap violation, got: {err_msg}"
        );
    }
}
