/// Minimal STObject serializer for rippled wire-compatible validation encoding.
///
/// Implements the XRPL binary serialization format for the subset of fields
/// needed by STValidation objects.
///
/// Field IDs and type codes verified against rippled
/// `include/xrpl/protocol/detail/sfields.macro` and
/// `include/xrpl/protocol/SField.h`.

// SField type IDs (STYPE codes from rippled SField.h)
const STI_UINT32: u8 = 2;
const STI_UINT64: u8 = 3;
const STI_UINT256: u8 = 5;
const STI_AMOUNT: u8 = 6;
const STI_VL: u8 = 7;
const STI_VECTOR256: u8 = 19;

/// XRP native amount "positive" flag bit, set on the high u64 to mark a
/// non-negative XRP drops amount. Matches rippled `STAmount::cPositive`
/// (`0x4000_0000_0000_0000`).
const AMOUNT_NATIVE_POSITIVE: u64 = 0x4000_0000_0000_0000;
/// Bit that marks an STAmount as IOU/issued (high bit of the first byte).
/// Matches rippled `STAmount::cIssuedCurrency` (`0x8000_0000_0000_0000`).
const AMOUNT_ISSUED_FLAG: u64 = 0x8000_0000_0000_0000;

/// Encode a field header (type + field code) into the buffer.
fn encode_field_id(buf: &mut Vec<u8>, type_id: u8, field_id: u16) {
    if type_id < 16 && field_id < 16 {
        buf.push((type_id << 4) | (field_id as u8));
    } else if type_id < 16 && field_id >= 16 {
        buf.push(type_id << 4);
        buf.push(field_id as u8);
    } else if type_id >= 16 && field_id < 16 {
        buf.push(field_id as u8);
        buf.push(type_id);
    } else {
        buf.push(0);
        buf.push(type_id);
        buf.push(field_id as u8);
    }
}

/// Encode a VL (variable-length) prefix.
fn encode_vl_length(buf: &mut Vec<u8>, len: usize) {
    if len <= 192 {
        buf.push(len as u8);
    } else if len <= 12480 {
        let adjusted = len - 193;
        buf.push((adjusted / 256 + 193) as u8);
        buf.push((adjusted % 256) as u8);
    } else {
        let adjusted = len - 12481;
        buf.push((adjusted / 65536 + 241) as u8);
        buf.push(((adjusted / 256) % 256) as u8);
        buf.push((adjusted % 256) as u8);
    }
}

pub fn put_uint32(buf: &mut Vec<u8>, field_id: u16, value: u32) {
    encode_field_id(buf, STI_UINT32, field_id);
    buf.extend_from_slice(&value.to_be_bytes());
}

pub fn put_uint64(buf: &mut Vec<u8>, field_id: u16, value: u64) {
    encode_field_id(buf, STI_UINT64, field_id);
    buf.extend_from_slice(&value.to_be_bytes());
}

pub fn put_hash256(buf: &mut Vec<u8>, field_id: u16, value: &[u8; 32]) {
    encode_field_id(buf, STI_UINT256, field_id);
    buf.extend_from_slice(value);
}

pub fn put_vl(buf: &mut Vec<u8>, field_id: u16, value: &[u8]) {
    encode_field_id(buf, STI_VL, field_id);
    encode_vl_length(buf, value.len());
    buf.extend_from_slice(value);
}

/// Encode an XRP-native Amount (drops). Per rippled `STAmount::add`, native
/// non-negative XRP is serialized as a single big-endian u64 with the
/// `cPositive` bit (`0x4000_0000_0000_0000`) ORed in. Drops are unsigned
/// and so always non-negative.
pub fn put_amount_xrp(buf: &mut Vec<u8>, field_id: u16, drops: u64) {
    encode_field_id(buf, STI_AMOUNT, field_id);
    let encoded = drops | AMOUNT_NATIVE_POSITIVE;
    buf.extend_from_slice(&encoded.to_be_bytes());
}

/// Encode a Vector256 field: VL length prefix (in bytes, i.e. `entries.len()*32`)
/// followed by each 32-byte entry concatenated. Per rippled
/// `STVector256::add` / `Serializer::addVL`.
pub fn put_vector256(buf: &mut Vec<u8>, field_id: u16, entries: &[[u8; 32]]) {
    encode_field_id(buf, STI_VECTOR256, field_id);
    let byte_len = entries.len() * 32;
    encode_vl_length(buf, byte_len);
    for entry in entries {
        buf.extend_from_slice(entry);
    }
}

/// Decode a VL length prefix, returning (length, bytes_consumed).
pub fn decode_vl_length(data: &[u8]) -> Option<(usize, usize)> {
    if data.is_empty() {
        return None;
    }
    let b0 = data[0] as usize;
    if b0 <= 192 {
        Some((b0, 1))
    } else if b0 <= 240 {
        if data.len() < 2 {
            return None;
        }
        let len = 193 + (b0 - 193) * 256 + data[1] as usize;
        Some((len, 2))
    } else if b0 <= 254 {
        if data.len() < 3 {
            return None;
        }
        let len = 12481 + (b0 - 241) * 65536 + (data[1] as usize) * 256 + data[2] as usize;
        Some((len, 3))
    } else {
        None
    }
}

/// Decode a field header, returning (type_id, field_id, bytes_consumed).
pub fn decode_field_id(data: &[u8]) -> Option<(u8, u16, usize)> {
    if data.is_empty() {
        return None;
    }
    let b0 = data[0];
    let type_id = (b0 >> 4) & 0x0F;
    let field_id = b0 & 0x0F;

    match (type_id, field_id) {
        (0, 0) => {
            // Both extended
            if data.len() < 3 {
                return None;
            }
            Some((data[1], data[2] as u16, 3))
        }
        (0, f) => {
            // Type extended
            if data.len() < 2 {
                return None;
            }
            Some((data[1], f as u16, 2))
        }
        (t, 0) => {
            // Field extended
            if data.len() < 2 {
                return None;
            }
            Some((t, data[1] as u16, 2))
        }
        (t, f) => Some((t, f as u16, 1)),
    }
}

/// Decode a UINT32 value from the start of `data` (after the field header).
/// Returns (value, bytes_consumed).
pub fn decode_uint32(data: &[u8]) -> Option<(u32, usize)> {
    if data.len() < 4 {
        return None;
    }
    let mut bytes = [0u8; 4];
    bytes.copy_from_slice(&data[..4]);
    Some((u32::from_be_bytes(bytes), 4))
}

/// Decode a UINT64 value from the start of `data` (after the field header).
/// Returns (value, bytes_consumed).
pub fn decode_uint64(data: &[u8]) -> Option<(u64, usize)> {
    if data.len() < 8 {
        return None;
    }
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&data[..8]);
    Some((u64::from_be_bytes(bytes), 8))
}

/// Decode a UINT256 (32-byte hash) value from the start of `data`.
/// Returns (value, bytes_consumed).
pub fn decode_hash256(data: &[u8]) -> Option<([u8; 32], usize)> {
    if data.len() < 32 {
        return None;
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&data[..32]);
    Some((out, 32))
}

/// Decode a VL value (length-prefixed bytes) from the start of `data`.
/// Returns (value, bytes_consumed).
pub fn decode_vl(data: &[u8]) -> Option<(Vec<u8>, usize)> {
    let (len, hdr) = decode_vl_length(data)?;
    if data.len() < hdr + len {
        return None;
    }
    Some((data[hdr..hdr + len].to_vec(), hdr + len))
}

/// Decode an XRP-native Amount (drops). Returns `None` if the payload is too
/// short or is not a native XRP amount (issued-currency flag set).
/// Returns (drops, bytes_consumed).
pub fn decode_amount_xrp(data: &[u8]) -> Option<(u64, usize)> {
    if data.len() < 8 {
        return None;
    }
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&data[..8]);
    let raw = u64::from_be_bytes(bytes);
    // Reject IOU/issued amounts (high bit set).
    if raw & AMOUNT_ISSUED_FLAG != 0 {
        return None;
    }
    // Strip cPositive flag to recover the drops value. Native XRP is always
    // non-negative on the wire (validators have no use for negative drops).
    let drops = raw & !AMOUNT_NATIVE_POSITIVE;
    Some((drops, 8))
}

/// Decode a Vector256 value (VL prefix + concatenated 32-byte entries).
/// Returns (entries, bytes_consumed). Returns `None` if the payload length
/// is not a multiple of 32 or the data is truncated.
pub fn decode_vector256(data: &[u8]) -> Option<(Vec<[u8; 32]>, usize)> {
    let (byte_len, hdr) = decode_vl_length(data)?;
    if byte_len % 32 != 0 {
        return None;
    }
    if data.len() < hdr + byte_len {
        return None;
    }
    let mut out = Vec::with_capacity(byte_len / 32);
    for chunk in data[hdr..hdr + byte_len].chunks_exact(32) {
        let mut entry = [0u8; 32];
        entry.copy_from_slice(chunk);
        out.push(entry);
    }
    Some((out, hdr + byte_len))
}

#[cfg(test)]
mod tests {
    use super::*;

    // SField IDs verified against rippled
    // include/xrpl/protocol/detail/sfields.macro
    const SF_FLAGS: u16 = 2;
    const SF_LOAD_FEE: u16 = 24;
    const SF_RESERVE_BASE: u16 = 31;
    const SF_RESERVE_INCREMENT: u16 = 32;
    const SF_BASE_FEE: u16 = 5; // UINT64
    const SF_COOKIE: u16 = 10;
    const SF_SERVER_VERSION: u16 = 11;
    const SF_CONSENSUS_HASH: u16 = 23;
    const SF_VALIDATED_HASH: u16 = 25;
    const SF_BASE_FEE_DROPS: u16 = 22;
    const SF_RESERVE_BASE_DROPS: u16 = 23;
    const SF_RESERVE_INCREMENT_DROPS: u16 = 24;
    const SF_AMENDMENTS: u16 = 3;

    #[test]
    fn field_id_encoding() {
        // sfFlags: type=2 (UINT32), field=2 -> byte 0x22
        let mut buf = Vec::new();
        encode_field_id(&mut buf, 2, 2);
        assert_eq!(buf, vec![0x22]);

        // sfLedgerSequence: type=2, field=6 -> byte 0x26
        buf.clear();
        encode_field_id(&mut buf, 2, 6);
        assert_eq!(buf, vec![0x26]);

        // sfLedgerHash: type=5, field=1 -> byte 0x51
        buf.clear();
        encode_field_id(&mut buf, 5, 1);
        assert_eq!(buf, vec![0x51]);

        // sfSigningPubKey: type=7, field=3 -> byte 0x73
        buf.clear();
        encode_field_id(&mut buf, 7, 3);
        assert_eq!(buf, vec![0x73]);

        // sfReserveBase: type=2, field=31 (>=16) -> 0x20, 0x1F
        buf.clear();
        encode_field_id(&mut buf, 2, 31);
        assert_eq!(buf, vec![0x20, 0x1F]);

        // sfAmendments: type=19 (>=16), field=3 (<16) -> 0x03, 0x13
        buf.clear();
        encode_field_id(&mut buf, 19, 3);
        assert_eq!(buf, vec![0x03, 0x13]);

        // Both >=16: type=19, field=22 -> 0x00, 0x13, 0x16
        buf.clear();
        encode_field_id(&mut buf, 19, 22);
        assert_eq!(buf, vec![0x00, 0x13, 0x16]);
    }

    #[test]
    fn vl_length_encoding() {
        let mut buf = Vec::new();
        encode_vl_length(&mut buf, 33);
        assert_eq!(buf, vec![33]);

        buf.clear();
        encode_vl_length(&mut buf, 72);
        assert_eq!(buf, vec![72]);
    }

    #[test]
    fn uint32_encoding() {
        let mut buf = Vec::new();
        put_uint32(&mut buf, SF_FLAGS, 0x80000001); // sfFlags = vfFullValidation
        assert_eq!(&buf[0..1], &[0x22]); // field header
        assert_eq!(&buf[1..5], &[0x80, 0x00, 0x00, 0x01]); // value BE
    }

    fn roundtrip_uint32(field_id: u16, value: u32) {
        let mut buf = Vec::new();
        put_uint32(&mut buf, field_id, value);
        let (ty, fid, hdr) = decode_field_id(&buf).unwrap();
        assert_eq!(ty, STI_UINT32);
        assert_eq!(fid, field_id);
        let (decoded, consumed) = decode_uint32(&buf[hdr..]).unwrap();
        assert_eq!(decoded, value);
        assert_eq!(hdr + consumed, buf.len());
    }

    #[test]
    fn uint32_roundtrip_load_fee() {
        roundtrip_uint32(SF_LOAD_FEE, 256);
    }

    #[test]
    fn uint32_roundtrip_reserve_base() {
        roundtrip_uint32(SF_RESERVE_BASE, 10_000_000);
    }

    #[test]
    fn uint32_roundtrip_reserve_increment() {
        roundtrip_uint32(SF_RESERVE_INCREMENT, 2_000_000);
    }

    fn roundtrip_uint64(field_id: u16, value: u64) {
        let mut buf = Vec::new();
        put_uint64(&mut buf, field_id, value);
        let (ty, fid, hdr) = decode_field_id(&buf).unwrap();
        assert_eq!(ty, STI_UINT64);
        assert_eq!(fid, field_id);
        let (decoded, consumed) = decode_uint64(&buf[hdr..]).unwrap();
        assert_eq!(decoded, value);
        assert_eq!(hdr + consumed, buf.len());
    }

    #[test]
    fn uint64_roundtrip_base_fee() {
        roundtrip_uint64(SF_BASE_FEE, 10);
    }

    #[test]
    fn uint64_roundtrip_cookie() {
        roundtrip_uint64(SF_COOKIE, 0xDEAD_BEEF_CAFE_F00D);
    }

    #[test]
    fn uint64_roundtrip_server_version() {
        roundtrip_uint64(SF_SERVER_VERSION, 0x0102_0003_0000_0000);
    }

    fn roundtrip_hash256(field_id: u16, value: [u8; 32]) {
        let mut buf = Vec::new();
        put_hash256(&mut buf, field_id, &value);
        let (ty, fid, hdr) = decode_field_id(&buf).unwrap();
        assert_eq!(ty, STI_UINT256);
        assert_eq!(fid, field_id);
        let (decoded, consumed) = decode_hash256(&buf[hdr..]).unwrap();
        assert_eq!(decoded, value);
        assert_eq!(hdr + consumed, buf.len());
    }

    #[test]
    fn hash256_roundtrip_consensus_hash() {
        let mut h = [0u8; 32];
        for (i, b) in h.iter_mut().enumerate() {
            *b = i as u8;
        }
        roundtrip_hash256(SF_CONSENSUS_HASH, h);
    }

    #[test]
    fn hash256_roundtrip_validated_hash() {
        let h = [0xAA; 32];
        roundtrip_hash256(SF_VALIDATED_HASH, h);
    }

    fn roundtrip_amount_xrp(field_id: u16, drops: u64) {
        let mut buf = Vec::new();
        put_amount_xrp(&mut buf, field_id, drops);
        let (ty, fid, hdr) = decode_field_id(&buf).unwrap();
        assert_eq!(ty, STI_AMOUNT);
        assert_eq!(fid, field_id);
        let (decoded, consumed) = decode_amount_xrp(&buf[hdr..]).unwrap();
        assert_eq!(decoded, drops);
        assert_eq!(hdr + consumed, buf.len());
    }

    #[test]
    fn amount_xrp_roundtrip_base_fee_drops() {
        roundtrip_amount_xrp(SF_BASE_FEE_DROPS, 10);
    }

    #[test]
    fn amount_xrp_roundtrip_reserve_base_drops() {
        roundtrip_amount_xrp(SF_RESERVE_BASE_DROPS, 10_000_000);
    }

    #[test]
    fn amount_xrp_roundtrip_reserve_increment_drops() {
        roundtrip_amount_xrp(SF_RESERVE_INCREMENT_DROPS, 2_000_000);
    }

    #[test]
    fn amount_xrp_native_flag_is_set() {
        let mut buf = Vec::new();
        put_amount_xrp(&mut buf, SF_BASE_FEE_DROPS, 10);
        // Header is two bytes (type=6 <16, field=22 >=16): 0x60, 0x16
        assert_eq!(&buf[0..2], &[0x60, 0x16]);
        // High byte must have cPositive (0x40) set, no issued flag (0x80).
        assert_eq!(buf[2] & 0xC0, 0x40);
    }

    #[test]
    fn amount_xrp_rejects_issued_currency() {
        // Construct an issued-currency-marked amount byte stream and ensure
        // decode rejects it.
        let mut bytes = [0u8; 8];
        bytes[0] = 0x80; // cIssuedCurrency high bit
        assert!(decode_amount_xrp(&bytes).is_none());
    }

    #[test]
    fn vector256_roundtrip_amendments() {
        let entries = vec![[0x11u8; 32], [0x22u8; 32], [0x33u8; 32]];
        let mut buf = Vec::new();
        put_vector256(&mut buf, SF_AMENDMENTS, &entries);

        let (ty, fid, hdr) = decode_field_id(&buf).unwrap();
        assert_eq!(ty, STI_VECTOR256);
        assert_eq!(fid, SF_AMENDMENTS);

        let (decoded, consumed) = decode_vector256(&buf[hdr..]).unwrap();
        assert_eq!(decoded, entries);
        assert_eq!(hdr + consumed, buf.len());
    }

    #[test]
    fn vector256_empty_roundtrip() {
        let entries: Vec<[u8; 32]> = Vec::new();
        let mut buf = Vec::new();
        put_vector256(&mut buf, SF_AMENDMENTS, &entries);
        let (_, _, hdr) = decode_field_id(&buf).unwrap();
        let (decoded, _) = decode_vector256(&buf[hdr..]).unwrap();
        assert!(decoded.is_empty());
    }

    #[test]
    fn vector256_vl_prefix_is_byte_count() {
        // 2 entries -> 64 bytes -> single-byte VL length 64.
        let entries = vec![[0u8; 32], [1u8; 32]];
        let mut buf = Vec::new();
        put_vector256(&mut buf, SF_AMENDMENTS, &entries);
        // Header: type=19 (>=16), field=3 (<16) -> 0x03, 0x13
        assert_eq!(&buf[0..2], &[0x03, 0x13]);
        // VL length byte (<= 192): single byte = 64
        assert_eq!(buf[2], 64);
        // Followed by 64 bytes of entries
        assert_eq!(buf.len(), 2 + 1 + 64);
    }
}
