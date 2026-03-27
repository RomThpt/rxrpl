/// Minimal STObject serializer for rippled wire-compatible validation encoding.
///
/// Implements the XRPL binary serialization format for the subset of fields
/// needed by STValidation objects.

// SField type IDs
const STI_UINT32: u8 = 2;
const STI_UINT64: u8 = 3;
const STI_UINT256: u8 = 5;
const STI_VL: u8 = 7;

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

#[cfg(test)]
mod tests {
    use super::*;

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
        put_uint32(&mut buf, 2, 0x80000001); // sfFlags = vfFullValidation
        assert_eq!(&buf[0..1], &[0x22]); // field header
        assert_eq!(&buf[1..5], &[0x80, 0x00, 0x00, 0x01]); // value BE
    }
}
