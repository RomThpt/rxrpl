//! T27 — byte-level wire-diff regression suite for TMValidation.
//!
//! These tests assert that the bytes rxrpl emits on the wire match the
//! canonical layout produced by goXRPLd's `SerializeSTValidation` (which is
//! known to interoperate with rippled).  Each invariant comes directly from
//! the T27 acceptance criteria; if a future change re-introduces the
//! pre-fix ordering or flag bug, exactly one of these tests will fail and
//! point at the regressed property by name.
//!
//! Reference sources:
//!   * goXRPLd: `internal/consensus/adaptor/stvalidation.go`
//!     (`SerializeSTValidation`, `parseSTValidation`)
//!   * rippled: `src/libxrpl/protocol/STValidation.cpp` (`validationFormat`,
//!     `getSigningHash` → `STObject::getSigningHash(HashPrefix::validation)`)
//!     and `src/libxrpl/protocol/SField.h` (type/field codes).

use prost::Message;
use rxrpl_consensus::types::{NodeId, Validation};
use rxrpl_overlay::identity::{NodeIdentity, verify_validation_signature};
use rxrpl_overlay::proto_convert::{decode_validation, encode_validation};
use rxrpl_p2p_proto::proto::TmValidation;
use rxrpl_primitives::Hash256;

/// Build a deterministic full validation populated with two amendment IDs
/// (the smallest input that exercises the `sfSignature` insertion point).
fn signed_validation_with_amendments(id: &NodeIdentity) -> Validation {
    let mut v = Validation {
        node_id: NodeId(Hash256::new(id.node_id.0)),
        public_key: id.public_key_bytes().to_vec(),
        ledger_hash: Hash256::new([0xAB; 32]),
        ledger_seq: 12_345_678,
        full: true,
        close_time: 0, // not emitted by sign_validation
        sign_time: 770_000_001,
        signature: None,
        amendments: vec![Hash256::new([0x11; 32]), Hash256::new([0x22; 32])],
        signing_payload: None,
        ..Default::default()
    };
    id.sign_validation(&mut v);
    v
}

/// Pull the inner STObject blob out of a TMValidation protobuf payload.
fn stobject_bytes(wire: &[u8]) -> Vec<u8> {
    TmValidation::decode(wire)
        .expect("TmValidation must decode")
        .validation
        .unwrap_or_default()
}

/// (1) `sfFlags` for a full validation MUST equal `vfFullyCanonicalSig |
/// vfFullValidation` (`0x80000001`).  Per rippled `STValidation::sign`
/// (STValidation.cpp:236) `vfFullyCanonicalSig` (`0x80000000`) is asserted
/// on every outbound validation; without it rippled treats the signature
/// as not necessarily canonical and the suppression-hash bookkeeping
/// diverges from peers.
#[test]
fn full_validation_flags_set_canonical_and_full_bits() {
    let id = NodeIdentity::generate();
    let v = signed_validation_with_amendments(&id);
    let wire = encode_validation(&v, id.public_key_bytes());
    let stobj = stobject_bytes(&wire);

    // sfFlags is type=2 field=2 → header byte 0x22, followed by 4 BE bytes.
    assert_eq!(stobj[0], 0x22, "sfFlags must be the first field");
    let flags = u32::from_be_bytes([stobj[1], stobj[2], stobj[3], stobj[4]]);
    assert_eq!(
        flags, 0x8000_0001,
        "full validation must set vfFullyCanonicalSig|vfFullValidation"
    );
}

/// (2) Canonical type-then-field ordering of every field in the encoded
/// blob.  `STObject::add()` in rippled sorts on `fieldCode ascending`; the
/// goXRPL serializer emits in the same order.  Walking the wire bytes in
/// declaration order MUST yield strictly increasing `(type_code << 16) |
/// field_code` keys.
#[test]
fn encoded_fields_are_in_canonical_ascending_order() {
    let id = NodeIdentity::generate();
    let v = signed_validation_with_amendments(&id);
    let wire = encode_validation(&v, id.public_key_bytes());
    let stobj = stobject_bytes(&wire);

    let keys = decode_field_keys(&stobj);
    assert!(
        keys.windows(2).all(|w| w[0] < w[1]),
        "fields not in canonical ascending order: {:?}",
        keys.iter()
            .map(|k| format!("0x{k:06X}"))
            .collect::<Vec<_>>()
    );
}

/// (2b) The exact key sequence for the all-required-plus-amendments shape
/// MUST match the goXRPL reference: sfFlags, sfLedgerSequence, sfSigningTime,
/// sfLedgerHash, sfSigningPubKey, sfSignature, sfAmendments.  This is the
/// regression that the pre-fix encoder broke (sfAmendments was emitted
/// before sfSignature, breaking canonical order).
#[test]
fn signature_is_inserted_before_amendments_canonically() {
    let id = NodeIdentity::generate();
    let v = signed_validation_with_amendments(&id);
    let wire = encode_validation(&v, id.public_key_bytes());
    let stobj = stobject_bytes(&wire);

    let keys = decode_field_keys(&stobj);
    let expected: Vec<u32> = vec![
        (2 << 16) | 2,  // sfFlags
        (2 << 16) | 6,  // sfLedgerSequence
        (2 << 16) | 9,  // sfSigningTime
        (5 << 16) | 1,  // sfLedgerHash
        (7 << 16) | 3,  // sfSigningPubKey
        (7 << 16) | 6,  // sfSignature  ← canonical position
        (19 << 16) | 3, // sfAmendments ← AFTER sfSignature
    ];
    assert_eq!(
        keys, expected,
        "wire field sequence diverges from goXRPL/rippled canonical order"
    );
}

/// (3) `sfAmendments` is encoded with type-code 19 (Vector256) and
/// field-code 3.  The two-byte field header is `[0x03, 0x13]` (field<<0,
/// type<<0) per the rippled extended-encoding rules in
/// `STBase::getFName` / `Serializer::addFieldID`.
#[test]
fn amendments_field_header_is_type19_field3() {
    let id = NodeIdentity::generate();
    let v = signed_validation_with_amendments(&id);
    let wire = encode_validation(&v, id.public_key_bytes());
    let stobj = stobject_bytes(&wire);

    let amendments_start =
        find_field_offset(&stobj, 19, 3).expect("sfAmendments must appear in the encoded blob");
    assert_eq!(
        &stobj[amendments_start..amendments_start + 2],
        &[0x03, 0x13],
        "sfAmendments field header must be [0x03, 0x13]"
    );
    // VL-prefixed body: 2 entries × 32 bytes = 64 ⇒ single-byte VL = 64.
    assert_eq!(stobj[amendments_start + 2], 64, "VL byte length");
}

/// (4) secp256k1 signatures are serialized as ASN.1 DER, NOT raw R||S.
/// rippled rejects raw-R||S signatures: `PublicKey::ecdsaCanonicality`
/// requires `sig[0] == 0x30` (DER SEQUENCE tag) and parses two
/// `02 <len> <int>` components.
#[test]
fn signature_is_der_encoded_not_raw_r_s() {
    let id = NodeIdentity::generate();
    let v = signed_validation_with_amendments(&id);
    let wire = encode_validation(&v, id.public_key_bytes());
    let stobj = stobject_bytes(&wire);

    let sig_start = find_field_offset(&stobj, 7, 6).expect("sfSignature present");
    // After the field header (1 byte: 0x76) comes the VL prefix and DER body.
    assert_eq!(stobj[sig_start], 0x76, "sfSignature header byte");
    let (sig_len, vl_hdr_len) = decode_vl_length_inline(&stobj[sig_start + 1..])
        .expect("sfSignature VL length must decode");
    let body_start = sig_start + 1 + vl_hdr_len;
    let der = &stobj[body_start..body_start + sig_len];
    assert!(
        der.len() >= 8 && der.len() <= 72,
        "DER signature length out of bounds: {} bytes",
        der.len()
    );
    assert_eq!(
        der[0], 0x30,
        "DER signature must start with SEQUENCE (0x30)"
    );
    assert_eq!(
        der[1] as usize,
        der.len() - 2,
        "DER length byte must equal sig.len() - 2"
    );
    // The first INTEGER follows immediately after the SEQUENCE header.
    assert_eq!(der[2], 0x02, "first DER component must be INTEGER (0x02)");
}

/// (5) `sfSigningPubKey` is VL-prefixed and carries the 33-byte
/// secp256k1-compressed public key.  Per rippled the field header is
/// `0x73` (type=7, field=3); the VL prefix uses the standard XRPL VL
/// length encoding.
#[test]
fn signing_pubkey_is_vl_prefixed_33_bytes() {
    let id = NodeIdentity::generate();
    let v = signed_validation_with_amendments(&id);
    let wire = encode_validation(&v, id.public_key_bytes());
    let stobj = stobject_bytes(&wire);

    let pk_start = find_field_offset(&stobj, 7, 3).expect("sfSigningPubKey present");
    assert_eq!(stobj[pk_start], 0x73, "sfSigningPubKey header byte");
    let (pk_len, vl_hdr_len) =
        decode_vl_length_inline(&stobj[pk_start + 1..]).expect("VL length must decode");
    assert_eq!(pk_len, 33, "secp256k1 compressed pubkey is 33 bytes");
    let body_start = pk_start + 1 + vl_hdr_len;
    assert_eq!(
        &stobj[body_start..body_start + 33],
        id.public_key_bytes(),
        "embedded pubkey must equal NodeIdentity public key"
    );
    // Compressed key first byte is 0x02 or 0x03.
    assert!(
        stobj[body_start] == 0x02 || stobj[body_start] == 0x03,
        "compressed secp256k1 pubkey must start with 0x02 or 0x03"
    );
}

/// (6) `HashPrefix::validation` (`0x56414C00` = "VAL\0") is prepended to the
/// canonical strip-result before signing/verification.  We can't observe
/// the prefix on the wire (the wire is the strip-result + sfSignature),
/// but we can prove it is in the signing input by re-running the verifier:
/// `verify_validation_signature` reconstructs the prefix and would fail if
/// `sign_validation` had used a different one.
#[test]
fn signing_uses_validation_hash_prefix() {
    let id = NodeIdentity::generate();
    let v = signed_validation_with_amendments(&id);
    let wire = encode_validation(&v, id.public_key_bytes());

    // Round-trip through the wire so verification uses the
    // decoder-reconstructed signing_payload (the rippled-equivalent path).
    let decoded = decode_validation(&wire).expect("wire must decode");
    assert!(
        verify_validation_signature(&decoded),
        "decoded validation must verify — proves HashPrefix::validation is \
         applied consistently between sign and verify"
    );
}

/// (7) Frame header for mtVALIDATION: a 6-byte uncompressed header is
/// emitted by the `PeerCodec`.  The first 4 bytes are the BE payload
/// length with the 6 high bits clear (uncompressed flag); the next 2
/// bytes are the BE message type (mtVALIDATION = 41).
#[test]
fn frame_header_is_6_bytes_with_type_41() {
    use bytes::BytesMut;
    use rxrpl_p2p_proto::codec::{PeerCodec, PeerMessage};
    use rxrpl_p2p_proto::message::MessageType;
    use tokio_util::codec::Encoder;

    let id = NodeIdentity::generate();
    let v = signed_validation_with_amendments(&id);
    let payload = encode_validation(&v, id.public_key_bytes());

    let mut codec = PeerCodec;
    let mut frame = BytesMut::new();
    codec
        .encode(
            PeerMessage {
                msg_type: MessageType::Validation,
                payload: payload.clone(),
            },
            &mut frame,
        )
        .expect("frame encode must succeed");

    assert!(frame.len() >= 6, "frame must include the 6-byte header");
    let length_word = u32::from_be_bytes([frame[0], frame[1], frame[2], frame[3]]);
    assert_eq!(
        length_word & 0xFC00_0000,
        0,
        "compression flag bits must be zero (uncompressed)"
    );
    assert_eq!(
        length_word as usize,
        payload.len(),
        "length word must equal payload length"
    );
    let msg_type = u16::from_be_bytes([frame[4], frame[5]]);
    assert_eq!(msg_type, 41, "mtVALIDATION = 41");
    assert_eq!(
        frame.len(),
        6 + payload.len(),
        "frame is exactly header + payload bytes"
    );
}

/// Defensive: a validation WITHOUT amendments (the most common shape)
/// still ends with `sfSignature` and emits exactly 5 fields + signature.
/// Catches accidental regressions of the no-amendments path while we are
/// fixing the amendments path.
#[test]
fn no_amendments_path_unchanged() {
    let id = NodeIdentity::generate();
    let mut v = Validation {
        node_id: NodeId(Hash256::new(id.node_id.0)),
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
    id.sign_validation(&mut v);
    let wire = encode_validation(&v, id.public_key_bytes());
    let stobj = stobject_bytes(&wire);

    let keys = decode_field_keys(&stobj);
    let expected: Vec<u32> = vec![
        (2 << 16) | 2, // sfFlags
        (2 << 16) | 6, // sfLedgerSequence
        (2 << 16) | 9, // sfSigningTime
        (5 << 16) | 1, // sfLedgerHash
        (7 << 16) | 3, // sfSigningPubKey
        (7 << 16) | 6, // sfSignature
    ];
    assert_eq!(keys, expected, "no-amendments wire layout regressed");
}

/// (T29 / H12) `decode_validation` MUST reject any STObject payload that
/// repeats the same `(type_code, field_code)` pair.  rippled's
/// `STObject::checkSorting` (src/libxrpl/protocol/STObject.cpp) treats a
/// duplicate field as an unrecoverable parse error: a peer that crafts e.g.
/// two distinct `sfLedgerHash` fields could otherwise convince the local
/// node to validate hash A while the suppression-hash bookkeeping (which
/// runs over the raw bytes) sees hash B.  Either field accepted by the
/// decoder — first-wins or last-wins — would split the network's view of
/// the validation.  The only safe behaviour is to reject the payload.
#[test]
fn decode_validation_rejects_duplicate_ledger_hash() {
    use rxrpl_overlay::stobject;

    // Hand-craft an STObject that is byte-valid (every field encoding
    // parses, every length matches) but carries sfLedgerHash twice with
    // different 32-byte values.  Field order otherwise follows the
    // canonical layout the decoder expects.
    let mut stobj: Vec<u8> = Vec::new();
    stobject::put_uint32(&mut stobj, 2, 0x80000001); // sfFlags
    stobject::put_uint32(&mut stobj, 6, 42); // sfLedgerSequence
    stobject::put_uint32(&mut stobj, 9, 770_000_001); // sfSigningTime
    stobject::put_hash256(&mut stobj, 1, &[0xAA; 32]); // sfLedgerHash #1
    stobject::put_hash256(&mut stobj, 1, &[0xBB; 32]); // sfLedgerHash #2 — duplicate
    stobject::put_vl(&mut stobj, 3, &[0x02u8; 33]); // sfSigningPubKey

    let msg = TmValidation {
        validation: Some(stobj),
    };
    let wire = msg.encode_to_vec();

    let result = decode_validation(&wire);
    assert!(
        result.is_err(),
        "decode_validation must reject duplicate sfLedgerHash field, got {:?}",
        result
    );
}

/// (T29 / H12) `decode_validation` MUST reject any STObject payload whose
/// fields are not in strictly ascending `(type_code << 16) | field_code`
/// order.  Swapping two adjacent fields (here sfSigningTime (2,9) and
/// sfLedgerSequence (2,6)) produces a non-canonical layout that rippled
/// would reject; accepting it locally would let an attacker probe whether
/// our parser is order-tolerant and use that to bypass dedup logic.
#[test]
fn decode_validation_rejects_out_of_order_fields() {
    use rxrpl_overlay::stobject;

    // sfSigningTime (key 0x20009) emitted BEFORE sfLedgerSequence
    // (key 0x20006) — both UINT32, so the byte structure parses fine
    // but the canonical-order invariant is violated.
    let mut stobj: Vec<u8> = Vec::new();
    stobject::put_uint32(&mut stobj, 2, 0x80000001); // sfFlags (2,2)
    stobject::put_uint32(&mut stobj, 9, 770_000_001); // sfSigningTime (2,9) — out of order
    stobject::put_uint32(&mut stobj, 6, 42); // sfLedgerSequence (2,6) — should precede (2,9)
    stobject::put_hash256(&mut stobj, 1, &[0xCD; 32]); // sfLedgerHash
    stobject::put_vl(&mut stobj, 3, &[0x02u8; 33]); // sfSigningPubKey

    let msg = TmValidation {
        validation: Some(stobj),
    };
    let wire = msg.encode_to_vec();

    let result = decode_validation(&wire);
    assert!(
        result.is_err(),
        "decode_validation must reject out-of-order fields, got {:?}",
        result
    );
}

// ---------------------------------------------------------------------------
// Local helpers: minimal STObject walkers reimplemented in the test crate so
// the assertions don't depend on private rxrpl-overlay APIs.
// ---------------------------------------------------------------------------

/// Decode a single field header.  Returns `(type, field, header_byte_count)`.
fn decode_field_id(data: &[u8]) -> Option<(u8, u16, usize)> {
    let b0 = *data.first()?;
    let type_id = (b0 >> 4) & 0x0F;
    let field_id = b0 & 0x0F;
    match (type_id, field_id) {
        (0, 0) => Some((*data.get(1)?, *data.get(2)? as u16, 3)),
        (0, f) => Some((*data.get(1)?, f as u16, 2)),
        (t, 0) => Some((t, *data.get(1)? as u16, 2)),
        (t, f) => Some((t, f as u16, 1)),
    }
}

/// XRPL VL length prefix decoder.  Returns `(length, prefix_byte_count)`.
fn decode_vl_length_inline(data: &[u8]) -> Option<(usize, usize)> {
    let b0 = *data.first()? as usize;
    if b0 <= 192 {
        Some((b0, 1))
    } else if b0 <= 240 {
        let b1 = *data.get(1)? as usize;
        Some((193 + (b0 - 193) * 256 + b1, 2))
    } else if b0 <= 254 {
        let b1 = *data.get(1)? as usize;
        let b2 = *data.get(2)? as usize;
        Some((12481 + (b0 - 241) * 65536 + b1 * 256 + b2, 3))
    } else {
        None
    }
}

/// Walk the STObject and return the canonical `(type<<16)|field` key for
/// each field in declaration order.
fn decode_field_keys(stobj: &[u8]) -> Vec<u32> {
    let mut keys = Vec::new();
    let mut pos = 0;
    while pos < stobj.len() {
        let (type_id, field_id, hdr) = match decode_field_id(&stobj[pos..]) {
            Some(t) => t,
            None => break,
        };
        keys.push(((type_id as u32) << 16) | field_id as u32);
        pos += hdr + value_len(type_id, &stobj[pos + hdr..]).unwrap_or(usize::MAX);
        if pos > stobj.len() {
            break;
        }
    }
    keys
}

/// Find the first byte offset at which a field with the given type/field
/// header appears in the STObject.
fn find_field_offset(stobj: &[u8], target_type: u8, target_field: u16) -> Option<usize> {
    let mut pos = 0;
    while pos < stobj.len() {
        let field_start = pos;
        let (type_id, field_id, hdr) = decode_field_id(&stobj[pos..])?;
        if type_id == target_type && field_id == target_field {
            return Some(field_start);
        }
        pos += hdr + value_len(type_id, &stobj[pos + hdr..])?;
    }
    None
}

/// Length of a field value (after its header) for the type IDs that can
/// appear in a STValidation wire blob.
fn value_len(type_id: u8, after_hdr: &[u8]) -> Option<usize> {
    match type_id {
        2 => Some(4),  // UINT32
        3 => Some(8),  // UINT64
        5 => Some(32), // UINT256 / Hash256
        6 => Some(8),  // AMOUNT (XRP-native)
        7 | 19 => {
            // VL-prefixed: prefix bytes + body bytes.
            let (len, hdr) = decode_vl_length_inline(after_hdr)?;
            Some(hdr + len)
        }
        _ => None,
    }
}
